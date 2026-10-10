//! Immutable editor snapshots for the off-thread Automatic route solver.
//! Obstacles are the map's own rooms plus the link's readable endpoints.
//! Unrelated Secret rooms never influence a map link's route.

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
    // Errors are translation keys.
    let connection = area
        .get_connection(connection_id)
        .ok_or("mapper-route-link-changed")?;
    if connection.kind != ConnectionKind::Internal {
        return Err("mapper-route-same-level");
    }
    let endpoint_b = connection.endpoint_b.ok_or("mapper-route-same-level")?;
    let room_a = area
        .get_room_at(connection.endpoint_a.address())
        .ok_or("mapper-route-link-changed")?;
    let room_b = area
        .get_room_at(endpoint_b.address())
        .ok_or("mapper-route-link-changed")?;
    if room_a.get_level() != room_b.get_level() {
        return Err("mapper-route-same-level");
    }

    let endpoint_a = RouteEndpoint {
        room: connection.endpoint_a.address(),
        room_center: MapPoint::new(room_a.get_x(), room_a.get_y()),
        side: connection.endpoint_a.side,
        port_offset: connection.endpoint_a.port_offset,
    };
    let endpoint_b = RouteEndpoint {
        room: endpoint_b.address(),
        room_center: MapPoint::new(room_b.get_x(), room_b.get_y()),
        side: endpoint_b.side,
        port_offset: endpoint_b.port_offset,
    };
    let half_room = f64::from(ROOM_SIZE) / 2.0;
    let mut obstacles: Vec<_> = area
        .get_rooms()
        .iter()
        .filter(|room| room.get_level() == room_a.get_level())
        .map(|room| RouteObstacle {
            room: room.address(),
            bounds: RouteRect::from_center(
                MapPoint::new(room.get_x(), room.get_y()),
                half_room,
                half_room,
            ),
        })
        .collect();
    // A retained link may end in another source. Its readable endpoint is
    // necessary geometry; other rooms in that source remain excluded.
    for endpoint in [endpoint_a, endpoint_b] {
        if !endpoint.room.source.is_map() {
            obstacles.push(RouteObstacle {
                room: endpoint.room,
                bounds: RouteRect::from_center(endpoint.room_center, half_room, half_room),
            });
        }
    }
    obstacles.sort_by_key(|obstacle| obstacle.room);

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

/// Hash every qualified obstacle and its geometry, independent of input order.
fn obstacle_hash(obstacles: &[RouteObstacle]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut ordered = obstacles.to_vec();
    ordered.sort_by_key(|obstacle| obstacle.room);
    let mut hash = std::collections::hash_map::DefaultHasher::new();
    for obstacle in ordered {
        obstacle.room.hash(&mut hash);
        [
            obstacle.bounds.min_x,
            obstacle.bounds.min_y,
            obstacle.bounds.max_x,
            obstacle.bounds.max_y,
        ]
        .map(f64::to_bits)
        .hash(&mut hash);
    }
    hash.finish()
}

#[cfg(test)]
mod tests {
    use smudgy_cloud::RoomNumber;

    use super::*;

    fn obstacle(room: i32, x: f64, y: f64) -> RouteObstacle {
        RouteObstacle {
            room: smudgy_cloud::RoomAddress::map(RoomNumber(room)),
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

    #[tokio::test]
    async fn namesake_endpoints_route_without_revealing_other_secret_rooms() {
        use super::super::links::fixture::{C_GARDEN, link, secret, serving, the_maps};
        use smudgy_cloud::{
            RoomAddress,
            automatic_routing::{self, AutoRouteResult},
        };

        let mut map = the_maps(&["read"]).remove(0);
        map.rooms.retain(|room| room.room_number == RoomNumber(1));
        map.connections
            .retain(|connection| connection.id == link(C_GARDEN));
        let endpoint = map.connections[0].endpoint_b.as_mut().unwrap();
        endpoint.source = Some(secret());
        endpoint.room_number = RoomNumber(1);
        map.rooms[0]
            .exits
            .retain(|exit| exit.connection_id == link(C_GARDEN));
        map.rooms[0].exits[0].to_source = Some(secret());
        map.rooms[0].exits[0].to_room_number = Some(RoomNumber(1));
        let source = &mut map.sources[0];
        source.connections.clear();
        source.room_data.clear();
        source.rooms[0].exits.clear();
        source.rooms[0].x = 4.0;
        let mut unrelated = source.rooms[0].clone();
        unrelated.room_number = RoomNumber(2);
        unrelated.x = 2.0;
        source.rooms.push(unrelated);

        let map_id = map.area.id;
        let mapper = serving(vec![map]).await;
        let atlas = mapper.get_current_atlas();
        let cache = atlas.get_area(&map_id).unwrap();
        let (_, request) = capture(&cache, link(C_GARDEN)).unwrap();
        assert_eq!(request.endpoint_a.room, RoomAddress::map(RoomNumber(1)));
        assert_eq!(
            request.endpoint_b.room,
            RoomAddress::new(secret(), RoomNumber(1))
        );
        assert_eq!(
            request.obstacles.len(),
            2,
            "only the two endpoints are obstacles"
        );
        assert!(matches!(
            automatic_routing::solve(&request, &std::sync::atomic::AtomicBool::new(false)),
            AutoRouteResult::Solved { .. }
        ));
    }
}
