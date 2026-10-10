//! The mapper script API's names are camelCase, and through Smudgy 0.5.x the
//! `snake_case` names earlier versions used keep working: what the API returns
//! answers to both (with only the camelCase fields enumerable), what it takes
//! accepts both, the first `snake_case` name a script uses draws exactly one
//! notice, and a refusal's message carries its code in both spellings. The door
//! flags exits once took (`isClosed`, `isLocked`) are refused in either spelling
//! by every exit writer, with an error naming `door`.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_cloud::{CloudMapper, CompositeBackend, LocalBackend, Mapper, MapperBackend};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const SERVER: &str = "MapperScriptNames";

const NAMES_TS: &str = r##"
import { echo, mapper } from "smudgy:core";

const failures = [];
const check = (label, actual, expected) => {
    if (actual !== expected) failures.push(`${label}: got [${actual}] want [${expected}]`);
};
const snakeKeys = (value) => Object.keys(value).filter((key) => key.includes("_")).join(",");

await mapper.ready();
const area = await mapper.createArea("Names", { storage: "session" });
const one = await mapper.createRoom(area, { title: "One" });
const two = await mapper.createRoom(area, { title: "Two", x: 1 });

// camelCase in and out first: nothing here may draw the notice.
const east = await mapper.createRoomExit(area, one, {
    fromDirection: "East", toAreaId: area.id, toRoomNumber: two, isHidden: true,
});
const camel = mapper.getAreaById(area.id);
const room = camel.room(one);
check("roomNumber", room.roomNumber, one);
check("areaId", room.areaId, area.id);
check("roomNumbers", camel.roomNumbers.join(","), `${one},${two}`);
check("nextRoomNumber", camel.nextRoomNumber, two + 1);
const exit = room.exits.find((candidate) => candidate.id === east);
check("toRoomNumber", exit?.toRoomNumber, two);
check("isHidden", exit?.isHidden, true);
check("fromDirection", exit?.fromDirection, "East");
check("exit keys", snakeKeys(exit), "");
echo("CAMEL_DONE");

// snake_case in: every input accepts the old names.
const west = await mapper.createRoomExit(area, two, {
    from_direction: "West", to_area_id: area.id, to_room_number: one, is_hidden: true,
});
await mapper.setRoomExit(area, two, west, { is_hidden: false, door: { state: "locked", opensWith: "turn key" } });
const labelId = await mapper.createLabel(area, {
    x: 0, y: 0, width: 4, height: 1, text: "Hall", background_color: "#112233", font_size: 20,
});
await mapper.setLabel(area, labelId, { font_weight: 700 });
const shapeId = await mapper.createShape(area, {
    x: 0, y: 0, width: 2, height: 2, shape_type: "RoundedRectangle", stroke_width: 3,
});
await mapper.mutateArea(area, async (m) => {
    await m.createLink({
        endpoint_a: { room_number: one, side: "North", port_offset: 0.5, port_mode: "AutoPinned" },
        endpoint_b: { room_number: two, side: "South", port_offset: 0.5, port_mode: "Manual" },
        segment_shape: "Orthogonal",
        traversals: [
            { room_number: one, from_direction: "North", to_area_id: area.id, to_room_number: two },
        ],
    });
});

// snake_case out: what the API returns answers to the old names.
const after = mapper.getAreaById(area.id);
const back = after.room(two);
check("room_number", back.room_number, two);
check("area_id", back.area_id, area.id);
check("room_numbers", after.room_numbers.join(","), `${one},${two}`);
check("next_room_number", after.next_room_number, two + 1);
const westExit = back.exits.find((candidate) => candidate.id === west);
check("to_room_number", westExit?.to_room_number, one);
check("toRoomNumber again", westExit?.toRoomNumber, one);
check("is_hidden", westExit?.is_hidden, false);
check("door", JSON.stringify(westExit?.door), '{"state":"locked","name":null,"opensWith":"turn key"}');
check("no door flags", ["isClosed", "isLocked", "is_closed", "is_locked"].filter((name) => name in westExit).join(","), "");
check("from_area_id", westExit?.from_area_id, area.id);
check("west keys", snakeKeys(westExit), "");
const label = after.labels.find((candidate) => candidate.id === labelId);
check("backgroundColor", label?.backgroundColor, "#112233");
check("background_color", label?.background_color, "#112233");
check("font_size", label?.fontSize, 20);
check("font_weight", label?.font_weight, 700);
check("label keys", snakeKeys(label), "");
check("label json", JSON.stringify(label).includes("_"), false);
const shape = after.shapes.find((candidate) => candidate.id === shapeId);
check("shapeType", shape?.shapeType, "RoundedRectangle");
check("shape_type", shape?.shape_type, "RoundedRectangle");
check("stroke_width", shape?.strokeWidth, 3);
const link = after.connections.find((candidate) => candidate.segmentShape === "Orthogonal");
check("endpointA.roomNumber", link?.endpointA.roomNumber, one);
check("endpoint_a.room_number", link?.endpoint_a.room_number, one);
check("endpointB.portMode", link?.endpointB?.portMode, "Manual");
check("route_points", Array.isArray(link?.route_points), true);
check("link keys", snakeKeys(link) + snakeKeys(link.endpointA), "");
check(
    "linked traversal",
    after.room(one).exits.some((candidate) => candidate.connectionId === link?.id && candidate.toRoomNumber === two),
    true,
);

// A refusal's code, in both spellings.
let refusal = "";
try { await mapper.mergeAreas(area, []); } catch (error) { refusal = String(error?.message ?? error); }
check("camelCase code", refusal.includes("mergeAreasNoSources"), true);
check("snake_case code", refusal.includes("merge_areas_no_sources"), true);
let invalid = "";
try { await mapper.mergeAreas(area, [{ area, rooms: [1.5] }]); } catch (error) { invalid = String(error?.message ?? error); }
check("script-side code", invalid.includes("mergeAreasInvalidRooms; formerly merge_areas_invalid_rooms"), true);

// Door flags, in either spelling, are refused by every exit writer with an error naming
// `door`, and nothing is written.
const exitsBefore = mapper.getAreaById(area.id).room(one).exits.length;
const doorFlagFailures = [];
const refusesFlag = async (label, flag, write) => {
    let message = "";
    try { await write(); } catch (error) { message = String(error?.message ?? error); }
    const named = message.includes(flag) && message.includes("door");
    if (!message.startsWith("Exits take no") || !named) doorFlagFailures.push(`${label}: [${message}]`);
};
await refusesFlag("createRoomExit", "isClosed", () =>
    mapper.createRoomExit(area, one, { fromDirection: "Up", isClosed: true }));
await refusesFlag("createRoomExit false", "isLocked", () =>
    mapper.createRoomExit(area, one, { fromDirection: "Up", isLocked: false }));
await refusesFlag("setRoomExit", "is_closed", () =>
    mapper.setRoomExit(area, two, west, { is_closed: true }));
await refusesFlag("mutateArea createRoomExit", "is_locked", () =>
    mapper.mutateArea(area, async (m) => {
        await m.createRoomExit(one, { fromDirection: "Down", is_locked: true });
    }));
await refusesFlag("mutateArea setRoomExit", "isLocked", () =>
    mapper.mutateArea(area, async (m) => {
        await m.setRoomExit(two, west, { isLocked: true });
    }));
const flaggedLink = (flag) => ({
    endpointA: { roomNumber: one, side: "East", portOffset: 0.5, portMode: "AutoPinned" },
    traversals: [{ roomNumber: one, fromDirection: "In", [flag]: true }],
});
await refusesFlag("createLink", "isClosed", () => mapper.createLink(area, flaggedLink("isClosed")));
await refusesFlag("mutateArea createLink", "is_closed", () =>
    mapper.mutateArea(area, async (m) => { await m.createLink(flaggedLink("is_closed")); }));
check("door flags refused", doorFlagFailures.join(" | "), "");
check("nothing written", mapper.getAreaById(area.id).room(one).exits.length, exitsBefore);
check(
    "door untouched",
    JSON.stringify(mapper.getAreaById(area.id).room(two).exits.find((candidate) => candidate.id === west)?.door),
    '{"state":"locked","name":null,"opensWith":"turn key"}',
);

echo(failures.length === 0 ? "NAMES_OK" : `NAMES_FAIL ${failures.join(" | ")}`);
"##;

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

#[tokio::test]
async fn camel_case_names_with_snake_case_aliases_through_0_5() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");

    let modules = smudgy_home.join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create modules directory");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).expect("create logs directory");
    std::fs::write(modules.join("names.ts"), NAMES_TS).expect("write the module");

    // The session tier lives behind the composite; the cloud tier is never reached.
    let map_root = smudgy_home.join("map-names");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_377_u32),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: None,
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines: Vec<String> = Vec::new();
    let mut runtime = None;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while !lines.iter().any(|line| line.starts_with("NAMES_")) {
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
    // The notice rides the action queue, so let it drain.
    while let Ok(Some(event)) =
        tokio::time::timeout(Duration::from_millis(250), events.next()).await
    {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    if let Some(tx) = runtime {
        tx.send(RuntimeAction::Shutdown).ok();
    }

    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|line| line == "NAMES_OK"),
        "the names misbehaved:\n{transcript}"
    );
    let notices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.starts_with("[mapper] A script used the snake_case mapper name"))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        notices.len(),
        1,
        "one notice however many snake_case names the script uses:\n{transcript}"
    );
    let camel_done = lines
        .iter()
        .position(|line| line == "CAMEL_DONE")
        .expect("the camelCase half finished");
    assert!(
        notices[0] > camel_done,
        "camelCase names draw no notice:\n{transcript}"
    );
    assert!(
        lines[notices[0]].contains("from_direction") && lines[notices[0]].contains("fromDirection"),
        "the notice names the first old name and its replacement: {}",
        lines[notices[0]]
    );
}
