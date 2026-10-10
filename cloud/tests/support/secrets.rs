//! Owner Secrets and moves: POST /areas/{id}/secrets, PATCH and DELETE
//! /secrets/{id}, POST /areas/{id}/moves. Fidelity reference: the
//! smudgy-cloudflare service (`src/library/maps/secrets.ts`,
//! `src/library/graph/moves.ts`).
//!
//! A Secret keeps its own rooms and, at stand-in keys, its data and exits on
//! map rooms (see [`SecretRecord`]); its bundle is served only to its
//! readers, and a connection needing a room that is gone is dropped.
//!
//! Reviewed transfers live in `reviewed_moves`; qualified retained anchors
//! and their projection live in `source_refs`.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::http::{authenticate, bad_request, created, not_found, ok, parse_area_id, parse_body};
use super::secret_grants::OWNER_ACTIONS;
use super::state::{
    AreaRecord, ConnectionRecord, EndpointRecord, ExitRecord, LabelRecord, MockState, RoomRecord,
    SecretRecord, ShapeRecord, map_room_of, stand_in,
};

pub type Shared = Arc<Mutex<MockState>>;

/// Longest Secret name the server accepts, in characters.
const NAME_LIMIT: usize = 255;

/// A Secret's row in `GET /areas/{id}/secrets` and the CRUD responses, with
/// the caller's `actions` on it.
pub fn summary(secret: &SecretRecord, actions: &[&str]) -> Value {
    let mut out = json!({
        "source": secret.id,
        "name": secret.name,
        "ownership": ownership(secret),
        "color": secret.color,
        "actions": actions,
    });
    if let Some(record) = &secret.clan {
        out["clan_id"] = json!(record.clan_id);
    }
    out
}

/// A Secret's badge: `owner`, or a Clan Secret's `members` or `clan`.
fn ownership(secret: &SecretRecord) -> &'static str {
    secret
        .clan
        .as_ref()
        .map_or("owner", |record| record.ownership)
}

/// One of a Secret's exits as its reader sees it: rooms by their wire
/// numbers, `to_source` on a destination among the Secret's own rooms, and
/// a destination in a map the reader cannot read redacted to a token.
pub fn secret_exit_json(
    st: &MockState,
    viewer: Uuid,
    area_id: Uuid,
    wire: &str,
    exit: &ExitRecord,
) -> Value {
    let (exit, readable) = st.resolved_exit(viewer, area_id, exit);
    let from = map_room_of(exit.from_room_number).unwrap_or(exit.from_room_number);
    let mut out = json!({
        "id": exit.id,
        "from_room_number": from,
        "from_direction": exit.from_direction,
        "to_area_id": exit.to_area_id,
        "to_room_number": exit.to_room_number,
        "to_direction": exit.to_direction,
        "to_unknown": false,
        "path": exit.path,
        "command": exit.command,
        "weight": exit.weight,
        "connection_id": exit.connection_id,
        "is_hidden": exit.is_hidden,
        "door": super::state::door_json(exit.door.as_ref()),
    });
    if readable && let Some(secret) = exit.to_secret {
        // A room of another map's Secret, in that Secret's numbering.
        out["to_source"] = json!(st.destination_source(exit.to_area_id.unwrap(), secret));
        return out;
    }
    if !readable {
        let target = exit.to_area_id.expect("only a real target is unreadable");
        out["to_area_id"] = Value::Null;
        out["to_room_number"] = Value::Null;
        out["to_direction"] = Value::Null;
        out["to_unknown"] = json!(true);
        out["to_area_token"] = json!(super::state::to_area_token(
            viewer,
            if exit.to_room_number.is_some() {
                exit.id
            } else {
                target
            }
        ));
    } else if exit.to_area_id == Some(area_id)
        && let Some(key) = exit.to_room_number
    {
        match map_room_of(key) {
            Some(map_room) => out["to_room_number"] = json!(map_room),
            None => out["to_source"] = json!(wire),
        }
    }
    out
}

/// The room a Secret's connection endpoint names exists: one of its own
/// rooms, or a map room the map still holds.
fn endpoint_resolves(area: &AreaRecord, secret: &SecretRecord, endpoint: &EndpointRecord) -> bool {
    match map_room_of(endpoint.room_number) {
        Some(map_room) => area.rooms.contains_key(&map_room),
        None => secret.rooms.contains_key(&endpoint.room_number),
    }
}

/// A Secret's bundle in the map's projection, as `viewer` reads it with
/// `actions` on it: its own properties and rooms, its data and exits on map
/// rooms (`room_data`), and the connections whose every endpoint the
/// Secret's reader can read. A connection needing a missing endpoint is
/// dropped, as the server drops it.
pub fn secret_bundle(
    st: &MockState,
    viewer: Uuid,
    area: &AreaRecord,
    secret: &SecretRecord,
    actions: &[&str],
) -> Value {
    let index = area
        .secrets
        .iter()
        .position(|s| s.id == secret.id)
        .expect("held Secret");
    super::source_refs::bundle(
        st,
        viewer,
        area,
        super::source_refs::Source::Secret(index),
        actions,
    )
}

// ---------------------------------------------------------------------------
// A Secret as a document the map's appliers run over
// ---------------------------------------------------------------------------

/// Secret `secret` of `area` as a map-shaped document under the map's id:
/// its own rooms and, at [`STAND_IN`] + n, every map room n the map holds
/// (where the map has it, carrying only the Secret's data for it), with the
/// Secret's exits, connections, labels, shapes and own properties. The
/// map's appliers run over it unchanged; [`store_doc`] takes it back.
pub fn secret_doc(area: &AreaRecord, secret: &SecretRecord) -> AreaRecord {
    let mut doc = AreaRecord::new(area.id, area.user_id, area.atlas_id, area.name.clone(), 0);
    doc.properties = secret.properties.clone();
    doc.rooms = secret.rooms.clone();
    let side = super::source_refs::Source::Secret(
        area.secrets
            .iter()
            .position(|s| s.id == secret.id)
            .expect("held source"),
    );
    for (number, map_room) in area.rooms.iter().filter(|(_, r)| r.anchor.is_none()) {
        let Some(preferred) = stand_in(*number) else {
            continue;
        };
        let existing = secret
            .rooms
            .iter()
            .find(|(key, _)| {
                super::source_refs::resolve(area, side, **key)
                    .is_some_and(|(_, r)| r.identity == map_room.identity)
            })
            .map(|(key, _)| *key);
        let key = existing.unwrap_or_else(|| {
            let mut key = preferred;
            while doc.rooms.contains_key(&key) {
                key += 1;
            }
            key
        });
        let kept = doc
            .rooms
            .entry(key)
            .or_insert_with(|| RoomRecord::placeholder(key));
        kept.anchor = Some(map_room.identity);
        kept.x = map_room.x;
        kept.y = map_room.y;
        kept.level = map_room.level;
    }
    doc.exits.clone_from(&secret.exits);
    doc.connections.clone_from(&secret.connections);
    doc.labels.clone_from(&secret.labels);
    doc.shapes.clone_from(&secret.shapes);
    doc
}

/// Takes `doc` (see [`secret_doc`]) back into `secret`, keeping a map
/// room's stand-in only while it holds something or something refers to it.
pub fn store_doc(secret: &mut SecretRecord, doc: AreaRecord) {
    secret.properties = doc.properties;
    secret.rooms = doc.rooms;
    secret.exits = doc.exits;
    secret.connections = doc.connections;
    secret.labels = doc.labels;
    secret.shapes = doc.shapes;
    prune_stand_ins(secret, doc.id);
}

/// Drops the stand-ins of `secret` (of map `area_id`) that hold nothing:
/// no data, no exits leaving them or leading to them, no connection end.
pub fn prune_stand_ins(secret: &mut SecretRecord, area_id: Uuid) {
    let mut referenced: BTreeSet<i32> = BTreeSet::new();
    for exit in &secret.exits {
        referenced.insert(exit.from_room_number);
        if exit.to_area_id == Some(area_id)
            && let Some(key) = exit.to_room_number
        {
            referenced.insert(key);
        }
    }
    for connection in &secret.connections {
        referenced.insert(connection.endpoint_a.room_number);
        if let Some(b) = &connection.endpoint_b {
            referenced.insert(b.room_number);
        }
    }
    secret.rooms.retain(|key, room| {
        map_room_of(*key).is_none()
            || !room.properties.is_empty()
            || !room.tags.is_empty()
            || referenced.contains(key)
    });
}

/// The server's cascade for map room `map_room` of `area_id`, deleted or
/// moved out of the map, within one Secret of that map: the Secret loses
/// its data on the room and the exits it keeps there, its exits into the
/// room lose their destination, and its connections anchored on the room
/// lose that end, or go with their last member. Whether anything changed.
pub fn forget_map_room(secret: &mut SecretRecord, area_id: Uuid, map_room: i32) -> bool {
    let Some(key) = stand_in(map_room) else {
        return false;
    };
    let before = format!("{:?}", (&secret.rooms, &secret.exits, &secret.connections));
    let mut working = super::connections::Working::new();
    let mut doc = AreaRecord::new(area_id, Uuid::nil(), None, String::new(), 0);
    doc.rooms = std::mem::take(&mut secret.rooms);
    doc.exits = std::mem::take(&mut secret.exits);
    doc.connections = std::mem::take(&mut secret.connections);
    working.insert(area_id, doc);
    super::connections::repair_after_room_delete(&mut working, area_id, key);
    let mut doc = working.remove(&area_id).expect("just inserted");
    doc.rooms.remove(&key);
    secret.rooms = doc.rooms;
    secret.exits = doc.exits;
    secret.connections = doc.connections;
    prune_stand_ins(secret, area_id);
    before != format!("{:?}", (&secret.rooms, &secret.exits, &secret.connections))
}

/// Exits into the rooms of Secret `secret` of map `map_id` (all of them, or
/// those in `rooms`) are deleted, with connections they leave memberless,
/// from every map and every map's Secrets (format-3 §4.2): dangling, they
/// would show to readers of their source who do not read the Secret. No
/// revision moves; only the tokens of those shown them change.
pub fn drop_exits_into_secret(
    areas: &mut BTreeMap<Uuid, AreaRecord>,
    map_id: Uuid,
    secret: Uuid,
    rooms: Option<&BTreeSet<i32>>,
) {
    let into = |exit: &ExitRecord| {
        exit.to_room_identity.is_none()
            && exit.to_secret == Some(secret)
            && exit.to_area_id == Some(map_id)
            && rooms.is_none_or(|rooms| exit.to_room_number.is_some_and(|n| rooms.contains(&n)))
    };
    let drop_from = |exits: &mut Vec<ExitRecord>, connections: &mut Vec<ConnectionRecord>| {
        let gone: BTreeSet<Uuid> = exits
            .iter()
            .filter(|exit| into(exit))
            .map(|exit| exit.connection_id)
            .collect();
        exits.retain(|exit| !into(exit));
        connections.retain(|connection| {
            !gone.contains(&connection.id)
                || exits.iter().any(|exit| exit.connection_id == connection.id)
        });
    };
    for area in areas.values_mut() {
        drop_from(&mut area.exits, &mut area.connections);
        for held in &mut area.secrets {
            drop_from(&mut held.exits, &mut held.connections);
        }
    }
}

/// Every other map's Secrets lose the destination of their exits into map
/// room `map_room` of `area_id`, as the map's own exits do. The Secrets
/// changed, as (map, Secret).
pub fn forget_inbound(
    areas: &mut BTreeMap<Uuid, AreaRecord>,
    area_id: Uuid,
    map_room: i32,
) -> Vec<(Uuid, Uuid)> {
    let mut changed = Vec::new();
    for (host_id, host) in areas.iter_mut() {
        if *host_id == area_id {
            continue;
        }
        for secret in &mut host.secrets {
            let mut touched = false;
            for exit in &mut secret.exits {
                if exit.to_room_identity.is_none()
                    && exit.to_secret.is_none()
                    && exit.to_area_id == Some(area_id)
                    && exit.to_room_number == Some(map_room)
                {
                    exit.to_area_id = None;
                    exit.to_room_number = None;
                    exit.to_direction = None;
                    touched = true;
                }
            }
            if touched {
                changed.push((*host_id, secret.id));
            }
        }
    }
    changed
}

pub(super) fn label_json(l: &LabelRecord) -> Value {
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

pub(super) fn shape_json(s: &ShapeRecord) -> Value {
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

#[derive(Deserialize)]
struct NameBody {
    name: Option<String>,
}

fn valid_name(body: &str) -> Result<String, Response> {
    let body: NameBody = parse_body(body)?;
    let Some(name) = body.name else {
        return Err(bad_request("missing field `name`"));
    };
    if name.chars().count() > NAME_LIMIT {
        return Err(bad_request(&format!(
            "Name must be at most {NAME_LIMIT} characters"
        )));
    }
    if name.contains('\0') {
        return Err(bad_request("Name must not contain NUL characters"));
    }
    Ok(name)
}

/// The map holding Secret `id` and the Secret's index in it, when `viewer`
/// may `action` it: an owner Secret's map owner, or on a Clan Secret a
/// holder of that action.
fn owned_secret(st: &MockState, viewer: Uuid, id: Uuid, action: &str) -> Option<(Uuid, usize)> {
    st.areas.values().find_map(|area| {
        let index = area.secrets.iter().position(|secret| secret.id == id)?;
        let secret = &area.secrets[index];
        let may = match &secret.clan {
            Some(_) => st.secret_actions(viewer, area, secret).contains(&action),
            None => area.user_id == viewer,
        };
        may.then_some((area.id, index))
    })
}

// ---------------------------------------------------------------------------
// POST /areas/{id}/secrets, PATCH and DELETE /secrets/{id}
// ---------------------------------------------------------------------------

pub async fn create_secret(
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
    // The body's shape is judged before the map is looked up.
    let name = match valid_name(&body) {
        Ok(name) => name,
        Err(e) => return e,
    };
    let fields: Value = match parse_body(&body) {
        Ok(fields) => fields,
        Err(e) => return e,
    };
    // `color` is optional on create, as the server's: absent or null leaves
    // it to the palette.
    let color = match fields.get("color") {
        None | Some(Value::Null) => None,
        Some(Value::String(color)) if smudgy_cloud::format3::parse_hex_color(color).is_some() => {
            Some(color.to_lowercase())
        }
        Some(_) => return bad_request("Color must be #rrggbb"),
    };
    let clan_owned = match fields.get("ownership") {
        None | Some(Value::Null) => false,
        Some(Value::String(owner)) if owner == "owner" => false,
        Some(Value::String(owner)) if owner == "members" || owner == "clan" => true,
        Some(_) => return bad_request("`ownership` is `owner`, `members` or `clan`"),
    };
    match fields.get("clan_id") {
        None | Some(Value::Null) => {}
        Some(Value::String(raw)) if Uuid::parse_str(raw).is_ok() => {
            if !clan_owned {
                return bad_request("An owner Secret takes no `clan_id`");
            }
        }
        Some(_) => return bad_request("`clan_id` must be a UUID"),
    }
    if !clan_owned && !st.caps(viewer, area_id).is_some_and(|caps| caps.is_owner) {
        return not_found();
    }
    if !clan_owned
        && st
            .areas
            .get(&area_id)
            .is_some_and(|area| area.clan_id.is_some())
    {
        return not_found();
    }
    let mut secret = SecretRecord::new(Uuid::new_v4(), name);
    secret.color = color;
    if clan_owned {
        return super::clan_secrets::create(&mut st, viewer, area_id, secret, &fields);
    }
    let out = summary(&secret, &OWNER_ACTIONS);
    st.areas
        .get_mut(&area_id)
        .expect("caps proved the area exists")
        .secrets
        .push(secret);
    created(out)
}

/// `{name?, color?}`, at least one; `color: null` clears it. A change
/// moves the Secret's revision, as the server's does; a PATCH that changes
/// nothing doesn't.
pub async fn rename_secret(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
    body: String,
) -> Response {
    let Ok(id) = Uuid::parse_str(&raw_id) else {
        return bad_request("Invalid Secret ID");
    };
    let mut st = state.lock();
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some((area_id, index)) = owned_secret(&st, viewer, id, "rename") else {
        return not_found();
    };
    let fields: Value = match parse_body(&body) {
        Ok(fields) => fields,
        Err(e) => return e,
    };
    let name = match fields.get("name") {
        None => None,
        Some(_) => match valid_name(&body) {
            Ok(name) => Some(name),
            Err(e) => return e,
        },
    };
    let color = match fields.get("color") {
        None => None,
        Some(Value::Null) => Some(None),
        Some(Value::String(color)) if smudgy_cloud::format3::parse_hex_color(color).is_some() => {
            Some(Some(color.to_lowercase()))
        }
        Some(_) => return bad_request("Color must be #rrggbb"),
    };
    if name.is_none() && color.is_none() {
        return bad_request("Nothing to change");
    }
    let current = &st.areas[&area_id].secrets[index];
    let changes = name.as_ref().is_some_and(|name| *name != current.name)
        || color.as_ref().is_some_and(|color| *color != current.color);
    if changes && let Some(refusal) = super::clan_maps::refused_while_disposing(&st, area_id) {
        return refusal;
    }
    let secret = &mut st.areas.get_mut(&area_id).expect("just found").secrets[index];
    let mut changed = false;
    if let Some(name) = name {
        changed |= secret.name != name;
        secret.name = name;
    }
    if let Some(color) = color {
        changed |= secret.color != color;
        secret.color = color;
    }
    if changed {
        secret.rev += 1;
    }
    if secret.clan.is_some() {
        let area = &st.areas[&area_id];
        let secret = &area.secrets[index];
        return ok(summary(secret, &st.secret_actions(viewer, area, secret)));
    }
    ok(summary(secret, &OWNER_ACTIONS))
}

pub async fn delete_secret(
    State(state): State<Shared>,
    Path(raw_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Ok(id) = Uuid::parse_str(&raw_id) else {
        return bad_request("Invalid Secret ID");
    };
    let mut st = state.lock();
    st.bind_room_references();
    let (viewer, _) = match authenticate(&st, &headers) {
        Ok(v) => v,
        Err(e) => return e,
    };
    let Some((area_id, index)) = owned_secret(&st, viewer, id, "delete") else {
        return not_found();
    };
    if let Some(refusal) = super::clan_maps::refused_while_disposing(&st, area_id) {
        return refusal;
    }
    st.areas
        .get_mut(&area_id)
        .expect("just found")
        .secrets
        .remove(index);
    drop_exits_into_secret(&mut st.areas, area_id, id, None);
    ok(Value::Null)
}
