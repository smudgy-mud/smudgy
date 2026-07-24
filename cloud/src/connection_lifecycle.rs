//! Local-tier Connection lifecycle: the simplified creation/repair rules the
//! client applies wherever it edits an area without a server round-trip —
//! the [`crate::backends`] document applier and the mapper's optimistic area
//! cache. Both representations project their exits through [`ExitTopology`]
//! and share these functions, so the stored-membership invariants (every
//! exit belongs to exactly one Connection; a Connection has one or two
//! member exits; orphan Connections never persist) cannot drift between the
//! two.
//!
//! The rules deliberately mirror the server's, simplified for a single-user
//! tier: auto-pair only the unique direction-compatible reciprocal
//! one-member candidate; otherwise a fresh one-member Connection with the
//! direction-default anchors. Bearing-slotted port distribution is a server
//! concern — local tiers use the plain defaults.

use crate::{
    Connection, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionKind,
    ConnectionRouting, CornerStyle, DEFAULT_CONNECTION_COLOR, DEFAULT_CONNECTION_THICKNESS,
    ExitDirection, ExitId, MapPoint, PortMode, RoomNumber, RoomSide, SegmentShape,
    connection::{default_anchor_for_direction, side_nearest_bearing},
};

/// One exit's connection-relevant topology, projected out of whichever
/// representation the caller maintains (`Exit` in an area document,
/// `ExitCache` in the mapper's cache).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ExitTopology {
    pub id: ExitId,
    pub connection_id: ConnectionId,
    pub from_room: RoomNumber,
    pub from_direction: ExitDirection,
    /// Destination room when it stays in this area.
    pub to_room_in_area: Option<RoomNumber>,
    /// The arrival wall/direction at the destination, when known.
    pub to_direction: Option<ExitDirection>,
    /// The destination is outside this area (another area, or hidden behind
    /// `to_unknown`).
    pub leaves_area: bool,
}

/// A room's placement, for anchor bearings and level classification.
#[derive(Debug, Clone, Copy)]
pub struct RoomSite {
    pub x: f32,
    pub y: f32,
    pub level: i32,
}

/// Whether an update to these fields must run [`reattach_after_update`]:
/// destination and direction changes can re-pair, re-kind, or re-anchor the
/// exit's Connection; every other exit field is connection-neutral.
#[must_use]
pub fn topology_differs(before: &ExitTopology, after: &ExitTopology) -> bool {
    (
        before.to_room_in_area,
        before.to_direction,
        before.leaves_area,
        before.from_direction,
    ) != (
        after.to_room_in_area,
        after.to_direction,
        after.leaves_area,
        after.from_direction,
    )
}

/// §3.1 (simplified): resolves the Connection a newly created exit becomes a
/// member of. Auto-pairs onto the unique reciprocal one-member candidate
/// whose explicit directions do not contradict the new exit's; otherwise
/// pushes a fresh one-member Connection with direction-default anchors.
/// `peers` is every *other* exit in the area.
pub fn attach_exit(
    exit: &ExitTopology,
    peers: &[ExitTopology],
    connections: &mut Vec<Connection>,
    room_site: impl Fn(RoomNumber) -> Option<RoomSite>,
) -> ConnectionId {
    if let Some(candidate) = reciprocal_candidate(exit, peers)
        && connections.iter().any(|c| c.id == candidate)
    {
        return candidate;
    }
    let connection = default_connection_for(exit, &room_site);
    let id = connection.id;
    connections.push(connection);
    id
}

/// The unique auto-pair candidate for `exit`, if any: a one-member
/// Connection whose single member runs the reciprocal way between the same
/// two rooms and whose explicit directions do not contradict the new
/// exit's. Self-loops never pair.
fn reciprocal_candidate(exit: &ExitTopology, peers: &[ExitTopology]) -> Option<ConnectionId> {
    let to_room = exit.to_room_in_area?;
    if to_room == exit.from_room {
        return None;
    }
    let one_member = |peer: &ExitTopology| {
        peers
            .iter()
            .filter(|other| other.connection_id == peer.connection_id)
            .count()
            == 1
    };
    let mut candidates = peers.iter().filter(|peer| {
        peer.from_room == to_room
            && peer.to_room_in_area == Some(exit.from_room)
            && directions_compatible(exit, peer)
            && one_member(peer)
    });
    let first = candidates.next()?;
    if candidates.next().is_some() {
        // Ambiguity creates a separate one-way Connection.
        return None;
    }
    Some(first.connection_id)
}

/// The §3.1 direction-compatibility check between a new exit and a
/// reciprocal candidate: an explicit arrival direction must not contradict
/// the other exit's origin direction; an absent one contradicts nothing.
fn directions_compatible(exit: &ExitTopology, candidate: &ExitTopology) -> bool {
    exit.to_direction
        .is_none_or(|dir| dir == candidate.from_direction)
        && candidate
            .to_direction
            .is_none_or(|dir| dir == exit.from_direction)
}

/// A fresh one-member Connection for `exit`, kinded from its visible
/// topology, with §1.5 direction-default anchors and canonical endpoint
/// order (lower room number first; self-loops by side ordinal, then offset,
/// then origin role).
#[must_use]
pub fn default_connection_for(
    exit: &ExitTopology,
    room_site: impl Fn(RoomNumber) -> Option<RoomSite>,
) -> Connection {
    let origin_site = room_site(exit.from_room);
    match exit.to_room_in_area {
        Some(to_room) if to_room == exit.from_room => {
            let (side_a, offset_a) = default_anchor_for_direction(exit.from_direction, None);
            let arrival = exit.to_direction.unwrap_or(exit.from_direction);
            let (side_b, offset_b) = default_anchor_for_direction(arrival, None);
            let origin = endpoint(exit.from_room, side_a, offset_a);
            let destination = endpoint(exit.from_room, side_b, offset_b);
            // Canonical self-loop order: side ordinal, then offset, then the
            // origin role before the destination role.
            let flipped = (side_ordinal(side_b), offset_b) < (side_ordinal(side_a), offset_a);
            let (a, b) = if flipped {
                (destination, origin)
            } else {
                (origin, destination)
            };
            blank_connection(a, Some(b), ConnectionKind::SelfLoop)
        }
        Some(to_room) => {
            let destination_site = room_site(to_room);
            let bearing_ab = bearing_between(origin_site, destination_site);
            let bearing_ba = bearing_ab.map(|b| b.scale(-1.0));
            let (side_a, offset_a) = default_anchor_for_direction(exit.from_direction, bearing_ab);
            // Endpoint B defaults from the arrival direction, or the partner
            // bearing when it is absent (East fallback without either).
            let (side_b, offset_b) = match exit.to_direction {
                Some(direction) => default_anchor_for_direction(direction, bearing_ba),
                None => (bearing_ba.map_or(RoomSide::East, side_nearest_bearing), 0.5),
            };
            let kind = match (&origin_site, &destination_site) {
                (Some(origin), Some(destination)) if origin.level != destination.level => {
                    ConnectionKind::CrossLevel
                }
                _ => ConnectionKind::Internal,
            };
            let origin_endpoint = endpoint(exit.from_room, side_a, offset_a);
            let destination_endpoint = endpoint(to_room, side_b, offset_b);
            // Canonical order: the lower room number is endpoint A.
            let (a, b) = if to_room < exit.from_room {
                (destination_endpoint, origin_endpoint)
            } else {
                (origin_endpoint, destination_endpoint)
            };
            blank_connection(a, Some(b), kind)
        }
        None => {
            let (side, offset) = default_anchor_for_direction(exit.from_direction, None);
            let kind = if exit.leaves_area {
                ConnectionKind::External
            } else {
                ConnectionKind::Dangling
            };
            blank_connection(endpoint(exit.from_room, side, offset), None, kind)
        }
    }
}

/// Drops `connection_id`'s row when no surviving exit references it — the
/// "deleting the last exit deletes the Connection" invariant. `survivors`
/// is every exit remaining after the removal.
pub fn remove_orphan_connection(
    connection_id: ConnectionId,
    survivors: &[ExitTopology],
    connections: &mut Vec<Connection>,
) {
    if !survivors
        .iter()
        .any(|exit| exit.connection_id == connection_id)
    {
        connections.retain(|connection| connection.id != connection_id);
    }
}

/// §3.3 room deletion repair, run after the room's outgoing exits are
/// removed and inbound destinations nulled: Connections that lost every
/// member are deleted; Connections touching the deleted room that keep a
/// surviving member become Dangling, anchored at the surviving origin with
/// endpoint B and the stored route cleared.
pub fn repair_after_room_delete(
    deleted: RoomNumber,
    survivors: &[ExitTopology],
    connections: &mut Vec<Connection>,
) {
    connections.retain_mut(|connection| {
        let touches = connection.endpoint_a.room_number == deleted
            || connection
                .endpoint_b
                .is_some_and(|b| b.room_number == deleted);
        let survivor = survivors
            .iter()
            .find(|exit| exit.connection_id == connection.id);
        let Some(survivor) = survivor else {
            // Orphaned by this deletion: remove. A memberless Connection
            // that never touched the room was already corrupt; leave it for
            // the projection's own corruption handling.
            return !touches;
        };
        if touches {
            make_dangling(connection, survivor);
        }
        true
    });
}

/// §3.2 (simplified) after an exit's topology fields changed: a pair that is
/// no longer reciprocal splits (the edited exit re-attaches fresh, which may
/// auto-pair elsewhere); a one-member Connection is updated in place —
/// endpoint B recomputed or cleared, kind re-derived, canonical order
/// restored with route reversal, and stored routes cleared when the kind can
/// no longer use them. Returns the exit's (possibly new) Connection id.
/// `peers` is every exit in the area except the edited one.
pub fn reattach_after_update(
    before: &ExitTopology,
    after: &ExitTopology,
    peers: &[ExitTopology],
    connections: &mut Vec<Connection>,
    room_site: impl Fn(RoomNumber) -> Option<RoomSite>,
) -> ConnectionId {
    let partner = peers
        .iter()
        .find(|peer| peer.connection_id == before.connection_id);
    if let Some(partner) = partner {
        // Two members: still reciprocal (destination and directions all
        // compatible) leaves everything alone; otherwise the edited exit
        // moves out — the local mirror of "unlink then edit".
        let reciprocal = after.to_room_in_area == Some(partner.from_room)
            && partner.to_room_in_area == Some(after.from_room)
            && directions_compatible(after, partner);
        if reciprocal {
            return before.connection_id;
        }
        return attach_exit(after, peers, connections, room_site);
    }

    let Some(connection) = connections
        .iter_mut()
        .find(|connection| connection.id == before.connection_id)
    else {
        // The membership row is missing (corrupt input): self-heal by
        // attaching fresh.
        return attach_exit(after, peers, connections, room_site);
    };

    retarget_in_place(connection, after, &room_site);
    connection.id
}

/// The in-place §3.2 update of a one-member Connection to its exit's new
/// topology.
fn retarget_in_place(
    connection: &mut Connection,
    after: &ExitTopology,
    room_site: &impl Fn(RoomNumber) -> Option<RoomSite>,
) {
    let origin = after.from_room;
    // The endpoint that stays: the one on the exit's origin room, rebuilt
    // from the direction default if the stored row lost it (corrupt input).
    let origin_endpoint = [Some(connection.endpoint_a), connection.endpoint_b]
        .into_iter()
        .flatten()
        .find(|endpoint| endpoint.room_number == origin)
        .unwrap_or_else(|| {
            let (side, offset) = default_anchor_for_direction(after.from_direction, None);
            endpoint(origin, side, offset)
        });
    // The old far side, when it was a different room (a former self-loop has
    // no meaningful "old destination"; its endpoint is recomputed below).
    let old_destination = [Some(connection.endpoint_a), connection.endpoint_b]
        .into_iter()
        .flatten()
        .find(|other| other.room_number != origin);

    match after.to_room_in_area {
        Some(to_room) if to_room == origin => {
            let arrival = after.to_direction.unwrap_or(after.from_direction);
            let (side_b, offset_b) = default_anchor_for_direction(arrival, None);
            let destination = endpoint(origin, side_b, offset_b);
            let flipped = (side_ordinal(destination.side), destination.port_offset)
                < (
                    side_ordinal(origin_endpoint.side),
                    origin_endpoint.port_offset,
                );
            let (a, b) = if flipped {
                (destination, origin_endpoint)
            } else {
                (origin_endpoint, destination)
            };
            connection.endpoint_a = a;
            connection.endpoint_b = Some(b);
            connection.kind = ConnectionKind::SelfLoop;
            clear_route_if_unusable(connection);
        }
        Some(to_room) => {
            // A destination endpoint on the same room survives untouched
            // (Manual pins included); anything else is recomputed as an
            // AutoPinned direction default.
            let destination = old_destination
                .filter(|endpoint| endpoint.room_number == to_room)
                .unwrap_or_else(|| {
                    let bearing = bearing_between(room_site(to_room), room_site(origin));
                    let (side, offset) = match after.to_direction {
                        Some(direction) => default_anchor_for_direction(direction, bearing),
                        None => (bearing.map_or(RoomSide::East, side_nearest_bearing), 0.5),
                    };
                    endpoint(to_room, side, offset)
                });
            connection.kind = match (room_site(origin), room_site(to_room)) {
                (Some(a), Some(b)) if a.level != b.level => ConnectionKind::CrossLevel,
                _ => ConnectionKind::Internal,
            };
            let stored_a_room = if origin < to_room { origin } else { to_room };
            let (a, b) = if origin_endpoint.room_number == stored_a_room {
                (origin_endpoint, destination)
            } else {
                (destination, origin_endpoint)
            };
            if connection.endpoint_a.room_number != a.room_number {
                // Canonical order flipped: the stored path must not visibly
                // flip with it.
                connection.route_points.reverse();
            }
            connection.endpoint_a = a;
            connection.endpoint_b = Some(b);
            clear_route_if_unusable(connection);
        }
        None => {
            connection.endpoint_a = origin_endpoint;
            connection.endpoint_b = None;
            connection.kind = if after.leaves_area {
                ConnectionKind::External
            } else {
                ConnectionKind::Dangling
            };
            connection.route_points.clear();
            clamp_routing(connection);
        }
    }
}

/// Converts a Connection whose far side vanished into a Dangling one
/// anchored at `survivor`'s origin endpoint.
fn make_dangling(connection: &mut Connection, survivor: &ExitTopology) {
    let keep = [Some(connection.endpoint_a), connection.endpoint_b]
        .into_iter()
        .flatten()
        .find(|endpoint| endpoint.room_number == survivor.from_room)
        .unwrap_or_else(|| {
            let (side, offset) = default_anchor_for_direction(survivor.from_direction, None);
            endpoint(survivor.from_room, side, offset)
        });
    connection.endpoint_a = keep;
    connection.endpoint_b = None;
    connection.route_points.clear();
    connection.kind = ConnectionKind::Dangling;
    clamp_routing(connection);
}

/// A special kind cannot use routed modes or stored points: clear them so no
/// orphan coordinate frame survives the transition.
fn clear_route_if_unusable(connection: &mut Connection) {
    if !connection.kind.allows_routing(connection.routing) {
        connection.route_points.clear();
        clamp_routing(connection);
    }
}

fn clamp_routing(connection: &mut Connection) {
    if !connection.kind.allows_routing(connection.routing) {
        connection.routing = ConnectionRouting::Simple;
    }
}

fn endpoint(room_number: RoomNumber, side: RoomSide, port_offset: f32) -> ConnectionEndpoint {
    ConnectionEndpoint {
        room_number,
        side,
        port_offset,
        port_mode: PortMode::AutoPinned,
    }
}

fn blank_connection(
    endpoint_a: ConnectionEndpoint,
    endpoint_b: Option<ConnectionEndpoint>,
    kind: ConnectionKind,
) -> Connection {
    Connection {
        id: ConnectionId::new(),
        endpoint_a,
        endpoint_b,
        kind,
        routing: ConnectionRouting::Simple,
        segment_shape: SegmentShape::Direct,
        corner: CornerStyle::Sharp,
        route_points: Vec::new(),
        dash: ConnectionDash::Solid,
        color: DEFAULT_CONNECTION_COLOR.to_string(),
        thickness: DEFAULT_CONNECTION_THICKNESS,
    }
}

fn bearing_between(from: Option<RoomSite>, to: Option<RoomSite>) -> Option<MapPoint> {
    match (from, to) {
        (Some(from), Some(to)) => Some(MapPoint::new(to.x - from.x, to.y - from.y)),
        _ => None,
    }
}

fn side_ordinal(side: RoomSide) -> usize {
    RoomSide::ALL
        .iter()
        .position(|candidate| *candidate == side)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn exit(
        id: u128,
        connection: ConnectionId,
        from_room: i32,
        from_direction: ExitDirection,
        to_room: Option<i32>,
        to_direction: Option<ExitDirection>,
    ) -> ExitTopology {
        ExitTopology {
            id: ExitId(Uuid::from_u128(id)),
            connection_id: connection,
            from_room: RoomNumber(from_room),
            from_direction,
            to_room_in_area: to_room.map(RoomNumber),
            to_direction,
            leaves_area: false,
        }
    }

    fn flat_site(_: RoomNumber) -> Option<RoomSite> {
        Some(RoomSite {
            x: 0.0,
            y: 0.0,
            level: 0,
        })
    }

    #[test]
    fn attach_pairs_the_unique_reciprocal_candidate() {
        let existing = ConnectionId::new();
        let peer = exit(
            1,
            existing,
            2,
            ExitDirection::West,
            Some(1),
            Some(ExitDirection::East),
        );
        let mut connections = vec![default_connection_for(&peer, flat_site)];
        connections[0].id = existing;

        let new_exit = exit(
            2,
            ConnectionId::default(),
            1,
            ExitDirection::East,
            Some(2),
            Some(ExitDirection::West),
        );
        let id = attach_exit(&new_exit, &[peer], &mut connections, flat_site);
        assert_eq!(id, existing);
        assert_eq!(connections.len(), 1);
    }

    #[test]
    fn attach_refuses_direction_contradictions_and_ambiguity() {
        let existing = ConnectionId::new();
        let peer = exit(
            1,
            existing,
            2,
            ExitDirection::West,
            Some(1),
            Some(ExitDirection::North), // contradicts the new exit's East
        );
        let mut connections = vec![default_connection_for(&peer, flat_site)];
        connections[0].id = existing;

        let new_exit = exit(
            2,
            ConnectionId::default(),
            1,
            ExitDirection::East,
            Some(2),
            Some(ExitDirection::West),
        );
        let id = attach_exit(&new_exit, &[peer], &mut connections, flat_site);
        assert_ne!(id, existing);
        assert_eq!(connections.len(), 2, "a fresh one-way Connection appears");
    }

    #[test]
    fn new_connections_are_canonically_ordered_and_kinded() {
        // Origin room 5 → destination room 2: room 2 must be endpoint A.
        let one_way = exit(
            1,
            ConnectionId::default(),
            5,
            ExitDirection::West,
            Some(2),
            Some(ExitDirection::East),
        );
        let connection = default_connection_for(&one_way, flat_site);
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(2));
        assert_eq!(connection.kind, ConnectionKind::Internal);

        let cross = default_connection_for(&one_way, |room| {
            Some(RoomSite {
                x: 0.0,
                y: 0.0,
                level: i32::from(room == RoomNumber(5)),
            })
        });
        assert_eq!(cross.kind, ConnectionKind::CrossLevel);

        let dangling = exit(
            2,
            ConnectionId::default(),
            1,
            ExitDirection::North,
            None,
            None,
        );
        assert_eq!(
            default_connection_for(&dangling, flat_site).kind,
            ConnectionKind::Dangling
        );

        let external = ExitTopology {
            leaves_area: true,
            ..dangling
        };
        assert_eq!(
            default_connection_for(&external, flat_site).kind,
            ConnectionKind::External
        );

        let self_loop = exit(
            3,
            ConnectionId::default(),
            1,
            ExitDirection::South,
            Some(1),
            Some(ExitDirection::North),
        );
        let looped = default_connection_for(&self_loop, flat_site);
        assert_eq!(looped.kind, ConnectionKind::SelfLoop);
        // North (ordinal 0) sorts before South: the destination role leads.
        assert_eq!(looped.endpoint_a.side, RoomSide::North);
    }

    #[test]
    fn room_delete_repair_converts_survivors_and_drops_orphans() {
        let deleted = RoomNumber(9);
        let survivor_conn = ConnectionId::new();
        let orphan_conn = ConnectionId::new();
        let survivor = ExitTopology {
            to_room_in_area: None, // destination already nulled by the cascade
            ..exit(1, survivor_conn, 1, ExitDirection::East, None, None)
        };
        let mut connections = vec![
            // 1 ↔ 9, survivor keeps it.
            Connection {
                id: survivor_conn,
                endpoint_b: Some(endpoint(deleted, RoomSide::West, 0.5)),
                routing: ConnectionRouting::Manual,
                route_points: vec![MapPoint::new(1.0, 1.0)],
                ..blank_connection(
                    endpoint(RoomNumber(1), RoomSide::East, 0.5),
                    None,
                    ConnectionKind::Internal,
                )
            },
            // 9's self-loop: every member died with the room.
            Connection {
                id: orphan_conn,
                ..blank_connection(
                    endpoint(deleted, RoomSide::North, 0.5),
                    Some(endpoint(deleted, RoomSide::North, 0.5)),
                    ConnectionKind::SelfLoop,
                )
            },
        ];

        repair_after_room_delete(deleted, &[survivor], &mut connections);
        assert_eq!(connections.len(), 1);
        let repaired = &connections[0];
        assert_eq!(repaired.kind, ConnectionKind::Dangling);
        assert_eq!(repaired.endpoint_a.room_number, RoomNumber(1));
        assert!(repaired.endpoint_b.is_none());
        assert!(repaired.route_points.is_empty());
        assert_eq!(repaired.routing, ConnectionRouting::Simple);
    }

    #[test]
    fn retarget_swaps_canonical_order_and_reverses_the_route() {
        let id = ConnectionId::new();
        // One-member 1 → 5 with a stored manual route.
        let before = exit(
            1,
            id,
            5,
            ExitDirection::West,
            Some(6),
            Some(ExitDirection::East),
        );
        let mut connections = vec![Connection {
            id,
            endpoint_a: endpoint(RoomNumber(5), RoomSide::West, 0.5),
            endpoint_b: Some(endpoint(RoomNumber(6), RoomSide::East, 0.5)),
            routing: ConnectionRouting::Manual,
            route_points: vec![MapPoint::new(1.0, 0.0), MapPoint::new(2.0, 0.0)],
            ..blank_connection(
                endpoint(RoomNumber(5), RoomSide::West, 0.5),
                None,
                ConnectionKind::Internal,
            )
        }];
        // Retarget 5 → 2: canonical order flips (2 < 5).
        let after = ExitTopology {
            to_room_in_area: Some(RoomNumber(2)),
            ..before
        };
        let result = reattach_after_update(&before, &after, &[], &mut connections, flat_site);
        assert_eq!(result, id);
        let connection = &connections[0];
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(2));
        assert_eq!(
            connection.endpoint_b.expect("endpoint B").room_number,
            RoomNumber(5)
        );
        assert_eq!(
            connection.route_points,
            vec![MapPoint::new(2.0, 0.0), MapPoint::new(1.0, 0.0)],
            "the stored path must not visibly flip with the canonical swap"
        );
    }

    #[test]
    fn retarget_to_nowhere_clears_the_far_side() {
        let id = ConnectionId::new();
        let before = exit(
            1,
            id,
            1,
            ExitDirection::East,
            Some(2),
            Some(ExitDirection::West),
        );
        let mut connections = vec![Connection {
            id,
            endpoint_b: Some(endpoint(RoomNumber(2), RoomSide::West, 0.5)),
            routing: ConnectionRouting::Automatic,
            route_points: vec![MapPoint::new(1.0, 0.0)],
            ..blank_connection(
                endpoint(RoomNumber(1), RoomSide::East, 0.5),
                None,
                ConnectionKind::Internal,
            )
        }];
        let after = ExitTopology {
            to_room_in_area: None,
            to_direction: None,
            ..before
        };
        let result = reattach_after_update(&before, &after, &[], &mut connections, flat_site);
        assert_eq!(result, id);
        let connection = &connections[0];
        assert_eq!(connection.kind, ConnectionKind::Dangling);
        assert!(connection.endpoint_b.is_none());
        assert!(connection.route_points.is_empty());
        assert_eq!(connection.routing, ConnectionRouting::Simple);
    }

    #[test]
    fn breaking_a_pair_splits_the_edited_exit_out() {
        let shared = ConnectionId::new();
        let partner = exit(
            1,
            shared,
            2,
            ExitDirection::West,
            Some(1),
            Some(ExitDirection::East),
        );
        let before = exit(
            2,
            shared,
            1,
            ExitDirection::East,
            Some(2),
            Some(ExitDirection::West),
        );
        let mut connections = vec![Connection {
            id: shared,
            endpoint_b: Some(endpoint(RoomNumber(2), RoomSide::West, 0.5)),
            ..blank_connection(
                endpoint(RoomNumber(1), RoomSide::East, 0.5),
                None,
                ConnectionKind::Internal,
            )
        }];
        let after = ExitTopology {
            to_room_in_area: Some(RoomNumber(3)),
            to_direction: None,
            ..before
        };
        let result =
            reattach_after_update(&before, &after, &[partner], &mut connections, flat_site);
        assert_ne!(result, shared, "the edited exit moves to a new Connection");
        assert_eq!(connections.len(), 2);
        assert!(
            connections.iter().any(|c| c.id == shared),
            "the partner keeps the original Connection"
        );
    }

    #[test]
    fn orphan_removal_keeps_referenced_rows() {
        let id = ConnectionId::new();
        let keeper = exit(1, id, 1, ExitDirection::East, None, None);
        let mut connections = vec![blank_connection(
            endpoint(RoomNumber(1), RoomSide::East, 0.5),
            None,
            ConnectionKind::Dangling,
        )];
        connections[0].id = id;

        remove_orphan_connection(id, &[keeper], &mut connections);
        assert_eq!(connections.len(), 1);
        remove_orphan_connection(id, &[], &mut connections);
        assert!(connections.is_empty());
    }
}
