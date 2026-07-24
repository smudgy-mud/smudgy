//! Connection lifecycle helpers for the mock's appliers — the in-memory
//! mirror of the real server's `mapping/connections.rs`: §3.1 attachment for
//! new exits (auto-pair or a fresh one-member Connection with §1.5 anchors
//! and §4.3 insertion port slots), §3.3 cleanup when members disappear and
//! repair after a room deletion, and §3.2 endpoint maintenance after a
//! retarget. Every helper works over the mutation endpoint's working copy
//! (or the live state — same shape) and never bumps revisions itself.
//!
//! One knowing divergence from the current server file, flagged for the
//! parity report: `maintain_after_retarget`'s kept-Manual far endpoint. The
//! server SQL coalesces the preserved far port onto the ORIGIN's port and
//! stamps it `AutoPinned` (an apparent bug against its own "keeps a Manual
//! endpoint" intent), and reverses stored route points whenever the origin
//! lands in role B rather than when the canonical A room actually changes.
//! The mock implements the documented intent (preserve the Manual far
//! endpoint verbatim; reverse exactly when endpoint A's room changes),
//! matching the client's `connection_lifecycle` mirror.

use std::collections::BTreeMap;

use uuid::Uuid;

use super::state::{AreaRecord, ConnectionRecord, EndpointRecord};

/// The mock's working set: every area by id (the mutation endpoint clones
/// `MockState::areas` into exactly this shape).
pub type Working = BTreeMap<Uuid, AreaRecord>;

/// The §1.5 direction-to-anchor default: the wall and offset an endpoint is
/// initialized to. Non-planar (and absent) directions anchor on the wall
/// nearest the partner bearing, East at `0.5` when there is none.
pub fn anchor_for(direction: Option<&str>, bearing_x: f32, bearing_y: f32) -> (&'static str, f32) {
    match direction {
        Some("Northwest") => ("North", 0.0),
        Some("North") => ("North", 0.5),
        Some("Northeast") => ("North", 1.0),
        Some("East") => ("East", 0.5),
        Some("Southwest") => ("South", 0.0),
        Some("South") => ("South", 0.5),
        Some("Southeast") => ("South", 1.0),
        Some("West") => ("West", 0.5),
        _ => {
            if bearing_x.abs() >= bearing_y.abs() {
                if bearing_x >= 0.0 { ("East", 0.5) } else { ("West", 0.5) }
            } else if bearing_y >= 0.0 {
                ("South", 0.5)
            } else {
                ("North", 0.5)
            }
        }
    }
}

/// Whether a wall runs along the x axis (its bearing/offset order follows
/// x) or the y axis.
fn wall_axis_is_x(side: &str) -> bool {
    matches!(side, "North" | "South")
}

fn side_ordinal(side: &str) -> i32 {
    match side {
        "North" => 0,
        "East" => 1,
        "South" => 2,
        _ => 3,
    }
}

fn direction_component(direction: &str, axis_x: bool) -> f32 {
    const DIAG: f32 = std::f32::consts::FRAC_1_SQRT_2;
    let (dx, dy) = match direction {
        "North" => (0.0, -1.0),
        "East" => (1.0, 0.0),
        "South" => (0.0, 1.0),
        "West" => (-1.0, 0.0),
        "Northeast" => (DIAG, -DIAG),
        "Southeast" => (DIAG, DIAG),
        "Southwest" => (-DIAG, DIAG),
        "Northwest" => (-DIAG, -DIAG),
        _ => (0.0, 0.0),
    };
    if axis_x { dx } else { dy }
}

fn room_xy(working: &Working, area_id: Uuid, room: i32) -> Option<(f32, f32)> {
    working
        .get(&area_id)
        .and_then(|area| area.rooms.get(&room))
        .map(|room| (room.x, room.y))
}

fn room_secret(working: &Working, area_id: Uuid, room: i32) -> bool {
    working
        .get(&area_id)
        .and_then(|area| area.rooms.get(&room))
        .is_some_and(|room| room.is_secret)
}

fn member_count(area: &AreaRecord, connection_id: Uuid) -> usize {
    area.exits
        .iter()
        .filter(|exit| exit.connection_id == connection_id)
        .count()
}

/// The §4.3 auto-pinned slot for a NEW endpoint on `(room, side)`: existing
/// endpoints keep their offsets; the new one lands between its
/// bearing-neighboring occupied offsets (the wall edges count as 0 and 1).
/// Only endpoints of the same secrecy layout class participate, so a secret
/// endpoint can never influence a public coordinate.
pub fn insert_port_slot(
    working: &Working,
    area_id: Uuid,
    room_number: i32,
    side: &str,
    new_bearing: f32,
    secret_class: bool,
    default_offset: f32,
) -> f32 {
    let Some(area) = working.get(&area_id) else {
        return default_offset;
    };
    let axis_x = wall_axis_is_x(side);

    // (bearing, offset) of every same-class endpoint already on this wall.
    let mut occupied: Vec<(f32, f32)> = Vec::new();
    for connection in &area.connections {
        // Member facts: any member secret, any destination room secret
        // (ANY area — the mock, like the server, holds them all), and the
        // members' outbound direction for one-enders.
        let members: Vec<_> = area
            .exits
            .iter()
            .filter(|exit| exit.connection_id == connection.id)
            .collect();
        let any_member_secret = members.iter().any(|exit| exit.is_secret);
        let any_dest_secret = members.iter().any(|exit| {
            matches!(
                (exit.to_area_id, exit.to_room_number),
                (Some(to_area), Some(to_room)) if room_secret(working, to_area, to_room)
            )
        });
        let any_outbound = members
            .iter()
            .map(|exit| exit.from_direction.as_str())
            .min();

        let roles: [(&EndpointRecord, Option<&EndpointRecord>); 2] = [
            (&connection.endpoint_a, connection.endpoint_b.as_ref()),
            match connection.endpoint_b.as_ref() {
                Some(b) => (b, Some(&connection.endpoint_a)),
                None => (&connection.endpoint_a, None), // filtered below
            },
        ];
        for (index, (endpoint, partner)) in roles.into_iter().enumerate() {
            if index == 1 && connection.endpoint_b.is_none() {
                continue;
            }
            if endpoint.room_number != room_number || endpoint.side != side {
                continue;
            }
            let own_secret = room_secret(working, area_id, endpoint.room_number);
            let partner_secret =
                partner.is_some_and(|p| room_secret(working, area_id, p.room_number));
            let endpoint_secret =
                any_member_secret || any_dest_secret || own_secret || partner_secret;
            if endpoint_secret != secret_class {
                continue;
            }
            let bearing = match partner {
                Some(p) if p.room_number != room_number => {
                    let own = room_xy(working, area_id, room_number).unwrap_or((0.0, 0.0));
                    let other = room_xy(working, area_id, p.room_number).unwrap_or(own);
                    if axis_x { other.0 - own.0 } else { other.1 - own.1 }
                }
                Some(_) => 0.0, // self-loop partner: neutral bearing
                None => any_outbound.map_or(0.0, |d| direction_component(d, axis_x)),
            };
            occupied.push((bearing, endpoint.port_offset));
        }
    }

    if occupied.is_empty() {
        return default_offset;
    }
    occupied.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    let below = occupied
        .iter()
        .filter(|(bearing, _)| *bearing <= new_bearing)
        .map(|(_, port)| *port)
        .fold(f32::NAN, f32::max);
    let above = occupied
        .iter()
        .filter(|(bearing, _)| *bearing > new_bearing)
        .map(|(_, port)| *port)
        .fold(f32::NAN, f32::min);
    let low = if below.is_nan() { 0.0 } else { below };
    let high = if above.is_nan() { 1.0 } else { above };
    // A crowded or inverted gap stacks at the least-overlapping midpoint —
    // the editor surfaces the warning (§4.3); the slot stays in range.
    f32::midpoint(low, high).clamp(0.0, 1.0)
}

/// Facts needed to attach a new exit to a Connection.
pub struct NewExitLink {
    pub from_room: i32,
    pub from_direction: String,
    pub to_area_id: Option<Uuid>,
    pub to_room_number: Option<i32>,
    pub to_direction: Option<String>,
    pub is_secret: bool,
}

fn endpoint(room_number: i32, side: &str, port_offset: f32) -> EndpointRecord {
    EndpointRecord {
        room_number,
        side: side.to_string(),
        port_offset,
        port_mode: "AutoPinned".to_string(),
    }
}

/// §3.1 attachment for a newly created exit: auto-pair when exactly one
/// reciprocal one-member candidate exists whose explicit directions do not
/// contradict; otherwise create a one-member Connection with §1.5 anchors
/// and §4.3 port slots. Returns the connection id the new exit must carry.
///
/// `cleared` gates pairing per §6.2: an uncleared editor is never paired
/// onto a secret member's Connection.
pub fn attach_for_new_exit(
    working: &mut Working,
    area_id: Uuid,
    link: &NewExitLink,
    cleared: bool,
) -> Uuid {
    let same_area_dest = link.to_area_id == Some(area_id)
        && link.to_room_number.is_some()
        && link.to_room_number != Some(link.from_room);

    if same_area_dest {
        let area = working.get(&area_id).expect("scope area exists");
        let compatible: Vec<Uuid> = area
            .exits
            .iter()
            .filter(|exit| {
                exit.from_room_number == link.to_room_number.expect("same-area dest")
                    && exit.to_area_id == Some(area_id)
                    && exit.to_room_number == Some(link.from_room)
                    && (cleared || !exit.is_secret)
                    && member_count(area, exit.connection_id) == 1
                    && exit
                        .to_direction
                        .as_deref()
                        .is_none_or(|d| d == link.from_direction)
                    && link
                        .to_direction
                        .as_deref()
                        .is_none_or(|d| d == exit.from_direction)
            })
            .map(|exit| exit.connection_id)
            .collect();
        if let [only] = compatible.as_slice() {
            return *only;
        }
    }

    let from_xy = room_xy(working, area_id, link.from_room).unwrap_or((0.0, 0.0));
    let to_xy = if link.to_area_id == Some(area_id) {
        link.to_room_number
            .and_then(|room| room_xy(working, area_id, room))
    } else {
        None
    };

    let self_loop = link.to_area_id == Some(area_id) && link.to_room_number == Some(link.from_room);
    let has_b = same_area_dest || self_loop;
    let (o_side, o_offset_default) = anchor_for(
        Some(&link.from_direction),
        to_xy.unwrap_or(from_xy).0 - from_xy.0,
        to_xy.unwrap_or(from_xy).1 - from_xy.1,
    );

    let secret_class = link.is_secret
        || matches!(
            (link.to_area_id, link.to_room_number),
            (Some(to_area), Some(to_room)) if room_secret(working, to_area, to_room)
        )
        || room_secret(working, area_id, link.from_room);

    let o_bearing = if wall_axis_is_x(o_side) {
        to_xy.map_or_else(
            || direction_component(&link.from_direction, true),
            |to| to.0 - from_xy.0,
        )
    } else {
        to_xy.map_or_else(
            || direction_component(&link.from_direction, false),
            |to| to.1 - from_xy.1,
        )
    };

    let cid = Uuid::new_v4();
    if !has_b {
        let o_port = insert_port_slot(
            working,
            area_id,
            link.from_room,
            o_side,
            o_bearing,
            secret_class,
            o_offset_default,
        );
        let record =
            ConnectionRecord::blank(cid, endpoint(link.from_room, o_side, o_port), None);
        working
            .get_mut(&area_id)
            .expect("scope area exists")
            .connections
            .push(record);
        return cid;
    }

    // Same-area destination end.
    let (d_side, d_offset_default) = if self_loop {
        match link.to_direction.as_deref() {
            Some(direction) => anchor_for(Some(direction), 0.0, 0.0),
            None => (o_side, o_offset_default),
        }
    } else {
        anchor_for(
            link.to_direction.as_deref(),
            from_xy.0 - to_xy.unwrap_or(from_xy).0,
            from_xy.1 - to_xy.unwrap_or(from_xy).1,
        )
    };
    let to_room = link.to_room_number.expect("has_b requires a destination room");
    let d_bearing = if wall_axis_is_x(d_side) {
        from_xy.0 - to_xy.unwrap_or(from_xy).0
    } else {
        from_xy.1 - to_xy.unwrap_or(from_xy).1
    };

    let o_port = insert_port_slot(
        working,
        area_id,
        link.from_room,
        o_side,
        o_bearing,
        secret_class,
        o_offset_default,
    );
    let d_port = insert_port_slot(
        working, area_id, to_room, d_side, d_bearing, secret_class, d_offset_default,
    );

    // Canonical orientation: lower room number is endpoint A; self-loop
    // roles order by (side ordinal, offset, origin role first).
    let origin_first = if self_loop {
        (side_ordinal(o_side), o_port) <= (side_ordinal(d_side), d_port)
    } else {
        link.from_room < to_room
    };
    let (a, b) = if origin_first {
        (
            endpoint(link.from_room, o_side, o_port),
            endpoint(to_room, d_side, d_port),
        )
    } else {
        (
            endpoint(to_room, d_side, d_port),
            endpoint(link.from_room, o_side, o_port),
        )
    };
    working
        .get_mut(&area_id)
        .expect("scope area exists")
        .connections
        .push(ConnectionRecord::blank(cid, a, Some(b)));
    cid
}

/// §3.3 cleanup after a member exit was deleted: the last member takes the
/// Connection with it; a surviving member leaves the Connection one-way
/// with its geometry intact.
pub fn cleanup_after_exit_delete(working: &mut Working, area_id: Uuid, connection_id: Uuid) {
    let Some(area) = working.get_mut(&area_id) else {
        return;
    };
    if member_count(area, connection_id) == 0 {
        area.connections
            .retain(|connection| connection.id != connection_id);
    }
}

/// §3.3 repair after a room deletion, mirroring the server's order exactly:
/// the room's outgoing exits are removed first, inbound destinations (any
/// area) are nulled, Connections of THIS area that touch the room but keep
/// a surviving member convert to dangling (endpoint A = the survivor's
/// stored anchor, endpoint B and stored route cleared), and memberless
/// Connections touching the room are deleted.
pub fn repair_after_room_delete(working: &mut Working, area_id: Uuid, room_number: i32) {
    if let Some(area) = working.get_mut(&area_id) {
        area.exits.retain(|exit| exit.from_room_number != room_number);
    }
    for host in working.values_mut() {
        for exit in &mut host.exits {
            if exit.to_area_id == Some(area_id) && exit.to_room_number == Some(room_number) {
                exit.to_area_id = None;
                exit.to_room_number = None;
                exit.to_direction = None;
            }
        }
    }
    let Some(area) = working.get_mut(&area_id) else {
        return;
    };
    let survivors: Vec<Uuid> = area.exits.iter().map(|exit| exit.connection_id).collect();
    area.connections.retain_mut(|connection| {
        let touches = connection.endpoint_a.room_number == room_number
            || connection
                .endpoint_b
                .as_ref()
                .is_some_and(|b| b.room_number == room_number);
        if !touches {
            return true;
        }
        if survivors.contains(&connection.id) {
            // The old-row CASE semantics: when endpoint A was the deleted
            // room, endpoint B's stored anchor becomes A; B clears either
            // way, and the stored route goes with it.
            if connection.endpoint_a.room_number == room_number
                && let Some(b) = connection.endpoint_b.take()
            {
                connection.endpoint_a = b;
            }
            connection.endpoint_b = None;
            connection.route_points.clear();
            true
        } else {
            false
        }
    });
}

/// §3.2 endpoint maintenance after a one-member Connection's exit was
/// retargeted (see the module docs for the two knowing divergences from the
/// server SQL's quirks).
pub fn maintain_after_retarget(working: &mut Working, area_id: Uuid, connection_id: Uuid) {
    let Some(area) = working.get(&area_id) else {
        return;
    };
    let Some(exit) = area
        .exits
        .iter()
        .find(|exit| exit.connection_id == connection_id)
        .cloned()
    else {
        return;
    };
    let from_xy = room_xy(working, area_id, exit.from_room_number).unwrap_or((0.0, 0.0));
    let to_xy = if exit.to_area_id == Some(area_id) {
        exit.to_room_number
            .and_then(|room| room_xy(working, area_id, room))
    } else {
        None
    };

    let same_area_dest = exit.to_area_id == Some(area_id) && exit.to_room_number.is_some();

    // The stored origin anchor: whichever endpoint references the member's
    // origin room (falling back to endpoint A like the server's COALESCE).
    let take_origin = |connection: &ConnectionRecord| -> EndpointRecord {
        if connection.endpoint_a.room_number == exit.from_room_number {
            connection.endpoint_a.clone()
        } else {
            connection
                .endpoint_b
                .clone()
                .unwrap_or_else(|| connection.endpoint_a.clone())
        }
    };

    if !same_area_dest {
        // Dangling or external: endpoint A anchors at the member's origin;
        // B and the stored route go away.
        let Some(area) = working.get_mut(&area_id) else {
            return;
        };
        if let Some(connection) = area
            .connections
            .iter_mut()
            .find(|connection| connection.id == connection_id)
        {
            let mut origin = take_origin(connection);
            origin.room_number = exit.from_room_number;
            connection.endpoint_a = origin;
            connection.endpoint_b = None;
            connection.route_points.clear();
        }
        return;
    }

    let to_room = exit.to_room_number.expect("same-area destination has a room");
    let self_loop = to_room == exit.from_room_number;
    let stored = working
        .get(&area_id)
        .and_then(|area| {
            area.connections
                .iter()
                .find(|connection| connection.id == connection_id)
        })
        .cloned();
    let Some(stored) = stored else { return };

    // The far end keeps a Manual pin only when it still belongs to the same
    // room; otherwise it recomputes as an AutoPinned §1.5/§4.3 slot.
    let keep_far = stored
        .endpoint_b
        .as_ref()
        .is_some_and(|b| b.room_number == to_room && b.port_mode == "Manual");
    let far = if keep_far {
        stored.endpoint_b.clone().expect("kept endpoint exists")
    } else {
        let (side, default_offset) = if self_loop {
            match exit.to_direction.as_deref() {
                Some(direction) => anchor_for(Some(direction), 0.0, 0.0),
                None => anchor_for(Some(&exit.from_direction), 0.0, 0.0),
            }
        } else {
            anchor_for(
                exit.to_direction.as_deref(),
                from_xy.0 - to_xy.unwrap_or(from_xy).0,
                from_xy.1 - to_xy.unwrap_or(from_xy).1,
            )
        };
        let bearing = if wall_axis_is_x(side) {
            from_xy.0 - to_xy.unwrap_or(from_xy).0
        } else {
            from_xy.1 - to_xy.unwrap_or(from_xy).1
        };
        let port = insert_port_slot(
            working,
            area_id,
            to_room,
            side,
            bearing,
            exit.is_secret,
            default_offset,
        );
        endpoint(to_room, side, port)
    };

    // Canonical order: lower room number is A (origin stays A on self-loops
    // — the §1.4 role tie-break). The stored route reverses exactly when
    // endpoint A's room changes, so the visible path never flips.
    let origin_first = self_loop || exit.from_room_number < to_room;
    let Some(area) = working.get_mut(&area_id) else {
        return;
    };
    let Some(connection) = area
        .connections
        .iter_mut()
        .find(|connection| connection.id == connection_id)
    else {
        return;
    };
    let mut origin = take_origin(connection);
    origin.room_number = exit.from_room_number;
    let old_a_room = connection.endpoint_a.room_number;
    let (a, b) = if origin_first { (origin, far) } else { (far, origin) };
    if a.room_number != old_a_room {
        connection.route_points.reverse();
    }
    connection.endpoint_a = a;
    connection.endpoint_b = Some(b);
}
