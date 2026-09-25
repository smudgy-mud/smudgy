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

use encoding_rs::Encoding;

use super::telnet::{frame_subnegotiation, option};

/// TTYPE subnegotiation command bytes (RFC 1091).
pub mod ttype {
    /// "Here is my terminal type" — client → server, answering a `SEND`.
    pub const IS: u8 = 0;
    /// "Send me your (next) terminal type" — server → client.
    pub const SEND: u8 = 1;
}

/// RFC 1572 NEW-ENVIRON responder, answering two vocabularies. The MNES
/// identity variables (<https://tintin.mudhalla.net/protocols/mnes/>) are
/// `VAR`s: MNES is the NEW-ENVIRON replacement for TTYPE cycling, and a server
/// that sees the option accepted may query identity here *instead of* ever
/// sending a TTYPE `SEND` — leaving these unanswered reads as "no terminal
/// type, no color support" and downgrades the session to bare ANSI. The OSC 8
/// capability variables used by current MUD servers are `USERVAR`s. Nothing
/// from the host process environment is ever exposed.
pub mod new_environ {
    use super::super::telnet::{frame_subnegotiation, option};
    use super::{CLIENT_NAME, TERMINAL_TYPE};

    pub const IS: u8 = 0;
    pub const SEND: u8 = 1;
    const VAR: u8 = 0;
    const VALUE: u8 = 1;
    const ESC: u8 = 2;
    const USERVAR: u8 = 3;

    const CAPABILITIES: &[(&[u8], &[u8])] = &[
        (b"OSC_HYPERLINKS", b"1"),
        (b"OSC_HYPERLINKS_COMPACT", b"1"),
        (b"OSC_HYPERLINKS_DISABLED", b"1"),
        (b"OSC_HYPERLINKS_MENU", b"1"),
        (b"OSC_HYPERLINKS_PRESETS", b"1"),
        (b"OSC_HYPERLINKS_PROMPT", b"1"),
        (b"OSC_HYPERLINKS_SELECTION", b"1"),
        (b"OSC_HYPERLINKS_SEND", b"1"),
        (b"OSC_HYPERLINKS_SPOILER", b"1"),
        (b"OSC_HYPERLINKS_STYLE_BASIC", b"1"),
        (b"OSC_HYPERLINKS_STYLE_STATES", b"1"),
        (b"OSC_HYPERLINKS_TOOLTIP", b"1"),
        // Smudgy extension: tooltip text accepts the safe SGR subset parsed by
        // `parse_link_tooltip_text`; active terminal controls are discarded.
        (b"OSC_HYPERLINKS_TOOLTIP_SGR", b"1"),
        (b"OSC_HYPERLINKS_VISIBILITY", b"1"),
    ];

    /// The MNES identity variables, in the order an answer-everything reply
    /// reports them.
    const MNES_NAMES: &[&[u8]] = &[
        b"CLIENT_NAME",
        b"CLIENT_VERSION",
        b"CHARSET",
        b"MTTS",
        b"TERMINAL_TYPE",
    ];

    /// The `(canonical name, value)` for an MNES identity variable, or `None`
    /// for any other name. `MTTS` and `CHARSET` are live values (the TLS bit,
    /// the active wire encoding), so they arrive as parameters.
    fn mnes_value(name: &[u8], mtts: u16, charset: &str) -> Option<(&'static [u8], String)> {
        if name.eq_ignore_ascii_case(b"CLIENT_NAME") {
            Some((b"CLIENT_NAME", CLIENT_NAME.to_string()))
        } else if name.eq_ignore_ascii_case(b"CLIENT_VERSION") {
            Some((b"CLIENT_VERSION", env!("CARGO_PKG_VERSION").to_string()))
        } else if name.eq_ignore_ascii_case(b"CHARSET") {
            Some((b"CHARSET", charset.to_string()))
        } else if name.eq_ignore_ascii_case(b"MTTS") {
            Some((b"MTTS", mtts.to_string()))
        } else if name.eq_ignore_ascii_case(b"TERMINAL_TYPE") {
            Some((b"TERMINAL_TYPE", TERMINAL_TYPE.to_string()))
        } else {
            None
        }
    }

    /// Parse a SEND request body into `(type, name)` pairs — both `VAR` and
    /// `USERVAR`, since MNES requests identity as `VAR`s while the OSC 8
    /// convention requests capabilities as `USERVAR`s — unescaping RFC 1572
    /// `ESC` bytes. Parsing stops at a byte that is neither type marker.
    fn requested_vars(payload: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut requested = Vec::new();
        let mut cursor = 0;
        while cursor < payload.len() {
            let kind = payload[cursor];
            cursor += 1;
            if kind != VAR && kind != USERVAR {
                break;
            }
            let mut name = Vec::new();
            while cursor < payload.len() {
                match payload[cursor] {
                    VAR | USERVAR => break,
                    ESC => {
                        cursor += 1;
                        if let Some(&escaped) = payload.get(cursor) {
                            name.push(escaped);
                            cursor += 1;
                        }
                    }
                    byte => {
                        name.push(byte);
                        cursor += 1;
                    }
                }
            }
            requested.push((kind, name));
        }
        requested
    }

    fn push_entry(payload: &mut Vec<u8>, kind: u8, name: &[u8], value: &[u8]) {
        payload.push(kind);
        payload.extend_from_slice(name);
        payload.push(VALUE);
        payload.extend_from_slice(value);
    }

    /// Append the MNES identity variables, in the spec's order.
    fn push_identity(payload: &mut Vec<u8>, kind: u8, mtts: u16, charset: &str) {
        for &name in MNES_NAMES {
            if let Some((canonical, value)) = mnes_value(name, mtts, charset) {
                push_entry(payload, kind, canonical, value.as_bytes());
            }
        }
    }

    /// Append the OSC 8 capability catalogue.
    fn push_capabilities(payload: &mut Vec<u8>, kind: u8) {
        for &(name, value) in CAPABILITIES {
            push_entry(payload, kind, name, value);
        }
    }

    /// Answer a NEW-ENVIRON `SEND`. An empty request gets everything: the MNES
    /// identity as `VAR`s, then the capability catalogue as `USERVAR`s. A
    /// selective request is answered per recognized name in request order,
    /// echoing the type byte the server used (so its own table lookup matches
    /// however it typed the query) with the name's canonical spelling.
    /// Unrecognized names are omitted.
    ///
    /// A type byte carrying no name asks for every variable of that type (RFC 1572:
    /// "all variables of that type"), which is the form MNES documents for requesting
    /// the identity block. It is answered like the matching half of an empty request —
    /// omitting it would return the empty `IS` a server reads as "no terminal type, no
    /// color support", the very downgrade this responder exists to prevent.
    pub fn answer_send(request: &[u8], mtts: u16, charset: &str, replies: &mut Vec<u8>) {
        let mut payload = vec![IS];
        if request.is_empty() {
            push_identity(&mut payload, VAR, mtts, charset);
            push_capabilities(&mut payload, USERVAR);
        } else {
            for (kind, name) in requested_vars(request) {
                if name.is_empty() {
                    if kind == VAR {
                        push_identity(&mut payload, VAR, mtts, charset);
                    } else {
                        push_capabilities(&mut payload, USERVAR);
                    }
                } else if let Some((canonical, value)) = mnes_value(&name, mtts, charset) {
                    push_entry(&mut payload, kind, canonical, value.as_bytes());
                } else if let Some((canonical, value)) = CAPABILITIES
                    .iter()
                    .copied()
                    .find(|(catalogue_name, _)| catalogue_name.eq_ignore_ascii_case(&name))
                {
                    push_entry(&mut payload, kind, canonical, value);
                }
            }
        }
        frame_subnegotiation(option::NEW_ENVIRON, &payload, replies);
    }
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
    /// Client supports the Mud New-Environ Standard (the NEW-ENVIRON identity
    /// variables answered by [`super::new_environ::answer_send`]).
    pub const MNES: u16 = 512;
    /// Client is using a secure (TLS) connection.
    pub const SSL: u16 = 2048;

    /// The bitvector smudgy truthfully claims. Deliberately **not** claimed: VT100 (2 — the
    /// display is line-oriented, no cursor addressing), mouse tracking (16), OSC color
    /// palette (32 — OSC 4/104 are deliberately unsupported), screen reader (64), proxy
    /// (128), and MSLP (1024). `secure` ORs in [`SSL`] when the game connection
    /// runs over TLS (`GameStream::Tls`).
    #[must_use]
    pub const fn bitvector(secure: bool) -> u16 {
        let base = ANSI | UTF8 | COLORS_256 | TRUECOLOR | MNES;
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
    /// A translation table, offered in answer to a `REQUEST` carrying `[TTABLE]`.
    pub const TTABLE_IS: u8 = 4;
    /// "I cannot use that translation table." RFC 2066 §2 makes this one of the four
    /// messages every CHARSET implementation MUST support, even one that never sends a
    /// `TTABLE-IS` of its own — it is the only way to end an exchange a server opened with
    /// one, and silence would strand the server mid-subnegotiation (§5: another cannot
    /// start until this one completes).
    pub const TTABLE_REJECTED: u8 = 5;

    /// The `[TTABLE]` marker a REQUEST may carry, ahead of a version octet and then the
    /// ordinary charset list: RFC 2066 §2 gives the shape as
    /// `REQUEST { "[TTABLE ]" <Version> } <char set list>`.
    ///
    /// The marker is an *offer* — "answer with a translation table if you would rather" —
    /// not a replacement for the list. Declining the table is right (nobody implements
    /// them); declining the request is not, since §2 reserves `REJECTED` for a receiver
    /// "not capable of handling any of the specified character sets", and the list the
    /// marker precedes may well name one we speak.
    const TTABLE_MARKER: &[u8] = b"[TTABLE]";

    /// The separator our own REQUEST uses. RFC 2066 reads the byte after `REQUEST` as the
    /// separator for that message; a space is the RFC's own example and the form a legacy
    /// server is likeliest to parse.
    const REQUEST_SEPARATOR: u8 = b' ';

    /// The charsets our REQUEST offers, in preference order, for a connection configured to
    /// `configured`.
    ///
    /// UTF-8 alone by default. When the per-server `encoding` setting names something else it
    /// leads and UTF-8 follows: the setting is the user's stated preference, so a server that
    /// speaks it should honor it, while a server that cannot still gets a way off whatever
    /// legacy default it would otherwise have used.
    ///
    /// Deliberately short. Every label offered is one the server may pick, and outbound
    /// commands then have to *encode* into it — a wide offer list buys a little decoding
    /// fidelity and risks turning an em-dash in a user's command into a refused send.
    #[must_use]
    pub fn offer(configured: &'static Encoding) -> Vec<&'static Encoding> {
        if configured == UTF_8 {
            vec![UTF_8]
        } else {
            vec![configured, UTF_8]
        }
    }

    /// Frame our own `REQUEST` — the client half of RFC 2066's minimal implementation, owed
    /// the moment we answer `DO CHARSET` with `WILL CHARSET`.
    pub fn frame_request(configured: &'static Encoding, replies: &mut Vec<u8>) {
        let offered = offer(configured);
        let mut payload = Vec::with_capacity(
            1 + offered
                .iter()
                .map(|encoding| encoding.name().len() + 1)
                .sum::<usize>(),
        );
        payload.push(REQUEST);
        for encoding in offered {
            payload.push(REQUEST_SEPARATOR);
            payload.extend_from_slice(encoding.name().as_bytes());
        }
        frame_subnegotiation(option::CHARSET, &payload, replies);
    }

    /// The encoding a server's answer to *our* REQUEST selects, or `None` to stay put.
    ///
    /// `None` covers `REJECTED` (RFC 2066: nothing offered was supported), a malformed answer,
    /// and — the case worth the check — an `ACCEPTED` naming a charset we never offered. The
    /// server picks *from our list*; honoring anything else would let it move the whole
    /// session onto an encoding we may not be able to encode commands in. Labels are compared
    /// by the encoding they resolve to, not byte-wise, so a server echoing `utf8` or `UTF-8 `
    /// still matches.
    #[must_use]
    pub fn accepted_encoding(
        payload: &[u8],
        configured: &'static Encoding,
    ) -> Option<&'static Encoding> {
        let (&code, label) = payload.split_first()?;
        if code != ACCEPTED {
            return None;
        }
        let picked = Encoding::for_label_no_replacement(label)?;
        offer(configured).contains(&picked).then_some(picked)
    }

    /// Answer one CHARSET `REQUEST`, framing `ACCEPTED <label>` or `REJECTED` into
    /// `replies` and returning the encoding to switch the connection to (`None` on
    /// reject). Payload shape: `<sep> name <sep> name …` where the first byte is the
    /// separator. Labels echo back exactly as the server spelled them.
    ///
    /// `configured` is the per-server `encoding` setting, and it is preferred over UTF-8
    /// whenever the server offers both — the same order [`offer`] puts on the wire when we
    /// are the side asking, so the preference reads the same from either end.
    ///
    /// It is only a preference in one of those directions. When we drive, the *server* picks
    /// from our list, and §2 lets it "choose one based on its own preferences" rather than
    /// take the first it can handle — so a UTF-8-preferring server can still land the
    /// connection on UTF-8 with `configured` leading the offer. What this ordering removes is
    /// smudgy contradicting itself, not the server's last word.
    pub fn answer_request(
        payload: &[u8],
        configured: &'static Encoding,
        replies: &mut Vec<u8>,
    ) -> Option<&'static Encoding> {
        if let Some((label, encoding)) = choose(payload, configured) {
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
    /// Preference order: `configured` first — the user named it for this server, so a server
    /// able to speak it should — then UTF-8 wherever in the list it appears, then the first
    /// label `encoding_rs` resolves. The list order the server sent is only the tiebreak;
    /// RFC 2066 §2 leaves the pick to the receiver ("may choose one based on its own
    /// preferences").
    ///
    /// `for_label_no_replacement`, not `for_label`: the WHATWG mapping resolves the
    /// ISO-2022-CN/KR and HZ labels to the *replacement* encoding, whose decoder collapses
    /// every input run to a single U+FFFD — accepting one would destroy the whole session's
    /// feed. Those labels must be REJECTED like any other unsupported charset.
    fn choose<'a>(
        payload: &'a [u8],
        configured: &'static Encoding,
    ) -> Option<(&'a [u8], &'static Encoding)> {
        // Skip a leading `[TTABLE]` and the version octet behind it; what follows is an
        // ordinary charset list, and the table on offer is simply not taken up.
        let list = match payload.strip_prefix(TTABLE_MARKER) {
            Some(versioned) => versioned.split_first()?.1,
            None => payload,
        };
        let (&sep, names) = list.split_first()?;
        let mut utf8 = None;
        let mut first_supported = None;
        for label in names.split(|&b| b == sep).filter(|l| !l.is_empty()) {
            if let Some(encoding) = Encoding::for_label_no_replacement(label) {
                // Nothing later in the list can outrank the configured encoding, so the
                // first spelling of it settles the pick. (When `configured` *is* UTF-8 the
                // two preferences coincide and this is the UTF-8 early-out.)
                if encoding == configured {
                    return Some((label, encoding));
                }
                if encoding == UTF_8 && utf8.is_none() {
                    utf8 = Some((label, encoding));
                }
                if first_supported.is_none() {
                    first_supported = Some((label, encoding));
                }
            }
        }
        utf8.or(first_supported)
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
    /// Whether a CHARSET `REQUEST` of ours is outstanding. Gates the answer path, so an
    /// `ACCEPTED` nobody asked for can never move the connection's encoding.
    charset_requested: bool,
    /// Whether that outstanding request still holds outbound text (RFC 2066 §5). Set only
    /// when the offer could actually change our outbound encoding, and cleared by
    /// [`release_charset_hold`](Self::release_charset_hold) when the socket task gives up
    /// waiting — separate from `charset_requested`, so a late answer still lands.
    charset_hold: bool,
}

impl ProtocolState {
    #[must_use]
    pub const fn new(window_size: Arc<AtomicU32>, secure: bool) -> Self {
        Self {
            ttype_cursor: 0,
            secure,
            window_size,
            last_sent_dims: None,
            charset_requested: false,
            charset_hold: false,
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

    /// Answer a NEW-ENVIRON `SEND` (the MNES identity variables plus the OSC 8
    /// capability catalogue — see [`new_environ::answer_send`]), the MTTS
    /// bitvector reflecting this connection's transport. `charset` is the
    /// active wire encoding's name.
    pub fn on_new_environ_send(&self, request: &[u8], charset: &str, replies: &mut Vec<u8>) {
        new_environ::answer_send(request, mtts::bitvector(self.secure), charset, replies);
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

    /// Send our CHARSET `REQUEST`, framed into `replies` right behind the `WILL CHARSET` that
    /// occasioned it. RFC 2066's minimal implementation in full: a server that sent only `DO`
    /// may not request, so if we do not ask, no charset is ever negotiated.
    ///
    /// `active` is the encoding outbound text is being written in right now. If every charset
    /// offered *is* that encoding, no answer can change how we encode and the exchange is
    /// invisible to the write path; otherwise the request also takes the §5 outbound hold
    /// ([`charset_holds_outbound`](Self::charset_holds_outbound)).
    pub fn send_charset_request(
        &mut self,
        configured: &'static Encoding,
        active: &'static Encoding,
        replies: &mut Vec<u8>,
    ) {
        self.charset_requested = true;
        self.charset_hold = charset::offer(configured)
            .iter()
            .any(|offered| *offered != active);
        charset::frame_request(configured, replies);
    }

    /// Whether outbound text must wait for the answer to our `REQUEST` (RFC 2066 §5: "While a
    /// CHARSET subnegotiation is in progress, data SHOULD be queued").
    ///
    /// The rule matters in this direction and no other. A server changes how it *decodes* our
    /// stream the moment it sends `ACCEPTED`, and we have no way to mark that position in a
    /// stream flowing the other way — so for one round trip anything we write is read under a
    /// charset we did not encode it in. (The mirror case is safe for free: when the server
    /// requests and we answer, it switches on receiving our `ACCEPTED`, and everything sent
    /// before that arrives before it.)
    #[must_use]
    pub const fn charset_holds_outbound(&self) -> bool {
        self.charset_requested && self.charset_hold
    }

    /// Give up holding outbound text: the answer is taking too long, and a server that ignores
    /// a `REQUEST` outright is ordinary rather than exceptional. Only the hold is dropped — the
    /// request stays outstanding, so an answer that does eventually arrive is still honored
    /// (by then it describes a switch we can only make going forward anyway).
    pub const fn release_charset_hold(&mut self) {
        self.charset_hold = false;
    }

    /// The encoding the server's answer to our outstanding `REQUEST` switches us to, or `None`
    /// to stay on the configured one — including when no request of ours is outstanding, so an
    /// unsolicited `ACCEPTED` is inert. Either kind of answer closes the exchange.
    pub fn on_charset_answer(
        &mut self,
        payload: &[u8],
        configured: &'static Encoding,
    ) -> Option<&'static Encoding> {
        if !self.charset_requested {
            return None;
        }
        self.charset_requested = false;
        self.charset_hold = false;
        charset::accepted_encoding(payload, configured)
    }

    /// Drop any outstanding `REQUEST`. Two callers: the option going away (a later
    /// renegotiation then starts clean), and a server `REQUEST` arriving while ours is in
    /// flight — RFC 2066's simultaneous-request rule gives the server's the right of way, and
    /// the negative acknowledgment it owes ours is then nothing we need to wait for.
    pub fn reset_charset_request(&mut self) {
        self.charset_requested = false;
        self.charset_hold = false;
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
    use super::super::telnet::option::{NAWS, NEW_ENVIRON, TTYPE};
    use super::{
        CLIENT_NAME, ProtocolState, TERMINAL_TYPE, charset, mtts, new_environ, pack_dims, ttype,
        unpack_dims,
    };

    /// Strip one `IAC SB <opt> … IAC SE` frame, returning the option and payload.
    fn unframe(buf: &[u8]) -> (u8, Vec<u8>) {
        assert_eq!(&buf[..2], &[IAC, SB]);
        assert_eq!(&buf[buf.len() - 2..], &[IAC, SE]);
        (buf[2], buf[3..buf.len() - 2].to_vec())
    }

    #[test]
    fn mtts_bitvector_is_781_and_2829_when_secure() {
        // ANSI 1 + UTF-8 4 + 256-color 8 + truecolor 256 + MNES 512 = 781.
        assert_eq!(mtts::bitvector(false), 781);
        assert_eq!(mtts::bitvector(true), 2829);
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
        assert_eq!(&payload[1..], b"MTTS 2829", "SSL bit set over TLS");
    }

    #[test]
    fn new_environ_advertises_only_truthful_osc8_capabilities() {
        let mut replies = Vec::new();
        new_environ::answer_send(&[], mtts::bitvector(false), "UTF-8", &mut replies);
        let (option, payload) = unframe(&replies);
        assert_eq!(option, NEW_ENVIRON);
        assert_eq!(payload[0], new_environ::IS);
        assert!(
            payload
                .windows(b"OSC_HYPERLINKS_TOOLTIP".len())
                .any(|window| window == b"OSC_HYPERLINKS_TOOLTIP")
        );
        assert!(
            payload
                .windows(b"OSC_HYPERLINKS_PROMPT".len())
                .any(|window| window == b"OSC_HYPERLINKS_PROMPT")
        );
        for capability in [
            b"OSC_HYPERLINKS_COMPACT".as_slice(),
            b"OSC_HYPERLINKS_DISABLED".as_slice(),
            b"OSC_HYPERLINKS_MENU".as_slice(),
            b"OSC_HYPERLINKS_PRESETS".as_slice(),
            b"OSC_HYPERLINKS_SELECTION".as_slice(),
            b"OSC_HYPERLINKS_SPOILER".as_slice(),
            b"OSC_HYPERLINKS_STYLE_BASIC".as_slice(),
            b"OSC_HYPERLINKS_STYLE_STATES".as_slice(),
            b"OSC_HYPERLINKS_TOOLTIP_SGR".as_slice(),
            b"OSC_HYPERLINKS_VISIBILITY".as_slice(),
        ] {
            assert!(
                payload
                    .windows(capability.len())
                    .any(|window| window == capability)
            );
        }
    }

    /// RFC 1572 reads a type byte with no name as "all variables of that type", and MNES
    /// documents `SEND VAR` as the way to ask for the identity block. Answering it with an
    /// empty `IS` is exactly the "no terminal type, no color support" downgrade this
    /// responder exists to prevent.
    #[test]
    fn new_environ_send_var_with_no_name_returns_the_identity_block() {
        let mut all = Vec::new();
        new_environ::answer_send(&[], mtts::bitvector(false), "UTF-8", &mut all);
        let (_, everything) = unframe(&all);

        let mut replies = Vec::new();
        new_environ::answer_send(&[0], mtts::bitvector(false), "UTF-8", &mut replies);
        let (_, payload) = unframe(&replies);

        assert!(
            payload.len() > 1,
            "a bare SEND VAR must not be answered with an empty IS"
        );
        assert!(
            everything.starts_with(&payload),
            "it answers the VAR half of an empty request, verbatim"
        );
        assert!(payload.windows(11).any(|w| w == b"CLIENT_NAME"));
        assert!(payload.windows(4).any(|w| w == b"MTTS"));
    }

    /// The USERVAR half of the same rule: a bare `SEND USERVAR` gets the capability
    /// catalogue rather than silence.
    #[test]
    fn new_environ_send_uservar_with_no_name_returns_the_capabilities() {
        let mut replies = Vec::new();
        new_environ::answer_send(&[3], mtts::bitvector(false), "UTF-8", &mut replies);
        let (_, payload) = unframe(&replies);
        assert!(payload.windows(14).any(|w| w == b"OSC_HYPERLINKS"));
        assert!(
            !payload.windows(11).any(|w| w == b"CLIENT_NAME"),
            "identity is VAR-typed and does not belong in a USERVAR answer"
        );
    }

    #[test]
    fn new_environ_honors_selective_uservar_send() {
        let request = [
            3, b'O', b'S', b'C', b'_', b'H', b'Y', b'P', b'E', b'R', b'L', b'I', b'N', b'K', b'S',
            b'_', b'T', b'O', b'O', b'L', b'T', b'I', b'P',
        ];
        let mut replies = Vec::new();
        new_environ::answer_send(&request, mtts::bitvector(false), "UTF-8", &mut replies);
        let (_, payload) = unframe(&replies);
        assert_eq!(
            payload,
            [
                &[new_environ::IS, 3][..],
                b"OSC_HYPERLINKS_TOOLTIP",
                &[1, b'1'],
            ]
            .concat()
        );
    }

    #[test]
    fn new_environ_selectively_advertises_the_smudgy_tooltip_sgr_extension() {
        let request = [&[3][..], b"OSC_HYPERLINKS_TOOLTIP_SGR"].concat();
        let mut replies = Vec::new();
        new_environ::answer_send(&request, mtts::bitvector(false), "UTF-8", &mut replies);
        let (_, payload) = unframe(&replies);
        assert_eq!(
            payload,
            [
                &[new_environ::IS, 3][..],
                b"OSC_HYPERLINKS_TOOLTIP_SGR",
                &[1, b'1'],
            ]
            .concat()
        );
    }

    #[test]
    fn new_environ_answers_the_mnes_identity_vars() {
        // MNES requests identity variables as VARs (type 0), lowercase names
        // match case-insensitively, and answers echo the canonical spelling.
        let request = [
            &[0][..],
            b"CLIENT_NAME",
            &[0][..],
            b"mtts",
            &[0][..],
            b"TERMINAL_TYPE",
        ]
        .concat();
        let mut replies = Vec::new();
        new_environ::answer_send(&request, mtts::bitvector(false), "UTF-8", &mut replies);
        let (option, payload) = unframe(&replies);
        assert_eq!(option, NEW_ENVIRON);
        assert_eq!(
            payload,
            [
                &[new_environ::IS, 0][..],
                b"CLIENT_NAME",
                &[1][..],
                CLIENT_NAME.as_bytes(),
                &[0][..],
                b"MTTS",
                &[1][..],
                b"781",
                &[0][..],
                b"TERMINAL_TYPE",
                &[1][..],
                TERMINAL_TYPE.as_bytes(),
            ]
            .concat()
        );
    }

    #[test]
    fn new_environ_reports_charset_version_and_the_ssl_mtts_bit() {
        let request = [
            &[0][..],
            b"CHARSET",
            &[0][..],
            b"CLIENT_VERSION",
            &[0][..],
            b"MTTS",
        ]
        .concat();
        let mut replies = Vec::new();
        new_environ::answer_send(
            &request,
            mtts::bitvector(true),
            "windows-1252",
            &mut replies,
        );
        let (_, payload) = unframe(&replies);
        assert_eq!(
            payload,
            [
                &[new_environ::IS, 0][..],
                b"CHARSET",
                &[1][..],
                b"windows-1252",
                &[0][..],
                b"CLIENT_VERSION",
                &[1][..],
                env!("CARGO_PKG_VERSION").as_bytes(),
                &[0][..],
                b"MTTS",
                &[1][..],
                b"2829",
            ]
            .concat()
        );
    }

    #[test]
    fn new_environ_bare_send_reports_identity_and_capabilities() {
        let mut replies = Vec::new();
        new_environ::answer_send(&[], mtts::bitvector(false), "UTF-8", &mut replies);
        let (_, payload) = unframe(&replies);
        for expected in [
            [&[0][..], b"CLIENT_NAME", &[1][..], CLIENT_NAME.as_bytes()].concat(),
            [&[0][..], b"MTTS", &[1][..], b"781"].concat(),
            [
                &[0][..],
                b"TERMINAL_TYPE",
                &[1][..],
                TERMINAL_TYPE.as_bytes(),
            ]
            .concat(),
            [&[3][..], b"OSC_HYPERLINKS", &[1, b'1'][..]].concat(),
        ] {
            assert!(
                payload
                    .windows(expected.len())
                    .any(|window| window == expected),
                "missing entry {expected:?}"
            );
        }
    }

    #[test]
    fn protocol_state_reports_the_ssl_mtts_bit_through_new_environ() {
        let cell = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(pack_dims(80, 24)));
        let state = ProtocolState::new(cell, true);
        let mut replies = Vec::new();
        state.on_new_environ_send(&[&[0][..], b"MTTS"].concat(), "UTF-8", &mut replies);
        let (_, payload) = unframe(&replies);
        assert_eq!(
            payload,
            [&[new_environ::IS, 0][..], b"MTTS", &[1][..], b"2829"].concat()
        );
    }

    #[test]
    fn ttype_cycle_reports_name_type_mtts_then_repeats() {
        let mut state = ProtocolState::with_fixed_dims((80, 24));
        let expected = [
            CLIENT_NAME.to_string(),
            TERMINAL_TYPE.to_string(),
            "MTTS 781".to_string(),
            "MTTS 781".to_string(), // repetition signals end-of-list
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

    /// UTF-8 outranks whatever the server listed first, so long as the per-server
    /// `encoding` setting is not itself on offer.
    #[test]
    fn charset_request_prefers_utf8_over_earlier_offers() {
        use super::super::telnet::option::CHARSET;
        use super::charset;
        let mut replies = Vec::new();
        let enc =
            charset::answer_request(b";big5;UTF-8;iso-8859-1", encoding_rs::UTF_8, &mut replies);
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

    /// The configured encoding outranks UTF-8 when the server offers both — the same order
    /// [`charset::offer`] puts on the wire, so the outcome does not depend on which side
    /// opened the negotiation.
    #[test]
    fn charset_request_prefers_the_configured_encoding_over_utf8() {
        use super::charset;
        let mut replies = Vec::new();
        let enc = charset::answer_request(
            b";UTF-8;windows-1252",
            encoding_rs::WINDOWS_1252,
            &mut replies,
        );
        assert_eq!(
            enc,
            Some(encoding_rs::WINDOWS_1252),
            "the user named windows-1252 for this server; a server that speaks it should"
        );
        let (_, payload) = unframe(&replies);
        assert_eq!(&payload[1..], b"windows-1252");
    }

    /// Configured-beats-UTF-8 applies only when the server actually offers it; otherwise
    /// UTF-8 still wins over the rest of the list.
    #[test]
    fn charset_request_falls_back_to_utf8_when_configured_is_not_offered() {
        use super::charset;
        let mut replies = Vec::new();
        let enc = charset::answer_request(b";big5;UTF-8", encoding_rs::WINDOWS_1252, &mut replies);
        assert_eq!(enc, Some(encoding_rs::UTF_8));
        let (_, payload) = unframe(&replies);
        assert_eq!(&payload[1..], b"UTF-8");
    }

    /// Labels are compared by the encoding they resolve to, so a server spelling the
    /// configured charset differently than the setting does still matches.
    #[test]
    fn charset_request_matches_configured_by_encoding_not_spelling() {
        use super::charset;
        let mut replies = Vec::new();
        let enc =
            charset::answer_request(b";UTF-8;cp1252", encoding_rs::WINDOWS_1252, &mut replies);
        assert_eq!(enc, Some(encoding_rs::WINDOWS_1252));
        let (_, payload) = unframe(&replies);
        assert_eq!(
            &payload[1..],
            b"cp1252",
            "the echo keeps the server's spelling"
        );
    }

    #[test]
    fn charset_request_takes_the_first_resolvable_label() {
        use super::charset;
        let mut replies = Vec::new();
        // "NO-SUCH-CHARSET" resolves to nothing, so the first WHATWG-resolvable
        // offer (big5) wins over the later latin1.
        let enc = charset::answer_request(
            b" NO-SUCH-CHARSET big5 latin1",
            encoding_rs::UTF_8,
            &mut replies,
        );
        assert_eq!(enc, Some(encoding_rs::BIG5));
        let (_, payload) = unframe(&replies);
        assert_eq!(&payload[1..], b"big5");
    }

    #[test]
    fn charset_request_with_nothing_supported_is_rejected() {
        use super::charset;
        for payload in [&b";EBCDIC-US;KLINGON"[..], b""] {
            let mut replies = Vec::new();
            assert_eq!(
                charset::answer_request(payload, encoding_rs::UTF_8, &mut replies),
                None
            );
            let (_, reply) = unframe(&replies);
            assert_eq!(reply, vec![charset::REJECTED], "payload {payload:02x?}");
        }
    }

    /// RFC 2066 §2 shapes a REQUEST as `{ "[TTABLE ]" <Version> } <char set list>`: the
    /// marker offers a translation table, it does not replace the list behind it. Declining
    /// the table must not decline a charset we speak — REJECTED is for a receiver "not
    /// capable of handling any of the specified character sets".
    #[test]
    fn charset_request_reads_the_list_behind_a_ttable_marker() {
        use super::charset;
        let mut replies = Vec::new();
        let enc = charset::answer_request(b"[TTABLE]\x01;UTF-8", encoding_rs::UTF_8, &mut replies);
        assert_eq!(enc, Some(encoding_rs::UTF_8));
        let (_, payload) = unframe(&replies);
        assert_eq!(payload[0], charset::ACCEPTED);
        assert_eq!(&payload[1..], b"UTF-8");
    }

    /// A TTABLE request whose list holds nothing we speak is still a REJECTED, on the
    /// strength of the list rather than the marker.
    #[test]
    fn charset_request_with_a_ttable_and_no_usable_label_is_rejected() {
        use super::charset;
        let mut replies = Vec::new();
        let enc =
            charset::answer_request(b"[TTABLE]\x01;KLINGON", encoding_rs::UTF_8, &mut replies);
        assert_eq!(enc, None);
        let (_, reply) = unframe(&replies);
        assert_eq!(reply, vec![charset::REJECTED]);
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
                charset::answer_request(payload.as_bytes(), encoding_rs::UTF_8, &mut replies),
                None,
                "label {label} must not be accepted"
            );
            let (_, reply) = unframe(&replies);
            assert_eq!(reply, vec![charset::REJECTED]);
        }
    }

    /// The default connection asks for UTF-8 and nothing else.
    #[test]
    fn charset_request_offers_utf8_alone_by_default() {
        use super::super::telnet::option::CHARSET;
        use super::charset;
        let mut replies = Vec::new();
        charset::frame_request(encoding_rs::UTF_8, &mut replies);
        let (opt, payload) = unframe(&replies);
        assert_eq!(opt, CHARSET);
        assert_eq!(payload, [&[charset::REQUEST][..], b" UTF-8"].concat());
    }

    /// A per-server `encoding` override leads the offer; UTF-8 stays on as the fallback a
    /// server that cannot speak the override can still take.
    #[test]
    fn charset_request_leads_with_the_configured_override() {
        use super::charset;
        let mut replies = Vec::new();
        charset::frame_request(encoding_rs::WINDOWS_1252, &mut replies);
        let (_, payload) = unframe(&replies);
        assert_eq!(
            payload,
            [&[charset::REQUEST][..], b" windows-1252 UTF-8"].concat()
        );
    }

    /// An `ACCEPTED` counts only for a charset we actually offered, and only while a request
    /// of ours is outstanding; `REJECTED` leaves the connection where it was.
    #[test]
    fn charset_answers_switch_only_on_an_offered_label() {
        use super::charset;
        let configured = encoding_rs::WINDOWS_1252;

        let accepted_offered = [&[charset::ACCEPTED][..], b"utf8"].concat();
        assert_eq!(
            charset::accepted_encoding(&accepted_offered, configured),
            Some(encoding_rs::UTF_8),
            "label resolution is by encoding, not spelling"
        );
        for payload in [
            &[&[charset::ACCEPTED][..], b"big5"].concat()[..],
            &[&[charset::ACCEPTED][..], b"KLINGON"].concat(),
            &[charset::REJECTED],
            &[],
        ] {
            assert_eq!(
                charset::accepted_encoding(payload, configured),
                None,
                "payload {payload:02x?}"
            );
        }
    }

    /// The `ACCEPTED` gate: solicited answers land, unsolicited ones are inert, and one
    /// request is answered only once.
    #[test]
    fn charset_answers_are_ignored_without_an_outstanding_request() {
        use super::charset;
        let accepted = [&[charset::ACCEPTED][..], b"UTF-8"].concat();
        let mut state = ProtocolState::with_fixed_dims(super::DEFAULT_DIMS);

        assert_eq!(
            state.on_charset_answer(&accepted, encoding_rs::WINDOWS_1252),
            None,
            "nothing was asked, so nothing may switch"
        );

        let mut replies = Vec::new();
        state.send_charset_request(
            encoding_rs::WINDOWS_1252,
            encoding_rs::WINDOWS_1252,
            &mut replies,
        );
        assert_eq!(
            state.on_charset_answer(&accepted, encoding_rs::WINDOWS_1252),
            Some(encoding_rs::UTF_8)
        );
        assert_eq!(
            state.on_charset_answer(&accepted, encoding_rs::WINDOWS_1252),
            None,
            "the exchange is closed by the first answer"
        );

        state.send_charset_request(
            encoding_rs::WINDOWS_1252,
            encoding_rs::WINDOWS_1252,
            &mut replies,
        );
        state.reset_charset_request();
        assert_eq!(
            state.on_charset_answer(&accepted, encoding_rs::WINDOWS_1252),
            None,
            "a withdrawn request accepts no answer"
        );
    }

    /// The RFC 2066 §5 hold is taken only when an answer could change how we encode, and it
    /// ends on the answer, on a reset, or when the socket task gives up — the last of which
    /// leaves the request itself outstanding so a late answer still lands.
    #[test]
    fn the_outbound_hold_covers_only_a_request_that_could_change_the_encoding() {
        let mut replies = Vec::new();
        let mut state = ProtocolState::with_fixed_dims(super::DEFAULT_DIMS);
        assert!(!state.charset_holds_outbound(), "nothing requested yet");

        // UTF-8 offered to a UTF-8 connection: whatever the server answers, our encoder does
        // not move, so the write path never notices the exchange.
        state.send_charset_request(encoding_rs::UTF_8, encoding_rs::UTF_8, &mut replies);
        assert!(!state.charset_holds_outbound());

        // The override case: `windows-1252 UTF-8` offered while encoding windows-1252 — the
        // UTF-8 fallback would change the encoder, so outbound waits.
        state.send_charset_request(
            encoding_rs::WINDOWS_1252,
            encoding_rs::WINDOWS_1252,
            &mut replies,
        );
        assert!(state.charset_holds_outbound());

        let accepted = [&[charset::ACCEPTED][..], b"UTF-8"].concat();
        state.on_charset_answer(&accepted, encoding_rs::WINDOWS_1252);
        assert!(!state.charset_holds_outbound(), "the answer ends the hold");

        state.send_charset_request(
            encoding_rs::WINDOWS_1252,
            encoding_rs::WINDOWS_1252,
            &mut replies,
        );
        state.release_charset_hold();
        assert!(!state.charset_holds_outbound());
        assert_eq!(
            state.on_charset_answer(&accepted, encoding_rs::WINDOWS_1252),
            Some(encoding_rs::UTF_8),
            "giving up on the wait must not discard a late answer"
        );
    }

    #[test]
    fn dims_pack_round_trips() {
        for dims in [(80u16, 24u16), (0, 0), (u16::MAX, 1), (255, 511)] {
            assert_eq!(unpack_dims(pack_dims(dims.0, dims.1)), dims);
        }
    }
}
