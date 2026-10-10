//! Manual reflow crosses two installed, permission-restricted package isolates.
//! The authored commands module, mapper and layout Worker are served from the
//! real package cache. A commands-only entry and an inert welcome panel avoid
//! needing the UI crate's widget extension in this headless core test.

mod support;

use std::path::Path;

use serde_json::json;
use sha2::{Digest, Sha256};
use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{
    AreaId, ConnectionId, ConnectionKind, ConnectionRouting, ConnectionUpdates, DependencyKind,
    ExitArgs, ExitDirection, MapDestination, MapPoint, MapStorage, Mapper, ResolvedDependency,
    RoomNumber, RoomUpdates, SegmentShape,
};
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::package_cache::{CachedModule, CachedResolution, PackageCache};
use smudgy_script::{PackageDependency, PackageKey, PackageManifest};
use support::nukefire::{Session, local_mapper, smudgy_home, start_session};

const SERVER: &str = "NukeFireInstalledManualReflow";
const OWNER: &str = "kapusniak";
const SETTLEMENT: &str = "nukefire.layout.polish-exhausted-fingerprint";

/// This entry loads all of commands.ts, rather than extracting its reflow
/// function into trusted code. Test-only aliases execute with the genuine
/// scripts package origin, so the mapper's caller validation stays enabled.
const COMMAND_ENTRY: &str = r#"
import "./commands.ts";
import { createAlias, echo, mapper, getSessions } from "smudgy:core";
import { planAreaChange, loadLayoutModel, layoutSnapshotKey,
  createLayoutModel, resolveElevationGeometry } from "smudgy://kapusniak/map-layout";
import { applyManualLayout } from "smudgy:procedures/kapusniak/nukefire-mapper";
import { manualLayoutApplied } from "smudgy:events/kapusniak/nukefire-mapper";
manualLayoutApplied.from(getSessions()[0]).on((ack) => {
  echo("MANUAL-ACK " + ack.requestId + " " + ack.accepted + " " + (ack.error ?? ""));
});
createAlias(/^manual-invalid-final$/, () => {
  void (async () => {
    const location = mapper.getCurrentLocation();
    const result = await planAreaChange(location.area,
      { type: "reflow", anchor: location.room }, { includeSnapshotKeys: true });
    const room = mapper.getAreaById(location.area).room(location.room);
    applyManualLayout.to(getSessions()[0]).post({
      requestId: "deliberately-invalid-final",
      areaUuid: location.area,
      sourceSnapshotKey: result.sourceSnapshotKey,
      plannedSnapshotKey: result.plannedSnapshotKey,
      moves: [{ roomNumber: location.room, x: room.x + 30, y: room.y, level: room.level }],
    });
  })().catch((error) => echo("MANUAL-FIXTURE-ERROR " + error.message));
});
createAlias(/^manual-stale$/, () => {
  void (async () => {
    const location = mapper.getCurrentLocation();
    const result = await planAreaChange(location.area,
      { type: "reflow", anchor: location.room }, { includeSnapshotKeys: true });
    const room = mapper.getAreaById(location.area).room(location.room);
    await mapper.mutateArea(location.area, (mutation) => mutation.updateRooms([
      [location.room, { x: room.x + 7 }],
    ]));
    echo("MANUAL-STALE-CHANGED " + mapper.getAreaById(location.area).room(location.room).x);
    applyManualLayout.to(getSessions()[0]).post({
      requestId: "deliberately-stale",
      areaUuid: location.area,
      sourceSnapshotKey: result.sourceSnapshotKey,
      plannedSnapshotKey: result.plannedSnapshotKey,
      moves: result.patch.moves.map((move) => ({
        roomNumber: move.roomNumber, x: move.to.x, y: move.to.y, level: move.to.level,
      })),
    });
  })().catch((error) => echo("MANUAL-FIXTURE-ERROR " + error.message));
});
createAlias(/^manual-batch$/, () => {
  try {
    const location = mapper.getCurrentLocation();
    const before = loadLayoutModel(location.area);
    const planned = resolveElevationGeometry(createLayoutModel({
      ...before,
      rooms: before.rooms.map((room) => ({ ...room,
        position: { x: room.position.x + 3, y: room.position.y + 2, level: 1 },
      })),
      edges: before.edges.map(({ constraintVector, ...edge }) => edge),
    }));
    applyManualLayout.to(getSessions()[0]).post({
      requestId: "large-envelope-batch", areaUuid: location.area,
      sourceSnapshotKey: layoutSnapshotKey(before),
      plannedSnapshotKey: layoutSnapshotKey(planned),
      moves: planned.rooms.map((room) => ({ roomNumber: room.roomNumber, ...room.position })),
    });
  } catch (error) { echo("MANUAL-FIXTURE-ERROR " + error.message); }
});
createAlias(/^manual-detour$/, () => {
  try {
    const location = mapper.getCurrentLocation();
    const snapshot = layoutSnapshotKey(loadLayoutModel(location.area));
    applyManualLayout.to(getSessions()[0]).post({
      requestId: "route-only-amendment", areaUuid: location.area,
      sourceSnapshotKey: snapshot, plannedSnapshotKey: snapshot, moves: [],
      routeAmendments: [{ fromRoomNumber: 1, toRoomNumber: 2,
        waypoints: [{ x: 0, y: 7, level: 0 }, { x: 2, y: 7, level: 0 }],
      }],
    });
  } catch (error) { echo("MANUAL-FIXTURE-ERROR " + error.message); }
});
createAlias(/^manual-shift-detour$/, () => {
  try {
    const location = mapper.getCurrentLocation();
    const before = loadLayoutModel(location.area);
    const planned = resolveElevationGeometry(createLayoutModel({
      ...before,
      rooms: before.rooms.map((room) => ({ ...room, position: {
        x: room.position.x + 3, y: room.position.y + 2, level: room.position.level,
      } })),
      edges: before.edges.map(({ constraintVector, ...edge }) => edge),
    }));
    applyManualLayout.to(getSessions()[0]).post({
      requestId: "shifted-generated-amendment", areaUuid: location.area,
      sourceSnapshotKey: layoutSnapshotKey(before), plannedSnapshotKey: layoutSnapshotKey(planned),
      moves: planned.rooms.map((room) => ({ roomNumber: room.roomNumber, ...room.position })),
      routeAmendments: [{ fromRoomNumber: 1, toRoomNumber: 2,
        waypoints: [{ x: 3, y: 9, level: 0 }, { x: 5, y: 9, level: 0 }],
      }],
    });
  } catch (error) { echo("MANUAL-FIXTURE-ERROR " + error.message); }
});
createAlias(/^manual-unequal-authored$/, () => {
  try {
    const location = mapper.getCurrentLocation();
    const before = loadLayoutModel(location.area);
    const planned = resolveElevationGeometry(createLayoutModel({
      ...before,
      rooms: before.rooms.map((room) => ({ ...room, position: {
        x: room.roomNumber === 1 ? 1 : 3,
        y: room.roomNumber === 1 ? 2 : 4, level: 1,
      } })),
      edges: before.edges.map(({ constraintVector, ...edge }) => edge),
    }));
    applyManualLayout.to(getSessions()[0]).post({
      requestId: "unequal-authored-move", areaUuid: location.area,
      sourceSnapshotKey: layoutSnapshotKey(before), plannedSnapshotKey: layoutSnapshotKey(planned),
      moves: planned.rooms.map((room) => ({ roomNumber: room.roomNumber, ...room.position })),
    });
  } catch (error) { echo("MANUAL-FIXTURE-ERROR " + error.message); }
});
echo("MANUAL-COMMANDS-READY");
"#;

fn dependency(name: &str, version: &str, range: &str, kind: DependencyKind) -> ResolvedDependency {
    ResolvedDependency {
        owner_nickname: Some(OWNER.to_string()),
        name: name.to_string(),
        range: range.to_string(),
        resolved_version: version.to_string(),
        kind,
    }
}

fn manifest_dependency(
    manifest: &PackageManifest,
    name: &str,
    version: &str,
    kind: DependencyKind,
) -> ResolvedDependency {
    let declarations = match kind {
        DependencyKind::Dependency => &manifest.dependencies,
        DependencyKind::Requires => &manifest.requires,
    };
    let declaration = declarations
        .iter()
        .filter_map(|raw| PackageDependency::parse(raw).map(Result::unwrap))
        .find(|declaration| declaration.key.owner == OWNER && declaration.key.name == name)
        .expect("authored manifest declares fixture dependency");
    dependency(
        name,
        version,
        declaration.range.as_deref().unwrap_or("*"),
        kind,
    )
}

fn cache_package(
    name: &str,
    manifest: PackageManifest,
    sources: Vec<(String, String)>,
    dependencies: Vec<ResolvedDependency>,
) -> String {
    let cache = PackageCache::new().expect("temporary package cache");
    let mut modules = Vec::new();
    for (subpath, source) in sources {
        let hash = format!("{:x}", Sha256::digest(source.as_bytes()));
        cache
            .write_blob(&hash, &source)
            .expect("write package blob");
        modules.push(CachedModule {
            is_entry: subpath == "index.ts",
            subpath,
            content_hash: hash,
            media_type: "application/typescript".to_string(),
            byte_size: i64::try_from(source.len()).expect("small source module"),
        });
    }
    let mut integrity: Vec<_> = modules
        .iter()
        .map(|module| format!("{}={}", module.subpath, module.content_hash))
        .collect();
    integrity.sort();
    let integrity = integrity.join(";");
    let version = manifest.version.clone();
    cache
        .write_meta(
            &PackageKey {
                owner: OWNER.to_string(),
                name: name.to_string(),
            },
            &version,
            &CachedResolution {
                version: version.clone(),
                integrity: integrity.clone(),
                manifest,
                modules,
                dependencies,
                owner: None,
            },
        )
        .expect("cache authored package metadata");
    integrity
}

fn authored_package(name: &str) -> (PackageManifest, Vec<(String, String)>) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../packages")
        .join(name);
    let manifest = PackageManifest::parse(
        &std::fs::read_to_string(source.join("smudgy.package.json")).unwrap(),
    )
    .expect("authored package manifest");
    let sources = std::fs::read_dir(source)
        .unwrap()
        .map(Result::unwrap)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "ts" || extension == "tsx")
                && !entry.file_name().to_string_lossy().contains(".test.")
        })
        .map(|entry| {
            (
                entry.file_name().to_str().unwrap().to_string(),
                std::fs::read_to_string(entry.path()).unwrap(),
            )
        })
        .collect();
    (manifest, sources)
}

fn cache_gmcp_fixture() -> (PackageManifest, String) {
    let gmcp_manifest = PackageManifest::parse(
        r#"{"version":"1.0.0","entry":"index.ts",
        "permissions":{"smudgy":{"interop":["read"]}}}"#,
    )
    .unwrap();
    let gmcp_source = r#"
import gmcp from "smudgy:state/gmcp";
export const nukefire = gmcp;
export function watchMessage(name, handler) { return nukefire.watch(name, handler); }
export function onMessage(name, handler) {
  return nukefire.onWrite(name, (path, snapshot) => {
    if (path.toLowerCase() === name.toLowerCase() && snapshot !== undefined) handler(snapshot);
  });
}
"#;
    let gmcp_integrity = cache_package(
        "nukefire-gmcp",
        gmcp_manifest.clone(),
        vec![("index.ts".to_string(), gmcp_source.to_string())],
        vec![],
    );
    (gmcp_manifest, gmcp_integrity)
}

fn install_packages() {
    let server_dir = smudgy_home().join(SERVER);
    std::fs::create_dir_all(server_dir.join("modules")).unwrap();
    std::fs::create_dir_all(server_dir.join("logs")).unwrap();
    let (gmcp_manifest, gmcp_integrity) = cache_gmcp_fixture();
    let (layout_manifest, layout_sources) = authored_package("map-layout");
    let layout_version = layout_manifest.version.clone();
    let layout_integrity = cache_package(
        "map-layout",
        layout_manifest.clone(),
        layout_sources,
        vec![],
    );
    let (mapper_manifest, mapper_sources) = authored_package("nukefire-mapper");
    let mapper_version = mapper_manifest.version.clone();
    let mapper_integrity = cache_package(
        "nukefire-mapper",
        mapper_manifest.clone(),
        mapper_sources,
        vec![
            manifest_dependency(
                &mapper_manifest,
                "nukefire-gmcp",
                "1.0.0",
                DependencyKind::Dependency,
            ),
            manifest_dependency(
                &mapper_manifest,
                "map-layout",
                &layout_version,
                DependencyKind::Dependency,
            ),
            manifest_dependency(
                &mapper_manifest,
                "nukefire-gmcp",
                "1.0.0",
                DependencyKind::Requires,
            ),
        ],
    );
    let (scripts_manifest, mut scripts_sources) = authored_package("nukefire-scripts");
    for (subpath, source) in &mut scripts_sources {
        if subpath == "index.ts" {
            *source = COMMAND_ENTRY.to_string();
        } else if subpath == "welcome.tsx" {
            *source = "export function open() {}".to_string();
        }
    }
    let scripts_integrity = cache_package(
        "nukefire-scripts",
        scripts_manifest.clone(),
        scripts_sources,
        vec![
            manifest_dependency(
                &scripts_manifest,
                "nukefire-gmcp",
                "1.0.0",
                DependencyKind::Dependency,
            ),
            manifest_dependency(
                &scripts_manifest,
                "map-layout",
                &layout_version,
                DependencyKind::Dependency,
            ),
            manifest_dependency(
                &scripts_manifest,
                "nukefire-gmcp",
                "1.0.0",
                DependencyKind::Requires,
            ),
            manifest_dependency(
                &scripts_manifest,
                "nukefire-mapper",
                &mapper_version,
                DependencyKind::Requires,
            ),
        ],
    );
    for (name, manifest, integrity) in [
        ("nukefire-gmcp", gmcp_manifest, gmcp_integrity),
        ("map-layout", layout_manifest, layout_integrity),
        ("nukefire-mapper", mapper_manifest, mapper_integrity),
        ("nukefire-scripts", scripts_manifest, scripts_integrity),
    ] {
        let specifier = format!("smudgy://{OWNER}/{name}");
        shared_packages::install_package(SERVER, &specifier, UpdateMode::Auto, true).unwrap();
        shared_packages::record_resolution(SERVER, &specifier, &manifest.version, &integrity)
            .unwrap();
        let lock = shared_packages::load_lock(SERVER).unwrap();
        let row = lock.find(&specifier).unwrap();
        assert!(!row.trusted, "installed package roots must stay sandboxed");
        assert!(
            shared_packages::record_consent_if_unchanged(SERVER, row, &manifest.permissions)
                .unwrap()
        );
    }
}

async fn apply(mapper: &Mapper, area: AreaId, edits: Vec<AreaMutation>) {
    for batch in edits.chunks(256) {
        let submitted = mapper
            .mutate_area(area, batch.to_vec(), "Seed manual reflow test")
            .unwrap();
        if let Some(id) = submitted.operation_id() {
            mapper.wait_for_mutation(id).await.unwrap();
        }
    }
}

async fn seed_area(mapper: &Mapper, name: &str, cells: &[(i16, i16, i32)]) -> AreaId {
    let area = mapper
        .create_area_at(name.to_string(), MapDestination::loose(MapStorage::Local))
        .await
        .unwrap();
    let mut edits = vec![AreaMutation::UpsertAreaProperty {
        name: "nukefire.mapper".to_string(),
        value: "NukeFire.Map.Local".to_string(),
    }];
    for (index, &(x, y, level)) in cells.iter().enumerate() {
        edits.push(AreaMutation::UpsertRoom {
            room_source: None,
            room_number: RoomNumber(i32::try_from(index + 1).unwrap()),
            body: RoomUpdates {
                title: Some(format!("Room {}", index + 1)),
                external_id: Some(Some(format!("{}{}", name.len(), index + 1))),
                x: Some(f32::from(x)),
                y: Some(f32::from(y)),
                level: Some(level),
                ..RoomUpdates::default()
            },
        });
    }
    apply(mapper, area, edits).await;
    area
}

async fn connect(mapper: &Mapper, area: AreaId, from: i32, to: i32, direction: ExitDirection) {
    apply(
        mapper,
        area,
        vec![AreaMutation::CreateExit {
            room_source: None,
            room_number: RoomNumber(from),
            body: ExitArgs {
                from_direction: direction,
                to_area_id: Some(area),
                to_room_number: Some(RoomNumber(to)),
                ..ExitArgs::default()
            },
        }],
    )
    .await;
}

async fn seed_authored_orthogonal(mapper: &Mapper, name: &str) -> (AreaId, ConnectionId) {
    use smudgy_cloud::connection_geometry::{orthogonalize_route, port_position, stub_tip};
    let area = seed_area(mapper, name, &[(0, 0, 0), (4, 3, 0)]).await;
    // Stored host coordinates can be fractional even when the layout model
    // plans integral cells. Authored legs must use the actual endpoint deltas.
    apply(
        mapper,
        area,
        vec![
            AreaMutation::UpsertRoom {
                room_source: None,
                room_number: RoomNumber(1),
                body: RoomUpdates {
                    x: Some(0.25),
                    y: Some(0.5),
                    ..RoomUpdates::default()
                },
            },
            AreaMutation::UpsertRoom {
                room_source: None,
                room_number: RoomNumber(2),
                body: RoomUpdates {
                    x: Some(4.125),
                    y: Some(3.75),
                    ..RoomUpdates::default()
                },
            },
        ],
    )
    .await;
    connect(mapper, area, 1, 2, ExitDirection::East).await;
    let (id, points) = {
        let atlas = mapper.get_current_atlas();
        let snapshot = atlas.get_area(&area).unwrap();
        let id = snapshot.get_room(&RoomNumber(1)).unwrap().get_exits()[0].connection_id;
        let connection = snapshot.get_connection(id).unwrap();
        let tip = |endpoint: smudgy_cloud::ConnectionEndpoint| {
            let room = snapshot.get_room(&endpoint.room_number).unwrap();
            stub_tip(
                port_position(
                    MapPoint::new(room.get_x(), room.get_y()),
                    endpoint.side,
                    endpoint.port_offset,
                ),
                endpoint.side,
            )
        };
        let points = orthogonalize_route(
            tip(connection.endpoint_a),
            &[MapPoint::new(3.0, 7.0)],
            tip(connection.endpoint_b.unwrap()),
        )
        .unwrap();
        (id, points)
    };
    apply(
        mapper,
        area,
        vec![AreaMutation::UpdateConnection {
            connection_id: id,
            body: ConnectionUpdates {
                routing: Some(ConnectionRouting::Manual),
                segment_shape: Some(SegmentShape::Orthogonal),
                route_points: Some(points),
                ..ConnectionUpdates::default()
            },
        }],
    )
    .await;
    (area, id)
}

async fn expect_line(session: &mut Session, needle: &str) {
    assert!(
        session
            .wait_until(|lines| lines.iter().any(|line| line.contains(needle)))
            .await,
        "missing {needle}:\n{}",
        session.transcript()
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn installed_manual_reflows_commit_routes_acknowledge_and_reject_stale_results() {
    install_packages();
    let mapper = local_mapper(&smudgy_home().join("manual-map-store"));
    let bounded = seed_area(&mapper, "Bounded Rooms", &[(0, 0, 0), (20, 0, 0)]).await;
    connect(&mapper, bounded, 1, 2, ExitDirection::East).await;
    let perfect = seed_area(
        &mapper,
        "Perfect Levels",
        &[
            (0, 0, 1),
            (1, 0, 0),
            (2, 0, 1),
            (0, 1, 0),
            (1, 1, 1),
            (2, 1, 0),
        ],
    )
    .await;
    for (from, to, direction) in [
        (4, 1, ExitDirection::Down),
        (1, 2, ExitDirection::South),
        (2, 4, ExitDirection::Down),
        (5, 4, ExitDirection::South),
        (6, 4, ExitDirection::Down),
        (3, 4, ExitDirection::North),
        (2, 5, ExitDirection::West),
        (5, 1, ExitDirection::South),
    ] {
        connect(&mapper, perfect, from, to, direction).await;
    }
    let route_id = {
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&perfect).unwrap();
        area.get_room(&RoomNumber(6)).unwrap().get_exits()[0].connection_id
    };
    apply(
        &mapper,
        perfect,
        vec![AreaMutation::UpdateConnection {
            connection_id: route_id,
            body: ConnectionUpdates {
                routing: Some(ConnectionRouting::Automatic),
                ..ConnectionUpdates::default()
            },
        }],
    )
    .await;
    let batch_cells: Vec<_> = (0_i16..300)
        .map(|index| (index % 20, index / 20, 0))
        .collect();
    let batched = seed_area(&mapper, "Large Batch Rooms", &batch_cells).await;
    connect(&mapper, batched, 1, 300, ExitDirection::East).await;
    connect(&mapper, batched, 2, 299, ExitDirection::South).await;
    let (automatic_id, authored_id) = {
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&batched).unwrap();
        (
            area.get_room(&RoomNumber(1)).unwrap().get_exits()[0].connection_id,
            area.get_room(&RoomNumber(2)).unwrap().get_exits()[0].connection_id,
        )
    };
    let authored_points = vec![MapPoint::new(7.0, 8.0), MapPoint::new(9.0, 10.0)];
    apply(
        &mapper,
        batched,
        vec![
            AreaMutation::UpdateConnection {
                connection_id: automatic_id,
                body: ConnectionUpdates {
                    routing: Some(ConnectionRouting::Automatic),
                    ..ConnectionUpdates::default()
                },
            },
            AreaMutation::UpdateConnection {
                connection_id: authored_id,
                body: ConnectionUpdates {
                    routing: Some(ConnectionRouting::Manual),
                    route_points: Some(authored_points.clone()),
                    ..ConnectionUpdates::default()
                },
            },
        ],
    )
    .await;
    let detour = seed_area(&mapper, "Detour", &[(0, 0, 0), (2, 0, 0), (1, 0, 0)]).await;
    connect(&mapper, detour, 1, 2, ExitDirection::East).await;
    let detour_id = mapper
        .get_current_atlas()
        .get_area(&detour)
        .unwrap()
        .get_room(&RoomNumber(1))
        .unwrap()
        .get_exits()[0]
        .connection_id;
    let (unequal, unequal_id) = seed_authored_orthogonal(&mapper, "Unequal Authored").await;
    let (control, control_id) = seed_authored_orthogonal(&mapper, "Control Authored Route").await;
    apply(
        &mapper,
        control,
        vec![
            AreaMutation::UpsertRoom {
                room_source: None,
                room_number: RoomNumber(1),
                body: RoomUpdates {
                    x: Some(1.0),
                    y: Some(2.0),
                    level: Some(1),
                    ..RoomUpdates::default()
                },
            },
            AreaMutation::UpsertRoom {
                room_source: None,
                room_number: RoomNumber(2),
                body: RoomUpdates {
                    x: Some(3.0),
                    y: Some(4.0),
                    level: Some(1),
                    ..RoomUpdates::default()
                },
            },
        ],
    )
    .await;
    let expected_unequal_points = mapper
        .get_current_atlas()
        .get_area(&control)
        .unwrap()
        .get_connection(control_id)
        .unwrap()
        .route_points
        .clone();
    let location_aliases = format!(
        r#"import {{ createAlias, echo, mapper }} from "smudgy:core";
import {{ layoutSnapshotKey, loadLayoutModel }} from "smudgy://kapusniak/map-layout";
import {{ applyManualLayout }} from "smudgy:procedures/kapusniak/nukefire-mapper";
createAlias(/^locate-bounded$/, () => {{ mapper.setCurrentLocation("{bounded}", 1); echo("BOUNDED-LOCATED"); }});
createAlias(/^locate-perfect$/, () => {{ mapper.setCurrentLocation("{perfect}", 1); echo("PERFECT-LOCATED"); }});
createAlias(/^locate-batched$/, () => {{ mapper.setCurrentLocation("{batched}", 1); echo("BATCHED-LOCATED"); }});
createAlias(/^locate-detour$/, () => {{ mapper.setCurrentLocation("{detour}", 1); echo("DETOUR-LOCATED"); }});
createAlias(/^locate-unequal$/, () => {{ mapper.setCurrentLocation("{unequal}", 1); echo("UNEQUAL-LOCATED"); }});
createAlias(/^trusted-manual-request$/, () => {{
  const location = mapper.getCurrentLocation();
  const snapshot = layoutSnapshotKey(loadLayoutModel(location.area));
  applyManualLayout.post({{ requestId: "trusted-main-request", areaUuid: location.area,
    sourceSnapshotKey: snapshot, plannedSnapshotKey: snapshot, moves: [] }});
}});"#
    );
    std::fs::write(
        smudgy_home().join(SERVER).join("modules/manual-locate.ts"),
        location_aliases,
    )
    .unwrap();
    let mut session = start_session(SERVER, 9590, &mapper).await;
    expect_line(&mut session, "MANUAL-COMMANDS-READY").await;

    session.send("locate-bounded");
    expect_line(&mut session, "BOUNDED-LOCATED").await;
    session.send("nf reflow");
    expect_line(&mut session, "Bounded reflow moved").await;
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&bounded).unwrap();
    let west = area.get_room(&RoomNumber(1)).unwrap();
    let east = area.get_room(&RoomNumber(2)).unwrap();
    assert!(
        (east.get_x() - west.get_x()).abs() < 20.0,
        "bounded reflow commits its improvement"
    );
    assert!(
        area.get_property(SETTLEMENT).unwrap_or_default().is_empty(),
        "bounded work cannot claim perfect effort"
    );
    drop(atlas);

    session.send("locate-perfect");
    expect_line(&mut session, "PERFECT-LOCATED").await;
    session.send("nf reflow perfect");
    expect_line(&mut session, "Perfect reflow moved").await;
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&perfect).unwrap();
    let room4 = area.get_room(&RoomNumber(4)).unwrap();
    let room6 = area.get_room(&RoomNumber(6)).unwrap();
    assert_ne!(
        room4.get_level(),
        room6.get_level(),
        "the perfect layout really changes connection kind"
    );
    let connection = area.get_connection(route_id).unwrap();
    assert_eq!(connection.kind, ConnectionKind::CrossLevel);
    assert_eq!(connection.routing, ConnectionRouting::Simple);
    let settlement: serde_json::Value = serde_json::from_str(
        area.get_property(SETTLEMENT)
            .expect("manual settlement recorded before success"),
    )
    .unwrap();
    assert_eq!(settlement["v"], json!(3));
    assert!(
        settlement["c"]
            .as_array()
            .unwrap()
            .iter()
            .any(|context| context["e"] == 2),
        "actual cross-isolate bridge records perfect effort: {settlement}"
    );
    assert_eq!(
        session
            .lines
            .iter()
            .filter(|line| line.starts_with("MANUAL-ACK ") && line.contains(" true "))
            .count(),
        2,
        "each command gets an accepted acknowledgement before success"
    );
    drop(atlas);

    session.send("locate-bounded");
    session.run_for(std::time::Duration::from_millis(50)).await;
    let before_invalid = mapper
        .get_current_atlas()
        .get_area(&bounded)
        .unwrap()
        .get_room(&RoomNumber(1))
        .unwrap()
        .get_x();
    session.send("manual-invalid-final");
    expect_line(&mut session, "MANUAL-ACK deliberately-invalid-final false").await;
    assert!(
        (mapper
            .get_current_atlas()
            .get_area(&bounded)
            .unwrap()
            .get_room(&RoomNumber(1))
            .unwrap()
            .get_x()
            - before_invalid)
            .abs()
            < f32::EPSILON,
        "candidate final key rejection must precede all writes"
    );
    session.send("manual-stale");
    expect_line(&mut session, "MANUAL-ACK deliberately-stale false").await;
    let changed = session
        .lines
        .iter()
        .find_map(|line| line.strip_prefix("MANUAL-STALE-CHANGED "))
        .unwrap()
        .parse::<f32>()
        .unwrap();
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&bounded).unwrap();
    assert!(
        (area.get_room(&RoomNumber(1)).unwrap().get_x() - changed).abs() < f32::EPSILON,
        "stale source rejection cannot overwrite the intervening move"
    );
    assert!(area.get_property(SETTLEMENT).unwrap_or_default().is_empty());
    drop(atlas);

    // A sibling scripts isolate plans against shared maps, but the oldest
    // session alone commits. Its receipt is routed back to this caller.
    let mut sibling = start_session(SERVER, 9591, &mapper).await;
    expect_line(&mut sibling, "MANUAL-COMMANDS-READY").await;
    sibling.send("locate-bounded");
    expect_line(&mut sibling, "BOUNDED-LOCATED").await;
    sibling.send("nf reflow");
    expect_line(&mut sibling, "Bounded reflow moved").await;
    assert!(
        sibling
            .lines
            .iter()
            .any(|line| { line.starts_with("MANUAL-ACK 9591:") && line.contains(" true ") }),
        "the sibling receives the owner's accepted receipt:\n{}",
        sibling.transcript()
    );
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&bounded).unwrap();
    let west = area.get_room(&RoomNumber(1)).unwrap();
    let east = area.get_room(&RoomNumber(2)).unwrap();
    assert!(
        (east.get_x() - west.get_x() - 1.0).abs() < f32::EPSILON,
        "the owner commits the sibling's bounded result"
    );
    sibling.shutdown();
    assert!(
        !sibling.transcript().contains("Reflow failed"),
        "{}",
        sibling.transcript()
    );

    // More than 256 coordinate edits force multiple host envelopes. Both
    // links are temporarily between levels while their far endpoints await
    // a later envelope, even though their final kind stays Internal.
    session.send("locate-batched");
    expect_line(&mut session, "BATCHED-LOCATED").await;
    session.send("manual-batch");
    expect_line(&mut session, "MANUAL-ACK large-envelope-batch").await;
    assert!(
        session
            .lines
            .iter()
            .any(|line| line.contains("MANUAL-ACK large-envelope-batch true")),
        "the 300-room commit must keep every intermediate connection valid:\n{}",
        session.transcript()
    );
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&batched).unwrap();
    for room in area.get_rooms() {
        assert_eq!(room.get_level(), 1);
    }
    let automatic = area.get_connection(automatic_id).unwrap();
    assert_eq!(automatic.kind, ConnectionKind::Internal);
    assert_eq!(automatic.routing, ConnectionRouting::Automatic);
    let authored = area.get_connection(authored_id).unwrap();
    assert_eq!(authored.kind, ConnectionKind::Internal);
    assert_eq!(authored.routing, ConnectionRouting::Manual);
    assert_eq!(
        authored.route_points,
        authored_points
            .iter()
            .map(|point| MapPoint::new(point.x + 3.0, point.y + 2.0))
            .collect::<Vec<_>>(),
        "a shared translation spanning envelopes moves authored points exactly once"
    );
    drop(atlas);

    // A valid plan can only change generated presentation routes. No room
    // move is needed to carry the advisory detour through the same bridge.
    session.send("locate-detour");
    expect_line(&mut session, "DETOUR-LOCATED").await;
    session.send("manual-detour");
    expect_line(&mut session, "MANUAL-ACK route-only-amendment true").await;
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&detour).unwrap();
    let connection = area.get_connection(detour_id).unwrap();
    assert_eq!(connection.routing, ConnectionRouting::Automatic);
    assert!(
        connection
            .route_points
            .iter()
            .any(|point| (point.y - 7.0).abs() < f32::EPSILON),
        "the zero-move amendment's distinctive detour is persisted: {:?}",
        connection.route_points
    );
    for (number, x) in [(1, 0.0), (2, 2.0), (3, 1.0)] {
        assert!((area.get_room(&RoomNumber(number)).unwrap().get_x() - x).abs() < f32::EPSILON);
    }
    let unshifted_route = connection.route_points.clone();
    drop(atlas);
    session.send("manual-shift-detour");
    expect_line(&mut session, "MANUAL-ACK shifted-generated-amendment true").await;
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&detour).unwrap();
    let connection = area.get_connection(detour_id).unwrap();
    assert_eq!(
        connection.route_points,
        unshifted_route
            .iter()
            .map(|point| MapPoint::new(point.x + 3.0, point.y + 2.0))
            .collect::<Vec<_>>(),
        "same-level XY room moves must not translate newly generated final points twice"
    );
    assert_eq!(connection.routing, ConnectionRouting::Automatic);
    drop(atlas);
    session.send("trusted-manual-request");
    expect_line(&mut session, "MANUAL-ACK trusted-main-request true").await;

    session.send("locate-unequal");
    expect_line(&mut session, "UNEQUAL-LOCATED").await;
    session.send("manual-unequal-authored");
    expect_line(&mut session, "MANUAL-ACK unequal-authored-move true").await;
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&unequal).unwrap();
    let connection = area.get_connection(unequal_id).unwrap();
    assert_eq!(connection.routing, ConnectionRouting::Manual);
    assert_eq!(connection.segment_shape, SegmentShape::Orthogonal);
    assert_eq!(
        connection.route_points, expected_unequal_points,
        "restored authored legs match the ordinary host room-move contract"
    );
    session.shutdown();
    assert!(
        !session.transcript().contains("invalid_routing"),
        "{}",
        session.transcript()
    );
    assert!(
        !session.transcript().contains("Reflow failed"),
        "{}",
        session.transcript()
    );
    assert!(
        !session.transcript().contains("MANUAL-FIXTURE-ERROR"),
        "{}",
        session.transcript()
    );
}
