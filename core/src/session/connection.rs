use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use futures::channel::mpsc;
use tokio::{
    io::{self, AsyncWriteExt, Interest},
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
    use super::{gmcp, telnet};

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
            for &b in data {
                // CR/LF drive line breaks in the VT parser but are kept out of `StyledLine::raw`.
                if b != b'\n' && b != b'\r' {
                    self.vt_processor.push_raw_incoming_byte(b);
                }
                self.vt_parser.parse_byte(b, &mut *self.vt_processor);
            }
        }

        fn on_prompt(&mut self) {
            self.vt_processor.commit_prompt();
        }

        fn on_send(&mut self, bytes: &[u8]) {
            self.replies.extend_from_slice(bytes);
        }

        fn on_subnegotiation(&mut self, option: u8, payload: &[u8]) {
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
            if option != telnet::option::GMCP || !matches!(side, telnet::Side::Remote) {
                return;
            }
            if enabled {
                // Handshake immediately, in the same write the DO reply rides.
                gmcp::frame_handshake(self.replies);
                self.runtime_tx.send(RuntimeAction::GmcpEnabled).ok();
            } else {
                self.runtime_tx.send(RuntimeAction::GmcpDisabled).ok();
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
    socket_tx: Arc<RwLock<Option<WeakUnboundedSender<OutboundFrame>>>>,
    on_connect: Option<Box<dyn FnOnce() + Send>>,
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("disconnect", &self.disconnect)
            .field("runtime_tx", &self.runtime_tx)
            .field("ui_tx", &self.ui_tx)
            .field("socket_tx", &self.socket_tx)
            .field("on_connect", &self.on_connect.is_some())
            .finish()
    }
}

impl Connection {
    #[must_use]
    pub fn new(
        runtime_tx: UnboundedSender<RuntimeAction>,
        ui_tx: futures::channel::mpsc::Sender<TaggedSessionEvent>,
    ) -> Self {
        Self {
            disconnect: None,
            runtime_tx,
            ui_tx,
            socket_tx: Arc::new(RwLock::new(None)),
            on_connect: None,
        }
    }

    /// Send raw data to the connected socket.
    ///
    /// # Errors
    ///
    /// Returns an error if no socket is currently registered, or if the socket
    /// sender can no longer be upgraded (the connection task has gone away).
    ///
    /// # Panics
    ///
    /// Panics if the `socket_tx` lock is poisoned, or if sending on the socket
    /// channel fails (the receiver was dropped).
    pub fn write(&self, data: Arc<String>) -> Result<(), anyhow::Error> {
        self.write_frame(OutboundFrame::Text(data))
    }

    /// Send a pre-framed binary message (a telnet subnegotiation such as a GMCP send) to
    /// the connected socket. Same queue as [`Self::write`], so ordering with normal sends
    /// holds by construction.
    ///
    /// # Errors
    ///
    /// Returns an error if no socket is currently registered, or if the socket
    /// sender can no longer be upgraded (the connection task has gone away).
    ///
    /// # Panics
    ///
    /// Panics if the `socket_tx` lock is poisoned, or if sending on the socket
    /// channel fails (the receiver was dropped).
    pub fn write_raw(&self, frame: Arc<[u8]>) -> Result<(), anyhow::Error> {
        self.write_frame(OutboundFrame::Raw(frame))
    }

    fn write_frame(&self, frame: OutboundFrame) -> Result<(), anyhow::Error> {
        let socket_tx = self.socket_tx.read().unwrap();
        if let Some(socket_tx) = socket_tx.as_ref() {
            if let Some(socket_tx) = socket_tx.upgrade() {
                socket_tx.send(frame).unwrap();
                Ok(())
            } else {
                Err(anyhow::anyhow!("Socket tx is not upgradeable"))
            }
        } else {
            Err(anyhow::anyhow!("Socket no longer exists"))
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
    /// This function can panic under the following conditions:
    /// - If sending initial messages (like "Connecting to...") to the session runtime fails (channel closed).
    /// - If `stream.set_nodelay(true)` fails on the newly connected TCP stream.
    /// - If sending the `UpdateWriteToSocketTx` action to the session runtime fails (channel closed).
    /// - If writing the `send_on_connect` data to the TCP stream fails.
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

        tokio::spawn(async move {
            let mut vt_parser = VTParser::new();
            let mut vt_processor = VtProcessor::new(runtime_tx.clone());
            // Telnet/IAC preprocessor: consumes negotiation + prompt markers so the VT parser only
            // ever sees pure game text. Persists across reads (a sequence may straddle a read).
            let mut telnet = telnet::TelnetParser::new();
            // Negotiation replies to write back to the server, reused across reads.
            let mut telnet_replies: Vec<u8> = Vec::new();
            let (write_to_socket_tx, mut write_to_socket_rx) =
                tokio::sync::mpsc::unbounded_channel::<OutboundFrame>();

            runtime_tx
                .send(RuntimeAction::Echo(Arc::new(format!(
                    "Connecting to {addr}..."
                ))))
                .unwrap();
            info!("Connecting to {addr}...");

            match TcpStream::connect(addr).await {
                Ok(mut stream) => {
                    runtime_tx
                        .send(RuntimeAction::Echo(Arc::new("Connected.".to_string())))
                        .unwrap();
                    stream.set_nodelay(true).unwrap();
                    info!("Connected");

                    if let Some(on_connect) = on_connect {
                        on_connect();
                    }

                    socket_tx
                        .write()
                        .unwrap()
                        .replace(write_to_socket_tx.downgrade());

                    runtime_tx.send(RuntimeAction::Connected).unwrap();

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

                    loop {
                        select! {
                            Ok(ready) = stream.ready(Interest::READABLE) => {
                                if ready.is_readable() {
                                    let mut data: Vec<u8> = Vec::with_capacity(65536);

                                    match stream.try_read_buf(&mut data) {
                                        Ok(n) => {
                                            if n == 0 {
                                                break;
                                            }

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
                                            // Flush any negotiation replies the parser produced.
                                            if !telnet_replies.is_empty()
                                                && stream.write_all(&telnet_replies).await.is_err()
                                            {
                                                break;
                                            }

                                            vt_processor.notify_end_of_buffer();
                                        }
                                        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                                        }
                                        Err(_) => {
                                            // TODO: notify session that the try_read_buf errored
                                            // return Err::<(), anyhow::Error>(e.into());
                                            break;
                                        }
                                    }
                                }
                            }
                            Some(ref frame) = write_to_socket_rx.recv() => {
                                if stream.write_all(frame.bytes()).await.is_err() {
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
                _ => {
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
            socket_tx.write().unwrap().take();
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
