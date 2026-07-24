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
    /// Mud Client Compression Protocol v2 (zlib). The *negotiation* and the start marker
    /// (`IAC SB MCCP2 IAC SE`) are handled here — [`TelnetParser::receive`] halts at the
    /// marker so the connection can splice its inflater in front of the parser; the
    /// decompression itself lives in `inflow.rs`.
    pub const MCCP2: u8 = 86;
    /// Mud Client Compression Protocol v3 — *outbound* (client→server) compression.
    /// **Declined by default** (`accept_local` omits it): compressing our own outbound stream
    /// under TLS is a CRIME-shaped chosen-plaintext length oracle on typed secrets for a
    /// keystroke-trickle benefit (`docs/telnet.md` §6.2).
    pub const MCCP3: u8 = 87;
    /// Mud eXtension Protocol.
    pub const MXP: u8 = 91;
    /// MCCPX (draft, taranion/mudstandards) — negotiated-algorithm compression. Inbound only:
    /// the server offers `WILL MCCPX`, we reply the encodings we accept, and it begins a
    /// `deflate` or `zstd` stream. We never initiate it outbound (same oracle as MCCP3).
    pub const MCCPX: u8 = 88;
    /// ATCP — the legacy GMCP predecessor.
    pub const ATCP: u8 = 200;
    /// Generic Mud Communication Protocol (the de-facto JSON-over-subnegotiation standard).
    pub const GMCP: u8 = 201;
}

/// MCCPX subnegotiation command codes (taranion/mudstandards `mccpX_draft.md`).
pub mod mccpx {
    /// Decompressor → compressor: "here are the encodings I accept" (comma-separated,
    /// preference order).
    pub const ACCEPT_ENCODING: u8 = 1;
    /// Compressor → decompressor: "I am starting this encoding"; the compressed stream
    /// follows immediately.
    pub const BEGIN_ENCODING: u8 = 2;
    /// "I don't understand that subnegotiation code" — echoes the unknown code back.
    pub const MCCPX_WONT: u8 = 252;
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

/// Append `payload` to `into` with every literal `0xFF` doubled (`IAC IAC`) — the telnet
/// escaping rule, shared by subnegotiation framing and the outbound charset encoder (whose
/// legacy encodings can produce a raw `0xFF`, e.g. Latin-1 `ÿ`).
pub fn double_iac_into(payload: &[u8], into: &mut Vec<u8>) {
    let mut rest = payload;
    while let Some(idx) = memchr(command::IAC, rest) {
        into.extend_from_slice(&rest[..=idx]);
        into.push(command::IAC);
        rest = &rest[idx + 1..];
    }
    into.extend_from_slice(rest);
}

/// Frame one outbound subnegotiation — `IAC SB <option> <payload…> IAC SE`, with every literal
/// `0xFF` in `payload` doubled (`IAC IAC`) — appending to `into`. The inverse of the parser's
/// subnegotiation extraction; the write path for GMCP sends and the NAWS/TTYPE/CHARSET
/// responders.
pub fn frame_subnegotiation(option: u8, payload: &[u8], into: &mut Vec<u8>) {
    into.reserve(payload.len() + 5);
    into.extend_from_slice(&[command::IAC, command::SB, option]);
    double_iac_into(payload, into);
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
/// prompts with `IAC EOR`), `ECHO` (RFC 857 — the server taking over echoing is the classic
/// password-prompt signal, surfaced through `on_option` so the client can mask its input),
/// `CHARSET` (the server drives the RFC 2066 REQUEST; the responder lives in the connection
/// layer), and the data protocols `GMCP` / `MSDP` / `MSSP` (their payloads arrive as
/// subnegotiations). Everything else is refused with `DONT`.
#[must_use]
fn accept_remote(option: u8) -> bool {
    matches!(
        option,
        option::ECHO | option::EOR | option::CHARSET | option::GMCP | option::MSDP | option::MSSP
    )
}

/// The compression a completed subnegotiation just started — the caller splices the matching
/// inflater in front of the remaining input. Kept telnet-layer-pure (no `inflow` dependency);
/// the connection maps it to the concrete decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompressionStart {
    /// zlib deflate — MCCP2, or MCCPX `BEGIN_ENCODING deflate`.
    Deflate,
    /// Zstandard — MCCPX `BEGIN_ENCODING zstd`.
    Zstd,
    /// MCCPX `BEGIN_ENCODING` naming an encoding we never offered — a protocol violation the
    /// caller treats as disconnect-grade (the compressed bytes can't be decoded).
    Unsupported,
}

/// The MCCPX encodings we accept, preference order (best ratio first) — the single source for
/// both the `ACCEPT_ENCODING` offer we send and the `BEGIN_ENCODING` name we parse, so the two
/// can never drift. `none` is not offered (the draft marks it testing-only).
const OFFERED_ENCODINGS: &[(&[u8], CompressionStart)] = &[
    (b"zstd", CompressionStart::Zstd),
    (b"deflate", CompressionStart::Deflate),
];

/// The `ACCEPT_ENCODING` payload body: the offered names comma-joined in preference order
/// (e.g. `zstd,deflate`). The `ACCEPT_ENCODING` command byte is prepended by the caller.
#[must_use]
pub fn offered_encodings() -> Vec<u8> {
    OFFERED_ENCODINGS
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(&b","[..])
}

/// The codec a `BEGIN_ENCODING` name selects, or `None` if we never offered it.
#[must_use]
fn offered_codec(name: &[u8]) -> Option<CompressionStart> {
    OFFERED_ENCODINGS
        .iter()
        .find(|(offered, _)| *offered == name)
        .map(|(_, codec)| *codec)
}

/// Whether we agree to enable `option` on **our** side when the server sends `DO` (we reply
/// `WILL`). We accept `SGA` (harmless, and common), `EOR`, and the two options whose
/// subnegotiation responders live in the connection layer (`responders.rs`): `TTYPE` (terminal
/// type + MTTS capability advertisement) and `NAWS` (window-size report). `DO CHARSET` is
/// refused: per RFC 2066 the WILL side is expected to drive the REQUEST, and smudgy only
/// *answers* requests — accepting would leave a strict server waiting forever for a REQUEST
/// that never comes. Servers negotiate charsets with us via their own `WILL CHARSET`.
#[must_use]
fn accept_local(option: u8) -> bool {
    matches!(
        option,
        option::SGA | option::EOR | option::TTYPE | option::NAWS
    )
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
    /// Whether inbound compression offers (`WILL MCCP2` / `WILL MCCPX`) are accepted — the
    /// per-server "Allow compression" setting. Off ⇒ every compression option is declined.
    accept_compression: bool,
    /// A compression option is negotiated on the stream. Exactly one compression wrapper is
    /// allowed at a time (MCCPX draft MUST); while set, every *other* compression option is
    /// declined. Cleared by [`clear_remote`](Self::clear_remote) at stream end.
    compression_claimed: bool,
    /// Set when a negotiated compression-start marker just completed, carrying the codec, so
    /// the caller switches to its inflater. A latch (not inferred from the consumed count): a
    /// marker at the exact end of a read buffer consumes the whole buffer yet must still arm
    /// the switch for the next read. Cleared by
    /// [`take_compression_started`](Self::take_compression_started).
    compression_started: Option<CompressionStart>,
}

impl Default for TelnetParser {
    fn default() -> Self {
        Self::new()
    }
}

impl TelnetParser {
    /// Create a fresh parser in the data state with all options disabled and compression
    /// offers accepted.
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
            accept_compression: true,
            compression_claimed: false,
            compression_started: None,
        }
    }

    /// Take and clear the compression-start latch. The caller checks this after every
    /// [`receive`](Self::receive) — regardless of whether the marker left a tail in the same
    /// buffer — and switches to the returned codec's inflater when it is `Some`.
    pub fn take_compression_started(&mut self) -> Option<CompressionStart> {
        self.compression_started.take()
    }

    /// Set whether compression offers are accepted (the per-server setting). Takes effect
    /// on future negotiations; call before the first bytes flow.
    pub fn set_accept_compression(&mut self, accept: bool) {
        self.accept_compression = accept;
    }

    /// The full remote-side acceptance policy: the static table, plus the compression gate
    /// (the per-server setting **and** the one-wrapper-at-a-time mutual exclusion — the first
    /// compression option to be accepted claims the stream; the rest are declined).
    fn accepts_remote(&self, option: u8) -> bool {
        if option == option::MCCP2 || option == option::MCCPX {
            return self.accept_compression && !self.compression_claimed;
        }
        accept_remote(option)
    }

    /// The compression a just-completed subnegotiation starts, or `None` if it is not a
    /// (negotiated) compression-start marker. MCCP2's marker is an empty `SB`; MCCPX's is
    /// `BEGIN_ENCODING <name>`, whose name selects the codec (an un-offered name is
    /// `Unsupported` — still a halt, so the compressed tail never feeds the parser as telnet).
    fn compression_start_for(&self, option: u8, payload: &[u8]) -> Option<CompressionStart> {
        match option {
            option::MCCP2 if self.remote_enabled[usize::from(option::MCCP2)] => {
                Some(CompressionStart::Deflate)
            }
            option::MCCPX
                if self.remote_enabled[usize::from(option::MCCPX)]
                    && payload.first() == Some(&mccpx::BEGIN_ENCODING) =>
            {
                // A name we offered selects its codec; anything else is a protocol violation
                // (still a halt, so the compressed tail can't be misread as telnet).
                Some(offered_codec(&payload[1..]).unwrap_or(CompressionStart::Unsupported))
            }
            _ => None,
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

    /// Clear the server-side negotiated state for `option` without sending a reply — for
    /// an option whose lifecycle ends out-of-band rather than via a telnet `WONT`. MCCP2 /
    /// MCCPX are the case: an orderly stream end ends the compression at the codec layer, and
    /// clearing the flag lets a later server `WILL` renegotiate cleanly instead of being
    /// swallowed as already-enabled. Ending a compression option also releases the
    /// one-wrapper-at-a-time claim so a *different* compressor may be negotiated next.
    pub fn clear_remote(&mut self, option: u8) {
        self.remote_enabled[usize::from(option)] = false;
        if option == option::MCCP2 || option == option::MCCPX {
            self.compression_claimed = false;
        }
    }

    /// Feed a received buffer through the parser, driving `sink` with the decoded output.
    ///
    /// Returns the number of input bytes consumed. This is `input.len()` except when a
    /// compression-start subnegotiation (MCCP2's empty marker, or MCCPX `BEGIN_ENCODING`)
    /// completes: the parser stops at the byte just past its `IAC SE`, because everything
    /// after it is a compressed stream this parser must not touch, and arms the
    /// [`take_compression_started`](Self::take_compression_started) latch. The caller routes
    /// the remainder through its inflater (and back into this parser, whose state persists).
    ///
    /// # Performance
    ///
    /// In [`State::Data`] (the steady state) the next `IAC` is located with a single SIMD `memchr`
    /// and the run before it is handed to [`TelnetSink::on_data`] as one borrowed slice — no copy,
    /// no allocation, no per-byte branching. A buffer with no `IAC` is therefore one `memchr` plus
    /// one `on_data`. Byte-at-a-time handling is confined to the (short, infrequent) interior of an
    /// `IAC …` control sequence.
    #[must_use = "fewer bytes than input.len() consumed means a compression stream started; dropping the tail loses data"]
    pub fn receive(&mut self, input: &[u8], sink: &mut impl TelnetSink) -> usize {
        let mut i = 0;
        while i < input.len() {
            if self.state == State::Data {
                // Fast path: emit everything up to the next IAC in one borrowed slice.
                match memchr(command::IAC, &input[i..]) {
                    None => {
                        sink.on_data(&input[i..]);
                        return input.len();
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
                let halt = self.step(input[i], sink);
                i += 1;
                if halt {
                    return i;
                }
            }
        }
        input.len()
    }

    /// Advance the control-sequence state machine by one byte, returning whether the caller
    /// must halt (a compression-start marker completed). Only called when `self.state` is not
    /// [`State::Data`]. Split out from [`receive`](Self::receive) to keep each function small
    /// and the hot bulk-scan loop tight.
    fn step(&mut self, b: u8, sink: &mut impl TelnetSink) -> bool {
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
            State::SubIac => return self.step_sub_iac(b, sink),
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
        false
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

    /// Handle the byte after an `IAC` seen *inside* a subnegotiation, returning whether the
    /// completed subnegotiation is a compression-start marker (the caller's halt signal).
    fn step_sub_iac(&mut self, b: u8, sink: &mut impl TelnetSink) -> bool {
        match b {
            command::SE => {
                sink.on_subnegotiation(self.sub_option, &self.sub_buf);
                self.state = State::Data;
                // A marker for a negotiated compression option starts a stream (and arms the
                // switch, carrying the codec). A marker for an option we declined, or an
                // MCCPX subnegotiation that isn't `BEGIN_ENCODING`, is not a start — don't
                // halt, let the parser resync / the sink reply.
                if let Some(start) = self.compression_start_for(self.sub_option, &self.sub_buf) {
                    self.compression_started = Some(start);
                    return true;
                }
                return false;
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
        false
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
                } else if self.accepts_remote(option) {
                    self.remote_enabled[opt] = true;
                    // Claim the stream for the first compression option to be accepted, so
                    // `accepts_remote` declines any other (one wrapper at a time).
                    if option == option::MCCP2 || option == option::MCCPX {
                        self.compression_claimed = true;
                    }
                    sink.on_send(&[command::IAC, command::DO, option]);
                    sink.on_option(Side::Remote, option, true);
                } else {
                    sink.on_send(&[command::IAC, command::DONT, option]);
                }
            }
            command::WONT => {
                if self.remote_enabled[opt] {
                    self.remote_enabled[opt] = false;
                    // A compression option turned off via telnet WONT (rather than a codec
                    // stream-end) must also release the one-wrapper claim, or all future
                    // compression negotiation would be declined for the session.
                    if option == option::MCCP2 || option == option::MCCPX {
                        self.compression_claimed = false;
                    }
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
        let consumed = p.receive(input, &mut r);
        assert_eq!(
            consumed,
            input.len(),
            "no test through this helper carries a compression-start marker"
        );
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

    /// The ECHO option (RFC 857) is accepted and its full lifecycle is
    /// surfaced: `WILL ECHO` is answered `DO` and reported enabled, the
    /// matching `WONT` is answered `DONT` and reported disabled — the
    /// negotiation half of the password auto-mask.
    #[test]
    fn server_echo_negotiation_answers_and_reports_both_edges() {
        use super::command::WONT;
        use super::option::ECHO;
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, ECHO], &mut r);
        assert_eq!(r.sent, &[IAC, DO, ECHO]);
        assert_eq!(r.options, &[(Side::Remote, ECHO, true)]);
        assert!(p.remote_enabled(ECHO));

        let _ = p.receive(&[IAC, WONT, ECHO], &mut r);
        assert_eq!(&r.sent[3..], &[IAC, DONT, ECHO]);
        assert_eq!(r.options[1], (Side::Remote, ECHO, false));
        assert!(!p.remote_enabled(ECHO));
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

    /// TTYPE and NAWS are accepted on our side (their subnegotiation responders live in the
    /// connection layer) and surfaced through `on_option` so those responders can act.
    #[test]
    fn server_do_for_ttype_and_naws_is_accepted_with_will() {
        use super::option::TTYPE;
        let r = run(&[IAC, DO, TTYPE, IAC, DO, NAWS]);
        assert_eq!(r.sent, &[IAC, WILL, TTYPE, IAC, WILL, NAWS]);
        assert_eq!(
            r.options,
            &[(Side::Local, TTYPE, true), (Side::Local, NAWS, true)]
        );
    }

    /// `DO CHARSET` is refused (the WILL side owns the REQUEST, and smudgy never
    /// initiates one — accepting would deadlock a strict RFC 2066 server), while the
    /// server-driven `WILL CHARSET` is accepted so its REQUEST can be answered.
    #[test]
    fn charset_is_accepted_remote_but_refused_local() {
        use super::command::WONT;
        use super::option::CHARSET;
        let r = run(&[IAC, DO, CHARSET, IAC, WILL, CHARSET]);
        assert_eq!(
            r.sent,
            &[IAC, WONT, CHARSET, IAC, DO, CHARSET],
            "DO answered WONT; WILL answered DO"
        );
        assert_eq!(r.options, &[(Side::Remote, CHARSET, true)]);
    }

    #[test]
    fn redundant_will_after_agreement_produces_no_reply() {
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, GMCP], &mut r);
        let _ = p.receive(&[IAC, WILL, GMCP], &mut r); // re-assertion
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
        assert_eq!(
            &framed[3..framed.len() - 2],
            &[b'a', IAC, IAC, b'b', IAC, IAC, IAC, IAC]
        );
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
        let _ = p.receive(&[b'>', IAC], &mut r);
        let _ = p.receive(&[GA], &mut r);
        // ...and split a subnegotiation across three reads.
        let _ = p.receive(&[IAC, SB, GMCP, b'C', b'o'], &mut r);
        let _ = p.receive(b"re.Ping", &mut r);
        let _ = p.receive(&[IAC, SE], &mut r);
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

    /// `WILL MCCP2` is accepted (with compression allowed, the default) and the start
    /// marker halts `receive` at the byte just past `IAC SE`, leaving the compressed tail
    /// unconsumed for the caller's inflater.
    #[test]
    fn mccp2_start_marker_halts_receive_at_the_switchover() {
        use super::option::MCCP2;
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, MCCP2], &mut r);
        assert_eq!(r.sent, &[IAC, DO, MCCP2]);
        assert!(p.remote_enabled(MCCP2));

        // "abc" + marker + pseudo-compressed tail, one buffer.
        let input = [b'a', b'b', b'c', IAC, SB, MCCP2, IAC, SE, 0x78, 0x9C, 0x01];
        let consumed = p.receive(&input, &mut r);
        assert_eq!(consumed, 8, "halt lands just past the IAC SE");
        assert_eq!(
            p.take_compression_started(),
            Some(super::CompressionStart::Deflate),
            "the switch must be armed with deflate"
        );
        assert_eq!(p.take_compression_started(), None, "and the latch clears");
        assert_eq!(r.data, b"abc", "the tail must not reach the sink as data");
        assert_eq!(r.subs, vec![(MCCP2, Vec::new())]);
    }

    /// The marker at the exact end of a read buffer: `receive` consumes the whole buffer,
    /// so the switch must be armed by the parser's latch, not inferred from a leftover tail
    /// (the boundary the caller would otherwise miss, feeding the next read's zlib bytes to
    /// the parser as plaintext).
    #[test]
    fn mccp2_marker_at_buffer_end_still_arms_the_switch() {
        use super::option::MCCP2;
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, MCCP2], &mut r);

        // The marker is the final bytes of this read; nothing follows it here.
        let input = [b'h', b'i', IAC, SB, MCCP2, IAC, SE];
        let consumed = p.receive(&input, &mut r);
        assert_eq!(consumed, input.len(), "the whole buffer is consumed");
        assert_eq!(
            p.take_compression_started(),
            Some(super::CompressionStart::Deflate),
            "the switch must arm even with no tail in this buffer"
        );
    }

    /// A compression-start marker for an option we DECLINED (compression off) is a server
    /// protocol violation: it must not halt or arm the switch.
    #[test]
    fn mccp2_marker_without_negotiation_does_not_arm() {
        use super::option::MCCP2;
        let mut p = TelnetParser::new();
        p.set_accept_compression(false);
        let mut r = Recorder::default();
        // Decline the offer, then the server (wrongly) sends the marker anyway.
        let _ = p.receive(&[IAC, WILL, MCCP2], &mut r);
        assert!(!p.remote_enabled(MCCP2));
        let input = [IAC, SB, MCCP2, IAC, SE, b'o', b'k'];
        let consumed = p.receive(&input, &mut r);
        assert_eq!(consumed, input.len(), "no halt: parsing continues");
        assert_eq!(
            p.take_compression_started(),
            None,
            "a declined marker never arms"
        );
        assert_eq!(r.data, b"ok", "the tail parses as ordinary data");
    }

    /// MCCPX: `WILL MCCPX` is accepted (`DO`), and the `BEGIN_ENCODING <codec>` marker halts
    /// `receive` and arms the switch with the named codec.
    #[test]
    fn mccpx_begin_encoding_arms_the_named_codec() {
        use super::mccpx::BEGIN_ENCODING;
        use super::option::MCCPX;
        for (name, codec) in [
            (&b"deflate"[..], super::CompressionStart::Deflate),
            (b"zstd", super::CompressionStart::Zstd),
        ] {
            let mut p = TelnetParser::new();
            let mut r = Recorder::default();
            let _ = p.receive(&[IAC, WILL, MCCPX], &mut r);
            assert_eq!(r.sent, &[IAC, DO, MCCPX]);

            let mut input = vec![IAC, SB, MCCPX, BEGIN_ENCODING];
            input.extend_from_slice(name);
            input.extend_from_slice(&[IAC, SE, 0xAA, 0xBB]); // compressed tail
            let consumed = p.receive(&input, &mut r);
            assert_eq!(consumed, input.len() - 2, "halt just past IAC SE");
            assert_eq!(p.take_compression_started(), Some(codec));
        }
    }

    /// MCCPX `BEGIN_ENCODING` naming an encoding we never offered halts (so the compressed
    /// tail can't be misread as telnet) but arms `Unsupported` — the caller disconnects.
    #[test]
    fn mccpx_unoffered_encoding_arms_unsupported() {
        use super::mccpx::BEGIN_ENCODING;
        use super::option::MCCPX;
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, MCCPX], &mut r);
        let mut input = vec![IAC, SB, MCCPX, BEGIN_ENCODING];
        input.extend_from_slice(b"brotli");
        input.extend_from_slice(&[IAC, SE]);
        let _ = p.receive(&input, &mut r);
        assert_eq!(
            p.take_compression_started(),
            Some(super::CompressionStart::Unsupported)
        );
    }

    /// One compression wrapper at a time (MCCPX draft MUST): once MCCP2 is claimed, a later
    /// `WILL MCCPX` in the same session is declined, and vice versa.
    #[test]
    fn compression_is_mutually_exclusive() {
        use super::option::{MCCP2, MCCPX};
        // MCCP2 first claims; MCCPX is then declined.
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, MCCP2, IAC, WILL, MCCPX], &mut r);
        assert_eq!(r.sent, &[IAC, DO, MCCP2, IAC, DONT, MCCPX]);

        // Symmetric: MCCPX first claims; MCCP2 is then declined.
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, MCCPX, IAC, WILL, MCCP2], &mut r);
        assert_eq!(r.sent, &[IAC, DO, MCCPX, IAC, DONT, MCCP2]);
    }

    /// The claim releases at stream end (`clear_remote`) so a *different* compressor can be
    /// negotiated afterward.
    #[test]
    fn ending_a_compression_stream_releases_the_claim() {
        use super::option::{MCCP2, MCCPX};
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, MCCP2], &mut r);
        p.clear_remote(MCCP2);
        let _ = p.receive(&[IAC, WILL, MCCPX], &mut r);
        assert_eq!(
            r.sent,
            &[IAC, DO, MCCP2, IAC, DO, MCCPX],
            "MCCPX now accepted"
        );
    }

    /// A compression option turned off via telnet `WONT` (not a codec stream-end) must
    /// release the one-wrapper claim, or all later compression negotiation would be declined.
    #[test]
    fn a_compression_wont_releases_the_claim() {
        use super::command::WONT;
        use super::option::{MCCP2, MCCPX};
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        // Accept MCCP2, then the server changes its mind with WONT before any stream.
        let _ = p.receive(&[IAC, WILL, MCCP2, IAC, WONT, MCCP2], &mut r);
        // A later MCCPX offer must still be accepted (the claim was released).
        let _ = p.receive(&[IAC, WILL, MCCPX], &mut r);
        assert_eq!(
            r.sent,
            &[IAC, DO, MCCP2, IAC, DONT, MCCP2, IAC, DO, MCCPX],
            "the claim released on WONT, so MCCPX is accepted"
        );
    }

    /// MCCP3 (outbound compression) is declined by default — `DO MCCP3` is answered `WONT`.
    /// The security posture (`docs/telnet.md` §6.2): compressing our own
    /// outbound stream under TLS is a CRIME-shaped password oracle.
    #[test]
    fn mccp3_outbound_compression_is_declined() {
        use super::command::WONT;
        use super::option::MCCP3;
        let r = run(&[IAC, DO, MCCP3]);
        assert_eq!(r.sent, &[IAC, WONT, MCCP3]);
        assert!(r.options.is_empty(), "MCCP3 never becomes locally enabled");
    }

    /// With compression disallowed (the per-server setting off), `WILL MCCP2` is declined,
    /// so no start marker can legitimately follow.
    #[test]
    fn mccp2_is_declined_when_compression_is_off() {
        use super::option::MCCP2;
        let mut p = TelnetParser::new();
        p.set_accept_compression(false);
        let mut r = Recorder::default();
        let _ = p.receive(&[IAC, WILL, MCCP2], &mut r);
        assert_eq!(r.sent, &[IAC, DONT, MCCP2]);
        assert!(!p.remote_enabled(MCCP2));
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
        assert!(
            r.subs.is_empty(),
            "an oversized subnegotiation must deliver nothing"
        );
        assert_eq!(
            r.data, b"after",
            "the stream must resync at the real IAC SE"
        );
        assert_eq!(r.prompts, 0);
    }

    #[test]
    fn discarded_subnegotiation_does_not_poison_the_next_one() {
        let mut p = TelnetParser::new();
        let mut r = Recorder::default();
        let mut oversized = vec![IAC, SB, GMCP];
        oversized.extend_from_slice(&vec![0u8; super::MAX_SUBNEGOTIATION_PAYLOAD + 1]);
        oversized.extend_from_slice(&[IAC, SE]);
        let _ = p.receive(&oversized, &mut r);
        let _ = p.receive(&[IAC, SB, GMCP, b'o', b'k', IAC, SE], &mut r);
        assert_eq!(r.subs, vec![(GMCP, b"ok".to_vec())]);
    }
}
