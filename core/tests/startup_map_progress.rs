//! Startup must publish its map phase while a cloud response is still pending.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_cloud::{CloudMapper, Mapper};
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, runtime::RuntimeAction, spawn,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn map_loading_progress_reaches_the_ui_before_the_cloud_answers() {
    let home = tempfile::tempdir().unwrap();
    smudgy_core::set_smudgy_home(home.path());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let (pause, held) = tokio::sync::watch::channel(true);
    let server = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            let mut held = held.clone();
            tokio::spawn(async move {
                let mut request = [0; 4096];
                if socket.read(&mut request).await.unwrap() == 0 {
                    return;
                }
                held.wait_for(|held| !held).await.unwrap();
                let body = r#"{"success":true,"data":[]}"#;
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                socket.write_all(response.as_bytes()).await.ok();
            });
        }
    });
    let mapper = Mapper::new(
        Arc::new(CloudMapper::new(url, "test".into())),
        home.path().join("maps"),
    );
    let mut events = Box::pin(spawn(Arc::new(SessionParams {
        session_id: SessionId::from(9701),
        server_name: Arc::new("StartupMapProgress".into()),
        profile_name: Arc::new("Test".into()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper),
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    })));

    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = events.next().await {
            match event.event {
                SessionEvent::RuntimeReady(_) => panic!("cloud is still held"),
                SessionEvent::UpdateBuffer(updates)
                    if updates.iter().any(|update| {
                        matches!(update, BufferUpdate::ReplaceSystem(row) if row.line.text.contains("Loading maps"))
                    }) => {
                    return;
                }
                _ => {}
            }
        }
        panic!("session ended before publishing map progress");
    })
    .await
    .expect("map progress must be visible before the startup deadline");

    pause.send_replace(false);
    tokio::time::timeout(Duration::from_secs(15), async {
        while let Some(event) = events.next().await {
            if let SessionEvent::RuntimeReady(tx) = event.event {
                tx.send(RuntimeAction::Shutdown).unwrap();
                return;
            }
        }
        panic!("session ended before becoming ready");
    })
    .await
    .expect("startup must complete when cloud loading resumes");
    server.abort();
}
