//! P2 — /areas CRUD, room/property/exit/label/shape writes, /sync.
//! Mirrors `handlers.rs` + `db.rs` (MapQueries) semantics: can_edit gating,
//! secret clearance, the exit destination matrix, dual-rev bumps.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use parking_lot::Mutex;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{
    authenticate, bad_request, created, err, gate_verified, not_found, ok, parse_area_id,
    parse_body,
};
use super::projection::{project_area, project_list_item, viewer_covers};
use super::state::{
    AreaPropRecord, AreaRecord, Caps, ExitRecord, LabelRecord, MockState, RoomPropRecord,
    RoomRecord, ShapeRecord, access_fingerprint,
};

pub type Shared = Arc<Mutex<MockState>>;

pub const DIRECTIONS: [&str; 14] = [
    "North",
    "East",
    "South",
    "West",
    "Up",
    "Down",
    "Northeast",
    "Northwest",
    "Southeast",
    "Southwest",
    "In",
    "Out",
    "Special",
    "Other",
];
pub const STYLES: [&str; 5] = ["Normal", "Dashed", "Dotted", "Meandering", "Stub"];
pub const SHAPE_TYPES: [&str; 2] = ["Rectangle", "RoundedRectangle"];
pub const H_ALIGN: [&str; 3] = ["Left", "Center", "Right"];
pub const V_ALIGN: [&str; 3] = ["Top", "Center", "Bottom"];

fn check_enum(value: &str, allowed: &[&str], what: &str) -> Result<(), Response> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(bad_request(&format!("Invalid {what}: {value}")))
    }
}

/// `Option<Option<T>>` body fields: key omitted = `None`, `null` =
/// `Some(None)`, value = `Some(Some(v))`. Pair with `#[serde(default)]`.
fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// Caps for a write/read on an existing area; uniform 404 when absent.
fn require_caps(st: &MockState, viewer: Uuid, area_id: Uuid) -> Result<Caps, Response> {
    st.caps(viewer, area_id).ok_or_else(not_found)
}

fn embedded_exit_json(e: &ExitRecord) -> Value {
    json!({
        "id": e.id,
        "from_direction": e.from_direction,
        "to_area_id": e.to_area_id,
        "to_room_number": e.to_room_number,
        "to_direction": e.to_direction,
        "path": e.path,
        "is_hidden": e.is_hidden,
        "is_closed": e.is_closed,
        "is_locked": e.is_locked,
        "weight": e.weight,
        "command": e.command,
        "style": e.style,
        "color": e.color,
    })
}

fn embedded_label_json(l: &LabelRecord) -> Value {
    json!({
        "id": l.id,
        "level": l.level,
        "x": l.x,
        "y": l.y,
        "width": l.width,
        "height": l.height,
        "horizontal_alignment": l.horizontal_alignment,
        "vertical_alignment": l.vertical_alignment,
        "text": l.text,
        "color": l.color,
        "background_color": l.background_color,
        "font_size": l.font_size,
        "font_weight": l.font_weight,
    })
}

fn embedded_shape_json(s: &ShapeRecord) -> Value {
    json!({
        "id": s.id,
        "level": s.level,
        "x": s.x,
        "y": s.y,
        "width": s.width,
        "height": s.height,
        "background_color": s.background_color,
        "stroke_color": s.stroke_color,
        "shape_type": s.shape_type,
        "border_radius": s.border_radius,
        "stroke_width": s.stroke_width,
    })
}

fn legacy_area_json(area: &AreaRecord) -> Value {
    json!({
        "id": area.id,
        "user_id": area.user_id,
        "atlas_id": area.atlas_id,
        "name": area.name,
        "created_at": area.created_at,
        "rev": area.rev,
    })
}

// ---------------------------------------------------------------------------
// Area CRUD
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateAreaRequest {
    name: String,
    atlas_id: Option<Uuid>,
}

/// POST /areas — no verified gate; atlas (when given) must be caller-owned.
pub async fn create_area(State(state): State<Shared>, headers: HeaderMap, body: String) -> Response {
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: CreateAreaRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Some(atlas_id) = req.atlas_id {
        let owned = st
            .atlases
            .get(&atlas_id)
            .is_some_and(|a| a.user_id == viewer);
        if !owned {
            return not_found();
        }
    }
    let seq = st.next_seq();
    let area = AreaRecord::new(Uuid::new_v4(), viewer, req.atlas_id, req.name, seq);
    let response = legacy_area_json(&area);
    st.areas.insert(area.id, area);
    created(response)
}

/// GET /areas — owned ∪ grant-covered, projected list items, created order.
pub async fn list_areas(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut areas: Vec<&AreaRecord> = st
        .areas
        .values()
        .filter(|a| viewer_covers(&st, viewer, a))
        .collect();
    areas.sort_by_key(|a| a.created_seq);
    let rows: Vec<Value> = areas
        .into_iter()
        .map(|a| project_list_item(&st, viewer, a))
        .collect();
    ok(json!(rows))
}

/// GET /areas/{id} — viewer-scoped projection; uniform 404 when not viewable.
pub async fn get_area(
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
    match project_area(&st, viewer, area_id) {
        Some(projection) => ok(projection),
        None => not_found(),
    }
}

#[derive(Deserialize)]
struct UpdateAreaRequest {
    name: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    atlas_id: Option<Option<Uuid>>,
}

/// PUT /areas/{id} — OWNER-only rename/atlas-move; bumps both revs; the
/// atlas-move drift cleanup deletes Area-scope re-shares parented on the old
/// atlas grant.
pub async fn update_area(
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
    let req: UpdateAreaRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let Some(area) = st.areas.get(&area_id) else {
        return not_found();
    };
    if area.user_id != viewer {
        return not_found();
    }
    let old_atlas = area.atlas_id;
    let old_name = area.name.clone();

    if let Some(Some(target_atlas)) = req.atlas_id {
        let owned = st
            .atlases
            .get(&target_atlas)
            .is_some_and(|a| a.user_id == viewer);
        if !owned {
            return not_found();
        }
    }

    let new_atlas = req.atlas_id.unwrap_or(old_atlas);
    let new_name = req.name.unwrap_or(old_name.clone());
    let changed = new_atlas != old_atlas || new_name != old_name;

    {
        let area = st.areas.get_mut(&area_id).expect("area exists");
        area.atlas_id = new_atlas;
        area.name = new_name;
        if changed {
            // BEFORE-UPDATE self trigger: name/atlas changes bump BOTH revs.
            area.rev += 1;
            area.public_rev += 1;
        }
    }

    // Drift cleanup (AFTER UPDATE OF atlas_id): delete Area-scope grants on
    // this area whose parent is an Atlas grant on the OLD atlas.
    if new_atlas != old_atlas
        && let Some(old_atlas) = old_atlas
    {
        let owner = st.areas.get(&area_id).expect("area exists").user_id;
        let parent_ids: Vec<Uuid> = st
            .grants
            .iter()
            .filter(|g| g.atlas_id == Some(old_atlas) && g.owner_id == owner)
            .map(|g| g.id)
            .collect();
        let doomed: Vec<Uuid> = st
            .grants
            .iter()
            .filter(|g| {
                g.area_id == Some(area_id)
                    && g.parent_grant_id
                        .is_some_and(|p| parent_ids.contains(&p))
            })
            .map(|g| g.id)
            .collect();
        st.delete_grants_cascading(&doomed);
    }

    let area = st.areas.get(&area_id).expect("area exists");
    ok(legacy_area_json(area))
}

/// DELETE /areas/{id} — OWNER-only; cascades grants; inbound cross-area exits
/// get their destination nulled (FK `ON DELETE SET NULL` on the room pair).
pub async fn delete_area(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
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
    let Some(area) = st.areas.get(&area_id) else {
        return not_found();
    };
    if area.user_id != viewer {
        return not_found();
    }

    st.areas.remove(&area_id);
    // FK SET NULL on (to_area_id, to_room_number): null the destination of
    // exits in OTHER areas that pointed at a room in the deleted area.
    let mut touched: Vec<(Uuid, bool)> = Vec::new();
    for other in st.areas.values_mut() {
        for exit in &mut other.exits {
            if exit.to_area_id == Some(area_id) && exit.to_room_number.is_some() {
                exit.to_area_id = None;
                exit.to_room_number = None;
                touched.push((other.id, exit.is_secret));
            }
        }
    }
    for (host, secret) in touched {
        st.bump(Some(host), !secret, false);
    }
    // Grants on the area cascade (subtrees via parent FK).
    let doomed: Vec<Uuid> = st
        .grants
        .iter()
        .filter(|g| g.area_id == Some(area_id))
        .map(|g| g.id)
        .collect();
    st.delete_grants_cascading(&doomed);
    ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Area properties
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PropertyRequest {
    value: String,
    is_secret: Option<bool>,
}

/// PUT /areas/{id}/properties/{name}
pub async fn upsert_area_property(
    State(state): State<Shared>,
    Path((raw_id, name)): Path<(String, String)>,
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
    let req: PropertyRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    if req.is_secret.is_some() && !caps.cleared() {
        return not_found();
    }

    let area = st.areas.get_mut(&area_id).expect("area exists");
    let (old_secret, new_secret) = match area.properties.get_mut(&name) {
        Some(existing) => {
            if existing.is_secret && !caps.cleared() {
                return err(409, "property name unavailable");
            }
            let old = existing.is_secret;
            existing.value.clone_from(&req.value);
            existing.created_at = Utc::now();
            existing.is_secret = req.is_secret.unwrap_or(existing.is_secret);
            (old, existing.is_secret)
        }
        None => {
            let secret = req.is_secret.unwrap_or(false);
            area.properties.insert(
                name.clone(),
                AreaPropRecord {
                    value: req.value.clone(),
                    is_secret: secret,
                    created_at: Utc::now(),
                },
            );
            (secret, secret)
        }
    };
    let prop_created_at = area.properties[&name].created_at;
    st.bump(Some(area_id), !(old_secret && new_secret), false);
    ok(json!({
        "area_id": area_id,
        "name": name,
        "value": req.value,
        "created_at": prop_created_at,
    }))
}

/// DELETE /areas/{id}/properties/{name} — no clearance gate on deletes.
pub async fn delete_area_property(
    State(state): State<Shared>,
    Path((raw_id, name)): Path<(String, String)>,
    headers: HeaderMap,
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
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let area = st.areas.get_mut(&area_id).expect("area exists");
    match area.properties.remove(&name) {
        Some(prop) => {
            st.bump(Some(area_id), !prop.is_secret, false);
            ok(Value::Null)
        }
        None => err(404, "Property not found"),
    }
}

// ---------------------------------------------------------------------------
// Rooms (NOTE: upsert is the BARE PUT /areas/{id}/{room_number} path)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct UpsertRoomRequest {
    title: Option<String>,
    description: Option<String>,
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    color: Option<String>,
    is_secret: Option<bool>,
}

/// PUT /areas/{id}/{room_number}
pub async fn upsert_room(
    State(state): State<Shared>,
    Path((raw_id, raw_room)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(room_number) = raw_room.parse::<i32>() else {
        return bad_request(&format!("Invalid room number: {raw_room}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: UpsertRoomRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    if req.is_secret.is_some() && !caps.cleared() {
        return not_found();
    }

    let area = st.areas.get_mut(&area_id).expect("area exists");
    let (old_secret, new_secret) = match area.rooms.get_mut(&room_number) {
        Some(room) => {
            if room.is_secret && !caps.cleared() {
                return err(409, "room number unavailable");
            }
            let old = room.is_secret;
            if let Some(title) = req.title {
                room.title = title;
            }
            if let Some(description) = req.description {
                room.description = description;
            }
            if let Some(level) = req.level {
                room.level = level;
            }
            if let Some(x) = req.x {
                room.x = x;
            }
            if let Some(y) = req.y {
                room.y = y;
            }
            if let Some(color) = req.color {
                room.color = color;
            }
            room.is_secret = req.is_secret.unwrap_or(room.is_secret);
            (old, room.is_secret)
        }
        None => {
            let secret = req.is_secret.unwrap_or(false);
            let room = RoomRecord {
                room_number,
                title: req.title.unwrap_or_default(),
                description: req.description.unwrap_or_default(),
                level: req.level.unwrap_or(0),
                x: req.x.unwrap_or(0.0),
                y: req.y.unwrap_or(0.0),
                color: req.color.unwrap_or_default(),
                is_secret: secret,
                created_at: Utc::now(),
                properties: std::collections::BTreeMap::new(),
                tags: std::collections::BTreeSet::new(),
            };
            area.rooms.insert(room_number, room);
            (secret, secret)
        }
    };
    st.bump(Some(area_id), !(old_secret && new_secret), false);

    // Mutation responses are NOT projected: full properties + exits.
    let area = st.areas.get(&area_id).expect("area exists");
    let room = area.rooms.get(&room_number).expect("room exists");
    let props: Vec<Value> = room
        .properties
        .iter()
        .map(|(name, p)| json!({"name": name, "value": p.value}))
        .collect();
    let exits: Vec<Value> = area
        .exits
        .iter()
        .filter(|e| e.from_room_number == room_number)
        .map(embedded_exit_json)
        .collect();
    ok(json!({
        "area_id": area_id,
        "room_number": room.room_number,
        "title": room.title,
        "description": room.description,
        "color": room.color,
        "level": room.level,
        "x": room.x,
        "y": room.y,
        "created_at": room.created_at,
        "properties": props,
        "exits": exits,
    }))
}

/// DELETE /areas/{id}/rooms/{room_number} — nulls inbound exits; a secret
/// room's deletion suppresses ALL public_rev bumps in the transaction.
pub async fn delete_room(
    State(state): State<Shared>,
    Path((raw_id, raw_room)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(room_number) = raw_room.parse::<i32>() else {
        return bad_request(&format!("Invalid room number: {raw_room}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }

    let Some(room_secret) = st
        .areas
        .get(&area_id)
        .and_then(|a| a.rooms.get(&room_number))
        .map(|r| r.is_secret)
    else {
        return err(404, "Room not found");
    };
    let suppress = room_secret;

    // 1. Null inbound exits' destinations (any area), bumping per trigger.
    let mut inbound: Vec<(Uuid, bool)> = Vec::new();
    for other in st.areas.values_mut() {
        for exit in &mut other.exits {
            if exit.to_area_id == Some(area_id) && exit.to_room_number == Some(room_number) {
                inbound.push((other.id, exit.is_secret));
                exit.to_area_id = None;
                exit.to_room_number = None;
                exit.to_direction = None;
            }
        }
    }
    for (host, secret) in inbound {
        // From-side bump (unchanged secrecy) + old-to-side bump on this area.
        st.bump(Some(host), !secret, suppress);
        st.bump(Some(area_id), !secret, suppress);
    }

    // 2. Cascade deletes: room properties + outbound exits, then the room.
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let room = area.rooms.remove(&room_number).expect("room exists");
    let mut bumps: Vec<(Option<Uuid>, bool)> = Vec::new();
    for prop in room.properties.values() {
        bumps.push((Some(area_id), !prop.is_secret));
    }
    let mut kept = Vec::with_capacity(area.exits.len());
    for exit in std::mem::take(&mut area.exits) {
        if exit.from_room_number == room_number {
            bumps.push((Some(area_id), !exit.is_secret));
            bumps.push((exit.to_area_id, !exit.is_secret));
        } else {
            kept.push(exit);
        }
    }
    area.exits = kept;
    bumps.push((Some(area_id), !room.is_secret));
    for (target, public) in bumps {
        st.bump(target, public, suppress);
    }
    ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Room properties
// ---------------------------------------------------------------------------

/// PUT /areas/{id}/rooms/{room_number}/properties/{name}
pub async fn upsert_room_property(
    State(state): State<Shared>,
    Path((raw_id, raw_room, name)): Path<(String, String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(room_number) = raw_room.parse::<i32>() else {
        return bad_request(&format!("Invalid room number: {raw_room}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: PropertyRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    if req.is_secret.is_some() && !caps.cleared() {
        return not_found();
    }

    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return err(404, "Room not found");
    };
    let (old_secret, new_secret) = match room.properties.get_mut(&name) {
        Some(existing) => {
            if existing.is_secret && !caps.cleared() {
                return err(409, "property name unavailable");
            }
            let old = existing.is_secret;
            existing.value.clone_from(&req.value);
            existing.is_secret = req.is_secret.unwrap_or(existing.is_secret);
            (old, existing.is_secret)
        }
        None => {
            let secret = req.is_secret.unwrap_or(false);
            room.properties.insert(
                name.clone(),
                RoomPropRecord {
                    value: req.value.clone(),
                    is_secret: secret,
                },
            );
            (secret, secret)
        }
    };
    st.bump(Some(area_id), !(old_secret && new_secret), false);
    ok(json!({"name": name, "value": req.value}))
}

/// DELETE /areas/{id}/rooms/{room_number}/properties/{name}
pub async fn delete_room_property(
    State(state): State<Shared>,
    Path((raw_id, raw_room, name)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(room_number) = raw_room.parse::<i32>() else {
        return bad_request(&format!("Invalid room number: {raw_room}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let removed = area
        .rooms
        .get_mut(&room_number)
        .and_then(|room| room.properties.remove(&name));
    match removed {
        Some(prop) => {
            st.bump(Some(area_id), !prop.is_secret, false);
            ok(Value::Null)
        }
        None => err(404, "Room property not found"),
    }
}

// ---------------------------------------------------------------------------
// Room tags — non-secret, normalized to UPPERCASE
// ---------------------------------------------------------------------------

/// PUT /areas/{id}/rooms/{room_number}/tags/{tag}
pub async fn add_room_tag(
    State(state): State<Shared>,
    Path((raw_id, raw_room, tag)): Path<(String, String, String)>,
    headers: HeaderMap,
    _body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(room_number) = raw_room.parse::<i32>() else {
        return bad_request(&format!("Invalid room number: {raw_room}"));
    };
    let normalized = tag.trim().to_uppercase();
    if normalized.is_empty() {
        return bad_request("Tag must not be empty");
    }
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return err(404, "Room not found");
    };
    // Tags are non-secret: any real change bumps the public rev. An idempotent
    // re-add changes nothing.
    let inserted = room.tags.insert(normalized);
    if inserted {
        st.bump(Some(area_id), true, false);
    }
    ok(Value::Null)
}

/// DELETE /areas/{id}/rooms/{room_number}/tags/{tag}
pub async fn remove_room_tag(
    State(state): State<Shared>,
    Path((raw_id, raw_room, tag)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(room_number) = raw_room.parse::<i32>() else {
        return bad_request(&format!("Invalid room number: {raw_room}"));
    };
    let normalized = tag.trim().to_uppercase();
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let removed = area
        .rooms
        .get_mut(&room_number)
        .is_some_and(|room| room.tags.remove(&normalized));
    if removed {
        st.bump(Some(area_id), true, false);
        ok(Value::Null)
    } else {
        err(404, "Room tag not found")
    }
}

// ---------------------------------------------------------------------------
// Exits — destination matrix
// ---------------------------------------------------------------------------

/// The destination matrix: viewable target, link-or-placeholder, the bounded
/// secret-room 409 oracle. Creates the target placeholder when permitted.
fn resolve_exit_destination(
    st: &mut MockState,
    host_area: Uuid,
    host_cleared: bool,
    viewer: Uuid,
    to_area_id: Uuid,
    to_room_number: Option<i32>,
) -> Result<(), Response> {
    let caps = st.caps(viewer, to_area_id).unwrap_or(Caps::NONE);
    let same_area = to_area_id == host_area;
    let target_cleared = if same_area {
        host_cleared
    } else {
        caps.see_secrets()
    };

    if !same_area && !caps.can_view {
        return Err(not_found());
    }
    let Some(to_room) = to_room_number else {
        return Ok(());
    };

    let existing_secret = st
        .areas
        .get(&to_area_id)
        .and_then(|a| a.rooms.get(&to_room))
        .map(|r| r.is_secret);
    match existing_secret {
        Some(secret) => {
            if secret && !target_cleared {
                // 409 only for callers who can edit the target; view-only
                // callers get the uniform 404 (no secret-room oracle).
                if same_area || caps.can_edit {
                    return Err(err(409, "room number unavailable"));
                }
                return Err(not_found());
            }
            Ok(())
        }
        None => {
            if !same_area && !caps.can_edit {
                return Err(not_found());
            }
            let inserted = match st.areas.get_mut(&to_area_id) {
                Some(area) => {
                    area.rooms
                        .insert(to_room, RoomRecord::placeholder(to_room));
                    true
                }
                None => false,
            };
            if inserted {
                st.bump(Some(to_area_id), true, false);
            }
            Ok(())
        }
    }
}

#[derive(Deserialize)]
struct CreateExitRequest {
    from_direction: String,
    to_area_id: Option<Uuid>,
    to_room_number: Option<i32>,
    to_direction: Option<String>,
    path: Option<String>,
    is_hidden: bool,
    is_closed: bool,
    is_locked: bool,
    weight: f32,
    command: Option<String>,
    style: Option<String>,
    color: Option<String>,
    is_secret: Option<bool>,
}

/// POST /areas/{id}/rooms/{room_number}/exits
pub async fn create_exit(
    State(state): State<Shared>,
    Path((raw_id, raw_room)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(room_number) = raw_room.parse::<i32>() else {
        return bad_request(&format!("Invalid room number: {raw_room}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: CreateExitRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Err(e) = check_enum(&req.from_direction, &DIRECTIONS, "direction") {
        return e;
    }
    if let Some(d) = &req.to_direction
        && let Err(e) = check_enum(d, &DIRECTIONS, "direction")
    {
        return e;
    }
    if let Some(s) = &req.style
        && let Err(e) = check_enum(s, &STYLES, "style")
    {
        return e;
    }
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let host_cleared = caps.cleared();
    if req.is_secret.is_some() && !host_cleared {
        return not_found();
    }

    if let Some(to_area_id) = req.to_area_id
        && let Err(e) = resolve_exit_destination(
            &mut st,
            area_id,
            host_cleared,
            viewer,
            to_area_id,
            req.to_room_number,
        )
    {
        return e;
    }

    // From-room placeholder (always allowed — caller can edit the host).
    let from_room_created = {
        let area = st.areas.get_mut(&area_id).expect("area exists");
        match area.rooms.entry(room_number) {
            std::collections::btree_map::Entry::Vacant(slot) => {
                slot.insert(RoomRecord::placeholder(room_number));
                true
            }
            std::collections::btree_map::Entry::Occupied(_) => false,
        }
    };
    if from_room_created {
        st.bump(Some(area_id), true, false);
    }

    let exit = ExitRecord {
        id: Uuid::new_v4(),
        from_room_number: room_number,
        from_direction: req.from_direction,
        to_area_id: req.to_area_id,
        to_room_number: req.to_room_number,
        to_direction: req.to_direction,
        path: req.path.unwrap_or_default(),
        is_hidden: req.is_hidden,
        is_closed: req.is_closed,
        is_locked: req.is_locked,
        weight: req.weight,
        command: req.command.unwrap_or_default(),
        style: req.style.unwrap_or_else(|| "Normal".to_string()),
        color: req.color.unwrap_or_default(),
        is_secret: req.is_secret.unwrap_or(false),
    };
    let response = embedded_exit_json(&exit);
    let exit_secret = exit.is_secret;
    let to_area = exit.to_area_id;
    st.areas
        .get_mut(&area_id)
        .expect("area exists")
        .exits
        .push(exit);
    // INSERT trigger: bump from-area AND to-area (each with its predicate).
    st.bump(Some(area_id), !exit_secret, false);
    st.bump(to_area, !exit_secret, false);
    created(response)
}

#[derive(Deserialize)]
struct UpdateExitRequest {
    from_direction: Option<String>,
    to_area_id: Option<Uuid>,
    to_room_number: Option<i32>,
    to_direction: Option<String>,
    path: Option<String>,
    is_hidden: Option<bool>,
    is_closed: Option<bool>,
    is_locked: Option<bool>,
    weight: Option<f32>,
    command: Option<String>,
    style: Option<String>,
    color: Option<String>,
    is_secret: Option<bool>,
    clear_to: Option<bool>,
}

/// PUT /areas/{id}/exits/{exit_id} — COALESCE semantics; `clear_to` nulls and
/// wins; destination changes re-resolve through the matrix; touching an
/// already-secret exit requires clearance.
pub async fn update_exit(
    State(state): State<Shared>,
    Path((raw_id, raw_exit)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(exit_id) = Uuid::parse_str(&raw_exit) else {
        return bad_request(&format!("Invalid exit ID: {raw_exit}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: UpdateExitRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Some(d) = &req.from_direction
        && let Err(e) = check_enum(d, &DIRECTIONS, "direction")
    {
        return e;
    }
    if let Some(d) = &req.to_direction
        && let Err(e) = check_enum(d, &DIRECTIONS, "direction")
    {
        return e;
    }
    if let Some(s) = &req.style
        && let Err(e) = check_enum(s, &STYLES, "style")
    {
        return e;
    }
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let host_cleared = caps.cleared();

    let Some(current) = st
        .areas
        .get(&area_id)
        .and_then(|a| a.exits.iter().find(|e| e.id == exit_id))
        .cloned()
    else {
        return not_found();
    };
    if req.is_secret.is_some() && !host_cleared {
        return not_found();
    }
    if current.is_secret && !host_cleared {
        return not_found();
    }

    let clear_to = req.clear_to.unwrap_or(false);
    let (new_to_area, new_to_room) = if clear_to {
        (None, None)
    } else {
        (
            req.to_area_id.or(current.to_area_id),
            req.to_room_number.or(current.to_room_number),
        )
    };
    let destination_changed =
        clear_to || req.to_area_id.is_some() || req.to_room_number.is_some();

    if destination_changed
        && let Some(to_area_id) = new_to_area
        && let Err(e) = resolve_exit_destination(
            &mut st,
            area_id,
            host_cleared,
            viewer,
            to_area_id,
            new_to_room,
        )
    {
        return e;
    }

    let old_to_area = current.to_area_id;
    let old_secret = current.is_secret;
    let updated = {
        let area = st.areas.get_mut(&area_id).expect("area exists");
        let exit = area
            .exits
            .iter_mut()
            .find(|e| e.id == exit_id)
            .expect("exit exists");
        if let Some(d) = req.from_direction {
            exit.from_direction = d;
        }
        if clear_to {
            exit.to_area_id = None;
            exit.to_room_number = None;
            exit.to_direction = None;
        } else {
            if let Some(a) = req.to_area_id {
                exit.to_area_id = Some(a);
            }
            if let Some(r) = req.to_room_number {
                exit.to_room_number = Some(r);
            }
            if let Some(d) = req.to_direction {
                exit.to_direction = Some(d);
            }
        }
        if let Some(p) = req.path {
            exit.path = p;
        }
        if let Some(v) = req.is_hidden {
            exit.is_hidden = v;
        }
        if let Some(v) = req.is_closed {
            exit.is_closed = v;
        }
        if let Some(v) = req.is_locked {
            exit.is_locked = v;
        }
        if let Some(v) = req.weight {
            exit.weight = v;
        }
        if let Some(c) = req.command {
            exit.command = c;
        }
        if let Some(s) = req.style {
            exit.style = s;
        }
        if let Some(c) = req.color {
            exit.color = c;
        }
        if let Some(s) = req.is_secret {
            exit.is_secret = s;
        }
        exit.clone()
    };

    // UPDATE trigger: from-side bump; to-side per old/new target.
    let new_secret = updated.is_secret;
    st.bump(Some(area_id), !(old_secret && new_secret), false);
    if old_to_area == updated.to_area_id {
        st.bump(updated.to_area_id, !(old_secret && new_secret), false);
    } else {
        st.bump(old_to_area, !old_secret, false);
        st.bump(updated.to_area_id, !new_secret, false);
    }
    ok(embedded_exit_json(&updated))
}

/// DELETE /areas/{id}/exits/{exit_id}
pub async fn delete_exit(
    State(state): State<Shared>,
    Path((raw_id, raw_exit)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(exit_id) = Uuid::parse_str(&raw_exit) else {
        return bad_request(&format!("Invalid exit ID: {raw_exit}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(idx) = area.exits.iter().position(|e| e.id == exit_id) else {
        return err(404, "Exit not found");
    };
    let exit = area.exits.remove(idx);
    st.bump(Some(area_id), !exit.is_secret, false);
    st.bump(exit.to_area_id, !exit.is_secret, false);
    ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Labels
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateLabelRequest {
    level: Option<i32>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    horizontal_alignment: String,
    vertical_alignment: String,
    text: String,
    color: Option<String>,
    background_color: Option<String>,
    font_size: Option<i32>,
    font_weight: Option<i32>,
    is_secret: Option<bool>,
}

/// POST /areas/{id}/labels
pub async fn create_label(
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
    let req: CreateLabelRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Err(e) = check_enum(&req.horizontal_alignment, &H_ALIGN, "horizontal alignment") {
        return e;
    }
    if let Err(e) = check_enum(&req.vertical_alignment, &V_ALIGN, "vertical alignment") {
        return e;
    }
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    if req.is_secret.is_some() && !caps.cleared() {
        return not_found();
    }

    let label = LabelRecord {
        id: Uuid::new_v4(),
        level: req.level.unwrap_or(0),
        x: req.x,
        y: req.y,
        width: req.width,
        height: req.height,
        horizontal_alignment: req.horizontal_alignment,
        vertical_alignment: req.vertical_alignment,
        text: req.text,
        color: req.color.unwrap_or_else(|| "black".to_string()),
        background_color: req.background_color.unwrap_or_else(|| "white".to_string()),
        font_size: req.font_size.unwrap_or(12),
        font_weight: req.font_weight.unwrap_or(400),
        is_secret: req.is_secret.unwrap_or(false),
    };
    let response = embedded_label_json(&label);
    let secret = label.is_secret;
    st.areas
        .get_mut(&area_id)
        .expect("area exists")
        .labels
        .push(label);
    st.bump(Some(area_id), !secret, false);
    created(response)
}

#[derive(Deserialize)]
struct UpdateLabelRequest {
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
    horizontal_alignment: Option<String>,
    vertical_alignment: Option<String>,
    text: Option<String>,
    color: Option<String>,
    background_color: Option<String>,
    font_size: Option<i32>,
    font_weight: Option<i32>,
    is_secret: Option<bool>,
}

/// PUT /areas/{id}/labels/{label_id}
pub async fn update_label(
    State(state): State<Shared>,
    Path((raw_id, raw_label)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(label_id) = Uuid::parse_str(&raw_label) else {
        return bad_request(&format!("Invalid label ID: {raw_label}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: UpdateLabelRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Some(a) = &req.horizontal_alignment
        && let Err(e) = check_enum(a, &H_ALIGN, "horizontal alignment")
    {
        return e;
    }
    if let Some(a) = &req.vertical_alignment
        && let Err(e) = check_enum(a, &V_ALIGN, "vertical alignment")
    {
        return e;
    }
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    if req.is_secret.is_some() && !caps.cleared() {
        return not_found();
    }

    let cleared = caps.cleared();
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(label) = area.labels.iter_mut().find(|l| l.id == label_id) else {
        return err(404, "Label not found");
    };
    // Touching an already-secret label requires clearance (atomic WHERE).
    if label.is_secret && !cleared {
        return not_found();
    }
    let old_secret = label.is_secret;
    if let Some(v) = req.level {
        label.level = v;
    }
    if let Some(v) = req.x {
        label.x = v;
    }
    if let Some(v) = req.y {
        label.y = v;
    }
    if let Some(v) = req.width {
        label.width = v;
    }
    if let Some(v) = req.height {
        label.height = v;
    }
    if let Some(v) = req.horizontal_alignment {
        label.horizontal_alignment = v;
    }
    if let Some(v) = req.vertical_alignment {
        label.vertical_alignment = v;
    }
    if let Some(v) = req.text {
        label.text = v;
    }
    if let Some(v) = req.color {
        label.color = v;
    }
    if let Some(v) = req.background_color {
        label.background_color = v;
    }
    if let Some(v) = req.font_size {
        label.font_size = v;
    }
    if let Some(v) = req.font_weight {
        label.font_weight = v;
    }
    if let Some(v) = req.is_secret {
        label.is_secret = v;
    }
    let response = embedded_label_json(label);
    let new_secret = label.is_secret;
    st.bump(Some(area_id), !(old_secret && new_secret), false);
    ok(response)
}

/// DELETE /areas/{id}/labels/{label_id}
pub async fn delete_label(
    State(state): State<Shared>,
    Path((raw_id, raw_label)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(label_id) = Uuid::parse_str(&raw_label) else {
        return bad_request(&format!("Invalid label ID: {raw_label}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(idx) = area.labels.iter().position(|l| l.id == label_id) else {
        return err(404, "Label not found");
    };
    let label = area.labels.remove(idx);
    st.bump(Some(area_id), !label.is_secret, false);
    ok(Value::Null)
}

// ---------------------------------------------------------------------------
// Shapes (NOTE: update takes `radius`; create takes `border_radius`)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateShapeRequest {
    level: Option<i32>,
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: String,
    border_radius: Option<f32>,
    stroke_width: Option<f32>,
    is_secret: Option<bool>,
}

/// POST /areas/{id}/shapes
pub async fn create_shape(
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
    let req: CreateShapeRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Err(e) = check_enum(&req.shape_type, &SHAPE_TYPES, "shape type") {
        return e;
    }
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    if req.is_secret.is_some() && !caps.cleared() {
        return not_found();
    }

    let shape = ShapeRecord {
        id: Uuid::new_v4(),
        level: req.level.unwrap_or(0),
        x: req.x,
        y: req.y,
        width: req.width,
        height: req.height,
        background_color: Some(req.background_color.unwrap_or_else(|| "grey".to_string())),
        stroke_color: Some(
            req.stroke_color
                .unwrap_or_else(|| "transparent".to_string()),
        ),
        shape_type: req.shape_type,
        border_radius: req.border_radius.unwrap_or(0.0),
        stroke_width: req.stroke_width.unwrap_or(1.0),
        is_secret: req.is_secret.unwrap_or(false),
    };
    let response = embedded_shape_json(&shape);
    let secret = shape.is_secret;
    st.areas
        .get_mut(&area_id)
        .expect("area exists")
        .shapes
        .push(shape);
    st.bump(Some(area_id), !secret, false);
    created(response)
}

#[derive(Deserialize)]
struct UpdateShapeRequest {
    level: Option<i32>,
    x: Option<f32>,
    y: Option<f32>,
    width: Option<f32>,
    height: Option<f32>,
    background_color: Option<String>,
    stroke_color: Option<String>,
    shape_type: Option<String>,
    /// Asymmetric with create: the UPDATE field is named `radius`.
    radius: Option<f32>,
    stroke_width: Option<f32>,
    is_secret: Option<bool>,
}

/// PUT /areas/{id}/shapes/{shape_id}
pub async fn update_shape(
    State(state): State<Shared>,
    Path((raw_id, raw_shape)): Path<(String, String)>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(shape_id) = Uuid::parse_str(&raw_shape) else {
        return bad_request(&format!("Invalid shape ID: {raw_shape}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: UpdateShapeRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Some(t) = &req.shape_type
        && let Err(e) = check_enum(t, &SHAPE_TYPES, "shape type")
    {
        return e;
    }
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    if req.is_secret.is_some() && !caps.cleared() {
        return not_found();
    }

    let cleared = caps.cleared();
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(shape) = area.shapes.iter_mut().find(|s| s.id == shape_id) else {
        return err(404, "Shape not found");
    };
    if shape.is_secret && !cleared {
        return not_found();
    }
    let old_secret = shape.is_secret;
    if let Some(v) = req.level {
        shape.level = v;
    }
    if let Some(v) = req.x {
        shape.x = v;
    }
    if let Some(v) = req.y {
        shape.y = v;
    }
    if let Some(v) = req.width {
        shape.width = v;
    }
    if let Some(v) = req.height {
        shape.height = v;
    }
    if let Some(v) = req.background_color {
        shape.background_color = Some(v);
    }
    if let Some(v) = req.stroke_color {
        shape.stroke_color = Some(v);
    }
    if let Some(v) = req.shape_type {
        shape.shape_type = v;
    }
    if let Some(v) = req.radius {
        shape.border_radius = v;
    }
    if let Some(v) = req.stroke_width {
        shape.stroke_width = v;
    }
    if let Some(v) = req.is_secret {
        shape.is_secret = v;
    }
    let response = embedded_shape_json(shape);
    let new_secret = shape.is_secret;
    st.bump(Some(area_id), !(old_secret && new_secret), false);
    ok(response)
}

/// DELETE /areas/{id}/shapes/{shape_id}
pub async fn delete_shape(
    State(state): State<Shared>,
    Path((raw_id, raw_shape)): Path<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Ok(shape_id) = Uuid::parse_str(&raw_shape) else {
        return bad_request(&format!("Invalid shape ID: {raw_shape}"));
    };
    let mut st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let caps = match require_caps(&st, viewer, area_id) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if !caps.can_edit {
        return not_found();
    }
    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(idx) = area.shapes.iter().position(|s| s.id == shape_id) else {
        return err(404, "Shape not found");
    };
    let shape = area.shapes.remove(idx);
    st.bump(Some(area_id), !shape.is_secret, false);
    ok(Value::Null)
}

// ---------------------------------------------------------------------------
// GET /sync
// ---------------------------------------------------------------------------

/// GET /sync — VERIFIED only; `[{area_id, rev (projected), fingerprint}]`,
/// ordered by area id.
pub async fn sync(State(state): State<Shared>, headers: HeaderMap) -> Response {
    let st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if let Err(e) = gate_verified(&st, viewer) {
        return e;
    }
    // BTreeMap iteration order == `ORDER BY a.id` (bytewise uuid order).
    let rows: Vec<Value> = st
        .areas
        .values()
        .filter(|a| viewer_covers(&st, viewer, a))
        .map(|a| {
            let caps = st.caps(viewer, a.id).expect("area exists");
            json!({
                "area_id": a.id,
                "rev": if caps.see_secrets() { a.rev } else { a.public_rev },
                "access_fingerprint": access_fingerprint(&caps),
            })
        })
        .collect();
    ok(json!(rows))
}
