//! The graphics decoder in `SessionParams` reaches every connection the session makes: a
//! transmit on a fresh socket is decoded and answered, and so is one after a reconnect,
//! since the `Connect` dispatch arm hands the decoder to each new connection.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::StreamExt;
use smudgy_core::session::connection::{GraphicsDecoder, InboundCompression, TlsMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{SessionEvent, SessionId, SessionParams, spawn};
use smudgy_protocol::graphics::{DecodeRequest, DecodeResult, DecodedPixels, encode_payload};

/// Expands `f=24` pixels to RGBA on the calling thread.
struct StubDecoder;

impl GraphicsDecoder for StubDecoder {
    fn decode(&self, request: DecodeRequest, done: Box<dyn FnOnce(DecodeResult) + Send + 'static>) {
        let rgba: Vec<u8> = request
            .data
            .chunks_exact(3)
            .flat_map(|px| [px[0], px[1], px[2], 0xff])
            .collect();
        done(Ok(DecodedPixels {
            width: request.width,
            height: request.height,
            rgba: rgba.into(),
        }));
    }
}

/// Read from `sock` until `collected` contains `needle` (or panic at the deadline),
/// returning where the match starts.
fn read_until(sock: &mut TcpStream, collected: &mut Vec<u8>, needle: &[u8], what: &str) -> usize {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(pos) = collected
            .windows(needle.len())
            .position(|window| window == needle)
        {
            return pos;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}; received so far: {collected:02x?}"
        );
        let mut buf = [0_u8; 256];
        match sock.read(&mut buf) {
            Ok(0) => panic!("socket closed while waiting for {what}"),
            Ok(n) => collected.extend_from_slice(&buf[..n]),
            Err(e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => panic!("read failed while waiting for {what}: {e}"),
        }
    }
}

/// Wait for `wanted`, draining everything else so the runtime's UI channel never blocks.
async fn wait_for(
    events: &mut std::pin::Pin<
        Box<impl futures::Stream<Item = smudgy_core::session::TaggedSessionEvent>>,
    >,
    wanted: impl Fn(&SessionEvent) -> bool,
    what: &str,
) {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
            .unwrap_or_else(|| panic!("event stream ended before {what}"));
        if wanted(&event.event) {
            return;
        }
    }
}

#[tokio::test]
async fn the_decoder_reaches_each_connection_in_turn() {
    let server_name = "test_graphics_decoder_on_connect";
    let home = tempfile::tempdir().expect("create temp home");
    smudgy_core::set_smudgy_home(home.path());
    let effective_home = smudgy_core::get_smudgy_home().expect("resolve test home");
    std::fs::create_dir_all(effective_home.join(server_name).join("logs"))
        .expect("create logs directory");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let port = listener.local_addr().expect("listener address").port();
    // Each accept gets a transmit-and-display then DA1, and must read the transmit's
    // `OK` before DA1's answer; then the server hangs up, so the client reconnects.
    let server = std::thread::spawn(move || {
        for id in 1..=2_u32 {
            let (mut socket, _) = listener.accept().expect("accept client");
            socket
                .set_read_timeout(Some(Duration::from_millis(200)))
                .expect("set read timeout");
            let payload = encode_payload(&[9, 8, 7]);
            socket
                .write_all(
                    format!("\x1b_Ga=T,i={id},f=24,s=1,v=1;{payload}\x1b\\\x1b[c\r\n").as_bytes(),
                )
                .expect("send the image and DA1");
            let mut received = Vec::new();
            let ok = read_until(
                &mut socket,
                &mut received,
                format!("\x1b_Gi={id};OK\x1b\\").as_bytes(),
                "the transmit's OK",
            );
            let da1 = read_until(&mut socket, &mut received, b"\x1b[?62;c", "DA1's answer");
            assert!(ok < da1, "connection {id}: OK before DA1 in {received:?}");
        }
    });

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_303_u32),
        server_name: Arc::new(server_name.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
        graphics_decoder: Some(Arc::new(StubDecoder)),
    });
    let mut events = Box::pin(spawn(params));
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };
    let connect = || RuntimeAction::Connect {
        host: Arc::new("127.0.0.1".to_string()),
        port,
        wss_url: None,
        send_on_connect: None,
        send_on_connect_redactions: Arc::new(Vec::new()),
        encoding: None,
        compression: InboundCompression::NONE,
        tls: TlsMode::Off,
        graphics: smudgy_core::models::server::GraphicsMode::default(),
        graphics_local_files: false,
    };

    tx.send(connect()).expect("first connect");
    wait_for(
        &mut events,
        |e| matches!(e, SessionEvent::Connected),
        "the first Connected",
    )
    .await;
    wait_for(
        &mut events,
        |e| matches!(e, SessionEvent::Disconnected),
        "the server hanging up on the first socket",
    )
    .await;

    tx.send(connect()).expect("second connect");
    wait_for(
        &mut events,
        |e| matches!(e, SessionEvent::Connected),
        "the second Connected",
    )
    .await;
    wait_for(
        &mut events,
        |e| matches!(e, SessionEvent::Disconnected),
        "the server hanging up on the second socket",
    )
    .await;

    server.join().expect("test server thread");
    tx.send(RuntimeAction::Shutdown).ok();
}
