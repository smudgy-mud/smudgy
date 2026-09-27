use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::{InboundCompression, TlsMode, shutdown_io_runtime};
use smudgy_core::session::registry;
use smudgy_core::session::runtime::{RuntimeAction, RuntimeThreadJoinOutcome, join_runtime_thread};
use smudgy_core::session::{SessionEvent, SessionId, SessionParams, spawn};

static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[tokio::test]
async fn connected_session_runtime_joins_on_shutdown() {
    let _guard = TEST_LOCK.lock().unwrap();
    let session_id = SessionId::from(9201_u32);
    let server_name = "test_session_shutdown".to_string();
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let effective_home = smudgy_core::get_smudgy_home().expect("resolve test home");
    std::fs::create_dir_all(effective_home.join(&server_name).join("logs")).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept client");
        let mut buffer = [0_u8; 64];
        while socket.read(&mut buffer).is_ok_and(|read| read != 0) {}
    });

    let params = Arc::new(SessionParams {
        session_id,
        server_name: Arc::new(server_name),
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

    tx.send(RuntimeAction::Connect {
        host: Arc::new("127.0.0.1".to_string()),
        port,
        wss_url: None,
        send_on_connect: None,
        send_on_connect_redactions: Arc::new(Vec::new()),
        encoding: None,
        compression: InboundCompression::NONE,
        tls: TlsMode::Off,
    })
    .unwrap();

    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .expect("timed out waiting for Connected")
            .expect("event stream ended before Connected");
        if matches!(event.event, SessionEvent::Connected) {
            break;
        }
    }

    tx.send(RuntimeAction::Shutdown).unwrap();
    drop(tx);
    drop(events);
    shutdown_io_runtime();
    let joined = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || join_runtime_thread(session_id)),
    )
    .await
    .expect("connected runtime did not join")
    .expect("join task panicked");
    assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });
    server.join().unwrap();
}

#[tokio::test]
async fn dropping_event_stream_before_ready_unregisters_runtime() {
    let _guard = TEST_LOCK.lock().unwrap();
    let session_id = SessionId::from(9202_u32);
    let server_name = "test_pre_ready_session_shutdown".to_string();
    let home = tempfile::tempdir().expect("create temp home");
    smudgy_core::set_smudgy_home(home.path());
    let effective_home = smudgy_core::get_smudgy_home().expect("resolve test home");
    std::fs::create_dir_all(effective_home.join(&server_name).join("logs")).unwrap();

    let params = Arc::new(SessionParams {
        session_id,
        server_name: Arc::new(server_name),
        profile_name: Arc::new("test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let events = spawn(params);
    assert!(
        registry::get_runtime(session_id).is_some(),
        "spawn registers the runtime before returning its event stream"
    );

    // Reproduces closing a session while scripts are still loading: the UI
    // subscription disappears before RuntimeReady can hand it the normal
    // runtime-action sender.
    drop(events);

    let joined = tokio::time::timeout(
        Duration::from_secs(30),
        tokio::task::spawn_blocking(move || join_runtime_thread(session_id)),
    )
    .await
    .expect("immediately stopped runtime did not join")
    .expect("join task panicked");
    assert_eq!(joined, RuntimeThreadJoinOutcome::Clean { session_id });
    assert!(
        registry::get_runtime(session_id).is_none(),
        "joined runtime remains published in the session registry"
    );
}
