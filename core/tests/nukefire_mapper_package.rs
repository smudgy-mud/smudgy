//! End-to-end smoke coverage for the authored `nukefire-mapper` package.
//! A minimal local `nukefire-gmcp` fixture exposes the same retained-tree and
//! per-message helpers the mapper consumes; the real mapper and map-layout
//! sources run sandboxed under their manifests.

mod support;

use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{
    ExitArgs, ExitDirection, MapDestination, MapStorage, Mapper, PortMode, RoomNumber, RoomSide,
    RoomUpdates,
};
use smudgy_core::models::shared_packages;
use smudgy_core::session::runtime::RuntimeAction;
use support::nukefire::{
    MAPPER_SPEC, Session, find_file, gmcp, install_mapper_packages, local_mapper, smudgy_home,
    start_session, wait_for_map_state,
};

const SERVER: &str = "tdome.nukefire.org";

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
    let smudgy_home = smudgy_home();
    install_mapper_packages(SERVER);
    shared_packages::save_param_value(
        SERVER,
        MAPPER_SPEC,
        "debugMappingDecisions",
        serde_json::json!(true),
    )
    .expect("enable mapper decision log");

    let map_root = smudgy_home.join("map-test");
    let mapper = local_mapper(&map_root);

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
                },
                AreaMutation::UpsertAreaProperty {
                    name: "nukefire.mapper".to_string(),
                    value: "NukeFire.Map.Local".to_string(),
                },
                // Simulate an earlier quiet reflow which was interrupted. The
                // package must retry it passively on this area's first entry.
                AreaMutation::UpsertAreaProperty {
                    name: "nukefire.layout.polish-pending".to_string(),
                    value: "true".to_string(),
                },
                AreaMutation::UpsertRoom {
                    room_source: None,
                    room_number: RoomNumber(1),
                    body: room("Existing Port Target", "500", 0.0, 0.0),
                },
                AreaMutation::UpsertRoom {
                    room_source: None,
                    room_number: RoomNumber(2),
                    body: room("Existing Reciprocal Source", "501", -3.0, 0.0),
                },
                AreaMutation::UpsertRoom {
                    room_source: None,
                    room_number: RoomNumber(3),
                    body: room("Existing Northwest Source", "502", -3.0, -1.0),
                },
                AreaMutation::UpsertRoom {
                    room_source: None,
                    room_number: RoomNumber(4),
                    body: room("Existing Southwest Source", "503", -3.0, 1.0),
                },
                AreaMutation::CreateExit {
                    room_source: None,
                    room_number: RoomNumber(2),
                    body: exit(ExitDirection::East, 1, Some(ExitDirection::West), None),
                },
                AreaMutation::CreateExit {
                    room_source: None,
                    room_number: RoomNumber(1),
                    body: exit(ExitDirection::West, 2, Some(ExitDirection::East), None),
                },
                AreaMutation::CreateExit {
                    room_source: None,
                    room_number: RoomNumber(3),
                    body: exit(
                        ExitDirection::Special,
                        1,
                        None,
                        Some("existing-northwest-arrival"),
                    ),
                },
                AreaMutation::CreateExit {
                    room_source: None,
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
    let Session {
        tx,
        mut events,
        mut lines,
    } = start_session(SERVER, 9360, &mapper).await;
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
    // The zone the player stands in is filed into the map for its area name:
    // that map is keyed by the folded name and marked as the mapper's own.
    assert_eq!(areas[0].get_property("nukefire.area"), Some("tek angeles"));
    assert_eq!(
        areas[0].get_property("nukefire.mapper"),
        Some("NukeFire.Map.Local")
    );
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

    // Re-enter the first zone on a server plane which has no room or vertical
    // seam in the durable map yet. Map.Local room z values are relative to its
    // current plane, so the mapper must not merge this new chart into level 0.
    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 210, "name": "High Re-entry", "area": "Tek Angeles",
          "zone": 42, "terrain": "inside", "exits": {},
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    let high_reentry_snapshot = r#"{
      "version": 1, "source": "bigmap+gps", "center": 210,
      "zone": 30, "plane": 2,
      "rooms": [{
        "vnum": 210, "name": "High Re-entry", "zone": 30,
        "terrain": "inside", "x": 0, "y": 0, "z": 0,
        "current": true, "route": false, "destination": false
      }],
      "links": [],
      "gps": {
        "active": false, "type": "none", "target": -1,
        "description": "", "steps": 0, "route_raw": ""
      },
      "truncated": false
    }"#;
    tx.send(gmcp("NukeFire.Map.Local", high_reentry_snapshot))
        .unwrap();
    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            mapper
                .get_current_atlas()
                .find_room_by_external_id("210")
                .is_some()
        })
        .await,
        "timed out waiting for the zone re-entry"
    );
    let transcript = lines.join("\n");
    let atlas = mapper.get_current_atlas();
    let (_, high_reentry) = atlas
        .find_room_by_external_id("210")
        .expect("high re-entry room");
    assert_eq!(
        high_reentry.get_level(),
        2,
        "an unanchored zone re-entry follows Map.Local.plane; transcript:\n{transcript}"
    );
    assert!(
        !transcript.contains("invalid_routing"),
        "zone re-entry produced an invalid connection mutation:\n{transcript}"
    );

    // A reciprocal west-wall connection keeps its semantic midpoint while
    // two one-way arrivals fan into the neighboring canonical port lanes.
    // This runs through the authored package and script-visible
    // Exit.connection_id, not merely the pure TypeScript allocator.
    //
    // The six rooms tile a full 2x3 block on purpose. Two arrivals can only
    // share the target's west wall from the diagonals either side of the
    // reciprocal, which spans three rows -- and passive polish would compact
    // any slack in that footprint, carrying an arrival onto another wall and
    // dissolving the fan this asserts. A block with no empty cell is already
    // at the minimum area and perimeter its room count allows, so the polish
    // pass has nothing to gain and the arrangement holds however the quiet
    // window falls.
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
          "terrain": "city", "x": -1, "y": 0, "z": 0,
          "current": false, "route": false, "destination": false
        },
        {
          "vnum": 402, "name": "Northwest Source", "zone": 32,
          "terrain": "city", "x": -1, "y": -1, "z": 0,
          "current": false, "route": false, "destination": false
        },
        {
          "vnum": 403, "name": "Southwest Source", "zone": 32,
          "terrain": "city", "x": -1, "y": 1, "z": 0,
          "current": false, "route": false, "destination": false
        },
        {
          "vnum": 404, "name": "North Filler", "zone": 32,
          "terrain": "city", "x": 0, "y": -1, "z": 0,
          "current": false, "route": false, "destination": false
        },
        {
          "vnum": 405, "name": "South Filler", "zone": 32,
          "terrain": "city", "x": 0, "y": 1, "z": 0,
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
        },
        {
          "from": 404, "to": 400, "direction": "south",
          "bidirectional": true, "closed": false, "locked": false, "route": false
        },
        {
          "from": 405, "to": 400, "direction": "north",
          "bidirectional": true, "closed": false, "locked": false, "route": false
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
        "timed out waiting for one-way port disambiguation: west wall {:?}\n{}",
        target_port_layout(&mapper, "400", RoomSide::West),
        lines.join("\n")
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
        "timed out waiting for existing port migration: west wall {:?}\n{}",
        target_port_layout(&mapper, "500", RoomSide::West),
        lines.join("\n")
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
                memo["v"] == 3
                    && memo["g"]
                        .as_str()
                        .is_some_and(|geometry| !geometry.is_empty())
                    && memo["c"].as_array().is_some_and(|settlements| {
                        settlements.len() == 1
                            && settlements[0]["k"].as_str().is_some_and(|key| {
                                key.len() == 32 && key.bytes().all(|byte| byte.is_ascii_hexdigit())
                            })
                            && settlements[0]["s"] == 2
                            && settlements[0]["e"] == 1
                            && matches!(
                                settlements[0]["t"].as_str(),
                                Some("perfect" | "fixed-point" | "ceiling")
                            )
                    })
            })
        })
        .await,
        "timed out waiting for passive layout polish to memoize this fixed-point context"
    );
    assert_eq!(
        area_property_for_room(&mapper, "500", "nukefire.layout.polish-pending").as_deref(),
        Some(""),
        "a fixed point clears the legacy pending bit while retaining settlement evidence"
    );
    let current_decision_records = || -> Vec<serde_json::Value> {
        std::fs::read_to_string(&decision_log)
            .unwrap_or_default()
            .lines()
            // The package appends one complete JSON line at a time. Ignore a
            // concurrently observed partial tail while polling below.
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    };
    let settled_layout_decisions = current_decision_records()
        .iter()
        .filter(|record| {
            record["kind"] == "layout-decision"
                && record["area"]["name"] == "Port Existing Test"
                && record["trigger"]["moveExisting"].as_bool() == Some(true)
        })
        .count();
    assert!(
        settled_layout_decisions > 0,
        "the initial passive polish must reach the Worker before testing re-entry"
    );

    // Leave the settled area, then return through the same center/chart. The
    // durable v3 memo must be evaluated before dispatching a second Worker.
    let visits_to_400 = current_decision_records()
        .iter()
        .filter(|record| record["kind"] == "current-location" && record["vnum"] == 400)
        .count();
    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 400, "name": "Port Target", "area": "Port Test",
          "zone": 32, "terrain": "city", "exits": {},
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    tx.send(gmcp("NukeFire.Map.Local", port_snapshot)).unwrap();
    assert!(
        wait_for_map_state(&mut events, &mut lines, || {
            current_decision_records()
                .iter()
                .filter(|record| record["kind"] == "current-location" && record["vnum"] == 400)
                .count()
                > visits_to_400
        })
        .await,
        "timed out leaving the settled area before the re-entry check"
    );

    let prior_settled_skips = current_decision_records()
        .iter()
        .filter(|record| {
            record["kind"] == "layout-polish-retry-skipped"
                && record["area"]["name"] == "Port Existing Test"
        })
        .count();
    tx.send(gmcp(
        "Room.Info",
        r#"{
          "num": 500, "name": "Existing Port Target", "area": "Port Existing Test",
          "zone": 33, "terrain": "city", "exits": {},
          "coords": { "x": 0, "y": 0, "z": 0 }
        }"#,
    ))
    .unwrap();
    tx.send(gmcp("NukeFire.Map.Local", existing_port_snapshot))
        .unwrap();
    let reentry_skipped = wait_for_map_state(&mut events, &mut lines, || {
        current_decision_records()
            .iter()
            .filter(|record| {
                record["kind"] == "layout-polish-retry-skipped"
                    && record["area"]["name"] == "Port Existing Test"
                    && record["reason"] == "settled"
            })
            .count()
            > prior_settled_skips
    })
    .await;
    assert!(
        reentry_skipped,
        "timed out waiting for same-chart re-entry to consume the settled memo; tail: {:#?}",
        current_decision_records()
            .into_iter()
            .rev()
            .take(12)
            .collect::<Vec<_>>()
    );
    assert_eq!(
        current_decision_records()
            .iter()
            .filter(|record| {
                record["kind"] == "layout-decision"
                    && record["area"]["name"] == "Port Existing Test"
                    && record["trigger"]["moveExisting"].as_bool() == Some(true)
            })
            .count(),
        settled_layout_decisions,
        "same-chart A→B→A re-entry must not dispatch a second layout decision"
    );

    let records: Vec<serde_json::Value> = std::fs::read_to_string(&decision_log)
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
