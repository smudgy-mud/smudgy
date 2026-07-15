use std::sync::Arc;

use tokio::sync::mpsc::UnboundedSender;
use vtparse::{CsiParam, VTActor};

use crate::session::{
    runtime::RuntimeAction,
    styled_line::StyledLine,
    styled_line::{LinkAction, LinkSpan, Style, VtSpan},
};

mod sgr;
pub use sgr::{AnsiColor, Color};
// Expose the SGR interpreter to the `smudgy_bench` crate without widening the normal public
// API: `mod sgr` stays private; the function becomes reachable only under the feature.
#[cfg(feature = "bench-api")]
pub use sgr::process as sgr_process;

/// The most bytes an OSC 8 URI may carry; longer links are ignored (the text
/// still displays, unlinked).
const MAX_OSC8_URI_LEN: usize = 8192;

/// Map an OSC 8 URI to its click action. The scheme allowlist is the trust
/// boundary: `http`/`https` open the browser (behind the per-server confirm),
/// a `send:` URI sends its percent-decoded command (same gate), and anything
/// else — `file:`, `javascript:`, unknown schemes — yields no link at all.
fn link_action_for_uri(uri: &str) -> Option<LinkAction> {
    if uri.len() > MAX_OSC8_URI_LEN {
        log::debug!("OSC 8 URI over {MAX_OSC8_URI_LEN} bytes ignored");
        return None;
    }
    // Compare on bytes: a `str` slice at `p.len()` would panic when the URI
    // begins with multibyte UTF-8 that straddles that offset (server input).
    let prefix = |p: &str| {
        uri.len() > p.len() && uri.as_bytes()[..p.len()].eq_ignore_ascii_case(p.as_bytes())
    };
    if prefix("http://") || prefix("https://") {
        return Some(LinkAction::OpenUrl(Arc::from(uri)));
    }
    if prefix("send:") {
        return Some(LinkAction::ServerSend(Arc::from(
            percent_decode(&uri[5..]).as_str(),
        )));
    }
    log::debug!("OSC 8 URI with unsupported scheme ignored");
    None
}

/// Decode `%XX` escapes (RFC 3986); anything malformed passes through
/// verbatim. `+` is not space — that is form encoding, not URI encoding.
fn percent_decode(s: &str) -> String {
    fn hex_pair(bytes: &[u8]) -> Option<u8> {
        let hi = (*bytes.first()? as char).to_digit(16)?;
        let lo = (*bytes.get(1)? as char).to_digit(16)?;
        u8::try_from(hi * 16 + lo).ok()
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && let Some(byte) = bytes.get(i + 1..).and_then(hex_pair)
        {
            out.push(byte);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[derive(Debug)]
pub struct VtProcessor {
    cursor_style: Style,
    buf: String,
    buf_raw: Vec<u8>,
    span_info: Vec<VtSpan>,
    /// Completed link ranges of the pending line (closed OSC 8 links).
    link_info: Vec<LinkSpan>,
    /// The active OSC 8 link, applied to every character until `ESC]8;;`.
    /// Like the cursor style, it survives line commits (a multi-line link)
    /// and carriage-return overprints.
    cursor_link: Option<LinkAction>,
    /// Where in `buf` the active link's current range began.
    link_open_pos: usize,
    /// A bare `\r` arrived: the next printable character overwrites the open
    /// line (carriage-return overprint — progress bars, spinners) instead of
    /// appending to it. Holds `buf_raw`'s length at the `\r`, so the restart
    /// can drop exactly the superseded frame's raw bytes while keeping any
    /// that arrived after it (escape sequences, the new frame's first char —
    /// the connection pushes raw bytes before the parser sees them). `\n`
    /// clears it, so CRLF stays an ordinary commit. Persists across reads
    /// like the rest of the parse state.
    pending_cr: Option<usize>,
    session_runtime_tx: UnboundedSender<RuntimeAction>,
    /// Whether any trigger currently carries a raw pattern — the only consumer
    /// of `StyledLine::raw`. Owned by the trigger manager, read here at line
    /// boundaries; `None` (tests, benches) means always capture.
    raw_wanted: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The `raw_wanted` value latched for the line being accumulated. Refreshed
    /// only when the raw buffer is empty, so an emitted line's raw form is
    /// always complete-or-absent, never a torn suffix.
    capture_raw: bool,
}

const INPUT_BUFFER_CAPACITY: usize = 1024;

impl VtProcessor {
    #[must_use]
    pub fn new(session_runtime_tx: UnboundedSender<RuntimeAction>) -> Self {
        VtProcessor {
            cursor_style: Style {
                fg: Color::DefaultForeground { bold: false },
                bg: Color::DefaultBackground,
            },
            buf: String::with_capacity(INPUT_BUFFER_CAPACITY),
            buf_raw: Vec::with_capacity(INPUT_BUFFER_CAPACITY),
            span_info: Vec::new(),
            link_info: Vec::new(),
            cursor_link: None,
            link_open_pos: 0,
            pending_cr: None,
            session_runtime_tx,
            raw_wanted: None,
            capture_raw: true,
        }
    }

    /// Ties raw capture to the given flag (the trigger manager's "any raw
    /// pattern exists" bit). Takes effect at the next line boundary.
    pub fn set_raw_wanted_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.capture_raw = flag.load(std::sync::atomic::Ordering::Relaxed);
        self.raw_wanted = Some(flag);
    }

    /// Re-latch `capture_raw` from the shared flag. Called wherever `buf_raw`
    /// empties (line commit, prompt commit, buffer flush), keeping each line's
    /// raw form all-or-nothing.
    fn refresh_capture_raw(&mut self) {
        if let Some(flag) = &self.raw_wanted {
            self.capture_raw = flag.load(std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// Whether raw bytes are currently being captured. The byte loop hoists
    /// its per-byte push behind this — sound because the flag only changes
    /// from the session thread, never mid-run.
    #[must_use]
    pub fn capture_raw(&self) -> bool {
        self.capture_raw
    }

    /// Close the active link's current range into `link_info` (empty ranges
    /// are dropped). `keep_active` retains the link across the boundary — a
    /// line commit inside a still-open link — while an explicit `ESC]8;;`
    /// ends it.
    fn close_link_range(&mut self, keep_active: bool) {
        if let Some(action) = &self.cursor_link {
            if self.buf.len() > self.link_open_pos {
                self.link_info.push(LinkSpan {
                    begin_pos: self.link_open_pos,
                    end_pos: self.buf.len(),
                    action: action.clone(),
                });
            }
            if !keep_active {
                self.cursor_link = None;
            }
        }
        self.link_open_pos = self.buf.len();
    }

    /// Begin a link at the current buffer position, closing any open one (a
    /// second open without a close replaces it from that point).
    fn open_link(&mut self, action: LinkAction) {
        self.close_link_range(false);
        self.cursor_link = Some(action);
        self.link_open_pos = self.buf.len();
    }

    /// A carriage-return overprint: discard the open frame — the local pending
    /// bytes, and (via [`RuntimeAction::RetractIncomingPartialLine`]) any
    /// prefix already flushed upstream as a partial — so the text after the
    /// `\r` replaces it. Raw bytes that arrived after the `\r` (up to and
    /// including the character triggering the restart) belong to the new
    /// frame and are kept. The cursor style survives, as on a real terminal.
    fn restart_open_line(&mut self, raw_mark: usize) {
        self.buf.clear();
        self.span_info.clear();
        // An active link survives the overprint like the cursor style does;
        // ranges already banked for the superseded frame are discarded.
        self.link_info.clear();
        self.link_open_pos = 0;
        self.buf_raw.drain(..raw_mark.min(self.buf_raw.len()));
        self.session_runtime_tx
            .send(RuntimeAction::RetractIncomingPartialLine)
            .unwrap();
    }

    fn change_style(&mut self, new_style: Style) {
        self.span_info.push(VtSpan {
            begin_pos: match self.span_info.last() {
                Some(span_info) => span_info.end_pos,
                None => 0,
            },
            end_pos: self.buf.len(),
            style: self.cursor_style,
        });

        self.cursor_style = new_style;
    }

    pub fn consume_into_pending_line(&mut self) -> StyledLine {
        self.change_style(self.cursor_style);
        // A link still open at the boundary contributes its range so far and
        // stays active: its next range begins at 0 on the next line.
        self.close_link_range(true);
        let mut line = StyledLine::new_with_raw(
            &self.buf,
            self.span_info.drain(..).collect(),
            self.capture_raw.then_some(self.buf_raw.as_slice()),
        );
        line.links = self.link_info.drain(..).collect();
        self.link_open_pos = 0;
        line
    }

    /// Notifies that the end of a buffer of incoming data has been reached.
    ///
    /// This finalizes any pending partial line and sends it, then requests a repaint.
    ///
    /// # Panics
    ///
    /// Panics if the `session_runtime_tx` channel is closed (i.e., the session runtime has been dropped).
    pub fn notify_end_of_buffer(&mut self) {
        let pending_line = Arc::new(self.consume_into_pending_line());
        if !self.buf.is_empty() {
            self.session_runtime_tx
                .send(RuntimeAction::HandleIncomingPartialLine(pending_line))
                .unwrap();
            self.buf.clear();
            self.buf_raw.clear();
            self.buf.shrink_to(INPUT_BUFFER_CAPACITY);
            self.buf_raw.shrink_to(INPUT_BUFFER_CAPACITY);
            self.refresh_capture_raw();
            // The frame a pending `\r` marked was just flushed upstream as a
            // partial; the restart's retraction covers it, and no local raw
            // bytes remain to drop.
            if self.pending_cr.is_some() {
                self.pending_cr = Some(0);
            }
        }
        self.session_runtime_tx
            .send(RuntimeAction::RequestRepaint)
            .unwrap();
    }

    /// Commit the pending bytes as a **prompt**: emit them on the partial-line path (so
    /// `prompt:`-flagged triggers fire) and reset the buffers so the next bytes start a fresh line.
    ///
    /// Driven by the telnet layer when it decodes a prompt boundary (`IAC GA` / `IAC EOR`) — a
    /// precise, server-sent signal, unlike the partial-line-at-end-of-buffer heuristic in
    /// [`notify_end_of_buffer`](Self::notify_end_of_buffer). Clearing the buffers here is what stops
    /// that heuristic from re-emitting the same prompt at end of read. A no-op when nothing is
    /// pending (e.g. a bare `IAC GA` with no preceding text).
    ///
    /// # Panics
    ///
    /// Panics if the `session_runtime_tx` channel is closed (the session runtime has been dropped).
    pub fn commit_prompt(&mut self) {
        // A prompt boundary finalizes the line; a `\r` just before it must
        // not overwrite what follows.
        self.pending_cr = None;
        if self.buf.is_empty() {
            return;
        }
        let pending_line = Arc::new(self.consume_into_pending_line());
        self.session_runtime_tx
            .send(RuntimeAction::HandleIncomingPartialLine(pending_line))
            .unwrap();
        self.buf.clear();
        self.buf_raw.clear();
        self.buf.shrink_to(INPUT_BUFFER_CAPACITY);
        self.buf_raw.shrink_to(INPUT_BUFFER_CAPACITY);
        self.refresh_capture_raw();
    }

    fn commit_line(&mut self) {
        let pending_line = Arc::new(self.consume_into_pending_line());
        self.session_runtime_tx
            .send(RuntimeAction::HandleIncomingLine(pending_line))
            .unwrap();
        self.buf.clear();
        self.buf_raw.clear();
        self.refresh_capture_raw();
    }

    fn push_incoming_char(&mut self, ch: char) {
        self.buf.push(ch);
    }

    pub fn push_raw_incoming_byte(&mut self, byte: u8) {
        if self.capture_raw {
            self.buf_raw.push(byte);
        }
    }
}

impl VTActor for VtProcessor {
    fn print(&mut self, b: char) {
        if let Some(raw_mark) = self.pending_cr.take() {
            self.restart_open_line(raw_mark);
        }
        self.push_incoming_char(b);
    }

    fn execute_c0_or_c1(&mut self, control: u8) {
        match control {
            b'\n' => {
                self.pending_cr = None;
                self.commit_line();
            }
            b'\r' => self.pending_cr = Some(self.buf_raw.len()),
            _ => {}
        }
    }

    fn dcs_hook(
        &mut self,
        _byte: u8,
        _params: &[i64],
        _intermediates: &[u8],
        _ignored_excess_intermediates: bool,
    ) {
    }

    fn dcs_put(&mut self, _byte: u8) {}

    fn dcs_unhook(&mut self) {}

    fn esc_dispatch(
        &mut self,
        _params: &[i64],
        _intermediates: &[u8],
        _ignored_excess_intermediates: bool,
        _byte: u8,
    ) {
    }

    fn csi_dispatch(&mut self, params: &[CsiParam], _parameters_truncated: bool, byte: u8) {
        if byte == b'm' {
            let new_style = sgr::process(self.cursor_style, params);
            self.change_style(new_style);
        }
    }

    fn osc_dispatch(&mut self, params: &[&[u8]]) {
        // OSC 8 hyperlinks: `8 ; params ; URI`. vtparse splits the payload on
        // every `;`, but a URI may itself contain them — everything past the
        // second separator is the URI, rejoined. The params field (id=, …) is
        // accepted and unused. All other OSC selectors are ignored.
        if params.first() != Some(&&b"8"[..]) {
            return;
        }
        // A well-formed OSC 8 is `8 ; params ; URI`; anything shorter (a
        // truncated `ESC]8;` or bare `ESC]8`) is treated as a close so a
        // degenerate sequence can't leave a link open over later lines.
        if params.len() < 3 {
            self.close_link_range(false);
            return;
        }
        let uri = params[2..].join(&b';');
        if uri.is_empty() {
            self.close_link_range(false);
            return;
        }
        match link_action_for_uri(&String::from_utf8_lossy(&uri)) {
            Some(action) => self.open_link(action),
            // Unsupported scheme: the text still displays, unlinked.
            None => self.close_link_range(false),
        }
    }

    fn apc_dispatch(&mut self, _data: Vec<u8>) {}
}

#[cfg(test)]
mod tests {
    use super::{Color, VtProcessor};
    use crate::session::runtime::RuntimeAction;
    use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};
    use vtparse::VTParser;

    struct Harness {
        parser: VTParser,
        processor: VtProcessor,
        rx: UnboundedReceiver<RuntimeAction>,
    }

    fn harness() -> Harness {
        let (tx, rx) = unbounded_channel();
        Harness {
            parser: VTParser::new(),
            processor: VtProcessor::new(tx),
            rx,
        }
    }

    impl Harness {
        /// Mirrors the connection's byte loop: raw bytes (minus CR/LF) are
        /// pushed before the parser sees each byte.
        fn feed(&mut self, bytes: &[u8]) {
            for &b in bytes {
                if b != b'\n' && b != b'\r' {
                    self.processor.push_raw_incoming_byte(b);
                }
                self.parser.parse_byte(b, &mut self.processor);
            }
        }

        fn actions(&mut self) -> Vec<RuntimeAction> {
            let mut out = Vec::new();
            while let Ok(action) = self.rx.try_recv() {
                out.push(action);
            }
            out
        }
    }

    /// The committed/partial line texts and where retractions fall between them.
    fn transcript(actions: &[RuntimeAction]) -> Vec<String> {
        actions
            .iter()
            .filter_map(|action| match action {
                RuntimeAction::HandleIncomingLine(line) => Some(format!("line:{}", line.text)),
                RuntimeAction::HandleIncomingPartialLine(line) => {
                    Some(format!("partial:{}", line.text))
                }
                RuntimeAction::RetractIncomingPartialLine => Some("retract".to_string()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn bare_cr_overprints_the_open_line() {
        let mut h = harness();
        h.feed(b"10%\r20%\r100%\n");
        assert_eq!(
            transcript(&h.actions()),
            ["retract", "retract", "line:100%"]
        );
    }

    #[test]
    fn crlf_commits_normally() {
        let mut h = harness();
        h.feed(b"a\r\nb\r\n");
        assert_eq!(transcript(&h.actions()), ["line:a", "line:b"]);
    }

    #[test]
    fn newline_then_cr_line_endings_commit_normally() {
        // Some servers terminate with \n\r; the stray \r restarts an empty
        // frame, which retracts nothing upstream and loses no text.
        let mut h = harness();
        h.feed(b"a\n\rb\n");
        let transcript = transcript(&h.actions());
        assert_eq!(transcript[0], "line:a");
        assert_eq!(transcript.last().unwrap(), "line:b");
    }

    #[test]
    fn cursor_style_survives_an_overprint() {
        let mut h = harness();
        h.feed(b"\x1b[31mold\rnew\n");
        let actions = h.actions();
        let line = actions
            .iter()
            .find_map(|action| match action {
                RuntimeAction::HandleIncomingLine(line) => Some(line.clone()),
                _ => None,
            })
            .expect("a committed line");
        assert_eq!(line.text, "new");
        assert!(
            line.spans.iter().all(|span| matches!(
                span.style.fg,
                Color::Ansi {
                    color: super::sgr::AnsiColor::Red,
                    bold: false
                }
            )),
            "the SGR set before the overprint must still color the new frame: {:?}",
            line.spans
        );
        assert_eq!(line.raw(), Some("new"));
    }

    #[test]
    fn raw_capture_follows_the_wanted_flag_at_line_boundaries() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let flag = Arc::new(AtomicBool::new(false));
        let mut h = harness();
        h.processor.set_raw_wanted_flag(flag.clone());

        // No raw trigger registered: the wire bytes are not copied.
        h.feed(b"\x1b[31mplain\x1b[0m\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "plain");
        assert_eq!(lines[0].raw(), None);

        // The flag flips mid-line: the in-flight line stays complete-or-absent
        // (absent), and capture starts with the next line.
        h.feed(b"mid");
        flag.store(true, Ordering::Relaxed);
        h.feed(b"line\n\x1b[32mnext\x1b[0m\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].raw(), None, "flag flips apply at line boundaries");
        assert_eq!(lines[1].raw(), Some("\x1b[32mnext\x1b[0m"));

        // And back off: the latch was taken at the last commit, so one more
        // line still captures (harmless — it's yesterday's behavior), and the
        // flip settles at the next boundary.
        flag.store(false, Ordering::Relaxed);
        h.feed(b"latched\nafter\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].raw(), Some("latched"));
        assert_eq!(lines[1].raw(), None);
    }

    #[test]
    fn overprint_after_a_flushed_partial_retracts_it() {
        let mut h = harness();
        h.feed(b"10%");
        h.processor.notify_end_of_buffer();
        h.feed(b"\r20%\n");
        assert_eq!(
            transcript(&h.actions()),
            ["partial:10%", "retract", "line:20%"]
        );
    }

    #[test]
    fn prompt_boundary_clears_a_pending_cr() {
        let mut h = harness();
        h.feed(b"> \r");
        h.processor.commit_prompt();
        h.feed(b"ok\n");
        assert_eq!(transcript(&h.actions()), ["partial:> ", "line:ok"]);
    }

    use crate::session::styled_line::{LinkAction, StyledLine};

    fn committed_lines(actions: &[RuntimeAction]) -> Vec<std::sync::Arc<StyledLine>> {
        actions
            .iter()
            .filter_map(|action| match action {
                RuntimeAction::HandleIncomingLine(line) => Some(line.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn osc8_http_link_spans_the_enclosed_text() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com\x1b\\click me\x1b]8;;\x1b\\ done\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "click me done");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(lines[0].links[0].begin_pos, 0);
        assert_eq!(lines[0].links[0].end_pos, "click me".len());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::OpenUrl(std::sync::Arc::from("https://example.com"))
        );
    }

    #[test]
    fn osc8_uri_may_contain_semicolons_and_bel_terminates() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com/a;b=1\x07x\x1b]8;;\x07\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::OpenUrl(std::sync::Arc::from("https://example.com/a;b=1"))
        );
    }

    #[test]
    fn osc8_link_continues_across_a_line_commit() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com\x1b\\one\ntwo\x1b]8;;\x1b\\!\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "one");
        assert_eq!(
            (lines[0].links[0].begin_pos, lines[0].links[0].end_pos),
            (0, 3)
        );
        assert_eq!(lines[1].text, "two!");
        assert_eq!(
            (lines[1].links[0].begin_pos, lines[1].links[0].end_pos),
            (0, 3),
            "the continuation restarts at column 0 and ends at the close"
        );
    }

    #[test]
    fn osc8_send_scheme_percent_decodes_into_server_send() {
        let mut h = harness();
        h.feed(b"\x1b]8;;send:say%20hello%2C%20world\x1b\\hi\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(
            lines[0].links[0].action,
            LinkAction::ServerSend(std::sync::Arc::from("say hello, world"))
        );
    }

    #[test]
    fn osc8_unsupported_schemes_render_plain_text() {
        let mut h = harness();
        h.feed(b"\x1b]8;;file:///etc/passwd\x1b\\name\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "name");
        assert!(lines[0].links.is_empty());
    }

    #[test]
    fn osc8_oversized_uri_is_ignored() {
        let mut payload = b"\x1b]8;;https://example.com/".to_vec();
        payload.extend(std::iter::repeat_n(b'x', 9000));
        payload.extend_from_slice(b"\x1b\\text\x1b]8;;\x1b\\\n");
        let mut h = harness();
        h.feed(&payload);
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "text");
        assert!(lines[0].links.is_empty());
    }

    #[test]
    fn osc8_multibyte_uri_does_not_panic() {
        // A URI of multibyte UTF-8 whose bytes straddle the scheme-prefix
        // lengths must not panic the scheme check; it is simply unlinked.
        let mut h = harness();
        h.feed("\x1b]8;;\u{e9}\u{e9}\u{e9}\u{e9}\x1b\\text\x1b]8;;\x1b\\\n".as_bytes());
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "text");
        assert!(lines[0].links.is_empty());
    }

    #[test]
    fn osc8_link_survives_a_cr_overprint() {
        let mut h = harness();
        h.feed(b"\x1b]8;;https://example.com\x1b\\old\rnew\x1b]8;;\x1b\\\n");
        let lines = committed_lines(&h.actions());
        assert_eq!(lines[0].text, "new");
        assert_eq!(lines[0].links.len(), 1);
        assert_eq!(
            (lines[0].links[0].begin_pos, lines[0].links[0].end_pos),
            (0, 3)
        );
    }
}
