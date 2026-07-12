//! End-to-end: event unsubscribe (`PACKAGE-EVENTS.md` §4.2.1). A consumer handle's `.on(...)`
//! returns a subscription whose `off()` cancels it. A handler unsubscribes itself on its first
//! delivery; a second host emit of the same event must then NOT reach it — proving `op_smudgy_off`
//! removes the subscriber from the shared `EventRegistry`.
//!
//! This lives in its own test binary (not alongside `events_integration.rs`) because
//! `set_smudgy_home` is a process-global `OnceLock`: two tests in one binary running in parallel
//! would clobber each other's temp home (first-set-wins), so each home-setting test gets its own
//! process.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_cloud::{AreaId, Uuid};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// Subscribe to the host-native `map:room` event (the `smudgy:events/map` catalog handle) and
/// unsubscribe from inside the handler. The host emits `map:room` on every location change;
/// after the first delivery `sub.off()` drops the subscriber, so a second location change must
/// not echo again. `sub` is assigned synchronously right after `.on(...)` returns and the
/// handler only ever runs later (on a host emit), so the closure always sees a defined `sub`.
/// The handler calls `off()` twice to also prove idempotency — the second `off()` must be a
/// harmless no-op (no throw), not a double-removal panic.
const OFF_TS: &str = r#"
import { echo } from "smudgy:core";
import { room } from "smudgy:events/map";

const sub = room.on((payload) => {
    echo("ROOM:" + payload.roomNumber);
    sub.off();
    sub.off();
});
"#;

#[tokio::test]
async fn off_stops_further_delivery() {
    // Hermetic smudgy home (this test binary is its own process, so the global home is not shared
    // with other test files).
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);

    let server = "EventsOffTest";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("events_off.ts"), OFF_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7012),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: None,
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));

    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        if let SessionEvent::RuntimeReady(tx) = event.event {
            break tx;
        }
    };

    // First location change → `map:room` delivered → the handler echoes `ROOM:1`, then unsubscribes
    // itself. Drain to quiescence so the handler (and its `off()`) has run before the second emit.
    tx.send(RuntimeAction::SetCurrentLocation(AreaId(Uuid::nil()), Some(1)))
        .unwrap();
    let mut first = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    first.push(line.text.clone());
                }
            }
        }
    }

    // Second location change → the subscriber is gone → no `ROOM:2` echo should appear.
    tx.send(RuntimeAction::SetCurrentLocation(AreaId(Uuid::nil()), Some(2)))
        .unwrap();
    let mut second = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    second.push(line.text.clone());
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    assert!(
        first.iter().any(|l| l == "ROOM:1"),
        "the first map:room must reach the subscriber before off().\nFirst batch:\n{}",
        first.join("\n")
    );
    assert!(
        !second.iter().any(|l| l == "ROOM:2"),
        "after sub.off(), a second map:room must NOT reach the handler.\nSecond batch:\n{}",
        second.join("\n")
    );
}
