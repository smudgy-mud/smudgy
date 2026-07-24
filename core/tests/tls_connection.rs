//! TLS game-connection coverage (`docs/telnet.md` Phase 5): a real
//! [`Connection`] handshaking against a local `tokio-rustls` server with a self-signed
//! certificate. Exercises the `GameStream::Tls` read/write path (including the no-op-waker
//! `try_fill` drain) and the two verification modes: `NoVerify` accepts the self-signed cert
//! and data flows; `Verify` rejects it (the policy never silently falls back to plaintext).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt;
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};

use smudgy_core::session::connection::{Connection, TlsMode, responders};
use smudgy_core::session::runtime::RuntimeAction;

/// A self-signed cert + key for `localhost`, and a rustls server config using them.
fn self_signed_server_config() -> tokio_rustls::rustls::ServerConfig {
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".to_string()])
        .expect("generate self-signed cert");
    let cert_der = CertificateDer::from(cert.cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(cert.key_pair.serialize_der()).expect("key der");
    tokio_rustls::rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .expect("server config")
}

/// Spawn a one-shot TLS server that accepts a single connection, completes the handshake,
/// and writes `greeting`. If `echo_reply` is set, it then reads at least one byte from the
/// client (exercising the client's outbound TLS write + flush) and writes that reply back.
/// Returns the bound port. Runs on the test's tokio runtime.
async fn spawn_tls_server(greeting: &'static [u8], echo_reply: Option<&'static [u8]>) -> u16 {
    use tokio::io::AsyncReadExt;
    // Ensure the process-global provider is installed for the server side too.
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
    let acceptor = TlsAcceptor::from(Arc::new(self_signed_server_config()));
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    tokio::spawn(async move {
        if let Ok((tcp, _)) = listener.accept().await
            && let Ok(mut tls) = acceptor.accept(tcp).await
        {
            let _ = tls.write_all(greeting).await;
            let _ = tls.flush().await;
            if let Some(reply) = echo_reply {
                // A single read that returns proves the client's command was encrypted AND
                // flushed to the socket (without the flush it would sit in the rustls buffer
                // and this read would block until timeout).
                let mut buf = [0_u8; 256];
                if tokio::time::timeout(Duration::from_secs(5), tls.read(&mut buf))
                    .await
                    .is_ok_and(|r| matches!(r, Ok(n) if n > 0))
                {
                    let _ = tls.write_all(reply).await;
                    let _ = tls.flush().await;
                }
            }
            // Hold the connection open briefly so the client reads before FIN.
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    });
    port
}

fn new_connection(runtime_tx: tokio::sync::mpsc::UnboundedSender<RuntimeAction>) -> Connection {
    let (ui_tx, _ui_rx) = futures::channel::mpsc::channel(64);
    Connection::new(
        runtime_tx,
        ui_tx,
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicU32::new(responders::pack_dims(80, 24))),
    )
}

/// Collect runtime actions for up to `timeout`, returning the emitted line texts and echoes.
fn drain(
    rx: &mut tokio::sync::mpsc::UnboundedReceiver<RuntimeAction>,
    timeout: Duration,
) -> (Vec<String>, Vec<String>) {
    let deadline = Instant::now() + timeout;
    let mut lines = Vec::new();
    let mut echoes = Vec::new();
    while Instant::now() < deadline {
        match rx.try_recv() {
            Ok(RuntimeAction::HandleIncomingLine(line)) => lines.push(line.text.clone()),
            Ok(RuntimeAction::HandleIncomingPartialLine(line)) => lines.push(line.text.clone()),
            Ok(RuntimeAction::Echo(text)) => echoes.push(text.to_string()),
            Ok(_) => {}
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    (lines, echoes)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_no_verify_accepts_self_signed_and_data_flows_both_ways() {
    let port = spawn_tls_server(
        b"\x1b[32mSecure hello over TLS\x1b[0m\r\n",
        Some(b"SERVER_GOT_COMMAND\r\n"),
    )
    .await;

    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut connection = new_connection(runtime_tx);
    connection.connect("localhost", port, None, None, true, TlsMode::NoVerify);

    // Give the handshake a moment, then send a command over TLS — retry until the socket
    // task has registered (write() errors until then). The reply proves the outbound write
    // reached the server (i.e. was flushed, not stuck in the rustls buffer).
    let deadline = Instant::now() + Duration::from_secs(5);
    while connection
        .write(Arc::new("look\n".to_string()))
        .await
        .is_err()
        && Instant::now() < deadline
    {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let (lines, echoes) = drain(&mut runtime_rx, Duration::from_secs(10));
    assert!(
        lines.iter().any(|l| l.contains("Secure hello over TLS")),
        "the TLS-decrypted greeting must render; lines={lines:?} echoes={echoes:?}"
    );
    assert!(
        lines.iter().any(|l| l.contains("SERVER_GOT_COMMAND")),
        "the server's reply proves the outbound TLS write was flushed; lines={lines:?}"
    );
    connection.disconnect();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tls_verify_rejects_a_self_signed_certificate() {
    let port = spawn_tls_server(b"never reaches the client\r\n", None).await;

    let (runtime_tx, mut runtime_rx) = tokio::sync::mpsc::unbounded_channel();
    let mut connection = new_connection(runtime_tx);
    // Full verification against the OS trust store: a self-signed cert must fail.
    connection.connect("localhost", port, None, None, true, TlsMode::Verify);

    let (lines, echoes) = drain(&mut runtime_rx, Duration::from_secs(10));
    assert!(
        lines.is_empty(),
        "no application data may flow when verification fails; lines={lines:?}"
    );
    assert!(
        echoes.iter().any(|e| e.starts_with("Connection failed")),
        "verification failure must surface as a named connect failure; echoes={echoes:?}"
    );
    connection.disconnect();
}
