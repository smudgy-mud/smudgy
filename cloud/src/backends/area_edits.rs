//! Pure mutation helpers over [`AreaWithDetails`], shared by the backends
//! that own full area documents ([`super::local`] on disk,
//! [`super::ephemeral`] in memory). Each mirrors the server's semantics for
//! the corresponding write, so the backends stay thin wrappers that add
//! persistence and locking around one authoritative document edit.
//!
//! [`apply_envelope`] is the shared CAS applier for those tiers: it validates
//! the envelope's precondition against the document's revision, applies every
//! operation in order (all-or-nothing — the caller discards the document on
//! any error), bumps the revision exactly once, and assembles the same
//! [`MutationResult`] echo shape the server returns. Local tiers are
//! single-user and always cleared, so there is no secrecy gating or
//! redaction anywhere in this module.

use uuid::Uuid;

use crate::{
    AreaId, AreaWithDetails, CloudError, CloudResult, Connection, ConnectionArgs, ConnectionId,
    ConnectionKind, ConnectionRouting, DEFAULT_CONNECTION_COLOR, Exit, ExitArgs, ExitId,
    ExitUpdates, Label, LabelArgs, LabelId, MAX_COLOR_LEN, MAX_COORDINATE, MAX_ROUTE_POINTS,
    MapPoint, Property, RoomNumber, RoomUpdates, RoomWithDetails, SegmentShape, Shape, ShapeArgs,
    ShapeId, THICKNESS_RANGE, canonicalize_css_color, connection_geometry,
    connection_lifecycle::{self, ExitTopology, RoomSite},
    mapper::RoomKey,
    mutation::{
        AreaMutation, MutationEnvelope, MutationResult, OpResult, Precondition, ResourceKind,
        VersionInfo,
    },
};

pub(super) fn apply_room_updates(room: &mut RoomWithDetails, updates: &RoomUpdates) {
    if let Some(title) = &updates.title {
        room.title.clone_from(title);
    }
    if let Some(description) = &updates.description {
        room.description.clone_from(description);
    }
    if let Some(level) = updates.level {
        room.level = level;
    }
    if let Some(x) = updates.x {
        room.x = x;
    }
    if let Some(y) = updates.y {
        room.y = y;
    }
    if let Some(color) = &updates.color {
        room.color.clone_from(color);
    }
    if let Some(is_secret) = updates.is_secret {
        room.is_secret = is_secret;
    }
    if let Some(external_id) = &updates.external_id {
        room.external_id.clone_from(external_id);
    }
}

pub(super) fn apply_exit_updates(exit: &mut Exit, updates: ExitUpdates) {
    if let Some(from_direction) = updates.from_direction {
        exit.from_direction = from_direction;
    }
    // Mirror the server's COALESCE semantics: only `clear_to` nulls a
    // destination; an absent `to_*` leaves it unchanged.
    if updates.clear_to == Some(true) {
        exit.to_area_id = None;
        exit.to_room_number = None;
        exit.to_direction = None;
    } else {
        if let Some(to_area_id) = updates.to_area_id {
            exit.to_area_id = Some(to_area_id);
        }
        if let Some(to_room_number) = updates.to_room_number {
            exit.to_room_number = Some(to_room_number);
        }
        if let Some(to_direction) = updates.to_direction {
            exit.to_direction = Some(to_direction);
        }
    }
    if let Some(path) = updates.path {
        exit.path = path;
    }
    if let Some(is_hidden) = updates.is_hidden {
        exit.is_hidden = is_hidden;
    }
    if let Some(is_closed) = updates.is_closed {
        exit.is_closed = is_closed;
    }
    if let Some(is_locked) = updates.is_locked {
        exit.is_locked = is_locked;
    }
    if let Some(weight) = updates.weight {
        exit.weight = weight;
    }
    if let Some(command) = updates.command {
        exit.command = command;
    }
    if let Some(is_secret) = updates.is_secret {
        exit.is_secret = is_secret;
    }
}

/// Sets (or inserts) a property on a `Vec<Property>`. Secrecy follows the
/// server's COALESCE: an absent `is_secret` preserves the existing flag
/// (defaulting to public on insert); a present one sets it.
pub(super) fn upsert_property(
    properties: &mut Vec<Property>,
    name: &str,
    value: &str,
    is_secret: Option<bool>,
) {
    if let Some(existing) = properties.iter_mut().find(|p| p.name == name) {
        existing.value = value.to_string();
        if let Some(is_secret) = is_secret {
            existing.is_secret = is_secret;
        }
    } else {
        properties.push(Property {
            name: name.to_string(),
            value: value.to_string(),
            is_secret: is_secret.unwrap_or(false),
        });
    }
}

/// A room as `PUT /areas/{a}/{room}` first materializes it: every field at
/// its server-side column default.
fn blank_room(number: RoomNumber) -> RoomWithDetails {
    RoomWithDetails {
        room_number: number,
        title: String::new(),
        description: String::new(),
        level: 0,
        x: 0.0,
        y: 0.0,
        color: String::new(),
        properties: Vec::new(),
        exits: Vec::new(),
        tags: std::collections::BTreeSet::default(),
        is_secret: false,
        external_id: None,
    }
}

/// Upserts a room (`PUT /areas/{a}/{room}` creates the room if absent) and
/// returns it as stored.
fn upsert_room_details<'a>(
    area: &'a mut AreaWithDetails,
    number: RoomNumber,
    updates: &RoomUpdates,
) -> &'a mut RoomWithDetails {
    let idx = area
        .rooms
        .iter()
        .position(|r| r.room_number == number)
        .unwrap_or_else(|| {
            area.rooms.push(blank_room(number));
            area.rooms.len() - 1
        });
    let room = &mut area.rooms[idx];
    apply_room_updates(room, updates);
    room
}

/// Deletes a room and mirrors the server's cascade within the area:
/// inbound destinations are nulled, orphaned Connections deleted, and
/// Connections kept alive by a surviving member converted to dangling
/// (cross-area links are the live cache's concern).
pub(super) fn delete_room(area: &mut AreaWithDetails, area_id: AreaId, number: RoomNumber) {
    area.rooms.retain(|r| r.room_number != number);
    for room in &mut area.rooms {
        for exit in &mut room.exits {
            if exit.to_area_id == Some(area_id) && exit.to_room_number == Some(number) {
                exit.to_area_id = None;
                exit.to_room_number = None;
                exit.to_direction = None;
            }
        }
    }
    let survivors = exit_topologies(area, None);
    connection_lifecycle::repair_after_room_delete(number, &survivors, &mut area.connections);
}

/// Materializes an exit from its creation args as a member of `connection_id`
/// (resolved by the caller's attach), honoring a client-minted id and
/// minting one when absent (the server's v2 contract).
pub(crate) fn exit_from_args(exit_data: ExitArgs, connection_id: ConnectionId) -> Exit {
    Exit {
        id: exit_data.id.unwrap_or_else(|| ExitId(Uuid::new_v4())),
        from_direction: exit_data.from_direction,
        to_area_id: exit_data.to_area_id,
        to_room_number: exit_data.to_room_number,
        to_direction: exit_data.to_direction,
        path: exit_data.path.unwrap_or_default(),
        is_hidden: exit_data.is_hidden,
        is_closed: exit_data.is_closed,
        is_locked: exit_data.is_locked,
        weight: exit_data.weight,
        command: exit_data.command.unwrap_or_default(),
        connection_id,
        to_unknown: false,
        to_area_token: None,
        is_secret: exit_data.is_secret.unwrap_or(false),
    }
}

/// Projects one stored exit into its connection-relevant topology.
fn exit_topology(area_id: AreaId, from_room: RoomNumber, exit: &Exit) -> ExitTopology {
    let same_area = exit.to_area_id == Some(area_id);
    ExitTopology {
        id: exit.id,
        connection_id: exit.connection_id,
        from_room,
        from_direction: exit.from_direction,
        to_room_in_area: if same_area { exit.to_room_number } else { None },
        to_direction: exit.to_direction,
        leaves_area: exit.to_unknown || (!same_area && exit.to_area_id.is_some()),
    }
}

/// Every exit's topology in the document, optionally excluding one (the
/// exit being edited or deleted).
fn exit_topologies(area: &AreaWithDetails, exclude: Option<ExitId>) -> Vec<ExitTopology> {
    let area_id = area.area.id;
    area.rooms
        .iter()
        .flat_map(|room| {
            room.exits
                .iter()
                .filter(|exit| Some(exit.id) != exclude)
                .map(move |exit| exit_topology(area_id, room.room_number, exit))
        })
        .collect()
}

/// A room-placement lookup over the document, for anchor bearings and
/// level classification.
fn room_site(area: &AreaWithDetails) -> impl Fn(RoomNumber) -> Option<RoomSite> + '_ {
    |number| {
        area.rooms
            .iter()
            .find(|room| room.room_number == number)
            .map(|room| RoomSite {
                x: room.x,
                y: room.y,
                level: room.level,
            })
    }
}

/// Materializes `number` as a blank placeholder room when absent.
fn ensure_room(area: &mut AreaWithDetails, number: RoomNumber) {
    if !area.rooms.iter().any(|r| r.room_number == number) {
        area.rooms.push(blank_room(number));
    }
}

/// Creates an exit on a room, honoring a client-minted id and minting one
/// when absent (the server's v2 contract). The exit's Connection is
/// resolved here, mirroring the server's creation semantics: auto-pair the
/// unique reciprocal one-member candidate, else a fresh one-member
/// Connection with direction-default anchors.
///
/// Server parity: an absent from-room is materialized as a blank
/// placeholder (the server INSERTs it `ON CONFLICT DO NOTHING`), and a
/// same-area destination room is placeholder-created the way the server's
/// destination matrix does. A cross-area destination stays a stored
/// reference — a single-area applier cannot create rooms in a foreign
/// document; the live cache and the sync engine own cross-area healing.
pub(super) fn create_room_exit(
    area: &mut AreaWithDetails,
    room_key: &RoomKey,
    mut exit_data: ExitArgs,
) -> CloudResult<Exit> {
    ensure_room(area, room_key.room_number);
    if exit_data.to_area_id == Some(area.area.id)
        && let Some(to_room) = exit_data.to_room_number
    {
        ensure_room(area, to_room);
    }
    let exit_id = exit_data.id.unwrap_or_else(|| ExitId(Uuid::new_v4()));
    exit_data.id = Some(exit_id);
    let same_area = exit_data.to_area_id == Some(area.area.id);
    let topology = ExitTopology {
        id: exit_id,
        connection_id: ConnectionId::default(),
        from_room: room_key.room_number,
        from_direction: exit_data.from_direction,
        to_room_in_area: if same_area {
            exit_data.to_room_number
        } else {
            None
        },
        to_direction: exit_data.to_direction,
        leaves_area: !same_area && exit_data.to_area_id.is_some(),
    };
    let connection_id = if let Some(connection_id) = exit_data.connection_id {
        let member_count = exit_topologies(area, None)
            .iter()
            .filter(|exit| exit.connection_id == connection_id)
            .count();
        if member_count >= 2 || !area.connections.iter().any(|c| c.id == connection_id) {
            return Err(invalid_connection(if member_count >= 2 {
                "too_many_members"
            } else {
                "connection_not_found"
            }));
        }
        connection_id
    } else {
        let peers = exit_topologies(area, None);
        let mut connections = std::mem::take(&mut area.connections);
        let connection_id =
            connection_lifecycle::attach_exit(&topology, &peers, &mut connections, room_site(area));
        area.connections = connections;
        connection_id
    };

    let exit = exit_from_args(exit_data, connection_id);
    let room = area
        .rooms
        .iter_mut()
        .find(|r| r.room_number == room_key.room_number)
        .expect("the from-room was just materialized");
    room.exits.push(exit.clone());
    Ok(exit)
}

fn invalid_connection(reason: &str) -> CloudError {
    CloudError::InvalidConnection(reason.to_string())
}

fn room_level(details: &AreaWithDetails, number: RoomNumber) -> CloudResult<i32> {
    details
        .rooms
        .iter()
        .find(|room| room.room_number == number)
        .map(|room| room.level)
        .ok_or_else(|| invalid_connection("invalid_endpoint"))
}

fn provisional_kind(
    details: &AreaWithDetails,
    a: crate::ConnectionEndpoint,
    b: Option<crate::ConnectionEndpoint>,
) -> CloudResult<ConnectionKind> {
    let a_level = room_level(details, a.room_number)?;
    let Some(b) = b else {
        return Ok(ConnectionKind::Dangling);
    };
    let b_level = room_level(details, b.room_number)?;
    if a.room_number == b.room_number {
        Ok(ConnectionKind::SelfLoop)
    } else if a_level == b_level {
        Ok(ConnectionKind::Internal)
    } else {
        Ok(ConnectionKind::CrossLevel)
    }
}

fn canonicalize_connection(connection: &mut Connection) {
    let Some(endpoint_b) = connection.endpoint_b else {
        return;
    };
    let flip = if connection.endpoint_a.room_number == endpoint_b.room_number {
        (
            connection.endpoint_a.side as u8,
            connection.endpoint_a.port_offset,
        ) > (endpoint_b.side as u8, endpoint_b.port_offset)
    } else {
        connection.endpoint_a.room_number > endpoint_b.room_number
    };
    if flip {
        connection.endpoint_b = Some(connection.endpoint_a);
        connection.endpoint_a = endpoint_b;
        connection.route_points.reverse();
    }
}

fn normalize_connection(connection: &mut Connection) -> CloudResult<()> {
    if connection.route_points.len() > MAX_ROUTE_POINTS {
        return Err(invalid_connection("too_many_points"));
    }
    for endpoint in [Some(connection.endpoint_a), connection.endpoint_b]
        .into_iter()
        .flatten()
    {
        if !endpoint.port_offset.is_finite() || !(0.0..=1.0).contains(&endpoint.port_offset) {
            return Err(invalid_connection("invalid_endpoint"));
        }
    }
    if !connection.thickness.is_finite() || !THICKNESS_RANGE.contains(&connection.thickness) {
        return Err(invalid_connection("invalid_thickness"));
    }
    if connection.color.trim().is_empty() {
        connection.color = DEFAULT_CONNECTION_COLOR.to_string();
    } else {
        if connection.color.len() > MAX_COLOR_LEN {
            return Err(invalid_connection("invalid_color"));
        }
        connection.color = canonicalize_css_color(&connection.color)
            .ok_or_else(|| invalid_connection("invalid_color"))?;
    }
    let mut previous = None;
    for point in &connection.route_points {
        if !point.x.is_finite()
            || !point.y.is_finite()
            || point.x.abs() > MAX_COORDINATE
            || point.y.abs() > MAX_COORDINATE
        {
            return Err(invalid_connection("invalid_point"));
        }
        if previous == Some(*point) {
            return Err(invalid_connection("duplicate_point"));
        }
        previous = Some(*point);
    }
    canonicalize_connection(connection);
    Ok(())
}

fn connection_from_args(
    details: &AreaWithDetails,
    args: &ConnectionArgs,
) -> CloudResult<Connection> {
    let mut connection = Connection {
        id: args.id,
        endpoint_a: args.endpoint_a,
        endpoint_b: args.endpoint_b,
        kind: provisional_kind(details, args.endpoint_a, args.endpoint_b)?,
        routing: args.routing,
        segment_shape: args.segment_shape,
        corner: args.corner,
        route_points: args.route_points.clone(),
        dash: args.dash,
        color: args.color.clone(),
        thickness: args.thickness,
    };
    normalize_connection(&mut connection)?;
    if !connection.kind.allows_routing(connection.routing) {
        return Err(invalid_connection("invalid_routing"));
    }
    Ok(connection)
}

fn connection_members(details: &AreaWithDetails, id: ConnectionId) -> Vec<ExitTopology> {
    exit_topologies(details, None)
        .into_iter()
        .filter(|exit| exit.connection_id == id)
        .collect()
}

fn members_are_reciprocal(a: &ExitTopology, b: &ExitTopology) -> bool {
    a.from_room != b.from_room
        && a.to_room_in_area == Some(b.from_room)
        && b.to_room_in_area == Some(a.from_room)
        && a.to_direction
            .is_none_or(|direction| direction == b.from_direction)
        && b.to_direction
            .is_none_or(|direction| direction == a.from_direction)
}

fn member_matches_endpoints(
    details: &AreaWithDetails,
    connection: &Connection,
    member: &ExitTopology,
) -> bool {
    let endpoint_a = connection.endpoint_a.room_number;
    match connection.endpoint_b {
        Some(endpoint_b) if endpoint_b.room_number == endpoint_a => {
            !member.leaves_area
                && member.from_room == endpoint_a
                && member.to_room_in_area == Some(endpoint_a)
        }
        Some(endpoint_b) => {
            let expected_destination = if member.from_room == endpoint_a {
                Some(endpoint_b.room_number)
            } else if member.from_room == endpoint_b.room_number {
                Some(endpoint_a)
            } else {
                None
            };
            !member.leaves_area
                && expected_destination.is_some()
                && member.to_room_in_area == expected_destination
        }
        None => {
            let exit = details
                .rooms
                .iter()
                .flat_map(|room| &room.exits)
                .find(|exit| exit.id == member.id)
                .expect("member topology came from a stored exit");
            let dangling =
                !exit.to_unknown && exit.to_area_id.is_none() && exit.to_room_number.is_none();
            let known_external = !exit.to_unknown
                && exit
                    .to_area_id
                    .is_some_and(|area_id| area_id != details.area.id)
                && exit.to_room_number.is_some();
            let redacted_external =
                exit.to_unknown && exit.to_area_id.is_none() && exit.to_room_number.is_none();
            member.from_room == endpoint_a && (dangling || known_external || redacted_external)
        }
    }
}

fn refresh_kind(details: &AreaWithDetails, connection: &Connection) -> CloudResult<ConnectionKind> {
    let members = connection_members(details, connection.id);
    let kind = match connection.endpoint_b {
        None => {
            if members.iter().any(|member| member.leaves_area) {
                ConnectionKind::External
            } else {
                ConnectionKind::Dangling
            }
        }
        Some(endpoint_b) if endpoint_b.room_number == connection.endpoint_a.room_number => {
            ConnectionKind::SelfLoop
        }
        Some(endpoint_b) => {
            if room_level(details, endpoint_b.room_number)?
                == room_level(details, connection.endpoint_a.room_number)?
            {
                ConnectionKind::Internal
            } else {
                ConnectionKind::CrossLevel
            }
        }
    };
    Ok(kind)
}

fn validate_connection_graph(details: &mut AreaWithDetails) -> CloudResult<()> {
    let ids: std::collections::HashSet<_> = details.connections.iter().map(|c| c.id).collect();
    if ids.len() != details.connections.len() {
        return Err(invalid_connection("duplicate_connection"));
    }
    for room in &details.rooms {
        for exit in &room.exits {
            if !ids.contains(&exit.connection_id) {
                return Err(invalid_connection("connection_not_found"));
            }
        }
    }

    for index in 0..details.connections.len() {
        let mut connection = details.connections[index].clone();
        let members = connection_members(details, connection.id);
        if !(1..=2).contains(&members.len()) {
            return Err(invalid_connection(if members.is_empty() {
                "no_members"
            } else {
                "too_many_members"
            }));
        }
        connection.kind = refresh_kind(details, &connection)?;
        normalize_connection(&mut connection)?;
        if !connection.kind.allows_routing(connection.routing) {
            return Err(invalid_connection("invalid_routing"));
        }
        if members.len() == 2
            && (!matches!(
                connection.kind,
                ConnectionKind::Internal | ConnectionKind::CrossLevel
            ) || !members_are_reciprocal(&members[0], &members[1]))
        {
            return Err(invalid_connection("invalid_membership"));
        }
        for member in &members {
            if !member_matches_endpoints(details, &connection, member) {
                return Err(invalid_connection("invalid_endpoint"));
            }
        }
        if connection.segment_shape == SegmentShape::Orthogonal
            && matches!(
                connection.routing,
                ConnectionRouting::Manual | ConnectionRouting::Automatic
            )
        {
            let room_a = details
                .rooms
                .iter()
                .find(|room| room.room_number == connection.endpoint_a.room_number)
                .expect("endpoint validated");
            let endpoint_b =
                connection
                    .endpoint_b
                    .zip(connection.endpoint_b.and_then(|endpoint| {
                        details
                            .rooms
                            .iter()
                            .find(|room| room.room_number == endpoint.room_number)
                    }));
            let geometry = connection_geometry::resolve(&connection_geometry::GeometryInput {
                kind: connection.kind,
                routing: connection.routing,
                corner: connection.corner,
                endpoint_a: connection_geometry::EndpointGeometry {
                    room_center: MapPoint::new(room_a.x, room_a.y),
                    side: connection.endpoint_a.side,
                    port_offset: connection.endpoint_a.port_offset,
                },
                endpoint_b: endpoint_b.map(|(endpoint, room)| {
                    connection_geometry::EndpointGeometry {
                        room_center: MapPoint::new(room.x, room.y),
                        side: endpoint.side,
                        port_offset: endpoint.port_offset,
                    }
                }),
                route_points: &connection.route_points,
                thickness: connection.thickness,
            });
            if geometry.centerline.windows(2).any(|segment| {
                let dx = (segment[0].x - segment[1].x).abs();
                let dy = (segment[0].y - segment[1].y).abs();
                dx > f32::EPSILON && dy > f32::EPSILON
            }) {
                return Err(invalid_connection("non_orthogonal"));
            }
        }
        details.connections[index] = connection;
    }
    details.connections.sort_by_key(|connection| connection.id);
    Ok(())
}

fn endpoint_tip(
    details: &AreaWithDetails,
    endpoint: crate::ConnectionEndpoint,
) -> Option<MapPoint> {
    let room = details
        .rooms
        .iter()
        .find(|room| room.room_number == endpoint.room_number)?;
    let port = crate::connection_geometry::port_position(
        MapPoint::new(room.x, room.y),
        endpoint.side,
        endpoint.port_offset,
    );
    Some(crate::connection_geometry::stub_tip(port, endpoint.side))
}

/// Preserves stored geometry across a compound room move. A shared delta
/// translates every stored point; otherwise Orthogonal routed endpoints
/// repair only their adjacent legs and insert the minimum endpoint elbow
/// when a fixed interior vertex can no longer meet the moved stub tip.
#[allow(clippy::too_many_lines)]
fn maintain_routes_after_room_moves(before: &AreaWithDetails, after: &mut AreaWithDetails) {
    for index in 0..after.connections.len() {
        let mut connection = after.connections[index].clone();
        let Some(endpoint_b) = connection.endpoint_b else {
            continue;
        };
        let Some(old_a) = before
            .rooms
            .iter()
            .find(|room| room.room_number == connection.endpoint_a.room_number)
        else {
            continue;
        };
        let Some(new_a) = after
            .rooms
            .iter()
            .find(|room| room.room_number == connection.endpoint_a.room_number)
        else {
            continue;
        };
        let Some(old_b) = before
            .rooms
            .iter()
            .find(|room| room.room_number == endpoint_b.room_number)
        else {
            continue;
        };
        let Some(new_b) = after
            .rooms
            .iter()
            .find(|room| room.room_number == endpoint_b.room_number)
        else {
            continue;
        };
        let delta_a = MapPoint::new(new_a.x - old_a.x, new_a.y - old_a.y);
        let delta_b = MapPoint::new(new_b.x - old_b.x, new_b.y - old_b.y);
        if delta_a == MapPoint::default() && delta_b == MapPoint::default() {
            continue;
        }
        if delta_a == delta_b {
            for point in &mut connection.route_points {
                *point = *point + delta_a;
            }
            after.connections[index] = connection;
            continue;
        }
        if connection.segment_shape != SegmentShape::Orthogonal
            || !matches!(
                connection.routing,
                ConnectionRouting::Manual | ConnectionRouting::Automatic
            )
        {
            continue;
        }
        let Some(old_tip_a) = endpoint_tip(before, connection.endpoint_a) else {
            continue;
        };
        let Some(old_tip_b) = endpoint_tip(before, endpoint_b) else {
            continue;
        };
        let Some(new_tip_a) = endpoint_tip(after, connection.endpoint_a) else {
            continue;
        };
        let Some(new_tip_b) = endpoint_tip(after, endpoint_b) else {
            continue;
        };
        if connection.route_points.is_empty() {
            if (new_tip_a.x - new_tip_b.x).abs() > f32::EPSILON
                && (new_tip_a.y - new_tip_b.y).abs() > f32::EPSILON
            {
                connection
                    .route_points
                    .push(MapPoint::new(new_tip_b.x, new_tip_a.y));
            }
        } else {
            let old_first = connection.route_points[0];
            if (old_first.y - old_tip_a.y).abs() <= f32::EPSILON {
                connection.route_points[0].y = new_tip_a.y;
            } else {
                connection.route_points[0].x = new_tip_a.x;
            }
            let last = connection.route_points.len() - 1;
            let old_last = connection.route_points[last];
            if (old_last.y - old_tip_b.y).abs() <= f32::EPSILON {
                connection.route_points[last].y = new_tip_b.y;
            } else {
                connection.route_points[last].x = new_tip_b.x;
            }
            let first = connection.route_points[0];
            if (first.x - new_tip_a.x).abs() > f32::EPSILON
                && (first.y - new_tip_a.y).abs() > f32::EPSILON
            {
                connection
                    .route_points
                    .insert(0, MapPoint::new(first.x, new_tip_a.y));
            }
            let last = *connection.route_points.last().expect("non-empty");
            if (last.x - new_tip_b.x).abs() > f32::EPSILON
                && (last.y - new_tip_b.y).abs() > f32::EPSILON
            {
                connection
                    .route_points
                    .push(MapPoint::new(new_tip_b.x, last.y));
            }
        }
        connection.route_points.dedup();
        after.connections[index] = connection;
    }
}

/// Materializes a label from its creation args, honoring a client-minted id
/// and minting one when absent.
pub(crate) fn label_from_args(args: LabelArgs) -> Label {
    Label {
        id: args.id.unwrap_or_else(|| LabelId(Uuid::new_v4())),
        level: args.level,
        x: args.x,
        y: args.y,
        width: args.width,
        height: args.height,
        horizontal_alignment: args.horizontal_alignment,
        vertical_alignment: args.vertical_alignment,
        text: args.text,
        color: args.color,
        background_color: args.background_color.unwrap_or_default(),
        font_size: args.font_size,
        font_weight: args.font_weight,
        is_secret: args.is_secret.unwrap_or(false),
    }
}

/// Materializes a shape from its creation args, honoring a client-minted id
/// and minting one when absent.
pub(crate) fn shape_from_args(args: ShapeArgs) -> Shape {
    Shape {
        id: args.id.unwrap_or_else(|| ShapeId(Uuid::new_v4())),
        level: args.level,
        x: args.x,
        y: args.y,
        width: args.width,
        height: args.height,
        background_color: args.background_color,
        stroke_color: args.stroke_color,
        shape_type: args.shape_type,
        border_radius: args.border_radius,
        stroke_width: args.stroke_width.unwrap_or(1.0),
        is_secret: args.is_secret.unwrap_or(false),
    }
}

/// The room an operation addresses, or [`CloudError::RoomNotFound`].
fn room_mut(
    area: &mut AreaWithDetails,
    area_id: AreaId,
    number: RoomNumber,
) -> CloudResult<&mut RoomWithDetails> {
    area.rooms
        .iter_mut()
        .find(|r| r.room_number == number)
        .ok_or_else(|| CloudError::RoomNotFound(RoomKey::new(area_id, number)))
}

/// Applies one operation of a mutation envelope to the area document,
/// mirroring the server's applier: upserts COALESCE into existing fields,
/// creates honor client-minted ids, deletions of absent entities report the
/// entity-specific not-found error, and each echo carries the entity as
/// stored after the change.
#[allow(clippy::too_many_lines)] // one exhaustive dispatch over the op alphabet
pub(crate) fn apply_mutation(
    details: &mut AreaWithDetails,
    op: &AreaMutation,
) -> CloudResult<OpResult> {
    let area_id = details.area.id;
    match op {
        AreaMutation::UpsertRoom { room_number, body } => {
            let room = upsert_room_details(details, *room_number, body);
            Ok(OpResult::Room { room: room.clone() })
        }
        AreaMutation::DeleteRoom { room_number } => {
            if !details.rooms.iter().any(|r| r.room_number == *room_number) {
                return Err(CloudError::RoomNotFound(RoomKey::new(
                    area_id,
                    *room_number,
                )));
            }
            delete_room(details, area_id, *room_number);
            Ok(OpResult::RoomDeleted {
                room_number: *room_number,
            })
        }
        AreaMutation::UpsertRoomProperty {
            room_number,
            name,
            value,
            is_secret,
        } => {
            let room = room_mut(details, area_id, *room_number)?;
            upsert_property(&mut room.properties, name, value, *is_secret);
            Ok(OpResult::RoomProperty {
                room_number: *room_number,
                name: name.clone(),
            })
        }
        AreaMutation::DeleteRoomProperty { room_number, name } => {
            let room = room_mut(details, area_id, *room_number)?;
            let idx = room
                .properties
                .iter()
                .position(|p| p.name == *name)
                .ok_or_else(|| CloudError::PropertyNotFound {
                    entity_type: "room".to_string(),
                    entity_id: format!("{area_id}:{room_number}"),
                    property_name: name.clone(),
                })?;
            room.properties.remove(idx);
            Ok(OpResult::RoomPropertyDeleted {
                room_number: *room_number,
                name: name.clone(),
            })
        }
        AreaMutation::AddRoomTag { room_number, tag } => {
            let room = room_mut(details, area_id, *room_number)?;
            room.tags.insert(tag.clone());
            Ok(OpResult::RoomTag {
                room_number: *room_number,
                tag: tag.clone(),
            })
        }
        AreaMutation::RemoveRoomTag { room_number, tag } => {
            // Removing an absent tag succeeds, like the server's DELETE.
            let room = room_mut(details, area_id, *room_number)?;
            room.tags.remove(tag);
            Ok(OpResult::RoomTagRemoved {
                room_number: *room_number,
                tag: tag.clone(),
            })
        }
        AreaMutation::UpsertAreaProperty {
            name,
            value,
            is_secret,
        } => {
            upsert_property(&mut details.properties, name, value, *is_secret);
            Ok(OpResult::AreaProperty { name: name.clone() })
        }
        AreaMutation::DeleteAreaProperty { name } => {
            let idx = details
                .properties
                .iter()
                .position(|p| p.name == *name)
                .ok_or_else(|| CloudError::PropertyNotFound {
                    entity_type: "area".to_string(),
                    entity_id: area_id.to_string(),
                    property_name: name.clone(),
                })?;
            details.properties.remove(idx);
            Ok(OpResult::AreaPropertyDeleted { name: name.clone() })
        }
        AreaMutation::CreateExit { room_number, body } => {
            let key = RoomKey::new(area_id, *room_number);
            let exit = create_room_exit(details, &key, body.clone())?;
            Ok(OpResult::Exit { exit })
        }
        AreaMutation::UpdateExit { exit_id, body } => {
            let (from_room, before) = details
                .rooms
                .iter()
                .find_map(|room| {
                    room.exits
                        .iter()
                        .find(|exit| exit.id == *exit_id)
                        .map(|exit| {
                            (
                                room.room_number,
                                exit_topology(area_id, room.room_number, exit),
                            )
                        })
                })
                .ok_or(CloudError::ExitNotFound(*exit_id))?;
            let updated = {
                let exit = details
                    .rooms
                    .iter_mut()
                    .flat_map(|room| room.exits.iter_mut())
                    .find(|exit| exit.id == *exit_id)
                    .expect("located above");
                apply_exit_updates(exit, body.clone());
                exit.clone()
            };
            // §3.2 (local mirror): a destination/direction change re-pairs,
            // re-kinds, or re-anchors the exit's Connection.
            let after = exit_topology(area_id, from_room, &updated);
            let mut echo = updated;
            if connection_lifecycle::topology_differs(&before, &after) {
                let peers = exit_topologies(details, Some(*exit_id));
                if peers
                    .iter()
                    .any(|peer| peer.connection_id == before.connection_id)
                {
                    return Err(CloudError::StructuralConflict(
                        "unlink_before_edit".to_string(),
                    ));
                }
                let mut connections = std::mem::take(&mut details.connections);
                let connection_id = connection_lifecycle::reattach_after_update(
                    &before,
                    &after,
                    &peers,
                    &mut connections,
                    room_site(details),
                );
                details.connections = connections;
                if connection_id != echo.connection_id {
                    echo.connection_id = connection_id;
                    let exit = details
                        .rooms
                        .iter_mut()
                        .flat_map(|room| room.exits.iter_mut())
                        .find(|exit| exit.id == *exit_id)
                        .expect("located above");
                    exit.connection_id = connection_id;
                }
            }
            Ok(OpResult::Exit { exit: echo })
        }
        AreaMutation::DeleteExit { exit_id } => {
            let removed_connection = {
                let room = details
                    .rooms
                    .iter_mut()
                    .find(|room| room.exits.iter().any(|exit| exit.id == *exit_id))
                    .ok_or(CloudError::ExitNotFound(*exit_id))?;
                let connection_id = room
                    .exits
                    .iter()
                    .find(|exit| exit.id == *exit_id)
                    .map(|exit| exit.connection_id);
                room.exits.retain(|exit| exit.id != *exit_id);
                connection_id
            };
            // Deleting the last member exit deletes the Connection.
            if let Some(connection_id) = removed_connection {
                let survivors = exit_topologies(details, None);
                connection_lifecycle::remove_orphan_connection(
                    connection_id,
                    &survivors,
                    &mut details.connections,
                );
            }
            Ok(OpResult::ExitDeleted { exit_id: *exit_id })
        }
        AreaMutation::CreateConnection { body } => {
            if details
                .connections
                .iter()
                .any(|connection| connection.id == body.id)
            {
                return Err(invalid_connection("duplicate_connection"));
            }
            let connection = connection_from_args(details, body)?;
            details.connections.push(connection.clone());
            Ok(OpResult::Connection { connection })
        }
        AreaMutation::UpdateConnection {
            connection_id,
            body,
        } => {
            let current = details
                .connections
                .iter()
                .find(|connection| connection.id == *connection_id)
                .cloned()
                .ok_or_else(|| invalid_connection("connection_not_found"))?;
            let mut updated = body.clone().apply(&current);
            if updated.endpoint_a.room_number != current.endpoint_a.room_number
                || updated.endpoint_b.map(|endpoint| endpoint.room_number)
                    != current.endpoint_b.map(|endpoint| endpoint.room_number)
            {
                return Err(invalid_connection("endpoint_room_immutable"));
            }
            normalize_connection(&mut updated)?;
            if !updated.kind.allows_routing(updated.routing) {
                return Err(invalid_connection("invalid_routing"));
            }
            let stored = details
                .connections
                .iter_mut()
                .find(|connection| connection.id == *connection_id)
                .expect("located above");
            *stored = updated.clone();
            Ok(OpResult::Connection {
                connection: updated,
            })
        }
        AreaMutation::Pair {
            keep_connection_id,
            merge_connection_id,
        } => {
            if keep_connection_id == merge_connection_id {
                return Err(invalid_connection("same_connection"));
            }
            let keep_members = connection_members(details, *keep_connection_id);
            let merge_members = connection_members(details, *merge_connection_id);
            let ([keep], [merge]) = (keep_members.as_slice(), merge_members.as_slice()) else {
                return Err(invalid_connection("pair_requires_one_member"));
            };
            if !members_are_reciprocal(keep, merge) {
                return Err(invalid_connection("not_reciprocal"));
            }
            if !details
                .connections
                .iter()
                .any(|connection| connection.id == *keep_connection_id)
                || !details
                    .connections
                    .iter()
                    .any(|connection| connection.id == *merge_connection_id)
            {
                return Err(invalid_connection("connection_not_found"));
            }
            for room in &mut details.rooms {
                for exit in &mut room.exits {
                    if exit.connection_id == *merge_connection_id {
                        exit.connection_id = *keep_connection_id;
                    }
                }
            }
            details
                .connections
                .retain(|connection| connection.id != *merge_connection_id);
            let connection = details
                .connections
                .iter()
                .find(|connection| connection.id == *keep_connection_id)
                .cloned()
                .expect("validated above");
            Ok(OpResult::Connection { connection })
        }
        AreaMutation::Unlink {
            exit_id,
            new_connection_id,
        } => {
            if details
                .connections
                .iter()
                .any(|connection| connection.id == *new_connection_id)
            {
                return Err(invalid_connection("duplicate_connection"));
            }
            let (from_room, old_connection_id) = details
                .rooms
                .iter()
                .find_map(|room| {
                    room.exits
                        .iter()
                        .find(|exit| exit.id == *exit_id)
                        .map(|exit| (room.room_number, exit.connection_id))
                })
                .ok_or(CloudError::ExitNotFound(*exit_id))?;
            if connection_members(details, old_connection_id).len() != 2 {
                return Err(invalid_connection("unlink_requires_pair"));
            }
            let mut cloned = details
                .connections
                .iter()
                .find(|connection| connection.id == old_connection_id)
                .cloned()
                .ok_or_else(|| invalid_connection("connection_not_found"))?;
            cloned.id = *new_connection_id;
            // Give the split line a nearby stored port without moving the
            // original. The server may choose a denser authoritative slot;
            // this deterministic local result keeps the two lines operable.
            let offset = |value: f32| {
                if value <= 0.9 {
                    value + 0.05
                } else {
                    value - 0.05
                }
            };
            if cloned.endpoint_a.room_number == from_room {
                cloned.endpoint_a.port_offset = offset(cloned.endpoint_a.port_offset);
                cloned.endpoint_a.port_mode = crate::PortMode::AutoPinned;
            } else if let Some(endpoint) = cloned.endpoint_b.as_mut()
                && endpoint.room_number == from_room
            {
                endpoint.port_offset = offset(endpoint.port_offset);
                endpoint.port_mode = crate::PortMode::AutoPinned;
            }
            for room in &mut details.rooms {
                if let Some(exit) = room.exits.iter_mut().find(|exit| exit.id == *exit_id) {
                    exit.connection_id = *new_connection_id;
                }
            }
            details.connections.push(cloned.clone());
            let old = details
                .connections
                .iter()
                .find(|connection| connection.id == old_connection_id)
                .cloned()
                .expect("source exists");
            Ok(OpResult::Connections {
                connections: vec![old, cloned],
            })
        }
        AreaMutation::DeleteLink { connection_id } => {
            if !details
                .connections
                .iter()
                .any(|connection| connection.id == *connection_id)
            {
                return Err(invalid_connection("connection_not_found"));
            }
            for room in &mut details.rooms {
                room.exits
                    .retain(|exit| exit.connection_id != *connection_id);
            }
            details
                .connections
                .retain(|connection| connection.id != *connection_id);
            Ok(OpResult::ConnectionDeleted {
                connection_id: *connection_id,
            })
        }
        AreaMutation::CreateLabel { body } => {
            let label = label_from_args(body.clone());
            details.labels.push(label.clone());
            Ok(OpResult::Label { label })
        }
        AreaMutation::UpdateLabel { label_id, body } => {
            let label = details
                .labels
                .iter_mut()
                .find(|label| label.id == *label_id)
                .ok_or(CloudError::LabelNotFound(*label_id))?;
            *label = body.clone().apply(label);
            Ok(OpResult::Label {
                label: label.clone(),
            })
        }
        AreaMutation::DeleteLabel { label_id } => {
            let idx = details
                .labels
                .iter()
                .position(|label| label.id == *label_id)
                .ok_or(CloudError::LabelNotFound(*label_id))?;
            details.labels.remove(idx);
            Ok(OpResult::LabelDeleted {
                label_id: *label_id,
            })
        }
        AreaMutation::CreateShape { body } => {
            let shape = shape_from_args(body.clone());
            details.shapes.push(shape.clone());
            Ok(OpResult::Shape { shape })
        }
        AreaMutation::UpdateShape { shape_id, body } => {
            let shape = details
                .shapes
                .iter_mut()
                .find(|shape| shape.id == *shape_id)
                .ok_or(CloudError::ShapeNotFound(*shape_id))?;
            *shape = body.clone().apply(shape);
            Ok(OpResult::Shape {
                shape: shape.clone(),
            })
        }
        AreaMutation::DeleteShape { shape_id } => {
            let idx = details
                .shapes
                .iter()
                .position(|shape| shape.id == *shape_id)
                .ok_or(CloudError::ShapeNotFound(*shape_id))?;
            details.shapes.remove(idx);
            Ok(OpResult::ShapeDeleted {
                shape_id: *shape_id,
            })
        }
    }
}

/// The single-precondition check local tiers enforce: an envelope is
/// conditioned on exactly its own area, at the document's current revision.
/// Access fingerprints are ignored — local areas have no access control.
fn validate_preconditions(
    details: &AreaWithDetails,
    area_id: AreaId,
    preconditions: &[Precondition],
) -> CloudResult<()> {
    let [precondition] = preconditions else {
        return Err(CloudError::InvalidInput(
            "a mutation envelope must carry exactly one precondition".to_string(),
        ));
    };
    if precondition.resource != ResourceKind::Area || precondition.id != area_id.0 {
        return Err(CloudError::InvalidInput(
            "a mutation envelope's precondition must name the addressed area".to_string(),
        ));
    }
    if precondition.expected_rev != details.area.rev {
        return Err(CloudError::RevisionConflict {
            id: area_id.0,
            expected_rev: precondition.expected_rev,
            current_rev: details.area.rev,
        });
    }
    Ok(())
}

/// Applies a whole mutation envelope to the area document: precondition
/// check, every operation in order, then exactly one revision bump. Any
/// error leaves an unspecified partially-applied document — the caller must
/// discard it (local reloads from disk, ephemeral applies to a working
/// clone), so the stored area only ever moves atomically.
pub(crate) fn apply_envelope(
    details: &mut AreaWithDetails,
    area_id: AreaId,
    envelope: &MutationEnvelope,
) -> CloudResult<MutationResult> {
    validate_preconditions(details, area_id, &envelope.preconditions)?;
    let before = details.clone();
    let mut data = Vec::with_capacity(envelope.payload.len());
    for op in &envelope.payload {
        data.push(apply_mutation(details, op)?);
    }
    maintain_routes_after_room_moves(&before, details);
    validate_connection_graph(details)?;
    details.area.rev += 1;
    Ok(MutationResult {
        operation_id: envelope.operation_id,
        versions: vec![VersionInfo {
            resource: ResourceKind::Area,
            id: area_id.0,
            rev: details.area.rev,
            deleted: false,
        }],
        data,
    })
}
