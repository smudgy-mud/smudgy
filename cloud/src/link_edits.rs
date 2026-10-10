//! Link edits expressed as atomic batches of existing mutation operations.
//! Room arguments are source-qualified addresses; operation ownership is set
//! by the batch's source. No room-number translation occurs here.

use crate::{
    AreaId, ConnectionId, ExitArgs, ExitId, ExitUpdates, RoomAddress,
    mapper::exit_cache::ExitCache, mutation::AreaMutation,
};

/// The default exit back along `exit`, which leaves room `from_room` of
/// area `area_id` for another room of the same area: from that room, in
/// the direction `exit` arrives from (else the opposite of the one it
/// leaves by), to `from_room`, arriving from the direction `exit` leaves
/// by. It keeps the exit's weight and hidden flag; its door, command and
/// path start empty, since each side of a link has its own. `None` when
/// `exit` leads nowhere in `area_id`.
#[must_use]
pub fn reverse_of(area_id: AreaId, from_room: RoomAddress, exit: &ExitCache) -> Option<ExitArgs> {
    if exit.wire_to_area_id() != Some(area_id) {
        return None;
    }
    exit.to_room_number?;
    Some(ExitArgs {
        id: Some(ExitId::new()),
        from_direction: exit
            .to_direction
            .unwrap_or_else(|| exit.from_direction.opposite()),
        to_area_id: Some(area_id),
        to_room_number: Some(from_room.number),
        to_source: from_room.wire_source(),
        to_direction: Some(exit.from_direction),
        is_hidden: exit.is_hidden,
        weight: exit.weight,
        ..ExitArgs::default()
    })
}

/// Makes a two-way link one-way: its `return_exit` goes, and the link keeps
/// its connection and its other exit.
#[must_use]
pub fn make_one_way(return_exit: ExitId) -> Vec<AreaMutation> {
    vec![AreaMutation::DeleteExit {
        exit_id: return_exit,
    }]
}

/// Makes a one-way link two-way: `reverse`, an exit leaving `far_room` (the
/// room the link leads to) back along it, joins the link's `connection`.
#[must_use]
pub fn make_two_way(
    connection: ConnectionId,
    far_room: RoomAddress,
    reverse: ExitArgs,
) -> Vec<AreaMutation> {
    vec![join(connection, far_room, reverse)]
}

/// Swaps a one-way link's ends: `reverse`, an exit leaving `far_room` back
/// along the link, joins its `connection` first, then the link's exit `old`
/// goes. The other order would delete the connection with its last member,
/// and the create would then fail (`connection_not_found`).
#[must_use]
pub fn swap(
    old: ExitId,
    connection: ConnectionId,
    far_room: RoomAddress,
    reverse: ExitArgs,
) -> Vec<AreaMutation> {
    vec![
        join(connection, far_room, reverse),
        AreaMutation::DeleteExit { exit_id: old },
    ]
}

/// Retargets a two-way link: its `return_exit` goes, its exit `kept` takes
/// `retarget` (a new destination), and `new_return`, an exit leaving the
/// new far room `new_far_room` back along the link, joins the same
/// `connection`. A link stays one connection throughout, so its route,
/// colour and dash stay with it.
#[must_use]
pub fn retarget_two_way(
    connection: ConnectionId,
    return_exit: ExitId,
    kept: ExitId,
    retarget: ExitUpdates,
    new_far_room: RoomAddress,
    new_return: ExitArgs,
) -> Vec<AreaMutation> {
    vec![
        AreaMutation::DeleteExit {
            exit_id: return_exit,
        },
        AreaMutation::UpdateExit {
            exit_id: kept,
            body: retarget,
        },
        join(connection, new_far_room, new_return),
    ]
}

fn join(connection: ConnectionId, room: RoomAddress, mut exit: ExitArgs) -> AreaMutation {
    exit.connection_id = Some(connection);
    exit.new_connection_id = None;
    exit.id = Some(exit.id.unwrap_or_else(ExitId::new));
    AreaMutation::CreateExit {
        room_number: room.number,
        room_source: room.wire_source(),
        body: exit,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RoomNumber;
    use crate::{
        AreaWithDetails, Door, DoorState, ExitDirection, backends::area_edits,
        mutation::AreaMutation,
    };
    use uuid::Uuid;

    fn room(number: i32, x: f32) -> crate::RoomWithDetails {
        crate::RoomWithDetails {
            room_number: RoomNumber(number),
            title: format!("room {number}"),
            description: String::new(),
            level: 0,
            x,
            y: 0.0,
            color: String::new(),
            properties: Vec::new(),
            exits: Vec::new(),
            tags: std::collections::BTreeSet::new(),
            external_id: None,
        }
    }

    /// Rooms 1, 2 and 3 in a row, with a one-way link 1 → 2 (East).
    fn document() -> (AreaWithDetails, ExitId, ConnectionId) {
        let area_id = AreaId(Uuid::new_v4());
        let mut details: AreaWithDetails = serde_json::from_value(serde_json::json!({
            "id": area_id, "user_id": null, "atlas_id": null, "name": "Links",
            "created_at": "2026-10-06T00:00:00Z", "rev": 1,
            "format_version": crate::AREA_FORMAT_VERSION,
            "properties": [], "rooms": [], "labels": [], "shapes": [], "connections": []
        }))
        .expect("document");
        details.rooms = vec![room(1, 0.0), room(2, 4.0), room(3, 8.0)];
        let exit = ExitId::new();
        let created = area_edits::apply_mutation(
            &mut details,
            &AreaMutation::CreateExit {
                room_number: RoomNumber(1),
                room_source: None,
                body: ExitArgs {
                    id: Some(exit),
                    from_direction: ExitDirection::East,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(2)),
                    to_direction: Some(ExitDirection::West),
                    door: Some(Door::new(DoorState::Locked)),
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            },
        )
        .expect("the link");
        let crate::mutation::OpResult::Exit { exit: created } = created else {
            panic!("an exit echo");
        };
        (details, exit, created.connection_id)
    }

    fn apply(details: &mut AreaWithDetails, ops: &[AreaMutation]) {
        for op in ops {
            area_edits::apply_mutation(details, op).expect("applies");
        }
    }

    fn exits(details: &AreaWithDetails) -> Vec<(i32, ExitDirection, Option<i32>, ConnectionId)> {
        let mut all: Vec<_> = details
            .rooms
            .iter()
            .flat_map(|room| {
                room.exits.iter().map(|exit| {
                    (
                        room.room_number.0,
                        exit.from_direction,
                        exit.to_room_number.map(|room| room.0),
                        exit.connection_id,
                    )
                })
            })
            .collect();
        all.sort_by_key(|(room, ..)| *room);
        all
    }

    fn the_exit(details: &AreaWithDetails, id: ExitId) -> ExitCache {
        let exit = details
            .rooms
            .iter()
            .flat_map(|room| room.exits.iter())
            .find(|exit| exit.id == id)
            .expect("the exit")
            .clone();
        ExitCache::from(exit)
    }

    #[test]
    fn a_swap_creates_the_reverse_exit_before_deleting_the_old_one() {
        let (mut details, exit, connection) = document();
        let area_id = details.area.id;
        let reverse = reverse_of(
            area_id,
            RoomAddress::map(RoomNumber(1)),
            &the_exit(&details, exit),
        )
        .unwrap();
        assert_eq!(reverse.from_direction, ExitDirection::West);
        assert_eq!(reverse.door, None, "each side has its own door");
        let ops = swap(exit, connection, RoomAddress::map(RoomNumber(2)), reverse);
        assert!(matches!(ops[0], AreaMutation::CreateExit { .. }));
        assert!(matches!(ops[1], AreaMutation::DeleteExit { .. }));
        apply(&mut details, &ops);
        assert_eq!(
            exits(&details),
            [(2, ExitDirection::West, Some(1), connection)],
            "the link now runs 2 → 1 on its own connection"
        );
        assert_eq!(details.connections.len(), 1);

        // The other order loses the connection with its last member.
        let (mut details, exit, connection) = document();
        let area_id = details.area.id;
        let reverse = reverse_of(
            area_id,
            RoomAddress::map(RoomNumber(1)),
            &the_exit(&details, exit),
        )
        .unwrap();
        let mut reversed = swap(exit, connection, RoomAddress::map(RoomNumber(2)), reverse);
        reversed.reverse();
        area_edits::apply_mutation(&mut details, &reversed[0]).expect("the delete");
        assert!(area_edits::apply_mutation(&mut details, &reversed[1]).is_err());
    }

    #[test]
    fn one_way_and_two_way_keep_the_connection() {
        let (mut details, exit, connection) = document();
        let area_id = details.area.id;
        let reverse = reverse_of(
            area_id,
            RoomAddress::map(RoomNumber(1)),
            &the_exit(&details, exit),
        )
        .unwrap();
        let return_id = reverse.id.unwrap();
        apply(
            &mut details,
            &make_two_way(connection, RoomAddress::map(RoomNumber(2)), reverse),
        );
        assert_eq!(
            exits(&details),
            [
                (1, ExitDirection::East, Some(2), connection),
                (2, ExitDirection::West, Some(1), connection)
            ]
        );
        apply(&mut details, &make_one_way(return_id));
        assert_eq!(
            exits(&details),
            [(1, ExitDirection::East, Some(2), connection)]
        );
        assert_eq!(details.connections.len(), 1);
    }

    #[test]
    fn a_retargeted_two_way_link_stays_one_connection() {
        let (mut details, exit, connection) = document();
        let area_id = details.area.id;
        let reverse = reverse_of(
            area_id,
            RoomAddress::map(RoomNumber(1)),
            &the_exit(&details, exit),
        )
        .unwrap();
        let return_id = reverse.id.unwrap();
        apply(
            &mut details,
            &make_two_way(connection, RoomAddress::map(RoomNumber(2)), reverse),
        );
        let retarget = ExitUpdates {
            to_room_number: Some(RoomNumber(3)),
            ..ExitUpdates::default()
        };
        let new_return = ExitArgs {
            from_direction: ExitDirection::West,
            to_area_id: Some(area_id),
            to_room_number: Some(RoomNumber(1)),
            to_direction: Some(ExitDirection::East),
            weight: 1.0,
            ..ExitArgs::default()
        };
        apply(
            &mut details,
            &retarget_two_way(
                connection,
                return_id,
                exit,
                retarget,
                RoomAddress::map(RoomNumber(3)),
                new_return,
            ),
        );
        assert_eq!(
            exits(&details),
            [
                (1, ExitDirection::East, Some(3), connection),
                (3, ExitDirection::West, Some(1), connection)
            ]
        );
        assert_eq!(details.connections.len(), 1);
        let kept = the_exit(&details, exit);
        assert_eq!(
            kept.door.map(|door| door.state),
            Some(DoorState::Locked),
            "a retarget keeps the exit's door"
        );
    }

    #[test]
    fn a_link_into_another_place_has_no_reverse() {
        let (details, exit, _) = document();
        let mut into_secret = the_exit(&details, exit);
        into_secret.to_secret_map = Some(AreaId(Uuid::new_v4()));
        assert!(
            reverse_of(
                details.area.id,
                RoomAddress::map(RoomNumber(1)),
                &into_secret
            )
            .is_none()
        );
        let mut elsewhere = the_exit(&details, exit);
        elsewhere.to_area_id = Some(AreaId(Uuid::new_v4()));
        assert!(reverse_of(details.area.id, RoomAddress::map(RoomNumber(1)), &elsewhere).is_none());
    }
}
