//! End-to-end coverage of the catalogue broadcast wiring in the session run loop
//! (`docs/interop.md` §10): the new-subscriber snapshot sent through
//! `Runtime::subscribe_catalogue`, the leading-edge send + trailing-edge one-shot
//! (`catalogue_resend_at`) that lands a burst's final state within the send window
//! instead of at the 500 ms safety tick, and the entry-budget refusal notice surfacing
//! as a session echo. The pure `CatalogueCadence` state machine has unit tests; these
//! exercise the runtime side those tests cannot see.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::runtime::catalogue::{CatalogueEvent, CatalogueKind, CatalogueSnapshot};
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// Module for the cadence test: an alias that emits one event per firing, so the test
/// can drive dirty catalogue turns one external action at a time and read them back as
/// the `tick` entry's occurrence count in each snapshot.
const CADENCE_TS: &str = r#"
import { createAlias, createEvent } from "smudgy:core";
const tick = createEvent<any>("tick");
createAlias("^burst$", () => { tick.emit({ n: 1 }); });
"#;

/// Module for the notice test: dynamic minting one past the per-producer entry budget
/// (512), so the 513th undeclared entry queues the one-time teaching notice the drain
/// point must surface as a session echo.
const MINT_TS: &str = r#"
import { createEvent, echo } from "smudgy:core";
for (let i = 0; i < 513; i++) createEvent("mint_" + i);
echo("MINTED");
"#;

/// Writes `source` as the server's only module and spawns the session, returning the
/// runtime-action sender plus the still-unconsumed event stream (the caller decides how
/// to drain it).
async fn spawn_session(
    session_id: u32,
    server: &str,
    source: &str,
) -> (
    tokio::sync::mpsc::UnboundedSender<RuntimeAction>,
    std::pin::Pin<Box<dyn futures::Stream<Item = smudgy_core::session::TaggedSessionEvent>>>,
    Vec<String>,
) {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let home = smudgy_core::get_smudgy_home().expect("smudgy home");
    let modules_dir = home.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("catalogue_test.ts"), source).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events: std::pin::Pin<Box<dyn futures::Stream<Item = _>>> = Box::pin(spawn(params));
    let mut lines = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    (tx, events, lines)
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

/// The `tick` event entry's occurrence count in a snapshot (0 when absent).
fn tick_occurrences(snapshot: &CatalogueSnapshot) -> u64 {
    snapshot
        .entries
        .iter()
        .find(|e| e.kind == CatalogueKind::Event && &*e.name == "tick")
        .map_or(0, |e| e.occurrences)
}

/// Receive the next snapshot with its arrival time, failing loudly on timeout.
async fn next_snapshot(
    rx: &mut tokio::sync::broadcast::Receiver<CatalogueEvent>,
) -> (tokio::time::Instant, Arc<CatalogueSnapshot>) {
    let event = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("timed out waiting for a catalogue snapshot")
        .expect("catalogue broadcast closed");
    let CatalogueEvent::Snapshot(snapshot) = event;
    (tokio::time::Instant::now(), snapshot)
}

#[tokio::test]
async fn catalogue_broadcast_sends_on_subscribe_then_leads_and_trails_bursts() {
    let session_id = 7411;
    let (tx, events, _lines) = spawn_session(session_id, "CatalogueCadence", CADENCE_TS).await;
    // Keep the UI receiver alive (an echo failure tears the session down); the handful of
    // buffer updates this test generates sits far below the channel's capacity, so the
    // stream needs no active draining.
    let _events = events;

    let runtime = smudgy_core::session::registry::get_runtime(SessionId::from(session_id))
        .expect("the spawned session is registered");
    let mut catalogue_rx = runtime.subscribe_catalogue();

    // A fresh subscriber gets its snapshot at the next drain, window or no window; wake
    // the parked loop with any action.
    tx.send(RuntimeAction::Echo(Arc::new("wake".to_string())))
        .unwrap();
    let (_, first) = next_snapshot(&mut catalogue_rx).await;
    assert_eq!(
        tick_occurrences(&first),
        0,
        "the new-subscriber snapshot precedes any burst"
    );

    // Each round: two dirty turns in quick succession. The drain between them sends the
    // leading-edge snapshot (first emit only); the second turn's drain lands inside the
    // window, defers, and the trailing-edge one-shot delivers the final state without any
    // further external action — well before the 500 ms safety tick.
    for round in 1..=3u64 {
        // Open the window so the round's first dirty drain is a leading edge.
        tokio::time::sleep(Duration::from_millis(80)).await;
        tx.send(RuntimeAction::Send(Arc::new("burst".to_string())))
            .unwrap();
        tx.send(RuntimeAction::Send(Arc::new("burst".to_string())))
            .unwrap();
        let (leading_at, leading) = next_snapshot(&mut catalogue_rx).await;
        let (trailing_at, trailing) = next_snapshot(&mut catalogue_rx).await;
        assert_eq!(
            tick_occurrences(&leading),
            2 * round - 1,
            "round {round}: the leading-edge snapshot carries the first emit only"
        );
        assert_eq!(
            tick_occurrences(&trailing),
            2 * round,
            "round {round}: the deferred snapshot carries the burst's final state"
        );
        let gap = trailing_at.duration_since(leading_at);
        assert!(
            gap < Duration::from_millis(250),
            "round {round}: the trailing edge must land at the window's edge (~33 ms), \
             not the 500 ms safety tick; got {gap:?}"
        );
    }

    tx.send(RuntimeAction::Shutdown).ok();
}

#[tokio::test]
async fn entry_budget_notice_is_echoed_into_the_session() {
    let (tx, mut events, mut lines) =
        spawn_session(7412, "CatalogueNotice", MINT_TS).await;
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l.contains("MINTED")),
        "the module ran to completion.\n{transcript}"
    );
    assert!(
        lines
            .iter()
            .any(|l| l.contains("session catalogue is full")),
        "the 513th undeclared entry queues the teaching notice and the drain point \
         echoes it.\n{transcript}"
    );
}
