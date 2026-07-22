//! Immutable editor snapshots for the off-thread Automatic route solver.
//! The solver deliberately sees the public room projection even when the
//! author can see secrets, preventing generated public geometry from encoding
//! unrelated secret-room positions.

use smudgy_cloud::{
    AreaId, ConnectionId, ConnectionKind, CornerStyle, MapPoint,
    automatic_routing::{AutoRouteRequest, RouteEndpoint, RouteObstacle, RouteRect},
    connection_geometry::ROOM_SIZE,
    mapper::area_cache::AreaCache,
};

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Snapshot {
    pub area_id: AreaId,
    pub area_rev: i64,
    pub connection_id: ConnectionId,
    pub endpoint_a: RouteEndpoint,
    pub endpoint_b: RouteEndpoint,
    pub obstacle_hash: u64,
    pub thickness_bits: u32,
    pub corner: CornerStyle,
}

/// Captures every input whose change invalidates an in-flight result.
pub(super) fn capture(
    area: &AreaCache,
    connection_id: ConnectionId,
) -> Result<(Snapshot, AutoRouteRequest), &'static str> {
    let connection = area
        .get_connection(connection_id)
        .ok_or("Connection no longer exists")?;
    if connection.kind != ConnectionKind::Internal {
        return Err("Automatic routing is available only for internal same-level links");
    }
    let endpoint_b = connection
        .endpoint_b
        .ok_or("Automatic routing requires two endpoints")?;
    let room_a = area
        .get_room(&connection.endpoint_a.room_number)
        .ok_or("Connection endpoint room is missing")?;
    let room_b = area
        .get_room(&endpoint_b.room_number)
        .ok_or("Connection endpoint room is missing")?;
    if room_a.get_level() != room_b.get_level() {
        return Err("Automatic routing requires endpoints on the same level");
    }

    let endpoint_a = RouteEndpoint {
        room_number: connection.endpoint_a.room_number,
        room_center: MapPoint::new(room_a.get_x(), room_a.get_y()),
        side: connection.endpoint_a.side,
        port_offset: connection.endpoint_a.port_offset,
    };
    let endpoint_b = RouteEndpoint {
        room_number: endpoint_b.room_number,
        room_center: MapPoint::new(room_b.get_x(), room_b.get_y()),
        side: endpoint_b.side,
        port_offset: endpoint_b.port_offset,
    };
    let endpoint_rooms = [endpoint_a.room_number, endpoint_b.room_number];
    let half_room = f64::from(ROOM_SIZE) / 2.0;
    let mut obstacles: Vec<_> = area
        .get_rooms()
        .iter()
        .filter(|room| {
            include_public_obstacle(
                room.get_level(),
                room.is_secret(),
                room.get_room_number(),
                room_a.get_level(),
                endpoint_rooms,
            )
        })
        .map(|room| RouteObstacle {
            room_number: room.get_room_number(),
            bounds: RouteRect::from_center(
                MapPoint::new(room.get_x(), room.get_y()),
                half_room,
                half_room,
            ),
        })
        .collect();
    obstacles.sort_by_key(|obstacle| obstacle.room_number);

    let request = AutoRouteRequest {
        endpoint_a,
        endpoint_b,
        obstacles,
        thickness: connection.thickness,
        corner: connection.corner,
    };
    let snapshot = Snapshot {
        area_id: *area.get_id(),
        area_rev: area.get_rev(),
        connection_id,
        endpoint_a,
        endpoint_b,
        obstacle_hash: obstacle_hash(&request.obstacles),
        thickness_bits: connection.thickness.to_bits(),
        corner: connection.corner,
    };
    Ok((snapshot, request))
}

fn include_public_obstacle(
    room_level: i32,
    room_is_secret: bool,
    room_number: smudgy_cloud::RoomNumber,
    route_level: i32,
    endpoint_rooms: [smudgy_cloud::RoomNumber; 2],
) -> bool {
    room_level == route_level && (!room_is_secret || endpoint_rooms.contains(&room_number))
}

/// Stable FNV-1a over the public obstacle projection. Sorting occurs in
/// [`capture`], and this function also sorts defensively for focused tests.
fn obstacle_hash(obstacles: &[RouteObstacle]) -> u64 {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut ordered = obstacles.to_vec();
    ordered.sort_by_key(|obstacle| obstacle.room_number);
    let mut hash = OFFSET;
    for obstacle in ordered {
        for byte in obstacle.room_number.0.to_le_bytes().into_iter().chain(
            [
                obstacle.bounds.min_x.to_bits(),
                obstacle.bounds.min_y.to_bits(),
                obstacle.bounds.max_x.to_bits(),
                obstacle.bounds.max_y.to_bits(),
            ]
            .into_iter()
            .flat_map(u64::to_le_bytes),
        ) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(PRIME);
        }
    }
    hash
}

#[cfg(test)]
mod tests {
    use smudgy_cloud::RoomNumber;

    use super::*;

    fn obstacle(room: i32, x: f64, y: f64) -> RouteObstacle {
        RouteObstacle {
            room_number: RoomNumber(room),
            bounds: RouteRect {
                min_x: x,
                min_y: y,
                max_x: x + 0.5,
                max_y: y + 0.5,
            },
        }
    }

    #[test]
    fn obstacle_signature_is_order_independent_but_geometry_sensitive() {
        let a = obstacle(1, 0.0, 0.0);
        let b = obstacle(2, 2.0, 0.0);
        assert_eq!(obstacle_hash(&[a, b]), obstacle_hash(&[b, a]));
        assert_ne!(
            obstacle_hash(&[a, b]),
            obstacle_hash(&[a, obstacle(2, 2.5, 0.0)])
        );
    }

    #[test]
    fn public_policy_omits_only_unrelated_secret_rooms_on_the_route_level() {
        let endpoints = [RoomNumber(1), RoomNumber(2)];
        assert!(include_public_obstacle(
            0,
            false,
            RoomNumber(3),
            0,
            endpoints
        ));
        assert!(!include_public_obstacle(
            0,
            true,
            RoomNumber(3),
            0,
            endpoints
        ));
        assert!(include_public_obstacle(
            0,
            true,
            RoomNumber(1),
            0,
            endpoints
        ));
        assert!(!include_public_obstacle(
            1,
            false,
            RoomNumber(3),
            0,
            endpoints
        ));
    }
}
