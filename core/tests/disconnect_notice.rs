//! The disconnect notice reports how long the RUNTIME held the connection:
//! the clock starts when `Connected` is dispatched and is read when the
//! socket task's `DisconnectNotice` is dispatched — behind every line the
//! socket queued before it closed.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{SessionEvent, SessionId, SessionParams, spawn};

type Events =
    std::pin::Pin<Box<dyn futures::Stream<Item = smudgy_core::session::TaggedSessionEvent>>>;

async fn spawn_session(
    server: &str,
    id: u32,
) -> (Events, tokio::sync::mpsc::UnboundedSender<RuntimeAction>) {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home_path = smudgy_core::get_smudgy_home().expect("resolve test home");
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });
    let mut events: Events = Box::pin(spawn(params));
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };
    (events, tx)
}

/// Drain events until a main-buffer line starting with `prefix` appears.
async fn wait_for_line_starting_with(events: &mut Events, prefix: &str) -> String {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for a line starting with {prefix:?}"))
            .unwrap_or_else(|| panic!("event stream ended before a line starting with {prefix:?}"));
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let Some(line) = update.main_text()
                    && line.text.starts_with(prefix)
                {
                    return line.text.clone();
                }
            }
        }
    }
}

fn parse_duration(text: &str) -> Duration {
    let (hms, fraction) = text.rsplit_once('.').expect("HH:MM:SS.ffff");
    let mut parts = hms.split(':');
    let hours: u64 = parts.next().unwrap().parse().unwrap();
    let minutes: u64 = parts.next().unwrap().parse().unwrap();
    let seconds: u64 = parts.next().unwrap().parse().unwrap();
    assert!(parts.next().is_none(), "unexpected extra field in {text:?}");
    assert_eq!(fraction.len(), 4, "four fractional digits in {text:?}");
    let tenth_millis: u64 = fraction.parse().unwrap();
    Duration::from_secs(hours * 3600 + minutes * 60 + seconds)
        + Duration::from_micros(tenth_millis * 100)
}

#[tokio::test]
async fn connection_lost_notice_reports_the_runtimes_connected_time() {
    let (mut events, tx) = spawn_session("DisconnectNoticeLost", 7201).await;

    tx.send(RuntimeAction::Connected).unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), events.next())
            .await
            .expect("timed out waiting for Connected")
            .expect("event stream ended before Connected");
        if matches!(event.event, SessionEvent::Connected) {
            break;
        }
    }

    let held_for = Duration::from_millis(120);
    tokio::time::sleep(held_for).await;

    tx.send(RuntimeAction::Disconnected {
        connection_generation: 0,
    })
    .unwrap();
    tx.send(RuntimeAction::DisconnectNotice {
        connection_generation: 0,
        graceful: false,
    })
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let notice = wait_for_line_starting_with(&mut events, "Connection lost after ").await;
    let reported = parse_duration(notice.strip_prefix("Connection lost after ").unwrap());
    assert!(
        reported >= held_for,
        "the notice must cover the time the runtime held the connection: {notice:?}"
    );
    assert!(
        reported < Duration::from_secs(30),
        "the notice is measured from this connection's Connected, not the epoch: {notice:?}"
    );
}

#[tokio::test]
async fn graceful_notice_is_worded_as_disconnected() {
    let (mut events, tx) = spawn_session("DisconnectNoticeGraceful", 7202).await;

    tx.send(RuntimeAction::Connected).unwrap();
    tx.send(RuntimeAction::Disconnected {
        connection_generation: 0,
    })
    .unwrap();
    tx.send(RuntimeAction::DisconnectNotice {
        connection_generation: 0,
        graceful: true,
    })
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let notice = wait_for_line_starting_with(&mut events, "Disconnected after ").await;
    parse_duration(notice.strip_prefix("Disconnected after ").unwrap());
}

/// A replaced socket's late notice must not borrow the NEW connection's
/// clock: it carries the old generation and reads as a bare notice.
#[tokio::test]
async fn a_stale_generations_notice_reads_bare() {
    let (mut events, tx) = spawn_session("DisconnectNoticeStale", 7203).await;

    tx.send(RuntimeAction::Connected).unwrap();
    tx.send(RuntimeAction::DisconnectNotice {
        connection_generation: 41,
        graceful: true,
    })
    .unwrap();
    tx.send(RuntimeAction::RequestRepaint).unwrap();

    let notice = wait_for_line_starting_with(&mut events, "Disconnected").await;
    assert_eq!(notice, "Disconnected.");
}
