use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use futures::channel::mpsc;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    select,
    sync::{
        mpsc::{UnboundedSender, WeakSender, error::TrySendError},
        oneshot,
    },
};
use vt_processor::VtProcessor;
use vtparse::VTParser;

use super::{TaggedSessionEvent, runtime::RuntimeAction};

pub mod gmcp;
pub mod msdp;
pub mod telnet;
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
    use super::vt_processor::VtProcessor;
    use super::{gmcp, msdp, telnet};

    /// Bridges the telnet preprocessor to the rest of the inbound pipeline for one socket read.
    ///
    /// Pure application bytes are fed through the VT parser and accumulated into `StyledLine::raw`; a
    /// prompt boundary flushes the pending line via [`VtProcessor::commit_prompt`]; negotiation replies
    /// are buffered into `replies` to be written back to the socket after the read. GMCP
    /// subnegotiations forward as [`RuntimeAction::GmcpMessage`] on the same channel the line
    /// actions ride — the exact stream position is the ordering guarantee
    /// (`docs/gmcp-plan.md` §3.3) — and GMCP option changes drive the handshake +
    /// [`RuntimeAction::GmcpEnabled`]/[`RuntimeAction::GmcpDisabled`]. Other subnegotiations
    /// and option changes still fall through the default no-op hooks (the MSDP/… springboard —
    /// see `docs/telnet-preprocessor-plan.md`).
    pub struct TelnetBridge<'a> {
        pub vt_parser: &'a mut VTParser,
        pub vt_processor: &'a mut VtProcessor,
        /// Reused across reads; the caller clears it before each `receive` and drains it after.
        pub replies: &'a mut Vec<u8>,
        /// The session action channel — the same one [`VtProcessor`] emits line actions on,
        /// so GMCP messages interleave with lines in wire order.
        pub runtime_tx: &'a UnboundedSender<RuntimeAction>,
    }

    impl telnet::TelnetSink for TelnetBridge<'_> {
        fn on_data(&mut self, data: &[u8]) {
            // The capture decision is hoisted out of the byte loop: the flag
            // only changes from the session thread (the same thread this runs
            // on), and a line commit inside the run can only re-latch the same
            // value, so it cannot flip mid-run.
            if self.vt_processor.capture_raw() {
                for &b in data {
                    // CR/LF drive line breaks in the VT parser but are kept out
                    // of `StyledLine::raw`.
                    if b != b'\n' && b != b'\r' {
                        self.vt_processor.push_raw_incoming_byte(b);
                    }
                    self.vt_parser.parse_byte(b, &mut *self.vt_processor);
                }
            } else {
                for &b in data {
                    self.vt_parser.parse_byte(b, &mut *self.vt_processor);
                }
            }
        }

        fn on_prompt(&mut self) {
            self.vt_processor.commit_prompt();
        }

        fn on_send(&mut self, bytes: &[u8]) {
            self.replies.extend_from_slice(bytes);
        }

        fn on_subnegotiation(&mut self, option: u8, payload: &[u8]) {
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
            if !matches!(side, telnet::Side::Remote) {
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
                _ => {}
            }
        }
    }

    /// Run one received buffer through the telnet preprocessor and VT parser, accumulating any
    /// negotiation replies into `replies` (cleared first). The caller writes `replies` back to the
    /// socket and calls [`VtProcessor::notify_end_of_buffer`] afterward.
    pub fn feed_inbound(
        data: &[u8],
        telnet: &mut telnet::TelnetParser,
        vt_parser: &mut VTParser,
        vt_processor: &mut VtProcessor,
        replies: &mut Vec<u8>,
        runtime_tx: &UnboundedSender<RuntimeAction>,
    ) {
        replies.clear();
        let mut bridge = TelnetBridge {
            vt_parser,
            vt_processor,
            replies,
            runtime_tx,
        };
        telnet.receive(data, &mut bridge);
    }
}

#[cfg(not(feature = "bench-api"))]
use ingest::feed_inbound;
// Expose the inbound ingest glue to the `smudgy_bench` crate without widening the normal
// public API (the same pattern as the trigger-engine re-export in `runtime.rs`): the module
// stays private; the items become reachable only under the feature.
#[cfg(feature = "bench-api")]
pub use ingest::{TelnetBridge, feed_inbound};

/// One queued socket write. Text is the ordinary command path (UTF-8, written verbatim);
/// Raw is the binary path for telnet-framed protocol messages (GMCP sends, future
/// subnegotiation responders — `docs/gmcp-plan.md` §6.3). One channel carries both, so a
/// protocol frame and a user command queued in either order reach the wire in that order.
#[derive(Clone, Debug)]
pub enum OutboundFrame {
    Text(Arc<String>),
    Raw(Arc<[u8]>),
}

/// Maximum number of application/protocol frames waiting for the socket task.
/// A bounded queue keeps a stalled server or runaway script from retaining an
/// unbounded amount of memory. Telnet negotiation replies are written inline
/// and do not consume these slots.
const OUTBOUND_QUEUE_CAPACITY: usize = 256;

impl OutboundFrame {
    fn bytes(&self) -> &[u8] {
        match self {
            Self::Text(text) => text.as_bytes(),
            Self::Raw(bytes) => bytes,
        }
    }
}

pub struct Connection {
    disconnect: Option<oneshot::Sender<()>>,
    runtime_tx: UnboundedSender<RuntimeAction>,
    ui_tx: mpsc::Sender<TaggedSessionEvent>,
    socket_tx: Arc<RwLock<Option<WeakSender<OutboundFrame>>>>,
    on_connect: Option<Box<dyn FnOnce() + Send>>,
    /// The trigger manager's "any trigger has a raw pattern" flag; each connect
    /// task hands it to its [`VtProcessor`] so per-line raw capture only runs
    /// while something can match on it.
    raw_wanted: Arc<std::sync::atomic::AtomicBool>,
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
            .field("on_connect", &self.on_connect.is_some())
            .field("raw_wanted", &self.raw_wanted)
            .finish()
    }
}

impl Connection {
    #[must_use]
    pub fn new(
        runtime_tx: UnboundedSender<RuntimeAction>,
        ui_tx: futures::channel::mpsc::Sender<TaggedSessionEvent>,
        raw_wanted: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            disconnect: None,
            runtime_tx,
            ui_tx,
            socket_tx: Arc::new(RwLock::new(None)),
            on_connect: None,
            raw_wanted,
        }
    }

    /// Send raw data to the connected socket.
    ///
    /// # Errors
    ///
    /// Returns an error if the socket lock is poisoned, no socket is currently
    /// registered, the sender can no longer be upgraded, or its bounded queue
    /// is full or closed.
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
    /// registered, the sender can no longer be upgraded, or its bounded queue
    /// is full or closed.
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
        match socket_tx.try_send(frame) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(_)) => Err(anyhow::anyhow!(
                "Socket write queue is full ({OUTBOUND_QUEUE_CAPACITY} pending frames)"
            )),
            Err(TrySendError::Closed(_)) => Err(anyhow::anyhow!("Socket write queue is closed")),
        }
    }

    /// Establishes a TCP connection to the specified host and port.
    ///
    /// This function spawns a new Tokio task to handle the connection, including
    /// reading data from the socket, processing it with a VT parser, and sending
    /// outgoing data.
    ///
    /// If a previous connection managed by this `Connection` instance exists, it will
    /// be signaled to disconnect.
    ///
    /// When `raw_log_path` is `Some`, the exact bytes received from the server
    /// (including ANSI escape sequences and CR/LF) are appended to that file
    /// for the lifetime of the connection. Failure to create or write the file
    /// is logged and otherwise ignored.
    ///
    /// # Panics
    ///
    /// Panics if called outside a Tokio runtime, or if the callback registered
    /// with [`Self::on_connect`] panics.
    pub fn connect(&mut self, host: &str, port: u16, raw_log_path: Option<PathBuf>) {
        let addr = format!("{host}:{port}");
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

        tokio::spawn(async move {
            let mut vt_parser = VTParser::new();
            let mut vt_processor = VtProcessor::new(runtime_tx.clone());
            vt_processor.set_raw_wanted_flag(raw_wanted);
            // Telnet/IAC preprocessor: consumes negotiation + prompt markers so the VT parser only
            // ever sees pure game text. Persists across reads (a sequence may straddle a read).
            let mut telnet = telnet::TelnetParser::new();
            // Negotiation replies to write back to the server, reused across reads.
            let mut telnet_replies: Vec<u8> = Vec::new();
            let (write_to_socket_tx, mut write_to_socket_rx) =
                tokio::sync::mpsc::channel::<OutboundFrame>(OUTBOUND_QUEUE_CAPACITY);

            if runtime_tx
                .send(RuntimeAction::Echo(Arc::new(format!(
                    "Connecting to {addr}..."
                ))))
                .is_err()
            {
                warn!("Connection task stopped because the runtime channel is closed");
                return;
            }
            info!("Connecting to {addr}...");

            match TcpStream::connect(&addr).await {
                Ok(mut stream) => {
                    if runtime_tx
                        .send(RuntimeAction::Echo(Arc::new("Connected.".to_string())))
                        .is_err()
                    {
                        warn!("Connected to {addr}, but the runtime channel is closed");
                        return;
                    }
                    if let Err(err) = stream.set_nodelay(true) {
                        warn!("Failed to disable Nagle's algorithm for {addr}: {err}");
                    }
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

                    if runtime_tx.send(RuntimeAction::Connected).is_err() {
                        warn!("Connected to {addr}, but the runtime channel closed during setup");
                        clear_socket_sender(&socket_tx);
                        return;
                    }

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
                    // Reuse one receive buffer for the life of the connection. `read_buf` is
                    // cancellation-safe inside `select!`, so a disconnect cannot consume and
                    // discard bytes when its branch wins.
                    let mut data = Vec::with_capacity(65536);

                    loop {
                        select! {
                            read = stream.read_buf(&mut data) => {
                                match read {
                                    Ok(0) => break,
                                    Ok(_) => {
                                        if let Some(mut writer) = raw_log.take() {
                                            match writer.write_all(&data) {
                                                Ok(()) => raw_log = Some(writer),
                                                Err(err) => {
                                                    warn!("Raw log write failed; disabling the raw log: {err:?}");
                                                }
                                            }
                                        }

                                        feed_inbound(
                                            &data,
                                            &mut telnet,
                                            &mut vt_parser,
                                            &mut vt_processor,
                                            &mut telnet_replies,
                                            &runtime_tx,
                                        );
                                        data.clear();

                                        // Flush any negotiation replies the parser produced.
                                        if !telnet_replies.is_empty()
                                            && let Err(err) = stream.write_all(&telnet_replies).await
                                        {
                                            warn!("Failed to write telnet reply to {addr}: {err}");
                                            break;
                                        }

                                        vt_processor.notify_end_of_buffer();
                                    }
                                    Err(err) => {
                                        warn!("Socket read from {addr} failed: {err}");
                                        break;
                                    }
                                }
                            }
                            Some(ref frame) = write_to_socket_rx.recv() => {
                                if let Err(err) = stream.write_all(frame.bytes()).await {
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
                    runtime_tx
                        .send(RuntimeAction::Echo(Arc::new(
                            "Connection failed".to_string(),
                        )))
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
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio::sync::{mpsc as tokio_mpsc, oneshot};
    use tokio::time::timeout;

    use super::*;

    fn test_connection() -> (Connection, tokio_mpsc::UnboundedReceiver<RuntimeAction>) {
        let (runtime_tx, runtime_rx) = tokio_mpsc::unbounded_channel();
        let (ui_tx, _ui_rx) = mpsc::channel(1);
        (
            Connection::new(runtime_tx, ui_tx, Arc::new(AtomicBool::new(false))),
            runtime_rx,
        )
    }

    #[test]
    fn write_returns_an_error_when_the_socket_queue_is_closed() {
        let (mut connection, _runtime_rx) = test_connection();
        let (socket_tx, socket_rx) = tokio_mpsc::channel(1);
        let weak_socket_tx = socket_tx.downgrade();
        drop(socket_rx);
        connection.socket_tx = Arc::new(RwLock::new(Some(weak_socket_tx)));

        let error = connection
            .write(Arc::new("look".to_string()))
            .expect_err("a closed socket queue must be reported instead of panicking");

        assert!(error.to_string().contains("closed"));
        drop(socket_tx);
    }

    #[test]
    fn write_rejects_a_frame_when_the_socket_queue_is_full() {
        let (mut connection, _runtime_rx) = test_connection();
        let (socket_tx, _socket_rx) = tokio_mpsc::channel(OUTBOUND_QUEUE_CAPACITY);
        connection.socket_tx = Arc::new(RwLock::new(Some(socket_tx.downgrade())));

        for index in 0..OUTBOUND_QUEUE_CAPACITY {
            connection
                .write(Arc::new(format!("command-{index}")))
                .expect("the configured queue capacity should accept this frame");
        }

        let error = connection
            .write(Arc::new("one-too-many".to_string()))
            .expect_err("a full socket queue must reject the frame instead of growing");

        assert!(error.to_string().contains("full"));
        assert!(
            error
                .to_string()
                .contains(&OUTBOUND_QUEUE_CAPACITY.to_string())
        );
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
        connection.connect("127.0.0.1", port, None);

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
        connection.connect("127.0.0.1", port, None);

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
