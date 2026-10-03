//! Exact post-commit room migrations for every isolate in one session.

use std::{
    cell::RefCell,
    collections::VecDeque,
    rc::Rc,
    sync::{Arc, Mutex, Weak},
};

use smudgy_cloud::{AreaId, Mapper, RoomRemap, mapper::MapperEvent};

use super::{CurrentLocation, RuntimeAction};

type MarkerAddress = Mutex<(AreaId, Option<i32>)>;
type PendingMarkers = Rc<RefCell<Vec<Weak<MarkerAddress>>>>;

/// One subscriber survives script reloads and is shared by all session ops.
#[derive(Clone)]
pub(crate) struct SharedMapperEvents {
    subscription: Option<Arc<smudgy_cloud::mapper::pending::RoomRemapSubscription>>,
    pending_markers: PendingMarkers,
    queued_remaps: Rc<RefCell<VecDeque<RuntimeAction>>>,
}

pub(crate) fn subscribe(mapper: Option<&Mapper>) -> SharedMapperEvents {
    SharedMapperEvents {
        subscription: mapper.map(Mapper::subscribe_room_remaps),
        pending_markers: Rc::default(),
        queued_remaps: Rc::default(),
    }
}

/// Retain an address only as long as its queued action lives. Later explicit
/// setters create new markers, so a reused address cannot inherit an old remap.
pub(crate) fn marker(
    events: &SharedMapperEvents,
    area: AreaId,
    room: Option<i32>,
) -> RuntimeAction {
    let location = Arc::new(Mutex::new((area, room)));
    let mut pending = events.pending_markers.borrow_mut();
    // Ordinary movement does not produce remaps. Prune expired weak entries
    // periodically without repeatedly scanning one large live script frame.
    if pending.len() >= 64 && pending.len().is_power_of_two() {
        pending.retain(|marker| marker.strong_count() > 0);
    }
    pending.push(Arc::downgrade(&location));
    RuntimeAction::SetPendingCurrentLocation(location)
}

/// Update readable locations immediately, then deliver migration callbacks
/// before marker callbacks. The issuer's op and the idle runtime use the same
/// receiver, so a commit is observed once even across package isolates.
pub(crate) fn drain(
    events: &SharedMapperEvents,
    current: &CurrentLocation,
) -> VecDeque<RuntimeAction> {
    observe(events, current);
    std::mem::take(&mut *events.queued_remaps.borrow_mut())
}

/// An op must update readable locations immediately, but only the runtime can
/// place new native callbacks behind older listeners already in its frame.
/// Staging here also survives reload and preserves one queue across isolates.
pub(crate) fn observe(events: &SharedMapperEvents, current: &CurrentLocation) {
    let Some(subscription) = events.subscription.as_ref() else {
        return;
    };
    let remaps = remap_actions(subscription.take(), events, current);
    if !remaps.is_empty() {
        insert_remaps(&mut events.queued_remaps.borrow_mut(), remaps);
    }
}

/// Preserve earlier migration deliveries, including their remaining host
/// callbacks, while getting the new batch ahead of queued marker actions.
pub(crate) fn insert_remaps(
    actions: &mut VecDeque<RuntimeAction>,
    mut remaps: VecDeque<RuntimeAction>,
) {
    let after_previous = actions
        .iter()
        .rposition(|action| match action {
            RuntimeAction::MapperRoomsMerged { .. } => true,
            RuntimeAction::CallJavascriptFunction { matches, .. } => {
                matches.iter().any(|capture| {
                    capture.name.as_deref() == Some("event") && capture.value == "map:merged"
                })
            }
            _ => false,
        })
        .map_or(0, |index| index + 1);
    let mut later = actions.split_off(after_previous);
    actions.append(&mut remaps);
    actions.append(&mut later);
}

fn remap_actions(
    remaps: impl IntoIterator<Item = MapperEvent>,
    events: &SharedMapperEvents,
    current: &CurrentLocation,
) -> VecDeque<RuntimeAction> {
    let mut actions = VecDeque::new();
    let initial_location = *current.borrow();
    for event in remaps {
        if let MapperEvent::AreasMerged { into, rooms, .. } = event {
            actions.push_back(RuntimeAction::MapperRoomsMerged {
                into,
                rooms: rooms.clone().into(),
            });
            if let Some(location) = current.borrow_mut().as_mut() {
                remap_location(location, into, &rooms);
            }
            // A frame can already contain a marker created before the
            // commit. Rewrite that pending address too, or its later
            // dispatch would restore the deleted source after map:merged.
            events.pending_markers.borrow_mut().retain(|pending| {
                let Some(pending) = pending.upgrade() else {
                    return false;
                };
                remap_location(&mut pending.lock().unwrap(), into, &rooms);
                true
            });
        }
    }
    if *current.borrow() != initial_location
        && let Some((area, room)) = *current.borrow()
    {
        // Consecutive migrations can delete an intermediate destination before
        // this session wakes. Deliver their remaps in order, then only the
        // final marker, so map:room never points at that transient address.
        actions.push_back(marker(events, area, room));
    }
    actions
}

fn remap_location(location: &mut (AreaId, Option<i32>), into: AreaId, rooms: &[RoomRemap]) {
    if let (area, Some(room)) = *location
        && let Some(moved) = rooms
            .iter()
            .find(|moved| moved.from.area_id == area && moved.from.room_number.0 == room)
    {
        *location = (into, Some(moved.to.0));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::{AreaId, RoomNumber, RoomRemap, Uuid, mapper::RoomKey};

    fn migration(from: AreaId, old: i32, into: AreaId, new: i32) -> MapperEvent {
        MapperEvent::AreasMerged {
            into,
            deleted: if from == into { vec![] } else { vec![from] },
            rooms: vec![RoomRemap {
                from: RoomKey::new(from, RoomNumber(old)),
                to: RoomNumber(new),
            }],
        }
    }

    fn pending_location(action: &RuntimeAction) -> (AreaId, Option<i32>) {
        let RuntimeAction::SetPendingCurrentLocation(location) = action else {
            panic!("expected a pending marker");
        };
        *location.lock().unwrap()
    }

    #[test]
    fn pending_markers_follow_chained_remaps_without_rewriting_new_setters() {
        let area = AreaId(Uuid::new_v4());
        let events = subscribe(None);
        let current = Rc::new(RefCell::new(Some((area, Some(2)))));
        let old_marker = marker(&events, area, Some(2));
        let first = remap_actions([migration(area, 2, area, 1)], &events, &current);
        assert_eq!(pending_location(&old_marker), (area, Some(1)));
        assert_eq!(pending_location(&first[1]), (area, Some(1)));

        // Room 2 was recreated after the join. This new explicit setter is
        // registered after the first remap and must retain its new identity.
        let reused_marker = marker(&events, area, Some(2));
        *current.borrow_mut() = Some((area, Some(2)));
        let second = remap_actions([migration(area, 1, area, 3)], &events, &current);
        assert_eq!(pending_location(&old_marker), (area, Some(3)));
        assert_eq!(pending_location(&first[1]), (area, Some(3)));
        assert_eq!(pending_location(&reused_marker), (area, Some(2)));
        assert_eq!(*current.borrow(), Some((area, Some(2))));
        assert_eq!(second.len(), 1, "the explicit current room did not move");

        drop(old_marker);
        drop(first);
        drop(reused_marker);
        remap_actions([migration(area, 3, area, 4)], &events, &current);
        assert!(events.pending_markers.borrow().is_empty());
    }

    #[test]
    fn new_remaps_follow_queued_delivery_before_pending_markers() {
        let source = AreaId(Uuid::new_v4());
        let first = AreaId(Uuid::new_v4());
        let second = AreaId(Uuid::new_v4());
        let events = subscribe(None);
        let current = Rc::new(RefCell::new(Some((source, Some(1)))));
        let mut frame = VecDeque::from([marker(&events, source, Some(1))]);
        insert_remaps(
            &mut frame,
            remap_actions([migration(source, 1, first, 4)], &events, &current),
        );
        insert_remaps(
            &mut frame,
            remap_actions([migration(first, 4, second, 9)], &events, &current),
        );
        assert!(matches!(frame[0], RuntimeAction::MapperRoomsMerged { into, .. } if into == first));
        assert!(
            matches!(frame[1], RuntimeAction::MapperRoomsMerged { into, .. } if into == second)
        );
        assert!(
            frame
                .iter()
                .skip(2)
                .all(|action| pending_location(action) == (second, Some(9)))
        );

        // Once dispatch expands a native event, its remaining listeners still
        // run before a newer migration, even when an earlier listener commits.
        let callback = RuntimeAction::CallJavascriptFunction {
            isolate: super::super::IsolateId::Main,
            id: super::super::script_engine::FunctionId(0),
            matches: Arc::new(vec![super::super::trigger::MatchCapture {
                name: Some(std::borrow::Cow::Borrowed("event")),
                value: "map:merged".into(),
            }]),
            depth: 0,
            is_captured: None,
        };
        let mut frame = VecDeque::from([callback, marker(&events, second, Some(9))]);
        let third = AreaId(Uuid::new_v4());
        insert_remaps(
            &mut frame,
            remap_actions([migration(second, 9, third, 3)], &events, &current),
        );
        assert!(matches!(
            frame[0],
            RuntimeAction::CallJavascriptFunction { .. }
        ));
        assert!(matches!(frame[1], RuntimeAction::MapperRoomsMerged { into, .. } if into == third));
    }

    #[test]
    fn queued_migrations_compose_before_the_final_marker() {
        let source = AreaId(Uuid::new_v4());
        let intermediate = AreaId(Uuid::new_v4());
        let destination = AreaId(Uuid::new_v4());
        let mut events = Vec::new();
        let current = Rc::new(RefCell::new(Some((source, Some(1)))));
        for (from, old, into, new) in [
            (source, 1, intermediate, 4),
            (intermediate, 4, destination, 9),
        ] {
            events.push(MapperEvent::AreasMerged {
                into,
                deleted: vec![from],
                rooms: vec![RoomRemap {
                    from: RoomKey::new(from, RoomNumber(old)),
                    to: RoomNumber(new),
                }],
            });
        }
        let subscriber = subscribe(None);
        let actions = remap_actions(events, &subscriber, &current);
        assert_eq!(*current.borrow(), Some((destination, Some(9))));
        assert_eq!(actions.len(), 3);
        assert!(
            matches!(actions[0], RuntimeAction::MapperRoomsMerged { into, .. } if into == intermediate)
        );
        assert!(
            matches!(actions[1], RuntimeAction::MapperRoomsMerged { into, .. } if into == destination)
        );
        assert!(
            matches!(&actions[2], RuntimeAction::SetPendingCurrentLocation(location) if *location.lock().unwrap() == (destination, Some(9)))
        );
    }
}
