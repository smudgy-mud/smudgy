//! Live smoke test for the relocation server-copy gate, against a REAL cloud
//! API. Ignored by default and additionally env-gated, so neither `cargo
//! test` nor `--ignored` sweeps can hit the network by accident.
//!
//! Run it deliberately:
//!
//! ```text
//! SMUDGY_LIVE_SMOKE=1 \
//! SMUDGY_LIVE_TOKEN=smudgy_sess_…              # session token or API key
//! SMUDGY_LIVE_BASE_URL=https://api.dev.smudgy.org   # optional; this is the default
//!     cargo test -p smudgy_cloud --test live_smoke -- --ignored --nocapture
//! ```
//!
//! The production host is refused unless `SMUDGY_LIVE_ALLOW_PROD=1` is also
//! set. Every object the test creates is named `smoke-copy-gate-<suffix>` and
//! deleted on both success and failure; anything it could not delete is
//! reported in the failure message.
//!
//! What it validates, end to end against the live server:
//!
//! 1. An eligible cloud→cloud Copy (no cross-area exits, acknowledged rev)
//!    takes the server-side clone (`POST /areas/{id}/copy`): the destination
//!    carries `copied_from_*` provenance, which only the server clone mints —
//!    the replay path scrubs it — so provenance IS the gate-path witness.
//! 2. The clone's content matches the source: rooms (numbers preserved),
//!    exits, connection grouping, labels, shapes, properties, tags — with
//!    fresh exit/connection/label/shape identities. Atlas placement honored.
//! 3. An ineligible source (cross-area exit) falls back to envelope replay
//!    and still succeeds: no provenance, outbound link demoted to dangling,
//!    loose placement honored, no `(relocating)` debris left behind.

#![allow(
    clippy::too_many_lines,
    clippy::similar_names,
    clippy::items_after_statements
)]

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use smudgy_cloud::mapper::RoomKey;
use smudgy_cloud::{
    AreaId, AreaWithDetails, AtlasId, CLIENT_VERSION, CachedCloudMapper, CloudMapper, Credential,
    CredentialSource, ExitArgs, ExitDirection, HorizontalAlignment, LabelArgs, MapDestination,
    MapStorage, Mapper, RelocationMode, RoomNumber, RoomUpdates, ShapeArgs, ShapeType,
    VerticalAlignment,
};
use uuid::Uuid;

const DEFAULT_BASE_URL: &str = "https://api.dev.smudgy.org";
const PROD_BASE_URL: &str = "https://api.smudgy.org";
const SMOKE_PREFIX: &str = "smoke-copy-gate-";

macro_rules! ensure {
    ($cond:expr, $($arg:tt)+) => {
        if !($cond) {
            return Err(format!($($arg)+));
        }
    };
}

/// The env contract; `None` means the test should self-skip.
fn live_env() -> Option<(String, String)> {
    std::env::var("SMUDGY_LIVE_SMOKE").ok()?;
    let token = std::env::var("SMUDGY_LIVE_TOKEN").ok()?;
    let base_url =
        std::env::var("SMUDGY_LIVE_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
    Some((base_url, token))
}

/// Objects created on the live account; deleted best-effort by [`cleanup`].
#[derive(Default)]
struct Created {
    areas: Vec<AreaId>,
    atlases: Vec<AtlasId>,
}

/// Best-effort deletion of everything the run created, newest first (copies
/// before their sources). Returns human-readable failures, empty on a clean
/// sweep.
async fn cleanup(mapper: &Mapper, created: &Created) -> Vec<String> {
    let mut failures = Vec::new();
    for area_id in created.areas.iter().rev() {
        if let Err(error) = mapper.delete_area_and_wait(*area_id).await {
            failures.push(format!("area {area_id} not deleted: {error}"));
        }
    }
    for atlas_id in created.atlases.iter().rev() {
        if let Err(error) = mapper.delete_atlas(*atlas_id).await {
            failures.push(format!("atlas {atlas_id} not deleted: {error}"));
        }
    }
    // Authoritative post-sweep check: no smoke-named object may survive.
    match mapper.list_areas().await {
        Ok(areas) => {
            for area in areas {
                if area.name.starts_with(SMOKE_PREFIX) {
                    failures.push(format!(
                        "area '{}' ({}) still on the account after cleanup",
                        area.name, area.id
                    ));
                }
            }
        }
        Err(error) => failures.push(format!("post-cleanup area listing failed: {error}")),
    }
    match mapper.list_atlases().await {
        Ok(atlases) => {
            for atlas in atlases {
                if atlas.name.starts_with(SMOKE_PREFIX) {
                    failures.push(format!(
                        "atlas '{}' ({}) still on the account after cleanup",
                        atlas.name, atlas.id
                    ));
                }
            }
        }
        Err(error) => failures.push(format!("post-cleanup atlas listing failed: {error}")),
    }
    failures
}

/// Raw `GET /areas/{id}` outside the client stack: server-truth evidence that
/// does not pass through the mapper's caches.
async fn fetch_area_raw(
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    area_id: AreaId,
) -> Result<serde_json::Value, String> {
    let response = http
        .get(format!("{base_url}/areas/{area_id}"))
        .header("authorization", format!("Bearer {token}"))
        .header("x-smudgy-client-version", CLIENT_VERSION)
        .send()
        .await
        .map_err(|error| format!("raw GET /areas/{area_id} failed: {error}"))?;
    let status = response.status();
    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|error| format!("raw GET /areas/{area_id}: bad body: {error}"))?;
    ensure!(
        status.is_success() && body["success"] == serde_json::Value::Bool(true),
        "raw GET /areas/{area_id}: status {status}, body {body}"
    );
    Ok(body["data"].clone())
}

/// Where an exit leads, normalized so source and copy can be compared across
/// differing area ids.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum Target {
    InArea(i32),
    Dangling,
    External(String),
}

type ExitFingerprint = (
    String, // from_direction
    Target,
    Option<String>, // to_direction
    String,         // path
    String,         // command
    u32,            // weight bits
    (bool, bool, bool, bool), // hidden/closed/locked/secret
);

/// Fingerprints one document's per-room content with area-id-relative exit
/// targets. `demote_external`: cross-area targets are expected to arrive
/// dangling on the other side (the replay contract).
fn room_fingerprints(
    doc: &AreaWithDetails,
    demote_external: bool,
) -> BTreeMap<i32, (Vec<String>, Vec<ExitFingerprint>)> {
    let own = doc.area.id;
    doc.rooms
        .iter()
        .map(|room| {
            let mut meta = vec![
                room.title.clone(),
                room.description.clone(),
                room.level.to_string(),
                room.x.to_bits().to_string(),
                room.y.to_bits().to_string(),
                room.color.clone(),
                room.is_secret.to_string(),
                format!("{:?}", room.external_id),
                format!("{:?}", room.tags),
            ];
            let mut properties: Vec<String> = room
                .properties
                .iter()
                .map(|property| format!("{}={} secret={}", property.name, property.value, property.is_secret))
                .collect();
            properties.sort();
            meta.extend(properties);
            let mut exits: Vec<ExitFingerprint> = room
                .exits
                .iter()
                .map(|exit| {
                    let target = match exit.to_area_id {
                        Some(area) if area == own => {
                            exit.to_room_number.map_or(Target::Dangling, |number| {
                                Target::InArea(number.0)
                            })
                        }
                        Some(area) if demote_external => {
                            let _ = area;
                            Target::Dangling
                        }
                        Some(area) => Target::External(area.to_string()),
                        None => Target::Dangling,
                    };
                    // A demoted target also sheds its room number/direction.
                    let to_direction = if target == Target::Dangling {
                        None
                    } else {
                        exit.to_direction.map(|direction| format!("{direction:?}"))
                    };
                    (
                        format!("{:?}", exit.from_direction),
                        target,
                        to_direction,
                        exit.path.clone(),
                        exit.command.clone(),
                        exit.weight.to_bits(),
                        (exit.is_hidden, exit.is_closed, exit.is_locked, exit.is_secret),
                    )
                })
                .collect();
            exits.sort();
            (room.room_number.0, (meta, exits))
        })
        .collect()
}

/// The connection partition: which (room, direction) member exits share one
/// connection, and that connection's kind. Ids are erased so source and copy
/// compare structurally. `demote_external` maps the source's `External`
/// expectation onto the replay contract's `Dangling`.
fn connection_partition(doc: &AreaWithDetails, demote_external: bool) -> BTreeSet<String> {
    let mut members: HashMap<_, BTreeSet<String>> = HashMap::new();
    for room in &doc.rooms {
        for exit in &room.exits {
            members
                .entry(exit.connection_id)
                .or_default()
                .insert(format!("{}:{:?}", room.room_number.0, exit.from_direction));
        }
    }
    doc.connections
        .iter()
        .map(|connection| {
            let mut kind = format!("{:?}", connection.kind);
            if demote_external && kind == "External" {
                kind = "Dangling".to_string();
            }
            let empty = BTreeSet::new();
            let group = members.get(&connection.id).unwrap_or(&empty);
            format!("{kind} {group:?}")
        })
        .collect()
}

fn label_fingerprints(doc: &AreaWithDetails) -> BTreeSet<String> {
    doc.labels
        .iter()
        .map(|label| {
            format!(
                "{:?} {} {} {} {} {} {:?} {:?} {} {} {} {} {}",
                label.text,
                label.level,
                label.x.to_bits(),
                label.y.to_bits(),
                label.width.to_bits(),
                label.height.to_bits(),
                label.horizontal_alignment,
                label.vertical_alignment,
                label.color,
                label.background_color,
                label.font_size,
                label.font_weight,
                label.is_secret,
            )
        })
        .collect()
}

fn shape_fingerprints(doc: &AreaWithDetails) -> BTreeSet<String> {
    doc.shapes
        .iter()
        .map(|shape| {
            format!(
                "{:?} {} {} {} {} {} {:?} {:?} {} {} {}",
                shape.shape_type,
                shape.level,
                shape.x.to_bits(),
                shape.y.to_bits(),
                shape.width.to_bits(),
                shape.height.to_bits(),
                shape.background_color,
                shape.stroke_color,
                shape.border_radius.to_bits(),
                shape.stroke_width.to_bits(),
                shape.is_secret,
            )
        })
        .collect()
}

fn area_properties(doc: &AreaWithDetails) -> BTreeSet<String> {
    doc.properties
        .iter()
        .map(|property| format!("{}={} secret={}", property.name, property.value, property.is_secret))
        .collect()
}

/// Structural equality of a copy against its source (content identical, all
/// object identities fresh). `demote_external`: outbound cross-area links are
/// expected dangling in the copy (the replay contract; the server-clone path
/// is only ever taken when there are none, so passing `false` there is
/// equally strict).
fn compare_copy(
    src: &AreaWithDetails,
    dst: &AreaWithDetails,
    demote_external: bool,
) -> Result<(), String> {
    ensure!(
        dst.area.name == src.area.name,
        "copy name '{}' differs from source '{}'",
        dst.area.name,
        src.area.name
    );
    let src_rooms = room_fingerprints(src, demote_external);
    let dst_rooms = room_fingerprints(dst, false);
    ensure!(
        src_rooms == dst_rooms,
        "room content mismatch:\n  source: {src_rooms:#?}\n  copy: {dst_rooms:#?}"
    );
    let src_partition = connection_partition(src, demote_external);
    let dst_partition = connection_partition(dst, false);
    ensure!(
        src_partition == dst_partition,
        "connection partition mismatch:\n  source: {src_partition:#?}\n  copy: {dst_partition:#?}"
    );
    ensure!(
        label_fingerprints(src) == label_fingerprints(dst),
        "label mismatch:\n  source: {:#?}\n  copy: {:#?}",
        label_fingerprints(src),
        label_fingerprints(dst)
    );
    ensure!(
        shape_fingerprints(src) == shape_fingerprints(dst),
        "shape mismatch:\n  source: {:#?}\n  copy: {:#?}",
        shape_fingerprints(src),
        shape_fingerprints(dst)
    );
    ensure!(
        area_properties(src) == area_properties(dst),
        "area property mismatch:\n  source: {:#?}\n  copy: {:#?}",
        area_properties(src),
        area_properties(dst)
    );

    // Fresh identities: no exit/connection/label/shape id may survive.
    let src_exit_ids: BTreeSet<String> = src
        .rooms
        .iter()
        .flat_map(|room| room.exits.iter().map(|exit| exit.id.to_string()))
        .collect();
    let dst_exit_ids: BTreeSet<String> = dst
        .rooms
        .iter()
        .flat_map(|room| room.exits.iter().map(|exit| exit.id.to_string()))
        .collect();
    ensure!(
        src_exit_ids.is_disjoint(&dst_exit_ids),
        "copy reuses source exit ids"
    );
    let src_connection_ids: BTreeSet<String> = src
        .connections
        .iter()
        .map(|connection| connection.id.to_string())
        .collect();
    let dst_connection_ids: BTreeSet<String> = dst
        .connections
        .iter()
        .map(|connection| connection.id.to_string())
        .collect();
    ensure!(
        src_connection_ids.is_disjoint(&dst_connection_ids),
        "copy reuses source connection ids"
    );
    let src_label_ids: BTreeSet<String> =
        src.labels.iter().map(|label| label.id.to_string()).collect();
    let dst_label_ids: BTreeSet<String> =
        dst.labels.iter().map(|label| label.id.to_string()).collect();
    ensure!(
        src_label_ids.is_disjoint(&dst_label_ids),
        "copy reuses source label ids"
    );
    let src_shape_ids: BTreeSet<String> =
        src.shapes.iter().map(|shape| shape.id.to_string()).collect();
    let dst_shape_ids: BTreeSet<String> =
        dst.shapes.iter().map(|shape| shape.id.to_string()).collect();
    ensure!(
        src_shape_ids.is_disjoint(&dst_shape_ids),
        "copy reuses source shape ids"
    );
    Ok(())
}

async fn wait_until(what: &str, mut condition: impl FnMut() -> bool) -> Result<(), String> {
    for _ in 0..2400u32 {
        if condition() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(format!("timed out waiting for {what}"))
}

async fn settle(mapper: &Mapper, what: &str) -> Result<(), String> {
    match mapper.wait_for_sync_completion(180).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(format!("sync did not settle within 180s while {what}")),
        Err(()) => Err(format!("sync operations FAILED while {what}")),
    }
}

fn room_key(area_id: AreaId, number: i32) -> RoomKey {
    RoomKey {
        area_id,
        room_number: RoomNumber(number),
    }
}

fn room(title: &str, x: f32, y: f32) -> RoomUpdates {
    RoomUpdates {
        title: Some(title.to_string()),
        description: Some(format!("{title} description")),
        level: Some(0),
        x: Some(x),
        y: Some(y),
        color: Some("#a0b0c0".to_string()),
        is_secret: None,
        external_id: None,
    }
}

fn exit_to(
    to_area_id: Option<AreaId>,
    to_room_number: Option<i32>,
    from: ExitDirection,
    to: Option<ExitDirection>,
) -> ExitArgs {
    ExitArgs {
        id: None,
        connection_id: None,
        new_connection_id: None,
        is_secret: None,
        from_direction: from,
        to_area_id,
        to_room_number: to_room_number.map(RoomNumber),
        to_direction: to,
        path: None,
        is_hidden: false,
        is_closed: false,
        is_locked: false,
        weight: 1.0,
        command: None,
    }
}

async fn run(
    mapper: &Mapper,
    http: &reqwest::Client,
    base_url: &str,
    token: &str,
    suffix: &str,
    created: &mut Created,
) -> Result<(), String> {
    // ---- Phase 0: the sync engine must be live (first tick done). --------
    wait_until("initial sync tick", || {
        mapper.sync_status().last_sync.is_some()
    })
    .await?;

    // ---- Phase 1: fixtures ----------------------------------------------
    let atlas = mapper
        .create_atlas_at(format!("{SMOKE_PREFIX}atlas-{suffix}"), MapStorage::Cloud)
        .await
        .map_err(|error| format!("create atlas: {error}"))?;
    created.atlases.push(atlas.id);

    let src_name = format!("{SMOKE_PREFIX}src-{suffix}");
    let src = mapper
        .create_area_at(src_name.clone(), MapDestination::loose(MapStorage::Cloud))
        .await
        .map_err(|error| format!("create source area: {error}"))?;
    created.areas.push(src);

    // Rooms with deliberately non-contiguous numbers.
    for (number, title, x, y) in [
        (1, "Gatehouse", 0.0, 0.0),
        (2, "Great Hall", 24.0, 0.0),
        (7, "Undercroft", 24.0, 24.0),
    ] {
        mapper
            .upsert_room(room_key(src, number), room(title, x, y))
            .map_err(|error| format!("upsert room {number}: {error}"))?;
    }
    mapper
        .set_room_property(room_key(src, 1), "visited".to_string(), "true".to_string())
        .map_err(|error| format!("room property: {error}"))?;
    mapper
        .add_room_tag(room_key(src, 2), "shop".to_string())
        .map_err(|error| format!("room tag: {error}"))?;
    mapper
        .set_area_property(src, "smoke".to_string(), "copy-gate".to_string())
        .map_err(|error| format!("area property: {error}"))?;

    // Paired 1 E<->W 2, one-way 2 S->7, dangling 7 N.
    mapper
        .create_exit(
            room_key(src, 1),
            exit_to(Some(src), Some(2), ExitDirection::East, Some(ExitDirection::West)),
        )
        .await
        .map_err(|error| format!("paired exit: {error}"))?;
    mapper
        .create_exit(
            room_key(src, 2),
            exit_to(Some(src), Some(7), ExitDirection::South, None),
        )
        .await
        .map_err(|error| format!("one-way exit: {error}"))?;
    mapper
        .create_exit(
            room_key(src, 7),
            exit_to(None, None, ExitDirection::North, None),
        )
        .await
        .map_err(|error| format!("dangling exit: {error}"))?;

    mapper
        .create_label(
            src,
            LabelArgs {
                id: None,
                is_secret: None,
                level: 0,
                x: 4.0,
                y: -10.0,
                width: 80.0,
                height: 16.0,
                horizontal_alignment: HorizontalAlignment::Center,
                vertical_alignment: VerticalAlignment::Center,
                text: "Smoke Keep".to_string(),
                color: "#101010".to_string(),
                background_color: Some("#fafafa".to_string()),
                font_size: 12,
                font_weight: 600,
            },
        )
        .await
        .map_err(|error| format!("label: {error}"))?;
    mapper
        .create_shape(
            src,
            ShapeArgs {
                id: None,
                is_secret: None,
                level: 0,
                x: -8.0,
                y: -16.0,
                width: 64.0,
                height: 56.0,
                background_color: Some("#eef2f6".to_string()),
                stroke_color: Some("#223344".to_string()),
                shape_type: ShapeType::RoundedRectangle,
                border_radius: 3.0,
                stroke_width: Some(1.5),
            },
        )
        .await
        .map_err(|error| format!("shape: {error}"))?;

    settle(mapper, "populating the source area").await?;

    let src_doc = mapper
        .export_area(src)
        .await
        .map_err(|error| format!("export source: {error}"))?;
    ensure!(src_doc.rooms.len() == 3, "source should hold 3 rooms");
    ensure!(
        src_doc.connections.len() == 3,
        "source should hold 3 connections (paired, one-way, dangling), got {}",
        src_doc.connections.len()
    );

    // ---- Phase 2: eligible copy must take the server clone ---------------
    let started = Instant::now();
    let relocation = mapper
        .relocate_areas(
            vec![src],
            MapDestination::in_atlas(MapStorage::Cloud, atlas.id),
            RelocationMode::Copy,
        )
        .await
        .map_err(|error| format!("eligible copy failed: {error}"))?;
    let eligible_elapsed = started.elapsed();
    ensure!(
        relocation.destination_ids.len() == 1,
        "one destination expected"
    );
    let dest = relocation.destination_ids[0];
    created.areas.push(dest);
    ensure!(dest != src, "copy must mint a fresh area id");
    eprintln!("eligible copy: {src} -> {dest} in {eligible_elapsed:?}");

    // Server-truth check, outside the client stack: only the server clone
    // records copied_from provenance (the replay path scrubs it), so this is
    // the witness that `server_copy_applies` really routed to POST /copy.
    let raw = fetch_area_raw(http, base_url, token, dest).await?;
    ensure!(
        raw["copied_from_area_id"].as_str() == Some(src.to_string().as_str()),
        "destination lacks server-clone provenance; the gate did NOT take the \
         server copy path (copied_from_area_id = {})",
        raw["copied_from_area_id"]
    );
    ensure!(
        raw["atlas_id"].as_str() == Some(atlas.id.to_string().as_str()),
        "server clone not filed into the destination atlas (atlas_id = {})",
        raw["atlas_id"]
    );
    ensure!(
        raw["name"].as_str() == Some(src_name.as_str()),
        "server clone name should equal the source name, got {}",
        raw["name"]
    );
    eprintln!(
        "server clone provenance: copied_from_rev={} (source confirmed rev {}), clone rev={}",
        raw["copied_from_rev"], src_doc.area.rev, raw["rev"]
    );

    let dest_doc = mapper
        .export_area(dest)
        .await
        .map_err(|error| format!("export clone: {error}"))?;
    ensure!(
        dest_doc.area.copied_from_area_id == Some(src),
        "exported clone lost its provenance"
    );
    compare_copy(&src_doc, &dest_doc, false)?;
    ensure!(
        mapper.abandoned_relocation_areas().is_empty(),
        "server-clone path must not leave '(relocating)' debris"
    );

    // ---- Phase 3: cross-area exit forces the replay fallback -------------
    let ext = mapper
        .create_area_at(
            format!("{SMOKE_PREFIX}ext-{suffix}"),
            MapDestination::loose(MapStorage::Cloud),
        )
        .await
        .map_err(|error| format!("create external area: {error}"))?;
    created.areas.push(ext);
    mapper
        .upsert_room(room_key(ext, 1), room("Beyond the Wall", 0.0, 0.0))
        .map_err(|error| format!("external room: {error}"))?;
    mapper
        .create_exit(
            room_key(src, 2),
            exit_to(Some(ext), Some(1), ExitDirection::East, None),
        )
        .await
        .map_err(|error| format!("cross-area exit: {error}"))?;
    settle(mapper, "adding the cross-area exit").await?;

    let src_doc2 = mapper
        .export_area(src)
        .await
        .map_err(|error| format!("re-export source: {error}"))?;

    let started = Instant::now();
    let relocation2 = mapper
        .relocate_areas(
            vec![src],
            MapDestination::loose(MapStorage::Cloud),
            RelocationMode::Copy,
        )
        .await
        .map_err(|error| format!("ineligible copy (replay fallback) failed: {error}"))?;
    let replay_elapsed = started.elapsed();
    let dest2 = relocation2.destination_ids[0];
    created.areas.push(dest2);
    ensure!(dest2 != src && dest2 != dest, "replay copy must be a fresh area");
    eprintln!("replay copy: {src} -> {dest2} in {replay_elapsed:?}");

    let raw2 = fetch_area_raw(http, base_url, token, dest2).await?;
    ensure!(
        raw2["copied_from_area_id"].is_null(),
        "replay-path copy must NOT carry server-clone provenance \
         (copied_from_area_id = {}); the gate let a cross-area source through",
        raw2["copied_from_area_id"]
    );
    ensure!(
        raw2["atlas_id"].is_null(),
        "loose destination should have no atlas, got {}",
        raw2["atlas_id"]
    );
    ensure!(
        raw2["name"].as_str() == Some(src_name.as_str()),
        "replay copy should shed the in-progress marker, got {}",
        raw2["name"]
    );

    let dest2_doc = mapper
        .export_area(dest2)
        .await
        .map_err(|error| format!("export replay copy: {error}"))?;
    compare_copy(&src_doc2, &dest2_doc, true)?;
    // The outbound link really was demoted, not silently preserved.
    let demoted = dest2_doc.rooms.iter().all(|room| {
        room.exits
            .iter()
            .all(|exit| exit.to_area_id.is_none_or(|target| target == dest2))
    });
    ensure!(
        demoted,
        "replay copy still links outside itself — outbound exits must dangle"
    );
    ensure!(
        mapper.abandoned_relocation_areas().is_empty(),
        "replay path left '(relocating)' debris"
    );

    // ---- Phase 4: Copy mode left the source untouched ---------------------
    let src_after = mapper
        .export_area(src)
        .await
        .map_err(|error| format!("final export of source: {error}"))?;
    ensure!(
        src_after.rooms.len() == 3 && src_after.area.name == src_name,
        "source changed during copies"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live cloud smoke; set SMUDGY_LIVE_SMOKE=1 + SMUDGY_LIVE_TOKEN and run with --ignored"]
async fn relocation_copy_gate_live_smoke() {
    let Some((base_url, token)) = live_env() else {
        eprintln!("skipping: SMUDGY_LIVE_SMOKE / SMUDGY_LIVE_TOKEN not set");
        return;
    };
    assert!(
        base_url.trim_end_matches('/') != PROD_BASE_URL
            || std::env::var("SMUDGY_LIVE_ALLOW_PROD").is_ok(),
        "refusing to run the live smoke against production without SMUDGY_LIVE_ALLOW_PROD=1"
    );

    let credential = if token.starts_with("smudgy_sess_") {
        Credential::Session(token.clone())
    } else {
        Credential::ApiKey(token.clone())
    };
    let cache_dir: PathBuf =
        std::env::temp_dir().join(format!("smudgy-live-smoke-{}", Uuid::new_v4()));
    let credentials = CredentialSource::new(Some(credential));
    let backend = CachedCloudMapper::new(
        CloudMapper::with_credentials(base_url.clone(), credentials),
        &cache_dir,
    );
    let mapper = Mapper::new(Arc::new(backend), &cache_dir);
    let http = reqwest::Client::new();

    let suffix = Uuid::new_v4().simple().to_string()[..8].to_string();
    let mut created = Created::default();
    let result = run(&mapper, &http, &base_url, &token, &suffix, &mut created).await;
    let leftovers = cleanup(&mapper, &created).await;
    let _ = std::fs::remove_dir_all(&cache_dir);

    if let Err(message) = result {
        assert!(
            leftovers.is_empty(),
            "smoke FAILED: {message}\nAND cleanup left debris on the account: {leftovers:#?}"
        );
        panic!("smoke FAILED (account cleaned up): {message}");
    }
    assert!(
        leftovers.is_empty(),
        "smoke passed but cleanup left debris on the account: {leftovers:#?}"
    );
}
