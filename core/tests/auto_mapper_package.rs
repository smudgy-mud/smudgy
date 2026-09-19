//! End-to-end coverage of the first-party `auto-mapper` package
//! (`packages/auto-mapper/`, `docs/gmcp-mapping.md` §5.3), installed **untrusted** so
//! it runs sandboxed to its manifest — a deliberate dogfood of the capability model
//! (interop:read + mapper:write + automations:aliases + session:echo + gmcp:send).
//!
//! The real package source is copied from the repo into the test server's local-package
//! override dir, a real `Mapper` (composite backend, in-memory + local tiers, dead cloud)
//! is attached to the session, and GMCP `Room.Info` messages drive it: walk two rooms
//! (auto-create in a durable local zone area, arrival exits linked both ways, stubs for
//! unexplored exits), revisit the first (follow, no duplicate), and verify the map is
//! available to a fresh mapper on the next run.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use futures::{Stream, StreamExt};
use smudgy_cloud::{
    CloudMapper, CompositeBackend, Credential, CredentialSource, LocalBackend, MapDestination,
    MapStorage, Mapper, MapperBackend, PackageApiClient, RoomNumber, RoomUpdates, mapper::RoomKey,
};
use smudgy_core::models::local_packages::packages_dir;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, TaggedSessionEvent, spawn,
};

const COMPLETION_TIMEOUT: Duration = Duration::from_mins(1);
const COMPLETION_POLL_INTERVAL: Duration = Duration::from_millis(10);
const SERVER: &str = "AutoMapperTest";

fn copy_package(server: &str, name: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packages")
        .join(name);
    let dest = packages_dir(server).expect("packages dir").join(name);
    std::fs::create_dir_all(&dest).unwrap();
    for entry in std::fs::read_dir(&source).unwrap_or_else(|_| panic!("read package {name}")) {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), dest.join(entry.file_name())).unwrap();
        }
    }
}

fn copy_package_source(server: &str) {
    copy_package(server, "map-layout");
    copy_package(server, "auto-mapper");
    let directory = packages_dir(server)
        .expect("packages dir")
        .join("auto-mapper");
    for name in ["engine.ts", "smudgy.package.json"] {
        let path = directory.join(name);
        let source = std::fs::read_to_string(&path).expect("read auto-mapper dependency");
        std::fs::write(
            path,
            source.replace("smudgy://kapusniak/map-layout", "smudgy://local/map-layout"),
        )
        .expect("localize auto-mapper dependency");
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

async fn wait_until<S, F>(events: &mut S, lines: &mut Vec<String>, description: &str, ready: F)
where
    S: Stream<Item = TaggedSessionEvent> + Unpin,
    F: FnMut(&[String]) -> bool,
{
    wait_until_observing(events, lines, description, ready, |_| {}).await;
}

/// Wait for the operation's observable postcondition while continuing to collect session output.
/// Durable mapper work can legitimately produce no events for seconds, so event-stream silence is
/// never treated as completion.
async fn wait_until_observing<S, F, O>(
    events: &mut S,
    lines: &mut Vec<String>,
    description: &str,
    mut ready: F,
    mut observe: O,
) where
    S: Stream<Item = TaggedSessionEvent> + Unpin,
    F: FnMut(&[String]) -> bool,
    O: FnMut(&SessionEvent),
{
    let completed = tokio::time::timeout(COMPLETION_TIMEOUT, async {
        loop {
            if ready(lines) {
                return true;
            }
            tokio::select! {
                event = events.next() => {
                    let Some(event) = event else {
                        return false;
                    };
                    observe(&event.event);
                    if let SessionEvent::UpdateBuffer(updates) = &event.event {
                        collect(updates, lines);
                    }
                }
                () = tokio::time::sleep(COMPLETION_POLL_INTERVAL) => {}
            }
        }
    })
    .await;

    assert!(
        matches!(completed, Ok(true)),
        "timed out waiting for {description}.\n{}",
        lines.join("\n")
    );
}

/// Atlas predicates observe the mapper's journaled optimistic state. Once a final effect is
/// visible, drain its already-enqueued mutation before a test relies on durable backend state.
async fn drain_mutation_queue(mapper: &Mapper) {
    match mapper.wait_for_sync_completion(60).await {
        Ok(true) => {}
        Ok(false) => panic!("mutation queue still pending after 60 seconds"),
        Err(()) => panic!("mutation queue reported failed operations"),
    }
}

/// Wait for the package's final location event rather than the earlier optimistic room publish.
/// Matching the event's exact room key also ignores stale location events left in the stream.
async fn wait_until_current_room<S>(
    events: &mut S,
    lines: &mut Vec<String>,
    description: &str,
    mapper: &Mapper,
    external_id: &str,
) where
    S: Stream<Item = TaggedSessionEvent> + Unpin,
{
    let located = Cell::new(false);
    wait_until_observing(
        events,
        lines,
        description,
        |_| located.get(),
        |event| {
            let SessionEvent::SetCurrentLocation(area_id, Some(room_number)) = event else {
                return;
            };
            let atlas = mapper.get_current_atlas();
            let room_number = RoomNumber(*room_number);
            let is_requested_room = atlas.get_area(area_id).is_some_and(|area| {
                area.get_room(&room_number)
                    .is_some_and(|room| room.get_external_id() == Some(external_id))
            });
            located.set(is_requested_room);
        },
    )
    .await;
}

fn room_title_is(mapper: &Mapper, external_id: &str, title: &str) -> bool {
    mapper
        .get_current_atlas()
        .find_room_by_external_id(external_id)
        .is_some_and(|(_, room)| room.get_title() == title)
}

fn room_is_materialized(mapper: &Mapper, external_id: &str) -> bool {
    mapper
        .get_current_atlas()
        .find_room_by_external_id(external_id)
        .is_some_and(|(_, room)| room.get_property("unvisited") != Some("true"))
}

fn rooms_linked(mapper: &Mapper, from_id: &str, to_id: &str) -> bool {
    let atlas = mapper.get_current_atlas();
    let Some((_, from)) = atlas.find_room_by_external_id(from_id) else {
        return false;
    };
    let Some((to, _)) = atlas.find_room_by_external_id(to_id) else {
        return false;
    };
    from.get_exits().iter().any(|exit| {
        exit.to_area_id == Some(to.area_id) && exit.to_room_number == Some(to.room_number)
    })
}

fn rooms_reciprocally_linked(mapper: &Mapper, a: &str, b: &str) -> bool {
    rooms_linked(mapper, a, b) && rooms_linked(mapper, b, a)
}

fn room_has_exit_count(mapper: &Mapper, external_id: &str, count: usize) -> bool {
    mapper
        .get_current_atlas()
        .find_room_by_external_id(external_id)
        .is_some_and(|(_, room)| room.get_exits().len() == count)
}

#[tokio::test]
async fn auto_mapper_maps_follows_and_persists() {
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

    // ---- A real mapper: local tier on temp disk, dead cloud, internal session tier. ----
    let map_root = smudgy_home.join("map-test");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    // Seed an unrelated room into the cell immediately east of the first observed room.
    // map-layout should preserve the cardinal adjacency by reflowing this blocker; the
    // old collision nudge stretched the east link past it to x=2.
    let seeded_midgaard = mapper
        .create_area_at(
            "midgaard".to_string(),
            MapDestination::loose(MapStorage::Session),
        )
        .await
        .expect("create seeded session area");
    let blocker_number = RoomNumber(1);
    let blocker = mapper
        .upsert_room(
            RoomKey::new(seeded_midgaard, blocker_number),
            RoomUpdates {
                title: Some("Layout blocker".to_string()),
                x: Some(1.0),
                y: Some(0.0),
                ..RoomUpdates::default()
            },
        )
        .expect("seed layout blocker");
    if let Some(operation_id) = blocker.operation_id() {
        mapper
            .wait_for_mutation(operation_id)
            .await
            .expect("layout blocker acknowledged");
    }

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
    // onto the origin cell. Either bug stacks rooms 101 and 103. The three rooms also use
    // `num`, `vnum`, and `id` respectively; conflicting lower-priority fields prove the
    // generic identity precedence is num -> vnum -> id.
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 100, "vnum": "wrong-vnum-100", "id": "wrong-id-100",
             "name": "Temple Square", "zone": "midgaard", "terrain": "city",
             "exits": { "e": 101, "n": 102 },
             "coord": { "id": 0, "x": 30, "y": 20, "cont": 0 } }"#,
    ))
    .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "vnum": 101, "id": "wrong-id-101",
             "name": "Market Street", "zone": "midgaard", "terrain": "city",
             "exits": { "w": 100, "e": 103 },
             "coord": { "id": 0, "x": 30, "y": 20, "cont": 0 } }"#,
    ))
    .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "id": "103", "name": "East Gate", "zone": "midgaard", "terrain": "city",
             "exits": { "w": 101 },
             "coord": { "id": 0, "x": 30, "y": 20, "cont": 0 } }"#,
    ))
    .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 100, "vnum": "wrong-vnum-100", "id": "wrong-id-100",
             "name": "Temple Square Revisited", "zone": "midgaard", "terrain": "city",
             "exits": { "e": 101, "n": 102 },
             "coord": { "id": 0, "x": 30, "y": 20, "cont": 0 } }"#,
    ))
    .unwrap();

    // Wait until every queued creation and revisit has completed before asserting.
    wait_until(
        &mut events,
        &mut lines,
        "the initial walk and revisit",
        |_| {
            room_title_is(&mapper, "100", "Temple Square Revisited")
                && rooms_reciprocally_linked(&mapper, "100", "101")
                && rooms_reciprocally_linked(&mapper, "101", "103")
                && rooms_linked(&mapper, "100", "102")
        },
    )
    .await;
    let transcript = lines.join("\n");

    // ---- The durable map: one local zone area, bound ids, linked exits. ----
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
    assert_eq!(mapper.area_storage(&key100.area_id), MapStorage::Local);
    assert_ne!(
        key100.area_id, seeded_midgaard,
        "the legacy session area was promoted before auto-mapping"
    );
    assert_eq!(
        mapper.session_area_ids().len(),
        0,
        "auto-mapper must not leave an ephemeral map behind"
    );
    let area = atlas.get_area(&key100.area_id).expect("zone area");
    assert_eq!(area.get_name(), "midgaard");
    // 100, 101, 103 visited + the unvisited placeholder for 102 (every room the server
    // names is on the map), plus the seeded layout blocker. Revisiting room 100 must not
    // duplicate anything.
    assert_eq!(
        area.room_count(),
        5,
        "three visited rooms + the 102 placeholder + the seeded blocker"
    );
    assert_eq!(room100.get_title(), "Temple Square Revisited");
    assert_eq!(room101.get_title(), "Market Street");
    assert_eq!(room103.get_title(), "East Gate");
    assert!(
        (room101.get_x() - (room100.get_x() + 1.0)).abs() < f32::EPSILON
            && (room101.get_y() - room100.get_y()).abs() < f32::EPSILON,
        "map-layout keeps the east room adjacent instead of nudging it past the blocker"
    );
    let blocker = area
        .get_room(&blocker_number)
        .expect("seeded layout blocker remains in the area");
    assert!(
        (blocker.get_x() - room101.get_x()).abs() > 0.5
            || (blocker.get_y() - room101.get_y()).abs() > 0.5
            || blocker.get_level() != room101.get_level(),
        "map-layout reflows the blocker out of the new east room's cell"
    );
    // No two rooms may stack: catches both trusting the zone-granular Aardwolf
    // coord and reading placement through a stale area-handle snapshot.
    let positions = [("100", &room100), ("101", &room101), ("103", &room103)];
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
    let exits_101 = room101.get_exits();
    let west_back = exits_101
        .iter()
        .find(|e| e.to_room_number == Some(key100.room_number))
        .unwrap_or_else(|| panic!("101 links back west to 100.\n{transcript}"));
    // Reconciliation: the two reciprocal traversals share ONE Connection (the
    // stub-upgrade recreates the exit so the host's auto-pair folds it onto the
    // arrival exit's Connection) — the map draws a single two-way link, not two
    // parallel one-way lines.
    assert_eq!(
        east.connection_id, west_back.connection_id,
        "100<->101 reciprocal exits pair onto one Connection.\n{transcript}"
    );
    let east_103 = exits_101
        .iter()
        .find(|e| e.to_room_number == Some(key103.room_number))
        .expect("101 links east to 103");
    let exits_103 = room103.get_exits();
    let west_101 = exits_103
        .iter()
        .find(|e| e.to_room_number == Some(key101.room_number))
        .unwrap_or_else(|| panic!("103 links back west to 101.\n{transcript}"));
    assert_eq!(
        east_103.connection_id, west_101.connection_id,
        "101<->103 reciprocal exits pair onto one Connection.\n{transcript}"
    );
    // Unexplored-but-named neighbors exist as unvisited placeholders: 100's north exit
    // links to a real room bound to id 102 and marked unvisited (the Mudlet pattern).
    let (key102, room102) = atlas
        .find_room_by_external_id("102")
        .unwrap_or_else(|| panic!("neighbor 102 exists as a placeholder.\n{transcript}"));
    assert_eq!(
        key102.area_id, key100.area_id,
        "the placeholder joins the zone area"
    );
    assert_eq!(
        room102.get_property("unvisited"),
        Some("true"),
        "the 102 placeholder is marked unvisited.\n{transcript}"
    );
    assert!(
        room100
            .get_exits()
            .iter()
            .any(|e| e.to_room_number == Some(key102.room_number)),
        "100's north exit links to the 102 placeholder.\n{transcript}"
    );

    // ---- Mapping continues directly in the durable local area. ----
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 102, "name": "North Road", "zone": "midgaard", "terrain": "road",
             "exits": { "s": 100 } }"#,
    ))
    .unwrap();
    wait_until(
        &mut events,
        &mut lines,
        "room 102 to materialize and link back to room 100",
        |_| {
            room_is_materialized(&mapper, "102") && rooms_reciprocally_linked(&mapper, "100", "102")
        },
    )
    .await;
    // The predicate above becomes true when the final mutation is optimistically published.
    // Confirm it reached the local backend before shutting this mapper down and reloading it.
    drain_mutation_queue(&mapper).await;
    let transcript = lines.join("\n");
    let atlas = mapper.get_current_atlas();
    let (key102, _) = atlas
        .find_room_by_external_id("102")
        .unwrap_or_else(|| panic!("room 102 mapped in the durable area.\n{transcript}"));
    assert_eq!(
        key102.area_id, key100.area_id,
        "new rooms stay in the durable zone area"
    );
    // Discovering 102 upgrades the waiting north placeholder to a real room, and the
    // reciprocal exits pair onto one Connection.
    let (_, durable_100) = atlas
        .find_room_by_external_id("100")
        .expect("room 100 resolves in the durable area");
    let exits_100 = durable_100.get_exits();
    let north = exits_100
        .iter()
        .find(|e| e.to_room_number == Some(key102.room_number))
        .unwrap_or_else(|| panic!("100's north placeholder links to 102.\n{transcript}"));
    let (_, room102) = atlas.find_room_by_external_id("102").expect("room 102");
    let exits_102 = room102.get_exits();
    let south = exits_102
        .iter()
        .find(|e| e.to_room_number == Some(key100.room_number))
        .unwrap_or_else(|| panic!("102 links south back to 100.\n{transcript}"));
    assert_eq!(
        north.connection_id, south.connection_id,
        "100<->102 reciprocal exits pair onto one Connection.\n{transcript}"
    );
    // The placeholder materialized on its first real visit.
    assert_ne!(
        room102.get_property("unvisited"),
        Some("true"),
        "visiting 102 cleared its unvisited marker.\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
    drop(events);
    drop(mapper);

    // ---- A NEW RUN: fresh Mapper over the same on-disk local tier, fresh session.
    // The saved "midgaard" map must be ADOPTED, not redrawn: a known room follows into
    // it, a new room is created inside it, and no session area appears.
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache2"));

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9338_u32),
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
            .expect("timed out waiting for RuntimeReady (second run)")
            .expect("event stream ended before RuntimeReady (second run)");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };

    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    // Revisit a known room, then step somewhere new in the same zone.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 102, "name": "North Road", "zone": "midgaard", "terrain": "road",
             "exits": { "s": 100, "n": 105 } }"#,
    ))
    .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 105, "name": "North Gate", "zone": "midgaard", "terrain": "city",
             "exits": { "s": 102 } }"#,
    ))
    .unwrap();

    wait_until(
        &mut events,
        &mut lines,
        "the saved area to be adopted and room 105 to link to room 102",
        |_| rooms_reciprocally_linked(&mapper, "102", "105"),
    )
    .await;
    let transcript = lines.join("\n");

    let atlas = mapper.get_current_atlas();
    let (key102b, _) = atlas
        .find_room_by_external_id("102")
        .unwrap_or_else(|| panic!("saved room 102 resolves in the new run.\n{transcript}"));
    let (key105, _) = atlas
        .find_room_by_external_id("105")
        .unwrap_or_else(|| panic!("room 105 was auto-created in the new run.\n{transcript}"));
    assert_eq!(
        key105.area_id, key102b.area_id,
        "the new room joins the SAVED midgaard map (adopted by name).\n{transcript}"
    );
    assert!(
        mapper.area_storage(&key105.area_id) != MapStorage::Session,
        "the adopted area is the saved local map, not a session copy.\n{transcript}"
    );
    assert_eq!(
        mapper.session_area_ids().len(),
        0,
        "no duplicate session area is minted for a saved zone.\n{transcript}"
    );
    assert_eq!(
        atlas
            .get_area(&key105.area_id)
            .expect("midgaard")
            .get_name(),
        "midgaard"
    );
    // Revisit reconciliation: run 1 saved room 102 with only its south exit; run 2's
    // fix advertises n:105, so the revisit adds the stub, 105's discovery upgrades it,
    // and the reciprocal traversals pair onto one Connection.
    let (_, room102b) = atlas.find_room_by_external_id("102").expect("room 102");
    let (_, room105b) = atlas.find_room_by_external_id("105").expect("room 105");
    let exits_102b = room102b.get_exits();
    let north_105 = exits_102b
        .iter()
        .find(|e| e.to_room_number == Some(key105.room_number))
        .unwrap_or_else(|| {
            panic!("revisit reconciliation adds 102's newly advertised north exit.\n{transcript}")
        });
    let exits_105b = room105b.get_exits();
    let south_102 = exits_105b
        .iter()
        .find(|e| e.to_room_number == Some(key102b.room_number))
        .unwrap_or_else(|| panic!("105 links south back to 102.\n{transcript}"));
    assert_eq!(
        north_105.connection_id, south_102.connection_id,
        "the revisit-added exit pairs onto one Connection.\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// The zone-crossing walk: entering a new zone opens exactly one new durable local area,
/// exits across the border link area-to-area (the pending stub upgrades into the
/// other area), and coming BACK to the first zone — both revisiting a known room and
/// discovering a new one — reuses the original zone area instead of minting
/// "old-town (2)". Terrain lands as the room's `terrain` property and its color wash.
#[tokio::test]
async fn auto_mapper_crosses_zones_and_returns_without_duplicates() {
    const ZONE_SERVER: &str = "AutoMapperZones";
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(ZONE_SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(ZONE_SERVER).join("logs")).unwrap();
    copy_package_source(ZONE_SERVER);
    shared_packages::install_package(
        ZONE_SERVER,
        "smudgy://local/auto-mapper",
        UpdateMode::Auto,
        true,
    )
    .unwrap();

    let map_root = smudgy_home.join("map-test-zones");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9336_u32),
        server_name: Arc::new(ZONE_SERVER.to_string()),
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

    // ---- The walk: two rooms in old-town, east across the border into wildwood,
    // back west to the known rooms, then north into a NEW old-town room. Room 200's
    // north exit and room 201's east exit start as stubs naming ids seen later, so
    // both cross the stub-upgrade path — one within the zone, one across zones.
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    for payload in [
        r#"{ "num": 200, "name": "West Plaza", "zone": "old-town", "terrain": "city",
             "exits": { "e": 201, "n": 202 } }"#,
        r#"{ "num": 201, "name": "East Plaza", "zone": "old-town", "terrain": "city",
             "exits": { "w": 200, "e": 300 } }"#,
        r#"{ "num": 300, "name": "Forest Edge", "zone": "wildwood", "terrain": "forest",
             "exits": { "w": 201 } }"#,
        r#"{ "num": 201, "name": "East Plaza", "zone": "old-town", "terrain": "city",
             "exits": { "w": 200, "e": 300 } }"#,
        r#"{ "num": 200, "name": "West Plaza", "zone": "old-town", "terrain": "city",
             "exits": { "e": 201, "n": 202 } }"#,
        r#"{ "num": 202, "name": "North Lane", "zone": "old-town", "terrain": "road",
             "exits": { "s": 200 } }"#,
    ] {
        tx.send(gmcp("Room.Info", payload)).unwrap();
    }

    wait_until(
        &mut events,
        &mut lines,
        "the zone-crossing walk to link rooms 200, 201, 300, and 202",
        |_| {
            rooms_reciprocally_linked(&mapper, "200", "201")
                && rooms_reciprocally_linked(&mapper, "201", "300")
                && rooms_reciprocally_linked(&mapper, "200", "202")
        },
    )
    .await;
    let transcript = lines.join("\n");

    let atlas = mapper.get_current_atlas();
    let (key200, room200) = atlas
        .find_room_by_external_id("200")
        .unwrap_or_else(|| panic!("room 200 was auto-created.\n{transcript}"));
    let (key201, room201) = atlas
        .find_room_by_external_id("201")
        .unwrap_or_else(|| panic!("room 201 was auto-created.\n{transcript}"));
    let (key300, room300) = atlas
        .find_room_by_external_id("300")
        .unwrap_or_else(|| panic!("room 300 was auto-created.\n{transcript}"));
    let (key202, _room202) = atlas
        .find_room_by_external_id("202")
        .unwrap_or_else(|| panic!("room 202 was auto-created after the return.\n{transcript}"));

    // ---- Crossing: one new area for wildwood, and only one.
    assert_eq!(
        key200.area_id, key201.area_id,
        "old-town rooms share one area"
    );
    assert_ne!(
        key300.area_id, key200.area_id,
        "crossing zones opens a separate area"
    );
    assert_eq!(
        atlas
            .get_area(&key300.area_id)
            .expect("wildwood area")
            .get_name(),
        "wildwood"
    );

    // ---- Return: the known rooms were followed and the NEW room 202 landed in the
    // ORIGINAL old-town area — no second area for a revisited zone.
    assert_eq!(
        key202.area_id, key200.area_id,
        "a new room discovered after returning joins the original zone area.\n{transcript}"
    );
    assert_eq!(
        mapper.session_area_ids().len(),
        0,
        "old-town and wildwood must not use session storage.\n{transcript}"
    );
    assert_eq!(mapper.area_storage(&key200.area_id), MapStorage::Local);
    assert_eq!(mapper.area_storage(&key300.area_id), MapStorage::Local);
    assert_eq!(
        atlas
            .get_area(&key200.area_id)
            .expect("old-town area")
            .room_count(),
        3,
        "old-town holds rooms 200, 201, 202 — revisits created nothing.\n{transcript}"
    );

    // ---- The border exits link area-to-area. 201's east stub (minted while 300 was
    // unseen) upgraded into wildwood; 300's west exit linked back on arrival.
    // Room numbers are per-area (200 and 300 are both room 1 of their areas), so
    // every exit match must qualify by (area, room).
    let border_east = room201
        .get_exits()
        .iter()
        .find(|e| {
            e.to_area_id == Some(key300.area_id) && e.to_room_number == Some(key300.room_number)
        })
        .unwrap_or_else(|| panic!("201 links east into wildwood.\n{transcript}"));
    assert!(
        border_east.to_room_number.is_some(),
        "the border stub was upgraded to a real link"
    );
    let border_west = room300
        .get_exits()
        .iter()
        .find(|e| {
            e.to_area_id == Some(key201.area_id) && e.to_room_number == Some(key201.room_number)
        })
        .unwrap_or_else(|| panic!("300 links west back to 201.\n{transcript}"));
    assert!(border_west.to_room_number.is_some());

    // ---- Return discovery upgrades the waiting stub within the zone too: 200's
    // north exit now really points at 202.
    let north = room200
        .get_exits()
        .iter()
        .find(|e| {
            e.to_area_id == Some(key202.area_id) && e.to_room_number == Some(key202.room_number)
        })
        .unwrap_or_else(|| panic!("200's north stub upgraded to link 202.\n{transcript}"));
    assert!(north.to_room_number.is_some());

    // ---- Terrain: recorded as the room property and as the color wash.
    assert_eq!(room200.get_property("terrain"), Some("city"));
    assert_eq!(room300.get_property("terrain"), Some("forest"));
    assert_eq!(room200.get_color(), "#8a8a8a", "city wash");
    assert_eq!(room300.get_color(), "#3a7a3a", "forest wash");

    // ---- The identity-withheld sentinel (Aardwolf mazes send num: -1): never map it.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": -1, "name": "A Twisty Maze", "zone": "old-town", "terrain": "city",
             "exits": { "n": -1 } }"#,
    ))
    .unwrap();
    // A following known-room update is an ordered marker proving the preceding unmappable
    // report was consumed without drawing anything.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 200, "name": "West Plaza After Maze", "zone": "old-town",
             "terrain": "city", "exits": { "e": 201, "n": 202 } }"#,
    ))
    .unwrap();
    wait_until(
        &mut events,
        &mut lines,
        "the post-maze known-room marker",
        |_| room_title_is(&mapper, "200", "West Plaza After Maze"),
    )
    .await;
    let transcript = lines.join("\n");
    let atlas = mapper.get_current_atlas();
    assert!(
        atlas.find_room_by_external_id("-1").is_none(),
        "the -1 sentinel must not be minted as a room.\n{transcript}"
    );
    assert_eq!(
        mapper.session_area_ids().len(),
        0,
        "an unmappable fix opens no area.\n{transcript}"
    );
    assert_eq!(
        atlas
            .get_area(&key200.area_id)
            .expect("old-town area")
            .room_count(),
        3,
        "an unmappable fix adds no room.\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// The IRE dialect (Achaea-shape `Room.Info`): `area`/`environment` naming and the
/// `"area,x,y"` coords string place rooms by server coordinates. The adapter had no
/// end-to-end coverage — the goldens are Aardwolf (GMCP) and Luminari (MSDP).
#[tokio::test]
async fn auto_mapper_maps_ire_dialect_with_server_coords() {
    const IRE_SERVER: &str = "AutoMapperIre";
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(IRE_SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(IRE_SERVER).join("logs")).unwrap();
    copy_package_source(IRE_SERVER);
    shared_packages::install_package(
        IRE_SERVER,
        "smudgy://local/auto-mapper",
        UpdateMode::Auto,
        true,
    )
    .unwrap();

    let map_root = smudgy_home.join("map-test-ire");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9337_u32),
        server_name: Arc::new(IRE_SERVER.to_string()),
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
        r#"{ "num": 4711, "name": "Centre of the crossroads",
             "area": "the village of Tasur'ke", "environment": "road",
             "coords": "77,5,7,2", "exits": { "n": 4712 } }"#,
    ))
    .unwrap();

    wait_until(
        &mut events,
        &mut lines,
        "the IRE room and its placeholder exit",
        |_| rooms_linked(&mapper, "4711", "4712"),
    )
    .await;
    let transcript = lines.join("\n");

    let atlas = mapper.get_current_atlas();
    let (key, room) = atlas
        .find_room_by_external_id("4711")
        .unwrap_or_else(|| panic!("IRE room was auto-created.\n{transcript}"));
    assert_eq!(mapper.area_storage(&key.area_id), MapStorage::Local);
    assert_eq!(
        atlas.get_area(&key.area_id).expect("area").get_name(),
        "the village of Tasur'ke",
        "IRE's `area` field names the zone"
    );
    // "area,x,y[,level]" server coords place the room on the grid (GRID spacing = 1.0,
    // matching the map's one-unit-per-room pitch); the 4th slot is the floor, so
    // multi-level IRE areas separate by z instead of stacking on one plane.
    assert!((room.get_x() - 5.0).abs() < f32::EPSILON, "x = 5 * GRID");
    assert!((room.get_y() - 7.0).abs() < f32::EPSILON, "y = 7 * GRID");
    assert_eq!(room.get_level(), 2, "the coords 4th slot is the level");
    // `environment` is the IRE spelling of terrain: property + color wash.
    assert_eq!(room.get_property("terrain"), Some("road"));
    assert_eq!(room.get_color(), "#b09a6a", "road wash");
    // The named neighbor 4712 exists as an unvisited placeholder, linked from 4711.
    let (key4712, room4712) = atlas
        .find_room_by_external_id("4712")
        .unwrap_or_else(|| panic!("neighbor 4712 exists as a placeholder.\n{transcript}"));
    assert_eq!(
        room4712.get_property("unvisited"),
        Some("true"),
        "the 4712 placeholder is marked unvisited.\n{transcript}"
    );
    assert!(
        room.get_exits()
            .iter()
            .any(|e| e.to_room_number == Some(key4712.room_number)),
        "4711's north exit links to the placeholder.\n{transcript}"
    );

    // ---- Visiting the placeholder materializes it: server coords re-place it (they
    // beat the direction-offset guess), the marker clears, and the reciprocal exits
    // pair onto one Connection.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 4712, "name": "North of the crossroads",
             "area": "the village of Tasur'ke", "environment": "road",
             "coords": "77,5,8,2", "exits": { "s": 4711 } }"#,
    ))
    .unwrap();
    wait_until(
        &mut events,
        &mut lines,
        "IRE room 4712 to materialize and link back to 4711",
        |_| {
            room_is_materialized(&mapper, "4712")
                && rooms_reciprocally_linked(&mapper, "4711", "4712")
        },
    )
    .await;
    let transcript = lines.join("\n");
    let atlas = mapper.get_current_atlas();
    let (key4712b, room4712b) = atlas
        .find_room_by_external_id("4712")
        .unwrap_or_else(|| panic!("4712 still resolves after materialization.\n{transcript}"));
    assert_ne!(
        room4712b.get_property("unvisited"),
        Some("true"),
        "visiting 4712 cleared its unvisited marker.\n{transcript}"
    );
    assert!(
        (room4712b.get_x() - 5.0).abs() < f32::EPSILON
            && (room4712b.get_y() - 8.0).abs() < f32::EPSILON,
        "materialization re-placed 4712 by its server coords (got {},{}).\n{transcript}",
        room4712b.get_x(),
        room4712b.get_y()
    );
    assert_eq!(room4712b.get_level(), 2);
    assert_eq!(room4712b.get_title(), "North of the crossroads");
    let (_, room4711b) = atlas.find_room_by_external_id("4711").expect("room 4711");
    let exits_4711 = room4711b.get_exits();
    let north_4712 = exits_4711
        .iter()
        .find(|e| e.to_room_number == Some(key4712b.room_number))
        .unwrap_or_else(|| panic!("4711 still links north to 4712.\n{transcript}"));
    let exits_4712 = room4712b.get_exits();
    let south_4711 = exits_4712
        .iter()
        .find(|e| e.to_room_number == Some(key.room_number))
        .unwrap_or_else(|| panic!("4712 links south back to 4711.\n{transcript}"));
    assert_eq!(
        north_4712.connection_id, south_4711.connection_id,
        "materialized reciprocal exits pair onto one Connection.\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// The MSDP half of the dual-protocol contract: the golden's composite `ROOM` table
/// (Luminari shape — string vnums, full-word directions, COORDS) creates rooms placed by
/// server coordinates in a durable local zone area.
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

    wait_until(
        &mut events,
        &mut lines,
        "the MSDP room and its placeholder exit",
        |_| rooms_linked(&mapper, "14100", "14101"),
    )
    .await;
    let transcript = lines.join("\n");

    let atlas = mapper.get_current_atlas();
    let (key, room) = atlas
        .find_room_by_external_id("14100")
        .unwrap_or_else(|| panic!("MSDP room was auto-created.\n{transcript}"));
    assert_eq!(mapper.area_storage(&key.area_id), MapStorage::Local);
    let area = atlas.get_area(&key.area_id).expect("zone area");
    assert_eq!(area.get_name(), "Training Halls");
    assert_eq!(room.get_title(), "A Small Island Beach");
    // Server coords place the room on the grid (GRID spacing = 1.0 in the package).
    assert!((room.get_x() - 4.0).abs() < f32::EPSILON, "x = 4 * GRID");
    assert!((room.get_y() - 7.0).abs() < f32::EPSILON, "y = 7 * GRID");
    // The named neighbor 14101 exists as an unvisited placeholder, linked from 14100.
    let (key14101, room14101) = atlas
        .find_room_by_external_id("14101")
        .unwrap_or_else(|| panic!("neighbor 14101 exists as a placeholder.\n{transcript}"));
    assert_eq!(
        room14101.get_property("unvisited"),
        Some("true"),
        "the 14101 placeholder is marked unvisited.\n{transcript}"
    );
    assert!(
        room.get_exits()
            .iter()
            .any(|e| e.to_room_number == Some(key14101.room_number)),
        "14100's east exit links to the placeholder.\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// The id-less dialect (LP-family GMCP: room ids present, exit destinations withheld):
/// the observed movement command is the only evidence connecting consecutive rooms.
/// The package watches `sys:send` for direction tokens; a walk east must place the new
/// room east of the previous one AND link both traversals onto one Connection —
/// generic_mapper's consume-the-stub rule. Also covers the adapter hardening bounds
/// (giant ids withheld, absurd coords ignored) and the opt-in `mapprune` sweep.
#[tokio::test]
async fn auto_mapper_maps_idless_exits_by_movement() {
    const MOVE_SERVER: &str = "AutoMapperMoves";
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(MOVE_SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(MOVE_SERVER).join("logs")).unwrap();
    copy_package_source(MOVE_SERVER);
    shared_packages::install_package(
        MOVE_SERVER,
        "smudgy://local/auto-mapper",
        UpdateMode::Auto,
        true,
    )
    .unwrap();

    let map_root = smudgy_home.join("map-test-moves");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9339_u32),
        server_name: Arc::new(MOVE_SERVER.to_string()),
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
    // RuntimeReady guarantees the package's subscriptions are registered; the first room's
    // observable postcondition also waits through the maps-loaded barrier. Each subsequent step
    // completes before the next command goes out, matching human/network interleaving.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 900, "name": "Trail Head", "zone": "trailfields", "exits": { "e": "" } }"#,
    ))
    .unwrap();
    wait_until(
        &mut events,
        &mut lines,
        "room 900 and its destination-less east stub",
        |_| room_has_exit_count(&mapper, "900", 1),
    )
    .await;
    // Walk east — the sent command is observed via sys:send and attributed to the next fix.
    tx.send(RuntimeAction::Send(Arc::new("e".to_string())))
        .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 901, "name": "Open Trail", "zone": "trailfields", "exits": { "w": "", "e": "" } }"#,
    ))
    .unwrap();
    wait_until(
        &mut events,
        &mut lines,
        "movement evidence to link rooms 900 and 901",
        |_| rooms_reciprocally_linked(&mapper, "900", "901"),
    )
    .await;
    // Walk back west into known terrain: follow only, nothing new minted.
    tx.send(RuntimeAction::Send(Arc::new("w".to_string())))
        .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 900, "name": "Trail Head Revisited", "zone": "trailfields",
             "exits": { "e": "" } }"#,
    ))
    .unwrap();

    wait_until(
        &mut events,
        &mut lines,
        "the return to known room 900",
        |_| room_title_is(&mapper, "900", "Trail Head Revisited"),
    )
    .await;
    let transcript = lines.join("\n");

    let atlas = mapper.get_current_atlas();
    let (key900, room900) = atlas
        .find_room_by_external_id("900")
        .unwrap_or_else(|| panic!("room 900 was auto-created.\n{transcript}"));
    let (key901, room901) = atlas
        .find_room_by_external_id("901")
        .unwrap_or_else(|| panic!("room 901 was auto-created.\n{transcript}"));
    assert_eq!(key900.area_id, key901.area_id, "one area for the zone");
    // Placement followed the walked direction: 901 sits one unit east of 900.
    assert!(
        (room901.get_x() - (room900.get_x() + 1.0)).abs() < 0.01
            && (room901.get_y() - room900.get_y()).abs() < 0.01,
        "901 placed east of 900 by the observed command (got {},{} from {},{}).\n{transcript}",
        room901.get_x(),
        room901.get_y(),
        room900.get_x(),
        room900.get_y()
    );
    // Both traversals exist and share ONE Connection: the walk proved 900-e->901, and
    // 901's advertised (destination-less) west stub was consumed for the reverse.
    let exits_900 = room900.get_exits();
    let east = exits_900
        .iter()
        .find(|e| e.to_room_number == Some(key901.room_number))
        .unwrap_or_else(|| panic!("900 links east to 901 by movement evidence.\n{transcript}"));
    let exits_901 = room901.get_exits();
    let west = exits_901
        .iter()
        .find(|e| e.to_room_number == Some(key900.room_number))
        .unwrap_or_else(|| panic!("901's west stub was consumed to link back.\n{transcript}"));
    assert_eq!(
        east.connection_id, west.connection_id,
        "movement-linked traversals pair onto one Connection.\n{transcript}"
    );
    // The unexplored east exit of 901 stays a dangling stub; walking back minted nothing.
    assert!(exits_901.iter().any(|e| e.to_room_number.is_none()));
    assert_eq!(
        atlas
            .get_area(&key900.area_id)
            .expect("zone area")
            .room_count(),
        2,
        "the return walk re-used the mapped rooms.\n{transcript}"
    );
    assert_eq!(
        exits_900.len(),
        1,
        "no duplicate east exit from the re-walk.\n{transcript}"
    );

    // A refused north move produces no room report, then east reaches a new room. With
    // two unresolved commands and no destination ids, attribution is ambiguous: the
    // mapper may create/follow room 903, but must not invent 900 --north--> 903.
    tx.send(RuntimeAction::Send(Arc::new("n".to_string())))
        .unwrap();
    tx.send(RuntimeAction::Send(Arc::new("e".to_string())))
        .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 903, "name": "Ambiguous Trail", "zone": "trailfields", "exits": {} }"#,
    ))
    .unwrap();
    wait_until_current_room(
        &mut events,
        &mut lines,
        "ambiguous-arrival room 903",
        &mapper,
        "903",
    )
    .await;
    let transcript = lines.join("\n");
    let atlas = mapper.get_current_atlas();
    let (key903, _) = atlas
        .find_room_by_external_id("903")
        .unwrap_or_else(|| panic!("ambiguous arrival is still mapped.\n{transcript}"));
    let (_, room900_after_failure) = atlas.find_room_by_external_id("900").expect("room 900");
    assert!(
        !room900_after_failure
            .get_exits()
            .iter()
            .any(|exit| exit.to_room_number == Some(key903.room_number)),
        "ambiguous movement must not turn the failed north command into an exit.\n{transcript}"
    );

    // ---- Adapter hardening: a giant id is withheld identity; absurd coords are
    // ignored in favor of walk inference.
    let giant = "x".repeat(5000);
    tx.send(gmcp(
        "Room.Info",
        &format!(
            r#"{{ "num": "{giant}", "name": "Bogus", "zone": "trailfields", "exits": {{}} }}"#
        ),
    ))
    .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 902, "name": "Far Field", "zone": "trailfields", "coords": "1,999999999,5", "exits": {} }"#,
    ))
    .unwrap();
    wait_until_current_room(
        &mut events,
        &mut lines,
        "bounded-coordinate room 902",
        &mapper,
        "902",
    )
    .await;
    let transcript = lines.join("\n");
    let atlas = mapper.get_current_atlas();
    assert!(
        atlas.find_room_by_external_id(&giant).is_none(),
        "an over-length id must not be minted.\n{transcript}"
    );
    let (_, room902) = atlas
        .find_room_by_external_id("902")
        .unwrap_or_else(|| panic!("room 902 was auto-created.\n{transcript}"));
    assert!(
        room902.get_x().abs() < 100.0 && room902.get_y().abs() < 100.0,
        "out-of-bounds server coords fall back to walk inference (got {},{}).\n{transcript}",
        room902.get_x(),
        room902.get_y()
    );

    // ---- mapprune (opt-in): 900 stops advertising its east exit; the revisit prunes it.
    tx.send(RuntimeAction::Send(Arc::new("mapprune on".to_string())))
        .unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 900, "name": "Trail Head", "zone": "trailfields", "exits": {} }"#,
    ))
    .unwrap();
    wait_until(
        &mut events,
        &mut lines,
        "mapprune to remove room 900's stale exit",
        |_| room_has_exit_count(&mapper, "900", 0),
    )
    .await;
    let transcript = lines.join("\n");
    let atlas = mapper.get_current_atlas();
    let (_, room900_pruned) = atlas.find_room_by_external_id("900").expect("room 900");
    assert!(
        room900_pruned.get_exits().is_empty(),
        "mapprune removes the compass exit the server stopped reporting.\n{transcript}"
    );

    tx.send(RuntimeAction::Shutdown).ok();
}

/// Continent rooms (Aardwolf `coord.cont == 1`) are follow-only: a KNOWN room from an
/// imported overland map still moves the current-location marker, while unknown
/// continent rooms are never drawn (creating rooms across a 1000x1000 grid belongs to a
/// future grid regime, docs/gmcp-mapping.md §4).
#[tokio::test]
async fn auto_mapper_follows_continent_rooms_without_drawing() {
    const CONT_SERVER: &str = "AutoMapperCont";
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(CONT_SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(CONT_SERVER).join("logs")).unwrap();
    copy_package_source(CONT_SERVER);
    shared_packages::install_package(
        CONT_SERVER,
        "smudgy://local/auto-mapper",
        UpdateMode::Auto,
        true,
    )
    .unwrap();

    let map_root = smudgy_home.join("map-test-cont");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    // Stand in for an imported overland map: one continent room bound to its id.
    let mesolar = mapper
        .create_area_at(
            "Mesolar".to_string(),
            MapDestination::loose(MapStorage::Local),
        )
        .await
        .expect("create the overland area");
    mapper
        .upsert_room(
            RoomKey::new(mesolar, RoomNumber(1)),
            RoomUpdates {
                title: Some("On a dusty road".to_string()),
                external_id: Some(Some("35200".to_string())),
                ..RoomUpdates::default()
            },
        )
        .expect("seed room should enqueue");

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9340_u32),
        server_name: Arc::new(CONT_SERVER.to_string()),
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
    // The known continent room: followed.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 35200, "name": "On a dusty road", "zone": "mesolar", "terrain": "field",
             "exits": { "e": 35201 },
             "coord": { "id": 0, "x": 500, "y": 300, "cont": 1 } }"#,
    ))
    .unwrap();
    // An unknown continent room: never drawn.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 35201, "name": "On a dusty road", "zone": "mesolar", "terrain": "field",
             "exits": { "w": 35200 },
             "coord": { "id": 0, "x": 501, "y": 300, "cont": 1 } }"#,
    ))
    .unwrap();
    // Follow the known room again as an ordered marker after the unknown-room report.
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 35200, "name": "On a dusty road", "zone": "mesolar", "terrain": "field",
             "exits": { "e": 35201 },
             "coord": { "id": 0, "x": 500, "y": 300, "cont": 1 } }"#,
    ))
    .unwrap();

    let located = Cell::new(None);
    let known_location_events = Cell::new(0_usize);
    wait_until_observing(
        &mut events,
        &mut lines,
        "both known-room follows around the unknown continent room",
        |_| known_location_events.get() == 2,
        |event| {
            if let SessionEvent::SetCurrentLocation(area, room) = event {
                located.set(Some((*area, *room)));
                known_location_events.set(known_location_events.get() + 1);
            }
        },
    )
    .await;
    let transcript = lines.join("\n");

    assert_eq!(
        located.get(),
        Some((mesolar, Some(1))),
        "a known continent room is FOLLOWED (follow-only, not follow-never).\n{transcript}"
    );
    let atlas = mapper.get_current_atlas();
    assert!(
        atlas.find_room_by_external_id("35201").is_none(),
        "unknown continent rooms are never drawn.\n{transcript}"
    );
    assert_eq!(
        atlas.get_area(&mesolar).expect("mesolar").room_count(),
        1,
        "the overland map gained no rooms.\n{transcript}"
    );
    assert_eq!(
        mapper.session_area_ids().len(),
        0,
        "no zone area was opened for continent fixes.\n{transcript}"
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
        .create_area_at(
            "Other Server Map".to_string(),
            MapDestination::loose(MapStorage::Local),
        )
        .await
        .expect("create the stand-in area");
    mapper
        .upsert_room(
            RoomKey::new(elsewhere, RoomNumber(1)),
            RoomUpdates {
                title: Some("A Familiar Cell".to_string()),
                external_id: Some(Some("9500".to_string())),
                ..RoomUpdates::default()
            },
        )
        .expect("seed room should enqueue");
    mapper.set_scope_exclusions(HashSet::new(), std::iter::once(elsewhere).collect());
    assert!(
        mapper
            .get_current_atlas()
            .find_room_by_external_id("9500")
            .is_none(),
        "the scope-excluded room is absent from normal identification"
    );
    let area_count_before = mapper.get_current_atlas().areas().len();

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

    let rescue_offered = Cell::new(false);
    wait_until_observing(
        &mut events,
        &mut lines,
        "the cross-entry rescue offer",
        |_| rescue_offered.get(),
        |event| {
            if matches!(event, SessionEvent::OfferMapRescue { .. }) {
                rescue_offered.set(true);
            }
        },
    )
    .await;
    let transcript = lines.join("\n");

    assert!(
        rescue_offered.get(),
        "a room mapped on another entry raises the cross-entry rescue offer.\n{transcript}"
    );
    // No duplicate was minted: no new durable zone area appeared, and "9500"
    // still resolves nowhere in normal identification.
    assert_eq!(
        mapper.get_current_atlas().areas().len(),
        area_count_before,
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

/// The unwritable-map fallback: an adopted durable zone map whose backend
/// refuses the write must be detached and the SAME fix retried into a fresh
/// local area. The mutator's draft room number resolves before submission,
/// so a failed submit must not leave the retry loop believing a room exists —
/// that phantom would end the loop early and links/current-location would
/// bind a room number the fallback area never held.
#[tokio::test]
async fn auto_mapper_retries_into_a_local_area_when_the_bound_map_refuses_writes() {
    const RETRY_SERVER: &str = "AutoMapperRetry";
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(RETRY_SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(RETRY_SERVER).join("logs")).unwrap();
    copy_package_source(RETRY_SERVER);
    shared_packages::install_package(
        RETRY_SERVER,
        "smudgy://local/auto-mapper",
        UpdateMode::Auto,
        true,
    )
    .unwrap();

    let map_root = smudgy_home.join("map-test-retry");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    // A saved local map the package will adopt by folded name.
    let bound = mapper
        .create_area_at(
            "walledgarden".to_string(),
            MapDestination::loose(MapStorage::Local),
        )
        .await
        .expect("create the bound local area");
    let seed = mapper
        .upsert_room(
            RoomKey::new(bound, RoomNumber(1)),
            RoomUpdates {
                title: Some("Garden Gate".to_string()),
                external_id: Some(Some("700".to_string())),
                ..RoomUpdates::default()
            },
        )
        .expect("seed room should enqueue");
    if let Some(operation_id) = seed.operation_id() {
        mapper
            .wait_for_mutation(operation_id)
            .await
            .expect("seed room acknowledged");
    }

    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9341_u32),
        server_name: Arc::new(RETRY_SERVER.to_string()),
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

    // Let initial map loading finish while the document is still readable, then make this
    // one area read-only to the backend without disabling new local map creation. A
    // future-format document is deliberately refused on every platform.
    mapper
        .import_areas_if_absent(Vec::new())
        .await
        .expect("wait for the initial local map load");
    let bound_path = map_root
        .join("local")
        .join("areas-v2")
        .join(format!("{bound}.json"));
    let mut document: serde_json::Value = serde_json::from_slice(
        &std::fs::read(&bound_path).expect("read bound local area document"),
    )
    .expect("parse bound local area document");
    document["format_version"] = serde_json::json!(smudgy_cloud::AREA_FORMAT_VERSION + 1);
    std::fs::write(
        &bound_path,
        serde_json::to_vec_pretty(&document).expect("serialize future-format area document"),
    )
    .expect("replace bound map with future-format document");
    // External file edits enter the shared store only through explicit refresh.
    mapper
        .refresh_local_store()
        .await
        .expect("refresh external edit");

    // A NEW room in the adopted zone: the first attempt drafts into the bound
    // local map and fails at submit; the retry must land a REAL room in a
    // fresh local area.
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{ "num": 701, "name": "Overgrown Path", "zone": "walledgarden", "terrain": "field",
             "exits": {} }"#,
    ))
    .unwrap();

    let fallback_located = Cell::new(false);
    wait_until_observing(
        &mut events,
        &mut lines,
        "the unwritable-map retry to create room 701 in a fallback area",
        |lines| lines.iter().any(|line| line.contains("not writable")) && fallback_located.get(),
        |event| {
            let SessionEvent::SetCurrentLocation(area_id, Some(room_number)) = event else {
                return;
            };
            let atlas = mapper.get_current_atlas();
            let room_number = RoomNumber(*room_number);
            let is_fallback_room = *area_id != bound
                && atlas.get_area(area_id).is_some_and(|area| {
                    area.get_room(&room_number)
                        .is_some_and(|room| room.get_external_id() == Some("701"))
                });
            fallback_located.set(is_fallback_room);
        },
    )
    .await;
    let transcript = lines.join("\n");

    assert!(
        lines.iter().any(|line| line.contains("not writable")),
        "the unwritable adoption is announced once.\n{transcript}"
    );
    assert_eq!(
        mapper.session_area_ids().len(),
        0,
        "the fallback must never open a session area.\n{transcript}"
    );
    let atlas = mapper.get_current_atlas();
    let fallback_area = atlas
        .areas()
        .find(|area| {
            *area.get_id() != bound
                && area
                    .get_rooms()
                    .iter()
                    .any(|room| room.get_external_id() == Some("701"))
        })
        .unwrap_or_else(|| panic!("the fallback local area was created.\n{transcript}"));
    let fallback = *fallback_area.get_id();
    assert_ne!(fallback, bound, "the unwritable map was detached");
    assert_eq!(mapper.area_storage(&fallback), MapStorage::Local);
    assert_eq!(
        fallback_area.room_count(),
        1,
        "the retry created a real room in the fallback area — a phantom draft \
         number would have left it empty.\n{transcript}"
    );
    let fallback_room = fallback_area
        .get_rooms()
        .iter()
        .find(|room| room.get_external_id() == Some("701"))
        .expect("the retried room exists");
    assert_eq!(fallback_room.get_external_id(), Some("701"));
    assert_eq!(fallback_room.get_title(), "Overgrown Path");

    tx.send(RuntimeAction::Shutdown).ok();
}
