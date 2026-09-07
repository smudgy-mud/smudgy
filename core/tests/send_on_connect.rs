//! Profile auto-send timing over the real socket and session-runtime paths.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::triggers::TriggerDefinition;
use smudgy_core::session::connection::{InboundCompression, TlsMode};
use smudgy_core::session::runtime::{IsolateId, Origin, RuntimeAction};
use smudgy_core::session::{SessionEvent, SessionId, SessionParams, spawn};

fn expect_no_client_data(socket: &mut std::net::TcpStream, phase: &str) {
    let mut byte = [0_u8; 1];
    match socket.read(&mut byte) {
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ) => {}
        Ok(0) => panic!("client disconnected while waiting during {phase}"),
        Ok(_) => panic!("profile text was sent too early during {phase}"),
        Err(error) => panic!("socket read failed during {phase}: {error}"),
    }
}

#[tokio::test]
async fn profile_text_waits_for_first_processed_display_packet() {
    let server_name = "test_deferred_send_on_connect";
    let home = tempfile::tempdir().expect("create temp home");
    smudgy_core::set_smudgy_home(home.path());
    let effective_home = smudgy_core::get_smudgy_home().expect("resolve test home");
    std::fs::create_dir_all(effective_home.join(server_name).join("logs"))
        .expect("create logs directory");

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let port = listener.local_addr().expect("listener address").port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept client");
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .expect("set short read timeout");

        // A successful TCP connect alone must not release the profile text.
        expect_no_client_data(&mut socket, "connect");

        // A complete packet containing only an empty terminal line is still
        // not displayable and must not release it either.
        socket.write_all(b"\r\n").expect("send empty packet");
        expect_no_client_data(&mut socket, "empty packet");

        socket
            .write_all(b"Welcome\r\n")
            .expect("send displayable packet");
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set command read timeout");

        // The incoming trigger must run before the deferred profile command:
        // this proves release happens after the packet's runtime processing,
        // not merely after its bytes were parsed on the socket worker.
        let expected = b"triggered\r\nlogin\r\n";
        let mut received = vec![0_u8; expected.len()];
        socket
            .read_exact(&mut received)
            .expect("read trigger and profile commands");
        assert_eq!(received, expected);
    });

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_301_u32),
        server_name: Arc::new(server_name.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
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

    tx.send(RuntimeAction::AddTrigger {
        isolate: IsolateId::Main,
        origin: Origin::User,
        name: Arc::new("send_before_profile".to_string()),
        trigger: Box::new(TriggerDefinition {
            patterns: Some(vec!["^Welcome$".to_string()]),
            script: Some("triggered".to_string()),
            ..TriggerDefinition::default()
        }),
        fire_limit: None,
        line_limit: None,
    })
    .expect("register trigger");
    tx.send(RuntimeAction::Connect {
        host: Arc::new("127.0.0.1".to_string()),
        port,
        send_on_connect: Some(Arc::new("login".to_string())),
        send_on_connect_redactions: Arc::new(Vec::new()),
        encoding: None,
        compression: InboundCompression::NONE,
        tls: TlsMode::Off,
    })
    .expect("connect session");

    server.join().expect("test server thread");
    tx.send(RuntimeAction::Shutdown).ok();
}

/// The reconnect race: a replaced socket's late `Disconnected` (stamped with
/// the OLD connection's generation) must not erase the NEW connection's
/// pending profile send, which is still waiting for the new socket's first
/// displayable packet.
#[tokio::test]
async fn replaced_sockets_late_disconnected_keeps_the_new_pending_send() {
    let server_name = "test_send_on_connect_replaced_socket";
    let home = tempfile::tempdir().expect("create temp home");
    smudgy_core::set_smudgy_home(home.path());
    let effective_home = smudgy_core::get_smudgy_home().expect("resolve test home");
    std::fs::create_dir_all(effective_home.join(server_name).join("logs"))
        .expect("create logs directory");

    let first_listener = TcpListener::bind("127.0.0.1:0").expect("bind first server");
    let first_port = first_listener.local_addr().expect("first address").port();
    let second_listener = TcpListener::bind("127.0.0.1:0").expect("bind second server");
    let second_port = second_listener.local_addr().expect("second address").port();

    // The first server just holds its socket open until the client replaces
    // the connection (read returns 0 at that point).
    let first_server = std::thread::spawn(move || {
        let (mut socket, _) = first_listener.accept().expect("accept first client");
        socket
            .set_read_timeout(Some(Duration::from_secs(30)))
            .expect("set first read timeout");
        let mut byte = [0_u8; 1];
        let _ = socket.read(&mut byte);
    });

    // The second server releases its greeting only when the test signals that
    // the stale Disconnected has been dispatched, then expects the deferred
    // profile text.
    let (release_greeting, greeting_gate) = std::sync::mpsc::channel::<()>();
    let second_server = std::thread::spawn(move || {
        let (mut socket, _) = second_listener.accept().expect("accept second client");
        socket
            .set_read_timeout(Some(Duration::from_millis(400)))
            .expect("set second read timeout");
        expect_no_client_data(&mut socket, "second connect");

        greeting_gate
            .recv_timeout(Duration::from_secs(30))
            .expect("test signals the greeting");
        socket
            .write_all(b"Welcome\r\n")
            .expect("send displayable packet");
        socket
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("set profile read timeout");

        let expected = b"login\r\n";
        let mut received = vec![0_u8; expected.len()];
        socket
            .read_exact(&mut received)
            .expect("the NEW connection's profile send survives the stale Disconnected");
        assert_eq!(received, expected);
    });

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_302_u32),
        server_name: Arc::new(server_name.to_string()),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
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

    // First connection, no profile send.
    tx.send(RuntimeAction::Connect {
        host: Arc::new("127.0.0.1".to_string()),
        port: first_port,
        send_on_connect: None,
        send_on_connect_redactions: Arc::new(Vec::new()),
        encoding: None,
        compression: InboundCompression::NONE,
        tls: TlsMode::Off,
    })
    .expect("connect first socket");
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for first Connected")
            .expect("event stream ended before first Connected");
        if matches!(event.event, SessionEvent::Connected) {
            break;
        }
    }

    // Replace the connection. The new pending profile send is installed
    // before the old socket's teardown can report.
    tx.send(RuntimeAction::Connect {
        host: Arc::new("127.0.0.1".to_string()),
        port: second_port,
        send_on_connect: Some(Arc::new("login".to_string())),
        send_on_connect_redactions: Arc::new(Vec::new()),
        encoding: None,
        compression: InboundCompression::NONE,
        tls: TlsMode::Off,
    })
    .expect("connect second socket");

    // The replaced socket's late Disconnected passes through dispatch here;
    // only after it has been processed may the greeting release the send.
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for the stale Disconnected")
            .expect("event stream ended before the stale Disconnected");
        if matches!(event.event, SessionEvent::Disconnected) {
            break;
        }
    }
    release_greeting.send(()).expect("signal the greeting");

    // Keep draining so the runtime's UI channel never blocks while the
    // second server waits for the profile text.
    while !second_server.is_finished() {
        let _ = tokio::time::timeout(Duration::from_millis(100), events.next()).await;
    }
    first_server.join().expect("first server thread");
    second_server.join().expect("second server thread");
    tx.send(RuntimeAction::Shutdown).ok();
}
