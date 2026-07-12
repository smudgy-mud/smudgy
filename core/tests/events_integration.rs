//! End-to-end: the cross-package / host event bus (`PACKAGE-EVENTS.md`, handle surface per
//! docs/interop.md §11). A loaded module declares an event handle and emits on its
//! own (host-stamped) namespace; a consumer handle (the `events.lookup` escape hatch) and a
//! platform catalog import (`smudgy:events/map`) receive the deliveries — proving handle
//! `.emit` → consumer `.on` routes through the Rust host (the same `CallJavascriptFunction`
//! path used for cross-isolate delivery).

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_cloud::{AreaId, Uuid};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const QUIET_PERIOD: Duration = Duration::from_millis(900);

/// Declare an event handle, subscribe to this module's own (stamped) namespace through the
/// dynamic consumer lookup, then emit through the handle. `ping.emit(…)` is stamped by the
/// host to the same canonical name `events.lookup("user", "ping")` subscribes to (a
/// `modules/` file is a `user#` emitter), so the handler runs and echoes the round-tripped
/// payload. Producer handles deliberately carry no `.on` (interop.md §4c), which is why the
/// consumer side goes through the lookup.
const EVENTS_TS: &str = r#"
import { createEvent, events, echo } from "smudgy:core";
import { room } from "smudgy:events/map";

const ping = createEvent<{ msg: string }>("ping");
events.lookup("user", "ping").on((payload) => {
    echo("GOT:" + payload.msg);
});
// A host-native event via the typed platform catalog: the host emits map:room at the
// location-change site.
room.on((payload) => {
    echo("ROOM:" + payload.roomNumber);
});
// The emitter-reflex call shape (`on("name", fn)`) must fail at subscription time with an
// error naming the real event/state, not register the string and fail at dispatch.
try {
    (room as any).on("room", () => {});
} catch (e) {
    echo("GUARD-ON:" + (e as Error).message);
}
// watch("path", fn) is the legitimate SCOPED form (interop.md 2); the guard trips on a
// scoped call whose second argument is not a callback.
try {
    (globalThis as any).__smudgy_interop_consumer("user").state("vitals").watch("hp", "nope");
} catch (e) {
    echo("GUARD-WATCH:" + (e as Error).message);
}
ping.emit({ msg: "hi" });
"#;

#[tokio::test]
async fn emit_delivers_to_on_handler_via_the_host() {
    // Hermetic smudgy home (this test binary is its own process, so the global home is not shared
    // with other test files).
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);

    let server = "EventsTest";
    let modules_dir = home_path.join(server).join("modules");
    std::fs::create_dir_all(&modules_dir).unwrap();
    std::fs::create_dir_all(home_path.join(server).join("logs")).unwrap();
    std::fs::write(modules_dir.join("events.ts"), EVENTS_TS).unwrap();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(7011),
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

    // Drive a host-native event: a location change must emit `map:room` to the subscriber.
    tx.send(RuntimeAction::SetCurrentLocation(AreaId(Uuid::nil()), Some(42)))
        .unwrap();

    let mut lines = Vec::new();
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            for update in updates.iter() {
                if let BufferUpdate::Append(line) = update {
                    lines.push(line.text.clone());
                }
            }
        }
    }

    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l == "GOT:hi"),
        "ping.emit(…) must deliver to events.lookup('user','ping').on(…) through the host.\nTranscript:\n{transcript}"
    );
    assert!(
        lines.iter().any(|l| l == "ROOM:42"),
        "a SetCurrentLocation must deliver map:room to the smudgy:events/map room handle.\nTranscript:\n{transcript}"
    );
    let guard_on = lines
        .iter()
        .find(|l| l.starts_with("GUARD-ON:"))
        .unwrap_or_else(|| panic!("on(\"name\", fn) must throw at subscription time.\nTranscript:\n{transcript}"));
    assert!(
        guard_on.contains("on() expects a callback function (got string)")
            && guard_on.contains("\"room\" event"),
        "the on() guard must name the received type and the real event.\nGot: {guard_on}"
    );
    let guard_watch = lines
        .iter()
        .find(|l| l.starts_with("GUARD-WATCH:"))
        .unwrap_or_else(|| panic!("a scoped watch without a callback must throw at subscription time.\nTranscript:\n{transcript}"));
    assert!(
        guard_watch.contains("watch() expects a callback function (got string)")
            && guard_watch.contains("\"vitals\" state"),
        "the watch() guard must name the received type and the real state.\nGot: {guard_watch}"
    );
}
