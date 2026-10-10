//! P2 — /areas CRUD, room/property/label/shape writes, /sync.
//! Mirrors `handlers.rs` + `db.rs` (MapQueries) semantics: can_edit gating
//! and rev bumps. Write bodies carrying `is_secret` are refused, as on the
//! mutation envelope.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{
    authenticate, bad_request, created, err, gate_verified, not_found, ok, parse_area_id,
    parse_body,
};
use super::mutations::mentions_is_secret;
use super::projection::{project_area, project_list_item, viewer_covers};
use super::state::{
    AreaPropRecord, AreaRecord, Caps, ExitRecord, LabelRecord, MockState, RoomPropRecord,
    RoomRecord, ShapeRecord, projection_token,
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
pub const SHAPE_TYPES: [&str; 2] = ["Rectangle", "RoundedRectangle"];
pub const H_ALIGN: [&str; 3] = ["Left", "Center", "Right"];
pub const V_ALIGN: [&str; 3] = ["Top", "Center", "Bottom"];

pub fn check_enum(value: &str, allowed: &[&str], what: &str) -> Result<(), Response> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(bad_request(&format!("Invalid {what}: {value}")))
    }
}

/// `Option<Option<T>>` body fields: key omitted = `None`, `null` =
/// `Some(None)`, value = `Some(Some(v))`. Pair with `#[serde(default)]`.
pub fn double_option<'de, T, D>(de: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Deserialize::deserialize(de).map(Some)
}

/// Parses a per-entity write body. Format 3 replaces `is_secret` with
/// sources, so a body carrying the key anywhere is a 400.
fn parse_write_body<T: DeserializeOwned>(body: &str) -> Result<T, Response> {
    if serde_json::from_str::<Value>(body).is_ok_and(|value| mentions_is_secret(&value)) {
        return Err(bad_request(
            "`is_secret` is not part of format 3; write to a source instead",
        ));
    }
    parse_body(body)
}

/// Caps for a write/read on an existing area; uniform 404 when absent.
pub fn require_caps(st: &MockState, viewer: Uuid, area_id: Uuid) -> Result<Caps, Response> {
    st.caps(viewer, area_id).ok_or_else(not_found)
}

/// The `EmbeddedExit` echo shape of the v2 contract: `connection_id`
/// instead of the retired per-exit style/color.
pub fn embedded_exit_json(e: &ExitRecord) -> Value {
    json!({
        "id": e.id,
        "from_direction": e.from_direction,
        "to_area_id": e.to_area_id,
        "to_room_number": e.to_room_number,
        "to_direction": e.to_direction,
        "path": e.path,
        "is_hidden": e.is_hidden,
        "door": super::state::door_json(e.door.as_ref()),
        "weight": e.weight,
        "command": e.command,
        "connection_id": e.connection_id,
    })
    .tap_source(e)
}

/// Adds `to_source` to an exit's JSON when it leads into a room of another
/// map's Secret.
trait TapSource {
    fn tap_source(self, exit: &ExitRecord) -> Value;
}

impl TapSource for Value {
    fn tap_source(mut self, exit: &ExitRecord) -> Value {
        if let Some(secret) = exit.to_secret {
            self["to_source"] = json!(secret);
        }
        self
    }
}

pub fn embedded_label_json(l: &LabelRecord) -> Value {
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

pub fn embedded_shape_json(s: &ShapeRecord) -> Value {
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
    let mut out = json!({
        "id": area.id,
        "user_id": area.user_id,
        "atlas_id": area.atlas_id,
        "name": area.name,
        "created_at": area.created_at,
        "rev": area.rev,
    });
    // A clan's map names its clan in place of a user.
    if let Some(clan) = area.clan_id {
        out["user_id"] = Value::Null;
        out["clan_id"] = json!(clan);
    }
    out
}

// ---------------------------------------------------------------------------
// Area CRUD
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct CreateAreaRequest {
    name: String,
    atlas_id: Option<Uuid>,
    #[serde(default)]
    clan_id: Option<Uuid>,
    #[serde(default)]
    ownership: Option<String>,
}

/// POST /areas — no verified gate; atlas (when given) must be caller-owned.
pub async fn create_area(
    State(state): State<Shared>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let mut st = state.lock();
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: CreateAreaRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    // With `clan_id`, the map goes in the clan (docs/clans.md §6).
    if let Some(clan) = req.clan_id {
        return match super::clan_maps::create_clan_area(
            &mut st,
            viewer,
            clan,
            req.atlas_id,
            req.name,
            req.ownership.as_deref(),
        ) {
            Ok(id) => created(legacy_area_json(&st.areas[&id])),
            Err(response) => response,
        };
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
    let mut st = state.lock();
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    if st.fail_area_reads > 0 {
        st.fail_area_reads -= 1;
        return super::http::err(500, "injected read failure");
    }
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
    access_review: Option<String>,
}

/// PUT /areas/{id} — OWNER-only rename/atlas-move; bumps the rev; the
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
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: UpdateAreaRequest = match parse_body(&body) {
        Ok(r) => r,
        Err(e) => return e,
    };
    if let Some(refusal) = req.name.as_deref().and_then(super::http::name_refusal) {
        return refusal;
    }

    let Some(area) = st.areas.get(&area_id) else {
        return not_found();
    };
    if area.clan_id.is_some() {
        let renames = req.name.as_ref().is_some_and(|name| *name != area.name);
        if let Err(response) =
            super::clan_maps::may_update_clan_area(&st, viewer, area, renames, req.atlas_id)
        {
            return response;
        }
    } else if area.user_id != viewer {
        return not_found();
    }
    let old_atlas = area.atlas_id;
    let old_name = area.name.clone();

    if area.clan_id.is_none()
        && let Some(Some(target_atlas)) = req.atlas_id
    {
        let owned = st
            .atlases
            .get(&target_atlas)
            .is_some_and(|a| a.user_id == viewer);
        if !owned {
            return not_found();
        }
    }

    let new_atlas = req.atlas_id.unwrap_or(old_atlas);
    if new_atlas != old_atlas {
        let review = match super::filing::review(
            &st,
            viewer,
            area_id,
            new_atlas,
            req.access_review.as_deref(),
        ) {
            Ok(value) => value,
            Err(response) => return response,
        };
        if let Err(response) =
            super::reviewed_moves::check_review(&review, req.access_review.as_deref())
        {
            return response;
        }
    }
    let new_name = req.name.unwrap_or(old_name.clone());
    let changed = new_atlas != old_atlas || new_name != old_name;
    if changed && let Some(refusal) = super::clan_maps::refused_while_disposing(&st, area_id) {
        return refusal;
    }

    {
        let area = st.areas.get_mut(&area_id).expect("area exists");
        area.atlas_id = new_atlas;
        area.name = new_name;
        if changed {
            // BEFORE-UPDATE self trigger: name/atlas changes bump the rev.
            area.rev += 1;
        }
    }
    // A clan map leaving a folder leaves the reach of the folder's grants,
    // so its sharers may lose `area.share_external` on it.
    if new_atlas != old_atlas {
        super::shares::sweep_outside_shares(&mut st);
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
                    && g.parent_grant_id.is_some_and(|p| parent_ids.contains(&p))
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
    axum::extract::RawQuery(query): axum::extract::RawQuery,
    headers: HeaderMap,
) -> Response {
    let area_id = match parse_area_id(&raw_id) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let mut st = state.lock();
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    // The query's shape is judged before the map is looked up.
    if let Err(refusal) = super::http::expected_rev(query.as_deref()) {
        return refusal;
    }
    let Some(area) = st.areas.get(&area_id) else {
        return not_found();
    };
    // A clan's map goes with `area.delete`; anyone else's only by its owner.
    let may_delete = if area.clan_id.is_some() {
        super::clan_maps::area_actions(&st, viewer, area)
            .is_some_and(|actions| actions.contains("area.delete"))
    } else {
        area.user_id == viewer
    };
    if !may_delete {
        return not_found();
    }
    if let Some(refusal) = super::clan_maps::refused_while_disposing(&st, area_id) {
        return refusal;
    }
    if st.fail_area_deletes > 0 {
        st.fail_area_deletes -= 1;
        return err(500, "injected delete failure");
    }

    remove_area(&mut st, area_id);
    ok(Value::Null)
}

fn local_move_review(st: &MockState, viewer: Uuid, area: &AreaRecord) -> Option<Value> {
    use sha2::{Digest, Sha256};
    if area.clan_id.is_some() || area.user_id != viewer {
        return None;
    }
    let mut grants: Vec<_> = st
        .grants
        .iter()
        .filter(|grant| grant.covers_area(area))
        .collect();
    grants.sort_by_key(|grant| grant.id);
    let mut secret_grants: Vec<_> = area
        .secrets
        .iter()
        .flat_map(|secret| &secret.grants)
        .collect();
    secret_grants.sort_by_key(|grant| grant.id);
    let token = Sha256::digest(format!(
        "local-move-sharing/v1:{viewer}:{}:{:?}:{grants:?}:{secret_grants:?}",
        area.id, area.atlas_id
    ));
    Some(json!({
        "area_id": area.id,
        "sharing_token": format!("p_{}", &hex::encode(token)[..32]),
        "has_shares": !grants.is_empty() || !secret_grants.is_empty(),
    }))
}

pub async fn review_local_move(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let id = match parse_area_id(&raw_id) {
        Ok(id) => id,
        Err(error) => return error,
    };
    let st = state.lock();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(who) => who,
        Err(error) => return error,
    };
    st.areas
        .get(&id)
        .and_then(|area| local_move_review(&st, viewer, area))
        .map_or_else(not_found, ok)
}

pub async fn finish_local_move(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let id = match parse_area_id(&raw_id) {
        Ok(id) => id,
        Err(error) => return error,
    };
    let guard: smudgy_cloud::relocation::LocalMoveGuard = match parse_body(&body) {
        Ok(guard) => guard,
        Err(error) => return error,
    };
    let valid = |token: &str| {
        token.len() == 34
            && token.starts_with("p_")
            && token[2..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    };
    if !valid(&guard.sharing_token) || !valid(&guard.expected_projection_token) {
        return bad_request("Invalid local-move token");
    }
    let mut st = state.lock();
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(who) => who,
        Err(error) => return error,
    };
    let Some(review) = st
        .areas
        .get(&id)
        .and_then(|area| local_move_review(&st, viewer, area))
    else {
        return not_found();
    };
    let Some(projection) = project_area(&st, viewer, id) else {
        return not_found();
    };
    if review["sharing_token"] != guard.sharing_token
        || (review["has_shares"] == true && !guard.shared_loss_confirmed)
        || projection["projection_token"] != guard.expected_projection_token
    {
        return err(409, "revision_conflict");
    }
    if st.fail_area_deletes > 0 {
        st.fail_area_deletes -= 1;
        return err(500, "injected delete failure");
    }
    remove_area(&mut st, id);
    ok(Value::Null)
}

/// Removes a map with everything kept beside it, as `DELETE /areas/{id}`
/// does: its clan links, the destinations of exits other maps keep into it,
/// and the grants on it with their subtrees.
pub fn remove_area(st: &mut MockState, area_id: Uuid) {
    st.bind_room_references();
    let removed = st.areas.remove(&area_id);
    // Exits into its Secrets' rooms go, everywhere, moving nothing.
    for secret in removed.iter().flat_map(|area| &area.secrets) {
        super::secrets::drop_exits_into_secret(&mut st.areas, area_id, secret.id, None);
    }
    // Deleting a map takes it out of grant scopes and ends its ownership
    // and transfer offers.
    super::clan_maps::forget_area(st, area_id);
    super::transfers::cancel_offers_of(st, area_id);
    // FK SET NULL on (to_area_id, to_room_number): null the destination of
    // exits in OTHER areas that pointed at a room in the deleted area.
    let mut touched: Vec<Uuid> = Vec::new();
    for other in st.areas.values_mut() {
        for exit in &mut other.exits {
            if exit.to_room_identity.is_none()
                && exit.to_area_id == Some(area_id)
                && exit.to_room_number.is_some()
            {
                exit.to_area_id = None;
                exit.to_room_number = None;
                touched.push(other.id);
            }
        }
    }
    for host in touched {
        st.bump(Some(host));
    }
    // Grants on the area cascade (subtrees via parent FK).
    let doomed: Vec<Uuid> = st
        .grants
        .iter()
        .filter(|g| g.area_id == Some(area_id))
        .map(|g| g.id)
        .collect();
    st.delete_grants_cascading(&doomed);
}

// ---------------------------------------------------------------------------
// Area properties
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct PropertyRequest {
    value: String,
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
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: PropertyRequest = match parse_write_body(&body) {
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

    let area = st.areas.get_mut(&area_id).expect("area exists");
    area.properties.insert(
        name.clone(),
        AreaPropRecord {
            value: req.value.clone(),
            created_at: Utc::now(),
        },
    );
    let prop_created_at = area.properties[&name].created_at;
    st.bump(Some(area_id));
    ok(json!({
        "area_id": area_id,
        "name": name,
        "value": req.value,
        "created_at": prop_created_at,
    }))
}

/// DELETE /areas/{id}/properties/{name}
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
    st.bind_room_references();
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
    if area.properties.remove(&name).is_some() {
        st.bump(Some(area_id));
        ok(Value::Null)
    } else {
        err(404, "Property not found")
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
    /// Absent = unchanged, null = clear, string = set (`Option<Option<_>>`
    /// needs the double-option helper — a bare nested option folds null into
    /// absent).
    #[serde(default, deserialize_with = "double_option")]
    external_id: Option<Option<String>>,
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
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: UpsertRoomRequest = match parse_write_body(&body) {
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

    let area = st.areas.get_mut(&area_id).expect("area exists");
    match area.rooms.get_mut(&room_number) {
        Some(room) => {
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
            if let Some(binding) = req.external_id {
                room.external_id = binding;
            }
        }
        None => {
            let room = RoomRecord {
                identity: Uuid::new_v4(),
                anchor: None,
                room_number,
                title: req.title.unwrap_or_default(),
                description: req.description.unwrap_or_default(),
                level: req.level.unwrap_or(0),
                x: req.x.unwrap_or(0.0),
                y: req.y.unwrap_or(0.0),
                color: req.color.unwrap_or_default(),
                created_at: Utc::now(),
                properties: std::collections::BTreeMap::new(),
                tags: std::collections::BTreeSet::new(),
                external_id: req.external_id.flatten(),
            };
            area.rooms.insert(room_number, room);
        }
    }
    st.bump(Some(area_id));

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
        "external_id": room.external_id,
    }))
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
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: PropertyRequest = match parse_write_body(&body) {
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

    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(room) = area.rooms.get_mut(&room_number) else {
        return err(404, "Room not found");
    };
    room.properties.insert(
        name.clone(),
        RoomPropRecord {
            value: req.value.clone(),
        },
    );
    st.bump(Some(area_id));
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
    st.bind_room_references();
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
    if removed.is_some() {
        st.bump(Some(area_id));
        ok(Value::Null)
    } else {
        err(404, "Room property not found")
    }
}

// ---------------------------------------------------------------------------
// Room tags — normalized to UPPERCASE
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
    st.bind_room_references();
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
    // Any real change bumps the rev. An idempotent re-add changes nothing.
    let inserted = room.tags.insert(normalized);
    if inserted {
        st.bump(Some(area_id));
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
    st.bind_room_references();
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
        st.bump(Some(area_id));
        ok(Value::Null)
    } else {
        err(404, "Room tag not found")
    }
}

// ---------------------------------------------------------------------------
// Exits: the legacy per-entity exit routes are gone from the mock. On the
// real server they became thin envelope wrappers over the same compound
// appliers (`handlers.rs` parses a MutationEnvelope and forwards to
// `AreaMutation::{CreateExit, UpdateExit, DeleteExit}`), and every client
// path drives `POST /areas/{id}/mutations` directly — which is where the
// mock's v2 Connection semantics live (`mutations.rs`).
// ---------------------------------------------------------------------------

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
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: CreateLabelRequest = match parse_write_body(&body) {
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
    };
    let response = embedded_label_json(&label);
    st.areas
        .get_mut(&area_id)
        .expect("area exists")
        .labels
        .push(label);
    st.bump(Some(area_id));
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
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: UpdateLabelRequest = match parse_write_body(&body) {
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

    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(label) = area.labels.iter_mut().find(|l| l.id == label_id) else {
        return err(404, "Label not found");
    };
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
    let response = embedded_label_json(label);
    st.bump(Some(area_id));
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
    st.bind_room_references();
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
    area.labels.remove(idx);
    st.bump(Some(area_id));
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
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: CreateShapeRequest = match parse_write_body(&body) {
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
    };
    let response = embedded_shape_json(&shape);
    st.areas
        .get_mut(&area_id)
        .expect("area exists")
        .shapes
        .push(shape);
    st.bump(Some(area_id));
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
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let req: UpdateShapeRequest = match parse_write_body(&body) {
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

    let area = st.areas.get_mut(&area_id).expect("area exists");
    let Some(shape) = area.shapes.iter_mut().find(|s| s.id == shape_id) else {
        return err(404, "Shape not found");
    };
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
    let response = embedded_shape_json(shape);
    st.bump(Some(area_id));
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
    st.bind_room_references();
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
    area.shapes.remove(idx);
    st.bump(Some(area_id));
    ok(Value::Null)
}

// ---------------------------------------------------------------------------
// GET /sync
// ---------------------------------------------------------------------------

/// GET /sync — VERIFIED only; `[{area_id, rev (shared), fingerprint}]`,
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
            let mut revisions = serde_json::Map::new();
            revisions.insert("map".to_string(), json!(a.rev));
            for (secret, _) in st.readable_secrets(viewer, a) {
                revisions.insert(secret.id.to_string(), json!(secret.rev));
            }
            json!({
                "area_id": a.id,
                "projection_token": projection_token(
                    &st,
                    viewer,
                    a,
                    &caps,
                    super::projection::atlas_name(&st, a).as_deref(),
                ),
                "revisions": revisions,
            })
        })
        .collect();
    ok(json!(rows))
}
