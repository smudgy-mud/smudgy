//! End-to-end coverage of the first-party `auto-mapper` package
//! (`packages/auto-mapper/`, `docs/gmcp-mapping-plan.md` §5.3), installed **untrusted** so
//! it runs sandboxed to its manifest — a deliberate dogfood of the capability model
//! (interop:read + mapper:write + automations:aliases + session:echo + gmcp:send).
//!
//! The real package source is copied from the repo into the test server's local-package
//! override dir, a real `Mapper` (composite backend, in-memory + local tiers, dead cloud)
//! is attached to the session, and GMCP `Room.Info` messages drive it: walk two rooms
//! (auto-create in an ephemeral zone area, arrival exits linked both ways, stubs for
//! unexplored exits), revisit the first (follow, no duplicate), then `savemap` promotes
//! the session map to the local tier and drops the original.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use smudgy_core::models::local_packages::packages_dir;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{BufferUpdate, SessionEvent, SessionId, SessionParams, spawn};
use smudgy_cloud::{
    CloudMapper, CompositeBackend, Credential, CredentialSource, ExitStyle, LocalBackend, Mapper,
    MapperBackend, PackageApiClient, RoomNumber, RoomUpdates, mapper::RoomKey,
};
use std::collections::HashSet;

const QUIET_PERIOD: Duration = Duration::from_millis(900);
const SERVER: &str = "AutoMapperTest";

fn copy_package_source(server: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packages")
        .join("auto-mapper");
    let dest = packages_dir(server).expect("packages dir").join("auto-mapper");
    std::fs::create_dir_all(&dest).unwrap();
    for entry in std::fs::read_dir(&source).expect("read packages/auto-mapper") {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), dest.join(entry.file_name())).unwrap();
        }
    }
}

fn gmcp(name: &str, data: &str) -> RuntimeAction {
    RuntimeAction::GmcpMessage {
        name: Arc::from(name),
        data: Some(Arc::from(data)),
    }
}

fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

#[tokio::test]
async fn auto_mapper_maps_follows_and_promotes() {
    // ---- Home + package install (untrusted → sandboxed to its manifest). ----
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).unwrap();
    copy_package_source(SERVER);
    shared_packages::install_package(SERVER, "smudgy://local/auto-mapper", UpdateMode::Auto, true)
        .unwrap();
    // Main-isolate probe: distinguishes "the watch never fires" from "the sandboxed
    // package never sees it" when diagnosing failures.
    std::fs::write(
        smudgy_home.join(SERVER).join("modules").join("probe.ts"),
        "import gmcp from \"smudgy:state/gmcp\";\n\
         import { echo } from \"smudgy:core\";\n\
         gmcp.watch(\"Room.Info\", (info: any) => echo(\"PROBE_ROOM:\" + info?.num));\n",
    )
    .unwrap();

    // ---- A real mapper: local tier on temp disk, dead cloud, internal ephemeral tier. ----
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
        session_id: SessionId::from(9333_u32),
        server_name: Arc::new(SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: Some(PackageApiClient::new(
            "http://127.0.0.1:0",
            CredentialSource::new(Some(Credential::ApiKey("test".into()))),
        )),
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines: Vec<String> = Vec::new();
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

    // ---- Walk: Aardwolf-dialect Room.Info, east twice (three CONSECUTIVE creations),
    // then back to the start. Consecutive creations are the regression shape twice over:
    // (a) every room carries the IDENTICAL `coord` — on Aardwolf that object is the
    // zone's position on its continent map, not a per-room coordinate (the golden's
    // capture shows adjacent rooms sharing x:30,y:20), and placing by it stacks the zone
    // on one spot; (b) a cached `Area` handle is an immutable snapshot, so a chain of
    // creations that reads the previous room's position through a stale handle collapses
    // onto the origin cell. Either bug stacks rooms 101 and 103.
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 100, "name": "Temple Square", "zone": "midgaard", "terrain": "city",
             "exits": { "e": 101, "n": 102 },
             "coord": { "id": 0, "x": 30, "y": 20, "cont": 0 } }"#,
    ))
    .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 101, "name": "Market Street", "zone": "midgaard", "terrain": "city",
             "exits": { "w": 100, "e": 103 },
             "coord": { "id": 0, "x": 30, "y": 20, "cont": 0 } }"#,
    ))
    .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 103, "name": "East Gate", "zone": "midgaard", "terrain": "city",
             "exits": { "w": 101 },
             "coord": { "id": 0, "x": 30, "y": 20, "cont": 0 } }"#,
    ))
    .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 100, "name": "Temple Square", "zone": "midgaard", "terrain": "city",
             "exits": { "e": 101, "n": 102 },
             "coord": { "id": 0, "x": 30, "y": 20, "cont": 0 } }"#,
    ))
    .unwrap();

    // Drain until quiet so every async creation lands before asserting.
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    let transcript = lines.join("\n");

    // ---- The session map: one ephemeral zone area, two rooms, bound ids, linked exits. ----
    let atlas = mapper.get_current_atlas();
    let (key100, room100) = atlas
        .find_room_by_external_id("100")
        .unwrap_or_else(|| panic!("room 100 was auto-created.\n{transcript}"));
    let (key101, room101) = atlas
        .find_room_by_external_id("101")
        .unwrap_or_else(|| panic!("room 101 was auto-created.\n{transcript}"));
    let (key103, room103) = atlas
        .find_room_by_external_id("103")
        .unwrap_or_else(|| panic!("room 103 was auto-created.\n{transcript}"));
    assert_eq!(key100.area_id, key101.area_id, "one area per zone");
    assert_eq!(key100.area_id, key103.area_id, "one area per zone");
    assert!(
        mapper.is_ephemeral(&key100.area_id),
        "auto-mapped rooms land in the ephemeral tier"
    );
    let area = atlas.get_area(&key100.area_id).expect("zone area");
    assert_eq!(area.get_name(), "midgaard");
    assert_eq!(area.room_count(), 3, "revisiting room 100 must not duplicate it");
    assert_eq!(room100.get_title(), "Temple Square");
    assert_eq!(room101.get_title(), "Market Street");
    // No two rooms may stack: catches both trusting the zone-granular Aardwolf
    // coord and reading placement through a stale area-handle snapshot.
    let positions = [
        ("100", &room100),
        ("101", &room101),
        ("103", &room103),
    ];
    for (i, (name_a, a)) in positions.iter().enumerate() {
        for (name_b, b) in positions.iter().skip(i + 1) {
            assert!(
                (a.get_x() - b.get_x()).abs() > 0.5
                    || (a.get_y() - b.get_y()).abs() > 0.5
                    || a.get_level() != b.get_level(),
                "rooms {name_a} and {name_b} stack at {},{} — placement collapsed.\n{transcript}",
                a.get_x(),
                a.get_y()
            );
        }
    }

    // Arrival exit east 100→101 and its listed reverse west 101→100.
    let east = room100
        .get_exits()
        .iter()
        .find(|e| e.to_room_number == Some(key101.room_number))
        .expect("100 links east to 101");
    assert_eq!(east.command.as_deref(), Some("east"));
    // Exactly one edge per direction: the stub-upgrade path must not be doubled
    // by an extra arrival-exit creation.
    assert_eq!(
        room101
            .get_exits()
            .iter()
            .filter(|e| e.to_room_number == Some(key103.room_number))
            .count(),
        1,
        "one east edge 101→103, not a stub-upgrade duplicate"
    );
    assert!(
        room101
            .get_exits()
            .iter()
            .any(|e| e.to_room_number == Some(key100.room_number)),
        "101 links back west to 100"
    );
    // Unexplored exits become stubs: n:102 from room 100, e:103 from room 101.
    assert!(
        room100
            .get_exits()
            .iter()
            .any(|e| e.style == ExitStyle::Stub && e.to_room_number.is_none()),
        "room 100's unexplored north exit is a stub"
    );

    // ---- savemap: promote to the local tier, drop the session original. ----
    tx.send(RuntimeAction::Send(Arc::new("savemap".to_string()))).unwrap();
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    let transcript = lines.join("\n");
    assert!(
        lines.iter().any(|l| l.contains("saved 1 map(s)")),
        "savemap reports the promotion.\n{transcript}"
    );

    let atlas = mapper.get_current_atlas();
    let (promoted_key, promoted_room) = atlas
        .find_room_by_external_id("100")
        .expect("room 100 still resolves after promotion");
    assert_ne!(promoted_key.area_id, key100.area_id, "a fresh (imported) area");
    assert!(
        !mapper.is_ephemeral(&promoted_key.area_id),
        "the promoted area is no longer ephemeral"
    );
    assert_eq!(promoted_room.get_title(), "Temple Square");
    assert!(
        atlas.get_area(&key100.area_id).is_none(),
        "the session original was dropped"
    );

    // ---- Mapping continues into the promoted area (zone rebind). ----
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 102, "name": "North Road", "zone": "midgaard", "terrain": "road",
             "exits": { "s": 100 } }"#,
    ))
    .unwrap();
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    let transcript = lines.join("\n");
    let atlas = mapper.get_current_atlas();
    let (key102, _) = atlas
        .find_room_by_external_id("102")
        .unwrap_or_else(|| panic!("room 102 mapped after promotion.\n{transcript}"));
    assert_eq!(
        key102.area_id, promoted_key.area_id,
        "post-promotion rooms land in the promoted area"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// The MSDP half of the dual-protocol contract: the golden's composite `ROOM` table
/// (Luminari shape — string vnums, full-word directions, COORDS) creates rooms placed by
/// server coordinates in an ephemeral zone area.
#[tokio::test]
async fn auto_mapper_maps_msdp_composite_room() {
    const MSDP_SERVER: &str = "AutoMapperMsdp";
    // The process-global home may already be set by the sibling test; both share it.
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(MSDP_SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(MSDP_SERVER).join("logs")).unwrap();
    copy_package_source(MSDP_SERVER);
    shared_packages::install_package(
        MSDP_SERVER,
        "smudgy://local/auto-mapper",
        UpdateMode::Auto,
        true,
    )
    .unwrap();

    let map_root = smudgy_home.join("map-test-msdp");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9334_u32),
        server_name: Arc::new(MSDP_SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: Some(PackageApiClient::new(
            "http://127.0.0.1:0",
            CredentialSource::new(Some(Credential::ApiKey("test".into()))),
        )),
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines: Vec<String> = Vec::new();
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

    const VAR: u8 = 1;
    const VAL: u8 = 2;
    const TABLE_OPEN: u8 = 3;
    const TABLE_CLOSE: u8 = 4;
    let room_table: Vec<u8> = [
        &[VAR][..],
        b"ROOM",
        &[VAL, TABLE_OPEN, VAR],
        b"VNUM",
        &[VAL],
        b"14100",
        &[VAR],
        b"NAME",
        &[VAL],
        b"A Small Island Beach",
        &[VAR],
        b"AREA",
        &[VAL],
        b"Training Halls",
        &[VAR],
        b"TERRAIN",
        &[VAL],
        b"Desert",
        &[VAR],
        b"COORDS",
        &[VAL, TABLE_OPEN, VAR],
        b"X",
        &[VAL],
        b"4",
        &[VAR],
        b"Y",
        &[VAL],
        b"7",
        &[VAR],
        b"Z",
        &[VAL],
        b"0",
        &[TABLE_CLOSE, VAR],
        b"EXITS",
        &[VAL, TABLE_OPEN, VAR],
        b"east",
        &[VAL],
        b"14101",
        &[TABLE_CLOSE, TABLE_CLOSE],
    ]
    .concat();

    tx.send(RuntimeAction::MsdpEnabled).unwrap();
    tx.send(RuntimeAction::MsdpMessage {
        payload: Arc::from(room_table),
    })
    .unwrap();

    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        if let SessionEvent::UpdateBuffer(updates) = event.event {
            collect(&updates, &mut lines);
        }
    }
    let transcript = lines.join("\n");

    let atlas = mapper.get_current_atlas();
    let (key, room) = atlas
        .find_room_by_external_id("14100")
        .unwrap_or_else(|| panic!("MSDP room was auto-created.\n{transcript}"));
    assert!(mapper.is_ephemeral(&key.area_id));
    let area = atlas.get_area(&key.area_id).expect("zone area");
    assert_eq!(area.get_name(), "Training Halls");
    assert_eq!(room.get_title(), "A Small Island Beach");
    // Server coords place the room on the grid (GRID spacing = 2.0 in the package).
    assert!((room.get_x() - 8.0).abs() < f32::EPSILON, "x = 4 * GRID");
    assert!((room.get_y() - 14.0).abs() < f32::EPSILON, "y = 7 * GRID");
    // The unexplored east exit is a stub awaiting 14101.
    assert!(
        room.get_exits()
            .iter()
            .any(|e| e.style == ExitStyle::Stub && e.to_room_number.is_none()),
        "unexplored east exit is a stub.\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// Cross-entry rescue guard (map scoping plan §3): a room already mapped on a
/// *different* server entry (a scope-excluded area) must NOT be re-minted. When
/// GMCP reports such a room, the package consults `rescueRoomByExternalId`,
/// which raises the "show here too?" offer (a `SessionEvent::OfferMapRescue`)
/// and returns true — so the package returns without auto-creating a duplicate.
#[tokio::test]
async fn auto_mapper_defers_to_cross_entry_rescue() {
    const RESCUE_SERVER: &str = "AutoMapperRescue";
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(RESCUE_SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(RESCUE_SERVER).join("logs")).unwrap();
    copy_package_source(RESCUE_SERVER);
    shared_packages::install_package(
        RESCUE_SERVER,
        "smudgy://local/auto-mapper",
        UpdateMode::Auto,
        true,
    )
    .unwrap();

    let map_root = smudgy_home.join("map-test-rescue");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    // Stand in for "a map homed on another entry": an area holding a room bound
    // to external id "9500", then scope-excluded. Normal identification will no
    // longer resolve "9500" (it is another entry's map), but the rescue index
    // still holds it.
    let elsewhere = mapper
        .create_area_ephemeral("Other Server Map".to_string())
        .await
        .expect("create the stand-in area");
    mapper.upsert_room(
        RoomKey::new(elsewhere, RoomNumber(1)),
        RoomUpdates {
            title: Some("A Familiar Cell".to_string()),
            external_id: Some(Some("9500".to_string())),
            ..RoomUpdates::default()
        },
    );
    mapper.set_scope_exclusions(HashSet::new(), std::iter::once(elsewhere).collect());
    assert!(
        mapper
            .get_current_atlas()
            .find_room_by_external_id("9500")
            .is_none(),
        "the scope-excluded room is absent from normal identification"
    );
    let ephemeral_before = mapper.ephemeral_area_ids().len();

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9335_u32),
        server_name: Arc::new(RESCUE_SERVER.to_string()),
        profile_name: Arc::new("Test".to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: Some(PackageApiClient::new(
            "http://127.0.0.1:0",
            CredentialSource::new(Some(Credential::ApiKey("test".into()))),
        )),
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events = Box::pin(spawn(params));
    let mut lines: Vec<String> = Vec::new();
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

    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 9500, "name": "A Familiar Cell", "zone": "elsewhere-zone",
             "terrain": "inside", "exits": { "n": 9501 } }"#,
    ))
    .unwrap();

    // Drain until quiet, watching for the rescue offer.
    let mut rescue_offered = false;
    while let Ok(Some(event)) = tokio::time::timeout(QUIET_PERIOD, events.next()).await {
        match event.event {
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            SessionEvent::OfferMapRescue { .. } => rescue_offered = true,
            _ => {}
        }
    }
    let transcript = lines.join("\n");

    assert!(
        rescue_offered,
        "a room mapped on another entry raises the cross-entry rescue offer.\n{transcript}"
    );
    // No duplicate was minted: no new ephemeral zone area appeared, and "9500"
    // still resolves nowhere in normal identification.
    assert_eq!(
        mapper.ephemeral_area_ids().len(),
        ephemeral_before,
        "the rescue path must not auto-create a duplicate zone area.\n{transcript}"
    );
    assert!(
        mapper
            .get_current_atlas()
            .find_room_by_external_id("9500")
            .is_none(),
        "no duplicate room 9500 was minted into a participating area.\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}
