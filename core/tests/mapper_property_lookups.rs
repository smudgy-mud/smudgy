//! End-to-end coverage for the indexed property and tag lookups on `mapper`
//! and on `Area`, over a real local-backed mapper: the ops are registered, the
//! arguments marshal, and the results a script sees match what the atlas and
//! area caches were told.

use std::{sync::Arc, time::Duration};

use futures::StreamExt;
use smudgy_cloud::{LocalBackend, Mapper, MapperBackend};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};

const SERVER: &str = "MapperPropertyLookups";

const LOOKUPS_TS: &str = r#"
import { echo, mapper } from "smudgy:core";

const titles = (rooms) => rooms.map((room) => room.title).sort().join(",");
const names = (areas) => areas.map((area) => area.name).sort().join(",");

const failures = [];
const check = (label, actual, expected) => {
    if (actual !== expected) failures.push(`${label}: got [${actual}] want [${expected}]`);
};

const city = await mapper.createArea("City", { storage: "local" });
const wild = await mapper.createArea("Wild", { storage: "local" });

await mapper.setAreaProperty(city, "kind", "settled");
await mapper.setAreaProperty(city, "era", "old");
await mapper.setAreaProperty(wild, "kind", "untamed");

const gate = await mapper.createRoom(city, { title: "Gate", x: 0, y: 0, level: 0 });
const road = await mapper.createRoom(city, { title: "Road", x: 1, y: 0, level: 0 });
const trail = await mapper.createRoom(wild, { title: "Trail", x: 0, y: 0, level: 0 });

await mapper.setRoomProperty(city, gate, "zone", "midgaard");
await mapper.setRoomProperty(city, road, "zone", "midgaard");
await mapper.setRoomProperty(wild, trail, "zone", "outback");
// A per-room identity: the case a by-value index exists for.
await mapper.setRoomProperty(city, gate, "vnum", "3001");
await mapper.setRoomProperty(city, road, "vnum", "3002");

await mapper.addRoomTag(city, gate, "shop");
await mapper.addRoomTag(wild, trail, "SHOP");

// Atlas-wide, across both areas.
check("byProperty", titles(mapper.findRoomsByProperty("zone", "midgaard")), "Gate,Road");
check("byProperty/unique", titles(mapper.findRoomsByProperty("vnum", "3001")), "Gate");
check("withProperty", titles(mapper.findRoomsWithProperty("zone")), "Gate,Road,Trail");
check("withProperty/partial", titles(mapper.findRoomsWithProperty("vnum")), "Gate,Road");
// Tags match case-insensitively in both directions.
check("withTag", titles(mapper.findRoomsWithTag("shop")), "Gate,Trail");
check("withTag/case", titles(mapper.findRoomsWithTag("ShOp")), "Gate,Trail");
// Misses are empty, not thrown.
check("byProperty/miss", titles(mapper.findRoomsByProperty("zone", "nowhere")), "");
check("withProperty/miss", titles(mapper.findRoomsWithProperty("absent")), "");
check("withTag/miss", titles(mapper.findRoomsWithTag("absent")), "");

// Areas by their own properties.
check("areasByProperty", names(mapper.findAreasByProperty("kind", "settled")), "City");
check("areasWithProperty", names(mapper.findAreasWithProperty("kind")), "City,Wild");
check("areasWithProperty/one", names(mapper.findAreasWithProperty("era")), "City");
check("areasByProperty/miss", names(mapper.findAreasByProperty("kind", "nowhere")), "");
check("areasWithProperty/miss", names(mapper.findAreasWithProperty("absent")), "");

// Area-scoped: the same questions, answered for one area only. Re-read the
// handle so it carries the snapshot the writes above produced.
const cityNow = mapper.getAreaById(city.id);
const wildNow = mapper.getAreaById(wild.id);
check("area/byProperty", titles(cityNow.findRoomsByProperty("zone", "midgaard")), "Gate,Road");
check("area/byProperty/other", titles(wildNow.findRoomsByProperty("zone", "midgaard")), "");
check("area/withProperty", titles(cityNow.findRoomsWithProperty("vnum")), "Gate,Road");
check("area/withTag", titles(cityNow.findRoomsWithTag("ShOp")), "Gate");
check("area/withTag/other", titles(wildNow.findRoomsWithTag("shop")), "Trail");
check("area/withProperty/miss", titles(cityNow.findRoomsWithProperty("absent")), "");

// A rewritten property leaves no stale entry behind.
await mapper.setRoomProperty(city, road, "zone", "elsewhere");
check("rewrite/old", titles(mapper.findRoomsByProperty("zone", "midgaard")), "Gate");
check("rewrite/new", titles(mapper.findRoomsByProperty("zone", "elsewhere")), "Road");
check("rewrite/name", titles(mapper.findRoomsWithProperty("zone")), "Gate,Road,Trail");

// So does a removed tag.
await mapper.removeRoomTag(city, gate, "SHOP");
check("untag", titles(mapper.findRoomsWithTag("shop")), "Trail");

echo(failures.length === 0 ? "LOOKUPS_OK" : `LOOKUPS_FAIL ${failures.join(" | ")}`);
"#;

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

#[tokio::test]
async fn property_and_tag_lookups_answer_from_the_indexes() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");

    let modules = smudgy_home.join(SERVER).join("modules");
    std::fs::create_dir_all(&modules).expect("create modules directory");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).expect("create logs directory");
    std::fs::write(modules.join("lookups.ts"), LOOKUPS_TS).expect("write lookups module");

    let map_root = smudgy_home.join("map-test");
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(LocalBackend::new(map_root.join("local")));
    let mapper = Mapper::new(backend, map_root.join("cache"));
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9_362_u32),
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
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let tx = loop {
        let remaining = deadline
            .checked_duration_since(tokio::time::Instant::now())
            .expect("timed out waiting for the lookups module");
        let event = tokio::time::timeout(remaining, events.next())
            .await
            .expect("timed out waiting for the lookups module")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    while !lines.iter().any(|line| line.starts_with("LOOKUPS_")) {
        let Some(remaining) = deadline.checked_duration_since(tokio::time::Instant::now()) else {
            break;
        };
        let Ok(Some(event)) = tokio::time::timeout(remaining, events.next()).await else {
            break;
        };
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    tx.send(RuntimeAction::Shutdown).ok();

    let transcript = lines.join("\n");
    assert!(
        transcript.contains("LOOKUPS_OK"),
        "indexed lookups did not answer as expected:\n{transcript}"
    );
}
