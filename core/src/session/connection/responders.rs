//! Subnegotiation responders — the stateful answers behind the options `accept_local` agrees
//! to. The telnet parser (`telnet.rs`) stays a pure byte-stream state machine; when an accepted
//! option requires a *reply payload* (TTYPE's `IS` responses, NAWS's dimension report), the
//! logic and its small per-connection state live here, driven from the connection's
//! [`TelnetSink`](super::telnet::TelnetSink) hooks. Replies are framed with
//! [`frame_subnegotiation`] into the same buffer negotiation answers ride, so they reach the
//! wire in stream order.
//!
//! Like the parser, this module is dependency-light and unit-testable in isolation. The design
//! brief is `docs/telnet.md` §2.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::telnet::{frame_subnegotiation, option};

/// TTYPE subnegotiation command bytes (RFC 1091).
pub mod ttype {
    /// "Here is my terminal type" — client → server, answering a `SEND`.
    pub const IS: u8 = 0;
    /// "Send me your (next) terminal type" — server → client.
    pub const SEND: u8 = 1;
}

/// MTTS capability bits, advertised in the third TTYPE response
/// (`MTTS <bitvector>`; <https://tintin.mudhalla.net/protocols/mtts/>).
pub mod mtts {
    /// Client supports ANSI color codes.
    pub const ANSI: u16 = 1;
    /// Client is using UTF-8 character encoding.
    pub const UTF8: u16 = 4;
    /// Client supports all xterm 256 color codes.
    pub const COLORS_256: u16 = 8;
    /// Client supports truecolor codes using semicolon notation.
    pub const TRUECOLOR: u16 = 256;
    /// Client is using a secure (TLS) connection.
    pub const SSL: u16 = 2048;

    /// The bitvector smudgy truthfully claims. Deliberately **not** claimed: VT100 (2 — the
    /// display is line-oriented, no cursor addressing), mouse tracking (16), OSC color
    /// palette (32 — OSC 4/104 are deliberately unsupported), screen reader (64), proxy
    /// (128), MNES (512), and MSLP (1024). `secure` ORs in [`SSL`] when the game connection
    /// runs over TLS (`GameStream::Tls`).
    #[must_use]
    pub const fn bitvector(secure: bool) -> u16 {
        let base = ANSI | UTF8 | COLORS_256 | TRUECOLOR;
        if secure { base | SSL } else { base }
    }
}

/// The client name reported in the first TTYPE `IS` response — uppercase per the MTTS
/// convention; server-side client-usage stats key on this string.
pub const CLIENT_NAME: &str = "SMUDGY";

/// The terminal type reported in the second TTYPE `IS` response: the strongest truthful
/// color claim, legible to servers that key on terminal-type names instead of MTTS.
pub const TERMINAL_TYPE: &str = "ANSI-TRUECOLOR";

/// The window size reported before the UI's first real measurement arrives — the telnet
/// convention for "a normal terminal".
pub const DEFAULT_DIMS: (u16, u16) = (80, 24);

/// CHARSET subnegotiation commands (RFC 2066) and the request responder.
pub mod charset {
    use encoding_rs::{Encoding, UTF_8};

    use super::super::telnet::frame_subnegotiation;
    use super::option;

    /// "Please pick one of these charsets" — the server's offer.
    pub const REQUEST: u8 = 1;
    /// "I accept this charset" — our pick, echoed by label.
    pub const ACCEPTED: u8 = 2;
    /// "None of those" — no offered label is supported.
    pub const REJECTED: u8 = 3;

    /// The `[TTABLE]` marker an RFC 2066 REQUEST may carry before the separator.
    /// Translation tables are a dead letter nobody implements; a request carrying one is
    /// answered `REJECTED` outright.
    const TTABLE_MARKER: &[u8] = b"[TTABLE]";

    /// Answer one CHARSET `REQUEST`, framing `ACCEPTED <label>` or `REJECTED` into
    /// `replies` and returning the encoding to switch the connection to (`None` on
    /// reject). Payload shape: `<sep> name <sep> name …` where the first byte is the
    /// separator; UTF-8 is preferred whenever offered, otherwise the first label
    /// `encoding_rs` resolves wins. Labels echo back exactly as the server spelled them.
    pub fn answer_request(payload: &[u8], replies: &mut Vec<u8>) -> Option<&'static Encoding> {
        if let Some((label, encoding)) = choose(payload) {
            let mut reply = Vec::with_capacity(label.len() + 1);
            reply.push(ACCEPTED);
            reply.extend_from_slice(label);
            frame_subnegotiation(option::CHARSET, &reply, replies);
            Some(encoding)
        } else {
            frame_subnegotiation(option::CHARSET, &[REJECTED], replies);
            None
        }
    }

    /// The `(label, encoding)` pick for a REQUEST payload, or `None` when nothing offered
    /// is supported (or the request is malformed / carries a TTABLE).
    ///
    /// `for_label_no_replacement`, not `for_label`: the WHATWG mapping resolves the
    /// ISO-2022-CN/KR and HZ labels to the *replacement* encoding, whose decoder collapses
    /// every input run to a single U+FFFD — accepting one would destroy the whole session's
    /// feed. Those labels must be REJECTED like any other unsupported charset.
    fn choose(payload: &[u8]) -> Option<(&[u8], &'static Encoding)> {
        if payload.starts_with(TTABLE_MARKER) {
            return None;
        }
        let (&sep, names) = payload.split_first()?;
        let mut first_supported = None;
        for label in names.split(|&b| b == sep).filter(|l| !l.is_empty()) {
            if let Some(encoding) = Encoding::for_label_no_replacement(label) {
                if encoding == UTF_8 {
                    return Some((label, encoding));
                }
                if first_supported.is_none() {
                    first_supported = Some((label, encoding));
                }
            }
        }
        first_supported
    }
}

/// Pack a `(cols, rows)` pair into the `u32` the cross-thread dimension cell holds.
#[must_use]
pub const fn pack_dims(cols: u16, rows: u16) -> u32 {
    let c = cols.to_be_bytes();
    let r = rows.to_be_bytes();
    u32::from_be_bytes([c[0], c[1], r[0], r[1]])
}

/// The inverse of [`pack_dims`].
#[must_use]
pub const fn unpack_dims(packed: u32) -> (u16, u16) {
    let b = packed.to_be_bytes();
    (
        u16::from_be_bytes([b[0], b[1]]),
        u16::from_be_bytes([b[2], b[3]]),
    )
}

/// Per-connection responder state, owned by the connect task alongside the
/// [`TelnetParser`](super::telnet::TelnetParser) so it persists across reads and dies with
/// the connection (a fresh connection always renegotiates from scratch).
///
/// Window dimensions are read from the session's shared size cell (written by the runtime
/// from UI reports) at the moment a report is due — the cell is the single source of truth,
/// so a report is never staler than the last UI report, and there is no per-connection copy
/// to fall out of sync.
#[derive(Debug)]
pub struct ProtocolState {
    /// Position in the TTYPE `IS` cycle: 0 = client name, 1 = terminal type, 2 = the MTTS
    /// bitvector, repeated verbatim thereafter (the repetition is the end-of-list signal).
    ttype_cursor: u8,
    /// Whether this connection is over TLS — sets the MTTS `SSL` bit (the advertisement must
    /// reflect the live transport).
    secure: bool,
    /// The session's current main-pane character grid, packed with [`pack_dims`]. Shared
    /// with the runtime, which stores UI grid reports into it.
    window_size: Arc<AtomicU32>,
    /// The dimensions most recently put on the wire, so a size-change wakeup only sends a
    /// NAWS update when the current size actually differs.
    last_sent_dims: Option<(u16, u16)>,
}

impl ProtocolState {
    #[must_use]
    pub const fn new(window_size: Arc<AtomicU32>, secure: bool) -> Self {
        Self {
            ttype_cursor: 0,
            secure,
            window_size,
            last_sent_dims: None,
        }
    }

    /// A `ProtocolState` over a private size cell holding `dims` — for tests and benches
    /// that have no runtime to share a cell with. Plain (non-TLS).
    #[must_use]
    pub fn with_fixed_dims(dims: (u16, u16)) -> Self {
        Self::new(Arc::new(AtomicU32::new(pack_dims(dims.0, dims.1))), false)
    }

    /// The current window size, clamped to `1×1` — a zero dimension is a protocol hazard
    /// (and reachable if a degenerate UI layout ever reports one), so it never reaches the
    /// wire regardless of what the cell holds.
    fn current_dims(&self) -> (u16, u16) {
        let (cols, rows) = unpack_dims(self.window_size.load(Ordering::Relaxed));
        (cols.max(1), rows.max(1))
    }

    /// Answer one TTYPE `SEND` with the next `IS` response in the MTTS cycle, framed into
    /// `replies`.
    pub fn on_ttype_send(&mut self, replies: &mut Vec<u8>) {
        let name = match self.ttype_cursor {
            0 => CLIENT_NAME.to_string(),
            1 => TERMINAL_TYPE.to_string(),
            _ => format!("MTTS {}", mtts::bitvector(self.secure)),
        };
        self.ttype_cursor = self.ttype_cursor.saturating_add(1).min(2);
        let mut payload = Vec::with_capacity(name.len() + 1);
        payload.push(ttype::IS);
        payload.extend_from_slice(name.as_bytes());
        frame_subnegotiation(option::TTYPE, &payload, replies);
    }

    /// Restart the TTYPE cycle. Called when the option is disabled, so a renegotiation
    /// re-reports from the client name.
    pub fn reset_ttype(&mut self) {
        self.ttype_cursor = 0;
    }

    /// The unconditional NAWS report RFC 1073 requires the moment the option turns on.
    pub fn send_naws(&mut self, replies: &mut Vec<u8>) {
        let dims = self.current_dims();
        self.last_sent_dims = Some(dims);
        frame_naws(dims, replies);
    }

    /// A size-change wakeup while NAWS is on: frame a report only if the current size
    /// differs from what is already on the wire. Returns whether a report was framed.
    pub fn send_naws_if_changed(&mut self, replies: &mut Vec<u8>) -> bool {
        let dims = self.current_dims();
        if self.last_sent_dims == Some(dims) {
            return false;
        }
        self.last_sent_dims = Some(dims);
        frame_naws(dims, replies);
        true
    }
}

/// Frame one NAWS dimension report (RFC 1073): two 16-bit big-endian values.
/// `frame_subnegotiation` doubles any `0xFF` byte a dimension of 255/511/… produces.
fn frame_naws((cols, rows): (u16, u16), replies: &mut Vec<u8>) {
    let c = cols.to_be_bytes();
    let r = rows.to_be_bytes();
    frame_subnegotiation(option::NAWS, &[c[0], c[1], r[0], r[1]], replies);
}

#[cfg(test)]
mod tests {
    use super::super::telnet::command::{IAC, SB, SE};
    use super::super::telnet::option::{NAWS, TTYPE};
    use super::{CLIENT_NAME, ProtocolState, TERMINAL_TYPE, mtts, pack_dims, ttype, unpack_dims};

    /// Strip one `IAC SB <opt> … IAC SE` frame, returning the option and payload.
    fn unframe(buf: &[u8]) -> (u8, Vec<u8>) {
        assert_eq!(&buf[..2], &[IAC, SB]);
        assert_eq!(&buf[buf.len() - 2..], &[IAC, SE]);
        (buf[2], buf[3..buf.len() - 2].to_vec())
    }

    #[test]
    fn mtts_bitvector_is_269_and_2317_when_secure() {
        assert_eq!(mtts::bitvector(false), 269);
        assert_eq!(mtts::bitvector(true), 2317);
    }

    #[test]
    fn ttype_reports_the_ssl_bit_on_a_secure_connection() {
        let cell = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(pack_dims(80, 24)));
        let mut state = ProtocolState::new(cell, true);
        // Advance to the MTTS entry (client name, terminal type, then MTTS).
        let mut replies = Vec::new();
        state.on_ttype_send(&mut replies);
        state.on_ttype_send(&mut replies);
        replies.clear();
        state.on_ttype_send(&mut replies);
        let (_, payload) = unframe(&replies);
        assert_eq!(&payload[1..], b"MTTS 2317", "SSL bit set over TLS");
    }

    #[test]
    fn ttype_cycle_reports_name_type_mtts_then_repeats() {
        let mut state = ProtocolState::with_fixed_dims((80, 24));
        let expected = [
            CLIENT_NAME.to_string(),
            TERMINAL_TYPE.to_string(),
            "MTTS 269".to_string(),
            "MTTS 269".to_string(), // repetition signals end-of-list
        ];
        for want in expected {
            let mut replies = Vec::new();
            state.on_ttype_send(&mut replies);
            let (opt, payload) = unframe(&replies);
            assert_eq!(opt, TTYPE);
            assert_eq!(payload[0], ttype::IS);
            assert_eq!(&payload[1..], want.as_bytes());
        }
    }

    #[test]
    fn ttype_cycle_resets_on_renegotiation() {
        let mut state = ProtocolState::with_fixed_dims((80, 24));
        let mut replies = Vec::new();
        state.on_ttype_send(&mut replies);
        state.on_ttype_send(&mut replies);
        state.reset_ttype();
        replies.clear();
        state.on_ttype_send(&mut replies);
        let (_, payload) = unframe(&replies);
        assert_eq!(&payload[1..], CLIENT_NAME.as_bytes());
    }

    #[test]
    fn naws_frames_big_endian_dimensions() {
        let mut state = ProtocolState::with_fixed_dims((120, 40));
        let mut replies = Vec::new();
        state.send_naws(&mut replies);
        let (opt, payload) = unframe(&replies);
        assert_eq!(opt, NAWS);
        assert_eq!(payload, vec![0, 120, 0, 40]);
    }

    #[test]
    fn naws_doubles_a_255_dimension_byte_on_the_wire() {
        // 255 columns puts a literal 0xFF in the payload; the frame must carry it doubled
        // (IAC IAC), and the un-doubled logical payload must still be 4 bytes.
        let mut state = ProtocolState::with_fixed_dims((255, 24));
        let mut replies = Vec::new();
        state.send_naws(&mut replies);
        // On-wire: IAC SB NAWS 0x00 0xFF 0xFF 0x00 0x18 IAC SE (the 0xFF doubled).
        assert_eq!(
            replies,
            vec![IAC, SB, NAWS, 0x00, 0xFF, 0xFF, 0x00, 0x18, IAC, SE]
        );
    }

    #[test]
    fn naws_wakeup_sends_only_on_a_real_change() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicU32, Ordering};
        let cell = Arc::new(AtomicU32::new(super::pack_dims(80, 24)));
        let mut state = ProtocolState::new(cell.clone(), false);

        // The enable-time report is unconditional and primes the dedupe.
        let mut replies = Vec::new();
        state.send_naws(&mut replies);
        assert!(!replies.is_empty());

        // A wakeup with an unchanged cell frames nothing…
        replies.clear();
        assert!(!state.send_naws_if_changed(&mut replies));
        assert!(replies.is_empty());

        // …and a real change frames exactly the new size.
        cell.store(super::pack_dims(100, 30), Ordering::Relaxed);
        assert!(state.send_naws_if_changed(&mut replies));
        let (_, payload) = unframe(&replies);
        assert_eq!(payload, vec![0, 100, 0, 30]);
    }

    #[test]
    fn zero_dimensions_never_reach_the_wire() {
        // A degenerate cell value (0×0) clamps to 1×1 on every read path.
        let mut state = ProtocolState::with_fixed_dims((0, 0));
        let mut replies = Vec::new();
        state.send_naws(&mut replies);
        let (_, payload) = unframe(&replies);
        assert_eq!(payload, vec![0, 1, 0, 1], "0x0 must never reach the wire");
    }

    #[test]
    fn charset_request_prefers_utf8_over_earlier_offers() {
        use super::super::telnet::option::CHARSET;
        use super::charset;
        let mut replies = Vec::new();
        let enc = charset::answer_request(b";big5;UTF-8;iso-8859-1", &mut replies);
        assert_eq!(enc, Some(encoding_rs::UTF_8));
        let (opt, payload) = unframe(&replies);
        assert_eq!(opt, CHARSET);
        assert_eq!(payload[0], charset::ACCEPTED);
        assert_eq!(
            &payload[1..],
            b"UTF-8",
            "the label echoes as the server spelled it"
        );
    }

    #[test]
    fn charset_request_takes_the_first_resolvable_label() {
        use super::charset;
        let mut replies = Vec::new();
        // "NO-SUCH-CHARSET" resolves to nothing, so the first WHATWG-resolvable
        // offer (big5) wins over the later latin1.
        let enc = charset::answer_request(b" NO-SUCH-CHARSET big5 latin1", &mut replies);
        assert_eq!(enc, Some(encoding_rs::BIG5));
        let (_, payload) = unframe(&replies);
        assert_eq!(&payload[1..], b"big5");
    }

    #[test]
    fn charset_request_with_nothing_supported_or_a_ttable_is_rejected() {
        use super::charset;
        for payload in [&b";EBCDIC-US;KLINGON"[..], b"[TTABLE]\x01;UTF-8", b""] {
            let mut replies = Vec::new();
            assert_eq!(charset::answer_request(payload, &mut replies), None);
            let (_, reply) = unframe(&replies);
            assert_eq!(reply, vec![charset::REJECTED], "payload {payload:02x?}");
        }
    }

    /// The WHATWG mapping resolves these labels to the *replacement* encoding, whose
    /// decoder turns the whole session into U+FFFD — they must be REJECTED, exactly like
    /// unknown labels.
    #[test]
    fn charset_request_rejects_replacement_encoding_labels() {
        use super::charset;
        for label in ["iso-2022-cn", "iso-2022-kr", "hz-gb-2312", "replacement"] {
            let mut replies = Vec::new();
            let payload = format!(";{label}");
            assert_eq!(
                charset::answer_request(payload.as_bytes(), &mut replies),
                None,
                "label {label} must not be accepted"
            );
            let (_, reply) = unframe(&replies);
            assert_eq!(reply, vec![charset::REJECTED]);
        }
    }

    #[test]
    fn dims_pack_round_trips() {
        for dims in [(80u16, 24u16), (0, 0), (u16::MAX, 1), (255, 511)] {
            assert_eq!(unpack_dims(pack_dims(dims.0, dims.1)), dims);
        }
    }
}
