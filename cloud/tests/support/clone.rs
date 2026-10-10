//! Secrets listing and copies: GET /areas/{id}/secrets, POST
//! /areas/{id}/copy, POST /atlases/{id}/copy.
//! Fidelity reference: the smudgy-cloudflare service (`src/maps/copies.ts`,
//! `src/library/maps/copies.ts`; docs/format-3.md §5.2). A copy carries the
//! map whole, with pairwise exit remap, and every Secret the copier holds
//! `copy` on as an owner Secret of the copy with no grants. A Secret they
//! read without `copy`, like one they do not read, leaves no trace.

use std::collections::HashMap;
use std::collections::btree_map::Entry;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::Response;
use chrono::Utc;
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use uuid::Uuid;

use super::http::{
    authenticate, bad_request, created, gate_verified, not_found, ok, parse_area_id, parse_body,
};
use super::secret_grants::COPY;
use super::state::{
    AreaRecord, AtlasRecord, Caps, ExitRecord, LabelRecord, MockState, RoomRecord, SecretRecord,
    ShapeRecord,
};

pub type Shared = Arc<Mutex<MockState>>;

// ---------------------------------------------------------------------------
// GET /areas/{id}/secrets
// ---------------------------------------------------------------------------

/// GET /areas/{id}/secrets — the Secrets on the map the caller can read, as
/// `{source, name, ownership, actions}`, in creation order.
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
    if !st.caps(viewer, area_id).is_some_and(|caps| caps.can_view) {
        return not_found();
    }
    let area = st.areas.get(&area_id).expect("caps proved the area exists");
    ok(json!(
        st.readable_secrets(viewer, area)
            .iter()
            .map(|(secret, actions)| super::secrets::summary(secret, actions))
            .collect::<Vec<_>>()
    ))
}

// ---------------------------------------------------------------------------
// Clone materializer (shared by area copy and atlas copy)
// ---------------------------------------------------------------------------

/// Materialize the map content of every `(src, new)` pair as owned rows in
/// the clones; remap exits pairwise; dangle targets the caller cannot read.
/// Mirrors `materialize_clone`: FK placeholders synthesized, rev triggers
/// fired per row.
fn materialize_clone(st: &mut MockState, viewer: Uuid, area_map: &[(Uuid, Uuid)]) {
    st.bind_room_references();
    let mut room_ids = HashMap::new();
    let remap: HashMap<Uuid, Uuid> = area_map.iter().copied().collect();

    // Per-target visibility, resolved BEFORE mutation.
    let mut target_visible: HashMap<Uuid, bool> = HashMap::new();
    for (src, _) in area_map {
        let Some(area) = st.areas.get(src) else {
            continue;
        };
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
        let Some(source) = st.areas.get(src).cloned() else {
            continue;
        };
        let mut row_count = 0usize;
        {
            let clone = st.areas.get_mut(new_area).expect("clone header exists");
            for room in source.rooms.values() {
                // The room, each property, and each tag are one row apiece.
                row_count += 1 + room.properties.len() + room.tags.len();
                let mut copied = room.clone();
                copied.identity = Uuid::new_v4();
                room_ids.insert(room.identity, copied.identity);
                clone.rooms.insert(room.room_number, copied);
            }
            for label in &source.labels {
                let mut copied = label.clone();
                copied.id = Uuid::new_v4();
                row_count += 1;
                clone.labels.push(copied);
            }
            for shape in &source.shapes {
                let mut copied = shape.clone();
                copied.id = Uuid::new_v4();
                row_count += 1;
                clone.shapes.push(copied);
            }
            for (name, prop) in &source.properties {
                row_count += 1;
                clone.properties.insert(name.clone(), prop.clone());
            }
        }
        for _ in 0..row_count {
            st.bump(Some(*new_area));
        }
    }

    // PASS 2 — Connections first (fresh UUIDs), then exits with rewired
    // `connection_id`s, mirroring the server: an exit is copied exactly
    // when its Connection was.
    for (src, new_area) in area_map {
        let Some(source) = st.areas.get(src).cloned() else {
            continue;
        };

        // Connections, copied under fresh ids. Endpoint B (and the stored
        // route with it) clears when the clone lacks its room — a copied
        // route may never keep a coordinate frame the clone does not
        // contain.
        let mut connection_map: HashMap<Uuid, Uuid> = HashMap::new();
        let mut copied_connections = Vec::new();
        for connection in &source.connections {
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
        // re-resolved (remapped clone / kept visible target / dangled). An
        // exit into another map's Secret room comes along only for a
        // copier who reads that Secret, keeping its destination, and is
        // otherwise left out with its connection, never dangled.
        let mut staged = Vec::new();
        for exit in &source.exits {
            let Some(new_connection) = connection_map.get(&exit.connection_id) else {
                continue;
            };
            let (new_to_area, new_to_room, new_to_dir) = match exit.to_area_id {
                _ if exit.to_room_identity.is_some() || exit.to_secret.is_some() => (
                    exit.to_area_id,
                    exit.to_room_number,
                    exit.to_direction.clone(),
                ),
                None => (None, None, None),
                Some(target) => {
                    if let Some(mapped) = remap.get(&target) {
                        (
                            Some(*mapped),
                            exit.to_room_number,
                            exit.to_direction.clone(),
                        )
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
            staged.push(copied);
        }

        // Defensive FK placeholders (every from-room/same-area to-room was
        // normally copied in pass 1).
        let mut placeholder_rooms: Vec<i32> = Vec::new();
        {
            let clone = st.areas.get_mut(new_area).expect("clone exists");
            copied_connections.retain(|connection| {
                staged
                    .iter()
                    .any(|exit: &ExitRecord| exit.connection_id == connection.id)
            });
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
            st.bump(Some(*new_area));
        }
        // Land the exits, firing the two-sided insert trigger.
        let mut bumps: Vec<Option<Uuid>> = Vec::new();
        {
            let clone = st.areas.get_mut(new_area).expect("clone exists");
            for exit in staged {
                bumps.push(Some(*new_area));
                if exit.to_secret.is_none() {
                    bumps.push(exit.to_area_id);
                }
                clone.exits.push(exit);
            }
        }
        for target in bumps {
            st.bump(target);
        }
    }

    // PASS 3 — every Secret the copier holds `copy` on, of any ownership,
    // becomes an owner Secret of the copy: same name, color and content, a
    // new id, revision 1 and no grants. A Secret they read without `copy`,
    // like one they cannot read, leaves no trace.
    for (src, new_area) in area_map {
        let Some(source) = st.areas.get(src) else {
            continue;
        };
        let copies: Vec<SecretRecord> = st
            .readable_secrets(viewer, source)
            .into_iter()
            .filter(|(_, actions)| actions.contains(&COPY))
            .map(|(secret, _)| copy_secret(st, viewer, secret, *src, *new_area, &remap))
            .collect();
        st.areas
            .get_mut(new_area)
            .expect("clone exists")
            .secrets
            .extend(copies);
    }
    for (src, new_area) in area_map {
        let source = &st.areas[src];
        let clone = &st.areas[new_area];
        let copied_sources: Vec<_> = st
            .readable_secrets(viewer, source)
            .into_iter()
            .filter(|(_, actions)| actions.contains(&COPY))
            .map(|(secret, _)| secret)
            .collect();
        for (original, copied) in copied_sources.iter().zip(&clone.secrets) {
            for (number, room) in &original.rooms {
                if let Some(copied_room) = copied.rooms.get(number) {
                    room_ids.insert(room.identity, copied_room.identity);
                }
            }
        }
    }
    for (_, new_area) in area_map {
        let clone = st.areas.get_mut(new_area).unwrap();
        for room in clone
            .rooms
            .values_mut()
            .chain(clone.secrets.iter_mut().flat_map(|s| s.rooms.values_mut()))
        {
            if let Some(anchor) = room.anchor {
                room.anchor = Some(room_ids.get(&anchor).copied().unwrap_or_else(Uuid::new_v4));
            }
        }
        for exit in clone.exits.iter_mut().chain(
            clone
                .secrets
                .iter_mut()
                .flat_map(|secret| &mut secret.exits),
        ) {
            if let Some(identity) = exit.to_room_identity.and_then(|id| room_ids.get(&id)) {
                exit.to_room_identity = Some(*identity);
                if let Some(target) = exit.to_area_id.and_then(|area| remap.get(&area)) {
                    exit.to_area_id = Some(*target);
                }
            }
        }
    }
}

/// A Secret of map `src` as an owner Secret of its copy `new_area`. Its
/// rooms keep their numbers, so its data stays keyed to the same map rooms;
/// exits, connections, labels and shapes get new ids. An exit into the
/// copied maps leads into the copies, one into a map the copier reads stays,
/// and any other dangles.
fn copy_secret(
    st: &MockState,
    viewer: Uuid,
    secret: &SecretRecord,
    src: Uuid,
    new_area: Uuid,
    remap: &HashMap<Uuid, Uuid>,
) -> SecretRecord {
    let mut copy = SecretRecord::new(Uuid::new_v4(), secret.name.clone());
    copy.color.clone_from(&secret.color);
    copy.properties.clone_from(&secret.properties);
    copy.rooms.clone_from(&secret.rooms);
    for room in copy.rooms.values_mut() {
        room.identity = Uuid::new_v4();
    }
    let mut connections: HashMap<Uuid, Uuid> = HashMap::new();
    for connection in &secret.connections {
        let mut copied = connection.clone();
        copied.id = Uuid::new_v4();
        connections.insert(connection.id, copied.id);
        copy.connections.push(copied);
    }
    for exit in &secret.exits {
        let mut copied = exit.clone();
        copied.id = Uuid::new_v4();
        if let Some(connection) = connections.get(&exit.connection_id) {
            copied.connection_id = *connection;
        }
        if let Some(target) = exit.to_area_id {
            if target == src {
                copied.to_area_id = Some(new_area);
            } else if let Some(mapped) = remap.get(&target) {
                copied.to_area_id = Some(*mapped);
            } else if exit.to_room_identity.is_none()
                && !st.caps(viewer, target).is_some_and(|caps| caps.can_view)
            {
                copied.to_area_id = None;
                copied.to_room_number = None;
                copied.to_direction = None;
            }
        }
        copy.exits.push(copied);
    }
    copy.labels = secret
        .labels
        .iter()
        .map(|label| LabelRecord {
            id: Uuid::new_v4(),
            ..label.clone()
        })
        .collect();
    copy.shapes = secret
        .shapes
        .iter()
        .map(|shape| ShapeRecord {
            id: Uuid::new_v4(),
            ..shape.clone()
        })
        .collect();
    copy
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
/// map's content with provenance; response rev is the header's
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
    if let Some(refusal) = req.name.as_deref().and_then(super::http::name_refusal) {
        return refusal;
    }

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
/// silently); intra-atlas links remap pairwise. An atlas the caller reads
/// no map of and does not administer, or a clan's atlas to a non-member,
/// is the uniform 404 (the server's `exportAtlasCopy`).
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
    if let Some(refusal) = req.name.as_deref().and_then(super::http::name_refusal) {
        return refusal;
    }

    let Some(atlas) = st.atlases.get(&atlas_id) else {
        return not_found();
    };
    let atlas_name = atlas.name.clone();

    // Member areas in stable (created, id) order.
    let mut members: Vec<(u64, Uuid)> = st
        .areas
        .values()
        .filter(|a| a.atlas_id == Some(atlas_id))
        .map(|a| (a.created_seq, a.id))
        .collect();
    members.sort_unstable();

    // The atlas is reachable when the caller reads one of its maps or
    // administers it; a clan's atlas, by the clan's members alone. Anything
    // else is the uniform 404, revealing nothing of it.
    let reads_a_map = members
        .iter()
        .any(|(_, member)| st.caps(viewer, *member).is_some_and(|caps| caps.can_view));
    let administers = atlas.clan_id.is_none()
        && (atlas.user_id == viewer
            || st.grants.iter().any(|grant| {
                grant.atlas_id == Some(atlas_id) && grant.grantee_id == viewer && grant.can_admin
            }));
    let outsider = atlas.clan_id.is_some_and(|clan| {
        !st.clans
            .clans
            .get(&clan)
            .is_some_and(|record| record.has_member(viewer))
    });
    if (!reads_a_map && !administers) || outsider {
        return not_found();
    }

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

    let new_name = req.name.unwrap_or_else(|| format!("{atlas_name} (copy)"));
    let new_atlas_id = Uuid::new_v4();
    st.atlases.insert(
        new_atlas_id,
        AtlasRecord {
            id: new_atlas_id,
            user_id: viewer,
            clan_id: None,
            name: new_name.clone(),
            created_at: Utc::now(),
            rev: 1,
        },
    );

    let area_map: Vec<(Uuid, Uuid)> = copyable.iter().map(|src| (*src, Uuid::new_v4())).collect();
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
