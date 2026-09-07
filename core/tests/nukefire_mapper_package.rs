//! End-to-end smoke coverage for the authored `nukefire-mapper` package.
//! A minimal local `nukefire-gmcp` fixture exposes the same retained-tree and
//! per-message helpers the mapper consumes; the real mapper and map-layout
//! sources run sandboxed under their manifests.

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use futures::StreamExt;
use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{
    CloudMapper, CompositeBackend, Credential, CredentialSource, ExitArgs, ExitDirection,
    LocalBackend, MapDestination, MapStorage, Mapper, MapperBackend, PackageApiClient, PortMode,
    RoomNumber, RoomSide, RoomUpdates,
};
use smudgy_core::models::local_packages::packages_dir;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, TaggedSessionEvent, spawn,
};

const MAP_WAIT: Duration = Duration::from_secs(15);
const SERVER: &str = "tdome.nukefire.org";
const MAPPER_SPEC: &str = "smudgy://local/nukefire-mapper";

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|candidate| candidate == name) {
            return Some(path);
        }
    }
    None
}

fn copy_package(server: &str, name: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packages")
        .join(name);
    let destination = packages_dir(server).expect("packages dir").join(name);
    std::fs::create_dir_all(&destination).expect("create package directory");
    for entry in std::fs::read_dir(&source).unwrap_or_else(|_| panic!("read package {name}")) {
        let entry = entry.expect("package entry");
        if entry.file_type().expect("entry type").is_file() {
            std::fs::copy(entry.path(), destination.join(entry.file_name()))
                .expect("copy package source");
        }
    }
}

fn localize_mapper_dependencies(server: &str) {
    let directory = packages_dir(server)
        .expect("packages dir")
        .join("nukefire-mapper");
    for entry in std::fs::read_dir(&directory).expect("read mapper package") {
        let entry = entry.expect("mapper package entry");
        let path = entry.path();
        let is_source = path.extension().is_some_and(|extension| extension == "ts");
        let is_manifest = path
            .file_name()
            .is_some_and(|name| name == "smudgy.package.json");
        if !is_source && !is_manifest {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read mapper source");
        let localized = source
            .replace(
                "smudgy://kapusniak/nukefire-gmcp",
                "smudgy://local/nukefire-gmcp",
            )
            .replace("smudgy://kapusniak/map-layout", "smudgy://local/map-layout");
        std::fs::write(path, localized).expect("localize mapper dependency");
    }
}

fn write_gmcp_fixture(server: &str) {
    let directory = packages_dir(server)
        .expect("packages dir")
        .join("nukefire-gmcp");
    std::fs::create_dir_all(&directory).expect("create GMCP fixture");
    std::fs::write(
        directory.join("smudgy.package.json"),
        r#"{
          "version": "0.0.0-test",
          "entry": "index.ts",
          "permissions": { "smudgy": { "interop": ["read"] } }
        }"#,
    )
    .expect("write GMCP fixture manifest");
    std::fs::write(
        directory.join("index.ts"),
        r#"import gmcp from "smudgy:state/gmcp";
export const nukefire = gmcp;
export function watchMessage(name: string, handler: (payload: any) => void) {
  return nukefire.watch(name, handler);
}
export function onMessage(name: string, handler: (payload: any) => void) {
  return nukefire.onWrite(name, (path: string, snapshot: any) => {
    if (path.toLowerCase() === name.toLowerCase() && snapshot !== undefined) handler(snapshot);
  });
}
"#,
    )
    .expect("write GMCP fixture source");
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

async fn wait_for_map_state<S, F>(events: &mut S, lines: &mut Vec<String>, ready: F) -> bool
where
    S: futures::Stream<Item = TaggedSessionEvent> + Unpin,
    F: Fn() -> bool,
{
    let deadline = tokio::time::Instant::now() + MAP_WAIT;
    loop {
        if ready() {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining.min(Duration::from_millis(100)), events.next()).await {
            Ok(Some(event)) => {
                if let SessionEvent::UpdateBuffer(updates) = event.event {
                    collect(&updates, lines);
                }
            }
            Ok(None) => return ready(),
            Err(_) => {}
        }
    }
}

fn target_port_layout(
    mapper: &Mapper,
    external_id: &str,
    side: RoomSide,
) -> Option<Vec<(usize, f32, PortMode)>> {
    let atlas = mapper.get_current_atlas();
    let (room_key, target) = atlas.find_room_by_external_id(external_id)?;
    let area = atlas.get_area(&room_key.area_id)?;
    let target_number = target.get_room_number();
    let mut ports: Vec<_> = area
        .get_connections()
        .iter()
        .filter_map(|connection| {
            let endpoint = if connection.endpoint_a.room_number == target_number {
                Some(connection.endpoint_a)
            } else {
                connection
                    .endpoint_b
                    .filter(|endpoint| endpoint.room_number == target_number)
            }?;
            if endpoint.side != side {
                return None;
            }
            let member_count = area
                .get_rooms()
                .iter()
                .flat_map(|room| room.get_exits())
                .filter(|exit| exit.connection_id == connection.id)
                .count();
            Some((member_count, endpoint.port_offset, endpoint.port_mode))
        })
        .collect();
    ports.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
    Some(ports)
}

fn area_property_for_room(mapper: &Mapper, external_id: &str, property: &str) -> Option<String> {
    let atlas = mapper.get_current_atlas();
    let (room_key, _) = atlas.find_room_by_external_id(external_id)?;
    atlas
        .get_area(&room_key.area_id)
        .and_then(|area| area.get_property(property).map(str::to_string))
}

#[tokio::test]
// Keep the authored-package smoke test as one chronological session: splitting
// it would hide which GMCP messages and durable mapper state share a runtime.
#[allow(clippy::too_many_lines)]
async fn nukefire_snapshot_creates_one_local_area_inside_the_nukefire_atlas() {
    let home = tempfile::tempdir().expect("create temp home");
    let home_path = home.path().to_path_buf();
    std::mem::forget(home);
    smudgy_core::set_smudgy_home(&home_path);
    let smudgy_home = smudgy_core::get_smudgy_home().expect("smudgy home");
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("modules")).unwrap();
    std::fs::create_dir_all(smudgy_home.join(SERVER).join("logs")).unwrap();
    copy_package(SERVER, "map-layout");
    copy_package(SERVER, "nukefire-mapper");
    write_gmcp_fixture(SERVER);
    localize_mapper_dependencies(SERVER);
    shared_packages::install_package(SERVER, MAPPER_SPEC, UpdateMode::Auto, true)
        .expect("install NukeFire mapper");
    shared_packages::save_param_value(
        SERVER,
        MAPPER_SPEC,
        "debugMappingDecisions",
        serde_json::json!(true),
    )
    .expect("enable mapper decision log");

    let map_root = smudgy_home.join("map-test");
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    let mapper = Mapper::new(backend, map_root.join("cache"));

    // Stand in for a map created by an older package version: every port still
    // occupies its semantic midpoint, and an otherwise identical Map.Local
    // snapshot must migrate it without waiting for unrelated topology edits.
    let existing_port_area = mapper
        .create_area_at(
            "Port Existing Test".to_string(),
            MapDestination::loose(MapStorage::Local),
        )
        .await
        .expect("create existing port area");
    let room = |title: &str, external_id: &str, x: f32, y: f32| RoomUpdates {
        title: Some(title.to_string()),
        external_id: Some(Some(external_id.to_string())),
        x: Some(x),
        y: Some(y),
        level: Some(0),
        ..RoomUpdates::default()
    };
    let exit = |direction, to, to_direction, command: Option<&str>| ExitArgs {
        from_direction: direction,
        to_area_id: Some(existing_port_area),
        to_room_number: Some(RoomNumber(to)),
        to_direction,
        weight: 1.0,
        command: command.map(str::to_string),
        ..ExitArgs::default()
    };
    let seeded = mapper
        .mutate_area(
            existing_port_area,
            vec![
                AreaMutation::UpsertAreaProperty {
                    name: "nukefire.zone".to_string(),
                    value: "33".to_string(),
                    is_secret: None,
                },
                // Simulate an earlier quiet reflow which was interrupted. The
                // package must retry it passively on this area's first entry.
                AreaMutation::UpsertAreaProperty {
                    name: "nukefire.layout.polish-pending".to_string(),
                    value: "true".to_string(),
                    is_secret: None,
                },
                AreaMutation::UpsertRoom {
                    room_number: RoomNumber(1),
                    body: room("Existing Port Target", "500", 0.0, 0.0),
                },
                AreaMutation::UpsertRoom {
                    room_number: RoomNumber(2),
                    body: room("Existing Reciprocal Source", "501", -3.0, 0.0),
                },
                AreaMutation::UpsertRoom {
                    room_number: RoomNumber(3),
                    body: room("Existing Northwest Source", "502", -3.0, -1.0),
                },
                AreaMutation::UpsertRoom {
                    room_number: RoomNumber(4),
                    body: room("Existing Southwest Source", "503", -3.0, 1.0),
                },
                AreaMutation::CreateExit {
                    room_number: RoomNumber(2),
                    body: exit(ExitDirection::East, 1, Some(ExitDirection::West), None),
                },
                AreaMutation::CreateExit {
                    room_number: RoomNumber(1),
                    body: exit(ExitDirection::West, 2, Some(ExitDirection::East), None),
                },
                AreaMutation::CreateExit {
                    room_number: RoomNumber(3),
                    body: exit(
                        ExitDirection::Special,
                        1,
                        None,
                        Some("existing-northwest-arrival"),
                    ),
                },
                AreaMutation::CreateExit {
                    room_number: RoomNumber(4),
                    body: exit(
                        ExitDirection::Special,
                        1,
                        None,
                        Some("existing-southwest-arrival"),
                    ),
                },
            ],
            "Seed existing centered ports",
        )
        .expect("seed existing centered ports");
    if let Some(operation_id) = seeded.operation_id() {
        mapper
            .wait_for_mutation(operation_id)
            .await
            .expect("existing centered ports acknowledged");
    }
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(9360_u32),
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
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 100, "name": "Central Plaza", "area": "Tek Angeles",
          "zone": 42, "terrain": "city", "exits": {},
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    let snapshot = r#"{
      "version": 1, "source": "bigmap+gps", "center": 100,
      "zone": 30, "plane": 0,
      "rooms": [{
        "vnum": 100, "name": "Central Plaza", "zone": 30,
        "terrain": "city", "x": 0, "y": 0, "z": 0,
        "current": true, "route": false, "destination": false
      }],
      "links": [],
      "gps": {
        "active": false, "type": "none", "target": -1,
        "description": "", "steps": 0, "route_raw": ""
      },
      "truncated": false
    }"#;
    tx.send(gmcp("NukeFire.Map.Local", snapshot)).unwrap();
    tx.send(gmcp("NukeFire.Map.Local", snapshot)).unwrap();

    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            mapper
                .get_current_atlas()
                .find_room_by_external_id("100")
                .is_some()
        })
        .await,
        "timed out waiting for room 100"
    );
    let transcript = lines.join("\n");
    assert!(
        !transcript.contains("[nukefire-mapper] failed")
            && !transcript.contains("[nukefire-mapper] smudgy:"),
        "mapper reported an error:\n{transcript}"
    );

    let atlases = mapper.list_atlases().await.expect("list atlases");
    let nukefire_atlases: Vec<_> = atlases
        .iter()
        .filter(|atlas| atlas.name == "Nukefire")
        .collect();
    assert_eq!(nukefire_atlases.len(), 1, "atlas upsert is idempotent");
    let atlas_id = nukefire_atlases[0].id;
    assert_eq!(mapper.atlas_storage(&atlas_id), Some(MapStorage::Local));

    let atlas = mapper.get_current_atlas();
    let areas: Vec<_> = atlas
        .areas()
        .filter(|area| area.meta().atlas_id == Some(atlas_id))
        .collect();
    assert_eq!(areas.len(), 1, "repeat snapshots reuse the zone area");
    assert_eq!(areas[0].get_name(), "Tek Angeles");
    assert_eq!(areas[0].get_property("nukefire.zone"), Some("30"));
    let (room, _) = atlas
        .find_room_by_external_id("100")
        .unwrap_or_else(|| panic!("room 100 was mapped; transcript:\n{transcript}"));
    assert_eq!(room.area_id, *areas[0].get_id());
    assert_eq!(mapper.area_storage(&room.area_id), MapStorage::Local);

    // NukeFire never includes the other endpoint of a vertical traversal in
    // Map.Local. Arrive below room 100 with a same-plane eastern neighbor: the
    // durable room is therefore only a resident, while both new rooms are
    // chart nodes whose reported z remains zero.
    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 200, "name": "Lower Landing", "area": "Tek Angeles",
          "zone": 42, "terrain": "inside", "exits": { "u": 100, "e": 201 },
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    let lower_snapshot = r#"{
      "version": 1, "source": "bigmap+gps", "center": 200,
      "zone": 30, "plane": 0,
      "rooms": [
        {
          "vnum": 200, "name": "Lower Landing", "zone": 30,
          "terrain": "inside", "x": 0, "y": 0, "z": 0,
          "current": true, "route": false, "destination": false
        },
        {
          "vnum": 201, "name": "Lower Hall", "zone": 30,
          "terrain": "inside", "x": 1, "y": 0, "z": 0,
          "current": false, "route": false, "destination": false
        }
      ],
      "links": [{
        "from": 200, "to": 201, "direction": "east",
        "bidirectional": true, "closed": false, "locked": false, "route": false
      }],
      "gps": {
        "active": false, "type": "none", "target": -1,
        "description": "", "steps": 0, "route_raw": ""
      },
      "truncated": false
    }"#;
    tx.send(gmcp("NukeFire.Map.Local", lower_snapshot)).unwrap();

    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            let atlas = mapper.get_current_atlas();
            atlas.find_room_by_external_id("200").is_some()
                && atlas.find_room_by_external_id("201").is_some()
        })
        .await,
        "timed out waiting for lower-level rooms"
    );
    let transcript = lines.join("\n");
    assert!(
        !transcript.contains("[nukefire-mapper] failed")
            && !transcript.contains("[nukefire-mapper] smudgy:"),
        "mapper reported an error during vertical arrival:\n{transcript}"
    );

    let atlas = mapper.get_current_atlas();
    let (_, room100) = atlas.find_room_by_external_id("100").expect("room 100");
    let (_, room200) = atlas.find_room_by_external_id("200").expect("room 200");
    let (_, room201) = atlas.find_room_by_external_id("201").expect("room 201");
    assert_eq!(
        room200.get_level(),
        room100.get_level() - 1,
        "resident-only down seam places the arrival one level below"
    );
    assert_eq!(
        room201.get_level(),
        room200.get_level(),
        "the rest of Map.Local follows the arrival onto its level"
    );
    assert!((room200.get_x() - room100.get_x()).abs() < f32::EPSILON);
    assert!((room200.get_y() - room100.get_y()).abs() < f32::EPSILON);
    assert!((room201.get_x() - room200.get_x() - 1.0).abs() < f32::EPSILON);
    assert!((room201.get_y() - room200.get_y()).abs() < f32::EPSILON);

    // Seed a deliberately stretched but otherwise valid corridor. The prompt
    // topology lane preserves Map.Local's x=4 placement; after the quiet
    // period, the full reflow must publish and durably apply its adjacent
    // best-so-far layout before exhaustive repair returns.
    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 300, "name": "Progress West", "area": "Progressive Test",
          "zone": 31, "terrain": "city", "exits": { "e": 301 },
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    let gapped_snapshot = r#"{
      "version": 1, "source": "bigmap+gps", "center": 300,
      "zone": 31, "plane": 0,
      "rooms": [
        {
          "vnum": 300, "name": "Progress West", "zone": 31,
          "terrain": "city", "x": 0, "y": 0, "z": 0,
          "current": true, "route": false, "destination": false
        },
        {
          "vnum": 301, "name": "Progress East", "zone": 31,
          "terrain": "city", "x": 4, "y": 0, "z": 0,
          "current": false, "route": false, "destination": false
        }
      ],
      "links": [{
        "from": 300, "to": 301, "direction": "east",
        "bidirectional": true, "closed": false, "locked": false, "route": false
      }],
      "gps": {
        "active": false, "type": "none", "target": -1,
        "description": "", "steps": 0, "route_raw": ""
      },
      "truncated": false
    }"#;
    tx.send(gmcp("NukeFire.Map.Local", gapped_snapshot))
        .unwrap();
    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            let atlas = mapper.get_current_atlas();
            let Some((_, room300)) = atlas.find_room_by_external_id("300") else {
                return false;
            };
            let Some((_, room301)) = atlas.find_room_by_external_id("301") else {
                return false;
            };
            (room301.get_x() - room300.get_x() - 1.0).abs() < f32::EPSILON
        })
        .await,
        "timed out waiting for progressive corridor reflow"
    );
    let atlas = mapper.get_current_atlas();
    let (_, room300) = atlas.find_room_by_external_id("300").expect("room 300");
    let (_, room301) = atlas.find_room_by_external_id("301").expect("room 301");
    assert!(
        (room301.get_x() - room300.get_x() - 1.0).abs() < f32::EPSILON,
        "progressive reflow compacted the stretched corridor"
    );
    // Coordinates become host-visible before the progressive callback resumes.
    // Switching areas can abort that callback before it logs the applied layout,
    // so wait for the record itself before sending the next synthetic area.
    let decision_log = find_file(&smudgy_home.join(SERVER), "mapping-decisions.jsonl")
        .expect("debug decision log was created");
    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            std::fs::read(&decision_log)
                .expect("read mapper decision log")
                .split_inclusive(|byte| *byte == b'\n')
                // The asynchronous writer may still be appending the last record.
                .filter(|line| line.ends_with(b"\n"))
                .map(|line| {
                    serde_json::from_slice::<serde_json::Value>(line)
                        .expect("valid decision record")
                })
                .any(|record| {
                    record["kind"] == "layout-progress-applied"
                        && record["area"]["name"] == "Progressive Test"
                        && record["movedRooms"].as_u64().is_some_and(|count| count > 0)
                })
        })
        .await,
        "timed out waiting for the progressive reflow decision record:\n{}",
        lines.join("\n")
    );

    // A reciprocal west-wall connection keeps its semantic midpoint while
    // two one-way arrivals fan into the neighboring canonical port lanes.
    // This runs through the authored package and script-visible
    // Exit.connection_id, not merely the pure TypeScript allocator.
    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 400, "name": "Port Target", "area": "Port Test",
          "zone": 32, "terrain": "city", "exits": {},
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    let port_snapshot = r#"{
      "version": 1, "source": "bigmap+gps", "center": 400,
      "zone": 32, "plane": 0,
      "rooms": [
        {
          "vnum": 400, "name": "Port Target", "zone": 32,
          "terrain": "city", "x": 0, "y": 0, "z": 0,
          "current": true, "route": false, "destination": false
        },
        {
          "vnum": 401, "name": "Reciprocal Source", "zone": 32,
          "terrain": "city", "x": -3, "y": 0, "z": 0,
          "current": false, "route": false, "destination": false
        },
        {
          "vnum": 402, "name": "Northwest Source", "zone": 32,
          "terrain": "city", "x": -3, "y": -1, "z": 0,
          "current": false, "route": false, "destination": false
        },
        {
          "vnum": 403, "name": "Southwest Source", "zone": 32,
          "terrain": "city", "x": -3, "y": 1, "z": 0,
          "current": false, "route": false, "destination": false
        }
      ],
      "links": [
        {
          "from": 401, "to": 400, "direction": "east",
          "bidirectional": true, "closed": false, "locked": false, "route": false
        },
        {
          "from": 402, "to": 400, "direction": "northwest-arrival",
          "bidirectional": false, "closed": false, "locked": false, "route": false
        },
        {
          "from": 403, "to": 400, "direction": "southwest-arrival",
          "bidirectional": false, "closed": false, "locked": false, "route": false
        }
      ],
      "gps": {
        "active": false, "type": "none", "target": -1,
        "description": "", "steps": 0, "route_raw": ""
      },
      "truncated": false
    }"#;
    tx.send(gmcp("NukeFire.Map.Local", port_snapshot)).unwrap();
    tx.send(gmcp("NukeFire.Map.Local", port_snapshot)).unwrap();
    let expected_ports = vec![
        (1, 0.2, PortMode::AutoPinned),
        (1, 0.8, PortMode::AutoPinned),
        (2, 0.5, PortMode::AutoPinned),
    ];
    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            target_port_layout(&mapper, "400", RoomSide::West).as_ref() == Some(&expected_ports)
        })
        .await,
        "timed out waiting for one-way port disambiguation"
    );
    let transcript = lines.join("\n");
    assert!(
        !transcript.contains("[nukefire-mapper] failed")
            && !transcript.contains("[nukefire-mapper] smudgy:"),
        "mapper reported an error during port disambiguation:\n{transcript}"
    );
    assert_eq!(
        target_port_layout(&mapper, "400", RoomSide::West),
        Some(expected_ports.clone()),
        "one-way arrivals use distinct target-wall lanes"
    );

    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 500, "name": "Existing Port Target", "area": "Port Existing Test",
          "zone": 33, "terrain": "city", "exits": {},
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    let existing_port_snapshot = r#"{
      "version": 1, "source": "bigmap+gps", "center": 500,
      "zone": 33, "plane": 0,
      "rooms": [
        {
          "vnum": 500, "name": "Existing Port Target", "zone": 33,
          "terrain": "city", "x": 0, "y": 0, "z": 0,
          "current": true, "route": false, "destination": false
        },
        {
          "vnum": 501, "name": "Existing Reciprocal Source", "zone": 33,
          "terrain": "city", "x": -3, "y": 0, "z": 0,
          "current": false, "route": false, "destination": false
        },
        {
          "vnum": 502, "name": "Existing Northwest Source", "zone": 33,
          "terrain": "city", "x": -3, "y": -1, "z": 0,
          "current": false, "route": false, "destination": false
        },
        {
          "vnum": 503, "name": "Existing Southwest Source", "zone": 33,
          "terrain": "city", "x": -3, "y": 1, "z": 0,
          "current": false, "route": false, "destination": false
        }
      ],
      "links": [
        {
          "from": 501, "to": 500, "direction": "east",
          "bidirectional": true, "closed": false, "locked": false, "route": false
        },
        {
          "from": 502, "to": 500, "direction": "existing-northwest-arrival",
          "bidirectional": false, "closed": false, "locked": false, "route": false
        },
        {
          "from": 503, "to": 500, "direction": "existing-southwest-arrival",
          "bidirectional": false, "closed": false, "locked": false, "route": false
        }
      ],
      "gps": {
        "active": false, "type": "none", "target": -1,
        "description": "", "steps": 0, "route_raw": ""
      },
      "truncated": false
    }"#;
    tx.send(gmcp("NukeFire.Map.Local", existing_port_snapshot))
        .unwrap();
    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            target_port_layout(&mapper, "500", RoomSide::West).as_ref() == Some(&expected_ports)
        })
        .await,
        "timed out waiting for existing port migration"
    );
    assert_eq!(
        target_port_layout(&mapper, "500", RoomSide::West),
        Some(expected_ports),
        "a settled area is reconciled on its first authoritative snapshot"
    );
    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            area_property_for_room(
                &mapper,
                "500",
                "nukefire.layout.polish-exhausted-fingerprint",
            )
            .and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
            .is_some_and(|memo| {
                memo["v"] == 2
                    && memo["g"]
                        .as_str()
                        .is_some_and(|geometry| !geometry.is_empty())
                    && memo["c"].as_array().is_some_and(|contexts| {
                        contexts.len() == 1
                            && contexts[0].as_str().is_some_and(|key| {
                                key.len() == 32 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                    })
            })
        })
        .await,
        "timed out waiting for passive layout polish to memoize this fixed-point context"
    );
    assert_eq!(
        area_property_for_room(&mapper, "500", "nukefire.layout.polish-pending").as_deref(),
        Some("true"),
        "a context-relative fixed point must preserve area-wide polish eligibility"
    );
    let records: Vec<serde_json::Value> = std::fs::read_to_string(decision_log)
        .expect("read mapper decision log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid decision record"))
        .collect();
    assert!(
        !records.iter().any(|record| {
            record["kind"] == "mutation-error"
                && record.to_string().contains("endpoint_room_immutable")
        }),
        "newly created links must be mirrored in canonical endpoint order"
    );
    let mutation_id = records
        .iter()
        .find(|record| record["kind"] == "mutation-start" && record["api"] == "mutateArea")
        .and_then(|record| record["mutationId"].as_u64())
        .expect("batched area mutation start is logged");
    assert!(
        records
            .iter()
            .any(|record| { record["kind"] == "mutation-start" && record["api"] == "createAtlas" })
    );
    assert!(
        records
            .iter()
            .any(|record| { record["kind"] == "mutation-start" && record["api"] == "createArea" })
    );
    assert!(records.iter().any(|record| {
        record["kind"] == "mutation-draft-complete"
            && record["mutationId"].as_u64() == Some(mutation_id)
    }));
    assert!(records.iter().any(|record| {
        record["kind"] == "mutation-complete" && record["mutationId"].as_u64() == Some(mutation_id)
    }));
    assert!(
        records
            .iter()
            .any(|record| record["kind"] == "current-location")
    );
    assert!(records.iter().any(|record| {
        record["kind"] == "layout-progress-applied"
            && record["area"]["name"] == "Progressive Test"
            && record["movedRooms"].as_u64().is_some_and(|count| count > 0)
    }));

    tx.send(RuntimeAction::Shutdown).ok();
}
