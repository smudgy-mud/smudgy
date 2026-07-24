use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use futures::channel::mpsc;
use tokio::{
    io::{self, AsyncRead, AsyncReadExt, AsyncWriteExt, Interest},
    net::TcpStream,
    select,
    sync::{
        mpsc::{UnboundedSender, WeakUnboundedSender},
        oneshot,
    },
};
use vt_processor::VtProcessor;
use vtparse::VTParser;

use super::{TaggedSessionEvent, runtime::RuntimeAction};

pub mod gmcp;
pub mod inflow;
pub mod msdp;
pub mod responders;
pub mod telnet;
pub mod transcode;
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
    use super::responders::{self, ProtocolState};
    use super::transcode::Transcode;
    use super::vt_processor::VtProcessor;
    use super::{gmcp, msdp, telnet};

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

    /// Feed one run of UTF-8 application bytes through the VT parser and the raw-capture
    /// path. Free-standing so `on_data` can call it with a slice borrowed from the
    /// transcode buffer (a disjoint field) without aliasing `&mut self` — and so the
    /// connect loop's teardown can feed the decoder's end-of-stream flush.
    pub fn feed_utf8(vt_parser: &mut VTParser, vt_processor: &mut VtProcessor, data: &[u8]) {
        // The capture decision is hoisted out of the byte loop. The trigger
        // manager flips the underlying flag from the session thread while
        // this runs on the socket runtime, but `capture_raw` can only FALL
        // mid-run (rises latch at batch boundaries, between runs), and a
        // fall is safe under either branch: the per-byte push re-checks it,
        // and a fallen line commits with its raw form absent, never torn.
        if vt_processor.capture_raw() {
            for &b in data {
                // CR/LF drive line breaks in the VT parser but are kept out
                // of `StyledLine::raw`.
                if b != b'\n' && b != b'\r' {
                    vt_processor.push_raw_incoming_byte(b);
                }
                vt_parser.parse_byte(b, &mut *vt_processor);
            }
        } else {
            for &b in data {
                vt_parser.parse_byte(b, &mut *vt_processor);
            }
        }
    }

    impl telnet::TelnetSink for TelnetBridge<'_> {
        fn on_data(&mut self, data: &[u8]) {
            if self.transcode.is_passthrough() {
                // UTF-8 (the overwhelming default): bytes flow exactly as ever.
                feed_utf8(self.vt_parser, self.vt_processor, data);
            } else {
                // Decode to UTF-8 first; `StyledLine::raw` and raw-pattern triggers
                // therefore see decoded text, not wire bytes.
                let utf8 = self.transcode.decode(data);
                feed_utf8(self.vt_parser, self.vt_processor, utf8.as_bytes());
            }
        }

        fn on_prompt(&mut self) {
            self.vt_processor.commit_prompt();
        }

        fn on_send(&mut self, bytes: &[u8]) {
            self.replies.extend_from_slice(bytes);
        }

        fn on_subnegotiation(&mut self, option: u8, payload: &[u8]) {
            if option == telnet::option::TTYPE {
                // RFC 1091 / MTTS: each `SEND` gets the next `IS` response in the cycle.
                // The reply rides the same inline buffer negotiation answers use.
                if payload == [responders::ttype::SEND] {
                    self.protocol.on_ttype_send(self.replies);
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
                // RFC 2066: answer the server's REQUEST — `answer_request` always frames a
                // reply (ACCEPTED or the mandatory REJECTED) — and switch the transcoder at
                // this exact stream position: the server changes encodings only after
                // seeing our ACCEPTED, so in-order delivery makes the switch race-free.
                // ACCEPTED/REJECTED we never solicited (we don't initiate) fall through
                // ignored.
                if let Some((&responders::charset::REQUEST, offer)) = payload.split_first() {
                    let accepted = responders::charset::answer_request(offer, self.replies);
                    if let Some(encoding) = accepted {
                        self.transcode.switch_to(encoding);
                    }
                }
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
            if matches!(side, telnet::Side::Local) {
                match option {
                    // Report the current window size the moment NAWS turns on (RFC 1073
                    // requires the first report immediately after the WILL); later size
                    // changes ride the `OutboundFrame::WindowSize` wakeup path.
                    telnet::option::NAWS if enabled => self.protocol.send_naws(self.replies),
                    // A disable restarts the TTYPE cycle so a renegotiation re-reports
                    // from the client name (the MTTS convention).
                    telnet::option::TTYPE if !enabled => self.protocol.reset_ttype(),
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
        telnet.receive(data, &mut bridge)
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
        }
    }

    /// Write all of `bytes` to the stream and flush. The flush matters for TLS: rustls
    /// buffers plaintext into the session record, so without it a lone interactive command
    /// would sit unsent until the next write. On plain TCP `flush` is a no-op (`nodelay` is
    /// already set), so flushing uniformly costs nothing there.
    async fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Plain(stream) => {
                stream.write_all(bytes).await?;
                stream.flush().await
            }
            Self::Tls(stream) => {
                stream.write_all(bytes).await?;
                stream.flush().await
            }
        }
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

/// Connect the transport: TCP, then a TLS handshake if requested. `host` is the server name
/// for certificate verification and SNI (a DNS name or IP literal).
async fn connect_stream(addr: &str, host: &str, tls: TlsMode) -> io::Result<GameStream> {
    // rustls 0.23 needs a process-global CryptoProvider. `smudgy_script` installs the
    // aws_lc_rs provider, but a session may connect before any script runs (and headless
    // tests have no script runtime), so install it here too — idempotent.
    static PROVIDER: OnceLock<()> = OnceLock::new();

    let stream = TcpStream::connect(addr).await?;
    stream.set_nodelay(true)?;
    if tls == TlsMode::Off {
        return Ok(GameStream::Plain(stream));
    }

    PROVIDER.get_or_init(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });

    let config = tls_client_config(tls == TlsMode::NoVerify)?;
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let server_name = rustls::pki_types::ServerName::try_from(host.to_string())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    let tls_stream = connector.connect(server_name, stream).await?;
    Ok(GameStream::Tls(Box::new(tls_stream)))
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

/// One queued socket write. Text is the ordinary command path (UTF-8, written verbatim);
/// Raw is the binary path for telnet-framed protocol messages (GMCP sends, future
/// subnegotiation responders — `docs/gmcp.md` §6.3). One channel carries both, so a
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

pub struct Connection {
    disconnect: Option<oneshot::Sender<()>>,
    runtime_tx: UnboundedSender<RuntimeAction>,
    ui_tx: mpsc::Sender<TaggedSessionEvent>,
    socket_tx: Arc<RwLock<Option<WeakUnboundedSender<OutboundFrame>>>>,
    on_connect: Option<Box<dyn FnOnce() + Send>>,
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
}

fn clear_socket_sender(socket_tx: &RwLock<Option<WeakUnboundedSender<OutboundFrame>>>) {
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
            .field("on_connect", &self.on_connect.is_some())
            .field("raw_wanted", &self.raw_wanted)
            .field("window_size", &self.window_size)
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
    ) -> Self {
        Self {
            disconnect: None,
            runtime_tx,
            ui_tx,
            socket_tx: Arc::new(RwLock::new(None)),
            on_connect: None,
            raw_wanted,
            window_size,
        }
    }

    /// Wake the live socket task after the shared size cell changed; the task re-reads the
    /// cell and sends a NAWS update iff the option is negotiated and the size actually
    /// changed. A no-op when disconnected — the cell already carries the value the next
    /// connect task reads.
    pub fn notify_window_size(&self) {
        // Ignore the error: no live socket means nothing to notify.
        let _ = self.write_frame(OutboundFrame::WindowSize);
    }

    /// Send raw data to the connected socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket lock is poisoned, no socket is currently
    /// registered, the sender can no longer be upgraded, or its queue is closed.
    pub fn write(&self, data: Arc<String>) -> Result<(), anyhow::Error> {
        self.write_frame(OutboundFrame::Text(data))
    }

    /// Send a pre-framed binary message (a telnet subnegotiation such as a GMCP send) to
    /// the connected socket. Same queue as [`Self::write`], so ordering with normal sends
    /// holds by construction.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket lock is poisoned, no socket is currently
    /// registered, the sender can no longer be upgraded, or its queue is closed.
    pub fn write_raw(&self, frame: Arc<[u8]>) -> Result<(), anyhow::Error> {
        self.write_frame(OutboundFrame::Raw(frame))
    }

    fn write_frame(&self, frame: OutboundFrame) -> Result<(), anyhow::Error> {
        let socket_tx = self
            .socket_tx
            .read()
            .map_err(|_| anyhow::anyhow!("Socket tx lock is poisoned"))?;
        let socket_tx = socket_tx
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Socket no longer exists"))?;
        let socket_tx = socket_tx
            .upgrade()
            .ok_or_else(|| anyhow::anyhow!("Socket tx is not upgradeable"))?;
        // The receiver can drop between the upgrade and the send (the connect task
        // tearing down) — an ordinary disconnected-socket error, not a panic condition.
        socket_tx
            .send(frame)
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
        compression: bool,
        tls: TlsMode,
    ) {
        let addr = format!("{host}:{port}");
        let host = host.to_string();
        let runtime_tx = self.runtime_tx.clone();
        let (tx, mut disconnect_rx) = oneshot::channel();

        if let Some(disconnect) = self.disconnect.take() {
            // This will error if the channel is already closed, which is fine
            disconnect.send(()).ok();
        }

        self.disconnect = Some(tx);

        self.socket_tx = Arc::new(RwLock::new(None));
        let socket_tx = self.socket_tx.clone();

        let on_connect = self.on_connect.take();
        let raw_wanted = self.raw_wanted.clone();
        let window_size = self.window_size.clone();

        spawn_io_task(async move {
            let mut vt_parser = VTParser::new();
            let mut vt_processor = VtProcessor::new(runtime_tx.clone());
            vt_processor.set_raw_wanted_flag(raw_wanted);
            // Telnet/IAC preprocessor: consumes negotiation + prompt markers so the VT parser only
            // ever sees pure game text. Persists across reads (a sequence may straddle a read).
            let mut telnet = telnet::TelnetParser::new();
            telnet.set_accept_compression(compression);
            // The MCCP stage ahead of the parser, plus its reused chunk buffer. (The
            // decompressed-bytes pacing counter is per-wake, declared in the read arm.)
            let mut inflow = inflow::Inflow::Plain;
            let mut inflate_buf: Vec<u8> = Vec::new();
            // Subnegotiation responder state (TTYPE cycle, NAWS reporting). Reads the
            // shared size cell at report time, so the first NAWS answer already carries
            // the size the UI last reported; `secure` sets the MTTS SSL bit.
            let mut protocol =
                responders::ProtocolState::new(window_size, tls != TlsMode::Off);
            // Charset transcoding: the per-server setting seeds it (None = UTF-8, a pure
            // pass-through); a CHARSET negotiation switches it mid-stream.
            let mut transcode =
                transcode::Transcode::new(encoding.unwrap_or(encoding_rs::UTF_8));
            // Negotiation replies to write back to the server, reused across reads.
            let mut telnet_replies: Vec<u8> = Vec::new();
            let (write_to_socket_tx, mut write_to_socket_rx) =
                tokio::sync::mpsc::unbounded_channel::<OutboundFrame>();

            runtime_tx
                .send(RuntimeAction::Echo(Arc::new(format!(
                    "Connecting to {addr}..."
                ))))
                .ok();
            info!("Connecting to {addr}...");

            let connect_result = match tokio::time::timeout(
                CONNECT_TIMEOUT,
                connect_stream(&addr, &host, tls),
            )
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
                    runtime_tx
                        .send(RuntimeAction::Echo(Arc::new("Connected.".to_string())))
                        .ok();
                    info!("Connected");

                    if let Some(on_connect) = on_connect {
                        on_connect();
                    }

                    {
                        let Ok(mut sender) = socket_tx.write() else {
                            warn!("Failed to register socket sender because its lock is poisoned");
                            return;
                        };
                        sender.replace(write_to_socket_tx.downgrade());
                    }

                    runtime_tx.send(RuntimeAction::Connected).ok();

                    // Raw wire log: exact bytes as received, including ANSI
                    // escape sequences and CR/LF. One file per connection;
                    // failure to create it is non-fatal.
                    let mut raw_log = raw_log_path.and_then(|path| match File::create(&path) {
                        Ok(file) => Some(BufWriter::with_capacity(65536, file)),
                        Err(err) => {
                            warn!("Failed to create raw log {}: {err:?}", path.display());
                            None
                        }
                    });

                    // Set when the loop exits because a disconnect was requested
                    // (user-initiated, or this connection superseded by a new
                    // one) rather than because the socket dropped underneath us.
                    let mut graceful = false;

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
                                                let mut slice: &[u8] = &data;
                                                'route: while !slice.is_empty() {
                                                    if inflow.is_plain() {
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
                                                            && let Err(err) =
                                                                stream.write_all(&telnet_replies).await
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
                                                            match codec_for(start).map(|c| inflow.begin(c)) {
                                                                Some(Ok(())) => {}
                                                                _ => {
                                                                    warn!("MCCPX: unusable compression start; disconnecting");
                                                                    runtime_tx
                                                                        .send(RuntimeAction::Echo(Arc::new(
                                                                            "Compression error — disconnecting.".to_string(),
                                                                        )))
                                                                        .ok();
                                                                    dead = true;
                                                                    break 'route;
                                                                }
                                                            }
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
                                                                                .write_all(&telnet_replies)
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
                                                                    // Orderly stream end: back to
                                                                    // plain telnet. Clear both
                                                                    // compression options' negotiated
                                                                    // state (only one was on; the
                                                                    // other clear is a no-op) so a
                                                                    // later WILL renegotiates cleanly
                                                                    // and releases the one-wrapper
                                                                    // claim. The tail (`slice`) is
                                                                    // plain again — the outer `'route`
                                                                    // loop routes it.
                                                                    inflow.end();
                                                                    telnet.clear_remote(telnet::option::MCCP2);
                                                                    telnet.clear_remote(telnet::option::MCCPX);
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
                                                                let dont = [
                                                                    telnet::command::IAC,
                                                                    telnet::command::DONT,
                                                                    compression_option,
                                                                ];
                                                                stream.write_all(&dont).await.ok();
                                                                runtime_tx
                                                                    .send(RuntimeAction::Echo(Arc::new(
                                                                        "Compression error — disconnecting.".to_string(),
                                                                    )))
                                                                    .ok();
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
                                    if fed {
                                        tokio::task::yield_now().await;
                                    }
                            }
                            Some(frame) = write_to_socket_rx.recv() => {
                                let outcome = match &frame {
                                    OutboundFrame::Text(text) => {
                                        if transcode.is_passthrough() {
                                            stream.write_all(text.as_bytes()).await
                                        } else {
                                            // Encode to the active charset and double any
                                            // 0xFF the encoding produced (UTF-8 output can
                                            // never contain one; legacy encodings can).
                                            stream
                                                .write_all(transcode.encode_outbound(text))
                                                .await
                                        }
                                    }
                                    OutboundFrame::Raw(bytes) => stream.write_all(bytes).await,
                                    OutboundFrame::WindowSize => {
                                        // Emit a NAWS update only when the option is
                                        // negotiated (an unsolicited report is a protocol
                                        // violation) and the size cell actually changed
                                        // since the last report (a repeat is noise).
                                        telnet_replies.clear();
                                        if telnet.local_enabled(telnet::option::NAWS)
                                            && protocol.send_naws_if_changed(&mut telnet_replies)
                                        {
                                            stream.write_all(&telnet_replies).await
                                        } else {
                                            Ok(())
                                        }
                                    }
                                };
                                if let Err(err) = outcome {
                                    warn!("Socket write to {addr} failed: {err}");
                                    break;
                                }
                            }
                            _ = &mut disconnect_rx => {
                                graceful = true;
                                break;
                            }
                            else => {
                                break;
                            }
                        }
                    }

                    // End-of-stream decoder flush: a socket that closed mid-character
                    // (a converting connection's pending multibyte lead byte) surfaces
                    // as U+FFFD on the final line instead of vanishing.
                    let tail = transcode.finish();
                    if !tail.is_empty() {
                        feed_utf8(&mut vt_parser, &mut vt_processor, tail.as_bytes());
                        vt_processor.notify_end_of_buffer();
                    }

                    if let Some(mut writer) = raw_log.take()
                        && let Err(err) = writer.flush()
                    {
                        warn!("Failed to flush raw log: {err:?}");
                    }

                    // Silently ignore errors here; when a session is closing the runtime may already be gone by the time
                    // we get here
                    runtime_tx
                        .send(RuntimeAction::Disconnected)
                        .map(|()| {
                            // A requested disconnect reads as a clean "Disconnected.";
                            // an unexpected socket drop reads as "Connection lost".
                            let notice = if graceful {
                                "Disconnected."
                            } else {
                                "Connection lost"
                            };
                            runtime_tx
                                .send(RuntimeAction::Echo(Arc::new(notice.to_string())))
                                .ok();
                        })
                        .ok();
                }
                Err(err) => {
                    warn!("Connection to {addr} failed: {err}");
                    // Name the reason in the session view — a TLS handshake failure
                    // (self-signed / expired / name mismatch) gives no other hint, and the
                    // policy is never a silent fallback to plaintext.
                    runtime_tx
                        .send(RuntimeAction::Echo(Arc::new(format!(
                            "Connection failed: {err}"
                        ))))
                        .map_err(|_| {
                            warn!("Error notifying runtime of connection failure; ignoring");
                        })
                        .ok();
                }
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

    pub fn on_connect(&mut self, on_connect: impl FnOnce() + Send + 'static) {
        self.on_connect = Some(Box::new(on_connect));
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, AtomicU32};
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::sync::mpsc as tokio_mpsc;
    use tokio::time::timeout;

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
            ),
            runtime_rx,
        )
    }

    /// Run one inbound buffer through the real ingest bridge (telnet
    /// preprocessor + VT parser + reply buffer), returning the negotiation
    /// replies and every action the bridge queued.
    fn ingest_buffer(input: &[u8]) -> (Vec<u8>, Vec<RuntimeAction>) {
        let mut protocol = responders::ProtocolState::with_fixed_dims(responders::DEFAULT_DIMS);
        let mut transcode = transcode::Transcode::new(encoding_rs::UTF_8);
        ingest_buffer_with(input, &mut protocol, &mut transcode)
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

    #[test]
    fn write_returns_an_error_when_the_socket_queue_is_closed() {
        let (mut connection, _runtime_rx) = test_connection();
        let (socket_tx, socket_rx) = tokio_mpsc::unbounded_channel();
        let weak_socket_tx = socket_tx.downgrade();
        drop(socket_rx);
        connection.socket_tx = Arc::new(RwLock::new(Some(weak_socket_tx)));

        let error = connection
            .write(Arc::new("look".to_string()))
            .expect_err("a closed socket queue must be reported instead of panicking");

        assert!(error.to_string().contains("closed"));
        drop(socket_tx);
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
        connection.connect("127.0.0.1", port, None, None, true, TlsMode::Off);

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
                    RuntimeAction::Disconnected => disconnected = true,
                    RuntimeAction::Echo(text) if text.as_str() == "Connection lost" => {
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
        connection.connect("127.0.0.1", port, None, None, true, TlsMode::Off);

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
                    RuntimeAction::Disconnected => disconnected = true,
                    RuntimeAction::Echo(text) if text.as_str() == "Disconnected." => {
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
}
