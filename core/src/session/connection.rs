use std::fs::File;
use std::io::{BufWriter, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use futures::{SinkExt, Stream, StreamExt, channel::mpsc};
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWriteExt, Interest},
    net::{TcpSocket, TcpStream},
    select,
    sync::{
        mpsc::{Sender, UnboundedSender, WeakSender, error::TrySendError},
        oneshot,
    },
};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, tungstenite::Message as WsMessage};
use vt_processor::VtProcessor;
use vtparse::VTParser;

use super::{TaggedSessionEvent, runtime::RuntimeAction};

pub use smudgy_protocol::gmcp;
pub mod inflow;
pub use smudgy_protocol::msdp;
pub use smudgy_protocol::mssp;
pub mod plain_run;
pub use smudgy_protocol::responders;
pub use smudgy_protocol::telnet;
pub use smudgy_protocol::transcode;
pub mod vt_processor;

/// The glue between the telnet preprocessor and the VT parser for one socket read. A private
/// module so these internals stay off the normal public API: the items are `pub` at the item
/// level and escape only through the `bench-api` re-export below; the connect loop's socket
/// task reaches them through the unconditional import.
mod ingest {
    use std::sync::Arc;

    use tokio::sync::mpsc::UnboundedSender;
    use vtparse::VTParser;

    use super::super::runtime::RuntimeAction;
    use super::plain_run::PlainRuns;
    use super::responders::{self, ProtocolState};
    use super::transcode::Transcode;
    use super::vt_processor::VtProcessor;
    use super::{gmcp, msdp, mssp, telnet};

    /// Bridges the telnet preprocessor to the rest of the inbound pipeline for one socket read.
    ///
    /// Pure application bytes are fed through the VT parser and accumulated into `StyledLine::raw`; a
    /// prompt boundary flushes the pending line via [`VtProcessor::commit_prompt`]; negotiation replies
    /// are buffered into `replies` to be written back to the socket after the read. GMCP
    /// subnegotiations forward as [`RuntimeAction::GmcpMessage`] on the same channel the line
    /// actions ride — the exact stream position is the ordering guarantee
    /// (`docs/gmcp.md` §3.3) — and GMCP option changes drive the handshake +
    /// [`RuntimeAction::GmcpEnabled`]/[`RuntimeAction::GmcpDisabled`]. Other subnegotiations
    /// and option changes still fall through the default no-op hooks (the MSDP/… springboard —
    /// see `docs/telnet.md`).
    pub struct TelnetBridge<'a> {
        pub vt_parser: &'a mut VTParser,
        pub vt_processor: &'a mut VtProcessor,
        /// Reused across reads; the caller clears it before each `receive` and drains it after.
        pub replies: &'a mut Vec<u8>,
        /// The session action channel — the same one [`VtProcessor`] emits line actions on,
        /// so GMCP messages interleave with lines in wire order.
        pub runtime_tx: &'a UnboundedSender<RuntimeAction>,
        /// Responder state (TTYPE cycle, NAWS dimensions) — owned by the connect task like
        /// the parser itself, so it persists across reads and dies with the connection.
        pub protocol: &'a mut ProtocolState,
        /// Charset transcoding — a pure pass-through on UTF-8 connections (one branch in
        /// `on_data`), a streaming decode to UTF-8 otherwise. Owned by the connect task;
        /// the write arm shares it for outbound encoding.
        pub transcode: &'a mut Transcode,
    }

    /// Feed one run of UTF-8 application bytes straight off the wire through the VT parser
    /// and the raw-capture path. Free-standing so `on_data` can call it without aliasing
    /// `&mut self`.
    pub fn feed_utf8(vt_parser: &mut VTParser, vt_processor: &mut VtProcessor, data: &[u8]) {
        feed(vt_parser, vt_processor, PlainRuns::new(data));
    }

    /// [`feed_utf8`] for text the transcoder decoded: valid UTF-8 by construction, so bulk
    /// runs skip validation. Takes `&str` so that promise is the type's, not the caller's.
    pub fn feed_str(vt_parser: &mut VTParser, vt_processor: &mut VtProcessor, text: &str) {
        feed(vt_parser, vt_processor, PlainRuns::from_text(text));
    }

    /// The byte loop. Whenever the parser is in Ground, the leading run it would only print
    /// (see [`plain_run`](super::plain_run)) is handed over whole — one raw-capture append,
    /// one `push_str` — and only the byte that ends a run goes through the state machine,
    /// one byte at a time until the parser is back in Ground.
    ///
    /// Raw capture follows `capture_raw`, which the processor changes only at line and batch
    /// boundaries (a fall applies at the next line, a rise at the next batch), so a line's raw
    /// form is complete-or-absent whichever path its bytes took.
    fn feed(vt_parser: &mut VTParser, vt_processor: &mut VtProcessor, mut runs: PlainRuns<'_>) {
        let data = runs.bytes();
        let mut offset = 0;
        while offset < data.len() {
            if vt_parser.is_ground() {
                let run = runs.at(offset);
                if !run.is_empty() {
                    vt_processor.push_raw_incoming_run(run);
                    vt_processor.print_run(run);
                    offset += run.len();
                    if offset == data.len() {
                        break;
                    }
                }
            }
            // Consume the known stopping byte immediately, without another empty run scan.
            let byte = data[offset];
            // CR/LF drive line breaks in the VT parser but are kept out of `StyledLine::raw`.
            if byte != b'\n' && byte != b'\r' {
                vt_processor.push_raw_incoming_byte(byte);
            }
            vt_parser.parse_byte(byte, vt_processor);
            offset += 1;
        }
    }

    impl TelnetBridge<'_> {
        /// One CHARSET subnegotiation (RFC 2066), in either direction.
        ///
        /// A server REQUEST is answered — [`answer_request`](responders::charset::answer_request)
        /// always frames a reply (ACCEPTED or the mandatory REJECTED) — and takes the right of
        /// way over a REQUEST of ours still in flight (the simultaneous-request rule).
        ///
        /// Only `ACCEPTED` and `REJECTED` are answers to a REQUEST of ours, and only they close
        /// the exchange. Anything else must leave our request outstanding: closing on a message
        /// that is not an answer would discard the real answer behind it and leave the two sides
        /// reading each other in different charsets. `TTABLE-IS` is declined explicitly, since
        /// §5 lets no further subnegotiation start until this one completes and silence would
        /// strand the server; the remaining codes are inert.
        ///
        /// Either way the transcoder switches at this exact stream position: encodings change
        /// only after the ACCEPTED crosses, so in-order delivery makes the switch race-free.
        fn on_charset_subnegotiation(&mut self, payload: &[u8]) {
            use responders::charset;
            let Some((&code, rest)) = payload.split_first() else {
                return;
            };
            let switch_to = match code {
                charset::REQUEST => {
                    self.protocol.reset_charset_request();
                    charset::answer_request(
                        rest,
                        self.transcode.configured_encoding(),
                        self.replies,
                    )
                }
                charset::ACCEPTED | charset::REJECTED => self
                    .protocol
                    .on_charset_answer(payload, self.transcode.configured_encoding()),
                // We never offer `[TTABLE]`, so a table is unsolicited — but it is still a
                // subnegotiation the server is waiting on, and TTABLE-REJECTED is the message
                // RFC 2066 §2 requires every implementation to be able to send. It terminates
                // the exchange, so any request of ours dies with it rather than waiting for an
                // answer that is no longer coming.
                charset::TTABLE_IS => {
                    telnet::frame_subnegotiation(
                        telnet::option::CHARSET,
                        &[charset::TTABLE_REJECTED],
                        self.replies,
                    );
                    self.protocol.reset_charset_request();
                    None
                }
                _ => None,
            };
            if let Some(encoding) = switch_to {
                self.transcode.switch_to(encoding);
            }
        }
    }

    impl telnet::TelnetSink for TelnetBridge<'_> {
        #[inline]
        fn on_data(&mut self, data: &[u8]) {
            if self.transcode.is_passthrough() {
                // UTF-8 (the overwhelming default): bytes flow exactly as ever.
                feed_utf8(self.vt_parser, self.vt_processor, data);
            } else {
                // Decode to UTF-8 first; `StyledLine::raw` and raw-pattern triggers
                // therefore see decoded text, not wire bytes.
                let utf8 = self.transcode.decode(data);
                feed_str(self.vt_parser, self.vt_processor, utf8);
            }
            // Terminal-query answers the processor owes (DA1, pixel sizes) join
            // the negotiation replies here, in stream order. One branch on an
            // empty Vec per data run; nothing per byte.
            if self.vt_processor.has_replies() {
                self.vt_processor.drain_replies(self.replies);
            }
        }

        fn on_prompt(&mut self) {
            self.vt_processor.commit_prompt();
        }

        fn on_send(&mut self, bytes: &[u8]) {
            self.replies.extend_from_slice(bytes);
        }

        fn on_subnegotiation(&mut self, option: u8, payload: &[u8]) {
            // Protocol messages share the line channel and must not overtake
            // complete lines buffered earlier in this parser feed.
            self.vt_processor.sink_mut().flush_lines();
            if option == telnet::option::TTYPE {
                // RFC 1091 / MTTS: each `SEND` gets the next `IS` response in the cycle.
                // The reply rides the same inline buffer negotiation answers use.
                if payload == [responders::ttype::SEND] {
                    self.protocol.on_ttype_send(self.replies);
                }
                return;
            }
            if option == telnet::option::NEW_ENVIRON {
                // RFC 1572: answer SEND with the MNES identity variables (the
                // NEW-ENVIRON replacement for TTYPE cycling — servers that see
                // this option accepted query terminal type and MTTS here) and
                // the OSC 8 features Smudgy truthfully implements.
                if let Some((&responders::new_environ::SEND, requested)) = payload.split_first() {
                    self.protocol.on_new_environ_send(
                        requested,
                        self.transcode.encoding().name(),
                        self.replies,
                    );
                }
                return;
            }
            if option == telnet::option::MCCPX {
                // MCCPX draft: the compressor's `BEGIN_ENCODING` is the parser's halt (it
                // arms the codec latch), so nothing to do here. Any *other* subnegotiation
                // code is one we don't understand — echo it back as `MCCPX_WONT` per the
                // draft's error rule. `ACCEPT_ENCODING` is ours to send, never to receive.
                if let Some((&code, _)) = payload.split_first()
                    && code != telnet::mccpx::BEGIN_ENCODING
                    && code != telnet::mccpx::ACCEPT_ENCODING
                {
                    telnet::frame_subnegotiation(
                        telnet::option::MCCPX,
                        &[telnet::mccpx::MCCPX_WONT, code],
                        self.replies,
                    );
                }
                return;
            }
            if option == telnet::option::CHARSET {
                self.on_charset_subnegotiation(payload);
                return;
            }
            if option == telnet::option::MSDP {
                if payload.len() > msdp::MAX_INBOUND_PAYLOAD {
                    log::warn!(
                        "MSDP payload of {} bytes exceeds the {} byte cap; dropped",
                        payload.len(),
                        msdp::MAX_INBOUND_PAYLOAD
                    );
                    return;
                }
                self.runtime_tx
                    .send(RuntimeAction::MsdpMessage {
                        payload: Arc::from(payload),
                    })
                    .ok();
                return;
            }
            if option == telnet::option::MSSP {
                if payload.len() > mssp::MAX_INBOUND_PAYLOAD {
                    log::warn!(
                        "MSSP payload of {} bytes exceeds the {} byte cap; dropped",
                        payload.len(),
                        mssp::MAX_INBOUND_PAYLOAD
                    );
                    return;
                }
                // Decoded against the configured (connect-time) encoding — MSSP can
                // arrive before CHARSET negotiation settles, so the live transcoder is
                // deliberately not consulted (`mssp` module docs).
                let pairs = mssp::parse_variables(payload, self.transcode.configured_encoding());
                if pairs.is_empty() {
                    return;
                }
                self.runtime_tx
                    .send(RuntimeAction::MsspVariables {
                        connection_generation: self.vt_processor.connection_generation(),
                        pairs: Arc::new(pairs),
                    })
                    .ok();
                return;
            }
            if option != telnet::option::GMCP {
                return;
            }
            if payload.len() > gmcp::MAX_INBOUND_PAYLOAD {
                log::warn!(
                    "GMCP payload of {} bytes exceeds the {} byte cap; dropped",
                    payload.len(),
                    gmcp::MAX_INBOUND_PAYLOAD
                );
                return;
            }
            let text = String::from_utf8_lossy(payload);
            let (name, data) = gmcp::split_message(&text);
            if name.is_empty() {
                return;
            }
            // Core.Ping is answered here at the wire (the reply rides the same inline
            // buffer negotiation answers use — no session-thread round-trip); the message
            // still forwards so the store and catalogue record it like any other.
            if name.eq_ignore_ascii_case("Core.Ping") {
                gmcp::frame_message("Core.Ping", None, self.replies);
            }
            self.runtime_tx
                .send(RuntimeAction::GmcpMessage {
                    name: Arc::from(name),
                    data: data.map(Arc::from),
                })
                .ok();
        }

        fn on_option(&mut self, side: telnet::Side, option: u8, enabled: bool) {
            self.vt_processor.sink_mut().flush_lines();
            if matches!(side, telnet::Side::Local) {
                match option {
                    // Report the current window size the moment NAWS turns on (RFC 1073
                    // requires the first report immediately after the WILL); later size
                    // changes ride the `OutboundFrame::WindowSize` wakeup path.
                    telnet::option::NAWS if enabled => self.protocol.send_naws(self.replies),
                    // A disable restarts the TTYPE cycle so a renegotiation re-reports
                    // from the client name (the MTTS convention).
                    telnet::option::TTYPE if !enabled => self.protocol.reset_ttype(),
                    // RFC 2066's minimal implementation, our half: the server sends DO
                    // CHARSET, we send WILL CHARSET and then the REQUEST. Nothing else will
                    // drive it — a server that sent only DO has not itself sent WILL, so the
                    // RFC does not entitle it to request, and both sides would wait forever.
                    telnet::option::CHARSET if enabled => self.protocol.send_charset_request(
                        self.transcode.configured_encoding(),
                        self.transcode.encoding(),
                        self.replies,
                    ),
                    // The option going away drops any request still outstanding, so a later
                    // renegotiation starts clean.
                    telnet::option::CHARSET => self.protocol.reset_charset_request(),
                    _ => {}
                }
                return;
            }
            match option {
                telnet::option::GMCP => {
                    if enabled {
                        // Handshake immediately, in the same write the DO reply rides.
                        gmcp::frame_handshake(self.replies);
                        self.runtime_tx.send(RuntimeAction::GmcpEnabled).ok();
                    } else {
                        self.runtime_tx.send(RuntimeAction::GmcpDisabled).ok();
                    }
                }
                telnet::option::MSDP => {
                    if enabled {
                        msdp::frame_handshake(self.replies);
                        self.runtime_tx.send(RuntimeAction::MsdpEnabled).ok();
                    } else {
                        self.runtime_tx.send(RuntimeAction::MsdpDisabled).ok();
                    }
                }
                telnet::option::ECHO => {
                    // RFC 857: the server taking over echoing is the classic
                    // password-prompt signal. The parser already answered on
                    // the wire (DO/DONT); this forwards the fact so the UI can
                    // mask/unmask the main input (`docs/input.md`
                    // §3.10 — pref-gated and mask-composed UI-side).
                    self.runtime_tx
                        .send(RuntimeAction::ServerEchoChanged { enabled })
                        .ok();
                }
                telnet::option::MCCPX if enabled => {
                    // MCCPX draft: the moment the option is agreed, tell the compressor
                    // which encodings we accept, in preference order (the single-source
                    // `offered_encodings` list). It then picks one and sends `BEGIN_ENCODING`.
                    telnet::frame_subnegotiation(
                        telnet::option::MCCPX,
                        &[
                            &[telnet::mccpx::ACCEPT_ENCODING][..],
                            &telnet::offered_encodings(),
                        ]
                        .concat(),
                        self.replies,
                    );
                }
                _ => {}
            }
        }
    }

    /// Run one received buffer through the telnet preprocessor and VT parser, accumulating any
    /// negotiation replies into `replies` (cleared first). The caller writes `replies` back to the
    /// socket after each call and calls [`VtProcessor::notify_end_of_buffer`] once at the end of
    /// the read batch (which may span several of these calls).
    ///
    /// Returns the bytes consumed — fewer than `data.len()` exactly when a compression-start
    /// marker completed (see [`telnet::TelnetParser::receive`]); the caller routes the tail
    /// through its inflater.
    #[must_use = "a partial consume means a compression stream started; dropping the tail loses data"]
    pub fn feed_inbound(
        data: &[u8],
        telnet: &mut telnet::TelnetParser,
        vt_parser: &mut VTParser,
        vt_processor: &mut VtProcessor,
        replies: &mut Vec<u8>,
        runtime_tx: &UnboundedSender<RuntimeAction>,
        protocol: &mut ProtocolState,
        transcode: &mut Transcode,
    ) -> usize {
        replies.clear();
        let mut bridge = TelnetBridge {
            vt_parser,
            vt_processor,
            replies,
            runtime_tx,
            protocol,
            transcode,
        };
        let consumed = telnet.receive(data, &mut bridge);
        // Deliver the tail now, even when the read ended without a newline or
        // at a compression boundary. Never await the next read to fill a batch.
        bridge.vt_processor.sink_mut().flush_lines();
        consumed
    }
}

#[cfg(not(feature = "bench-api"))]
use ingest::{feed_inbound, feed_utf8};
// Expose the inbound ingest glue to the `smudgy_bench` crate without widening the normal
// public API (the same pattern as the trigger-engine re-export in `runtime.rs`): the module
// stays private; the items become reachable only under the feature.
#[cfg(feature = "bench-api")]
pub use ingest::{TelnetBridge, feed_inbound, feed_utf8};

/// Size hint for a single socket read; `try_read_buf` reads into the buffer's spare
/// capacity, so this caps the chunk size too.
const READ_CHUNK_CAPACITY: usize = 65536;

/// Per-wake ingest budget for the socket read loop. On a fast producer (a localhost
/// benchmark server, a huge log replay) the socket never goes `WouldBlock` — and the
/// readiness future bypasses tokio's cooperative budget — so without a cap one
/// connection would hold its worker for the whole burst, starving other connections on
/// the socket runtime and never reaching the end-of-batch commit that delivers the
/// pending partial line and repaint. Draining up to this many bytes per wake amortizes
/// the per-batch costs over many chunks while keeping the commit cadence to a few
/// milliseconds of parse time.
const READ_BATCH_BUDGET: usize = 512 * 1024;

/// The game connection's byte stream, abstracting the transport under the read loop so the
/// same loop drives a plain TCP socket or a TLS session. The read side exposes the
/// readiness-batched pattern the loop is tuned around ([`fill`](GameStream::fill) awaits the
/// next chunk, [`try_fill`](GameStream::try_fill) drains without blocking); the per-read
/// `match` on the variant is one branch against thousands of ns of downstream parse work.
enum GameStream {
    /// A plain TCP connection.
    Plain(TcpStream),
    /// A TLS session over TCP.
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
    /// Binary Telnet messages over a verified secure WebSocket.
    Wss(Box<WebSocketStream<MaybeTlsStream<TcpStream>>>),
}

fn websocket_chunk(message: WsMessage, buf: &mut Vec<u8>) -> io::Result<Option<usize>> {
    match message {
        WsMessage::Binary(bytes) => {
            if bytes.is_empty() {
                return Ok(None);
            }
            buf.clear();
            buf.extend_from_slice(&bytes);
            Ok(Some(buf.len()))
        }
        WsMessage::Close(_) => Ok(Some(0)),
        WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) => Ok(None),
        WsMessage::Text(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MUD sent a non-binary WebSocket message",
        )),
    }
}

impl GameStream {
    /// Clear `buf`, then await and read the next chunk into it. Returns the byte count
    /// (`0` = EOF). The `Plain` path awaits readiness (bypassing tokio's cooperative budget,
    /// the property the batched drain relies on) then reads; the `Tls` path awaits a record.
    async fn fill(&mut self, buf: &mut Vec<u8>) -> io::Result<usize> {
        match self {
            Self::Plain(stream) => loop {
                stream.ready(Interest::READABLE).await?;
                buf.clear();
                match stream.try_read_buf(buf) {
                    Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                        // Spurious readiness; wait again.
                    }
                    other => return other,
                }
            },
            Self::Tls(stream) => {
                buf.clear();
                stream.read_buf(buf).await
            }
            Self::Wss(stream) => loop {
                match stream.next().await {
                    Some(Ok(message)) => {
                        if let Some(n) = websocket_chunk(message, buf)? {
                            return Ok(n);
                        }
                    }
                    Some(Err(error)) => return Err(io::Error::other(error)),
                    None => return Ok(0),
                }
            },
        }
    }

    /// Clear `buf`, then read the next already-available chunk without blocking. `Ok(None)`
    /// means no data is ready (the batch is drained); `Ok(Some(0))` is EOF.
    fn try_fill(&mut self, buf: &mut Vec<u8>) -> io::Result<Option<usize>> {
        buf.clear();
        match self {
            Self::Plain(stream) => match stream.try_read_buf(buf) {
                Ok(n) => Ok(Some(n)),
                Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => Ok(None),
                Err(e) => Err(e),
            },
            Self::Tls(stream) => {
                // Poll the TLS read once with a no-op waker: any buffered plaintext (a
                // record already decrypted) comes back now; `Pending` means nothing is
                // ready without awaiting. Safe under tokio's last-poll-wins waker rule — the
                // next `fill` in the select re-registers interest.
                let mut read_buf = tokio::io::ReadBuf::uninit(buf.spare_capacity_mut());
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                match std::pin::Pin::new(stream).poll_read(&mut cx, &mut read_buf) {
                    std::task::Poll::Ready(Ok(())) => {
                        let n = read_buf.filled().len();
                        // Safety: `poll_read` initialized and filled `n` bytes of the spare
                        // capacity we handed it.
                        unsafe { buf.set_len(buf.len() + n) };
                        Ok(Some(n))
                    }
                    std::task::Poll::Ready(Err(e)) => Err(e),
                    std::task::Poll::Pending => Ok(None),
                }
            }
            Self::Wss(stream) => loop {
                let waker = std::task::Waker::noop();
                let mut cx = std::task::Context::from_waker(waker);
                match std::pin::Pin::new(&mut **stream).poll_next(&mut cx) {
                    std::task::Poll::Ready(Some(Ok(message))) => {
                        if let Some(n) = websocket_chunk(message, buf)? {
                            return Ok(Some(n));
                        }
                    }
                    std::task::Poll::Ready(Some(Err(error))) => {
                        return Err(io::Error::other(error));
                    }
                    std::task::Poll::Ready(None) => return Ok(Some(0)),
                    std::task::Poll::Pending => return Ok(None),
                }
            },
        }
    }

    /// Write all of `bytes` to the stream and flush, requiring forward
    /// progress (see [`write_all_with_stall_guard`]): a peer that has stopped
    /// reading entirely fails the write with `TimedOut` after `stall` instead
    /// of parking it for the OS's own multi-minute TCP timeout.
    async fn write_all_stall_guarded(&mut self, bytes: &[u8], stall: Duration) -> io::Result<()> {
        match self {
            Self::Plain(stream) => write_all_with_stall_guard(stream, bytes, stall).await,
            Self::Tls(stream) => write_all_with_stall_guard(stream, bytes, stall).await,
            Self::Wss(stream) => match tokio::time::timeout(
                stall,
                stream.send(WsMessage::Binary(bytes.to_vec().into())),
            )
            .await
            {
                Ok(result) => result.map_err(io::Error::other),
                Err(_) => Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("no write progress within {stall:?}"),
                )),
            },
        }
    }
}

/// Write all of `bytes` and flush, requiring forward progress: every accepted
/// byte resets a `stall` clock, so a slowly draining peer is never cut off, but
/// one that has stopped reading entirely (a zero TCP window) fails the write
/// with `TimedOut` once the clock runs out.
///
/// The flush (same budget) matters for TLS: rustls buffers plaintext into the
/// session record, so without it a lone interactive command would sit unsent
/// until the next write. On plain TCP `flush` is a no-op (`nodelay` is already
/// set), so flushing uniformly costs nothing there.
///
/// Kernel buffering bounds how quickly a dead peer trips this: a write parks
/// only once the send buffer fills, which the explicitly pinned
/// [`SEND_BUFFER_BYTES`] keeps small on every platform (Windows's dynamic
/// send buffering would otherwise absorb tens of megabytes first).
async fn write_all_with_stall_guard<S>(
    stream: &mut S,
    mut bytes: &[u8],
    stall: Duration,
) -> io::Result<()>
where
    S: tokio::io::AsyncWrite + Unpin,
{
    fn stalled(stall: Duration) -> io::Error {
        io::Error::new(
            io::ErrorKind::TimedOut,
            format!("no write progress within {stall:?}"),
        )
    }
    while !bytes.is_empty() {
        match tokio::time::timeout(stall, stream.write(bytes)).await {
            Ok(Ok(0)) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(Ok(written)) => bytes = &bytes[written..],
            Ok(Err(err)) => return Err(err),
            Err(_elapsed) => return Err(stalled(stall)),
        }
    }
    match tokio::time::timeout(stall, stream.flush()).await {
        Ok(result) => result,
        Err(_elapsed) => Err(stalled(stall)),
    }
}

/// How to establish the game transport (`docs/telnet.md` Phase 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsMode {
    /// Plain TCP.
    Off,
    /// TLS with full certificate verification against the OS trust store.
    Verify,
    /// TLS accepting any certificate — for the self-signed certificates common on MUD TLS
    /// ports. Insecure (no authentication); opt-in per server.
    NoVerify,
}

/// Per-connection policy for accepting inbound compression negotiation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InboundCompression {
    mccp2: bool,
    mccp4: bool,
}

impl InboundCompression {
    pub const ALL: Self = Self::new(true, true);
    pub const NONE: Self = Self::new(false, false);

    #[must_use]
    pub const fn new(mccp2: bool, mccp4: bool) -> Self {
        Self { mccp2, mccp4 }
    }
}

impl TlsMode {
    /// Resolve the two boolean settings (`tls`, `tls_verify`) into a mode.
    #[must_use]
    pub fn from_settings(tls: bool, verify: bool) -> Self {
        match (tls, verify) {
            (false, _) => Self::Off,
            (true, true) => Self::Verify,
            (true, false) => Self::NoVerify,
        }
    }
}

/// The overall budget for establishing a connection (TCP connect + TLS handshake). The
/// handshake is awaited before the read loop's `select!`, so nothing polls the disconnect
/// signal during it; the timeout bounds a stalled or black-holed handshake to seconds
/// instead of the OS TCP timeout (minutes). Note (`docs/telnet.md` §6.5): a
/// rare coop-budget-induced early batch end on the TLS drain path is possible but harmless
/// (no data loss; the tail reads on the next wake) and does not affect the plain path.
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// The zero-progress budget for one outbound write. A slowly draining peer is never cut
/// off — every accepted byte resets the clock — but a peer that has stopped reading
/// entirely (a zero TCP window; the OS would otherwise park the write for its own
/// multi-minute timeout) forfeits the connection. Tearing down closes the frame queue,
/// which also unparks a runtime producer awaiting queue capacity — without this, a full
/// queue against a wedged server leaves the whole session (including its `Disconnect`
/// handling) frozen until the TCP stack gives up.
const WRITE_STALL_TIMEOUT: Duration = Duration::from_secs(30);

/// The socket send-buffer size, pinned at connect. The value is generous for a
/// client whose outbound is keystrokes and subnegotiation frames, and pinning
/// it is load-bearing on Windows: an explicit `SO_SNDBUF` disables winsock's
/// dynamic send buffering, which otherwise absorbs an effectively unbounded
/// outbound backlog against a peer that stopped reading — hiding a wedged
/// server from the write stall guard and re-introducing, in the kernel, the
/// retained-memory growth the bounded frame queue exists to prevent.
const SEND_BUFFER_BYTES: u32 = 256 * 1024;

/// Return numeric loopback endpoints for the special `localhost` name, avoiding an OS
/// name-service lookup. IPv4 comes first because local development servers commonly bind
/// only there; the IPv6 fallback preserves support for IPv6-only listeners.
fn localhost_candidates(host: &str, port: u16) -> Option<[SocketAddr; 2]> {
    (host.eq_ignore_ascii_case("localhost") || host.eq_ignore_ascii_case("localhost.")).then_some([
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    ])
}

/// Remove URI-style brackets before using the host-and-port resolver form. Smudgy has
/// historically accepted bracketed IPv6 literals because it formatted them as `[::1]:port`.
fn unbracket_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|inner| inner.strip_suffix(']'))
        .unwrap_or(host)
}

/// Connect to the first reachable candidate, with the send buffer pinned before the
/// connect (see [`SEND_BUFFER_BYTES`]). A refused buffer size is logged and ignored: it is
/// a tuning knob, never worth failing the connection over.
async fn connect_tcp_candidates(
    candidates: impl IntoIterator<Item = SocketAddr>,
) -> io::Result<TcpStream> {
    let mut last_error = None;
    for candidate in candidates {
        let socket_result = if candidate.is_ipv4() {
            TcpSocket::new_v4()
        } else {
            TcpSocket::new_v6()
        };
        let socket = match socket_result {
            Ok(socket) => socket,
            Err(err) => {
                last_error = Some(err);
                continue;
            }
        };
        if let Err(err) = socket.set_send_buffer_size(SEND_BUFFER_BYTES) {
            warn!("Failed to pin the socket send buffer size: {err}");
        }
        match socket.connect(candidate).await {
            Ok(stream) => return Ok(stream),
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "address resolved to no candidates",
        )
    }))
}

/// Resolve `host` and connect, trying each candidate address like
/// `TcpStream::connect` does, but with the send buffer pinned before the
/// connect (see [`SEND_BUFFER_BYTES`]). A refused buffer size is logged and
/// ignored: it is a tuning knob, never worth failing the connection over.
async fn connect_tcp(host: &str, port: u16) -> io::Result<TcpStream> {
    if let Some(candidates) = localhost_candidates(host, port) {
        return connect_tcp_candidates(candidates).await;
    }

    connect_tcp_candidates(tokio::net::lookup_host((unbracket_host(host), port)).await?).await
}

/// Connect the transport: TCP, then a TLS handshake if requested. `host` is the server name
/// for certificate verification and SNI (a DNS name or IP literal).
async fn connect_stream(host: &str, port: u16, tls: TlsMode) -> io::Result<GameStream> {
    let stream = connect_tcp(host, port).await?;
    stream.set_nodelay(true)?;
    if tls == TlsMode::Off {
        return Ok(GameStream::Plain(stream));
    }

    install_tls_provider();

    let config = tls_client_config(tls == TlsMode::NoVerify)?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let tls_stream = connector.connect(server_name, stream).await?;
    Ok(GameStream::Tls(Box::new(tls_stream)))
}

fn install_tls_provider() {
    // A connection may start before scripts install the process-global provider.
    static PROVIDER: OnceLock<()> = OnceLock::new();
    PROVIDER.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

async fn connect_wss(url: &str) -> io::Result<GameStream> {
    crate::models::server::parse_wss_address(url).map_err(io::Error::other)?;
    install_tls_provider();
    let config = Arc::new(tls_client_config(false)?);
    connect_wss_with_config(url, config).await
}

async fn connect_wss_with_config(
    url: &str,
    config: Arc<rustls::ClientConfig>,
) -> io::Result<GameStream> {
    let (stream, _) = tokio_tungstenite::connect_async_tls_with_config(
        url,
        None,
        true,
        Some(tokio_tungstenite::Connector::Rustls(config)),
    )
    .await
    .map_err(io::Error::other)?;
    Ok(GameStream::Wss(Box::new(stream)))
}

/// Build the client TLS config: the OS trust store (via `rustls-platform-verifier`), or —
/// when `insecure` — a verifier that accepts any certificate for self-signed MUD ports. A
/// platform-verifier init failure (a broken/locked-down OS trust store) is an error, not a
/// panic, so it surfaces as a named connect failure like any other TLS trouble.
fn tls_client_config(insecure: bool) -> io::Result<rustls::ClientConfig> {
    use rustls_platform_verifier::BuilderVerifierExt;

    let config = if insecure {
        rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(danger::NoCertVerification::new()))
            .with_no_client_auth()
    } else {
        rustls::ClientConfig::builder()
            .with_platform_verifier()
            .map_err(io::Error::other)?
            .with_no_client_auth()
    };
    Ok(config)
}

/// The "accept any certificate" verifier for the per-server insecure TLS opt-in. Isolated in
/// its own module so the `dangerous`-named surface is easy to audit.
mod danger {
    use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
    use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
    use rustls::pki_types::{CertificateDer, ServerName, UnixTime};
    use rustls::{DigitallySignedStruct, Error, SignatureScheme};

    /// Skips certificate-chain validation (the user's explicit "accept any certificate"
    /// choice) but still validates handshake signatures with the active provider, so the
    /// TLS session itself is well-formed.
    #[derive(Debug)]
    pub struct NoCertVerification(std::sync::Arc<CryptoProvider>);

    impl NoCertVerification {
        pub fn new() -> Self {
            Self(
                CryptoProvider::get_default()
                    .expect("a process-global CryptoProvider is installed before TLS use")
                    .clone(),
            )
        }
    }

    impl ServerCertVerifier for NoCertVerification {
        fn verify_server_cert(
            &self,
            _end_entity: &CertificateDer<'_>,
            _intermediates: &[CertificateDer<'_>],
            _server_name: &ServerName<'_>,
            _ocsp_response: &[u8],
            _now: UnixTime,
        ) -> Result<ServerCertVerified, Error> {
            Ok(ServerCertVerified::assertion())
        }

        fn verify_tls12_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            verify_tls12_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn verify_tls13_signature(
            &self,
            message: &[u8],
            cert: &CertificateDer<'_>,
            dss: &DigitallySignedStruct,
        ) -> Result<HandshakeSignatureValid, Error> {
            verify_tls13_signature(
                message,
                cert,
                dss,
                &self.0.signature_verification_algorithms,
            )
        }

        fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
            self.0.signature_verification_algorithms.supported_schemes()
        }
    }
}

/// Map a telnet-layer compression-start latch to the concrete inflater codec. `None` means
/// the start is unusable — an MCCPX `BEGIN_ENCODING` naming an encoding we never offered —
/// and the caller tears the connection down (the compressed tail can't be decoded).
fn codec_for(start: telnet::CompressionStart) -> Option<inflow::Codec> {
    match start {
        telnet::CompressionStart::Deflate => Some(inflow::Codec::Deflate),
        telnet::CompressionStart::Zstd => Some(inflow::Codec::Zstd),
        telnet::CompressionStart::Unsupported => None,
    }
}

/// RFC 8878's standard zstd-frame magic, in its little-endian wire order.
const ZSTD_FRAME_MAGIC: [u8; 4] = 0xFD2F_B528_u32.to_le_bytes();
/// MCCPX's orderly-shutdown marker. It is the first plaintext after the final frame.
const IAC_WONT_MCCPX: [u8; 3] = [
    telnet::command::IAC,
    telnet::command::WONT,
    telnet::option::MCCPX,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MccpxZstdBoundary {
    /// The available bytes are a proper prefix of one of the two valid markers.
    NeedMore,
    /// Another standard zstd frame follows under the same MCCPX activation.
    NextFrame,
    /// The compressed stream ended; route the marker and its tail as plain telnet.
    Wont,
    /// Neither a standard frame nor the required orderly MCCPX shutdown follows.
    Invalid,
}

/// Classify the first bytes after a clean MCCPX zstd frame boundary. MCCPX narrows the
/// otherwise general RFC 8878 frame sequence to standard data frames; skippable frames are
/// not application data and are rejected here along with magicless/garbage input.
fn classify_mccpx_zstd_boundary(input: &[u8]) -> MccpxZstdBoundary {
    if input.starts_with(&IAC_WONT_MCCPX) {
        MccpxZstdBoundary::Wont
    } else if input.starts_with(&ZSTD_FRAME_MAGIC) {
        MccpxZstdBoundary::NextFrame
    } else if IAC_WONT_MCCPX.starts_with(input) || ZSTD_FRAME_MAGIC.starts_with(input) {
        MccpxZstdBoundary::NeedMore
    } else {
        MccpxZstdBoundary::Invalid
    }
}

/// Best-effort protocol rejection before a compression-grade disconnect. The connection may
/// already be desynchronized, so failure to deliver `DONT` cannot change the teardown.
async fn report_compression_error(
    stream: &mut GameStream,
    runtime_tx: &UnboundedSender<RuntimeAction>,
    option: u8,
    write_stall_timeout: Duration,
) {
    let dont = [telnet::command::IAC, telnet::command::DONT, option];
    stream
        .write_all_stall_guarded(&dont, write_stall_timeout)
        .await
        .ok();
    runtime_tx
        .send(RuntimeAction::Echo(Arc::new(
            "Compression error — disconnecting.".to_string(),
        )))
        .ok();
}

/// Append to the raw wire log, disabling it on the first write failure. Once compression
/// is active the connect loop logs the *inflated* telnet stream, not the zlib bytes — the
/// log's purpose (replay/trigger debugging) needs readable telnet, not ciphertext.
fn log_raw(raw_log: &mut Option<BufWriter<File>>, bytes: &[u8]) {
    if let Some(writer) = raw_log.as_mut()
        && let Err(err) = writer.write_all(bytes)
    {
        warn!("Raw log write failed; disabling the raw log: {err:?}");
        *raw_log = None;
    }
}

/// The shared socket runtime. Connection tasks run here — off the per-session
/// current-thread runtimes — so wire parsing (telnet + VT + `StyledLine` construction)
/// runs in parallel with the session thread's trigger/script/UI dispatch instead of
/// alternating with it; the `RuntimeAction` channel is the only coupling, and its
/// single-producer ordering carries the wire-order guarantees across the thread
/// boundary. A small pool serves every session: the work is one parse task per live
/// connection.
///
/// The runtime is lazy but explicitly owned. Keeping a `Runtime` forever in a
/// `OnceLock` provides no ownership boundary at which its worker threads can be
/// joined; the application shutdown path calls [`shutdown_io_runtime`] after signaling
/// every session and before joining their threads, so socket producers quiesce first.
/// The `Option` also lets a test (or a future in-process app restart) create a fresh
/// pool after a completed shutdown.
static IO_RUNTIME: Mutex<Option<tokio::runtime::Runtime>> = Mutex::new(None);

fn spawn_io_task(future: impl Future<Output = ()> + Send + 'static) {
    let mut runtime = IO_RUNTIME.lock().unwrap();
    runtime
        .get_or_insert_with(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .thread_name("smudgy-socket")
                .enable_all()
                .build()
                .expect("failed to build the socket runtime")
        })
        .spawn(future);
}

/// Stop the shared connection-worker pool during final application teardown.
///
/// Active tasks are cancelled by Tokio's runtime shutdown. The application calls
/// this after every session has been sent its shutdown action but before joining the
/// session threads, so no socket producer can keep adding work ahead of their exit.
/// Dropping each session's [`Connection`] afterward finds its task already gone.
///
/// # Panics
///
/// Panics if the socket-runtime mutex is poisoned, the shutdown thread cannot
/// be spawned, or that thread panics.
pub fn shutdown_io_runtime() {
    // Do not hold the registry mutex while Tokio joins its workers. Apart from making
    // the ownership boundary clearer, this permits a later call to `spawn_io_task` to
    // build a fresh runtime instead of waiting on the old one's shutdown.
    let runtime = IO_RUNTIME.lock().unwrap().take();
    if let Some(runtime) = runtime {
        // Tokio forbids dropping a runtime from within another asynchronous
        // context. `run()` currently calls us after iced's executor returns,
        // but keeping this core API context-independent also makes direct
        // async integration tests (and future embedders) safe.
        std::thread::Builder::new()
            .name("smudgy-socket-shutdown".to_string())
            .spawn(move || runtime.shutdown_timeout(Duration::from_secs(5)))
            .expect("failed to spawn socket-runtime shutdown thread")
            .join()
            .expect("socket-runtime shutdown thread panicked");
    }
}

/// One queued socket write. Text already contains the caller's intended line endings;
/// the connection applies its character encoding and escapes IAC bytes. Raw carries
/// exact bytes from script `sendRaw` calls or protocol messages. One channel carries both, so a
/// protocol frame and a user command queued in either order reach the wire in that order.
/// `WindowSize` is a wakeup, not bytes: the current size lives in the shared size cell, and
/// only the socket task knows whether NAWS is negotiated — so the task reads the cell and
/// decides to emit (or swallow) the report there, in order with the writes around it.
#[derive(Clone, Debug)]
pub enum OutboundFrame {
    Text(Arc<String>),
    Raw(Arc<[u8]>),
    WindowSize,
}

/// Frames waiting for the socket worker. Producers await capacity instead of
/// dropping valid commands; script-side sanity caps stop genuinely runaway
/// bursts before they reach this queue.
const OUTBOUND_QUEUE_CAPACITY: usize = 256;

/// How long outbound frames wait on an unanswered CHARSET `REQUEST` of ours before the socket
/// task sends them anyway (RFC 2066 §5's queue rule, bounded).
///
/// A bound is mandatory, not defensive: plenty of servers open with `DO CHARSET` expecting to
/// send the `REQUEST` themselves and simply ignore ours, and the hold falls exactly on the
/// login exchange. Long enough for a round trip to any real MUD, short enough that a server
/// which never answers costs one noticeable pause and nothing worse.
const CHARSET_HOLD_TIMEOUT: Duration = Duration::from_secs(2);

/// Decodes a graphics transmit's pixels off the socket thread. The UI
/// implements this on its `smudgy-images` runtime; each connect task wraps it
/// into the [`smudgy_protocol::graphics::GraphicsHost`] its VT processor drives
/// and routes every completion back through its own `select!`.
///
/// A request arrives validated as far as its bytes allow without inflating
/// or decoding them: its dimensions are within kitty's 10000-pixel limit and
/// the session quota (`width * height * 4` bytes at most), and uncompressed
/// packed data has exactly [`smudgy_protocol::graphics::Format::expected_len`]
/// bytes. What is left is the decoder's: inflate `o=z` data (for packed
/// formats capped at `expected_len`, a longer stream being `EINVAL`), expand
/// RGB to RGBA, decode PNG, and produce exactly `width` x `height` RGBA
/// pixels; anything else fails the transmit.
///
/// An animation frame carries [`smudgy_protocol::graphics::DecodeRequest::compose`]:
/// the decoded `width` x `height` rectangle (checked to lie inside the
/// image) is then composed at its `x`, `y` onto the base frame — or, with no
/// base, onto a canvas filled with the `background` colour — by straight
/// alpha blending or overwriting, and the result is the whole canvas,
/// [`smudgy_protocol::graphics::Compose::canvas`] in size.
/// [`smudgy_protocol::graphics::Compose::apply`] is the reference
/// implementation. A frame-to-frame compose (`a=c`) arrives the same way
/// but carries no data: [`smudgy_protocol::graphics::Compose::source`]
/// names the frame and the rectangle to read from it, so the decoder
/// decodes nothing and composes straight from that frame (`Compose::apply`
/// with empty data). The processor sends at most one frame request per
/// image at a time, so a base or source is always a frame that has landed.
pub trait GraphicsDecoder: Send + Sync + 'static {
    /// Decode `request`, then call `done` exactly once, from any thread.
    fn decode(
        &self,
        request: smudgy_protocol::graphics::DecodeRequest,
        done: Box<dyn FnOnce(smudgy_protocol::graphics::DecodeResult) + Send + 'static>,
    );
}

/// A finished decode, on its way back to the socket task.
type DecodeDone = (
    smudgy_protocol::graphics::DecodeTicket,
    smudgy_protocol::graphics::DecodeResult,
);

/// A decode's way back to the socket task, carried into the decoder's
/// callback. The decoder must call it once; if it drops the callback
/// unrun instead (a panicking or shut-down decode task), the drop reports
/// the failure, so the reply parked for this decode — and every answer
/// queued behind it — is never held back forever.
struct DecodeCompletion {
    ticket: smudgy_protocol::graphics::DecodeTicket,
    done: tokio::sync::mpsc::UnboundedSender<DecodeDone>,
    sent: bool,
}

impl DecodeCompletion {
    fn complete(mut self, result: smudgy_protocol::graphics::DecodeResult) {
        self.sent = true;
        // A closed channel means the connection ended mid-decode.
        let _ = self.done.send((self.ticket, result));
    }
}

impl Drop for DecodeCompletion {
    fn drop(&mut self) {
        if !self.sent {
            let _ = self.done.send((
                self.ticket,
                Err(smudgy_protocol::graphics::GraphicsError::invalid(
                    "decoder dropped the request",
                )),
            ));
        }
    }
}

/// One connection's graphics host: decodes through the UI's decoder and
/// sends each completion to the socket task, whose `select!` hands it to
/// [`VtProcessor::graphics_decode_complete`]. It lives in the processor, so
/// it dies with the connection; a decode finishing later finds the channel
/// closed and its result is dropped, as the host contract allows.
struct ChannelGraphicsHost {
    decoder: Arc<dyn GraphicsDecoder>,
    done: tokio::sync::mpsc::UnboundedSender<DecodeDone>,
    /// Whether `t=f`, `t=t` and `t=s` transmits may read this machine's
    /// files and shared memory: only when the server is allowed local files
    /// (`ServerConfig::graphics_local_files`) and the peer is a loopback
    /// address (see [`local_media_allowed_for`]). A remote server names
    /// paths on a machine it does not share, and a loopback address is no
    /// proof the server is on this one (an SSH tunnel, a forwarded or
    /// published port, another user's process bound to it), so the user
    /// says which servers are. Refused, the command is answered `EBADF`
    /// without the filesystem being touched.
    local_media_allowed: bool,
}

impl ChannelGraphicsHost {
    /// The host for a connection to `peer` (`None` when the transport could
    /// not say), for a server allowed local files when `local_files`.
    fn new(
        decoder: Arc<dyn GraphicsDecoder>,
        done: tokio::sync::mpsc::UnboundedSender<DecodeDone>,
        peer: Option<SocketAddr>,
        local_files: bool,
    ) -> Self {
        let local_media_allowed = local_files && local_media_allowed_for(peer);
        debug!(
            "graphics local media {} for peer {peer:?} (local files {} for this server)",
            if local_media_allowed {
                "allowed"
            } else {
                "refused"
            },
            if local_files { "enabled" } else { "disabled" }
        );
        Self {
            decoder,
            done,
            local_media_allowed,
        }
    }
}

/// The connected peer's socket address: the TCP socket's under TLS and
/// under a WebSocket alike (so a WSS URL naming a host that resolved to a
/// loopback address counts as loopback). `None` when the transport cannot
/// say.
fn graphics_peer_addr(stream: &GameStream) -> Option<SocketAddr> {
    let tcp = match stream {
        GameStream::Plain(tcp) => tcp,
        GameStream::Tls(tls) => tls.get_ref().0,
        GameStream::Wss(ws) => match ws.get_ref() {
            MaybeTlsStream::Plain(tcp) => tcp,
            MaybeTlsStream::Rustls(tls) => tls.get_ref().0,
            _ => return None,
        },
    };
    tcp.peer_addr().ok()
}

/// Whether a peer could be allowed local media (`t=f`, `t=t`, `t=s`): only
/// one at a loopback address, an IPv4-mapped IPv6 one included, can share
/// this machine's files. An unknown peer cannot. Necessary, not
/// sufficient: a loopback address is reached through tunnels and forwarded
/// ports too, so the server must be allowed local files as well
/// ([`ChannelGraphicsHost::new`]).
fn local_media_allowed_for(peer: Option<SocketAddr>) -> bool {
    peer.is_some_and(|addr| addr.ip().to_canonical().is_loopback())
}

impl std::fmt::Debug for ChannelGraphicsHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelGraphicsHost")
            .field("done", &self.done)
            .field("local_media_allowed", &self.local_media_allowed)
            .finish_non_exhaustive()
    }
}

impl smudgy_protocol::graphics::GraphicsHost for ChannelGraphicsHost {
    fn decode(
        &mut self,
        ticket: smudgy_protocol::graphics::DecodeTicket,
        request: smudgy_protocol::graphics::DecodeRequest,
    ) {
        let completion = DecodeCompletion {
            ticket,
            done: self.done.clone(),
            sent: false,
        };
        self.decoder
            .decode(request, Box::new(move |result| completion.complete(result)));
    }

    fn read_media(
        &mut self,
        medium: smudgy_protocol::graphics::Medium,
        name: &[u8],
        offset: u32,
        size: u32,
    ) -> Result<Vec<u8>, smudgy_protocol::graphics::GraphicsError> {
        if !self.local_media_allowed {
            // The one `EBADF` every refusal gets, before anything is looked at.
            debug!(
                "graphics {medium:?} transmit refused: local files are not allowed for this server, or the peer is not on this machine"
            );
            return Err(smudgy_protocol::graphics::GraphicsError::bad_file());
        }
        // The read blocks the thread, and the socket runtime has two
        // workers shared by every connection: on it, tokio is told so, and
        // hands the other tasks on this worker elsewhere meanwhile. Outside
        // a runtime (tests), or on a current-thread one, where that is not
        // possible, the read simply runs.
        let read = || super::graphics_media::read_local_media(medium, name, offset, size);
        match tokio::runtime::Handle::try_current() {
            Ok(handle) if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
                tokio::task::block_in_place(read)
            }
            _ => read(),
        }
    }
}

pub struct Connection {
    disconnect: Option<oneshot::Sender<()>>,
    runtime_tx: UnboundedSender<RuntimeAction>,
    ui_tx: mpsc::Sender<TaggedSessionEvent>,
    socket_tx: Arc<RwLock<Option<WeakSender<OutboundFrame>>>>,
    /// Monotonic session-local id used to reject late packet markers from a
    /// socket that was replaced by a reconnect.
    generation: u64,
    /// The runtime's "a deferred profile send is pending" bit; each connect
    /// task hands it to its [`VtProcessor`] so packet-completion markers stop
    /// once the send releases (see `set_marker_armed_flag`).
    marker_armed: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The trigger manager's "any trigger has a raw pattern" flag; each connect
    /// task hands it to its [`VtProcessor`] so per-line raw capture only runs
    /// while something can match on it.
    raw_wanted: Arc<std::sync::atomic::AtomicBool>,
    /// The session's current main-pane character grid, packed with
    /// [`responders::pack_dims`]. Owned by the runtime (which updates it from UI reports)
    /// and read by each connect task at spawn, so a connection established after a resize
    /// reports the real size in its first NAWS answer; later changes ride
    /// [`OutboundFrame::WindowSize`].
    window_size: Arc<std::sync::atomic::AtomicU32>,
    /// The session's main-pane cell size in logical pixels, packed like
    /// `window_size` and owned the same way; the VT processor reads it to
    /// answer pixel-size queries.
    cell_size: Arc<std::sync::atomic::AtomicU32>,
    /// How the next connect task's VT processor treats graphics commands and
    /// terminal queries. Set from the server configuration before each connect.
    graphics_mode: smudgy_protocol::graphics::GraphicsMode,
    /// What the next connect task's VT processor decodes graphics transmits
    /// with. `None` parses and drops them.
    graphics_decoder: Option<Arc<dyn GraphicsDecoder>>,
    /// Whether the next connect task's graphics host may read this
    /// machine's files and shared memory for `t=f`, `t=t` and `t=s`
    /// (`ServerConfig::graphics_local_files`); honoured for a loopback peer
    /// only. Off until the server configuration says otherwise.
    graphics_local_files: bool,
    /// Zero-progress write budget handed to each connect task
    /// ([`WRITE_STALL_TIMEOUT`]); a field so tests can shrink it.
    write_stall_timeout: Duration,
}

fn clear_socket_sender(socket_tx: &RwLock<Option<WeakSender<OutboundFrame>>>) {
    match socket_tx.write() {
        Ok(mut sender) => {
            sender.take();
        }
        Err(_) => warn!("Failed to clear socket sender because its lock is poisoned"),
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("disconnect", &self.disconnect)
            .field("runtime_tx", &self.runtime_tx)
            .field("ui_tx", &self.ui_tx)
            .field("socket_tx", &self.socket_tx)
            .field("generation", &self.generation)
            .field("marker_armed", &self.marker_armed)
            .field("raw_wanted", &self.raw_wanted)
            .field("window_size", &self.window_size)
            .field("cell_size", &self.cell_size)
            .field("graphics_mode", &self.graphics_mode)
            .field("graphics_decoder", &self.graphics_decoder.is_some())
            .field("graphics_local_files", &self.graphics_local_files)
            .field("write_stall_timeout", &self.write_stall_timeout)
            .finish()
    }
}

impl Connection {
    #[must_use]
    pub fn new(
        runtime_tx: UnboundedSender<RuntimeAction>,
        ui_tx: futures::channel::mpsc::Sender<TaggedSessionEvent>,
        raw_wanted: Arc<std::sync::atomic::AtomicBool>,
        window_size: Arc<std::sync::atomic::AtomicU32>,
        cell_size: Arc<std::sync::atomic::AtomicU32>,
    ) -> Self {
        Self::with_generation(runtime_tx, ui_tx, raw_wanted, window_size, cell_size, 0)
    }

    #[must_use]
    pub(crate) fn with_generation(
        runtime_tx: UnboundedSender<RuntimeAction>,
        ui_tx: futures::channel::mpsc::Sender<TaggedSessionEvent>,
        raw_wanted: Arc<std::sync::atomic::AtomicBool>,
        window_size: Arc<std::sync::atomic::AtomicU32>,
        cell_size: Arc<std::sync::atomic::AtomicU32>,
        generation: u64,
    ) -> Self {
        Self {
            disconnect: None,
            runtime_tx,
            ui_tx,
            socket_tx: Arc::new(RwLock::new(None)),
            generation,
            marker_armed: None,
            raw_wanted,
            window_size,
            cell_size,
            graphics_mode: smudgy_protocol::graphics::GraphicsMode::Off,
            graphics_decoder: None,
            graphics_local_files: false,
            write_stall_timeout: WRITE_STALL_TIMEOUT,
        }
    }

    /// Give the next connect task the decoder its graphics host decodes
    /// through. Takes effect at the next `connect`, like the mode.
    pub fn set_graphics_decoder(&mut self, decoder: Arc<dyn GraphicsDecoder>) {
        self.graphics_decoder = Some(decoder);
    }

    /// Set how the next connect task treats graphics commands and terminal
    /// queries. Takes effect at the next `connect`; a live socket keeps the
    /// mode it was spawned with, like its encoding.
    pub fn set_graphics_mode(&mut self, mode: smudgy_protocol::graphics::GraphicsMode) {
        self.graphics_mode = mode;
    }

    /// Set whether the next connect task's graphics host may read this
    /// machine's files and shared memory for `t=f`, `t=t` and `t=s`
    /// (`ServerConfig::graphics_local_files`); honoured only when the peer
    /// turns out to be a loopback address. Takes effect at the next
    /// `connect`, like the mode.
    pub fn set_graphics_local_files(&mut self, allowed: bool) {
        self.graphics_local_files = allowed;
    }

    /// Wake the live socket task after the shared size cell changed; the task re-reads the
    /// cell and sends a NAWS update iff the option is negotiated and the size actually
    /// changed. A no-op when disconnected — the cell already carries the value the next
    /// connect task reads.
    pub fn notify_window_size(&self) {
        let Ok(socket_tx) = self.socket_sender() else {
            return;
        };
        // Size lives in a shared cell, so a full queue may coalesce this wakeup:
        // the socket task also checks for a changed size after every queued
        // frame, including the frames already occupying that queue.
        match socket_tx.try_send(OutboundFrame::WindowSize) {
            Ok(()) | Err(TrySendError::Full(_) | TrySendError::Closed(_)) => {}
        }
    }

    /// Send raw data to the connected socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket lock is poisoned, no socket is currently
    /// registered, the sender can no longer be upgraded, or its queue is closed.
    pub async fn write(&self, data: Arc<String>) -> Result<(), anyhow::Error> {
        self.write_frame(OutboundFrame::Text(data)).await
    }

    /// Send exact bytes (script binary input or a pre-framed protocol message) to
    /// the connected socket. Same queue as [`Self::write`], so ordering with normal sends
    /// holds by construction.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket lock is poisoned, no socket is currently
    /// registered, the sender can no longer be upgraded, or its queue is closed.
    pub async fn write_raw(&self, frame: Arc<[u8]>) -> Result<(), anyhow::Error> {
        self.write_frame(OutboundFrame::Raw(frame)).await
    }

    fn socket_sender(&self) -> Result<Sender<OutboundFrame>, anyhow::Error> {
        let socket_tx = self
            .socket_tx
            .read()
            .map_err(|_| anyhow::anyhow!("Socket tx lock is poisoned"))?;
        let socket_tx = socket_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Socket no longer exists"))?;
        socket_tx
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Socket tx is not upgradeable"))
    }

    async fn write_frame(&self, frame: OutboundFrame) -> Result<(), anyhow::Error> {
        let socket_tx = self.socket_sender()?;
        // The receiver can drop between the upgrade and the send (the connect task
        // tearing down) — an ordinary disconnected-socket error, not a panic condition.
        socket_tx
            .send(frame)
            .await
            .map_err(|_| anyhow::anyhow!("Socket closed while sending"))
    }

    /// Establishes a TCP connection to the specified host and port.
    ///
    /// The connection task — socket reads, telnet/VT parsing, and outgoing writes —
    /// is spawned onto the shared socket runtime, not the calling session's runtime,
    /// so parsing overlaps the session thread's dispatch. Because the session can
    /// therefore tear down while the task is mid-batch, every send to the session
    /// runtime is best-effort: a closed channel drops the action, and the task exits
    /// via the disconnect signal (dropping this `Connection` raises it).
    ///
    /// If a previous connection managed by this `Connection` instance exists, it will
    /// be signaled to disconnect.
    ///
    /// When `raw_log_path` is `Some`, the exact bytes received from the server
    /// (including ANSI escape sequences and CR/LF) are appended to that file
    /// for the lifetime of the connection. Failure to create or write the file
    /// is logged and otherwise ignored.
    ///
    /// `tls` selects the transport; a TLS handshake failure (including a rejected
    /// certificate under [`TlsMode::Verify`]) tears the connection down with a named
    /// error and never falls back to plaintext.
    ///
    /// # Panics
    ///
    /// This function panics if the socket-runtime mutex is poisoned or the shared
    /// worker pool cannot be built. Connect-time I/O failures (DNS, TCP, TLS
    /// handshake, `set_nodelay`) and socket-lock failures are reported, not panicked.
    pub fn connect(
        &mut self,
        host: &str,
        port: u16,
        raw_log_path: Option<PathBuf>,
        encoding: Option<&'static encoding_rs::Encoding>,
        compression: InboundCompression,
        tls: TlsMode,
    ) {
        self.connect_target(host, port, None, raw_log_path, encoding, compression, tls);
    }

    /// Connect via TCP/TLS or WSS, retaining the same Telnet/VT ingest path.
    #[allow(clippy::too_many_arguments)]
    pub fn connect_target(
        &mut self,
        host: &str,
        port: u16,
        wss_url: Option<&str>,
        raw_log_path: Option<PathBuf>,
        encoding: Option<&'static encoding_rs::Encoding>,
        compression: InboundCompression,
        tls: TlsMode,
    ) {
        let addr = wss_url.map_or_else(|| format!("{host}:{port}"), str::to_owned);
        let host = host.to_string();
        let wss_url = wss_url.map(str::to_owned);
        let runtime_tx = self.runtime_tx.clone();
        let (tx, mut disconnect_rx) = oneshot::channel();

        if let Some(disconnect) = self.disconnect.take() {
            // This will error if the channel is already closed, which is fine
            disconnect.send(()).ok();
        }

        self.disconnect = Some(tx);

        self.socket_tx = Arc::new(RwLock::new(None));
        let socket_tx = self.socket_tx.clone();

        let generation = self.generation;
        let marker_armed = self.marker_armed.clone();
        let raw_wanted = self.raw_wanted.clone();
        let window_size = self.window_size.clone();
        let cell_size = self.cell_size.clone();
        let graphics_mode = self.graphics_mode;
        let graphics_decoder = self.graphics_decoder.clone();
        let graphics_local_files = self.graphics_local_files;
        let write_stall_timeout = self.write_stall_timeout;

        spawn_io_task(async move {
            let mut vt_parser = VTParser::new();
            let mut vt_processor = VtProcessor::with_connection_generation(
                vt_processor::NativeVtSink::batched(runtime_tx.clone()),
                generation,
            );
            vt_processor.set_raw_wanted_flag(raw_wanted);
            if let Some(marker_armed) = marker_armed {
                vt_processor.set_marker_armed_flag(marker_armed);
            }
            vt_processor.set_graphics_mode(graphics_mode);
            vt_processor.set_terminal_geometry(window_size.clone(), cell_size);
            // Decode completions re-enter this task through the read loop's
            // `select!`, between read batches, so one never splits a batch and
            // its reply joins the queue in command order. The host keeps a
            // sender; this one keeps the receive arm armed when there is no host.
            // The host itself is installed once connected, when the peer's
            // address (which decides whether local media are allowed) is known.
            let (decode_done_tx, mut decode_done_rx) =
                tokio::sync::mpsc::unbounded_channel::<DecodeDone>();
            let mut graphics_decoder = graphics_decoder;
            // Telnet/IAC preprocessor: consumes negotiation + prompt markers so the VT parser only
            // ever sees pure game text. Persists across reads (a sequence may straddle a read).
            let mut telnet = telnet::TelnetParser::new();
            telnet.set_accept_compression(compression.mccp2, compression.mccp4);
            // The MCCP stage ahead of the parser, plus its reused chunk buffer. (The
            // decompressed-bytes pacing counter is per-wake, declared in the read arm.)
            let mut inflow = inflow::Inflow::Plain;
            let mut inflate_buf: Vec<u8> = Vec::new();
            // A clean MCCPX zstd frame end is not necessarily the end of compression: RFC
            // 8878 permits concatenated frames. At each boundary, accept either another
            // standard frame or the required plaintext `IAC WONT MCCPX`; a marker split
            // across socket reads is retained here (at most three bytes).
            let mut awaiting_mccpx_zstd_boundary = false;
            let mut mccpx_zstd_boundary_prefix = Vec::with_capacity(ZSTD_FRAME_MAGIC.len());
            // Subnegotiation responder state (TTYPE cycle, NAWS reporting). Reads the
            // shared size cell at report time, so the first NAWS answer already carries
            // the size the UI last reported; `secure` sets the MTTS SSL bit.
            let mut protocol = responders::ProtocolState::new(
                window_size,
                wss_url.is_some() || tls != TlsMode::Off,
            );
            // Charset transcoding: the per-server setting seeds it (None = UTF-8, a pure
            // pass-through); a CHARSET negotiation switches it mid-stream.
            let mut transcode = transcode::Transcode::new(encoding.unwrap_or(encoding_rs::UTF_8));
            // Negotiation replies to write back to the server, reused across reads.
            let mut telnet_replies: Vec<u8> = Vec::new();
            // Deadline for the RFC 2066 §5 outbound hold, set while a CHARSET `REQUEST` of
            // ours is outstanding and could change how we encode. `Some` disables the write
            // arm, so frames simply stay in their queue (in order, under the same
            // backpressure) instead of being buffered a second time here.
            let mut charset_hold_until: Option<tokio::time::Instant> = None;
            let (write_to_socket_tx, mut write_to_socket_rx) =
                tokio::sync::mpsc::channel::<OutboundFrame>(OUTBOUND_QUEUE_CAPACITY);

            // The session's connection rule already says this on screen (it
            // is the row the user clicked Connect on); this side only logs.
            info!("Connecting to {addr}...");

            // Set once the transport is up: selects the full teardown
            // (Disconnected + notice) over the connect-phase exits below.
            let mut connected = false;
            // Raw wire log, created on successful connect; declared here so the
            // teardown can flush it after the run future is dropped.
            let mut raw_log: Option<BufWriter<File>> = None;

            // Everything that talks to the server — DNS, TCP, the TLS
            // handshake, and the whole read/write loop — runs inside one
            // future raced against the disconnect signal, so a user's
            // disconnect preempts ANY await point: a write parked on a wedged
            // peer (when the frame queue is full, the session runtime is
            // parked awaiting capacity and cannot dispatch its own Disconnect;
            // the signal here is the only escape), a mid-batch read, or a
            // black-holed handshake that otherwise only CONNECT_TIMEOUT ends.
            let run = async {
                let connect_result = match tokio::time::timeout(CONNECT_TIMEOUT, async {
                    if let Some(url) = &wss_url {
                        connect_wss(url).await
                    } else {
                        connect_stream(&host, port, tls).await
                    }
                })
                .await
                {
                    Ok(result) => result,
                    Err(_elapsed) => Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("timed out after {}s", CONNECT_TIMEOUT.as_secs()),
                    )),
                };
                match connect_result {
                    Ok(mut stream) => {
                        // `RuntimeAction::Connected` below advances the session's
                        // connection rule to "Connected to …"; this side only logs.
                        info!("Connected");

                        if let Some(decoder) = graphics_decoder.take() {
                            vt_processor.set_graphics_host(Box::new(ChannelGraphicsHost::new(
                                decoder,
                                decode_done_tx.clone(),
                                graphics_peer_addr(&stream),
                                graphics_local_files,
                            )));
                        }

                        {
                            let Ok(mut sender) = socket_tx.write() else {
                                warn!(
                                    "Failed to register socket sender because its lock is poisoned"
                                );
                                return;
                            };
                            sender.replace(write_to_socket_tx.downgrade());
                        }

                        runtime_tx.send(RuntimeAction::Connected).ok();
                        connected = true;

                        // Raw wire log: exact bytes as received, including ANSI
                        // escape sequences and CR/LF. One file per connection;
                        // failure to create it is non-fatal.
                        raw_log = raw_log_path.and_then(|path| match File::create(&path) {
                            Ok(file) => Some(BufWriter::with_capacity(65536, file)),
                            Err(err) => {
                                warn!("Failed to create raw log {}: {err:?}", path.display());
                                None
                            }
                        });

                        // The read buffer, reused across wakes (fill/try_fill clear it).
                        let mut data: Vec<u8> = Vec::with_capacity(READ_CHUNK_CAPACITY);

                        loop {
                            select! {
                                res = stream.fill(&mut data) => {
                                    // Batched drain. `fill` awaits the next chunk (the Plain path
                                    // via readiness, bypassing tokio's coop budget so a fast
                                    // producer resolves synchronously); `try_fill` drains the rest
                                    // without blocking. Read up to READ_BATCH_BUDGET bytes, commit
                                    // the batch downstream as one unit, then yield so other
                                    // connections on this worker get a turn (see READ_BATCH_BUDGET).
                                    let mut batched = 0usize;
                                    // Decompressed bytes fed since the last mid-burst yield —
                                    // per-wake, so pacing restarts fresh each readable wake.
                                    let mut fed_since_yield = 0usize;
                                    // Whether any bytes were fed this wake; gates the end-of-batch
                                    // commit and yield.
                                    let mut fed = false;
                                    // Socket EOF or a failed reply write: tear the connection down,
                                    // after the end-of-batch commit delivers what already parsed.
                                    let mut dead = false;
                                    // The current chunk's byte count; `None` ends the drain.
                                    let mut read = match res {
                                        Ok(n) if n > 0 => Some(n),
                                        // EOF (`0`): tear down after the commit.
                                        Ok(_) => {
                                            dead = true;
                                            None
                                        }
                                        Err(err) => {
                                            warn!("Socket read from {addr} failed: {err}");
                                            dead = true;
                                            None
                                        }
                                    };

                                        'batch: while let Some(n) = read {
                                                {
                                                    // Route the read through the MCCP stage. Plain
                                                    // bytes feed the parser directly; when a
                                                    // compression-start marker halts the parser
                                                    // mid-buffer, the remainder — and every later
                                                    // read — inflates in bounded chunks that feed
                                                    // the same parser. The raw log records the
                                                    // post-inflate telnet stream (readable), never
                                                    // zlib bytes. Negotiation replies flush per
                                                    // feed, not per batch, so handshakes stay
                                                    // timely even inside a large burst.
                                                    // If a boundary marker began in a prior
                                                    // socket read, stitch its tiny prefix onto
                                                    // this read so classification and routing see
                                                    // the original wire bytes contiguously. The
                                                    // owned buffer lives for this whole route.
                                                    let stitched_boundary =
                                                        if awaiting_mccpx_zstd_boundary
                                                            && !mccpx_zstd_boundary_prefix.is_empty()
                                                        {
                                                            let mut bytes = std::mem::take(
                                                                &mut mccpx_zstd_boundary_prefix,
                                                            );
                                                            bytes.extend_from_slice(&data);
                                                            Some(bytes)
                                                        } else {
                                                            None
                                                        };
                                                    let mut slice: &[u8] = stitched_boundary
                                                        .as_deref()
                                                        .map_or(data.as_slice(), |bytes| bytes);
                                                    'route: while !slice.is_empty() {
                                                        if awaiting_mccpx_zstd_boundary {
                                                            match classify_mccpx_zstd_boundary(slice) {
                                                                MccpxZstdBoundary::NeedMore => {
                                                                    // The next read will be stitched onto
                                                                    // this proper marker prefix. No bytes
                                                                    // reach either decoder or telnet yet.
                                                                    mccpx_zstd_boundary_prefix
                                                                        .extend_from_slice(slice);
                                                                    slice = &[];
                                                                }
                                                                MccpxZstdBoundary::NextFrame => {
                                                                    // Leave `slice` untouched: the fresh
                                                                    // decoder must consume the frame magic.
                                                                    if let Err(err) =
                                                                        inflow.begin(inflow::Codec::Zstd)
                                                                    {
                                                                        warn!(
                                                                            "MCCP4: failed to initialize the next zstd frame: {err}"
                                                                        );
                                                                        report_compression_error(
                                                                            &mut stream,
                                                                            &runtime_tx,
                                                                            telnet::option::MCCPX,
                                                                            write_stall_timeout,
                                                                        )
                                                                        .await;
                                                                        dead = true;
                                                                        break 'route;
                                                                    }
                                                                    awaiting_mccpx_zstd_boundary = false;
                                                                }
                                                                MccpxZstdBoundary::Wont => {
                                                                    // `inflow` is already plain. Route the
                                                                    // untouched WONT through telnet so its
                                                                    // option state and any following plain
                                                                    // bytes advance in normal wire order.
                                                                    awaiting_mccpx_zstd_boundary = false;
                                                                }
                                                                MccpxZstdBoundary::Invalid => {
                                                                    warn!(
                                                                        "MCCP4: expected another zstd frame or IAC WONT at a frame boundary; disconnecting"
                                                                    );
                                                                    report_compression_error(
                                                                        &mut stream,
                                                                        &runtime_tx,
                                                                        telnet::option::MCCPX,
                                                                        write_stall_timeout,
                                                                    )
                                                                    .await;
                                                                    dead = true;
                                                                    break 'route;
                                                                }
                                                            }
                                                        } else if inflow.is_plain() {
                                                            let consumed = feed_inbound(
                                                                slice,
                                                                &mut telnet,
                                                                &mut vt_parser,
                                                                &mut vt_processor,
                                                                &mut telnet_replies,
                                                                &runtime_tx,
                                                                &mut protocol,
                                                                &mut transcode,
                                                            );
                                                            log_raw(&mut raw_log, &slice[..consumed]);
                                                            fed = true;
                                                            if !telnet_replies.is_empty()
                                                                && let Err(err) = stream
                                                                    .write_all_stall_guarded(
                                                                        &telnet_replies,
                                                                        write_stall_timeout,
                                                                    )
                                                                    .await
                                                            {
                                                                warn!(
                                                                    "Failed to write telnet reply to {addr}: {err}"
                                                                );
                                                                dead = true;
                                                                break 'route;
                                                            }
                                                            slice = &slice[consumed..];
                                                            // Arm the inflater from the parser's latch,
                                                            // not from a non-empty tail: a start marker
                                                            // at the exact end of a read consumes the
                                                            // whole buffer, yet the compressed stream —
                                                            // arriving in the NEXT read — must still
                                                            // inflate. The latch fires in both cases; a
                                                            // marker for a declined option never sets it.
                                                            if let Some(start) = telnet.take_compression_started() {
                                                                // Begin the matching inflater; an
                                                                // un-offered MCCPX encoding (`None`) or a
                                                                // decoder init failure is disconnect-grade.
                                                                let Some(Ok(())) = codec_for(start)
                                                                    .map(|c| inflow.begin(c))
                                                                else {
                                                                    warn!("MCCP4: unusable compression start; disconnecting");
                                                                    runtime_tx
                                                                        .send(RuntimeAction::Echo(Arc::new(
                                                                            "Compression error — disconnecting.".to_string(),
                                                                        )))
                                                                        .ok();
                                                                    dead = true;
                                                                    break 'route;
                                                                };
                                                            }
                                                        } else {
                                                            // Drain the decoder within this read:
                                                            // step until it needs new input or the
                                                            // stream ends. zstd can buffer output
                                                            // internally when a step's buffer fills,
                                                            // so once `slice` empties we keep stepping
                                                            // (empty input flushes the buffered tail
                                                            // and the End marker) instead of stranding
                                                            // it until the next socket read.
                                                            loop {
                                                            match inflow.step(slice, &mut inflate_buf) {
                                                                Ok(step) => {
                                                                    let (consumed, ended) = match step {
                                                                        inflow::InflateStep::Progress { consumed } => (consumed, false),
                                                                        inflow::InflateStep::End { consumed } => (consumed, true),
                                                                    };
                                                                    slice = &slice[consumed..];
                                                                    if !inflate_buf.is_empty() {
                                                                        log_raw(&mut raw_log, &inflate_buf);
                                                                        // Set before the feed/write, like
                                                                        // the plain branch: a reply-write
                                                                        // failure mid-inflate must still
                                                                        // let the end-of-batch commit
                                                                        // deliver the lines already fed.
                                                                        fed = true;
                                                                        // Decompressed bytes count toward
                                                                        // the batch budget, so a high-ratio
                                                                        // burst re-enters `select!` (and
                                                                        // checks disconnect) after bounded
                                                                        // *work*, not bounded compressed
                                                                        // input.
                                                                        batched += inflate_buf.len();
                                                                        let mut plain: &[u8] = &inflate_buf;
                                                                        while !plain.is_empty() {
                                                                            let fed_n = feed_inbound(
                                                                                plain,
                                                                                &mut telnet,
                                                                                &mut vt_parser,
                                                                                &mut vt_processor,
                                                                                &mut telnet_replies,
                                                                                &runtime_tx,
                                                                                &mut protocol,
                                                                                &mut transcode,
                                                                            );
                                                                            plain = &plain[fed_n..];
                                                                            if !telnet_replies.is_empty()
                                                                                && let Err(err) = stream
                                                                                    .write_all_stall_guarded(
                                                                                        &telnet_replies,
                                                                                        write_stall_timeout,
                                                                                    )
                                                                                    .await
                                                                            {
                                                                                warn!(
                                                                                    "Failed to write telnet reply to {addr}: {err}"
                                                                                );
                                                                                dead = true;
                                                                                break 'route;
                                                                            }
                                                                        }
                                                                        fed_since_yield += inflate_buf.len();
                                                                        if fed_since_yield >= READ_BATCH_BUDGET {
                                                                            // A high-ratio burst: keep the
                                                                            // commit cadence and give other
                                                                            // connections a turn without
                                                                            // stashing compressed input.
                                                                            vt_processor.notify_end_of_buffer();
                                                                            tokio::task::yield_now().await;
                                                                            fed_since_yield = 0;
                                                                        }
                                                                    }
                                                                    // A compression-start marker nested in
                                                                    // the decompressed bytes (a protocol
                                                                    // violation) armed the latch during the
                                                                    // feed above; discard it here — AFTER
                                                                    // the feed, so a marker in the same
                                                                    // chunk that ends the stream can't
                                                                    // survive `inflow.end()` and re-enter
                                                                    // compression on the plain tail.
                                                                    let _ = telnet.take_compression_started();
                                                                    if ended {
                                                                        // Zstd is offered only by MCCP4, so
                                                                        // the active codec—not Telnet state
                                                                        // that compressed bytes might have
                                                                        // mutated—determines whether this is
                                                                        // an MCCP4 frame boundary.
                                                                        let mccp4_zstd_frame = inflow.is_zstd();
                                                                        // A clean codec end does not revoke
                                                                        // its Telnet option agreement. MCCP2
                                                                        // may send another bare start marker
                                                                        // after copyover; only an actual WONT
                                                                        // disables the option and releases its
                                                                        // exclusivity claim.
                                                                        inflow.end();
                                                                        if mccp4_zstd_frame {
                                                                            // A frame ended, but the RFC
                                                                            // 8878 stream may continue with
                                                                            // another frame. Preserve the
                                                                            // MCCPX option claim until the
                                                                            // required plaintext WONT is
                                                                            // classified and parsed.
                                                                            awaiting_mccpx_zstd_boundary = true;
                                                                            mccpx_zstd_boundary_prefix.clear();
                                                                        }
                                                                        break;
                                                                    } else if consumed == 0 && inflate_buf.is_empty() {
                                                                        // Decoder drained: it needs new
                                                                        // input (arriving in a later read).
                                                                        break;
                                                                    }
                                                                }
                                                                Err(err) => {
                                                                    // Unrecoverable by construction: no
                                                                    // plaintext boundary can be re-found in
                                                                    // a desynced stream. Best-effort DONT
                                                                    // for whichever compression option is
                                                                    // live (the channel is desynced, so it
                                                                    // may not land), then tear down.
                                                                    warn!("Compression stream error; disconnecting: {err:?}");
                                                                    let compression_option =
                                                                        if telnet.remote_enabled(telnet::option::MCCPX) {
                                                                            telnet::option::MCCPX
                                                                        } else {
                                                                            telnet::option::MCCP2
                                                                        };
                                                                    report_compression_error(
                                                                        &mut stream,
                                                                        &runtime_tx,
                                                                        compression_option,
                                                                        write_stall_timeout,
                                                                    )
                                                                    .await;
                                                                    dead = true;
                                                                    break 'route;
                                                                }
                                                            }
                                                            }
                                                        }
                                                    }
                                                }
                                                if dead {
                                                    break 'batch;
                                                }
                                                batched += n;
                                                if batched >= READ_BATCH_BUDGET {
                                                    break 'batch;
                                                }
                                                // Drain the rest of what's already available; a
                                                // batch ends at WouldBlock (`None`) or the budget.
                                                read = match stream.try_fill(&mut data) {
                                                    Ok(Some(m)) if m > 0 => Some(m),
                                                    // WouldBlock: the batch is drained.
                                                    Ok(None) => None,
                                                    // EOF (`Some(0)`): tear down.
                                                    Ok(Some(_)) => {
                                                        dead = true;
                                                        None
                                                    }
                                                    Err(err) => {
                                                        warn!("Socket read from {addr} failed: {err}");
                                                        dead = true;
                                                        None
                                                    }
                                                };
                                        }

                                        if fed {
                                            // Once per batch, not per chunk: a line straddling a
                                            // chunk boundary keeps accumulating instead of taking a
                                            // spurious partial-line round trip through the trigger
                                            // engine and a retraction.
                                            vt_processor.notify_end_of_buffer();
                                        }
                                        if dead {
                                            break;
                                        }
                                        // This batch is where our CHARSET `REQUEST` went out,
                                        // or where its answer arrived. Arm the hold on the
                                        // first, drop it on the second; an already-armed
                                        // deadline is left alone so it cannot be pushed out
                                        // by unrelated traffic.
                                        charset_hold_until = if protocol.charset_holds_outbound() {
                                            charset_hold_until.or_else(|| {
                                                Some(tokio::time::Instant::now() + CHARSET_HOLD_TIMEOUT)
                                            })
                                        } else {
                                            None
                                        };
                                        if fed {
                                            tokio::task::yield_now().await;
                                        }
                                }
                                // A pixel decode landed: settle its parked reply, then write
                                // whatever now heads the reply queue — in command order, and
                                // never inside a read batch, since this arm only runs between
                                // them. Held back like the write arm during a CHARSET hold?
                                // No: replies to the server are protocol, like negotiation
                                // answers, which the read arm writes through the hold too.
                                Some((ticket, result)) = decode_done_rx.recv() => {
                                    vt_processor.graphics_decode_complete(ticket, result);
                                    vt_processor.sink_mut().flush_lines();
                                    if vt_processor.has_replies() {
                                        telnet_replies.clear();
                                        vt_processor.drain_replies(&mut telnet_replies);
                                        if let Err(err) = stream
                                            .write_all_stall_guarded(
                                                &telnet_replies,
                                                write_stall_timeout,
                                            )
                                            .await
                                        {
                                            warn!("Failed to write graphics reply to {addr}: {err}");
                                            break;
                                        }
                                    }
                                }
                                // RFC 2066 §5: while a CHARSET subnegotiation of ours is in
                                // progress, outbound data waits. The whole queue waits, not
                                // just its text frames — `Raw` sends are ordered against
                                // `Text` by construction, and picking text out of the middle
                                // would break that.
                                Some(frame) = write_to_socket_rx.recv(),
                                    if charset_hold_until.is_none() =>
                                {
                                    let mut outcome = match &frame {
                                        OutboundFrame::Text(text) => {
                                            if transcode.is_passthrough() {
                                                stream
                                                    .write_all_stall_guarded(
                                                        text.as_bytes(),
                                                        write_stall_timeout,
                                                    )
                                                    .await
                                            } else {
                                                // Reject an unmappable command atomically: do not
                                                // send a representable prefix, `?`, or an HTML
                                                // numeric reference. The error text deliberately
                                                // omits command content so redacted secrets stay
                                                // out of logs and client feedback.
                                                match transcode.encode_outbound(text) {
                                                    Ok(encoded) => {
                                                        stream
                                                            .write_all_stall_guarded(
                                                                encoded,
                                                                write_stall_timeout,
                                                            )
                                                            .await
                                                    }
                                                    Err(err) => {
                                                        warn!(
                                                            "Rejected outbound text for {addr}: {err}"
                                                        );
                                                        let _ = runtime_tx.send(RuntimeAction::Echo(
                                                            Arc::new(format!("Send error: {err}")),
                                                        ));
                                                        Ok(())
                                                    }
                                                }
                                            }
                                        }
                                        OutboundFrame::Raw(bytes) => {
                                            stream
                                                .write_all_stall_guarded(bytes, write_stall_timeout)
                                                .await
                                        }
                                        OutboundFrame::WindowSize => Ok(()),
                                    };
                                    // Emit a changed NAWS report after every queued frame. That
                                    // makes a full queue safely coalesce a WindowSize wakeup:
                                    // one of the already-pending frames will observe the shared
                                    // size cell as soon as capacity starts draining.
                                    telnet_replies.clear();
                                    if outcome.is_ok()
                                        && telnet.local_enabled(telnet::option::NAWS)
                                        && protocol.send_naws_if_changed(&mut telnet_replies)
                                    {
                                        outcome = stream
                                            .write_all_stall_guarded(
                                                &telnet_replies,
                                                write_stall_timeout,
                                            )
                                            .await;
                                    }
                                    if let Err(err) = outcome {
                                        warn!("Socket write to {addr} failed: {err}");
                                        break;
                                    }
                                }
                                // The answer never came. Release the queue and stop holding
                                // for this connection; the request itself stays outstanding,
                                // so an answer arriving later is still honored.
                                // `select!` evaluates a disabled branch's expression (it just
                                // never polls the future), so this must be total — the
                                // fallback deadline is built and dropped unpolled, and an
                                // unpolled `Sleep` never arms a timer.
                                () = tokio::time::sleep_until(
                                    charset_hold_until.unwrap_or_else(tokio::time::Instant::now),
                                ), if charset_hold_until.is_some() => {
                                    debug!(
                                        "No CHARSET answer from {addr} within {CHARSET_HOLD_TIMEOUT:?}; releasing held output"
                                    );
                                    protocol.release_charset_hold();
                                    charset_hold_until = None;
                                }
                                else => {
                                    break;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        warn!("Connection to {addr} failed: {err}");
                        // Name the reason in the session view — a TLS handshake failure
                        // (self-signed / expired / name mismatch) gives no other hint, and the
                        // policy is never a silent fallback to plaintext.
                        runtime_tx
                            .send(RuntimeAction::ConnectionFailed {
                                connection_generation: generation,
                                error: Arc::new(format!("{err}")),
                            })
                            .map_err(|_| {
                                warn!("Error notifying runtime of connection failure; ignoring");
                            })
                            .ok();
                    }
                }
            };

            // A requested disconnect (user-initiated, or this connection
            // superseded by a new one) reads as a clean "Disconnected."; every
            // other exit — stream EOF, a write/stall error, a failed connect —
            // reads as loss or failure. `run` is consumed here, releasing its
            // borrows before the teardown below uses them.
            let graceful = select! {
                () = run => false,
                _ = &mut disconnect_rx => true,
            };

            if connected {
                // End-of-stream decoder flush: a socket that closed mid-character
                // (a converting connection's pending multibyte lead byte) surfaces
                // as U+FFFD on the final line instead of vanishing.
                let tail = transcode.finish();
                if !tail.is_empty() {
                    feed_utf8(&mut vt_parser, &mut vt_processor, tail.as_bytes());
                    // Every parser feed ends in a flush. The finished tail cannot
                    // complete a line today, but keep the batching contract explicit
                    // rather than relying on the partial-line event below to flush.
                    vt_processor.sink_mut().flush_lines();
                    vt_processor.notify_end_of_buffer();
                }

                if let Some(mut writer) = raw_log.take()
                    && let Err(err) = writer.flush()
                {
                    warn!("Failed to flush raw log: {err:?}");
                }

                // Silently ignore errors here; when a session is closing the runtime may already be gone by the time
                // we get here. The notice itself is worded by the runtime, which
                // owns the connection clock (see `RuntimeAction::DisconnectNotice`).
                runtime_tx
                    .send(RuntimeAction::Disconnected {
                        connection_generation: generation,
                    })
                    .map(|()| {
                        runtime_tx
                            .send(RuntimeAction::DisconnectNotice {
                                connection_generation: generation,
                                graceful,
                            })
                            .ok();
                    })
                    .ok();
            } else if graceful {
                // The disconnect landed before the transport ever came up: the
                // attempt is abandoned with an echo alone. `Connected` (and
                // `sys:connect`) never fired, so no `Disconnected` unwind —
                // the runtime's connect/disconnect events stay strictly paired.
                // The rule reports it, so the abandoned attempt reads as one
                // row rather than a bare line under "Connecting to …".
                runtime_tx
                    .send(RuntimeAction::ConnectionAbandoned {
                        connection_generation: generation,
                    })
                    .ok();
            }
            trace!("Connection cleaning up");
            clear_socket_sender(&socket_tx);
        });
    }

    pub fn disconnect(&mut self) {
        if let Some(disconnect) = self.disconnect.take() {
            disconnect.send(()).ok();
        }
    }

    /// Ties this connection's packet-completion markers to the runtime's
    /// "a deferred profile send is pending" bit. Set before [`Self::connect`];
    /// the connect task hands it to its [`VtProcessor`]. Without one, markers
    /// are emitted on every read batch.
    pub(crate) fn set_marker_armed_flag(&mut self, flag: Arc<std::sync::atomic::AtomicBool>) {
        self.marker_armed = Some(flag);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32};
    use std::time::Duration;

    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc as tokio_mpsc;
    use tokio::time::timeout;
    use tokio_rustls::TlsAcceptor;

    use super::telnet::{command, option};
    use super::*;

    fn test_connection() -> (Connection, tokio_mpsc::UnboundedReceiver<RuntimeAction>) {
        let (runtime_tx, runtime_rx) = tokio_mpsc::unbounded_channel();
        let (ui_tx, _ui_rx) = mpsc::channel(1);
        (
            Connection::new(
                runtime_tx,
                ui_tx,
                Arc::new(AtomicBool::new(false)),
                Arc::new(AtomicU32::new(responders::pack_dims(80, 24))),
                Arc::new(AtomicU32::new(0)),
            ),
            runtime_rx,
        )
    }

    #[tokio::test]
    async fn wss_carries_binary_telnet_frames_over_verified_tls() {
        install_tls_provider();
        let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()])
            .expect("self-signed test certificate");
        let certificate = CertificateDer::from(cert.cert.der().to_vec());
        let key = PrivateKeyDer::try_from(cert.key_pair.serialize_der()).expect("test key");
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![certificate.clone()], key)
            .expect("server TLS config");
        let acceptor = TlsAcceptor::from(Arc::new(server_config));
        let mut roots = rustls::RootCertStore::empty();
        roots.add(certificate).expect("trusted test root");
        let client_config = rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth();

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("local port").port();
        let reject_acceptor = acceptor.clone();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept TCP");
            let tls = acceptor.accept(tcp).await.expect("accept TLS");
            let mut ws = tokio_tungstenite::accept_async(tls)
                .await
                .expect("accept WebSocket");
            ws.send(WsMessage::Binary(Vec::new().into()))
                .await
                .expect("empty frame");
            ws.send(WsMessage::Binary(b"welcome\r\n".to_vec().into()))
                .await
                .expect("greeting");
            let command = ws.next().await.expect("outbound frame").expect("frame");
            assert_eq!(command, WsMessage::Binary(b"look\r\n".to_vec().into()));
        });

        let url = format!("wss://localhost:{port}/ws/telnet/?profile=test");
        let mut stream = timeout(
            Duration::from_secs(5),
            connect_wss_with_config(&url, Arc::new(client_config)),
        )
        .await
        .expect("WSS connect deadline")
        .expect("WSS connect");
        let mut bytes = Vec::with_capacity(64);
        let count = timeout(Duration::from_secs(5), stream.fill(&mut bytes))
            .await
            .expect("read deadline")
            .expect("read WSS frame");
        assert_eq!(&bytes[..count], b"welcome\r\n");
        stream
            .write_all_stall_guarded(b"look\r\n", Duration::from_secs(5))
            .await
            .expect("send command");
        timeout(Duration::from_secs(5), server)
            .await
            .expect("server deadline")
            .expect("server task");

        // The production connector uses the OS verifier, not the test root.
        // A fresh self-signed endpoint must fail rather than falling back.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("local port").port();
        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.expect("accept TCP");
            let _ = reject_acceptor.accept(tcp).await;
        });
        let url = format!("wss://localhost:{port}/ws/telnet/");
        assert!(
            timeout(Duration::from_secs(5), connect_wss(&url))
                .await
                .expect("certificate rejection deadline")
                .is_err()
        );
        timeout(Duration::from_secs(5), server)
            .await
            .expect("server deadline")
            .expect("server task");
    }

    /// The pre-bulk-path byte loop, kept as the reference the Ground-state fast path must
    /// match action for action.
    fn feed_bytewise(parser: &mut VTParser, processor: &mut VtProcessor, data: &[u8]) {
        for &b in data {
            if b != b'\n' && b != b'\r' {
                processor.push_raw_incoming_byte(b);
            }
            parser.parse_byte(b, processor);
        }
    }

    /// Everything line-shaped the processor emitted, rendered so two feeders compare whole:
    /// text, spans, links, and raw alike.
    fn line_transcript(rx: &mut tokio_mpsc::UnboundedReceiver<RuntimeAction>) -> Vec<String> {
        let mut out = Vec::new();
        while let Ok(action) = rx.try_recv() {
            match action {
                RuntimeAction::HandleIncomingLine(line) => out.push(format!("line {line:?}")),
                RuntimeAction::HandleIncomingFragmentedLine {
                    line,
                    completion_fragment,
                } => out.push(format!("fragmented {line:?} + {completion_fragment:?}")),
                RuntimeAction::HandleIncomingPartialLine(line) => {
                    out.push(format!("partial {line:?}"));
                }
                RuntimeAction::RetractIncomingPartialLine => out.push("retract".into()),
                RuntimeAction::PromptBoundary => out.push("prompt".into()),
                _ => {}
            }
        }
        out
    }

    /// Wire shapes that exercise every edge the bulk path has: SGR, overprint, multibyte
    /// text, encoded C1 controls (`C2 9B` is CSI), invalid and truncated UTF-8, DEL and other
    /// C0 controls, OSC 8 links with and without deceptive invisibles, and a run long enough
    /// to span many scanner words.
    fn fast_path_cases() -> Vec<Vec<u8>> {
        let mut cases: Vec<Vec<u8>> = vec![
            b"plain ascii line\n".to_vec(),
            b"\x1b[31mred\x1b[0m and \x1b[1;32mbold green\x1b[0m\n".to_vec(),
            b"\x1b[31mold\rnew\n".to_vec(),
            "h\u{e9}llo w\u{f6}rld \u{2014} \u{2713} \u{4f60}\u{597d} \u{1f642}\n".into(),
            "latin-1 range \u{a0}\u{bf}\u{c0}\u{ff}\n".into(),
            "C1 NEL\u{85}after\n".into(),
            "C1 CSI \u{9b}31mred\u{9b}0m plain\n".into(),
            b"invalid \xff\xfe bytes\n".to_vec(),
            b"overlong \xc0\x80 surrogate \xed\xa0\x80 lead \xf5 tail\n".to_vec(),
            b"lone continuation \x80\xbf x\n".to_vec(),
            b"truncated tail \xe4\xbd".to_vec(),
            b"del\x7fbel\x07tab\tfeed\x0cnul\x00end\n".to_vec(),
            b"\x1b]8;;http://example.com/\x1b\\link\x1b]8;;\x1b\\ after\n".to_vec(),
            "\x1b]8;;http://example.com/\x1b\\li\u{200b}nk \u{ad}x\x1b]8;;\x1b\\\n".into(),
            "\x1b]8;;http://example.com/\u{fc}\x1b\\\u{fc}\x1b]8;;\x1b\\\n".into(),
            b"no newline at all".to_vec(),
            b"\r\n\r\n".to_vec(),
            b"\x1b[2J\x1b[H\x1b(B\x1bM x\n".to_vec(),
            b"prompt> ".to_vec(),
        ];
        let mut long = vec![b'x'; 1000];
        long[500] = 0x1b;
        long.extend_from_slice(b"[1mrest\n");
        cases.push(long);
        let everything = cases.concat();
        cases.push(everything);
        cases
    }

    #[test]
    fn bulk_path_matches_the_bytewise_loop_at_every_chunking() {
        for (case_index, case) in fast_path_cases().iter().enumerate() {
            for chunk_len in [1, 2, 3, 5, 7, 8, 9, 13, 16, 64, usize::MAX] {
                for flush_between_chunks in [false, true] {
                    let (fast_tx, mut fast_rx) = tokio_mpsc::unbounded_channel();
                    let (reference_tx, mut reference_rx) = tokio_mpsc::unbounded_channel();
                    let mut fast = (VTParser::new(), VtProcessor::new(fast_tx));
                    let mut reference = (VTParser::new(), VtProcessor::new(reference_tx));
                    for chunk in case.chunks(chunk_len.min(case.len()).max(1)) {
                        ingest::feed_utf8(&mut fast.0, &mut fast.1, chunk);
                        feed_bytewise(&mut reference.0, &mut reference.1, chunk);
                        if flush_between_chunks {
                            fast.1.notify_end_of_buffer();
                            reference.1.notify_end_of_buffer();
                        }
                    }
                    fast.1.notify_end_of_buffer();
                    reference.1.notify_end_of_buffer();
                    assert_eq!(
                        line_transcript(&mut fast_rx),
                        line_transcript(&mut reference_rx),
                        "case {case_index}, chunk {chunk_len}, flush {flush_between_chunks}"
                    );
                }
            }
        }
    }

    #[test]
    fn transcoded_text_matches_the_bytewise_loop_and_keeps_c1_controls_visible() {
        // ISO-8859-15: 0xE9 decodes to a printable, 0x85 to U+0085 (NEL), which the parser
        // must still see as a control even though the text arrives pre-validated.
        let mut transcode = transcode::Transcode::new(encoding_rs::ISO_8859_15);
        let decoded = transcode
            .decode(b"caf\xe9\x85\x1b[31mx\x1b[0m\n plain\n")
            .to_owned();
        assert!(decoded.contains("caf\u{e9}\u{85}"));

        let (fast_tx, mut fast_rx) = tokio_mpsc::unbounded_channel();
        let (reference_tx, mut reference_rx) = tokio_mpsc::unbounded_channel();
        let mut fast = (VTParser::new(), VtProcessor::new(fast_tx));
        let mut reference = (VTParser::new(), VtProcessor::new(reference_tx));
        ingest::feed_str(&mut fast.0, &mut fast.1, &decoded);
        feed_bytewise(&mut reference.0, &mut reference.1, decoded.as_bytes());
        let fast_lines = line_transcript(&mut fast_rx);
        assert_eq!(fast_lines, line_transcript(&mut reference_rx));
        assert!(fast_lines[0].contains("caf\u{e9}x"), "{fast_lines:?}");
    }

    #[test]
    fn raw_capture_skips_revalidation_only_for_verified_bytes() {
        let (tx, mut rx) = tokio_mpsc::unbounded_channel();
        let mut parser = VTParser::new();
        let mut processor = VtProcessor::new(tx);
        ingest::feed_utf8(&mut parser, &mut processor, "h\u{e9}llo\n".as_bytes());
        ingest::feed_utf8(&mut parser, &mut processor, b"h\xffi\n");
        ingest::feed_utf8(&mut parser, &mut processor, b"back to ascii\n");
        let mut lines = Vec::new();
        while let Ok(action) = rx.try_recv() {
            if let RuntimeAction::HandleIncomingLine(line) = action {
                lines.push(line);
            }
        }
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].raw(), Some("h\u{e9}llo"));
        // The per-byte path pushed a high byte: the lossy re-decode applies, as it always has.
        assert_eq!(lines[1].raw(), Some("h\u{fffd}i"));
        // The doubt does not outlive the line that raised it.
        assert_eq!(lines[2].raw(), Some("back to ascii"));
    }

    #[test]
    fn bulk_fallback_matches_bytewise_on_large_adversarial_inputs() {
        for seed in [
            &b"\xfe"[..],
            &b"x\xfe"[..],
            &b"\xc2\x85"[..],
            &b"x\xc2\x85"[..],
        ] {
            let mut data = seed.repeat(65_536 / seed.len());
            data.extend_from_slice(b"\nrecovered\n");
            for raw in [false, true] {
                let (fast_tx, mut fast_rx) = tokio_mpsc::unbounded_channel();
                let (reference_tx, mut reference_rx) = tokio_mpsc::unbounded_channel();
                let mut fast = (VTParser::new(), VtProcessor::new(fast_tx));
                let mut reference = (VTParser::new(), VtProcessor::new(reference_tx));
                let flag = Arc::new(std::sync::atomic::AtomicBool::new(raw));
                fast.1.set_raw_wanted_flag(flag.clone());
                reference.1.set_raw_wanted_flag(flag);
                for chunk in data.chunks(16_384) {
                    ingest::feed_utf8(&mut fast.0, &mut fast.1, chunk);
                    feed_bytewise(&mut reference.0, &mut reference.1, chunk);
                }
                assert_eq!(
                    line_transcript(&mut fast_rx),
                    line_transcript(&mut reference_rx)
                );
            }
        }
    }

    #[test]
    fn socket_resolution_bypasses_name_service_for_localhost() {
        let port = 4000;
        let expected = [
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
            SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
        ];

        assert_eq!(localhost_candidates("localhost", port), Some(expected));
        assert_eq!(localhost_candidates("LOCALHOST", port), Some(expected));
        assert_eq!(localhost_candidates("localhost.", port), Some(expected));
        assert_eq!(localhost_candidates("example.com", port), None);
    }

    #[test]
    fn socket_resolution_preserves_bracketed_ipv6_hosts() {
        assert_eq!(unbracket_host("[::1]"), "::1");
        assert_eq!(unbracket_host("::1"), "::1");
    }

    #[test]
    fn mccpx_zstd_boundary_accepts_split_frame_magic_and_wont() {
        for len in 0..ZSTD_FRAME_MAGIC.len() {
            assert_eq!(
                classify_mccpx_zstd_boundary(&ZSTD_FRAME_MAGIC[..len]),
                MccpxZstdBoundary::NeedMore,
                "standard-frame prefix length {len}"
            );
        }
        assert_eq!(
            classify_mccpx_zstd_boundary(&ZSTD_FRAME_MAGIC),
            MccpxZstdBoundary::NextFrame
        );

        for len in 0..IAC_WONT_MCCPX.len() {
            assert_eq!(
                classify_mccpx_zstd_boundary(&IAC_WONT_MCCPX[..len]),
                MccpxZstdBoundary::NeedMore,
                "WONT prefix length {len}"
            );
        }
        assert_eq!(
            classify_mccpx_zstd_boundary(&IAC_WONT_MCCPX),
            MccpxZstdBoundary::Wont
        );
    }

    #[test]
    fn mccpx_zstd_boundary_rejects_every_other_prefix() {
        assert_eq!(
            classify_mccpx_zstd_boundary(b"plain"),
            MccpxZstdBoundary::Invalid
        );
        assert_eq!(
            classify_mccpx_zstd_boundary(&[ZSTD_FRAME_MAGIC[0], 0]),
            MccpxZstdBoundary::Invalid
        );
        // RFC 8878 skippable-frame magic in wire order. MCCPX carries Telnet application
        // data, so only standard data frames are accepted between BEGIN and WONT.
        assert_eq!(
            classify_mccpx_zstd_boundary(&[0x50, 0x2A, 0x4D, 0x18]),
            MccpxZstdBoundary::Invalid
        );
    }

    /// Run one inbound buffer through the real ingest bridge (telnet
    /// preprocessor + VT parser + reply buffer), returning the negotiation
    /// replies and every action the bridge queued.
    fn ingest_buffer(input: &[u8]) -> (Vec<u8>, Vec<RuntimeAction>) {
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);
        ingest_buffer_with(input, &mut protocol, &mut transcode)
    }

    /// Terminal-query answers the VT processor owes ride the bridge's reply buffer, in
    /// stream order with negotiation replies, and only when the server's graphics mode
    /// asks for them.
    #[test]
    fn terminal_queries_answer_through_the_bridge_reply_buffer() {
        use smudgy_protocol::graphics::GraphicsMode;
        let run = |mode: GraphicsMode| {
            let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
            let mut telnet = telnet::TelnetParser::new();
            let mut vt_parser = VTParser::new();
            let mut vt_processor = VtProcessor::new(runtime_tx.clone());
            vt_processor.set_graphics_mode(mode);
            vt_processor.set_terminal_geometry(
                Arc::new(AtomicU32::new(responders::pack_dims(100, 30))),
                Arc::new(AtomicU32::new(responders::pack_dims(
                    smudgy_protocol::graphics::cell_fixed(8.0),
                    smudgy_protocol::graphics::cell_fixed(17.0),
                ))),
            );
            let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
            let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);
            let mut replies = Vec::new();
            // DA1, then a TTYPE SEND the responder answers, then a cell-size report:
            // the answers must come out in that order.
            let input = b"hello\x1b[c\xff\xfa\x18\x01\xff\xf0\x1b[16tworld\r\n";
            let consumed = ingest::feed_inbound(
                input,
                &mut telnet,
                &mut vt_parser,
                &mut vt_processor,
                &mut replies,
                &runtime_tx,
                &mut protocol,
                &mut transcode,
            );
            assert_eq!(consumed, input.len());
            drop(runtime_tx);
            let mut lines = Vec::new();
            while let Ok(action) = runtime_rx.try_recv() {
                if let RuntimeAction::HandleIncomingLine(line) = action {
                    lines.push(line.text.clone());
                }
            }
            (replies, lines)
        };

        let (replies, lines) = run(GraphicsMode::Full);
        assert_eq!(lines, ["helloworld"]);
        let find = |needle: &[u8]| replies.windows(needle.len()).position(|w| w == needle);
        let da1 = find(b"\x1b[?62;c").expect("DA1 answered");
        let ttype = find(&[0xff, 0xfa, 0x18]).expect("TTYPE answered");
        let size = find(b"\x1b[6;17;8t").expect("cell size reported");
        assert!(
            da1 < ttype && ttype < size,
            "stream order kept: {replies:?}"
        );

        let (replies, lines) = run(GraphicsMode::DisplayOnly);
        assert_eq!(lines, ["helloworld"]);
        assert!(
            !replies.windows(2).any(|w| w == b"\x1b["),
            "no terminal answers without the full protocol: {replies:?}"
        );
    }

    /// [`ingest_buffer`] against caller-owned responder/transcode state, for tests that
    /// span multiple buffers or assert on the state itself.
    fn ingest_buffer_with(
        input: &[u8],
        protocol: &mut responders::ProtocolState,
        transcode: &mut transcode::Transcode,
    ) -> (Vec<u8>, Vec<RuntimeAction>) {
        let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut telnet = telnet::TelnetParser::new();
        let mut vt_parser = VTParser::new();
        let mut vt_processor = VtProcessor::new(runtime_tx.clone());
        let mut replies = Vec::new();
        let consumed = ingest::feed_inbound(
            input,
            &mut telnet,
            &mut vt_parser,
            &mut vt_processor,
            &mut replies,
            &runtime_tx,
            protocol,
            transcode,
        );
        assert_eq!(
            consumed,
            input.len(),
            "no test through this helper carries a compression-start marker"
        );
        drop(runtime_tx);
        let mut actions = Vec::new();
        while let Ok(action) = runtime_rx.try_recv() {
            actions.push(action);
        }
        (replies, actions)
    }

    #[test]
    fn batched_lines_do_not_cross_protocol_events_or_wait_for_another_feed() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let mut processor = VtProcessor::new(vt_processor::NativeVtSink::batched(tx.clone()));
        let mut parser = VTParser::new();
        let mut telnet = telnet::TelnetParser::new();
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);
        let mut replies = Vec::new();
        let mut input = b"one\r\ntwo\r\n".to_vec();
        telnet::frame_subnegotiation(telnet::option::GMCP, b"Test.Event {}", &mut input);
        input.extend_from_slice(b"three\r\nfour\r\n");
        input.extend_from_slice(&[
            telnet::command::IAC,
            telnet::command::WILL,
            telnet::option::ECHO,
        ]);
        input.extend_from_slice(b"tail\r\n");
        assert_eq!(
            ingest::feed_inbound(
                &input,
                &mut telnet,
                &mut parser,
                &mut processor,
                &mut replies,
                &tx,
                &mut protocol,
                &mut transcode
            ),
            input.len()
        );
        assert!(
            matches!(rx.try_recv(), Ok(RuntimeAction::HandleIncomingLines(lines))
            if lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>() == ["one", "two"])
        );
        assert!(
            matches!(rx.try_recv(), Ok(RuntimeAction::GmcpMessage { name, .. }) if &*name == "Test.Event")
        );
        assert!(
            matches!(rx.try_recv(), Ok(RuntimeAction::HandleIncomingLines(lines))
            if lines.iter().map(|line| line.text.as_str()).collect::<Vec<_>>() == ["three", "four"])
        );
        assert!(matches!(
            rx.try_recv(),
            Ok(RuntimeAction::ServerEchoChanged { enabled: true })
        ));
        assert!(
            matches!(rx.try_recv(), Ok(RuntimeAction::HandleIncomingLine(line)) if line.text == "tail")
        );
        assert!(rx.try_recv().is_err());
    }

    /// The server-ECHO password signal end to end through the connection's
    /// ingest bridge: `WILL ECHO` is answered `DO` on the wire and queued as
    /// `ServerEchoChanged { enabled: true }`; the matching `WONT` is answered
    /// `DONT` and queued disabled.
    #[test]
    fn server_echo_negotiation_queues_the_mask_actions() {
        let (replies, actions) = ingest_buffer(&[command::IAC, command::WILL, option::ECHO]);
        assert_eq!(replies, &[command::IAC, command::DO, option::ECHO]);
        assert!(
            matches!(
                actions.as_slice(),
                [RuntimeAction::ServerEchoChanged { enabled: true }]
            ),
            "WILL ECHO must queue exactly the enable action; got {actions:?}"
        );

        // WONT without a prior WILL is a no-op (nothing was enabled)…
        let (replies, actions) = ingest_buffer(&[command::IAC, command::WONT, option::ECHO]);
        assert!(replies.is_empty());
        assert!(actions.is_empty());

        // …and the full lifecycle reports both edges on one parser.
        let (replies, actions) = ingest_buffer(&[
            command::IAC,
            command::WILL,
            option::ECHO,
            command::IAC,
            command::WONT,
            option::ECHO,
        ]);
        assert_eq!(
            replies,
            &[
                command::IAC,
                command::DO,
                option::ECHO,
                command::IAC,
                command::DONT,
                option::ECHO,
            ]
        );
        assert!(
            matches!(
                actions.as_slice(),
                [
                    RuntimeAction::ServerEchoChanged { enabled: true },
                    RuntimeAction::ServerEchoChanged { enabled: false },
                ]
            ),
            "the lifecycle must queue enable then disable; got {actions:?}"
        );
    }

    /// An MSSP subnegotiation end to end through the ingest bridge: the payload decodes
    /// to `MsspVariables` pairs (no reply — MSSP has no client→server direction), and a
    /// payload past the cap is dropped whole, never parsed.
    #[test]
    fn mssp_subnegotiation_forwards_decoded_variables_and_enforces_the_cap() {
        let mut input = vec![command::IAC, command::SB, option::MSSP, mssp::marker::VAR];
        input.extend_from_slice(b"NAME");
        input.push(mssp::marker::VAL);
        input.extend_from_slice(b"ArcticMUD");
        input.extend_from_slice(&[command::IAC, command::SE]);
        let (replies, actions) = ingest_buffer(&input);
        assert!(replies.is_empty(), "MSSP is inbound-only");
        match actions.as_slice() {
            [RuntimeAction::MsspVariables { pairs, .. }] => assert_eq!(
                pairs.as_slice(),
                &[(
                    "NAME".to_string(),
                    mssp::MsspValue::Text("ArcticMUD".to_string())
                )]
            ),
            other => panic!("expected one MsspVariables action; got {other:?}"),
        }

        // One byte past the cap: dropped whole.
        let mut oversized = vec![command::IAC, command::SB, option::MSSP, mssp::marker::VAR];
        oversized.extend_from_slice(b"NAME");
        oversized.push(mssp::marker::VAL);
        oversized.resize(3 + mssp::MAX_INBOUND_PAYLOAD + 1, b'a');
        oversized.extend_from_slice(&[command::IAC, command::SE]);
        let (replies, actions) = ingest_buffer(&oversized);
        assert!(replies.is_empty());
        assert!(actions.is_empty(), "an over-cap payload must be dropped");
    }

    /// The cap boundary through the wire: a payload of exactly
    /// `MAX_INBOUND_PAYLOAD` bytes parses whole — the drop threshold is
    /// strictly past the cap.
    #[test]
    fn mssp_payload_at_the_cap_parses_through_the_bridge() {
        let mut input = vec![command::IAC, command::SB, option::MSSP, mssp::marker::VAR];
        input.extend_from_slice(b"NAME");
        input.push(mssp::marker::VAL);
        input.resize(3 + mssp::MAX_INBOUND_PAYLOAD, b'a');
        input.extend_from_slice(&[command::IAC, command::SE]);
        let (replies, actions) = ingest_buffer(&input);
        assert!(replies.is_empty());
        match actions.as_slice() {
            [RuntimeAction::MsspVariables { pairs, .. }] => {
                assert_eq!(pairs.len(), 1);
                assert_eq!(pairs[0].0, "NAME");
                assert!(matches!(
                    &pairs[0].1,
                    mssp::MsspValue::Text(value) if value.len() == mssp::MAX_INBOUND_PAYLOAD - 6
                ));
            }
            other => panic!("expected one MsspVariables action; got {other:?}"),
        }
    }

    /// Malformed MSSP shapes through the wire: an orphan `VAL` lead-in is
    /// skipped, a nameless `VAR` is dropped whole, a `VAR` with no `VAL`
    /// carries empty text, and a payload yielding no variables queues no
    /// action at all — never a panic, never a reply.
    #[test]
    fn mssp_hostile_shapes_through_the_wire_forward_only_named_variables() {
        // Orphan VAL before the first VAR: the named variable behind it survives.
        let mut input = vec![command::IAC, command::SB, option::MSSP, mssp::marker::VAL];
        input.extend_from_slice(b"orphan");
        input.push(mssp::marker::VAR);
        input.extend_from_slice(b"CONTACT");
        input.push(mssp::marker::VAR);
        input.extend_from_slice(b"NAME");
        input.push(mssp::marker::VAL);
        input.extend_from_slice(b"x");
        input.extend_from_slice(&[command::IAC, command::SE]);
        let (replies, actions) = ingest_buffer(&input);
        assert!(replies.is_empty());
        match actions.as_slice() {
            [RuntimeAction::MsspVariables { pairs, .. }] => assert_eq!(
                pairs.as_slice(),
                &[
                    ("CONTACT".to_string(), mssp::MsspValue::Text(String::new())),
                    ("NAME".to_string(), mssp::MsspValue::Text("x".to_string())),
                ]
            ),
            other => panic!("expected one MsspVariables action; got {other:?}"),
        }

        // Nothing parseable — a nameless VAR, marker-free noise, an empty
        // subnegotiation — queues nothing.
        for payload in [
            &[mssp::marker::VAR, mssp::marker::VAL, b'l', b'o', b's', b't'][..],
            b"marker-free noise",
            &[],
        ] {
            let mut input = vec![command::IAC, command::SB, option::MSSP];
            input.extend_from_slice(payload);
            input.extend_from_slice(&[command::IAC, command::SE]);
            let (replies, actions) = ingest_buffer(&input);
            assert!(replies.is_empty());
            assert!(actions.is_empty(), "payload {payload:?} must queue nothing");
        }
    }

    /// Volume abuse through the wire: an array bomb (hundreds of `VAL`s under
    /// one variable) and a kilobytes-long value both arrive intact under the
    /// cap — the store budget downstream is the sizing authority, not the
    /// parser.
    #[test]
    fn mssp_array_bombs_and_kilobyte_values_arrive_intact() {
        let mut input = vec![command::IAC, command::SB, option::MSSP, mssp::marker::VAR];
        input.extend_from_slice(b"PORT");
        for _ in 0..300 {
            input.push(mssp::marker::VAL);
            input.extend_from_slice(b"6667");
        }
        input.push(mssp::marker::VAR);
        input.extend_from_slice(b"CONTACT");
        input.push(mssp::marker::VAL);
        input.extend_from_slice(&vec![b'a'; 4096]);
        input.extend_from_slice(&[command::IAC, command::SE]);
        let (replies, actions) = ingest_buffer(&input);
        assert!(replies.is_empty());
        match actions.as_slice() {
            [RuntimeAction::MsspVariables { pairs, .. }] => {
                assert_eq!(pairs.len(), 2);
                assert!(matches!(
                    &pairs[0].1,
                    mssp::MsspValue::List(items)
                        if items.len() == 300 && items.iter().all(|item| item == "6667")
                ));
                assert!(matches!(
                    &pairs[1].1,
                    mssp::MsspValue::Text(value) if value.len() == 4096
                ));
            }
            other => panic!("expected one MsspVariables action; got {other:?}"),
        }
    }

    /// MSSP values decode against the connect-time configured encoding, with
    /// the telnet layer's `IAC IAC` un-escape applied first — an escaped
    /// `0xFF` is one text byte by the time the decoder sees it.
    #[test]
    fn mssp_values_decode_against_the_configured_encoding() {
        let mut input = vec![command::IAC, command::SB, option::MSSP, mssp::marker::VAR];
        input.extend_from_slice(b"NAME");
        input.push(mssp::marker::VAL);
        input.extend_from_slice(b"Caf\xE9 ");
        // 0xFF, escaped on the wire as IAC IAC.
        input.extend_from_slice(&[command::IAC, command::IAC]);
        input.extend_from_slice(&[command::IAC, command::SE]);

        // A Latin-1 configured server decodes both bytes as it meant them…
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::WINDOWS_1252);
        let (_, actions) = ingest_buffer_with(&input, &mut protocol, &mut transcode);
        match actions.as_slice() {
            [RuntimeAction::MsspVariables { pairs, .. }] => assert_eq!(
                pairs.as_slice(),
                &[(
                    "NAME".to_string(),
                    mssp::MsspValue::Text("Caf\u{e9} \u{ff}".to_string())
                )]
            ),
            other => panic!("expected one MsspVariables action; got {other:?}"),
        }

        // …while the UTF-8 default replaces them rather than dropping the
        // variable.
        let (_, actions) = ingest_buffer(&input);
        match actions.as_slice() {
            [RuntimeAction::MsspVariables { pairs, .. }] => assert_eq!(
                pairs.as_slice(),
                &[(
                    "NAME".to_string(),
                    mssp::MsspValue::Text("Caf\u{fffd} \u{fffd}".to_string())
                )]
            ),
            other => panic!("expected one MsspVariables action; got {other:?}"),
        }
    }

    /// The TTYPE/MTTS identity handshake end to end through the ingest bridge:
    /// `DO TTYPE` is answered `WILL`, and each `SEND` subnegotiation gets the
    /// next `IS` response in the MTTS cycle, framed into the same reply buffer.
    #[test]
    fn ttype_send_is_answered_with_the_mtts_cycle() {
        use super::telnet::option::TTYPE;
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);

        let send = [
            command::IAC,
            command::SB,
            TTYPE,
            responders::ttype::SEND,
            command::IAC,
            command::SE,
        ];

        let mut negotiate_and_send = vec![command::IAC, command::DO, TTYPE];
        negotiate_and_send.extend_from_slice(&send);
        let (replies, _) = ingest_buffer_with(&negotiate_and_send, &mut protocol, &mut transcode);

        let mut expected = vec![command::IAC, command::WILL, TTYPE];
        telnet::frame_subnegotiation(
            TTYPE,
            &[&[responders::ttype::IS], responders::CLIENT_NAME.as_bytes()].concat(),
            &mut expected,
        );
        assert_eq!(replies, expected, "WILL + IS <client name> in one reply");

        // Subsequent SENDs advance the cycle: terminal type, then the MTTS
        // bitvector repeated verbatim.
        for want in [
            responders::TERMINAL_TYPE.to_string(),
            format!("MTTS {}", responders::mtts::bitvector(false)),
            format!("MTTS {}", responders::mtts::bitvector(false)),
        ] {
            let (replies, _) = ingest_buffer_with(&send, &mut protocol, &mut transcode);
            let mut expected = Vec::new();
            telnet::frame_subnegotiation(
                TTYPE,
                &[&[responders::ttype::IS], want.as_bytes()].concat(),
                &mut expected,
            );
            // A fresh parser per buffer re-answers nothing here (no negotiation
            // in the buffer), so the reply is exactly the IS frame.
            assert_eq!(replies, expected);
        }
    }

    #[test]
    fn new_environ_advertises_osc8_tooltips_end_to_end() {
        let option = telnet::option::NEW_ENVIRON;
        let requested = [
            &[responders::new_environ::SEND, 3][..],
            b"OSC_HYPERLINKS_TOOLTIP",
            &[3][..],
            b"OSC_HYPERLINKS_TOOLTIP_SGR",
        ]
        .concat();
        let mut input = vec![command::IAC, command::DO, option];
        telnet::frame_subnegotiation(option, &requested, &mut input);

        let (replies, actions) = ingest_buffer(&input);
        assert!(actions.is_empty());
        let mut expected = vec![command::IAC, command::WILL, option];
        responders::new_environ::answer_send(
            &requested[1..],
            responders::mtts::bitvector(false),
            "UTF-8",
            &mut expected,
        );
        assert_eq!(replies, expected);
        assert!(
            replies
                .windows(b"OSC_HYPERLINKS_TOOLTIP".len())
                .any(|window| window == b"OSC_HYPERLINKS_TOOLTIP")
        );
        assert!(
            replies
                .windows(b"OSC_HYPERLINKS_TOOLTIP_SGR".len())
                .any(|window| window == b"OSC_HYPERLINKS_TOOLTIP_SGR")
        );
    }

    /// MNES end to end: a server that queries identity through NEW-ENVIRON
    /// `VAR`s (skipping TTYPE entirely, as MNES allows) gets the terminal type
    /// and MTTS bitvector, with `CHARSET` reflecting the active wire encoding
    /// rather than the UTF-8 default.
    #[test]
    fn new_environ_answers_mnes_identity_end_to_end() {
        let option = telnet::option::NEW_ENVIRON;
        let requested = [
            &[responders::new_environ::SEND, 0][..],
            b"TERMINAL_TYPE",
            &[0][..],
            b"MTTS",
            &[0][..],
            b"CHARSET",
        ]
        .concat();
        let mut input = vec![command::IAC, command::DO, option];
        telnet::frame_subnegotiation(option, &requested, &mut input);

        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::WINDOWS_1252);
        let (replies, actions) = ingest_buffer_with(&input, &mut protocol, &mut transcode);
        assert!(actions.is_empty());

        let mut expected = vec![command::IAC, command::WILL, option];
        telnet::frame_subnegotiation(
            option,
            &[
                &[responders::new_environ::IS, 0][..],
                b"TERMINAL_TYPE",
                &[1][..],
                responders::TERMINAL_TYPE.as_bytes(),
                &[0][..],
                b"MTTS",
                &[1][..],
                responders::mtts::bitvector(false).to_string().as_bytes(),
                &[0][..],
                b"CHARSET",
                &[1][..],
                b"windows-1252",
            ]
            .concat(),
            &mut expected,
        );
        assert_eq!(replies, expected);
    }

    /// `DO NAWS` is accepted and immediately answered with the current window
    /// size (RFC 1073 requires the first report right after the WILL).
    #[test]
    fn do_naws_is_answered_with_will_and_an_immediate_size_report() {
        use super::telnet::option::NAWS;
        let mut protocol = responders::ProtocolState::with_fixed_dims((120, 40));
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);
        let (replies, _) = ingest_buffer_with(
            &[command::IAC, command::DO, NAWS],
            &mut protocol,
            &mut transcode,
        );
        let mut expected = vec![command::IAC, command::WILL, NAWS];
        telnet::frame_subnegotiation(NAWS, &[0, 120, 0, 40], &mut expected);
        assert_eq!(replies, expected);
    }

    /// The full CHARSET flow through the ingest bridge: `WILL CHARSET` is answered `DO`,
    /// a REQUEST offering Latin-1 is ACCEPTED, and the very next application bytes decode
    /// through the switched encoding — including an `IAC IAC`-escaped `0xFF` (`ÿ`), which
    /// the telnet layer un-escapes *before* the decoder sees it.
    #[test]
    fn charset_request_switches_decoding_at_the_stream_position() {
        use super::telnet::option::CHARSET;
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);

        let mut input = vec![command::IAC, command::WILL, CHARSET];
        input.extend_from_slice(&[command::IAC, command::SB, CHARSET]);
        input.push(responders::charset::REQUEST);
        input.extend_from_slice(b";windows-1252");
        input.extend_from_slice(&[command::IAC, command::SE]);
        // Application bytes in the new encoding, in the same buffer:
        // "café ÿ\r\n" with é = 0xE9 and ÿ = 0xFF (escaped as IAC IAC on the wire).
        input.extend_from_slice(&[b'c', b'a', b'f', 0xE9, b' ', command::IAC, command::IAC]);
        input.extend_from_slice(b"\r\n");

        let (replies, actions) = ingest_buffer_with(&input, &mut protocol, &mut transcode);

        let mut expected = vec![command::IAC, command::DO, CHARSET];
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::ACCEPTED][..], b"windows-1252"].concat(),
            &mut expected,
        );
        assert_eq!(replies, expected);
        assert_eq!(transcode.encoding(), encoding_rs::WINDOWS_1252);

        let line = actions.iter().find_map(|action| match action {
            RuntimeAction::HandleIncomingLine(line) => Some(line.text.clone()),
            _ => None,
        });
        assert_eq!(line.as_deref(), Some("caf\u{e9} \u{ff}"));
    }

    /// RFC 2066's minimal implementation, end to end: `DO CHARSET` is answered `WILL
    /// CHARSET` and, immediately behind it, our own `REQUEST`. A server that sent only `DO`
    /// is not entitled to request, so this is the whole negotiation if we do not.
    #[test]
    fn do_charset_is_answered_with_will_and_our_own_request() {
        use super::telnet::option::CHARSET;
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);

        let (replies, _) = ingest_buffer_with(
            &[command::IAC, command::DO, CHARSET],
            &mut protocol,
            &mut transcode,
        );

        let mut expected = vec![command::IAC, command::WILL, CHARSET];
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::REQUEST][..], b" UTF-8"].concat(),
            &mut expected,
        );
        assert_eq!(replies, expected);
    }

    /// The other half of that exchange: a connection configured to windows-1252 offers it
    /// first, the server takes the UTF-8 fallback, and the application bytes right behind the
    /// `ACCEPTED` decode through the switched encoding.
    #[test]
    fn charset_accepted_switches_decoding_at_the_stream_position() {
        use super::telnet::option::CHARSET;
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::WINDOWS_1252);

        let mut input = vec![command::IAC, command::DO, CHARSET];
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::ACCEPTED][..], b"UTF-8"].concat(),
            &mut input,
        );
        // "café\r\n" as UTF-8 — the same bytes read as "cafÃ©" under windows-1252.
        input.extend_from_slice("caf\u{e9}\r\n".as_bytes());

        let (replies, actions) = ingest_buffer_with(&input, &mut protocol, &mut transcode);

        let mut expected = vec![command::IAC, command::WILL, CHARSET];
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::REQUEST][..], b" windows-1252 UTF-8"].concat(),
            &mut expected,
        );
        assert_eq!(replies, expected, "the ACCEPTED itself is not answered");
        assert_eq!(transcode.encoding(), encoding_rs::UTF_8);

        let line = actions.iter().find_map(|action| match action {
            RuntimeAction::HandleIncomingLine(line) => Some(line.text.clone()),
            _ => None,
        });
        assert_eq!(line.as_deref(), Some("caf\u{e9}"));
    }

    /// A `TTABLE-IS` is declined with the `TTABLE-REJECTED` RFC 2066 §2 requires every
    /// implementation to be able to send — silence would strand the server, since §5 lets no
    /// further CHARSET subnegotiation start until this one completes.
    #[test]
    fn charset_ttable_is_declined_rather_than_ignored() {
        use super::telnet::option::CHARSET;
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);

        let mut input = vec![command::IAC, command::DO, CHARSET];
        telnet::frame_subnegotiation(
            CHARSET,
            &[responders::charset::TTABLE_IS, 0x08, 0x08],
            &mut input,
        );

        let (replies, _) = ingest_buffer_with(&input, &mut protocol, &mut transcode);

        let mut expected = vec![command::IAC, command::WILL, CHARSET];
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::REQUEST][..], b" UTF-8"].concat(),
            &mut expected,
        );
        telnet::frame_subnegotiation(
            CHARSET,
            &[responders::charset::TTABLE_REJECTED],
            &mut expected,
        );
        assert_eq!(replies, expected);
    }

    /// Only `ACCEPTED` and `REJECTED` answer a REQUEST of ours. A subnegotiation that is not
    /// an answer must leave the request outstanding — closing the exchange early would discard
    /// the real answer behind it and leave the two sides reading each other in different
    /// charsets.
    #[test]
    fn a_non_answer_does_not_consume_the_outstanding_charset_request() {
        use super::telnet::option::CHARSET;
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::WINDOWS_1252);

        let mut input = vec![command::IAC, command::DO, CHARSET];
        // Neither of these answers our REQUEST: an empty subnegotiation and a TTABLE-ACK.
        telnet::frame_subnegotiation(CHARSET, &[], &mut input);
        telnet::frame_subnegotiation(CHARSET, &[6], &mut input);
        // ...so the ACCEPTED behind them is still live and must land.
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::ACCEPTED][..], b"UTF-8"].concat(),
            &mut input,
        );

        let (_, _) = ingest_buffer_with(&input, &mut protocol, &mut transcode);
        assert_eq!(
            transcode.encoding(),
            encoding_rs::UTF_8,
            "the ACCEPTED must still be honored"
        );
    }

    /// An `ACCEPTED` naming a charset we never offered is ignored — the server picks from our
    /// list, and honoring anything else would hand it the session's encoding outright. So is
    /// one that arrives with no request of ours outstanding.
    #[test]
    fn charset_accepted_is_ignored_when_unoffered_or_unsolicited() {
        use super::telnet::option::CHARSET;
        let accepted_big5 = {
            let mut frame = Vec::new();
            telnet::frame_subnegotiation(
                CHARSET,
                &[&[responders::charset::ACCEPTED][..], b"big5"].concat(),
                &mut frame,
            );
            frame
        };

        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);
        let mut negotiated = vec![command::IAC, command::DO, CHARSET];
        negotiated.extend_from_slice(&accepted_big5);
        ingest_buffer_with(&negotiated, &mut protocol, &mut transcode);
        assert_eq!(
            transcode.encoding(),
            encoding_rs::UTF_8,
            "big5 was never in our offer"
        );

        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);
        let (replies, _) = ingest_buffer_with(&accepted_big5, &mut protocol, &mut transcode);
        assert_eq!(transcode.encoding(), encoding_rs::UTF_8);
        assert!(replies.is_empty(), "an unsolicited answer is inert");
    }

    /// RFC 2066's simultaneous-request rule: when the server's own `REQUEST` crosses ours, the
    /// server's wins — we answer it, and the negative acknowledgment it owes ours is nothing
    /// we wait for.
    #[test]
    fn a_server_request_overrides_our_outstanding_one() {
        use super::telnet::option::CHARSET;
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);

        let mut input = vec![command::IAC, command::DO, CHARSET];
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::REQUEST][..], b" big5"].concat(),
            &mut input,
        );
        // The server's rejection of our crossed request must not be mistaken for an answer
        // that still matters.
        telnet::frame_subnegotiation(CHARSET, &[responders::charset::REJECTED], &mut input);

        let (replies, _) = ingest_buffer_with(&input, &mut protocol, &mut transcode);

        let mut expected = vec![command::IAC, command::WILL, CHARSET];
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::REQUEST][..], b" UTF-8"].concat(),
            &mut expected,
        );
        telnet::frame_subnegotiation(
            CHARSET,
            &[&[responders::charset::ACCEPTED][..], b"big5"].concat(),
            &mut expected,
        );
        assert_eq!(replies, expected);
        assert_eq!(transcode.encoding(), encoding_rs::BIG5);
    }

    #[tokio::test]
    async fn write_returns_an_error_when_the_socket_queue_is_closed() {
        let (mut connection, _runtime_rx) = test_connection();
        let (socket_tx, socket_rx) = tokio_mpsc::channel(1);
        let weak_socket_tx = socket_tx.downgrade();
        drop(socket_rx);
        connection.socket_tx = Arc::new(RwLock::new(Some(weak_socket_tx)));

        let error = connection
            .write(Arc::new("look".to_string()))
            .await
            .expect_err("a closed socket queue must be reported instead of panicking");

        assert!(error.to_string().contains("closed"));
        drop(socket_tx);
    }

    /// What the decoder contract asks of a packed-format decode, minus the
    /// threads: inflate within the exact length the size demands, refuse
    /// any other length, expand RGB to RGBA, and compose an animation frame
    /// onto its canvas — or, for an `a=c`, which carries no data, compose
    /// the source frame's rectangle onto it.
    fn decode_packed(
        request: &smudgy_protocol::graphics::DecodeRequest,
    ) -> smudgy_protocol::graphics::DecodeResult {
        use smudgy_protocol::graphics::{Compression, DecodedPixels, Format, GraphicsError};
        let from_source = request
            .compose
            .as_ref()
            .is_some_and(|compose| compose.source.is_some());
        let rgba: Vec<u8> = if from_source {
            Vec::new()
        } else {
            let expected = request
                .format
                .expected_len(request.width, request.height)
                .ok_or_else(|| GraphicsError::invalid("only packed formats decode here"))?;
            let expected = usize::try_from(expected).expect("validated sizes fit");
            let data = match request.compression {
                Compression::None => request.data.clone(),
                Compression::Zlib => smudgy_protocol::graphics::inflate(&request.data, expected)
                    .map_err(|_| GraphicsError::invalid("Too much image data"))?,
            };
            if data.len() != expected {
                return Err(GraphicsError::invalid("Insufficient image data"));
            }
            if request.format == Format::Rgb {
                data.chunks_exact(3)
                    .flat_map(|pixel| [pixel[0], pixel[1], pixel[2], 0xff])
                    .collect()
            } else {
                data
            }
        };
        let (width, height, rgba) = match &request.compose {
            Some(compose) => {
                let canvas = compose.apply(&rgba, request.width, request.height)?;
                (compose.canvas.0, compose.canvas.1, canvas)
            }
            None => (request.width, request.height, rgba),
        };
        Ok(DecodedPixels {
            width,
            height,
            rgba: rgba.into(),
        })
    }

    /// A decoder that completes on a timer, as the UI's off-thread decode would.
    struct TimerDecoder {
        delay: Duration,
        decodes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl GraphicsDecoder for TimerDecoder {
        fn decode(
            &self,
            request: smudgy_protocol::graphics::DecodeRequest,
            done: Box<dyn FnOnce(smudgy_protocol::graphics::DecodeResult) + Send + 'static>,
        ) {
            self.decodes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let delay = self.delay;
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                done(decode_packed(&request));
            });
        }
    }

    type GatedDecode = (
        smudgy_protocol::graphics::DecodeRequest,
        Box<dyn FnOnce(smudgy_protocol::graphics::DecodeResult) + Send + 'static>,
    );

    /// A decoder that holds every request until the test completes it.
    #[derive(Default)]
    struct GateDecoder {
        held: std::sync::Mutex<Vec<GatedDecode>>,
    }

    impl GraphicsDecoder for GateDecoder {
        fn decode(
            &self,
            request: smudgy_protocol::graphics::DecodeRequest,
            done: Box<dyn FnOnce(smudgy_protocol::graphics::DecodeResult) + Send + 'static>,
        ) {
            self.held.lock().unwrap().push((request, done));
        }
    }

    impl GateDecoder {
        async fn next(&self) -> GatedDecode {
            timeout(Duration::from_secs(5), async {
                loop {
                    if let Some(held) = self.held.lock().unwrap().pop() {
                        return held;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .expect("a decode should be requested")
        }
    }

    /// A decoder that loses the request: the callback is dropped unrun, as a
    /// decode task that panicked or was shut down would.
    struct DroppingDecoder(Arc<std::sync::atomic::AtomicUsize>);

    impl GraphicsDecoder for DroppingDecoder {
        fn decode(
            &self,
            _request: smudgy_protocol::graphics::DecodeRequest,
            done: Box<dyn FnOnce(smudgy_protocol::graphics::DecodeResult) + Send + 'static>,
        ) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            drop(done);
        }
    }

    /// A one-pixel transmit that must be answered, then DA1.
    const TRANSMIT_THEN_DA1: &[u8] = b"\x1b_Ga=T,f=24,s=1,v=1,i=7;AAAA\x1b\\\x1b[c";

    /// A server that sends [`TRANSMIT_THEN_DA1`] and collects what comes back
    /// until `done` says so, returning the bytes and how long after sending
    /// the first byte arrived.
    async fn graphics_server(
        listener: TcpListener,
        done: impl Fn(&[u8]) -> bool,
    ) -> (Vec<u8>, Option<Duration>) {
        let (mut socket, _) = listener.accept().await.expect("accept");
        socket
            .write_all(TRANSMIT_THEN_DA1)
            .await
            .expect("send the commands");
        let sent = std::time::Instant::now();
        let mut first_reply = None;
        let mut received = Vec::new();
        let mut chunk = [0_u8; 256];
        while !done(&received) {
            let n = timeout(Duration::from_secs(5), socket.read(&mut chunk))
                .await
                .expect("the client should answer or hang up")
                .expect("read");
            if n == 0 {
                break;
            }
            first_reply.get_or_insert_with(|| sent.elapsed());
            received.extend_from_slice(&chunk[..n]);
        }
        (received, first_reply)
    }

    #[tokio::test]
    async fn a_graphics_reply_waits_for_its_decode_and_keeps_command_order() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(graphics_server(listener, |received| {
            received.windows(7).any(|w| w == b"\x1b[?62;c")
        }));
        let decodes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (mut connection, mut runtime_rx) = test_connection();
        connection.set_graphics_mode(smudgy_protocol::graphics::GraphicsMode::Full);
        connection.set_graphics_decoder(Arc::new(TimerDecoder {
            delay: Duration::from_millis(150),
            decodes: decodes.clone(),
        }));
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;
        let (received, first_reply) = server.await.expect("server task");
        assert_eq!(
            received, b"\x1b_Gi=7;OK\x1b\\\x1b[?62;c",
            "the transmit's OK precedes DA1's answer"
        );
        assert!(
            first_reply.is_some_and(|waited| waited >= Duration::from_millis(150)),
            "nothing was answered before the decode finished: {first_reply:?}"
        );
        assert_eq!(decodes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_decoder_that_drops_the_request_still_settles_its_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(graphics_server(listener, |received| {
            received.windows(7).any(|w| w == b"\x1b[?62;c")
        }));
        let decodes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (mut connection, mut runtime_rx) = test_connection();
        connection.set_graphics_mode(smudgy_protocol::graphics::GraphicsMode::Full);
        connection.set_graphics_decoder(Arc::new(DroppingDecoder(decodes.clone())));
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;
        let (received, _) = server.await.expect("server task");
        assert_eq!(
            received, b"\x1b_Gi=7;EINVAL:decoder dropped the request\x1b\\\x1b[?62;c",
            "the lost decode fails its reply, and DA1's answer follows instead of waiting forever"
        );
        assert_eq!(decodes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_decode_finishing_after_disconnect_is_dropped_without_a_reply() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        // Collect until the client hangs up.
        let server = tokio::spawn(graphics_server(listener, |_| false));
        let decodes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (mut connection, mut runtime_rx) = test_connection();
        connection.set_graphics_mode(smudgy_protocol::graphics::GraphicsMode::Full);
        connection.set_graphics_decoder(Arc::new(TimerDecoder {
            delay: Duration::from_millis(400),
            decodes: decodes.clone(),
        }));
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;
        // Let the transmit parse and its decode start, then hang up under it.
        timeout(Duration::from_secs(5), async {
            while decodes.load(std::sync::atomic::Ordering::SeqCst) == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the decode should start");
        connection.disconnect();
        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(
                    runtime_rx.recv().await,
                    Some(RuntimeAction::Disconnected { .. })
                ) {
                    break;
                }
            }
        })
        .await
        .expect("the socket task should end on disconnect");
        let (received, _) = server.await.expect("server task");
        assert!(
            received.is_empty(),
            "no reply may be written for a decode that outlived its connection: {received:?}"
        );
        // The timer fires into a closed channel; nothing panics, nothing leaks a reply.
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(decodes.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    /// The whole pipeline: a compressed transmit-and-display arrives, the
    /// line it landed on reaches the runtime carrying the placement, the
    /// decode is handed over still compressed, and once it completes the
    /// record is ready and the reply goes out ahead of DA1's.
    #[tokio::test]
    async fn a_placement_reaches_the_buffer_and_its_reply_follows_the_decode() {
        use smudgy_protocol::graphics::{Compression, ImageData, Placement, encode_payload};
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &[0x10, 0x20, 0x30, 0x40, 0x50, 0x60]).unwrap();
        let compressed = encoder.finish().unwrap();
        let mut commands = b"ab".to_vec();
        commands.extend_from_slice(
            format!(
                "\x1b_Ga=T,i=7,f=24,s=2,v=1,o=z;{}\x1b\\",
                encode_payload(&compressed)
            )
            .as_bytes(),
        );
        commands.extend_from_slice(b"cd\r\n\x1b[c");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            socket
                .write_all(&commands)
                .await
                .expect("send the commands");
            let mut received = Vec::new();
            let mut chunk = [0_u8; 256];
            while !received.windows(7).any(|w| w == b"\x1b[?62;c") {
                let n = timeout(Duration::from_secs(5), socket.read(&mut chunk))
                    .await
                    .expect("the client should answer")
                    .expect("read");
                if n == 0 {
                    break;
                }
                received.extend_from_slice(&chunk[..n]);
            }
            received
        });
        let decoder = Arc::new(GateDecoder::default());
        let (mut connection, mut runtime_rx) = test_connection();
        connection.set_graphics_mode(smudgy_protocol::graphics::GraphicsMode::Full);
        connection.set_graphics_decoder(decoder.clone());
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;

        let line = timeout(Duration::from_secs(5), async {
            loop {
                let lines = match runtime_rx.recv().await.expect("the session is live") {
                    RuntimeAction::HandleIncomingLine(line) => vec![line],
                    RuntimeAction::HandleIncomingLines(lines) => lines,
                    _ => continue,
                };
                if let Some(line) = lines.into_iter().find(|line| line.objects.is_some()) {
                    return line;
                }
            }
        })
        .await
        .expect("the line with the placement reaches the runtime");
        // No cell size reported yet: 2 px wide in a 9 px fallback cell.
        assert_eq!(line.text, "ab cd");
        let object = &line.objects.as_ref().unwrap()[0];
        assert_eq!(object.range, 2..3);
        let placement = object
            .resource
            .clone()
            .downcast::<Placement>()
            .expect("the resource is the placement");
        assert_eq!((placement.image.id, placement.geometry.columns), (7, 1));
        assert_eq!(placement.image.data(), ImageData::Pending);

        let (request, done) = decoder.next().await;
        assert_eq!(request.compression, Compression::Zlib);
        assert_eq!(request.data, compressed, "inflating is the decoder's job");
        assert_eq!((request.width, request.height), (2, 1));
        assert!(
            !server.is_finished(),
            "DA1's answer waits behind the transmit's parked reply"
        );
        done(decode_packed(&request));

        let received = server.await.expect("server task");
        assert_eq!(
            received, b"\x1b_Gi=7;OK\x1b\\\x1b[?62;c",
            "the transmit's OK precedes DA1's answer"
        );
        let pixels = placement.pixels().expect("the record is ready");
        assert_eq!(
            &pixels.rgba[..],
            [0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff]
        );
    }

    #[tokio::test]
    async fn legacy_encoding_rejects_an_unmappable_command_without_wire_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            let mut bytes = [0_u8; 6];
            timeout(Duration::from_secs(5), socket.read_exact(&mut bytes))
                .await
                .expect("a later valid command should arrive")
                .expect("read");
            bytes
        });

        let (mut connection, mut runtime_rx) = test_connection();
        connection.connect(
            "127.0.0.1",
            port,
            None,
            Some(encoding_rs::WINDOWS_1252),
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;

        connection
            .write(Arc::new("go \u{2192} east\r\n".to_string()))
            .await
            .expect("the socket queue is live");

        let warning = timeout(Duration::from_secs(5), async {
            loop {
                if let Some(RuntimeAction::Echo(text)) = runtime_rx.recv().await
                    && text.contains("cannot be represented")
                {
                    break text;
                }
            }
        })
        .await
        .expect("the rejected command should be reported");
        assert!(
            !warning.contains('\u{2192}'),
            "feedback must not leak command content"
        );

        connection
            .write(Arc::new("look\r\n".to_string()))
            .await
            .expect("rejection must leave the connection usable");
        assert_eq!(
            server.await.expect("server task"),
            *b"look\r\n",
            "no prefix, replacement marker, or numeric reference may precede the valid command"
        );
    }

    #[tokio::test]
    async fn write_waits_for_capacity_instead_of_dropping_a_valid_frame() {
        let (mut connection, _runtime_rx) = test_connection();
        let (socket_tx, mut socket_rx) = tokio_mpsc::channel(1);
        connection.socket_tx = Arc::new(RwLock::new(Some(socket_tx.downgrade())));

        connection
            .write(Arc::new("first".to_string()))
            .await
            .expect("first frame fills the queue");

        let second = connection.write(Arc::new("second".to_string()));
        tokio::pin!(second);
        assert!(
            timeout(Duration::from_millis(20), &mut second)
                .await
                .is_err(),
            "a full queue should apply backpressure"
        );

        assert!(matches!(
            socket_rx.recv().await,
            Some(OutboundFrame::Text(text)) if text.as_str() == "first"
        ));
        timeout(Duration::from_secs(1), &mut second)
            .await
            .expect("write should resume when capacity is available")
            .expect("receiver remains open");
        assert!(matches!(
            socket_rx.recv().await,
            Some(OutboundFrame::Text(text)) if text.as_str() == "second"
        ));
    }

    #[tokio::test]
    async fn write_delivers_a_500_frame_pathfinding_burst_in_order() {
        const BURST: usize = 500;

        let (mut connection, _runtime_rx) = test_connection();
        let (socket_tx, mut socket_rx) = tokio_mpsc::channel(16);
        connection.socket_tx = Arc::new(RwLock::new(Some(socket_tx.downgrade())));

        let producer = async {
            for index in 0..BURST {
                connection
                    .write(Arc::new(index.to_string()))
                    .await
                    .expect("the live receiver should eventually make capacity");
            }
        };
        let consumer = async {
            for index in 0..BURST {
                assert!(matches!(
                    socket_rx.recv().await,
                    Some(OutboundFrame::Text(text)) if text.as_str() == index.to_string()
                ));
            }
        };

        timeout(Duration::from_secs(1), async {
            tokio::join!(producer, consumer);
        })
        .await
        .expect("the full burst should drain without drops or deadlock");
    }

    /// Large enough to overwhelm the pinned send buffer ([`SEND_BUFFER_BYTES`])
    /// plus the shrunken peer receive buffer many times over, so a write
    /// against a peer that never reads parks mid-write on a zero TCP window —
    /// the wedged-server condition. The pinned send buffer is what makes this
    /// portable: Windows's dynamic send buffering otherwise absorbs the whole
    /// frame (measured: 64 MiB accepted in ~5ms against a 4 KB receive buffer)
    /// and the write never parks.
    fn wedge_frame() -> Arc<[u8]> {
        Arc::from(vec![b'x'; 64 * 1024 * 1024])
    }

    /// Accepts one connection and never reads from it, holding the socket open
    /// until released: the kernel buffers fill and the client's write parks
    /// indefinitely, exactly like a wedged (zero-window) server. The receive
    /// buffer is shrunk before listening (accepted sockets inherit it) because
    /// loopback defaults can otherwise absorb tens of megabytes and let the
    /// write complete instead of wedging.
    fn never_reading_server() -> (u16, oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
        let socket = tokio::net::TcpSocket::new_v4().expect("socket");
        socket
            .set_recv_buffer_size(4096)
            .expect("shrink receive buffer");
        socket
            .bind("127.0.0.1:0".parse().expect("loopback address"))
            .expect("bind");
        let listener = socket.listen(8).expect("listen");
        let port = listener.local_addr().expect("listener address").port();
        let (release_tx, release_rx) = oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.expect("accept");
            release_rx.await.ok();
        });
        (port, release_tx, server)
    }

    /// Read from `socket` until `collected` ends with a full copy of `needle`.
    async fn read_until(
        socket: &mut tokio::net::TcpStream,
        collected: &mut Vec<u8>,
        needle: &[u8],
    ) {
        while !collected
            .windows(needle.len())
            .any(|window| window == needle)
        {
            let mut buf = [0_u8; 512];
            let read = timeout(Duration::from_secs(5), socket.read(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {needle:02x?}"))
                .expect("read");
            assert_ne!(read, 0, "socket closed waiting for {needle:02x?}");
            collected.extend_from_slice(&buf[..read]);
        }
    }

    /// RFC 2066 §5: while our CHARSET REQUEST is unanswered, outbound text waits — and once the
    /// exchange terminates it goes out **in the negotiated charset**. A windows-1252 connection
    /// whose server takes the UTF-8 fallback is the case that needs it: without the hold, a
    /// command written in that window would leave as windows-1252 while the server had already
    /// switched to decoding UTF-8.
    #[tokio::test]
    async fn outbound_text_waits_for_the_charset_answer_and_leaves_in_the_new_charset() {
        use super::telnet::option::CHARSET;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let (requested_tx, requested_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            socket
                .write_all(&[command::IAC, command::DO, CHARSET])
                .await
                .expect("DO CHARSET");

            let mut request = Vec::new();
            telnet::frame_subnegotiation(
                CHARSET,
                &[&[responders::charset::REQUEST][..], b" windows-1252 UTF-8"].concat(),
                &mut request,
            );
            let mut seen = Vec::new();
            read_until(&mut socket, &mut seen, &request).await;
            requested_tx.send(()).expect("client still waiting");

            // The command is written now; nothing may reach us until we answer.
            let mut buf = [0_u8; 64];
            assert!(
                timeout(Duration::from_millis(300), socket.read(&mut buf))
                    .await
                    .is_err(),
                "outbound text must be held while the REQUEST is unanswered"
            );

            let mut accepted = Vec::new();
            telnet::frame_subnegotiation(
                CHARSET,
                &[&[responders::charset::ACCEPTED][..], b"UTF-8"].concat(),
                &mut accepted,
            );
            socket.write_all(&accepted).await.expect("ACCEPTED");

            let mut command_bytes = Vec::new();
            read_until(&mut socket, &mut command_bytes, b"\r\n").await;
            command_bytes
        });

        let (mut connection, mut runtime_rx) = test_connection();
        connection.connect(
            "127.0.0.1",
            port,
            None,
            Some(encoding_rs::WINDOWS_1252),
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;
        requested_rx.await.expect("the REQUEST reached the server");
        connection
            .write(Arc::new("caf\u{e9}\r\n".to_string()))
            .await
            .expect("the socket queue is live");

        let sent = timeout(Duration::from_secs(10), server)
            .await
            .expect("server task")
            .expect("server task panicked");
        assert_eq!(
            sent,
            "caf\u{e9}\r\n".as_bytes(),
            "the held command must leave in the charset the negotiation settled on (UTF-8), \
             not the windows-1252 it was configured with"
        );
        connection.disconnect();
    }

    /// The default connection offers UTF-8 while already speaking it, so no answer could move
    /// the encoder and nothing is ever held — the negotiation is invisible to the write path.
    #[tokio::test]
    async fn a_utf8_connection_holds_nothing_while_negotiating_charset() {
        use super::telnet::option::CHARSET;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let (requested_tx, requested_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            socket
                .write_all(&[command::IAC, command::DO, CHARSET])
                .await
                .expect("DO CHARSET");

            let mut request = Vec::new();
            telnet::frame_subnegotiation(
                CHARSET,
                &[&[responders::charset::REQUEST][..], b" UTF-8"].concat(),
                &mut request,
            );
            let mut seen = Vec::new();
            read_until(&mut socket, &mut seen, &request).await;
            requested_tx.send(()).expect("client still waiting");

            // Never answered — and it must not matter.
            let started = tokio::time::Instant::now();
            let mut command_bytes = Vec::new();
            read_until(&mut socket, &mut command_bytes, b"\r\n").await;
            (started.elapsed(), command_bytes)
        });

        let (mut connection, mut runtime_rx) = test_connection();
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;
        requested_rx.await.expect("the REQUEST reached the server");
        connection
            .write(Arc::new("look\r\n".to_string()))
            .await
            .expect("the socket queue is live");

        let (waited, sent) = timeout(Duration::from_secs(10), server)
            .await
            .expect("server task")
            .expect("server task panicked");
        assert_eq!(sent, b"look\r\n");
        assert!(
            waited < Duration::from_millis(500),
            "a UTF-8 connection must not wait on CHARSET; it took {waited:?}"
        );
        connection.disconnect();
    }

    /// A server that opens with `DO CHARSET` and then ignores our REQUEST is ordinary, so the
    /// hold is bounded: the text goes out anyway, in the configured charset, and the connection
    /// stays usable.
    #[tokio::test]
    async fn held_output_is_released_when_no_charset_answer_arrives() {
        use super::telnet::option::CHARSET;
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let (requested_tx, requested_rx) = oneshot::channel();

        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            socket
                .write_all(&[command::IAC, command::DO, CHARSET])
                .await
                .expect("DO CHARSET");

            let mut seen = Vec::new();
            read_until(
                &mut socket,
                &mut seen,
                &[command::IAC, command::WILL, CHARSET],
            )
            .await;
            requested_tx.send(()).expect("client still waiting");

            // No answer, ever. The command must still arrive, after the bounded wait.
            let started = tokio::time::Instant::now();
            let mut command_bytes = Vec::new();
            read_until(&mut socket, &mut command_bytes, b"\r\n").await;
            (started.elapsed(), command_bytes)
        });

        let (mut connection, mut runtime_rx) = test_connection();
        connection.connect(
            "127.0.0.1",
            port,
            None,
            Some(encoding_rs::WINDOWS_1252),
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;
        requested_rx.await.expect("the REQUEST reached the server");
        connection
            .write(Arc::new("caf\u{e9}\r\n".to_string()))
            .await
            .expect("the socket queue is live");

        let (waited, sent) = timeout(Duration::from_secs(10), server)
            .await
            .expect("server task")
            .expect("server task panicked");
        assert!(
            waited >= Duration::from_millis(1500),
            "the text left after {waited:?}, before the hold could have expired"
        );
        assert_eq!(
            sent, b"caf\xe9\r\n",
            "with no negotiation to honor, the configured windows-1252 stands"
        );
        connection.disconnect();
    }

    async fn expect_connected(runtime_rx: &mut tokio_mpsc::UnboundedReceiver<RuntimeAction>) {
        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(runtime_rx.recv().await, Some(RuntimeAction::Connected)) {
                    break;
                }
            }
        })
        .await
        .expect("connection should become ready");
    }

    /// A scripted zero-window peer: accepts a fixed number of bytes, then
    /// never accepts another.
    struct WedgedWriter {
        accept: usize,
    }

    impl tokio::io::AsyncWrite for WedgedWriter {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            if self.accept == 0 {
                // No waker is registered: only the stall guard's timer can
                // end the write, exactly like a peer that never reads again.
                return std::task::Poll::Pending;
            }
            let written = self.accept.min(buf.len());
            self.accept -= written;
            std::task::Poll::Ready(Ok(written))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// A slow but healthy peer: accepts exactly one byte per write call.
    struct TrickleWriter;

    impl tokio::io::AsyncWrite for TrickleWriter {
        fn poll_write(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<io::Result<usize>> {
            std::task::Poll::Ready(Ok(usize::from(!buf.is_empty())))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
        ) -> std::task::Poll<io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn stall_guard_times_out_a_peer_that_stopped_accepting_bytes() {
        let mut wedged = WedgedWriter { accept: 10 };
        let error = write_all_with_stall_guard(&mut wedged, &[b'x'; 64], Duration::from_millis(50))
            .await
            .expect_err("a zero-window peer must time the write out");
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn stall_guard_lets_a_slowly_draining_peer_finish() {
        write_all_with_stall_guard(&mut TrickleWriter, &[b'x'; 256], Duration::from_millis(50))
            .await
            .expect("byte-at-a-time progress is never a stall");
    }

    #[tokio::test]
    async fn disconnect_preempts_a_write_wedged_on_a_peer_that_stopped_reading() {
        let (port, release_tx, server) = never_reading_server();

        let (mut connection, mut runtime_rx) = test_connection();
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;

        connection
            .write_raw(wedge_frame())
            .await
            .expect("the frame should be queued");
        // Give the socket task time to pick the frame up and park inside the
        // write before asking for the disconnect.
        tokio::time::sleep(Duration::from_millis(500)).await;

        connection.disconnect();

        let mut disconnected = false;
        let mut reported_disconnect = false;
        timeout(Duration::from_secs(5), async {
            loop {
                let action = runtime_rx.recv().await.expect("runtime action");
                match action {
                    RuntimeAction::Disconnected { .. } => disconnected = true,
                    RuntimeAction::DisconnectNotice { graceful: true, .. } => {
                        reported_disconnect = true;
                    }
                    _ => {}
                }
                if disconnected && reported_disconnect {
                    break;
                }
            }
        })
        .await
        .expect("disconnect should preempt the wedged write, not wait out TCP");

        release_tx.send(()).ok();
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn a_stalled_write_times_out_and_unparks_a_producer_awaiting_capacity() {
        let (port, release_tx, server) = never_reading_server();

        let (mut connection, mut runtime_rx) = test_connection();
        connection.write_stall_timeout = Duration::from_millis(300);
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;

        connection
            .write_raw(wedge_frame())
            .await
            .expect("the frame should be queued");

        // Fill the queue behind the wedged frame until a write parks awaiting
        // capacity, then keep writing: the stall timeout must tear the socket
        // down and surface an error here instead of leaving the producer
        // parked for the TCP stack's own timeout.
        let producer = async { while connection.write(Arc::new("n".to_string())).await.is_ok() {} };
        let watcher = async {
            let mut disconnected = false;
            let mut reported_loss = false;
            loop {
                let action = runtime_rx.recv().await.expect("runtime action");
                match action {
                    RuntimeAction::Disconnected { .. } => disconnected = true,
                    RuntimeAction::DisconnectNotice {
                        graceful: false, ..
                    } => {
                        reported_loss = true;
                    }
                    _ => {}
                }
                if disconnected && reported_loss {
                    break;
                }
            }
        };

        timeout(Duration::from_secs(10), async {
            tokio::join!(producer, watcher);
        })
        .await
        .expect("the stall timeout should tear down the socket and unpark the producer");

        release_tx.send(()).ok();
        server.await.expect("server task");
    }

    #[tokio::test]
    async fn connection_reads_data_and_reports_a_remote_close() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("accept");
            socket.write_all(b"hello\r\n").await.expect("write line");
            socket.shutdown().await.expect("shutdown");
        });

        let (mut connection, mut runtime_rx) = test_connection();
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );

        let mut connected = false;
        let mut received_line = false;
        let mut disconnected = false;
        let mut reported_loss = false;
        timeout(Duration::from_secs(5), async {
            loop {
                let action = runtime_rx.recv().await.expect("runtime action");
                match action {
                    RuntimeAction::Connected => connected = true,
                    RuntimeAction::HandleIncomingLine(line) if line.text == "hello" => {
                        received_line = true;
                    }
                    RuntimeAction::Disconnected { .. } => disconnected = true,
                    RuntimeAction::DisconnectNotice {
                        graceful: false, ..
                    } => {
                        reported_loss = true;
                    }
                    _ => {}
                }
                if connected && received_line && disconnected && reported_loss {
                    break;
                }
            }
        })
        .await
        .expect("connection should process the line and remote close");

        server.await.expect("server task");
    }

    #[tokio::test]
    async fn disconnect_cancels_a_pending_read_cleanly() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let (accepted_tx, accepted_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let server = tokio::spawn(async move {
            let (_socket, _) = listener.accept().await.expect("accept");
            accepted_tx.send(()).ok();
            release_rx.await.ok();
        });

        let (mut connection, mut runtime_rx) = test_connection();
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );

        timeout(Duration::from_secs(5), async {
            loop {
                if matches!(runtime_rx.recv().await, Some(RuntimeAction::Connected)) {
                    break;
                }
            }
        })
        .await
        .expect("connection should become ready");
        timeout(Duration::from_secs(5), accepted_rx)
            .await
            .expect("server should accept the connection")
            .expect("accept signal");

        connection.disconnect();

        let mut disconnected = false;
        let mut reported_disconnect = false;
        timeout(Duration::from_secs(5), async {
            loop {
                let action = runtime_rx.recv().await.expect("runtime action");
                match action {
                    RuntimeAction::Disconnected { .. } => disconnected = true,
                    RuntimeAction::DisconnectNotice { graceful: true, .. } => {
                        reported_disconnect = true;
                    }
                    _ => {}
                }
                if disconnected && reported_disconnect {
                    break;
                }
            }
        })
        .await
        .expect("disconnect should cancel the pending read");

        release_tx.send(()).ok();
        server.await.expect("server task");
    }

    #[test]
    fn local_media_are_allowed_only_for_loopback_peers() {
        let peer = |addr: &str| Some(addr.parse::<SocketAddr>().expect("a socket address"));
        assert!(local_media_allowed_for(peer("127.0.0.1:4000")));
        assert!(local_media_allowed_for(peer("127.1.2.3:4000")));
        assert!(local_media_allowed_for(peer("[::1]:4000")));
        assert!(local_media_allowed_for(peer("[::ffff:127.0.0.1]:4000")));
        assert!(!local_media_allowed_for(peer("192.0.2.7:4000")));
        assert!(!local_media_allowed_for(peer("10.0.0.1:4000")));
        assert!(!local_media_allowed_for(peer("0.0.0.0:4000")));
        assert!(!local_media_allowed_for(peer("[2001:db8::1]:4000")));
        assert!(!local_media_allowed_for(peer("[::ffff:192.0.2.7]:4000")));
        assert!(!local_media_allowed_for(None));
    }

    /// A host for a remote peer, for an unknown one, or for a loopback peer
    /// of a server not allowed local files answers `EBADF` without looking:
    /// a temporary file that a read would delete is still there after it.
    #[test]
    fn local_media_need_the_servers_opt_in_and_a_loopback_peer() {
        use smudgy_protocol::graphics::{GraphicsError, GraphicsHost as _, Medium};
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("tty-graphics-protocol-host.rgb");
        std::fs::write(&path, [1, 2, 3, 4, 5, 6]).expect("write the pixels");
        let name = path.to_str().expect("a UTF-8 path").as_bytes();
        let (done, _done_rx) = tokio::sync::mpsc::unbounded_channel();
        let decoder: Arc<dyn GraphicsDecoder> = Arc::new(GateDecoder::default());
        let loopback: Option<SocketAddr> = Some("127.0.0.1:4000".parse().expect("an address"));
        let remote: Option<SocketAddr> = Some("192.0.2.7:4000".parse().expect("an address"));

        for (peer, local_files) in [(remote, true), (None, true), (loopback, false)] {
            let mut host =
                ChannelGraphicsHost::new(decoder.clone(), done.clone(), peer, local_files);
            assert!(
                !host.local_media_allowed,
                "{peer:?}, local files {local_files}"
            );
            for medium in [Medium::File, Medium::TempFile, Medium::SharedMemory] {
                let err = host
                    .read_media(medium, name, 0, 0)
                    .expect_err("local media are refused");
                assert_eq!(err, GraphicsError::bad_file());
            }
            assert!(path.exists(), "the refused read never touched the file");
        }

        let mut local = ChannelGraphicsHost::new(decoder, done, loopback, true);
        assert!(local.local_media_allowed);
        assert_eq!(
            local.read_media(Medium::TempFile, name, 0, 0),
            Ok(vec![1, 2, 3, 4, 5, 6])
        );
        assert!(
            !path.exists(),
            "the loopback read deleted the temporary file"
        );
    }

    /// A host allowed local media, with a file for it to read.
    fn local_media_host() -> (ChannelGraphicsHost, tempfile::TempDir, Vec<u8>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("pixels.rgb");
        std::fs::write(&path, [7, 8, 9]).expect("write the pixels");
        let name = path.to_str().expect("a UTF-8 path").as_bytes().to_vec();
        let (done, _done_rx) = tokio::sync::mpsc::unbounded_channel();
        let host = ChannelGraphicsHost::new(
            Arc::new(GateDecoder::default()),
            done,
            Some("127.0.0.1:4000".parse().expect("an address")),
            true,
        );
        (host, dir, name)
    }

    /// `read_media` blocks the thread it runs on. On the socket runtime (a
    /// multi-thread one) it tells tokio so; outside a runtime, or on a
    /// current-thread one, where it cannot, it simply reads.
    #[test]
    fn read_media_reads_outside_a_runtime() {
        use smudgy_protocol::graphics::{GraphicsHost as _, Medium};
        let (mut host, _dir, name) = local_media_host();
        assert!(tokio::runtime::Handle::try_current().is_err());
        assert_eq!(host.read_media(Medium::File, &name, 1, 0), Ok(vec![8, 9]));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn read_media_reads_on_a_multi_thread_runtime() {
        use smudgy_protocol::graphics::{GraphicsHost as _, Medium};
        let (mut host, _dir, name) = local_media_host();
        assert_eq!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::MultiThread
        );
        assert_eq!(host.read_media(Medium::File, &name, 0, 2), Ok(vec![7, 8]));
    }

    #[tokio::test]
    async fn read_media_reads_on_a_current_thread_runtime() {
        use smudgy_protocol::graphics::{GraphicsHost as _, Medium};
        let (mut host, _dir, name) = local_media_host();
        assert_eq!(
            tokio::runtime::Handle::current().runtime_flavor(),
            tokio::runtime::RuntimeFlavor::CurrentThread
        );
        assert_eq!(
            host.read_media(Medium::File, &name, 0, 0),
            Ok(vec![7, 8, 9])
        );
    }

    /// Send `commands` to the first client and collect what it writes back
    /// until DA1's answer arrives.
    async fn graphics_exchange(listener: TcpListener, commands: Vec<u8>) -> Vec<u8> {
        let (mut socket, _) = listener.accept().await.expect("accept");
        socket
            .write_all(&commands)
            .await
            .expect("send the commands");
        let mut received = Vec::new();
        let mut chunk = [0_u8; 256];
        while !received.windows(7).any(|w| w == b"\x1b[?62;c") {
            let n = timeout(Duration::from_secs(5), socket.read(&mut chunk))
                .await
                .expect("the client should answer")
                .expect("read");
            if n == 0 {
                break;
            }
            received.extend_from_slice(&chunk[..n]);
        }
        received
    }

    /// The local-media pipeline over a loopback connection: a `t=f`
    /// transmit-and-display names a file of packed RGB, the client reads
    /// it, hands the bytes to the decoder, places the image, and answers
    /// `OK` once the record is ready.
    #[tokio::test]
    async fn a_loopback_peer_transmits_from_a_file() {
        use smudgy_protocol::graphics::{Compression, Format, Placement, encode_payload};
        let pixels = [0x10, 0x20, 0x30, 0x40, 0x50, 0x60];
        let file = tempfile::Builder::new()
            .prefix("tty-graphics-protocol-")
            .suffix(".rgb")
            .tempfile()
            .expect("temp file");
        std::fs::write(file.path(), pixels).expect("write the pixels");
        let name = file.path().to_str().expect("a UTF-8 path");
        let mut commands = b"ab".to_vec();
        commands.extend_from_slice(
            format!(
                "\x1b_Ga=T,t=f,f=24,s=2,v=1,i=9;{}\x1b\\",
                encode_payload(name.as_bytes())
            )
            .as_bytes(),
        );
        commands.extend_from_slice(b"cd\r\n\x1b[c");

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(graphics_exchange(listener, commands));
        let decoder = Arc::new(GateDecoder::default());
        let (mut connection, mut runtime_rx) = test_connection();
        connection.set_graphics_mode(smudgy_protocol::graphics::GraphicsMode::Full);
        connection.set_graphics_decoder(decoder.clone());
        connection.set_graphics_local_files(true);
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;

        let line = timeout(Duration::from_secs(5), async {
            loop {
                let lines = match runtime_rx.recv().await.expect("the session is live") {
                    RuntimeAction::HandleIncomingLine(line) => vec![line],
                    RuntimeAction::HandleIncomingLines(lines) => lines,
                    _ => continue,
                };
                if let Some(line) = lines.into_iter().find(|line| line.objects.is_some()) {
                    return line;
                }
            }
        })
        .await
        .expect("the line with the placement reaches the runtime");
        assert_eq!(line.text, "ab cd");
        let placement = line.objects.as_ref().unwrap()[0]
            .resource
            .clone()
            .downcast::<Placement>()
            .expect("the resource is the placement");
        assert_eq!(placement.image.id, 9);

        let (request, done) = decoder.next().await;
        assert_eq!(request.data, pixels, "the file's bytes reach the decoder");
        assert_eq!(
            (
                request.format,
                request.compression,
                request.width,
                request.height
            ),
            (Format::Rgb, Compression::None, 2, 1)
        );
        done(decode_packed(&request));

        let received = server.await.expect("server task");
        assert_eq!(
            received, b"\x1b_Gi=9;OK\x1b\\\x1b[?62;c",
            "the transmit's OK precedes DA1's answer"
        );
        let ready = placement.pixels().expect("the record is ready");
        assert_eq!(
            &ready.rgba[..],
            [0x10, 0x20, 0x30, 0xff, 0x40, 0x50, 0x60, 0xff]
        );
        assert!(file.path().exists(), "t=f leaves the file alone");
    }

    /// Without the server's opt-in a loopback peer is refused local media
    /// before the filesystem is looked at: the temporary file a `t=t` would
    /// delete is still there, and nothing is decoded.
    #[tokio::test]
    async fn a_loopback_peer_without_the_opt_in_is_refused_local_media() {
        use smudgy_protocol::graphics::encode_payload;
        let file = tempfile::Builder::new()
            .prefix("tty-graphics-protocol-")
            .suffix(".rgb")
            .tempfile()
            .expect("temp file");
        std::fs::write(file.path(), [1, 2, 3, 4, 5, 6]).expect("write the pixels");
        let commands = format!(
            "\x1b_Ga=T,t=t,f=24,s=2,v=1,i=11;{}\x1b\\\x1b[c",
            encode_payload(file.path().to_str().expect("a UTF-8 path").as_bytes())
        )
        .into_bytes();

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(graphics_exchange(listener, commands));
        let decoder = Arc::new(GateDecoder::default());
        let (mut connection, mut runtime_rx) = test_connection();
        connection.set_graphics_mode(smudgy_protocol::graphics::GraphicsMode::Full);
        connection.set_graphics_decoder(decoder.clone());
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;
        let received = server.await.expect("server task");
        assert_eq!(
            received, b"\x1b_Gi=11;EBADF:Failed to read image file\x1b\\\x1b[?62;c",
            "refused without the opt-in, then DA1"
        );
        assert!(file.path().exists(), "the file was never touched");
        assert!(
            decoder.held.lock().unwrap().is_empty(),
            "nothing was decoded"
        );
    }

    /// A local-media query for a file that is not there answers the one
    /// `EBADF` every local-media failure gets.
    #[tokio::test]
    async fn a_local_media_query_for_a_missing_file_answers_ebadf() {
        use smudgy_protocol::graphics::encode_payload;
        let dir = tempfile::tempdir().expect("temp dir");
        let missing = dir.path().join("tty-graphics-protocol-missing.rgb");
        let commands = format!(
            "\x1b_Ga=q,t=f,f=24,s=2,v=1,i=10;{}\x1b\\\x1b[c",
            encode_payload(missing.to_str().expect("a UTF-8 path").as_bytes())
        )
        .into_bytes();

        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("listener address").port();
        let server = tokio::spawn(graphics_exchange(listener, commands));
        let decoder = Arc::new(GateDecoder::default());
        let (mut connection, mut runtime_rx) = test_connection();
        connection.set_graphics_mode(smudgy_protocol::graphics::GraphicsMode::Full);
        connection.set_graphics_decoder(decoder.clone());
        connection.set_graphics_local_files(true);
        connection.connect(
            "127.0.0.1",
            port,
            None,
            None,
            InboundCompression::ALL,
            TlsMode::Off,
        );
        expect_connected(&mut runtime_rx).await;
        let received = server.await.expect("server task");
        assert_eq!(
            received, b"\x1b_Gi=10;EBADF:Failed to read image file\x1b\\\x1b[?62;c",
            "the failed read answers EBADF, then DA1"
        );
        assert!(
            decoder.held.lock().unwrap().is_empty(),
            "nothing was decoded"
        );
    }
}
