use std::io::Read;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::connection::{TlsMode, shutdown_io_runtime};
use smudgy_core::session::runtime::{RuntimeAction, join_runtime_threads};
use smudgy_core::session::{SessionEvent, SessionId, SessionParams, spawn};

#[tokio::test]
async fn connected_session_runtime_joins_on_shutdown() {
    let server_name = "test_session_shutdown".to_string();
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    std::fs::create_dir_all(home_path.join(&server_name).join("logs")).unwrap();

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().expect("accept client");
        let mut buffer = [0_u8; 64];
        while socket.read(&mut buffer).is_ok_and(|read| read != 0) {}
    });

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9201u32),
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
        send_on_connect: None,
        send_on_connect_redactions: Arc::new(Vec::new()),
        encoding: None,
        compression: false,
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
    shutdown_io_runtime();
    join_runtime_threads();
    server.join().unwrap();
}
