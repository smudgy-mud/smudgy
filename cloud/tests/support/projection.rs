//! Viewer-scoped area projection — the redaction core (`get_area_projection`
//! in the real db.rs): the §6 Connection closure, the exit survival
//! predicate (an exit survives exactly when its Connection does), hidden
//! target tokens, linked areas, the viewer-salted content hash, projected
//! rev.

use std::collections::BTreeMap;

use serde_json::{Value, json};
use uuid::Uuid;

use super::state::{AreaRecord, ConnectionRecord, MockState, content_hash, to_area_token};

/// The §6 closure verdict for one Connection, evaluated over its whole
/// membership exactly like the server's `member_facts` CTE.
pub struct ConnectionVerdict {
    /// Any host-owned secret cause: member exit, either endpoint room, or a
    /// same-area destination room.
    pub host_secret_cause: bool,
    /// Any cross-area destination room is secret (cleared or not) — the
    /// derived `effective_secret` indicator folds it in.
    pub any_cross_secret: bool,
    /// A cross-area secret destination the viewer is NOT cleared for
    /// (`is_owner OR include_secrets` on the TARGET area; default deny).
    pub uncleared_cross_secret: bool,
    /// Any member leaves the area (drives the External kind).
    pub any_external: bool,
}

impl ConnectionVerdict {
    /// Whether the group is omitted for a viewer with host clearance `see`.
    pub fn omitted(&self, see: bool) -> bool {
        (self.host_secret_cause && !see) || self.uncleared_cross_secret
    }
}

/// Evaluates the §6 closure for `connection` in `area`, as `viewer`,
/// against the live state.
pub fn connection_verdict(
    state: &MockState,
    viewer: Uuid,
    area: &AreaRecord,
    connection: &ConnectionRecord,
) -> ConnectionVerdict {
    connection_verdict_in(
        &state.areas,
        |target| state.caps(viewer, target),
        area,
        connection,
    )
}

/// [`connection_verdict`] over an arbitrary area map — the mutation
/// endpoint's echoes evaluate it against the envelope's working copy, like
/// the server's in-transaction queries.
pub fn connection_verdict_in(
    areas: &BTreeMap<Uuid, AreaRecord>,
    caps: impl Fn(Uuid) -> Option<super::state::Caps>,
    area: &AreaRecord,
    connection: &ConnectionRecord,
) -> ConnectionVerdict {
    let members: Vec<_> = area
        .exits
        .iter()
        .filter(|exit| exit.connection_id == connection.id)
        .collect();
    let any_member_secret = members.iter().any(|exit| exit.is_secret);
    let same_area_dest_secret = members.iter().any(|exit| {
        exit.to_area_id == Some(area.id)
            && exit
                .to_room_number
                .and_then(|room| area.rooms.get(&room))
                .is_some_and(|room| room.is_secret)
    });
    let cross_secret = |exit: &super::state::ExitRecord| -> bool {
        matches!(
            (exit.to_area_id, exit.to_room_number),
            (Some(to_area), Some(to_room)) if to_area != area.id
                && areas
                    .get(&to_area)
                    .and_then(|target| target.rooms.get(&to_room))
                    .is_some_and(|room| room.is_secret)
        )
    };
    let any_cross_secret = members.iter().any(|exit| cross_secret(exit));
    let uncleared_cross_secret = members.iter().any(|exit| {
        cross_secret(exit)
            && !exit.to_area_id.is_some_and(|to_area| {
                caps(to_area).is_some_and(|target| target.is_owner || target.include_secrets)
            })
    });
    let any_external = members
        .iter()
        .any(|exit| exit.to_area_id.is_some_and(|to_area| to_area != area.id));

    let endpoint_room_secret = |room: i32| area.rooms.get(&room).is_some_and(|r| r.is_secret);
    let a_room_secret = endpoint_room_secret(connection.endpoint_a.room_number);
    let b_room_secret = connection
        .endpoint_b
        .as_ref()
        .is_some_and(|b| endpoint_room_secret(b.room_number));

    ConnectionVerdict {
        host_secret_cause: any_member_secret
            || a_room_secret
            || b_room_secret
            || same_area_dest_secret,
        any_cross_secret,
        uncleared_cross_secret,
        any_external,
    }
}

/// The derived, never-stored Connection kind, exactly as the server derives
/// it at projection: endpoint shape first, then external membership, then
/// room levels.
pub fn connection_kind(area: &AreaRecord, connection: &ConnectionRecord, any_external: bool) -> &'static str {
    match connection.endpoint_b.as_ref() {
        None => {
            if any_external {
                "External"
            } else {
                "Dangling"
            }
        }
        Some(b) if b.room_number == connection.endpoint_a.room_number => "SelfLoop",
        Some(b) => {
            let level_of = |room: i32| area.rooms.get(&room).map_or(0, |r| r.level);
            if level_of(b.room_number) == level_of(connection.endpoint_a.room_number) {
                "Internal"
            } else {
                "CrossLevel"
            }
        }
    }
}

fn endpoint_json(endpoint: &super::state::EndpointRecord) -> Value {
    json!({
        "room_number": endpoint.room_number,
        "side": endpoint.side,
        "port_offset": endpoint.port_offset,
        "port_mode": endpoint.port_mode,
    })
}

/// One projected Connection (the server's `Connection` wire struct,
/// `effective_secret` included).
fn connection_json(
    area: &AreaRecord,
    connection: &ConnectionRecord,
    verdict: &ConnectionVerdict,
) -> Value {
    let mut out = json!({
        "id": connection.id,
        "endpoint_a": endpoint_json(&connection.endpoint_a),
        "kind": connection_kind(area, connection, verdict.any_external),
        "routing": connection.routing,
        "segment_shape": connection.segment_shape,
        "corner": connection.corner,
        "route_points": connection
            .route_points
            .iter()
            .map(|(x, y)| json!({"x": x, "y": y}))
            .collect::<Vec<_>>(),
        "dash": connection.dash,
        "color": connection.color,
        "thickness": connection.thickness,
        // Every cause the closure tracks, cleared or not — survivors of an
        // uncleared viewer are all-public, so this is only ever true for a
        // viewer cleared on every secret cause.
        "effective_secret": verdict.host_secret_cause || verdict.any_cross_secret,
    });
    // Endpoint B rides the wire as an omitted field, not an explicit null.
    if let Some(b) = connection.endpoint_b.as_ref() {
        out["endpoint_b"] = endpoint_json(b);
    }
    out
}

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

    // (c) Connections under the §6 closure, stably id-sorted: an omitted
    // group leaves no trace, and its member exits vanish with it below.
    let mut sorted_connections: Vec<&ConnectionRecord> = area.connections.iter().collect();
    sorted_connections.sort_by_key(|connection| connection.id);
    let mut surviving: Vec<Uuid> = Vec::new();
    let mut connections: Vec<Value> = Vec::new();
    for connection in sorted_connections {
        let verdict = connection_verdict(state, viewer, area, connection);
        if verdict.omitted(see) {
            continue;
        }
        surviving.push(connection.id);
        connections.push(connection_json(area, connection, &verdict));
    }

    // (d) Exits — an exit survives exactly when its Connection does;
    // `to_visible` drives the unchanged unknown-target projection for
    // surviving exits into inaccessible (but not secret) foreign areas.
    let mut exits_by_room: BTreeMap<i32, Vec<Value>> = BTreeMap::new();
    let mut visible_targets: Vec<Uuid> = Vec::new();
    let mut hidden_tokens: Vec<String> = Vec::new();

    for exit in &area.exits {
        if !surviving.contains(&exit.connection_id) {
            continue;
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
                "connection_id": exit.connection_id,
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
                "connection_id": exit.connection_id,
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

    // linked_areas: derived only from surviving projected exits — visible
    // first (with resolved names), then hidden tokens.
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

    // Viewer-salted content hash over the REDACTED projection tuple, the
    // format version and closure-filtered connections included (a format
    // change can never alias an unchanged hash).
    let canonical = serde_json::to_vec(&(
        super::state::AREA_FORMAT_VERSION,
        &properties,
        &rooms,
        &labels,
        &shapes,
        &connections,
    ))
    .unwrap_or_default();
    let hash = content_hash(viewer, &canonical);

    let mut out = json!({
        "format_version": super::state::AREA_FORMAT_VERSION,
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
        "connections": connections,
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
