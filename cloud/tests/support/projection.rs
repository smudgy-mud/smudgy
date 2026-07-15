//! Viewer-scoped area projection — the redaction core (`get_area_projection`
//! in the real db.rs): secrecy filtering, the exit survival predicate, hidden
//! target tokens, linked areas, the viewer-salted content hash, projected rev.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use uuid::Uuid;

use super::state::{AreaRecord, MockState, content_hash, to_area_token};

/// Full `ProjectedArea` for `GET /areas/{id}` / preview. `None` when the area
/// is absent or `can_view` is false (handler -> uniform 404).
pub fn project_area(state: &MockState, viewer: Uuid, area_id: Uuid) -> Option<Value> {
    let caps = state.caps(viewer, area_id)?;
    if !caps.can_view {
        return None;
    }
    let area = state.areas.get(&area_id)?;
    let see = caps.see_secrets();

    // (e) Area properties — single-flag predicate, name order.
    let properties: Vec<Value> = area
        .properties
        .iter()
        .filter(|(_, p)| see || !p.is_secret)
        .map(|(name, p)| json!({"name": name, "value": p.value}))
        .collect();

    // (c) Exits — survival predicate + destination redaction.
    let mut exits_by_room: BTreeMap<i32, Vec<Value>> = BTreeMap::new();
    let mut visible_targets: Vec<Uuid> = Vec::new();
    let mut hidden_tokens: Vec<String> = Vec::new();

    for exit in &area.exits {
        let Some(from_room) = area.rooms.get(&exit.from_room_number) else {
            continue; // FK guarantees this in the real DB
        };
        if !see && (exit.is_secret || from_room.is_secret) {
            continue;
        }
        // Secret to-room: same-area judged by host see_secrets; cross-area by
        // the viewer's include_secrets ON THE TARGET (default deny).
        if let (Some(to_area), Some(to_room)) = (exit.to_area_id, exit.to_room_number) {
            let target_room_secret = state
                .areas
                .get(&to_area)
                .and_then(|a| a.rooms.get(&to_room))
                .is_some_and(|r| r.is_secret);
            if target_room_secret {
                let ok = if to_area == area_id {
                    see
                } else {
                    // effective include_secrets already ORs target ownership.
                    state
                        .caps(viewer, to_area)
                        .is_some_and(|c| c.include_secrets)
                };
                if !ok {
                    continue;
                }
            }
        }

        let to_visible = exit.to_area_id.is_none_or(|to_area| {
            to_area == area_id || state.caps(viewer, to_area).is_some_and(|c| c.can_view)
        });

        let projected = if to_visible {
            if let Some(target) = exit.to_area_id
                && target != area_id
                && !visible_targets.contains(&target)
            {
                visible_targets.push(target);
            }
            json!({
                "id": exit.id,
                "from_room_number": exit.from_room_number,
                "from_direction": exit.from_direction,
                "to_area_id": exit.to_area_id,
                "to_room_number": exit.to_room_number,
                "to_direction": exit.to_direction,
                "to_unknown": false,
                "is_secret": see && exit.is_secret,
                "path": exit.path,
                "command": exit.command,
                "weight": exit.weight,
                "style": exit.style,
                "color": exit.color,
                "is_hidden": exit.is_hidden,
                "is_closed": exit.is_closed,
                "is_locked": exit.is_locked,
            })
        } else {
            let token = exit
                .to_area_id
                .map(|target| to_area_token(viewer, target))
                .expect("redacted exits always have a real target");
            if !hidden_tokens.contains(&token) {
                hidden_tokens.push(token.clone());
            }
            json!({
                "id": exit.id,
                "from_room_number": exit.from_room_number,
                "from_direction": exit.from_direction,
                "to_area_id": null,
                "to_room_number": null,
                "to_direction": null,
                "to_unknown": true,
                "to_area_token": token,
                "is_secret": see && exit.is_secret,
                "path": exit.path,
                "command": exit.command,
                "weight": exit.weight,
                "style": exit.style,
                "color": exit.color,
                "is_hidden": exit.is_hidden,
                "is_closed": exit.is_closed,
                "is_locked": exit.is_locked,
            })
        };
        exits_by_room
            .entry(exit.from_room_number)
            .or_default()
            .push(projected);
    }

    // (a)+(b) Rooms + room properties — single-flag predicate, number order.
    let rooms: Vec<Value> = area
        .rooms
        .values()
        .filter(|r| see || !r.is_secret)
        .map(|room| {
            let props: Vec<Value> = room
                .properties
                .iter()
                .filter(|(_, p)| see || !p.is_secret)
                .map(|(name, p)| json!({"name": name, "value": p.value}))
                .collect();
            json!({
                "room_number": room.room_number,
                "title": room.title,
                "description": room.description,
                "color": room.color,
                "level": room.level,
                "x": room.x,
                "y": room.y,
                "properties": props,
                "exits": exits_by_room.remove(&room.room_number).unwrap_or_default(),
                // Tags are non-secret: emitted whenever the room itself is visible.
                "tags": room.tags.iter().collect::<Vec<_>>(),
                // Not a secret; rides the projection whenever the room does.
                "external_id": room.external_id,
            })
        })
        .collect();

    let labels: Vec<Value> = area
        .labels
        .iter()
        .filter(|l| see || !l.is_secret)
        .map(|l| {
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
        })
        .collect();

    let shapes: Vec<Value> = area
        .shapes
        .iter()
        .filter(|s| see || !s.is_secret)
        .map(|s| {
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
        })
        .collect();

    // linked_areas: visible first (with resolved names), then hidden tokens.
    let mut linked_areas: Vec<Value> = Vec::new();
    for target in &visible_targets {
        let name = state.areas.get(target).map(|a| a.name.clone());
        let mut entry = json!({"to_area_id": target, "visible": true});
        if let Some(name) = name {
            entry["name"] = json!(name);
        }
        linked_areas.push(entry);
    }
    for token in hidden_tokens {
        linked_areas.push(json!({"to_area_token": token, "visible": false}));
    }

    // Viewer-salted content hash over the REDACTED projection tuple.
    let canonical =
        serde_json::to_vec(&(&properties, &rooms, &labels, &shapes)).unwrap_or_default();
    let hash = content_hash(viewer, &canonical);

    let mut out = json!({
        "id": area.id,
        "user_id": area.user_id,
        "atlas_id": area.atlas_id,
        "name": area.name,
        "created_at": area.created_at,
        "rev": if see { area.rev } else { area.public_rev },
        "access": {
            "is_owner": caps.is_owner,
            "can_edit": caps.can_edit,
            "can_reshare": caps.can_reshare,
            "can_copy": caps.can_copy,
            "can_admin": caps.can_admin,
            "include_secrets": caps.include_secrets,
        },
        "content_hash": hash,
        "properties": properties,
        "rooms": rooms,
        "labels": labels,
        "shapes": shapes,
        "linked_areas": linked_areas,
    });

    // Denormalized atlas name, un-redacted for every can_view viewer (§4.1);
    // key omitted when the area is atlas-less (skip-when-none).
    if let Some(name) = atlas_name(state, area) {
        out["atlas_name"] = json!(name);
    }

    // Provenance: owner-only, null fields OMITTED.
    if caps.is_owner {
        if let Some(src) = area.copied_from_area_id {
            out["copied_from_area_id"] = json!(src);
        }
        if let Some(rev) = area.copied_from_rev {
            out["copied_from_rev"] = json!(rev);
        }
        if let Some(at) = area.copied_at {
            out["copied_at"] = json!(at);
        }
    }
    Some(out)
}

/// The area's denormalized atlas name (§4.1 of the map-server-scoping plan;
/// the `LEFT JOIN map_atlases` in `get_areas_for_viewer`/`get_area_projection`).
/// `Some` iff the area is filed in an atlas that exists in `MockState.atlases`;
/// emitted skip-when-none. The atlas CONTAINER is no longer redacted: any viewer
/// who `can_view` the area — the only viewers these projectors are ever invoked
/// for — sees the un-redacted `atlas_id` alongside this name. Container ops stay
/// gated by the atlas-scope checks, so knowing the id/name confers nothing.
pub fn atlas_name(state: &MockState, area: &AreaRecord) -> Option<String> {
    let atlas = area.atlas_id?;
    state.atlases.get(&atlas).map(|a| a.name.clone())
}

/// One row of `GET /areas` (`ProjectedAreaListItem`).
pub fn project_list_item(state: &MockState, viewer: Uuid, area: &AreaRecord) -> Value {
    let caps = state.caps(viewer, area.id).expect("area exists");
    let see = caps.see_secrets();
    let mut out = json!({
        "id": area.id,
        "user_id": area.user_id,
        "atlas_id": area.atlas_id,
        "name": area.name,
        "created_at": area.created_at,
        "rev": if see { area.rev } else { area.public_rev },
        "access": {
            "is_owner": caps.is_owner,
            "can_edit": caps.can_edit,
            "can_reshare": caps.can_reshare,
            "can_copy": caps.can_copy,
            "can_admin": caps.can_admin,
            "include_secrets": caps.include_secrets,
        },
    });
    // Denormalized atlas name, un-redacted for every can_view viewer (§4.1);
    // key omitted when the area is atlas-less (skip-when-none).
    if let Some(name) = atlas_name(state, area) {
        out["atlas_name"] = json!(name);
    }
    if caps.is_owner {
        if let Some(src) = area.copied_from_area_id {
            out["copied_from_area_id"] = json!(src);
        }
        if let Some(rev) = area.copied_from_rev {
            out["copied_from_rev"] = json!(rev);
        }
        if let Some(at) = area.copied_at {
            out["copied_at"] = json!(at);
        }
    } else if let Some(nickname) = state.user(area.user_id).and_then(|u| u.nickname.clone()) {
        // Owner nickname only on rows shared TO the caller, only when allocated.
        out["owner_nickname"] = json!(nickname);
    }
    out
}

/// Whether `viewer` can view `area` (owned or any covering grant) — the
/// row-inclusion predicate for `GET /areas` and `GET /sync`.
pub fn viewer_covers(state: &MockState, viewer: Uuid, area: &AreaRecord) -> bool {
    area.user_id == viewer
        || state
            .grants
            .iter()
            .any(|g| g.grantee_id == viewer && g.covers_area(area))
}
