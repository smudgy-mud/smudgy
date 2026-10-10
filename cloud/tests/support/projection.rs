//! Viewer-scoped area projection (`GET /areas/{id}`): the map bundle and the
//! viewer's readable Secret bundles, hidden-target tokens for exits into
//! maps the viewer cannot read, linked areas, and the projection token.

use serde_json::{Value, json};
use uuid::Uuid;

use super::state::{AreaRecord, ConnectionRecord, MockState, projection_token};

/// Whether any member exit of `connection` leaves `area` (drives the
/// External kind).
fn any_external(area: &AreaRecord, connection: &ConnectionRecord) -> bool {
    area.exits.iter().any(|exit| {
        exit.connection_id == connection.id
            && exit.to_area_id.is_some_and(|to_area| to_area != area.id)
    })
}

/// The derived, never-stored Connection kind, exactly as the server derives
/// it at projection: endpoint shape first, then external membership, then
/// room levels.
pub fn connection_kind(area: &AreaRecord, connection: &ConnectionRecord) -> &'static str {
    match connection.endpoint_b.as_ref() {
        None => {
            if any_external(area, connection) {
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

pub(super) fn endpoint_json(endpoint: &super::state::EndpointRecord) -> Value {
    json!({
        "room_number": endpoint.room_number,
        "side": endpoint.side,
        "port_offset": endpoint.port_offset,
        "port_mode": endpoint.port_mode,
    })
}

/// One projected Connection (the server's `Connection` wire struct).
pub(super) fn connection_json(area: &AreaRecord, connection: &ConnectionRecord) -> Value {
    let mut out = json!({
        "id": connection.id,
        "endpoint_a": endpoint_json(&connection.endpoint_a),
        "kind": connection_kind(area, connection),
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
    });
    // Endpoint B rides the wire as an omitted field, not an explicit null.
    if let Some(b) = connection.endpoint_b.as_ref() {
        out["endpoint_b"] = endpoint_json(b);
    }
    out
}

/// Whether connection `id`, kept beside `exits`, is hidden from `viewer`: it
/// has members, and every one is an exit into a Secret room they do not
/// read.
pub(super) fn hidden_connection(
    state: &MockState,
    viewer: Uuid,
    exits: &[super::state::ExitRecord],
    id: Uuid,
) -> bool {
    let mut members = exits
        .iter()
        .filter(|exit| exit.connection_id == id)
        .peekable();
    members.peek().is_some() && members.all(|exit| !state.shows_exit(viewer, exit))
}

/// Full `ProjectedArea` for `GET /areas/{id}` / preview. `None` when the area
/// is absent or `can_view` is false (handler -> uniform 404).
pub fn project_area(state: &MockState, viewer: Uuid, area_id: Uuid) -> Option<Value> {
    let caps = state.caps(viewer, area_id)?;
    if !caps.can_view {
        return None;
    }
    let area = state.areas.get(&area_id)?;

    let bundles = super::source_refs::bundles(state, viewer, area);
    let atlas = atlas_name(state, area);
    let mut out = json!({
        "format_version": super::state::WIRE_FORMAT_VERSION,
        "id": area.id, "user_id": area.user_id, "atlas_id": area.atlas_id,
        "name": area.name, "created_at": area.created_at,
        "access": {
            "is_owner": caps.is_owner, "can_edit": caps.can_edit,
            "can_reshare": caps.can_reshare, "can_copy": caps.can_copy,
            "can_admin": caps.can_admin, "include_secrets": caps.include_secrets,
        },
        "projection_token": projection_token(state, viewer, area, &caps, atlas.as_deref()),
        "linked_areas": super::source_refs::linked(state, area.id, &bundles),
        "sources": bundles,
    });

    // Denormalized atlas name, un-redacted for every can_view viewer (§4.1);
    // key omitted when the area is atlas-less (skip-when-none).
    if let Some(name) = atlas {
        out["atlas_name"] = json!(name);
    }
    super::clan_maps::ownership_fields(state, viewer, area, &mut out);

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
    let mut out = json!({
        "id": area.id,
        "user_id": area.user_id,
        "atlas_id": area.atlas_id,
        "name": area.name,
        "created_at": area.created_at,
        "projection_token": projection_token(
            state,
            viewer,
            area,
            &caps,
            atlas_name(state, area).as_deref(),
        ),
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
    } else if area.clan_id.is_none()
        && let Some(nickname) = state.user(area.user_id).and_then(|u| u.nickname.clone())
    {
        // Owner nickname only on rows shared TO the caller, only when allocated.
        out["owner_nickname"] = json!(nickname);
    }
    super::clan_maps::list_fields(state, viewer, area, &mut out);
    out
}

/// Whether `viewer` can view `area` (owned or any covering grant) — the
/// row-inclusion predicate for `GET /areas` and `GET /sync`.
pub fn viewer_covers(state: &MockState, viewer: Uuid, area: &AreaRecord) -> bool {
    state
        .caps(viewer, area.id)
        .is_some_and(|caps| caps.can_view)
}
