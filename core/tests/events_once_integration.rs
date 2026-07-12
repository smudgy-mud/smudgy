//! End-to-end: single-shot subscribe (`once`, sibling of `on` in `PACKAGE-EVENTS.md` §7). A
//! consumer handle's `.once(...)` is `.on(...)` that unsubscribes itself right before its first
//! (and only) delivery. This proves the two guarantees that distinguish it from `on`:
//!   1. The `fired` guard collapses *already-queued* deliveries: two synchronous `emit`s register two
//!      `CallJavascriptFunction` actions against the still-live subscriber (the self-`off()` only runs
//!      inside the first delivery, which dispatches later), yet the handler echoes exactly once.
//!   2. Auto-unsubscribe persists across host emits: after the first host `map:room`, a second one
//!      must NOT reach the handler.
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

/// (1) Two synchronous handle `.emit`s on our own (host-stamped) namespace. Both are queued
/// against the live `once` subscriber before the first delivery runs, so only the JS `fired`
/// guard keeps this to a single `PING` echo. (2) A `.once` on a host-native event (the
/// `smudgy:events/map` catalog handle): it must echo the first location change and then stay
/// silent for the second, because it auto-unsubscribed.
const ONCE_TS: &str = r#"
import { createEvent, events, echo } from "smudgy:core";
import { room } from "smudgy:events/map";

const ping = createEvent<{ msg: string }>("ping");
let pings = 0;
events.lookup("user", "ping").once((payload) => {
    pings += 1;
    echo("PING#" + pings + ":" + payload.msg);
});
ping.emit({ msg: "a" });
ping.emit({ msg: "b" });

room.once((payload) => {
    echo("ROOM:" + payload.roomNumber);
});
"#;

#[tokio::test]
async fn once_delivers_exactly_one_time() {
    // Hermetic smudgy home (this test binary is its own process, so the global home is not shared
    // with other test files).
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);

    let server = "EventsOnceTest";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("events_once.ts"), ONCE_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7013),
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

    // First location change → the host emits `map:room` → the `once` handler echoes, then auto-offs.
    // This is also the first thing to wake the idle event loop after startup, so the load-time
    // `emit("ping", …)` deliveries (queued during module evaluation) are dispatched here too.
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

    // Second location change → the subscriber is gone → no `ROOM:2` echo.
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

    // The two synchronous load-time emits must collapse to a single delivery (the `fired` guard):
    // exactly one `PING#…` line across every batch, and it is the first emit's payload.
    let pings: Vec<&String> = first
        .iter()
        .chain(second.iter())
        .filter(|l| l.starts_with("PING#"))
        .collect();
    assert_eq!(
        pings,
        vec!["PING#1:a"],
        "once('user#ping', …) must echo exactly once (the first emit), even though a second emit was \
         queued before the self-off() ran.\nFirst batch:\n{}\nSecond batch:\n{}",
        first.join("\n"),
        second.join("\n")
    );
    assert!(
        first.iter().any(|l| l == "ROOM:1"),
        "the first map:room must reach the once('map:room', …) handler.\nFirst batch:\n{}",
        first.join("\n")
    );
    assert!(
        !second.iter().any(|l| l == "ROOM:2"),
        "after the first delivery, once auto-unsubscribes, so a second map:room must NOT echo.\n\
         Second batch:\n{}",
        second.join("\n")
    );
}
