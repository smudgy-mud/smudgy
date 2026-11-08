//! Scripts read and write labels attached to rooms where they are drawn: a
//! label's `x`, `y` and `level` follow its room, `room` names the map room it
//! is attached to, `createLabel` and `setLabel` take positions as they are
//! drawn and a `room` to attach to (or `null` to free), and a deleted room
//! leaves its labels where they were.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_cloud::{CloudMapper, CompositeBackend, LocalBackend, Mapper, MapperBackend};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const SERVER: &str = "MapperScriptLabelRooms";

const LABEL_ROOMS_TS: &str = r#"
import { echo, mapper } from "smudgy:core";

const failures = [];
const check = (label, actual, expected) => {
    if (actual !== expected) failures.push(`${label}: got [${actual}] want [${expected}]`);
};

await mapper.ready();
const area = await mapper.createArea("Signs", { storage: "session" });
const gate = await mapper.createRoom(area, { title: "Gate", x: 4, y: 2, level: 1 });
const square = await mapper.createRoom(area, { title: "Square", x: 9, y: 9, level: 0 });
const label = (id) => mapper.getAreaById(area.id).labels.find((candidate) => candidate.id === id);

const sign = await mapper.createLabel(area, {
    room: gate, x: 4.5, y: 1, width: 2, height: 0.5, text: "Sign",
});
check("room", label(sign)?.room, gate);
check("x", label(sign)?.x, 4.5);
check("y", label(sign)?.y, 1);
check("level", label(sign)?.level, 1);

const free = await mapper.createLabel(area, { x: 1, y: 1, width: 2, height: 0.5, text: "Free" });
check("free room", label(free)?.room, undefined);
check("free has no room key", "room" in label(free), false);

await mapper.updateRoom(area, gate, { x: 6, y: 3 });
check("follows x", label(sign)?.x, 6.5);
check("follows y", label(sign)?.y, 2);

await mapper.setLabel(area, sign, { x: 7 });
check("set x", label(sign)?.x, 7);
check("still attached", label(sign)?.room, gate);

await mapper.setLabel(area, sign, { room: null });
check("freed", label(sign)?.room, undefined);
check("freed x", label(sign)?.x, 7);
check("freed y", label(sign)?.y, 2);
check("freed level", label(sign)?.level, 1);

await mapper.setLabel(area, sign, { room: square });
check("attached again", label(sign)?.room, square);
check("attached x", label(sign)?.x, 7);
check("attached level", label(sign)?.level, 0);

let missing = "";
try {
    await mapper.createLabel(area, { room: 99, x: 0, y: 0, width: 1, height: 1, text: "x" });
} catch (error) {
    missing = String(error?.message ?? error);
}
check("missing room refused", missing.includes("room 99"), true);

await mapper.deleteRoom(area, square);
check("freed by delete", label(sign)?.room, undefined);
check("freed by delete x", label(sign)?.x, 7);
check("freed by delete y", label(sign)?.y, 2);
check("freed by delete level", label(sign)?.level, 0);

echo(failures.length === 0 ? "LABEL_ROOMS_OK" : `LABEL_ROOMS_FAIL ${failures.join(" | ")}`);
"#;

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

#[tokio::test]
async fn scripts_place_attached_labels_where_they_are_drawn() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");

    let modules = smudgy_home.join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create modules directory");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).expect("create logs directory");
    std::fs::write(modules.join("label_rooms.ts"), LABEL_ROOMS_TS).expect("write the module");

    // The session tier lives behind the composite; the cloud tier is never reached.
    let map_root = smudgy_home.join("map-label-rooms");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_378_u32),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
        graphics_decoder: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines: Vec<String> = Vec::new();
    let mut runtime = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !lines.iter().any(|line| line.starts_with("LABEL_ROOMS_")) {
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            break;
        };
        let Ok(Some(event)) = tokio::time::timeout(remaining, events.next()).await else {
            break;
        };
        match event.event {
            SessionEvent::RuntimeReady(tx) => runtime = Some(tx),
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    }
    if let Some(tx) = runtime {
        tx.send(RuntimeAction::Shutdown).ok();
    }

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|line| line == "LABEL_ROOMS_OK"),
        "attached labels misbehaved:\n{transcript}"
    );
}
