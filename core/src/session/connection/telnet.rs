//! Telnet protocol preprocessor — the negotiation / IAC layer that sits **in front of** the VT
//! parser in the inbound data path.
//!
//! # Responsibility
//!
//! The telnet protocol interleaves in-band command bytes (`IAC = 0xFF` and its sequences) with the
//! application data stream. This layer consumes those commands so the VT parser only ever sees pure
//! application (game) text — never a stray `0xFF` or a negotiation triple — and surfaces the
//! protocol facts the rest of the client cares about: chiefly **prompt boundaries** (`IAC GA` /
//! `IAC EOR`) and **subnegotiations** (the carrier for GMCP / MSDP / MXP / NAWS / …).
//!
//! # Shape
//!
//! [`TelnetParser`] is a persistent byte-stream state machine. The caller pumps received buffers
//! through [`TelnetParser::receive`] together with a [`TelnetSink`]; the sink receives:
//!
//! - [`TelnetSink::on_data`] — runs of pure application bytes (forward to the VT parser),
//! - [`TelnetSink::on_prompt`] — a prompt boundary (`IAC GA`, or `IAC EOR`),
//! - [`TelnetSink::on_subnegotiation`] — a completed `IAC SB <opt> … IAC SE` payload,
//! - [`TelnetSink::on_send`] — bytes to write back to the server (negotiation replies),
//! - [`TelnetSink::on_option`] — an option's negotiated state flipped (protocol activation hook).
//!
//! The parser owns no I/O and no allocation on the steady-state path — see the performance notes
//! on [`TelnetParser::receive`]. It is deliberately free of any `smudgy` dependency so it stays
//! unit-testable in isolation and re-usable.
//!
//! # Performance (this is a hot path — every inbound byte)
//!
//! The common case is a buffer with **no** `IAC` byte at all. [`TelnetParser::receive`] handles
//! that case with a single [`memchr::memchr`] (SIMD-accelerated) scan and one `on_data` call over
//! the whole slice — zero per-byte branching in this layer, zero copies, zero allocation. Per-byte
//! work happens only *inside* a control sequence (`IAC …`), which is short and rare. The
//! negotiation bookkeeping is two fixed `[bool; 256]` tables touched only during negotiation (a
//! cold path — a handful of options at connect time), so clarity is chosen there deliberately.

use memchr::memchr;

/// Telnet command bytes (RFC 854 + the `EOR` extension, RFC 885).
pub mod command {
    /// Interpret As Command — the telnet escape byte. Doubled (`IAC IAC`) for a literal `0xFF`.
    pub const IAC: u8 = 255;
    /// Subnegotiation end.
    pub const SE: u8 = 240;
    /// No-op.
    pub const NOP: u8 = 241;
    /// Data Mark (Synch).
    pub const DM: u8 = 242;
    /// Break.
    pub const BRK: u8 = 243;
    /// Interrupt Process.
    pub const IP: u8 = 244;
    /// Abort Output.
    pub const AO: u8 = 245;
    /// Are You There.
    pub const AYT: u8 = 246;
    /// Erase Character.
    pub const EC: u8 = 247;
    /// Erase Line.
    pub const EL: u8 = 248;
    /// Go Ahead — the canonical "the line just sent is a prompt; no newline is coming" marker.
    pub const GA: u8 = 249;
    /// Subnegotiation begin.
    pub const SB: u8 = 250;
    /// "I will enable this option (on my side)."
    pub const WILL: u8 = 251;
    /// "I won't / will stop enabling this option."
    pub const WONT: u8 = 252;
    /// "Please enable this option on your side."
    pub const DO: u8 = 253;
    /// "Please don't / stop enabling this option on your side."
    pub const DONT: u8 = 254;
    /// End Of Record — the prompt marker used with the `EOR` option (a GA alternative).
    pub const EOR: u8 = 239;
}

/// Well-known telnet **option** codes. This is the springboard set — the protocols a MUD client is
/// expected to grow into. Only a subset is acted on today (see [`accept_remote`] / [`accept_local`]);
/// the rest are named so the negotiation table and the `on_option` / `on_subnegotiation` hooks have
/// a vocabulary to extend against.
pub mod option {
    /// Echo (RFC 857) — server echoes input; used to mask password fields.
    pub const ECHO: u8 = 1;
    /// Suppress Go Ahead (RFC 858).
    pub const SGA: u8 = 3;
    /// Terminal Type (RFC 1091).
    pub const TTYPE: u8 = 24;
    /// End Of Record negotiation (RFC 885) — server may then send `IAC EOR` as a prompt marker.
    pub const EOR: u8 = 25;
    /// Negotiate About Window Size (RFC 1073).
    pub const NAWS: u8 = 31;
    /// Charset (RFC 2066).
    pub const CHARSET: u8 = 42;
    /// Mud Server Data Protocol.
    pub const MSDP: u8 = 69;
    /// Mud Server Status Protocol.
    pub const MSSP: u8 = 70;
    /// Mud Client Compression Protocol v2 (zlib). **Not** handled by this layer — enabling it would
    /// require splicing a zlib inflate stream ahead of the parser; see the design plan.
    pub const MCCP2: u8 = 86;
    /// Mud eXtension Protocol.
    pub const MXP: u8 = 91;
    /// ATCP — the legacy GMCP predecessor.
    pub const ATCP: u8 = 200;
    /// Generic Mud Communication Protocol (the de-facto JSON-over-subnegotiation standard).
    pub const GMCP: u8 = 201;
}

/// Which peer an option-state change in [`TelnetSink::on_option`] refers to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Our side: the result of a `WILL`/`WONT` we sent (or a `DO`/`DONT` we received) — "*we* are
    /// doing X".
    Local,
    /// The server's side: the result of a `WILL`/`WONT` received (or a `DO`/`DONT` we sent) — "the
    /// *server* is doing X".
    Remote,
}

/// Frame one outbound subnegotiation — `IAC SB <option> <payload…> IAC SE`, with every literal
/// `0xFF` in `payload` doubled (`IAC IAC`) — appending to `into`. The inverse of the parser's
/// subnegotiation extraction; the write path for GMCP sends and any future NAWS/TTYPE/CHARSET
/// responders.
pub fn frame_subnegotiation(option: u8, payload: &[u8], into: &mut Vec<u8>) {
    into.reserve(payload.len() + 5);
    into.extend_from_slice(&[command::IAC, command::SB, option]);
    let mut rest = payload;
    while let Some(idx) = memchr(command::IAC, rest) {
        into.extend_from_slice(&rest[..=idx]);
        into.push(command::IAC);
        rest = &rest[idx + 1..];
    }
    into.extend_from_slice(rest);
    into.extend_from_slice(&[command::IAC, command::SE]);
}

/// Receives the decoded output of [`TelnetParser::receive`]. All methods are called **in stream
/// order**, so a sink can interleave application data and prompt/subnegotiation events exactly as
/// they appeared on the wire.
///
/// `on_subnegotiation` and `on_option` have default no-op bodies so a minimal consumer (one that
/// only wants data + prompts) need not implement them.
pub trait TelnetSink {
    /// A run of pure application (game) bytes — every telnet sequence removed and every `IAC IAC`
    /// un-escaped to a single `0xFF`. Forward verbatim to the VT parser. Called zero or more times
    /// per `receive`; an empty slice is never passed.
    fn on_data(&mut self, data: &[u8]);

    /// A prompt boundary: `IAC GA`, or `IAC EOR`. The application bytes emitted since the last line
    /// terminator form a complete prompt — the sink should finalize/flush the pending line as a
    /// prompt at this exact point in the stream.
    fn on_prompt(&mut self);

    /// Bytes the parser wants written back to the server (negotiation replies, and any
    /// subnegotiation responses a future protocol layer enqueues). Write verbatim; never empty.
    fn on_send(&mut self, bytes: &[u8]);

    /// A completed subnegotiation `IAC SB <option> <payload…> IAC SE`, with `IAC IAC` un-escaped in
    /// `payload`. The springboard hook for GMCP (`option::GMCP`), MSDP, MXP, NAWS, TTYPE, CHARSET, …
    fn on_subnegotiation(&mut self, option: u8, payload: &[u8]) {
        let _ = (option, payload);
    }

    /// An option's negotiated state changed (a `WILL`/`DO` was accepted, or a `WONT`/`DONT` took
    /// effect). Protocol layers activate/deactivate here — e.g. on `(Remote, GMCP, true)` send the
    /// `Core.Hello` / `Core.Supports.Set` handshake.
    fn on_option(&mut self, side: Side, option: u8, enabled: bool) {
        let _ = (side, option, enabled);
    }
}

/// Whether we agree to let the **server** enable `option` on its side when it sends `WILL` (we
/// reply `DO`). This is the negotiation policy and the primary extension point for new protocols.
///
/// We accept the options whose *server-driven* behavior we want today: `EOR` (so the server marks
/// prompts with `IAC EOR`), and the data protocols `GMCP` / `MSDP` / `MSSP` (their payloads arrive
/// as subnegotiations). Everything else is refused with `DONT`.
#[must_use]
fn accept_remote(option: u8) -> bool {
    matches!(
        option,
        option::EOR | option::GMCP | option::MSDP | option::MSSP
    )
}

/// Whether we agree to enable `option` on **our** side when the server sends `DO` (we reply
/// `WILL`). We accept `SGA` (harmless, and common) and `EOR`. Options that require us to *send* a
/// subnegotiation in response (`NAWS`, `TTYPE`, `CHARSET`) are refused with `WONT` until their
/// response logic is implemented — refusing is safer than half-answering.
#[must_use]
fn accept_local(option: u8) -> bool {
    matches!(option, option::SGA | option::EOR)
}

/// Internal parse state. Steady state is [`State::Data`] (the bulk-scan fast path); the other
/// variants are short-lived control-sequence states entered after an `IAC`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Bulk application data. Scanned with `memchr` for the next `IAC`.
    Data,
    /// Saw `IAC`; the next byte is a command.
    Iac,
    /// Saw `IAC <WILL|WONT|DO|DONT>`; the next byte is the option. Holds the command byte.
    Negotiate(u8),
    /// Saw `IAC SB`; the next byte is the subnegotiation option code.
    SubOption,
    /// Collecting subnegotiation payload bytes until `IAC SE`.
    Sub,
    /// Saw `IAC` while collecting a subnegotiation; the next byte selects `SE` (end) / `IAC`
    /// (literal `0xFF`) / abort.
    SubIac,
    /// A subnegotiation whose payload exceeded [`MAX_SUBNEGOTIATION_PAYLOAD`]: its bytes are
    /// consumed and dropped until `IAC SE`, and nothing is delivered. Keeps a hostile or broken
    /// server that never terminates a subnegotiation from growing the payload buffer without
    /// bound.
    SubDiscard,
    /// Saw `IAC` while discarding an oversized subnegotiation; same byte rules as [`State::SubIac`].
    SubDiscardIac,
}

/// The most subnegotiation payload the parser will buffer. One shared bound with the GMCP
/// inbound cap (`gmcp::MAX_INBOUND_PAYLOAD`): nothing downstream accepts a larger payload, so
/// buffering past it only serves a memory-exhaustion attack.
const MAX_SUBNEGOTIATION_PAYLOAD: usize = super::gmcp::MAX_INBOUND_PAYLOAD;

/// A persistent telnet protocol state machine. Construct one per connection and pump every received
/// buffer through [`receive`](Self::receive). State (including a half-finished control sequence or
/// subnegotiation) persists across calls, so sequences that straddle a TCP read boundary are
/// handled correctly.
#[derive(Debug)]
pub struct TelnetParser {
    state: State,
    /// The option code captured after `IAC SB`, pending the payload + `IAC SE`.
    sub_option: u8,
    /// Reused subnegotiation payload buffer — cleared, not freed, between subnegotiations, so a
    /// steady stream of small GMCP messages doesn't churn the allocator.
    sub_buf: Vec<u8>,
    /// `remote_enabled[opt]` — the server has this option enabled on *its* side (we sent `DO`).
    remote_enabled: [bool; 256],
    /// `local_enabled[opt]` — we have this option enabled on *our* side (we sent `WILL`).
    local_enabled: [bool; 256],
}

impl Default for TelnetParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TelnetParser {
    /// Create a fresh parser in the data state with all options disabled.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: State::Data,
            sub_option: 0,
            // Subnegotiation payloads are small (GMCP messages, an NAWS quad, a terminal type);
            // pre-size for the common case so the first few never reallocate.
            sub_buf: Vec::with_capacity(64),
            remote_enabled: [false; 256],
            local_enabled: [false; 256],
        }
    }

    /// Whether the server has `option` enabled on its side (we acknowledged its `WILL` with `DO`,
    /// or we requested it). Lets a protocol layer query, e.g., whether GMCP is live.
    #[must_use]
    pub fn remote_enabled(&self, option: u8) -> bool {
        self.remote_enabled[usize::from(option)]
    }

    /// Whether we have `option` enabled on our side.
    #[must_use]
    pub fn local_enabled(&self, option: u8) -> bool {
        self.local_enabled[usize::from(option)]
    }

    /// Feed a received buffer through the parser, driving `sink` with the decoded output.
    ///
    /// # Performance
    ///
    /// In [`State::Data`] (the steady state) the next `IAC` is located with a single SIMD `memchr`
    /// and the run before it is handed to [`TelnetSink::on_data`] as one borrowed slice — no copy,
    /// no allocation, no per-byte branching. A buffer with no `IAC` is therefore one `memchr` plus
    /// one `on_data`. Byte-at-a-time handling is confined to the (short, infrequent) interior of an
    /// `IAC …` control sequence.
    pub fn receive(&mut self, input: &[u8], sink: &mut impl TelnetSink) {
        let mut i = 0;
        while i < input.len() {
            if self.state == State::Data {
                // Fast path: emit everything up to the next IAC in one borrowed slice.
                match memchr(command::IAC, &input[i..]) {
                    None => {
                        sink.on_data(&input[i..]);
                        return;
                    }
                    Some(offset) => {
                        if offset > 0 {
                            sink.on_data(&input[i..i + offset]);
                        }
                        self.state = State::Iac;
                        i += offset + 1;
                    }
                }
            } else {
                // Control-sequence interior: one byte at a time until we return to Data.
                self.step(input[i], sink);
                i += 1;
            }
        }
    }

    /// Advance the control-sequence state machine by one byte. Only called when `self.state` is not
    /// [`State::Data`]. Split out from [`receive`](Self::receive) to keep each function small and
    /// the hot bulk-scan loop tight.
    fn step(&mut self, b: u8, sink: &mut impl TelnetSink) {
        match self.state {
            State::Data => unreachable!("step is only called outside Data state"),
            State::Iac => self.step_iac(b, sink),
            State::Negotiate(command) => {
                self.handle_negotiation(command, b, sink);
                self.state = State::Data;
            }
            State::SubOption => {
                self.sub_option = b;
                self.sub_buf.clear();
                self.state = State::Sub;
            }
            State::Sub => {
                if b == command::IAC {
                    self.state = State::SubIac;
                } else if self.sub_buf.len() < MAX_SUBNEGOTIATION_PAYLOAD {
                    self.sub_buf.push(b);
                } else {
                    log::warn!(
                        "Telnet subnegotiation for option {} exceeds the {} byte cap; discarding it",
                        self.sub_option,
                        MAX_SUBNEGOTIATION_PAYLOAD
                    );
                    self.sub_buf.clear();
                    // The buffer grew to the cap; release that memory rather than keep it
                    // parked for the (small) payloads the reuse policy is for.
                    self.sub_buf.shrink_to(64);
                    self.state = State::SubDiscard;
                }
            }
            State::SubIac => self.step_sub_iac(b, sink),
            State::SubDiscard => {
                if b == command::IAC {
                    self.state = State::SubDiscardIac;
                }
            }
            State::SubDiscardIac => match b {
                command::SE => self.state = State::Data,
                // A doubled IAC is a literal payload byte — still being discarded.
                command::IAC => self.state = State::SubDiscard,
                // Malformed, same rule as `step_sub_iac`: re-interpret as a fresh command.
                _ => {
                    self.state = State::Iac;
                    self.step_iac(b, sink);
                }
            },
        }
    }

    /// Handle the command byte following a bare `IAC`.
    fn step_iac(&mut self, b: u8, sink: &mut impl TelnetSink) {
        match b {
            // A doubled IAC is a literal 0xFF data byte.
            command::IAC => {
                sink.on_data(&[command::IAC]);
                self.state = State::Data;
            }
            command::GA | command::EOR => {
                sink.on_prompt();
                self.state = State::Data;
            }
            command::SB => self.state = State::SubOption,
            command::WILL | command::WONT | command::DO | command::DONT => {
                self.state = State::Negotiate(b);
            }
            // NOP, DM, BRK, IP, AO, AYT, EC, EL, a stray SE, or anything else with no option
            // argument: nothing actionable for a client today. Swallow and resume data.
            _ => self.state = State::Data,
        }
    }

    /// Handle the byte after an `IAC` seen *inside* a subnegotiation.
    fn step_sub_iac(&mut self, b: u8, sink: &mut impl TelnetSink) {
        match b {
            command::SE => {
                sink.on_subnegotiation(self.sub_option, &self.sub_buf);
                self.state = State::Data;
            }
            // IAC IAC inside a subnegotiation → a literal 0xFF payload byte.
            command::IAC => {
                self.sub_buf.push(command::IAC);
                self.state = State::Sub;
            }
            // Malformed (only SE and IAC are valid after IAC in a subnegotiation): abandon the
            // subnegotiation and re-interpret this byte as a fresh command after IAC.
            _ => {
                self.state = State::Iac;
                self.step_iac(b, sink);
            }
        }
    }

    /// Apply the negotiation state machine for one `<command> <option>` pair and emit the reply.
    ///
    /// This is a deliberately simple **reactive + optimistic** policy that is loop-safe for
    /// well-behaved servers: a reply is emitted only when the negotiated state actually changes, so
    /// a re-assertion of an already-settled option produces no answer (the classic negotiation
    /// loop is broken at the "no state change ⇒ no reply" rule). It does not implement the full
    /// RFC 1143 "Q method", which also covers simultaneous client/server initiation.
    fn handle_negotiation(&mut self, command: u8, option: u8, sink: &mut impl TelnetSink) {
        let opt = usize::from(option);
        match command {
            command::WILL => {
                if self.remote_enabled[opt] {
                    // Already on — no reply (loop avoidance).
                } else if accept_remote(option) {
                    self.remote_enabled[opt] = true;
                    sink.on_send(&[command::IAC, command::DO, option]);
                    sink.on_option(Side::Remote, option, true);
                } else {
                    sink.on_send(&[command::IAC, command::DONT, option]);
                }
            }
            command::WONT => {
                if self.remote_enabled[opt] {
                    self.remote_enabled[opt] = false;
                    sink.on_send(&[command::IAC, command::DONT, option]);
                    sink.on_option(Side::Remote, option, false);
                }
            }
            command::DO => {
                if self.local_enabled[opt] {
                    // Already on — no reply.
                } else if accept_local(option) {
                    self.local_enabled[opt] = true;
                    sink.on_send(&[command::IAC, command::WILL, option]);
                    sink.on_option(Side::Local, option, true);
                } else {
                    sink.on_send(&[command::IAC, command::WONT, option]);
                }
            }
            command::DONT => {
                if self.local_enabled[opt] {
                    self.local_enabled[opt] = false;
                    sink.on_send(&[command::IAC, command::WONT, option]);
                    sink.on_option(Side::Local, option, false);
                }
            }
            _ => unreachable!("handle_negotiation only receives WILL/WONT/DO/DONT"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::command::{DO, DONT, EOR, GA, IAC, SB, SE, WILL};
    use super::option::{GMCP, NAWS, SGA};
    use super::{Side, TelnetParser, TelnetSink};

    /// Recording sink that captures the full ordered event stream for assertions.
    #[derive(Default)]
    struct Recorder {
        /// Concatenated application data across all `on_data` calls.
        data: Vec<u8>,
        prompts: usize,
        sent: Vec<u8>,
        subs: Vec<(u8, Vec<u8>)>,
        options: Vec<(Side, u8, bool)>,
        /// Ordered tags, to assert interleaving (`d` data, `p` prompt, `s` sub).
        order: String,
    }

    impl TelnetSink for Recorder {
        fn on_data(&mut self, data: &[u8]) {
            assert!(
                !data.is_empty(),
                "on_data must never receive an empty slice"
            );
            self.data.extend_from_slice(data);
            self.order.push('d');
        }
        fn on_prompt(&mut self) {
            self.prompts += 1;
            self.order.push('p');
        }
        fn on_send(&mut self, bytes: &[u8]) {
            assert!(
                !bytes.is_empty(),
                "on_send must never receive an empty slice"
            );
            self.sent.extend_from_slice(bytes);
        }
        fn on_subnegotiation(&mut self, option: u8, payload: &[u8]) {
            self.subs.push((option, payload.to_vec()));
            self.order.push('s');
        }
        fn on_option(&mut self, side: Side, option: u8, enabled: bool) {
            self.options.push((side, option, enabled));
        }
    }

    fn run(input: &[u8]) -> Recorder {
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        p.receive(input, &mut r);
        r
    }

    #[test]
    fn plain_data_passes_through_untouched() {
        let r = run(b"Hello, world!\r\n");
        assert_eq!(r.data, b"Hello, world!\r\n");
        assert_eq!(r.prompts, 0);
        assert!(r.sent.is_empty());
        // No IAC ⇒ exactly one on_data call.
        assert_eq!(r.order, "d");
    }

    #[test]
    fn doubled_iac_is_a_literal_ff_byte() {
        let r = run(&[b'a', IAC, IAC, b'b']);
        assert_eq!(r.data, &[b'a', 0xFF, b'b']);
        assert_eq!(r.prompts, 0);
    }

    #[test]
    fn ga_marks_a_prompt_and_is_stripped() {
        let r = run(&[b'H', b'P', b':', b'1', b'0', b'0', IAC, GA]);
        assert_eq!(r.data, b"HP:100");
        assert_eq!(r.prompts, 1);
        assert_eq!(r.order, "dp");
    }

    #[test]
    fn eor_also_marks_a_prompt() {
        let r = run(&[b'>', IAC, EOR]);
        assert_eq!(r.data, b">");
        assert_eq!(r.prompts, 1);
    }

    #[test]
    fn prompt_can_be_followed_by_more_data_in_one_buffer() {
        // GA mid-buffer: a precise prompt boundary even when data follows in the same packet.
        let r = run(&[b'>', b' ', IAC, GA, b'o', b'k', b'\n']);
        assert_eq!(r.data, b"> ok\n");
        assert_eq!(r.prompts, 1);
        assert_eq!(r.order, "dpd");
    }

    #[test]
    fn server_will_for_accepted_option_is_acknowledged_with_do() {
        let r = run(&[IAC, WILL, GMCP]);
        assert_eq!(r.sent, &[IAC, DO, GMCP]);
        assert_eq!(r.options, &[(Side::Remote, GMCP, true)]);
        assert!(r.data.is_empty());
    }

    #[test]
    fn server_will_for_refused_option_is_declined_with_dont() {
        let r = run(&[IAC, WILL, NAWS]);
        assert_eq!(r.sent, &[IAC, DONT, NAWS]);
        assert!(r.options.is_empty());
    }

    #[test]
    fn server_do_for_accepted_option_is_acknowledged_with_will() {
        let r = run(&[IAC, DO, SGA]);
        assert_eq!(r.sent, &[IAC, WILL, SGA]);
        assert_eq!(r.options, &[(Side::Local, SGA, true)]);
    }

    #[test]
    fn redundant_will_after_agreement_produces_no_reply() {
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        p.receive(&[IAC, WILL, GMCP], &mut r);
        p.receive(&[IAC, WILL, GMCP], &mut r); // re-assertion
        // Only the first WILL is answered; the second is a no-op (loop avoidance).
        assert_eq!(r.sent, &[IAC, DO, GMCP]);
        assert_eq!(r.options.len(), 1);
        assert!(p.remote_enabled(GMCP));
    }

    #[test]
    fn gmcp_subnegotiation_payload_is_extracted() {
        // IAC SB GMCP "Core.Hello {}" IAC SE
        let mut input = vec![IAC, SB, GMCP];
        input.extend_from_slice(b"Core.Hello {}");
        input.extend_from_slice(&[IAC, SE]);
        let r = run(&input);
        assert_eq!(r.subs, vec![(GMCP, b"Core.Hello {}".to_vec())]);
        assert!(r.data.is_empty());
    }

    #[test]
    fn frame_subnegotiation_doubles_iac_and_round_trips() {
        // A payload containing a literal 0xFF must go out doubled and come back single.
        let payload = [b'a', IAC, b'b', IAC, IAC];
        let mut framed = Vec::new();
        super::frame_subnegotiation(GMCP, &payload, &mut framed);
        assert_eq!(&framed[..3], &[IAC, SB, GMCP]);
        assert_eq!(&framed[framed.len() - 2..], &[IAC, SE]);
        assert_eq!(&framed[3..framed.len() - 2], &[b'a', IAC, IAC, b'b', IAC, IAC, IAC, IAC]);
        let r = run(&framed);
        assert_eq!(r.subs, vec![(GMCP, payload.to_vec())]);

        // The common case: no IAC in the payload, framed verbatim.
        let mut plain = Vec::new();
        super::frame_subnegotiation(GMCP, b"Core.Ping", &mut plain);
        let expected: Vec<u8> = [&[IAC, SB, GMCP][..], b"Core.Ping", &[IAC, SE][..]].concat();
        assert_eq!(plain, expected);
    }

    #[test]
    fn subnegotiation_unescapes_doubled_iac_in_payload() {
        // Payload bytes: 0x01, 0xFF, 0x02  (the 0xFF arrives escaped as IAC IAC).
        let input = [IAC, SB, NAWS, 0x01, IAC, IAC, 0x02, IAC, SE];
        let r = run(&input);
        assert_eq!(r.subs, vec![(NAWS, vec![0x01, 0xFF, 0x02])]);
    }

    #[test]
    fn data_around_a_subnegotiation_interleaves_correctly() {
        let mut input = vec![b'a'];
        input.extend_from_slice(&[IAC, SB, GMCP]);
        input.extend_from_slice(b"x");
        input.extend_from_slice(&[IAC, SE, b'b']);
        let r = run(&input);
        assert_eq!(r.data, b"ab");
        assert_eq!(r.subs, vec![(GMCP, b"x".to_vec())]);
        assert_eq!(r.order, "dsd");
    }

    #[test]
    fn sequences_split_across_receive_calls_are_reassembled() {
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        // Split a GA right down the middle of the IAC pair...
        p.receive(&[b'>', IAC], &mut r);
        p.receive(&[GA], &mut r);
        // ...and split a subnegotiation across three reads.
        p.receive(&[IAC, SB, GMCP, b'C', b'o'], &mut r);
        p.receive(b"re.Ping", &mut r);
        p.receive(&[IAC, SE], &mut r);
        assert_eq!(r.data, b">");
        assert_eq!(r.prompts, 1);
        assert_eq!(r.subs, vec![(GMCP, b"Core.Ping".to_vec())]);
    }

    #[test]
    fn lone_commands_without_options_are_swallowed() {
        use super::command::{AYT, NOP};
        let r = run(&[b'a', IAC, NOP, b'b', IAC, AYT, b'c']);
        assert_eq!(r.data, b"abc");
        assert_eq!(r.prompts, 0);
        assert!(r.sent.is_empty());
    }

    #[test]
    fn subnegotiation_at_exactly_the_cap_is_delivered() {
        let payload = vec![b'x'; super::MAX_SUBNEGOTIATION_PAYLOAD];
        let mut input = vec![IAC, SB, GMCP];
        input.extend_from_slice(&payload);
        input.extend_from_slice(&[IAC, SE]);
        let r = run(&input);
        assert_eq!(r.subs.len(), 1);
        assert_eq!(r.subs[0].0, GMCP);
        assert_eq!(r.subs[0].1.len(), super::MAX_SUBNEGOTIATION_PAYLOAD);
    }

    #[test]
    fn oversized_subnegotiation_is_discarded_and_the_stream_resyncs() {
        let mut input = vec![IAC, SB, GMCP];
        input.extend_from_slice(&vec![b'x'; super::MAX_SUBNEGOTIATION_PAYLOAD + 1]);
        // A doubled IAC while discarding is a literal payload byte, not a terminator.
        input.extend_from_slice(&[IAC, IAC, b'y', IAC, SE]);
        input.extend_from_slice(b"after");
        let r = run(&input);
        assert!(r.subs.is_empty(), "an oversized subnegotiation must deliver nothing");
        assert_eq!(r.data, b"after", "the stream must resync at the real IAC SE");
        assert_eq!(r.prompts, 0);
    }

    #[test]
    fn discarded_subnegotiation_does_not_poison_the_next_one() {
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let mut oversized = vec![IAC, SB, GMCP];
        oversized.extend_from_slice(&vec![0u8; super::MAX_SUBNEGOTIATION_PAYLOAD + 1]);
        oversized.extend_from_slice(&[IAC, SE]);
        p.receive(&oversized, &mut r);
        p.receive(&[IAC, SB, GMCP, b'o', b'k', IAC, SE], &mut r);
        assert_eq!(r.subs, vec![(GMCP, b"ok".to_vec())]);
    }
}
