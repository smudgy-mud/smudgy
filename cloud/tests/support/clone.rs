//! P4 — secrets tooling + clone: POST /areas/{id}/secret-marks,
//! GET /areas/{id}/secrets, GET /areas/{id}/preview, POST /areas/{id}/copy,
//! POST /atlases/{id}/copy. Mirrors `MapQueries` chunk D + the clone
//! materializer (redacted projection as owned rows, pairwise exit remap).

use std::collections::HashMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{
    authenticate, bad_request, created, gate_verified, not_found, ok, parse_area_id, parse_body,
};
use super::projection::project_area;
use super::state::{AreaRecord, AtlasRecord, Caps, MockState, RoomRecord};

pub type Shared = Arc<Mutex<MockState>>;

// ---------------------------------------------------------------------------
// POST /areas/{id}/secret-marks
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct RoomPropKey {
    room_number: i32,
    name: String,
}

#[derive(Deserialize)]
struct SecretMarksRequest {
    secret: bool,
    #[serde(default)]
    rooms: Vec<i32>,
    #[serde(default)]
    exits: Vec<Uuid>,
    #[serde(default)]
    labels: Vec<Uuid>,
    #[serde(default)]
    shapes: Vec<Uuid>,
    #[serde(default)]
    room_properties: Vec<RoomPropKey>,
    #[serde(default)]
    area_properties: Vec<String>,
}

/// POST /areas/{id}/secret-marks — CLEARED callers only; area-scoped updates;
/// foreign ids silently ignored; per-type matched-row counts.
pub async fn secret_marks(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: SecretMarksRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let Some(caps) = st.caps(viewer, area_id) else {
        return not_found();
    };
    if !caps.cleared() {
        return not_found();
    }

    let secret = req.secret;
    let mut bumps: Vec<bool> = Vec::new(); // per touched row: bump_public?

    let area = st.areas.get_mut(&area_id).expect("area exists");
    let mut rooms = 0u64;
    for n in &req.rooms {
        if let Some(room) = area.rooms.get_mut(n) {
            bumps.push(!(room.is_secret && secret));
            room.is_secret = secret;
            rooms += 1;
        }
    }
    let mut exits = 0u64;
    for id in &req.exits {
        if let Some(exit) = area.exits.iter_mut().find(|e| e.id == *id) {
            bumps.push(!(exit.is_secret && secret));
            exit.is_secret = secret;
            exits += 1;
        }
    }
    let mut labels = 0u64;
    for id in &req.labels {
        if let Some(label) = area.labels.iter_mut().find(|l| l.id == *id) {
            bumps.push(!(label.is_secret && secret));
            label.is_secret = secret;
            labels += 1;
        }
    }
    let mut shapes = 0u64;
    for id in &req.shapes {
        if let Some(shape) = area.shapes.iter_mut().find(|s| s.id == *id) {
            bumps.push(!(shape.is_secret && secret));
            shape.is_secret = secret;
            shapes += 1;
        }
    }
    let mut room_properties = 0u64;
    for key in &req.room_properties {
        if let Some(prop) = area
            .rooms
            .get_mut(&key.room_number)
            .and_then(|r| r.properties.get_mut(&key.name))
        {
            bumps.push(!(prop.is_secret && secret));
            prop.is_secret = secret;
            room_properties += 1;
        }
    }
    let mut area_properties = 0u64;
    for name in &req.area_properties {
        if let Some(prop) = area.properties.get_mut(name) {
            bumps.push(!(prop.is_secret && secret));
            prop.is_secret = secret;
            area_properties += 1;
        }
    }
    for bump_public in bumps {
        st.bump(Some(area_id), bump_public, false);
    }

    ok(json!({
        "rooms": rooms,
        "exits": exits,
        "labels": labels,
        "shapes": shapes,
        "room_properties": room_properties,
        "area_properties": area_properties,
    }))
}

// ---------------------------------------------------------------------------
// GET /areas/{id}/secrets
// ---------------------------------------------------------------------------

/// GET /areas/{id}/secrets — OWNER-only flat audit list.
pub async fn list_secrets(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(area) = st.areas.get(&area_id) else {
        return not_found();
    };
    if area.user_id != viewer {
        return not_found();
    }

    let mut entries: Vec<Value> = Vec::new();
    for room in area.rooms.values().filter(|r| r.is_secret) {
        entries.push(json!({"kind": "room", "room_number": room.room_number}));
    }
    for exit in area.exits.iter().filter(|e| e.is_secret) {
        entries.push(json!({"kind": "exit", "id": exit.id}));
    }
    for label in area.labels.iter().filter(|l| l.is_secret) {
        entries.push(json!({"kind": "label", "id": label.id}));
    }
    for shape in area.shapes.iter().filter(|s| s.is_secret) {
        entries.push(json!({"kind": "shape", "id": shape.id}));
    }
    for room in area.rooms.values() {
        for (name, prop) in &room.properties {
            if prop.is_secret {
                entries.push(json!({
                    "kind": "room_property",
                    "room_number": room.room_number,
                    "name": name,
                }));
            }
        }
    }
    for (name, prop) in &area.properties {
        if prop.is_secret {
            entries.push(json!({"kind": "area_property", "name": name}));
        }
    }
    ok(json!(entries))
}

// ---------------------------------------------------------------------------
// GET /areas/{id}/preview[?share_id|as_user]
// ---------------------------------------------------------------------------

/// GET /areas/{id}/preview — OWNER-only; share_id wins over as_user; a bogus
/// share_id degrades to the anonymous worst case; audience-sees-nothing is a
/// 200 with data:null.
pub async fn preview_area(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some(area) = st.areas.get(&area_id) else {
        return not_found();
    };
    if area.user_id != viewer {
        return not_found();
    }

    let share_id = match params.get("share_id") {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => return bad_request("Invalid share_id"),
        },
        None => None,
    };
    let as_user = match params.get("as_user") {
        Some(raw) => match Uuid::parse_str(raw) {
            Ok(id) => Some(id),
            Err(_) => return bad_request("Invalid as_user"),
        },
        None => None,
    };

    // share_id wins; a grant that does not REACH this area degrades to the
    // anonymous worst case (a random uuid with no grants).
    let simulated: Uuid = if let Some(sid) = share_id {
        st.grants
            .iter()
            .find(|g| {
                g.id == sid
                    && (g.area_id == Some(area_id)
                        || (g.atlas_id.is_some() && g.atlas_id == area.atlas_id))
            })
            .map_or_else(Uuid::new_v4, |g| g.grantee_id)
    } else if let Some(uid) = as_user {
        uid
    } else {
        Uuid::new_v4()
    };

    match project_area(&st, simulated, area_id) {
        Some(projection) => ok(projection),
        None => ok(Value::Null),
    }
}

// ---------------------------------------------------------------------------
// Clone materializer (shared by area copy and atlas copy)
// ---------------------------------------------------------------------------

/// Materialize the CALLER's redacted projection of every `(src, new)` pair as
/// owned rows in the clones; remap exits pairwise; dangle hidden targets.
/// Mirrors `materialize_clone`: per-source `see_secrets`, `is_secret`
/// preserved, FK placeholders synthesized, rev triggers fired per row.
fn materialize_clone(st: &mut MockState, viewer: Uuid, area_map: &[(Uuid, Uuid)]) {
    let remap: HashMap<Uuid, Uuid> = area_map.iter().copied().collect();

    // Per-source clearance + per-target visibility, resolved BEFORE mutation.
    let mut see_secrets_by_src: HashMap<Uuid, bool> = HashMap::new();
    for (src, _) in area_map {
        let ss = st.caps(viewer, *src).is_some_and(|c| c.see_secrets());
        see_secrets_by_src.insert(*src, ss);
    }
    let mut target_visible: HashMap<Uuid, bool> = HashMap::new();
    for (src, _) in area_map {
        let Some(area) = st.areas.get(src) else { continue };
        for exit in &area.exits {
            if let Some(target) = exit.to_area_id {
                target_visible.entry(target).or_insert_with(|| {
                    target == *src || st.caps(viewer, target).is_some_and(|c| c.can_view)
                });
            }
        }
    }

    // PASS 1 — rooms + child content for every clone (exits land in pass 2).
    for (src, new_area) in area_map {
        let see = see_secrets_by_src[src];
        let Some(source) = st.areas.get(src).cloned() else {
            continue;
        };
        let mut bump_count_public = 0u32;
        let mut bump_count_secret = 0u32;
        {
            let clone = st.areas.get_mut(new_area).expect("clone header exists");
            for room in source.rooms.values().filter(|r| see || !r.is_secret) {
                let mut copied = room.clone();
                copied
                    .properties
                    .retain(|_, p| see || !p.is_secret);
                for prop in copied.properties.values() {
                    if prop.is_secret {
                        bump_count_secret += 1;
                    } else {
                        bump_count_public += 1;
                    }
                }
                // Tags are non-secret: each copied tag is a public insert.
                bump_count_public += u32::try_from(copied.tags.len()).unwrap_or(u32::MAX);
                if copied.is_secret {
                    bump_count_secret += 1;
                } else {
                    bump_count_public += 1;
                }
                clone.rooms.insert(copied.room_number, copied);
            }
            for label in source.labels.iter().filter(|l| see || !l.is_secret) {
                let mut copied = label.clone();
                copied.id = Uuid::new_v4();
                if copied.is_secret {
                    bump_count_secret += 1;
                } else {
                    bump_count_public += 1;
                }
                clone.labels.push(copied);
            }
            for shape in source.shapes.iter().filter(|s| see || !s.is_secret) {
                let mut copied = shape.clone();
                copied.id = Uuid::new_v4();
                if copied.is_secret {
                    bump_count_secret += 1;
                } else {
                    bump_count_public += 1;
                }
                clone.shapes.push(copied);
            }
            for (name, prop) in source.properties.iter().filter(|(_, p)| see || !p.is_secret) {
                if prop.is_secret {
                    bump_count_secret += 1;
                } else {
                    bump_count_public += 1;
                }
                clone.properties.insert(name.clone(), prop.clone());
            }
        }
        for _ in 0..bump_count_public {
            st.bump(Some(*new_area), true, false);
        }
        for _ in 0..bump_count_secret {
            st.bump(Some(*new_area), false, false);
        }
    }

    // PASS 2 — Connections first (fresh UUIDs, §6-closure-filtered), then
    // exits with rewired `connection_id`s, mirroring the server: a group is
    // copied IFF the cloner's projection of the source would include it,
    // and an exit is copied exactly when its Connection was — an uncleared
    // clone can never resurrect a group scrubbed from its source
    // projection.
    for (src, new_area) in area_map {
        let see = see_secrets_by_src[src];
        let Some(source) = st.areas.get(src).cloned() else {
            continue;
        };

        // (f)+(g) surviving Connections, copied under fresh ids. Endpoint B
        // (and the stored route with it) clears when the clone lacks its
        // room — a copied route may never keep a coordinate frame the clone
        // does not contain.
        let mut connection_map: HashMap<Uuid, Uuid> = HashMap::new();
        let mut copied_connections = Vec::new();
        for connection in &source.connections {
            let verdict = super::projection::connection_verdict(st, viewer, &source, connection);
            if verdict.omitted(see) {
                continue;
            }
            let mut copied = connection.clone();
            copied.id = Uuid::new_v4();
            connection_map.insert(connection.id, copied.id);
            let clone_has_b = copied.endpoint_b.as_ref().is_some_and(|b| {
                st.areas
                    .get(new_area)
                    .is_some_and(|clone| clone.rooms.contains_key(&b.room_number))
            });
            if copied.endpoint_b.is_some() && !clone_has_b {
                copied.endpoint_b = None;
                copied.route_points.clear();
            }
            copied_connections.push(copied);
        }

        // (h) Exits — copied iff their Connection was, destination
        // re-resolved (remapped clone / kept visible target / dangled).
        let mut staged = Vec::new();
        for exit in &source.exits {
            let Some(new_connection) = connection_map.get(&exit.connection_id) else {
                continue;
            };
            let (new_to_area, new_to_room, new_to_dir) = match exit.to_area_id {
                None => (None, None, None),
                Some(target) => {
                    if let Some(mapped) = remap.get(&target) {
                        (Some(*mapped), exit.to_room_number, exit.to_direction.clone())
                    } else if target_visible.get(&target).copied().unwrap_or(false) {
                        (Some(target), exit.to_room_number, exit.to_direction.clone())
                    } else {
                        // Hidden: dangle — the real UUID never enters the clone.
                        (None, None, None)
                    }
                }
            };

            let mut copied = exit.clone();
            copied.id = Uuid::new_v4();
            copied.connection_id = *new_connection;
            copied.to_area_id = new_to_area;
            copied.to_room_number = new_to_room;
            copied.to_direction = new_to_dir;
            copied.is_secret = see && exit.is_secret;
            staged.push(copied);
        }

        // Defensive FK placeholders (the closure normally guarantees every
        // surviving from-room/same-area to-room was copied in pass 1).
        let mut placeholder_rooms: Vec<i32> = Vec::new();
        {
            let clone = st.areas.get_mut(new_area).expect("clone exists");
            clone.connections.extend(copied_connections);
            for exit in &staged {
                if let Entry::Vacant(slot) = clone.rooms.entry(exit.from_room_number) {
                    slot.insert(RoomRecord::placeholder(exit.from_room_number));
                    placeholder_rooms.push(exit.from_room_number);
                }
                if exit.to_area_id == Some(*new_area)
                    && let Some(n) = exit.to_room_number
                    && let Entry::Vacant(slot) = clone.rooms.entry(n)
                {
                    slot.insert(RoomRecord::placeholder(n));
                    placeholder_rooms.push(n);
                }
            }
        }
        for _ in placeholder_rooms {
            st.bump(Some(*new_area), true, false);
        }
        // Land the exits, firing the two-sided insert trigger.
        let mut bumps: Vec<(Option<Uuid>, bool)> = Vec::new();
        {
            let clone = st.areas.get_mut(new_area).expect("clone exists");
            for exit in staged {
                bumps.push((Some(*new_area), !exit.is_secret));
                bumps.push((exit.to_area_id, !exit.is_secret));
                clone.exits.push(exit);
            }
        }
        for (target, public) in bumps {
            st.bump(target, public, false);
        }
    }
}

// ---------------------------------------------------------------------------
// POST /areas/{id}/copy
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct CopyAreaRequest {
    name: Option<String>,
    atlas_id: Option<Uuid>,
}

/// POST /areas/{id}/copy — VERIFIED + effective can_copy; materializes the
/// caller's redacted projection with provenance; response rev is the header's
/// initial 1 (matching the real RETURNING-before-triggers behavior).
pub async fn copy_area(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = gate_verified(&st, viewer) {
        return e;
    }
    let req: CopyAreaRequest = if body.trim().is_empty() {
        CopyAreaRequest::default()
    } else {
        match parse_body(&body) {
            Ok(r) => r,
            Err(e) => return e,
        }
    };

    let caps = st.caps(viewer, area_id).unwrap_or(Caps::NONE);
    if !(caps.can_view && caps.can_copy) {
        return not_found();
    }
    if let Some(atlas_id) = req.atlas_id {
        let owned = st
            .atlases
            .get(&atlas_id)
            .is_some_and(|a| a.user_id == viewer);
        if !owned {
            return not_found();
        }
    }

    let (src_rev, src_name) = {
        let src = st.areas.get(&area_id).expect("area exists");
        (src.rev, src.name.clone())
    };
    let name = req.name.unwrap_or_else(|| format!("{src_name} (copy)"));

    let new_area_id = Uuid::new_v4();
    let seq = st.next_seq();
    let mut header = AreaRecord::new(new_area_id, viewer, req.atlas_id, name, seq);
    header.copied_from_area_id = Some(area_id);
    header.copied_from_rev = Some(src_rev);
    header.copied_at = Some(Utc::now());
    let response = json!({
        "id": header.id,
        "user_id": header.user_id,
        "atlas_id": header.atlas_id,
        "name": header.name,
        "created_at": header.created_at,
        "rev": header.rev,
        "copied_from_area_id": header.copied_from_area_id,
        "copied_from_rev": header.copied_from_rev,
        "copied_at": header.copied_at,
    });
    st.areas.insert(new_area_id, header);

    materialize_clone(&mut st, viewer, &[(area_id, new_area_id)]);
    created(response)
}

// ---------------------------------------------------------------------------
// POST /atlases/{id}/copy
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
struct CopyAtlasRequest {
    name: Option<String>,
}

/// POST /atlases/{id}/copy — per-member effective can_copy decides copied vs
/// skipped (skipped = viewable-but-not-copyable; invisible members dropped
/// silently); intra-atlas links remap pairwise.
pub async fn copy_atlas(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Ok(atlas_id) = Uuid::parse_str(&raw_id) else {
        return bad_request(&format!("Invalid atlas ID: {raw_id}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = gate_verified(&st, viewer) {
        return e;
    }
    let req: CopyAtlasRequest = if body.trim().is_empty() {
        CopyAtlasRequest::default()
    } else {
        match parse_body(&body) {
            Ok(r) => r,
            Err(e) => return e,
        }
    };

    let Some(atlas_name) = st.atlases.get(&atlas_id).map(|a| a.name.clone()) else {
        return not_found();
    };

    // Member areas in stable (created, id) order.
    let mut members: Vec<(u64, Uuid)> = st
        .areas
        .values()
        .filter(|a| a.atlas_id == Some(atlas_id))
        .map(|a| (a.created_seq, a.id))
        .collect();
    members.sort_unstable();

    let mut copyable: Vec<Uuid> = Vec::new();
    let mut skipped: Vec<Uuid> = Vec::new();
    for (_, member) in &members {
        let caps = st.caps(viewer, *member).unwrap_or(Caps::NONE);
        if caps.can_view && caps.can_copy {
            copyable.push(*member);
        } else if caps.can_view {
            // Viewable-but-not-copyable is reported; invisible is dropped.
            skipped.push(*member);
        }
    }

    let new_name = req
        .name
        .unwrap_or_else(|| format!("{atlas_name} (copy)"));
    let new_atlas_id = Uuid::new_v4();
    st.atlases.insert(
        new_atlas_id,
        AtlasRecord {
            id: new_atlas_id,
            user_id: viewer,
            name: new_name.clone(),
            created_at: Utc::now(),
        },
    );

    let area_map: Vec<(Uuid, Uuid)> =
        copyable.iter().map(|src| (*src, Uuid::new_v4())).collect();
    let mut copied: Vec<Uuid> = Vec::new();
    for (src, new_area) in &area_map {
        let (src_rev, src_name) = {
            let source = st.areas.get(src).expect("member exists");
            (source.rev, source.name.clone())
        };
        let seq = st.next_seq();
        let mut header = AreaRecord::new(
            *new_area,
            viewer,
            Some(new_atlas_id),
            format!("{src_name} (copy)"),
            seq,
        );
        header.copied_from_area_id = Some(*src);
        header.copied_from_rev = Some(src_rev);
        header.copied_at = Some(Utc::now());
        st.areas.insert(*new_area, header);
        copied.push(*new_area);
    }
    if !area_map.is_empty() {
        materialize_clone(&mut st, viewer, &area_map);
    }

    created(json!({
        "atlas_id": new_atlas_id,
        "name": new_name,
        "copied": copied,
        "skipped": skipped,
    }))
}
