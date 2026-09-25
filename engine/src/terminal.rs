//! Browser-hosted scrollback backed by Smudgy's native VT semantics.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use smudgy_protocol::vt::{VtEvent, VtProcessor, VtSink};
use smudgy_session_model::{Color, Style, VtSpan};
use vtparse::VTParser;

pub use smudgy_session_model::StyledLine as TerminalLine;

/// Smudgy's normal large-scrollback ceiling.
pub use smudgy_session_model::DEFAULT_SCROLLBACK_LINES;

#[derive(Debug, Default)]
struct BufferSink {
    events: Vec<VtEvent>,
}

impl VtSink for BufferSink {
    #[inline]
    fn emit(&mut self, event: VtEvent) {
        self.events.push(event);
    }
}

/// Worker-owned terminal model. Both hosts now parse VT with the same
/// processor; only the statically selected event sink differs.
pub struct TerminalBuffer {
    parser: VTParser,
    processor: VtProcessor<BufferSink>,
    lines: VecDeque<TerminalLine>,
    current: TerminalLine,
    committed_text: Vec<String>,
    max_lines: usize,
    revision: u64,
    // Native commits a sent command onto the open prompt. The VT processor
    // still retains that server prefix for trigger matching; its eventual
    // completed line must not replay it into the presentation buffer.
    sent_on_open_line: bool,
    // GA/EOR closes the server's logical source line, but native leaves the
    // displayed prompt open. Track that boundary separately from a local send.
    prompt_boundary_open: bool,
    prompt_source: String,
}

/// Incremental presentation rows since a host last sampled a terminal.
#[derive(Debug, PartialEq, Eq)]
pub struct TerminalDelta<'a> {
    pub reset: bool,
    pub revision: u64,
    pub committed: usize,
    pub rows: Vec<&'a TerminalLine>,
    pub live: Option<&'a TerminalLine>,
}

#[derive(Debug, Default, Clone, Copy)]
pub struct TerminalDeltaCursor {
    revision: u64,
    committed: usize,
}

impl TerminalDeltaCursor {
    #[must_use]
    pub fn take<'a>(&mut self, terminal: &'a TerminalBuffer) -> TerminalDelta<'a> {
        let revision = terminal.revision();
        let committed = terminal.committed_len();
        let committed_revision = u64::try_from(committed).unwrap_or(u64::MAX);
        let reset = committed < self.committed
            || revision < self.revision
            || revision.saturating_sub(self.revision) > committed_revision;
        let appended = if reset || self.revision == 0 {
            committed
        } else {
            usize::try_from(revision.saturating_sub(self.revision))
                .unwrap_or(committed)
                .min(committed)
        };
        let first = committed.saturating_sub(appended);
        let rows = (first..committed)
            .filter_map(|index| terminal.rendered_line(index))
            .collect();
        let live = (!terminal.current.text.is_empty()).then_some(&terminal.current);
        self.revision = revision;
        self.committed = committed;
        TerminalDelta {
            reset,
            revision,
            committed,
            rows,
            live,
        }
    }
}

fn processor() -> VtProcessor<BufferSink> {
    let mut processor = VtProcessor::new(BufferSink::default());
    // Browser plaintext triggers never inspect pre-VT raw bytes. Disable
    // capture in the processor rather than copying them into every row.
    processor.set_raw_wanted_flag(Arc::new(AtomicBool::new(false)));
    processor
}

fn empty_line() -> TerminalLine {
    TerminalLine::new("", Vec::new())
}

impl Default for TerminalBuffer {
    fn default() -> Self {
        Self::with_max_lines(DEFAULT_SCROLLBACK_LINES)
    }
}

impl TerminalBuffer {
    #[must_use]
    pub fn with_max_lines(max_lines: usize) -> Self {
        Self {
            parser: VTParser::new(),
            processor: processor(),
            lines: VecDeque::new(),
            current: empty_line(),
            committed_text: Vec::new(),
            max_lines: max_lines.max(1),
            revision: 0,
            sent_on_open_line: false,
            prompt_boundary_open: false,
            prompt_source: String::new(),
        }
    }

    pub fn feed(&mut self, bytes: &[u8]) -> Vec<String> {
        self.feed_with_displayable_marker(bytes).0
    }

    /// Return the native VT processor's packet marker alongside completed lines.
    /// Historical rows and locally printed lines cannot set this marker.
    pub(crate) fn feed_with_displayable_marker(&mut self, bytes: &[u8]) -> (Vec<String>, bool) {
        for &byte in bytes {
            self.parser.parse_byte(byte, &mut self.processor);
        }
        self.processor.notify_end_of_buffer();
        let displayable = self.apply_events();
        (std::mem::take(&mut self.committed_text), displayable)
    }

    /// GA/EOR ends a server source line but leaves its visible prompt open.
    /// Return only the new source fragment for synchronous plaintext triggers.
    pub fn commit_prompt(&mut self) -> Vec<String> {
        self.processor.commit_prompt();
        self.apply_events();
        let prompt = std::mem::take(&mut self.prompt_source);
        (!prompt.is_empty()).then_some(prompt).into_iter().collect()
    }

    fn apply_events(&mut self) -> bool {
        let mut displayable = false;
        for event in std::mem::take(&mut self.processor.sink_mut().events) {
            match event {
                VtEvent::HandleIncomingPartialLine(line) => {
                    self.prompt_source.push_str(&line.text);
                    self.current = if self.current.text.is_empty() {
                        Arc::unwrap_or_clone(line)
                    } else {
                        self.current.append(&line)
                    };
                }
                VtEvent::HandleIncomingLine(line) => {
                    let source_text = line.text.clone();
                    let line = Arc::unwrap_or_clone(line);
                    let completed = if self.prompt_boundary_open && !self.current.text.is_empty() {
                        self.current.append(&line)
                    } else {
                        line
                    };
                    self.current = empty_line();
                    self.sent_on_open_line = false;
                    self.prompt_boundary_open = false;
                    self.prompt_source.clear();
                    self.push_line_with_source(completed, source_text);
                }
                VtEvent::HandleIncomingFragmentedLine {
                    line,
                    completion_fragment,
                } => {
                    let source_text = line.text.clone();
                    let completed = if self.sent_on_open_line || self.prompt_boundary_open {
                        let tail = Arc::unwrap_or_clone(completion_fragment);
                        if self.current.text.is_empty() {
                            tail
                        } else {
                            self.current.append(&tail)
                        }
                    } else {
                        Arc::unwrap_or_clone(line)
                    };
                    self.current = empty_line();
                    self.sent_on_open_line = false;
                    self.prompt_boundary_open = false;
                    self.prompt_source.clear();
                    self.push_line_with_source(completed, source_text);
                }
                VtEvent::PromptBoundary => {
                    self.sent_on_open_line = false;
                    self.prompt_boundary_open = true;
                }
                VtEvent::RetractIncomingPartialLine => {
                    self.current = empty_line();
                    self.sent_on_open_line = false;
                    self.prompt_boundary_open = false;
                    self.prompt_source.clear();
                }
                VtEvent::IncomingPacketProcessed {
                    has_displayable_text,
                    ..
                } => displayable |= has_displayable_text,
                VtEvent::RequestRepaint => {}
            }
        }
        displayable
    }

    fn push_line(&mut self, line: TerminalLine) {
        let source_text = line.text.clone();
        self.push_line_with_source(line, source_text);
    }

    fn push_line_with_source(&mut self, line: TerminalLine, source_text: String) {
        // A prior GA/EOR prompt or local echo can alter the visible row. The
        // automation host still matches the exact server logical line.
        self.committed_text.push(source_text);
        self.lines.push_back(line);
        self.revision = self.revision.wrapping_add(1);
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
        }
    }

    pub fn push_local_line(&mut self, text: impl Into<String>) {
        let text = text.into();
        let len = text.len();
        let line = TerminalLine::from_owned(
            text,
            vec![VtSpan {
                begin_pos: 0,
                end_pos: len,
                style: Style {
                    fg: Color::Rgb {
                        r: 118,
                        g: 179,
                        b: 229,
                    },
                    ..Style::DEFAULT
                },
            }],
        );
        self.lines.push_back(line);
        self.revision = self.revision.wrapping_add(1);
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
        }
    }

    /// Echo a successfully sent command without routing it back through
    /// network triggers. Like native, join an open prompt before committing.
    /// The protocol processor keeps its source fragments for trigger matching;
    /// `sent_on_open_line` suppresses their later replay into scrollback.
    pub fn echo_sent_command(&mut self, display: &str) {
        let command = TerminalLine::from_output_str(display);
        let line = if self.current.text.is_empty() {
            command
        } else {
            self.current.append(&command)
        };
        self.current = empty_line();
        self.sent_on_open_line = true;
        self.prompt_boundary_open = false;
        self.lines.push_back(line);
        self.revision = self.revision.wrapping_add(1);
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
        }
    }

    pub fn clear(&mut self) {
        self.parser = VTParser::new();
        self.processor = processor();
        self.lines.clear();
        self.current = empty_line();
        self.committed_text.clear();
        self.sent_on_open_line = false;
        self.prompt_boundary_open = false;
        self.prompt_source.clear();
        self.revision = self.revision.wrapping_add(1);
    }

    /// Start a fresh VT stream without discarding this session's transcript.
    /// A pending prompt becomes a historical row before the new socket writes.
    pub fn begin_new_connection(&mut self) {
        if !self.current.text.is_empty() {
            let line = std::mem::replace(&mut self.current, empty_line());
            self.push_line(line);
            self.committed_text.clear();
        }
        self.parser = VTParser::new();
        self.processor = processor();
        self.sent_on_open_line = false;
        self.prompt_boundary_open = false;
        self.prompt_source.clear();
    }

    #[must_use]
    pub fn lines(&self) -> impl DoubleEndedIterator<Item = &TerminalLine> + ExactSizeIterator {
        self.lines.iter()
    }

    #[must_use]
    pub fn committed_len(&self) -> usize {
        self.lines.len()
    }

    #[must_use]
    pub fn rendered_len(&self) -> usize {
        self.committed_len() + 1
    }

    #[must_use]
    pub const fn max_lines(&self) -> usize {
        self.max_lines
    }

    /// Change the history cap without reparsing or discarding the live row.
    /// A later delta resets if this evicts committed rows.
    pub fn set_max_lines(&mut self, max_lines: usize) {
        self.max_lines = max_lines.max(1);
        while self.lines.len() > self.max_lines {
            self.lines.pop_front();
        }
    }

    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    #[must_use]
    pub fn rendered_line(&self, index: usize) -> Option<&TerminalLine> {
        if index == self.lines.len() {
            Some(&self.current)
        } else {
            self.lines.get(index)
        }
    }

    #[must_use]
    pub const fn current_line(&self) -> &TerminalLine {
        &self.current
    }
}

pub type TerminalDocument = TerminalBuffer;

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn preserves_sgr_across_websocket_chunks() {
        let mut terminal = TerminalDocument::default();
        terminal.feed(b"plain \x1b[31");
        terminal.feed(b"mred\x1b[0m\n");
        let line = terminal.lines().next().unwrap();
        assert_eq!(line.text, "plain red");
        assert!(line.spans.len() >= 2);
        assert_eq!(
            line.spans
                .iter()
                .find(|span| span.begin_pos < span.end_pos && span.style.fg != Style::DEFAULT.fg)
                .unwrap()
                .style
                .fg,
            Color::Ansi {
                color: smudgy_session_model::AnsiColor::Red,
                bold: false,
            }
        );
    }

    #[test]
    fn carriage_return_replaces_a_prompt_line() {
        let mut terminal = TerminalDocument::default();
        terminal.feed(b"old prompt\rnew prompt\n");
        assert_eq!(terminal.lines().next().unwrap().text, "new prompt");
    }

    #[test]
    fn sent_command_joins_prompt_without_replaying_its_server_prefix() {
        let mut terminal = TerminalDocument::default();
        terminal.feed(b"Prompt> ");
        terminal.echo_sent_command("look");
        assert_eq!(terminal.lines().next().unwrap().text, "Prompt> look");
        assert!(terminal.current_line().text.is_empty());
        // Local echoes never enter the network-trigger feed.
        assert!(terminal.feed(b"").is_empty());

        terminal.feed(b"after ");
        assert_eq!(terminal.feed(b"command\r\n"), ["Prompt> after command"]);
        assert_eq!(
            terminal
                .lines()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["Prompt> look", "after command"]
        );
    }

    #[test]
    fn prompt_boundaries_keep_one_live_row_and_only_trigger_new_source_text() {
        let mut terminal = TerminalDocument::default();
        terminal.feed(b"HP:");
        assert_eq!(terminal.commit_prompt(), ["HP:"]);
        assert_eq!(terminal.committed_len(), 0);
        assert_eq!(terminal.current_line().text, "HP:");
        assert!(terminal.commit_prompt().is_empty());

        terminal.feed(b" 7");
        assert_eq!(terminal.commit_prompt(), [" 7"]);
        assert_eq!(terminal.current_line().text, "HP: 7");
        assert_eq!(terminal.committed_len(), 0);

        assert_eq!(terminal.feed(b" after\r\n"), [" after"]);
        assert_eq!(terminal.lines().next().unwrap().text, "HP: 7 after");
    }

    #[test]
    fn carriage_return_after_prompt_boundary_replaces_the_open_row() {
        let mut terminal = TerminalDocument::default();
        terminal.feed(b"old");
        terminal.commit_prompt();
        terminal.feed(b"\rnew\r\n");
        assert_eq!(terminal.lines().next().unwrap().text, "new");
    }

    #[test]
    fn sent_command_without_prompt_is_a_plain_historical_row() {
        let mut terminal = TerminalDocument::default();
        terminal.echo_sent_command("north");
        assert_eq!(terminal.lines().next().unwrap().text, "north");
        assert_eq!(terminal.lines().next().unwrap().spans.len(), 1);
        terminal.feed(b"next\r\n");
        assert_eq!(
            terminal
                .lines()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["north", "next"]
        );
    }

    #[test]
    fn large_scrollback_is_bounded_and_evicts_from_the_front() {
        const LIMIT: usize = 100_000;
        let mut terminal = TerminalBuffer::with_max_lines(LIMIT);
        for batch in 0..101 {
            let first = batch * 1_000;
            let text = (first..first + 1_000).fold(String::new(), |mut text, line| {
                writeln!(text, "line {line}").unwrap();
                text
            });
            terminal.feed(text.as_bytes());
        }
        assert_eq!(terminal.committed_len(), LIMIT);
        assert_eq!(terminal.lines().next().unwrap().text, "line 1000");
        assert_eq!(terminal.lines().next_back().unwrap().text, "line 100999");
    }

    #[test]
    fn shrinking_scrollback_keeps_newest_rows_and_resets_the_delta() {
        let mut terminal = TerminalBuffer::with_max_lines(4);
        terminal.feed(b"one\ntwo\nthree\nprompt");
        let mut cursor = TerminalDeltaCursor::default();
        assert_eq!(cursor.take(&terminal).committed, 3);

        terminal.set_max_lines(2);
        assert_eq!(terminal.max_lines(), 2);
        assert_eq!(
            terminal
                .lines()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["two", "three"]
        );
        let delta = cursor.take(&terminal);
        assert!(delta.reset);
        assert_eq!(delta.rows.len(), 2);
        assert_eq!(delta.live.unwrap().text, "prompt");
    }

    #[test]
    fn delta_cursor_sends_only_appended_rows_and_the_live_prompt() {
        let mut terminal = TerminalBuffer::with_max_lines(3);
        terminal.feed(b"one\ntwo\nprompt");
        let mut cursor = TerminalDeltaCursor::default();
        let first = cursor.take(&terminal);
        assert_eq!(
            first
                .rows
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["one", "two"]
        );
        assert_eq!(first.live.unwrap().text, "prompt");

        terminal.feed(b"\nthree\nnext");
        let second = cursor.take(&terminal);
        assert!(!second.reset);
        assert_eq!(
            second
                .rows
                .iter()
                .map(|line| line.text.as_str())
                .collect::<Vec<_>>(),
            ["prompt", "three"]
        );
        assert_eq!(second.live.unwrap().text, "next");
    }

    #[test]
    fn osc8_metadata_survives_into_worker_scrollback() {
        let mut terminal = TerminalBuffer::default();
        terminal.feed(b"\x1b]8;;https://example.org\x1b\\link\x1b]8;;\x1b\\\n");
        let line = terminal.lines().next().unwrap();
        assert_eq!(line.text, "link");
        assert_eq!(line.links.len(), 1);
    }

    #[test]
    fn reconnect_retains_scrollback_but_resets_vt_state() {
        let mut terminal = TerminalBuffer::default();
        terminal.feed(b"\x1b[31mold\nopen");
        terminal.begin_new_connection();
        terminal.feed(b"fresh\n");
        let lines: Vec<_> = terminal.lines().map(|line| line.text.as_str()).collect();
        assert_eq!(lines, ["old", "open", "fresh"]);
        assert_eq!(
            terminal
                .lines()
                .last()
                .unwrap()
                .spans
                .last()
                .unwrap()
                .style
                .fg,
            Style::DEFAULT.fg
        );
    }
}
