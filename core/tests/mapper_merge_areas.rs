//! End-to-end coverage for `mapper.mergeAreas` through V8 against a real
//! tiered mapper: the op is registered, the arguments marshal, a whole
//! merge and then a partial one land in the atlas cache the way a script
//! sees them, the session's current location follows the moved room (with
//! `map:room` fired), and every refusal surfaces under its code.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_cloud::{CloudMapper, CompositeBackend, LocalBackend, Mapper, MapperBackend};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const SERVER: &str = "MapperMergeAreas";

/// Areas A (rooms 1..3), B (rooms 1..2) and C (rooms 1..2) in the local tier
/// plus one session-tier area; A.3 and B.1 link east/west, C.1 exits east
/// into B.2 and north/south with C.2; the session stands in B.2. Folding B
/// into A at x+10 must renumber B.1 to A.4 and B.2 to A.5 (A's floor is 4),
/// retarget every exit that named a B room, drop B from every lookup, and
/// move the current location to A.5. Then moving only C.2 into A must land
/// it as A.6, leave C standing with C.1, point C.1's north exit at A.6 and
/// keep A.6's south exit on C.1.
const MERGE_TS: &str = r#"
import { echo, mapper } from "smudgy:core";
import { room as roomEvent } from "smudgy:events/map";

const failures = [];
const check = (label, actual, expected) => {
    if (actual !== expected) failures.push(`${label}: got [${actual}] want [${expected}]`);
};
const refuses = async (label, code, call) => {
    try {
        await call();
        failures.push(`${label}: resolved, want ${code}`);
    } catch (error) {
        const text = error instanceof Error ? error.message : String(error);
        if (!text.includes(code)) failures.push(`${label}: threw [${text}] want ${code}`);
    }
};

await mapper.ready();
const a = await mapper.createArea("Alpha", { storage: "local" });
const b = await mapper.createArea("Beta", { storage: "local" });
const c = await mapper.createArea("Gamma", { storage: "local" });
const s = await mapper.createArea("Scratch", { storage: "session" });
const tag = (areaId) => (areaId === a.id ? "A" : areaId === b.id ? "B" : areaId === c.id ? "C" : "?");

roomEvent.on((payload) => {
    echo(`MAP_ROOM ${tag(payload.areaId)} ${payload.roomNumber}`);
});

const a1 = await mapper.createRoom(a, { title: "A1", x: 0, y: 0, level: 0 });
const a2 = await mapper.createRoom(a, { title: "A2", x: 1, y: 0, level: 0 });
const a3 = await mapper.createRoom(a, { title: "A3", x: 2, y: 0, level: 0, externalId: "a3" });
const b1 = await mapper.createRoom(b, { title: "B1", x: 0, y: 0, level: 0, externalId: "b1" });
const b2 = await mapper.createRoom(b, { title: "B2", x: 1, y: 0, level: 0, externalId: "b2" });
await mapper.setRoomProperty(b, b2, "k", "v");
const c1 = await mapper.createRoom(c, { title: "C1", x: 0, y: 0, level: 0 });
const c2 = await mapper.createRoom(c, { title: "C2", x: 0, y: -1, level: 0, externalId: "c2" });
await mapper.createRoom(s, { title: "S1", x: 0, y: 0, level: 0 });
check("numbering", [a1, a2, a3, b1, b2, c1, c2].join(","), "1,2,3,1,2,1,2");

await mapper.createRoomExit(a, a3, { from_direction: "East", to_direction: "West", to_area_id: b.id, to_room_number: b1 });
await mapper.createRoomExit(b, b1, { from_direction: "West", to_direction: "East", to_area_id: a.id, to_room_number: a3 });
await mapper.createRoomExit(c, c1, { from_direction: "East", to_direction: "West", to_area_id: b.id, to_room_number: b2 });
await mapper.createRoomExit(c, c1, { from_direction: "North", to_direction: "South", to_area_id: c.id, to_room_number: c2 });
await mapper.createRoomExit(c, c2, { from_direction: "South", to_direction: "North", to_area_id: c.id, to_room_number: c1 });

mapper.setCurrentLocation(b.id, b2);

const result = await mapper.mergeAreas(a, [{ area: b, translate: { x: 10 } }]);
check("result/array", Array.isArray(result), true);
check(
    "remap",
    result.map((moved) => `${tag(moved.from.area)}${moved.from.room}>${moved.to}`).sort().join(","),
    "B1>4,B2>5",
);
check("areas", mapper.areas.some((area) => area.id === b.id), false);
let lookupThrew = false;
try { mapper.getAreaById(b.id); } catch { lookupThrew = true; }
check("getAreaById", lookupThrew, true);

const movedB2 = mapper.findRoomByExternalId("b2");
check("b2/area", tag(movedB2?.area_id), "A");
check("b2/room", movedB2?.room_number, 5);
check("b2/x", movedB2?.x, 11);
check(
    "property",
    mapper.findRoomsByProperty("k", "v").map((room) => `${tag(room.area_id)}${room.room_number}`).join(","),
    "A5",
);

const target = (exit) => `${tag(exit?.to_area_id)}${exit?.to_room_number}`;
const alpha = mapper.getAreaById(a.id);
check("a3/exit", target(alpha.room(a3)?.exits.find((exit) => exit.from_direction === "East")), "A4");
check("a4/exit", target(alpha.room(4)?.exits.find((exit) => exit.from_direction === "West")), "A3");
const gamma = mapper.getAreaById(c.id);
check("c1/exit", target(gamma.room(c1)?.exits.find((exit) => exit.from_direction === "East")), "A5");

const location = mapper.getCurrentLocation();
check("location", `${tag(location?.area)}${location?.room}`, "A5");

await refuses("no_sources", "merge_areas_no_sources", () => mapper.mergeAreas(a, []));
await refuses("same_area", "merge_areas_same_area", () => mapper.mergeAreas(a, [a]));
await refuses("repeated", "merge_areas_same_area", () => mapper.mergeAreas(a, [c, c]));
await refuses("mixed_tiers", "merge_areas_mixed_tiers", () => mapper.mergeAreas(a, [s]));
await refuses("room_not_found", "merge_areas_room_not_found", () => mapper.mergeAreas(a, [{ area: c, rooms: [99] }]));
await refuses("no_rooms", "merge_areas_no_rooms", () => mapper.mergeAreas(a, [{ area: c, rooms: [] }]));

for (const value of [NaN, Infinity, -Infinity, 1.5, 2147483648, "1", null]) {
    await refuses("invalid level", "merge_areas_invalid_translation", () => mapper.mergeAreas(a, [{ area: c, translate: { level: value } }]));
    await refuses("invalid room", "merge_areas_invalid_rooms", () => mapper.mergeAreas(a, [{ area: c, rooms: [value] }]));
    check("invalid input preserves source", mapper.areas.some((area) => area.id === c.id), true);
}
await refuses("sparse rooms", "merge_areas_invalid_rooms", () => mapper.mergeAreas(a, [{ area: c, rooms: new Array(2) }]));

// A partial merge: only C.2 moves (A's floor is now 6), C stays with C.1.
const partial = await mapper.mergeAreas(a, [{ area: c, rooms: [c2], translate: { y: 20 } }]);
check(
    "partial/remap",
    partial.map((moved) => `${tag(moved.from.area)}${moved.from.room}>${moved.to}`).join(","),
    "C2>6",
);
check("partial/c-stays", mapper.areas.some((area) => area.id === c.id), true);
const gammaAfter = mapper.getAreaById(c.id);
check("partial/c1-kept", gammaAfter.room(c1) !== undefined, true);
check("partial/c2-gone", gammaAfter.room(c2) == null, true);
check("partial/c-next", gammaAfter.next_room_number, c2);
check("partial/c1-north", target(gammaAfter.room(c1)?.exits.find((exit) => exit.from_direction === "North")), "A6");
check("partial/c1-east", target(gammaAfter.room(c1)?.exits.find((exit) => exit.from_direction === "East")), "A5");
const alphaAfter = mapper.getAreaById(a.id);
check("partial/a6-south", target(alphaAfter.room(6)?.exits.find((exit) => exit.from_direction === "South")), "C1");
check("partial/a6-y", alphaAfter.room(6)?.y, 19);
check("partial/c2-lookup", `${tag(mapper.findRoomByExternalId("c2")?.area_id)}${mapper.findRoomByExternalId("c2")?.room_number}`, "A6");
check("partial/location", `${tag(mapper.getCurrentLocation()?.area)}${mapper.getCurrentLocation()?.room}`, "A5");

echo(failures.length === 0 ? "MERGE_OK" : `MERGE_FAIL ${failures.join(" | ")}`);
"#;

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

#[tokio::test]
async fn merge_areas_folds_sources_into_the_destination() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");

    let modules = smudgy_home.join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create modules directory");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).expect("create logs directory");
    std::fs::write(modules.join("merge.ts"), MERGE_TS).expect("write merge module");

    // A tiered mapper: the session tier exists only behind the composite, and
    // the mixed-tier refusal needs a source that really lives there. The cloud
    // tier is never reached.
    let map_root = smudgy_home.join("map-test");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_371_u32),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines = Vec::new();
    let mut locations = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let tx = loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("timed out waiting for the merge module");
        let event = tokio::time::timeout(remaining, events.next())
            .await
            .expect("timed out waiting for the merge module")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            SessionEvent::SetCurrentLocation(area_id, room) => locations.push((area_id, room)),
            _ => {}
        }
    };
    // The sentinel is echoed from the module; the `map:room` delivery for the
    // remapped location rides the action the op queued, so it can land after.
    while !(lines.iter().any(|line| line.starts_with("MERGE_"))
        && lines.iter().any(|line| line == "MAP_ROOM A 5"))
    {
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            break;
        };
        let Ok(Some(event)) = tokio::time::timeout(remaining, events.next()).await else {
            break;
        };
        match event.event {
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            SessionEvent::SetCurrentLocation(area_id, room) => locations.push((area_id, room)),
            _ => {}
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|line| line == "MERGE_OK"),
        "mergeAreas did not land as the script expects:\n{transcript}"
    );

    let atlas = mapper.get_current_atlas();
    let mut names: Vec<String> = atlas
        .areas()
        .map(|area| area.get_name().to_string())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["Alpha", "Gamma", "Scratch"],
        "the destination, the partially merged third party and the session area remain"
    );
    let alpha = atlas
        .areas()
        .find(|area| area.get_name() == "Alpha")
        .expect("the destination area");
    assert_eq!(
        alpha.get_rooms().len(),
        6,
        "A holds its own rooms, B's, and the one room taken from C"
    );
    let gamma = atlas
        .areas()
        .find(|area| area.get_name() == "Gamma")
        .expect("the partial source stays");
    assert_eq!(
        gamma.get_rooms().len(),
        1,
        "C keeps the room it did not give"
    );

    // The remapped location reached the UI channel and fired `map:room`, the
    // same two effects `setCurrentLocation` has.
    assert_eq!(
        locations.last().copied(),
        Some((*alpha.get_id(), Some(5))),
        "the session's current location must follow the moved room to A.5; saw {locations:?}"
    );
    assert!(
        lines.iter().any(|line| line == "MAP_ROOM A 5"),
        "map:room must fire for the remapped location:\n{transcript}"
    );
}
