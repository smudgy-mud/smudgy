//! Compact, versioned messages between a session worker and the iced window.
//!
//! The browser must copy data between separate WASM memories. Encoding one
//! transferable `ArrayBuffer` keeps that unavoidable work linear and avoids a
//! JavaScript object allocation for every terminal span.

use smudgy_engine::{ConnectionState, TerminalDelta, TerminalLine};
use smudgy_session_model::{
    AnsiColor, Blink, Color, MAX_SCROLLBACK_LINES, Style, StyledLine, TextAttributes, Underline,
    VtSpan,
};

/// One ordered presentation update decoded for the browser window.
#[derive(Debug, Clone)]
pub struct SessionUpdate {
    pub sequence: u64,
    pub connection: ConnectionState,
    pub status: String,
    pub server_echo: bool,
    pub gmcp_messages: u64,
    pub max_lines: usize,
    pub revision: u64,
    pub committed: usize,
    pub reset: bool,
    pub rows: Vec<StyledLine>,
    pub live: Option<StyledLine>,
}

const MAGIC: &[u8; 4] = b"SMW7";
const FLAG_SERVER_ECHO: u8 = 1 << 0;
const FLAG_RESET: u8 = 1 << 1;
const FLAG_LIVE: u8 = 1 << 2;
const ATTR_BOLD: u8 = 1 << 0;
const ATTR_FAINT: u8 = 1 << 1;
const ATTR_ITALIC: u8 = 1 << 2;
const ATTR_CROSSED_OUT: u8 = 1 << 3;
const ATTR_REVERSE: u8 = 1 << 4;
const KNOWN_ATTRS: u8 = ATTR_BOLD | ATTR_FAINT | ATTR_ITALIC | ATTR_CROSSED_OUT | ATTR_REVERSE;

pub struct SessionFrame<'a> {
    pub sequence: u64,
    pub connection: ConnectionState,
    pub status: &'a str,
    pub server_echo: bool,
    pub gmcp_messages: u64,
    pub max_lines: usize,
    pub delta: TerminalDelta<'a>,
}

/// Encode one worker-owned session snapshot into the browser wire format.
///
/// # Errors
///
/// Returns an error if a platform-sized count or string cannot be represented
/// by the fixed-width wire format.
pub fn encode(frame: &SessionFrame<'_>) -> Result<Vec<u8>, String> {
    // Most presentation frames are short, but a 1,000-row burst otherwise
    // repeatedly grows the transferable buffer while encoding each span.
    let capacity = frame.delta.rows.len().saturating_mul(96).min(1_000_000);
    let mut out = Vec::with_capacity(capacity);
    out.extend_from_slice(MAGIC);
    put_u64(&mut out, frame.sequence);
    out.push(match frame.connection {
        ConnectionState::Disconnected => 0,
        ConnectionState::Connecting => 1,
        ConnectionState::Connected => 2,
    });
    let mut flags = 0;
    flags |= u8::from(frame.server_echo) * FLAG_SERVER_ECHO;
    flags |= u8::from(frame.delta.reset) * FLAG_RESET;
    flags |= u8::from(frame.delta.live.is_some()) * FLAG_LIVE;
    out.push(flags);
    put_u64(&mut out, frame.gmcp_messages);
    put_u64(&mut out, as_u64(frame.max_lines, "scrollback limit")?);
    put_u64(&mut out, frame.delta.revision);
    put_u64(
        &mut out,
        as_u64(frame.delta.committed, "committed row count")?,
    );
    put_str(&mut out, frame.status)?;
    put_u32(
        &mut out,
        u32::try_from(frame.delta.rows.len())
            .map_err(|_| "worker delta has too many rows".to_owned())?,
    );
    let mut linked_rows = Vec::new();
    for (index, line) in frame.delta.rows.iter().enumerate() {
        put_line(&mut out, line)?;
        if !line.links.is_empty() {
            linked_rows.push((index, *line));
        }
    }
    if let Some(line) = frame.delta.live {
        put_line(&mut out, line)?;
        if !line.links.is_empty() {
            linked_rows.push((frame.delta.rows.len(), line));
        }
    }
    // OSC links are sparse. Keep the common row encoding identical to the
    // no-link path and append only linked rows in a side table.
    put_u32(
        &mut out,
        u32::try_from(linked_rows.len()).map_err(|_| "too many link rows".to_owned())?,
    );
    for (index, line) in linked_rows {
        put_u32(
            &mut out,
            u32::try_from(index).map_err(|_| "too many link rows".to_owned())?,
        );
        let links = serde_json::to_vec(&line.links)
            .map_err(|error| format!("terminal links cannot be encoded: {error}"))?;
        put_u32(
            &mut out,
            u32::try_from(links.len()).map_err(|_| "terminal links are too large".to_owned())?,
        );
        out.extend_from_slice(&links);
    }
    Ok(out)
}

/// Decode one worker frame into the window's owned presentation update.
///
/// # Errors
///
/// Returns an error for an unsupported, malformed, truncated, or
/// platform-incompatible frame.
pub fn decode(bytes: &[u8]) -> Result<SessionUpdate, String> {
    let mut input = Input::new(bytes);
    if input.take(MAGIC.len())? != MAGIC {
        return Err("session worker sent an unsupported frame".to_owned());
    }
    let sequence = input.u64()?;
    let connection = match input.u8()? {
        0 => ConnectionState::Disconnected,
        1 => ConnectionState::Connecting,
        2 => ConnectionState::Connected,
        _ => return Err("session worker sent an invalid connection state".to_owned()),
    };
    let flags = input.u8()?;
    let gmcp_messages = input.u64()?;
    let max_lines = input.usize("scrollback limit")?;
    if max_lines == 0 || max_lines > MAX_SCROLLBACK_LINES {
        return Err("session worker sent an invalid scrollback limit".to_owned());
    }
    let revision = input.u64()?;
    let committed = input.usize("committed row count")?;
    let status = input.string()?;
    let row_count = input.count_with_minimum("row count", size_of::<u32>())?;
    let mut rows = Vec::with_capacity(row_count);
    for _ in 0..row_count {
        rows.push(input.line()?);
    }
    let mut live = if flags & FLAG_LIVE == 0 {
        None
    } else {
        Some(input.line()?)
    };
    let link_rows = input.count_with_minimum("link row count", 2 * size_of::<u32>())?;
    let mut previous = None;
    for _ in 0..link_rows {
        let index = usize::try_from(input.u32()?)
            .map_err(|_| "link row index does not fit this platform".to_owned())?;
        if previous.is_some_and(|last| index <= last) {
            return Err("session worker link rows are not ordered".to_owned());
        }
        previous = Some(index);
        let target_row = if index == rows.len() {
            live.as_mut()
                .ok_or_else(|| "session worker linked a missing live row".to_owned())?
        } else {
            rows.get_mut(index)
                .ok_or_else(|| "session worker link row exceeds its frame".to_owned())?
        };
        let links_len = input.count("link metadata length")?;
        let links: Vec<smudgy_session_model::LinkSpan> =
            serde_json::from_slice(input.take(links_len)?)
                .map_err(|error| format!("session worker sent invalid links: {error}"))?;
        validate_links(&links, &target_row.text)?;
        target_row.links = links;
    }
    if !input.is_empty() {
        return Err("session worker frame has trailing bytes".to_owned());
    }
    Ok(SessionUpdate {
        sequence,
        connection,
        status,
        server_echo: flags & FLAG_SERVER_ECHO != 0,
        gmcp_messages,
        max_lines,
        revision,
        committed,
        reset: flags & FLAG_RESET != 0,
        rows,
        live,
    })
}

fn put_line(out: &mut Vec<u8>, line: &TerminalLine) -> Result<(), String> {
    put_u32(
        out,
        u32::try_from(line.text.len()).map_err(|_| "terminal line is too large".to_owned())?,
    );
    out.extend_from_slice(line.text.as_bytes());
    put_u32(
        out,
        u32::try_from(line.spans.len())
            .map_err(|_| "terminal line has too many spans".to_owned())?,
    );
    for span in &line.spans {
        put_style(out, span.style);
        put_u32(
            out,
            u32::try_from(span.end_pos.saturating_sub(span.begin_pos))
                .map_err(|_| "terminal span is too large".to_owned())?,
        );
    }
    Ok(())
}

fn put_style(out: &mut Vec<u8>, style: Style) {
    put_color(out, style.fg);
    put_color(out, style.bg);
    let attributes = style.attributes;
    let mut flags = 0;
    flags |= u8::from(attributes.bold) * ATTR_BOLD;
    flags |= u8::from(attributes.faint) * ATTR_FAINT;
    flags |= u8::from(attributes.italic) * ATTR_ITALIC;
    flags |= u8::from(attributes.crossed_out) * ATTR_CROSSED_OUT;
    flags |= u8::from(attributes.reverse) * ATTR_REVERSE;
    out.push(flags);
    out.push(match attributes.underline {
        Underline::None => 0,
        Underline::Single => 1,
        Underline::Double => 2,
    });
    out.push(match attributes.blink {
        Blink::None => 0,
        Blink::Slow => 1,
        Blink::Fast => 2,
    });
}

fn put_color(out: &mut Vec<u8>, color: Color) {
    match color {
        Color::DefaultForeground { bold: false } => out.push(0),
        Color::DefaultForeground { bold: true } => out.push(1),
        Color::DefaultBackground => out.push(2),
        Color::Ansi { color, bold } => {
            out.push(if bold { 4 } else { 3 });
            out.push(match color {
                AnsiColor::Black => 0,
                AnsiColor::Red => 1,
                AnsiColor::Green => 2,
                AnsiColor::Yellow => 3,
                AnsiColor::Blue => 4,
                AnsiColor::Magenta => 5,
                AnsiColor::Cyan => 6,
                AnsiColor::White => 7,
            });
        }
        Color::Rgb { r, g, b } => {
            out.push(5);
            out.extend_from_slice(&[r, g, b]);
        }
        Color::Echo => out.push(6),
        Color::Output => out.push(7),
        Color::Warn => out.push(8),
    }
}

fn put_str(out: &mut Vec<u8>, value: &str) -> Result<(), String> {
    put_u32(
        out,
        u32::try_from(value.len()).map_err(|_| "worker string is too large".to_owned())?,
    );
    out.extend_from_slice(value.as_bytes());
    Ok(())
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn as_u64(value: usize, name: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("{name} does not fit the worker protocol"))
}

struct Input<'a> {
    remaining: &'a [u8],
}

impl<'a> Input<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    const fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], String> {
        let (value, remaining) = self
            .remaining
            .split_at_checked(count)
            .ok_or_else(|| "session worker frame ended early".to_owned())?;
        self.remaining = remaining;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, String> {
        let bytes = self.take(size_of::<u32>())?;
        Ok(u32::from_le_bytes(bytes.try_into().expect("fixed width")))
    }

    fn u64(&mut self) -> Result<u64, String> {
        let bytes = self.take(size_of::<u64>())?;
        Ok(u64::from_le_bytes(bytes.try_into().expect("fixed width")))
    }

    fn usize(&mut self, name: &str) -> Result<usize, String> {
        usize::try_from(self.u64()?)
            .map_err(|_| format!("session worker {name} does not fit this platform"))
    }

    fn count(&mut self, name: &str) -> Result<usize, String> {
        let value = usize::try_from(self.u32()?)
            .map_err(|_| format!("session worker {name} does not fit this platform"))?;
        if value > self.remaining.len() {
            return Err(format!("session worker {name} exceeds its frame"));
        }
        Ok(value)
    }

    fn count_with_minimum(&mut self, name: &str, item_bytes: usize) -> Result<usize, String> {
        let value = self.count(name)?;
        if value > self.remaining.len() / item_bytes {
            return Err(format!("session worker {name} exceeds its frame"));
        }
        Ok(value)
    }

    fn string_slice(&mut self) -> Result<&'a str, String> {
        let length = self.count("string length")?;
        std::str::from_utf8(self.take(length)?)
            .map_err(|_| "session worker sent invalid UTF-8".to_owned())
    }

    fn string(&mut self) -> Result<String, String> {
        self.string_slice().map(str::to_owned)
    }

    fn line(&mut self) -> Result<StyledLine, String> {
        let text = self.string()?;
        // Two color tags, three attribute bytes, and the text length.
        let span_count = self.count_with_minimum("span count", 5 + size_of::<u32>())?;
        let mut spans = Vec::with_capacity(span_count);
        let mut end_pos = 0usize;
        for _ in 0..span_count {
            let style = self.style()?;
            let begin_pos = end_pos;
            let span_len = usize::try_from(self.u32()?)
                .map_err(|_| "terminal span is too large".to_owned())?;
            end_pos = end_pos
                .checked_add(span_len)
                .ok_or_else(|| "terminal span exceeds its line".to_owned())?;
            if end_pos > text.len() || !text.is_char_boundary(end_pos) {
                return Err("terminal span exceeds its line".to_owned());
            }
            spans.push(VtSpan {
                begin_pos,
                end_pos,
                style,
            });
        }
        if end_pos != text.len() {
            return Err("terminal spans do not cover their line".to_owned());
        }
        Ok(StyledLine::from_owned(text, spans))
    }

    fn style(&mut self) -> Result<Style, String> {
        let fg = self.color()?;
        let bg = self.color()?;
        let flags = self.u8()?;
        if flags & !KNOWN_ATTRS != 0 {
            return Err("session worker sent invalid text attributes".to_owned());
        }
        let underline = match self.u8()? {
            0 => Underline::None,
            1 => Underline::Single,
            2 => Underline::Double,
            _ => return Err("session worker sent an invalid underline".to_owned()),
        };
        let blink = match self.u8()? {
            0 => Blink::None,
            1 => Blink::Slow,
            2 => Blink::Fast,
            _ => return Err("session worker sent an invalid blink".to_owned()),
        };
        Ok(Style {
            fg,
            bg,
            attributes: TextAttributes {
                bold: flags & ATTR_BOLD != 0,
                faint: flags & ATTR_FAINT != 0,
                italic: flags & ATTR_ITALIC != 0,
                underline,
                blink,
                crossed_out: flags & ATTR_CROSSED_OUT != 0,
                reverse: flags & ATTR_REVERSE != 0,
            },
        })
    }

    fn color(&mut self) -> Result<Color, String> {
        match self.u8()? {
            0 => Ok(Color::DefaultForeground { bold: false }),
            1 => Ok(Color::DefaultForeground { bold: true }),
            2 => Ok(Color::DefaultBackground),
            tag @ (3 | 4) => {
                let color = match self.u8()? {
                    0 => AnsiColor::Black,
                    1 => AnsiColor::Red,
                    2 => AnsiColor::Green,
                    3 => AnsiColor::Yellow,
                    4 => AnsiColor::Blue,
                    5 => AnsiColor::Magenta,
                    6 => AnsiColor::Cyan,
                    7 => AnsiColor::White,
                    _ => return Err("session worker sent an invalid ANSI color".to_owned()),
                };
                Ok(Color::Ansi {
                    color,
                    bold: tag == 4,
                })
            }
            5 => {
                let rgb = self.take(3)?;
                Ok(Color::Rgb {
                    r: rgb[0],
                    g: rgb[1],
                    b: rgb[2],
                })
            }
            6 => Ok(Color::Echo),
            7 => Ok(Color::Output),
            8 => Ok(Color::Warn),
            _ => Err("session worker sent an invalid color".to_owned()),
        }
    }
}

fn validate_links(links: &[smudgy_session_model::LinkSpan], text: &str) -> Result<(), String> {
    let mut last_end = 0;
    for link in links {
        if link.begin_pos < last_end
            || link.end_pos < link.begin_pos
            || link.end_pos > text.len()
            || !text.is_char_boundary(link.begin_pos)
            || !text.is_char_boundary(link.end_pos)
        {
            return Err("session worker sent an invalid link range".to_owned());
        }
        last_end = link.end_pos;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use smudgy_engine::{TerminalBuffer, TerminalDeltaCursor};

    use super::*;

    #[test]
    fn round_trips_borrowed_terminal_delta() {
        let mut terminal = TerminalBuffer::with_max_lines(123);
        terminal.feed(b"\x1b[1;31mred\x1b[0m plain\nprompt");
        let mut cursor = TerminalDeltaCursor::default();
        let frame = SessionFrame {
            sequence: 1,
            connection: ConnectionState::Connected,
            status: "Connected",
            server_echo: true,
            gmcp_messages: 17,
            max_lines: terminal.max_lines(),
            delta: cursor.take(&terminal),
        };

        let decoded = decode(&encode(&frame).unwrap()).unwrap();
        assert_eq!(decoded.connection, ConnectionState::Connected);
        assert_eq!(decoded.sequence, 1);
        assert_eq!(decoded.status, "Connected");
        assert!(decoded.server_echo);
        assert_eq!(decoded.gmcp_messages, 17);
        assert_eq!(decoded.max_lines, 123);
        assert!(!decoded.reset);
        assert_eq!(decoded.rows[0].text, "red plain");
        assert!(
            decoded.rows[0]
                .spans
                .iter()
                .any(|span| span.style.attributes.bold && span.begin_pos < span.end_pos)
        );
        assert_eq!(
            decoded.rows[0]
                .spans
                .iter()
                .find(|span| span.style.attributes.bold && span.begin_pos < span.end_pos)
                .unwrap()
                .style
                .fg,
            Color::Ansi {
                color: AnsiColor::Red,
                bold: false,
            }
        );
        assert_eq!(decoded.live.unwrap().text, "prompt");
    }

    #[test]
    fn preserves_the_full_native_sgr_style_across_the_worker_boundary() {
        let mut terminal = TerminalBuffer::default();
        terminal.feed(b"\x1b[2;3;6;9;21;7;48;5;214mstyled\n");
        let expected = terminal
            .lines()
            .next()
            .unwrap()
            .spans
            .iter()
            .find(|span| span.begin_pos < span.end_pos)
            .unwrap()
            .style;
        let mut cursor = TerminalDeltaCursor::default();
        let frame = SessionFrame {
            sequence: 0,
            connection: ConnectionState::Connected,
            status: "Connected",
            server_echo: false,
            gmcp_messages: 0,
            max_lines: terminal.max_lines(),
            delta: cursor.take(&terminal),
        };
        let decoded = decode(&encode(&frame).unwrap()).unwrap();

        assert_eq!(
            decoded.rows[0]
                .spans
                .iter()
                .find(|span| span.begin_pos < span.end_pos)
                .unwrap()
                .style,
            expected
        );
        assert!(expected.attributes.faint);
        assert!(expected.attributes.italic);
        assert!(expected.attributes.reverse);
        assert!(expected.attributes.crossed_out);
        assert_eq!(expected.attributes.blink, Blink::Fast);
        assert_eq!(expected.attributes.underline, Underline::Double);
    }

    #[test]
    fn preserves_server_authored_osc8_links_across_the_worker_boundary() {
        let mut terminal = TerminalBuffer::default();
        terminal.feed(b"\x1b]8;;https://example.org/help\x1b\\help\x1b]8;;\x1b\\\n");
        let expected = terminal.lines().next().unwrap().links.clone();
        assert_eq!(expected.len(), 1);
        let mut cursor = TerminalDeltaCursor::default();
        let frame = SessionFrame {
            sequence: 0,
            connection: ConnectionState::Connected,
            status: "Connected",
            server_echo: false,
            gmcp_messages: 0,
            max_lines: terminal.max_lines(),
            delta: cursor.take(&terminal),
        };
        let decoded = decode(&encode(&frame).unwrap()).unwrap();
        assert_eq!(decoded.rows[0].links, expected);
    }

    #[test]
    fn sparse_link_table_preserves_live_row_links() {
        let mut terminal = TerminalBuffer::default();
        terminal.feed(b"plain\n\x1b]8;;https://example.org/live\x1b\\live\x1b]8;;\x1b\\");
        let expected = terminal.current_line().links.clone();
        assert_eq!(expected.len(), 1);
        let mut cursor = TerminalDeltaCursor::default();
        let frame = SessionFrame {
            sequence: 3,
            connection: ConnectionState::Connected,
            status: "Connected",
            server_echo: false,
            gmcp_messages: 0,
            max_lines: terminal.max_lines(),
            delta: cursor.take(&terminal),
        };
        let decoded = decode(&encode(&frame).unwrap()).unwrap();
        assert!(decoded.rows[0].links.is_empty());
        assert_eq!(decoded.live.unwrap().links, expected);
    }

    #[test]
    fn rejects_truncated_and_trailing_frames() {
        assert!(decode(MAGIC).is_err());

        let line = TerminalLine::new(
            "line",
            vec![VtSpan {
                begin_pos: 0,
                end_pos: 4,
                style: Style::default(),
            }],
        );
        let terminal = TerminalBuffer::default();
        let frame = SessionFrame {
            sequence: 2,
            connection: ConnectionState::Disconnected,
            status: "Ready",
            server_echo: false,
            gmcp_messages: 0,
            max_lines: terminal.max_lines(),
            delta: TerminalDelta {
                reset: false,
                revision: 1,
                committed: 1,
                rows: vec![&line],
                live: None,
            },
        };
        let mut encoded = encode(&frame).unwrap();
        encoded.push(0);
        assert!(decode(&encoded).is_err());
    }
}
