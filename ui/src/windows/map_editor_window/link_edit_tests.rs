//! The link editor's edits end to end: undo of a first command, moving an
//! end without ever leaving the link nowhere, the way back of a link a
//! Secret keeps into another map, a dangling exit given a room, door
//! limits, turning an end of a link into another map, removing one's own
//! exit while another map keeps the way back, and that way back never
//! going alone when this map's write is refused and discarded.

use serde_json::json;
use smudgy_cloud::mapper::exit_cache::ExitCache;
use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{AreaWithDetails, Door, DoorState, ExitDirection, Mapper, RoomNumber, SourceId};
use smudgy_map_widget::map_editor::PlacedRoom;

use super::Message;
use super::commands::{CommandStack, FieldId, Mutation, Outcome};
use super::link_commands::{self, End, Sides};
use super::link_panel::{DoorSlot, Field, LinkMessage};
use super::links::fixture::*;
use super::links::{LinkView, RoomId, link_view};

fn keep_room(number: i32) -> RoomId {
    RoomId {
        map: area(KEEP),
        place: SourceId::Map,
        number: RoomNumber(number),
    }
}

fn view(mapper: &Mapper, id: smudgy_cloud::ConnectionId, anchor: i32) -> LinkView {
    let atlas = mapper.get_current_atlas();
    let keep = atlas.get_area(&area(KEEP)).expect("loaded");
    link_view(
        &atlas,
        &keep,
        id,
        Some(smudgy_cloud::RoomAddress::map(RoomNumber(anchor))),
    )
    .expect("the link")
}

fn exits(mapper: &Mapper, number: i32) -> Vec<ExitCache> {
    mapper
        .get_current_atlas()
        .get_area(&area(KEEP))
        .and_then(|area| area.get_room(&RoomNumber(number)).cloned())
        .map(|room| room.get_exits().to_vec())
        .unwrap_or_default()
}

fn garden_exit(mapper: &Mapper) -> ExitCache {
    exits(mapper, 1)
        .into_iter()
        .find(|exit| exit.id == exit_id(E_GARDEN))
        .expect("the Garden's exit")
}

/// Each write a command makes, in order: map or place, and its operations.
fn shapes(mutations: &[Mutation]) -> Vec<(bool, Vec<&'static str>)> {
    mutations
        .iter()
        .map(|mutation| {
            let (on_map, operations) = match mutation {
                Mutation::AreaBatch { operations, .. } => (true, operations),
                Mutation::SourceBatch { operations, .. } => (false, operations),
                _ => return (true, vec!["other"]),
            };
            (
                on_map,
                operations
                    .iter()
                    .map(|op| match op {
                        AreaMutation::DeleteLink { .. } => "delete_link",
                        AreaMutation::CreateConnection { .. } => "create_connection",
                        AreaMutation::CreateExit { .. } => "create_exit",
                        AreaMutation::UpdateExit { .. } => "update_exit",
                        _ => "other",
                    })
                    .collect(),
            )
        })
        .collect()
}

/// Undo of the panel's "☐ command" on an exit that had none takes the
/// command away again: the undo writes `""`, since an omitted command
/// means "unchanged".
#[tokio::test]
async fn undoing_a_first_command_takes_it_away() {
    let mapper = maps().await;
    assert_eq!(garden_exit(&mapper).command, None);
    let atlas = mapper.get_current_atlas();
    let command = link_commands::edit_exits(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_GARDEN), 1),
        Sides::One(End::From),
        FieldId::Command,
        |updates| updates.command = Some("pull lever".to_string()),
    )
    .expect("an edit");
    let [Mutation::AreaBatch { operations, .. }] = command.undo_mutations() else {
        panic!("one write");
    };
    let [AreaMutation::UpdateExit { body, .. }] = &operations[..] else {
        panic!("one exit update");
    };
    assert_eq!(
        (body.command.as_deref(), body.path.as_deref()),
        (Some(""), Some("")),
        "none, said outright"
    );
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    assert_eq!(stack.take_last_error(), None);
    assert_eq!(garden_exit(&mapper).command.as_deref(), Some("pull lever"));
    let _ = stack.undo(&mapper);
    assert_eq!(stack.take_last_error(), None);
    assert!(stack.can_redo(), "undo applied");
    assert_eq!(garden_exit(&mapper).command, None);
}

/// An end moved into another map's room keeps the link's place, so the
/// link is rewritten as one write: there is no moment it is gone.
#[tokio::test]
async fn an_end_moved_into_another_map_is_one_write() {
    let mapper = maps().await;
    let atlas = mapper.get_current_atlas();
    let ossuary = RoomId {
        map: area(CATACOMBS),
        place: SourceId::Map,
        number: RoomNumber(7),
    };
    let (command, id) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_GARDEN), 1),
        End::To,
        ossuary,
    )
    .expect("a retarget");
    assert_eq!(id, link(C_GARDEN), "the link keeps its id");
    assert_eq!(
        shapes(command.redo_mutations()),
        vec![(
            true,
            vec!["delete_link", "create_connection", "create_exit"]
        )]
    );
    assert_eq!(command.undo_mutations().len(), 1, "undo is one write too");
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    assert_eq!(stack.take_last_error(), None);
    let moved = garden_exit(&mapper);
    assert_eq!(
        (moved.to_area_id, moved.to_room_number),
        (Some(area(CATACOMBS)), Some(RoomNumber(7)))
    );
    let _ = stack.undo(&mapper);
    assert_eq!(stack.take_last_error(), None);
    let back = garden_exit(&mapper);
    assert_eq!(
        (back.to_area_id, back.to_room_number),
        (Some(area(KEEP)), Some(RoomNumber(3)))
    );
}

/// An end moved into a Secret's room makes the link again in the Secret
/// first, under new ids, and only then takes the map's link away; undo
/// brings the map's link back before the Secret's goes.
#[tokio::test]
async fn an_end_moved_into_a_secret_is_made_there_before_the_old_link_goes() {
    let mapper = maps().await;
    let atlas = mapper.get_current_atlas();
    let library = RoomId {
        map: area(KEEP),
        place: secret(),
        number: RoomNumber(1),
    };
    let (command, id) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_GARDEN), 1),
        End::To,
        library,
    )
    .expect("a retarget");
    assert_ne!(id, link(C_GARDEN));
    assert_eq!(
        shapes(command.redo_mutations()),
        vec![
            (false, vec!["create_connection", "create_exit"]),
            (true, vec!["delete_link"]),
        ]
    );
    assert_eq!(
        shapes(command.undo_mutations()),
        vec![
            (true, vec!["create_connection", "create_exit"]),
            (false, vec!["delete_link"]),
        ]
    );
}

/// The moved link's second write (the old link's delete) belongs to the
/// same gesture as its first: discarding a refused first write takes the
/// second off the queue with it, so the old link never goes alone.
#[tokio::test]
async fn the_rest_of_a_gesture_goes_with_its_discarded_write() {
    let mapper = maps().await;
    let atlas = mapper.get_current_atlas();
    let library = RoomId {
        map: area(KEEP),
        place: secret(),
        number: RoomNumber(1),
    };
    let (command, _) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_GARDEN), 1),
        End::To,
        library,
    )
    .expect("a retarget");
    let mut stack = CommandStack::default();
    let (_, operations) = stack.push_and_apply_tracked(&mapper, command);
    assert_eq!(operations.len(), 2, "the Secret's write, then the map's");
    assert_eq!(stack.operations_after(operations[0]), vec![operations[1]]);
    assert!(stack.operations_after(operations[1]).is_empty());
}

/// Serves the fixture's maps, refuses the writes `refuses` picks with
/// `refusal`, accepts the rest, and remembers every operation it was sent,
/// with its map.
struct Refusing {
    maps: Vec<AreaWithDetails>,
    refuses: fn(smudgy_cloud::AreaId, &smudgy_cloud::mutation::MutationEnvelope) -> bool,
    refusal: &'static str,
    sent: std::sync::Mutex<Vec<(smudgy_cloud::AreaId, AreaMutation)>>,
}

impl Refusing {
    /// Refuses every write to a Secret, as the server does once the Secret
    /// is gone or its access revoked.
    fn secrets() -> Self {
        Self {
            maps: the_maps(&["read", "add", "edit", "remove"]),
            refuses: |_, envelope| !envelope.source.is_map(),
            refusal: "secret_unavailable",
            sent: std::sync::Mutex::default(),
        }
    }

    /// Refuses every write to the Keep.
    fn the_keep() -> Self {
        Self {
            maps: the_maps(&["read", "add", "edit", "remove"]),
            refuses: |map, _| map == area(KEEP),
            refusal: "refused",
            sent: std::sync::Mutex::default(),
        }
    }

    /// Every operation sent, with its map.
    fn sent(&self) -> Vec<(smudgy_cloud::AreaId, AreaMutation)> {
        self.sent.lock().expect("the log").clone()
    }

    /// A mapper served by this backend, its maps loaded.
    async fn serve(self: &std::sync::Arc<Self>) -> Mapper {
        let dir =
            std::env::temp_dir().join(format!("smudgy-links-{}", smudgy_cloud::Uuid::new_v4()));
        let mapper = Mapper::new(self.clone(), dir);
        mapper.load_all_areas().await.expect("loads");
        mapper
    }
}

#[async_trait::async_trait]
impl smudgy_cloud::MapperBackend for Refusing {
    async fn create_area(
        &self,
        _request: smudgy_cloud::CreateAreaRequest,
    ) -> smudgy_cloud::CloudResult<smudgy_cloud::Area> {
        unreachable!("the test creates no maps")
    }
    async fn list_areas(&self) -> smudgy_cloud::CloudResult<Vec<smudgy_cloud::Area>> {
        Ok(self.maps.iter().map(|map| map.area.clone()).collect())
    }
    async fn get_area(
        &self,
        area_id: &smudgy_cloud::AreaId,
    ) -> smudgy_cloud::CloudResult<AreaWithDetails> {
        self.maps
            .iter()
            .find(|map| map.area.id == *area_id)
            .cloned()
            .ok_or(smudgy_cloud::CloudError::AreaNotFound(*area_id))
    }
    async fn update_area(
        &self,
        _area_id: &smudgy_cloud::AreaId,
        _updates: smudgy_cloud::AreaUpdates,
    ) -> smudgy_cloud::CloudResult<()> {
        Ok(())
    }
    async fn delete_area(&self, _area_id: &smudgy_cloud::AreaId) -> smudgy_cloud::CloudResult<()> {
        Ok(())
    }
    async fn execute_mutation(
        &self,
        area_id: &smudgy_cloud::AreaId,
        envelope: &smudgy_cloud::mutation::MutationEnvelope,
    ) -> smudgy_cloud::CloudResult<smudgy_cloud::mutation::MutationResult> {
        if (self.refuses)(*area_id, envelope) {
            return Err(smudgy_cloud::CloudError::InvalidInput(
                self.refusal.to_string(),
            ));
        }
        self.sent
            .lock()
            .expect("the log")
            .extend(envelope.payload.iter().cloned().map(|op| (*area_id, op)));
        Ok(smudgy_cloud::mutation::MutationResult {
            operation_id: envelope.operation_id,
            versions: vec![smudgy_cloud::mutation::VersionInfo::map_source(
                area_id.0, 100,
            )],
            data: Vec::new(),
        })
    }
}

/// Waits (half a minute at most) for the Keep's save status to satisfy
/// `done`.
async fn keep_status(
    mapper: &Mapper,
    done: impl Fn(&smudgy_cloud::mapper::AreaSaveStatus) -> bool,
) {
    for _ in 0..3000 {
        if done(&mapper.area_save_status(area(KEEP))) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    panic!("the Keep stays {:?}", mapper.area_save_status(area(KEEP)));
}

/// Waits for the Keep's queue to park on a refused write, and returns it.
async fn refused_in_the_keep(mapper: &Mapper) -> smudgy_cloud::mutation::OperationId {
    keep_status(mapper, |status| {
        matches!(
            status,
            smudgy_cloud::mapper::AreaSaveStatus::CouldNotSave { .. }
        )
    })
    .await;
    mapper
        .failed_operation_id(area(KEEP))
        .expect("the refused write parks the queue")
}

/// Discards the Keep's refused write `failed` as the editor's Discard does:
/// the rest of its gesture comes off the queue with it, and its command
/// leaves history.
async fn discard_in_the_keep(
    mapper: &Mapper,
    stack: &mut CommandStack,
    failed: smudgy_cloud::mutation::OperationId,
) {
    for operation in stack.operations_after(failed) {
        assert!(
            mapper
                .cancel_pending(area(KEEP), operation)
                .await
                .expect("cancels")
        );
    }
    mapper
        .resolve_failed(area(KEEP), false)
        .await
        .expect("discards");
    assert!(stack.discard_operation(failed));
}

/// Pushes `command`, refused in the Keep, and discards the refused write
/// while the window still waits for its acknowledgement; then answers that
/// wait as the window does. Returns the backend's log.
async fn refused_and_discarded(
    backend: &std::sync::Arc<Refusing>,
    mapper: &Mapper,
    command: super::commands::Command,
) -> Vec<(smudgy_cloud::AreaId, AreaMutation)> {
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply_tracked(mapper, command);
    assert_eq!(stack.take_last_error(), None);
    let waiting: Vec<_> = stack
        .acknowledgements(mapper)
        .into_iter()
        .map(tokio::spawn)
        .collect();
    assert_eq!(waiting.len(), 1, "the other map's write waits");
    let failed = refused_in_the_keep(mapper).await;
    discard_in_the_keep(mapper, &mut stack, failed).await;
    for waiting in waiting {
        let Outcome::Acknowledged {
            command,
            application,
            acknowledged,
        } = waiting.await.expect("the wait ends")
        else {
            panic!("an acknowledgement");
        };
        assert!(!acknowledged, "the refused write was never acknowledged");
        let _ = stack.follow_up(mapper, command, application, acknowledged);
    }
    keep_status(mapper, |status| {
        matches!(status, smudgy_cloud::mapper::AreaSaveStatus::Saved)
    })
    .await;
    assert_eq!(
        mapper.area_save_status(area(CATACOMBS)),
        smudgy_cloud::mapper::AreaSaveStatus::Saved,
        "nothing waits in the Catacombs"
    );
    backend.sent()
}

/// An end moved into a Secret the server then refuses: the Secret's create
/// parks the Keep's queue with the map's delete waiting behind it, and
/// discarding the refused create (as the editor's Discard does) takes the
/// delete with it, so the old link is never deleted on the server.
#[tokio::test]
async fn a_refused_create_never_leaves_its_delete_to_run_alone() {
    let backend = std::sync::Arc::new(Refusing::secrets());
    let mapper = backend.serve().await;
    let atlas = mapper.get_current_atlas();
    let library = RoomId {
        map: area(KEEP),
        place: secret(),
        number: RoomNumber(1),
    };
    let (command, _) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_GARDEN), 1),
        End::To,
        library,
    )
    .expect("a retarget");
    let mut stack = CommandStack::default();
    let (_, operations) = stack.push_and_apply_tracked(&mapper, command);
    assert_eq!(operations.len(), 2);
    let failed = refused_in_the_keep(&mapper).await;
    assert_eq!(failed, operations[0], "the create goes first");
    assert!(backend.sent().is_empty(), "nothing else went");

    discard_in_the_keep(&mapper, &mut stack, failed).await;
    keep_status(&mapper, |status| {
        matches!(status, smudgy_cloud::mapper::AreaSaveStatus::Saved)
    })
    .await;
    assert!(
        backend
            .sent()
            .iter()
            .all(|(_, operation)| !matches!(operation, AreaMutation::DeleteLink { .. })),
        "the old link was never deleted"
    );
    assert!(
        mapper
            .get_current_atlas()
            .get_area(&area(KEEP))
            .is_some_and(|keep| keep.get_connection(link(C_GARDEN)).is_some()),
        "and still shows"
    );
}

/// An end moved off the room another map keeps the way back to: the link
/// is rewritten in its place first, and that way back goes once the
/// rewrite is acknowledged; undo likewise restores it after the link.
#[tokio::test]
async fn an_end_moved_off_a_way_back_rewrites_the_link_before_the_way_back_goes() {
    let mapper = maps().await;
    let atlas = mapper.get_current_atlas();
    let (command, id) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_OSSUARY), 1),
        End::To,
        keep_room(3),
    )
    .expect("a retarget");
    assert_eq!(id, link(C_OSSUARY));
    let maps_written: Vec<_> = command
        .redo_mutations()
        .iter()
        .map(|mutation| match mutation {
            Mutation::AreaBatch { area_id, .. } | Mutation::SourceBatch { area_id, .. } => *area_id,
            _ => panic!("{mutation:?}"),
        })
        .collect();
    assert_eq!(maps_written, vec![area(KEEP), area(CATACOMBS)]);
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    assert_eq!(stack.take_last_error(), None);
    assert_eq!(catacombs_exits(&mapper).len(), 1, "the way back waits");
    stack.settle(&mapper).await;
    assert_eq!(stack.take_last_error(), None);
    assert!(catacombs_exits(&mapper).is_empty(), "the way back went");
    let _ = stack.undo(&mapper);
    assert_eq!(stack.take_last_error(), None);
    assert!(catacombs_exits(&mapper).is_empty(), "it comes back after");
    stack.settle(&mapper).await;
    assert_eq!(stack.take_last_error(), None);
    assert_eq!(catacombs_exits(&mapper).len(), 1);
    let _ = stack.redo(&mapper);
    stack.settle(&mapper).await;
    assert_eq!(stack.take_last_error(), None);
    assert!(catacombs_exits(&mapper).is_empty(), "redo takes it again");
}

/// The window closing before this map's rewrite is acknowledged queues the
/// way back's delete at once, beside the rewrite, rather than losing it
/// with the window's wait.
#[tokio::test]
async fn a_window_closing_queues_the_way_back_still_waiting() {
    let mapper = maps().await;
    let atlas = mapper.get_current_atlas();
    let (command, _) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_OSSUARY), 1),
        End::To,
        keep_room(3),
    )
    .expect("a retarget");
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    assert_eq!(catacombs_exits(&mapper).len(), 1, "the way back waits");
    stack.queue_waiting_follow_ups(&mapper);
    assert_eq!(stack.take_last_error(), None);
    assert!(catacombs_exits(&mapper).is_empty(), "the way back went");
    assert!(stack.acknowledgements(&mapper).is_empty(), "nothing waits");
    stack.settle(&mapper).await;
    assert!(catacombs_exits(&mapper).is_empty());
}

/// Undone before this map's rewrite is acknowledged, an end's move never
/// takes the way back another map keeps: its wait is answered no more.
#[tokio::test]
async fn an_end_moved_and_undone_at_once_leaves_the_way_back_alone() {
    let mapper = maps().await;
    let atlas = mapper.get_current_atlas();
    let (command, _) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_OSSUARY), 1),
        End::To,
        keep_room(3),
    )
    .expect("a retarget");
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    let waiting = stack.acknowledgements(&mapper);
    let _ = stack.undo(&mapper);
    assert_eq!(stack.take_last_error(), None);
    for waiting in waiting {
        if let Outcome::Acknowledged {
            command,
            application,
            acknowledged,
        } = waiting.await
        {
            let _ = stack.follow_up(&mapper, command, application, acknowledged);
        }
    }
    stack.settle(&mapper).await;
    assert_eq!(catacombs_exits(&mapper).len(), 1, "the way back stays");
    assert_eq!(
        keep_exit(&mapper, E_OSSUARY).to_area_id,
        Some(area(CATACOMBS)),
        "the link leads there again"
    );
}

/// An end moved off the room another map keeps the way back to, where the
/// Keep refuses the rewrite and the viewer discards it: the Catacombs' way
/// back is never deleted.
#[tokio::test]
async fn a_refused_rewrite_never_leaves_the_way_back_delete_to_run_alone() {
    let backend = std::sync::Arc::new(Refusing::the_keep());
    let mapper = backend.serve().await;
    let atlas = mapper.get_current_atlas();
    let (command, _) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_OSSUARY), 1),
        End::To,
        keep_room(3),
    )
    .expect("a retarget");
    let sent = refused_and_discarded(&backend, &mapper, command).await;
    assert!(
        sent.iter().all(|(map, _)| *map != area(CATACOMBS)),
        "the Catacombs' way back was never written"
    );
    assert_eq!(catacombs_exits(&mapper).len(), 1, "and still shows");
    assert_eq!(
        keep_exit(&mapper, E_OSSUARY).to_area_id,
        Some(area(CATACOMBS)),
        "the link leads there still"
    );
}

/// An end moved off the way back into a Secret, made again there before
/// the old link goes, where the server refuses the Secret's create and the
/// viewer discards it: neither the old link nor the Catacombs' way back is
/// deleted.
#[tokio::test]
async fn a_refused_create_never_leaves_the_way_back_delete_to_run_alone() {
    let backend = std::sync::Arc::new(Refusing::secrets());
    let mapper = backend.serve().await;
    let atlas = mapper.get_current_atlas();
    let library = RoomId {
        map: area(KEEP),
        place: secret(),
        number: RoomNumber(1),
    };
    let (command, _) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, link(C_OSSUARY), 1),
        End::To,
        library,
    )
    .expect("a retarget");
    let sent = refused_and_discarded(&backend, &mapper, command).await;
    assert!(
        sent.iter()
            .all(|(_, operation)| !matches!(operation, AreaMutation::DeleteLink { .. })),
        "nothing was deleted"
    );
    assert_eq!(
        catacombs_exits(&mapper).len(),
        1,
        "the way back still shows"
    );
}

/// A link removed whose way back another map keeps, where the Keep
/// refuses the delete and the viewer discards it: the Catacombs' way back
/// is never deleted.
#[tokio::test]
async fn a_refused_remove_never_leaves_the_way_back_delete_to_run_alone() {
    let backend = std::sync::Arc::new(Refusing::the_keep());
    let mapper = backend.serve().await;
    let atlas = mapper.get_current_atlas();
    let command = link_commands::remove(&atlas, area(KEEP), &view(&mapper, link(C_OSSUARY), 1))
        .expect("a removal");
    let sent = refused_and_discarded(&backend, &mapper, command).await;
    assert!(
        sent.iter().all(|(map, _)| *map != area(CATACOMBS)),
        "the Catacombs' way back was never written"
    );
    assert_eq!(catacombs_exits(&mapper).len(), 1, "and still shows");
}

/// A link removed whose way back another map keeps: the way back goes once
/// the Keep's delete is acknowledged, and undo restores both.
#[tokio::test]
async fn a_removed_links_way_back_goes_once_the_link_has() {
    let mapper = maps().await;
    let atlas = mapper.get_current_atlas();
    let command = link_commands::remove(&atlas, area(KEEP), &view(&mapper, link(C_OSSUARY), 1))
        .expect("a removal");
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    assert_eq!(stack.take_last_error(), None);
    assert_eq!(catacombs_exits(&mapper).len(), 1, "the way back waits");
    stack.settle(&mapper).await;
    assert!(catacombs_exits(&mapper).is_empty(), "the way back went");
    let _ = stack.undo(&mapper);
    stack.settle(&mapper).await;
    assert_eq!(stack.take_last_error(), None);
    assert_eq!(catacombs_exits(&mapper).len(), 1);
    assert_eq!(
        keep_exit(&mapper, E_OSSUARY).to_area_id,
        Some(area(CATACOMBS))
    );
}

/// A dangling exit given a room keeps its door, command, weight and hidden
/// flag; undo brings the dangling exit back with them.
#[tokio::test]
async fn a_dangling_exit_given_a_room_keeps_its_fields() {
    let mapper = maps().await;
    let (command, id) = link_commands::create_dangling(
        area(KEEP),
        keep_room(3),
        ExitDirection::North,
        SourceId::Map,
    )
    .expect("a dangling exit");
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    assert_eq!(stack.take_last_error(), None);
    let dangling = exits(&mapper, 3)
        .into_iter()
        .find(|exit| exit.connection_id == id)
        .expect("the exit");
    assert_eq!(
        (
            dangling.to_area_id,
            dangling.to_room_number,
            dangling.to_direction
        ),
        (None, None, None)
    );
    let door = Door {
        state: DoorState::Locked,
        name: Some("gate".to_string()),
        opens_with: Some("unlock gate".to_string()),
    };
    let atlas = mapper.get_current_atlas();
    let door_for_edit = door.clone();
    let edit = link_commands::edit_exits(
        &atlas,
        area(KEEP),
        &view(&mapper, id, 3),
        Sides::One(End::From),
        FieldId::Flags,
        move |updates| {
            updates.door = Some(Some(door_for_edit.clone()));
            updates.is_hidden = Some(true);
            updates.weight = Some(3.0);
            updates.command = Some("climb".to_string());
        },
    )
    .expect("an edit");
    let _ = stack.push_and_apply(&mapper, edit);
    assert_eq!(stack.take_last_error(), None);

    let atlas = mapper.get_current_atlas();
    let (retarget, new_id) = link_commands::retarget(
        &atlas,
        area(KEEP),
        &view(&mapper, id, 3),
        End::To,
        keep_room(5),
    )
    .expect("a destination");
    let _ = stack.push_and_apply(&mapper, retarget);
    assert_eq!(stack.take_last_error(), None);
    let forward = exits(&mapper, 3)
        .into_iter()
        .find(|exit| exit.connection_id == new_id)
        .expect("the exit");
    assert_eq!(forward.to_room_number, Some(RoomNumber(5)));
    assert_eq!(forward.to_direction, Some(ExitDirection::South));
    assert_eq!(forward.door.as_ref(), Some(&door));
    assert!(forward.is_hidden);
    assert!((forward.weight - 3.0).abs() < f32::EPSILON);
    assert_eq!(forward.command.as_deref(), Some("climb"));
    let back = exits(&mapper, 5)
        .into_iter()
        .find(|exit| exit.connection_id == new_id)
        .expect("the way back");
    assert_eq!(back.from_direction, ExitDirection::South);
    assert_eq!(back.door.as_ref(), Some(&door));

    let _ = stack.undo(&mapper);
    assert_eq!(stack.take_last_error(), None);
    let again = exits(&mapper, 3)
        .into_iter()
        .find(|exit| exit.id == forward.id)
        .expect("the exit is back");
    assert_eq!(
        (again.to_area_id, again.to_room_number, again.to_direction),
        (None, None, None),
        "dangling again"
    );
    assert_eq!(again.door.as_ref(), Some(&door));
    assert_eq!(again.command.as_deref(), Some("climb"));
    assert!(
        exits(&mapper, 5)
            .iter()
            .all(|exit| exit.connection_id != new_id)
    );
    let _ = stack.redo(&mapper);
    assert_eq!(stack.take_last_error(), None);
    assert!(
        exits(&mapper, 5)
            .iter()
            .any(|exit| exit.connection_id == new_id)
    );
}

/// A door name past 64 characters is refused before it is sent, with a
/// notice in the viewer's language; 64 two-byte letters pass.
#[tokio::test]
async fn an_overlong_door_name_is_refused_in_the_viewers_language() {
    let mut window = super::test_window(maps().await, area(KEEP));
    window
        .editor
        .select_link_from(link(C_GARDEN), PlacedRoom::map(RoomNumber(1)).into());
    window.inspector.resync(&window.mapper, &window.editor);
    let slot = DoorSlot::Side(End::From);
    let _ = window.update(Message::Links(LinkMessage::Door(
        slot,
        super::links::DoorState::Closed,
    )));
    let named = |window: &super::MapEditorWindow| {
        garden_exit(&window.mapper).door.and_then(|door| door.name)
    };
    let _ = window.update(Message::Links(LinkMessage::Typed(
        Field::Name(slot),
        "ż".repeat(65),
    )));
    assert_eq!(
        window
            .editor_notice
            .as_ref()
            .map(|(_, notice)| notice.clone()),
        Some(crate::i18n::t!(
            "link-door-name-too-long",
            "limit" => smudgy_cloud::DOOR_NAME_LIMIT
        ))
    );
    assert_eq!(named(&window), None, "nothing written");

    window.editor_notice = None;
    let _ = window.update(Message::Links(LinkMessage::Typed(
        Field::Name(slot),
        "ż".repeat(64),
    )));
    assert_eq!(window.editor_notice, None);
    assert_eq!(named(&window), Some("ż".repeat(64)));

    let _ = window.update(Message::Links(LinkMessage::Typed(
        Field::OpensWith(slot),
        "x".repeat(256),
    )));
    assert_eq!(
        window
            .editor_notice
            .as_ref()
            .map(|(_, notice)| notice.clone()),
        Some(crate::i18n::t!(
            "link-opens-with-too-long",
            "limit" => smudgy_cloud::DOOR_COMMAND_LIMIT
        ))
    );
}

fn catacombs_exits(mapper: &Mapper) -> Vec<ExitCache> {
    mapper
        .get_current_atlas()
        .get_area(&area(CATACOMBS))
        .and_then(|area| area.get_room(&RoomNumber(7)).cloned())
        .map(|room| room.get_exits().to_vec())
        .unwrap_or_default()
}

fn catacombs_exit(mapper: &Mapper, id: &str) -> ExitCache {
    catacombs_exits(mapper)
        .into_iter()
        .find(|exit| exit.id == exit_id(id))
        .expect("the far exit")
}

fn keep_exit(mapper: &Mapper, id: &str) -> ExitCache {
    exits(mapper, 1)
        .into_iter()
        .find(|exit| exit.id == exit_id(id))
        .expect("the near exit")
}

/// A link into another map that keeps the way back reads two-way; turning
/// either end turns the other exit's arrival with it, as within a map, in
/// one undo step.
#[tokio::test]
async fn turning_an_end_of_a_cross_map_link_turns_the_other_exits_arrival() {
    let mut window = super::test_window(maps().await, area(KEEP));
    window
        .editor
        .select_link_from(link(C_OSSUARY), PlacedRoom::map(RoomNumber(1)).into());
    window.inspector.resync(&window.mapper, &window.editor);
    assert!(window.selected_link_view().expect("selected").two_way());
    let _ = window.update(Message::Links(LinkMessage::Leaves(
        End::To,
        ExitDirection::North,
    )));
    assert_eq!(
        catacombs_exit(&window.mapper, E_BACK).from_direction,
        ExitDirection::North
    );
    assert_eq!(
        keep_exit(&window.mapper, E_OSSUARY).to_direction,
        Some(ExitDirection::North)
    );
    let _ = window.update(Message::Links(LinkMessage::Leaves(
        End::From,
        ExitDirection::East,
    )));
    let back = catacombs_exit(&window.mapper, E_BACK);
    let forward = keep_exit(&window.mapper, E_OSSUARY);
    assert_eq!(forward.from_direction, ExitDirection::East);
    assert_eq!(
        (forward.to_direction, back.to_direction),
        (Some(ExitDirection::North), Some(ExitDirection::East)),
    );
    // Undo turns both exits back.
    let _ = window.stack.undo(&window.mapper);
    assert_eq!(window.stack.take_last_error(), None);
    assert_eq!(
        catacombs_exit(&window.mapper, E_BACK).to_direction,
        Some(ExitDirection::Down)
    );
    assert_eq!(
        keep_exit(&window.mapper, E_OSSUARY).from_direction,
        ExitDirection::Down
    );
}

const HOME: &str = "000000a1-0000-4000-8000-0000000000a1";
const SHARED: &str = "000000a2-0000-4000-8000-0000000000a2";
const C_DOWN: &str = "00000000-0000-4000-8000-0000000000d1";
const C_UP: &str = "00000000-0000-4000-8000-0000000000d2";

/// Home #1 down into Shared #7, whose way back up Shared keeps; the viewer
/// only reads Shared.
async fn a_link_into_a_view_only_map() -> Mapper {
    let exit = |id: &str, dir: &str, map: &str, to: i32, back: &str, link: &str| {
        json!({
            "id": id, "from_direction": dir, "to_area_id": map, "to_room_number": to,
            "to_direction": back, "to_unknown": false, "path": "", "command": "", "weight": 1.0,
            "connection_id": link, "is_hidden": false, "door": null
        })
    };
    let connection = |id: &str, room: i32| {
        json!({
            "id": id, "endpoint_a": {"room_number": room, "side": "South", "port_offset": 0.5,
            "port_mode": "AutoPinned"}, "kind": "External", "routing": "Simple",
            "segment_shape": "Direct", "corner": "Sharp", "route_points": [], "dash": "Solid",
            "color": "#A4A4A4", "thickness": 1.0
        })
    };
    let map = |id: &str,
               name: &str,
               number: i32,
               exits: serde_json::Value,
               link: serde_json::Value,
               access: Option<serde_json::Value>| {
        let mut value = json!({
            "id": id, "user_id": null, "atlas_id": null, "name": name,
            "created_at": "2026-10-06T00:00:00Z",
            "format_version": smudgy_cloud::AREA_FORMAT_VERSION,
            "properties": [], "labels": [], "shapes": [],
            "rooms": [{"room_number": number, "title": name, "description": "", "color": "",
                "level": 0, "x": 0.0, "y": 0.0, "properties": [], "exits": exits, "tags": []}],
            "connections": [link]
        });
        if let Some(access) = access {
            value["access"] = access;
        }
        serde_json::from_value::<AreaWithDetails>(value).expect("a map")
    };
    let home = map(
        HOME,
        "Home",
        1,
        json!([exit(
            "00000000-0000-4000-8000-0000000000f1",
            "Down",
            SHARED,
            7,
            "Up",
            C_DOWN
        )]),
        connection(C_DOWN, 1),
        None,
    );
    let shared = map(
        SHARED,
        "Shared",
        7,
        json!([exit(
            "00000000-0000-4000-8000-0000000000f2",
            "Up",
            HOME,
            1,
            "Down",
            C_UP
        )]),
        connection(C_UP, 7),
        Some(
            json!({"is_owner": false, "can_edit": false, "can_reshare": false,
            "can_copy": false, "can_admin": false, "include_secrets": false}),
        ),
    );
    serving(vec![home, shared]).await
}

/// Removing a link whose way back another map keeps, when the viewer only
/// reads that map: the viewer's own exit goes, and the other map keeps its
/// exit.
#[tokio::test]
async fn ones_own_exit_goes_while_a_view_only_map_keeps_its_way_back() {
    let mut window = super::test_window(a_link_into_a_view_only_map().await, area(HOME));
    window
        .editor
        .select(smudgy_map_widget::map_editor::EntityId::Room(RoomNumber(1)));
    window.inspector.resync(&window.mapper, &window.editor);
    let view = {
        let atlas = window.mapper.get_current_atlas();
        let home = atlas.get_area(&area(HOME)).expect("loaded");
        link_view(
            &atlas,
            &home,
            link(C_DOWN),
            Some(smudgy_cloud::RoomAddress::map(RoomNumber(1))),
        )
        .expect("the link")
    };
    assert!(
        view.return_elsewhere.is_some(),
        "the Shared map keeps the way back"
    );
    let _ = window.update(Message::Links(LinkMessage::RowRemoved(link(C_DOWN))));
    let atlas = window.mapper.get_current_atlas();
    assert_eq!(window.editor_notice, None);
    assert!(
        atlas
            .get_area(&area(HOME))
            .is_some_and(|home| home.find_connection(link(C_DOWN)).is_none()),
        "the viewer's own exit goes"
    );
    assert!(
        atlas
            .get_area(&area(SHARED))
            .is_some_and(|shared| shared.find_connection(link(C_UP)).is_some()),
        "the Shared map keeps its exit"
    );
}

/// The exits of `map`'s room `number`, as (leaves, arrives from).
fn turns(mapper: &Mapper, map: &str, number: i32) -> Vec<(ExitDirection, Option<ExitDirection>)> {
    mapper
        .get_current_atlas()
        .get_area(&area(map))
        .and_then(|area| area.get_room(&RoomNumber(number)).cloned())
        .map(|room| {
            room.get_exits()
                .iter()
                .map(|exit| (exit.from_direction, exit.to_direction))
                .collect()
        })
        .unwrap_or_default()
}

/// Turning the From end of a link whose way back a view-only map keeps:
/// the viewer's own exit turns, and the way back keeps its arrival, as the
/// other map's own exit.
#[tokio::test]
async fn turning_the_from_end_leaves_a_view_only_maps_way_back_alone() {
    let mut window = super::test_window(a_link_into_a_view_only_map().await, area(HOME));
    window
        .editor
        .select_link_from(link(C_DOWN), PlacedRoom::map(RoomNumber(1)).into());
    window.inspector.resync(&window.mapper, &window.editor);
    assert!(
        window
            .selected_link_view()
            .expect("selected")
            .return_elsewhere
            .is_some(),
        "the Shared map keeps the way back"
    );
    let shared = turns(&window.mapper, SHARED, 7);
    let _ = window.update(Message::Links(LinkMessage::Leaves(
        End::From,
        ExitDirection::East,
    )));
    assert_eq!(window.editor_notice, None, "the turn is not refused");
    assert_eq!(window.stack.take_last_error(), None);
    assert_eq!(
        turns(&window.mapper, HOME, 1),
        vec![(ExitDirection::East, Some(ExitDirection::Up))],
        "the viewer's own exit turns"
    );
    assert_eq!(
        turns(&window.mapper, SHARED, 7),
        shared,
        "the way back stays"
    );
}

const E_SECRET_DOWN: &str = "00000000-0000-4000-8000-0000000000f1";
const C_SECRET_DOWN: &str = "00000000-0000-4000-8000-0000000000f2";

fn room(number: i32, title: &str, exits: serde_json::Value) -> serde_json::Value {
    json!({
        "room_number": number, "title": title, "description": "", "color": "",
        "level": 0, "x": 0.0, "y": f64::from(number), "properties": [], "exits": exits, "tags": []
    })
}

fn down_to_the_ossuary(own_room: bool) -> serde_json::Value {
    let mut end = json!({
        "room_number": 1, "side": "South", "port_offset": 0.5, "port_mode": "AutoPinned"
    });
    if own_room {
        end["source"] = json!(SECRET);
    }
    let exit = json!({
        "id": E_SECRET_DOWN, "from_direction": "Down", "to_area_id": CATACOMBS,
        "to_room_number": 7, "to_direction": "Up", "to_unknown": false, "path": "",
        "command": "", "weight": 1.0, "connection_id": C_SECRET_DOWN,
        "is_hidden": false, "door": null
    });
    let connection = json!({
        "id": C_SECRET_DOWN, "kind": "External", "endpoint_a": end,
        "routing": "Simple", "segment_shape": "Direct", "corner": "Sharp",
        "route_points": [], "dash": "Solid", "color": "#A4A4A4", "thickness": 1.0
    });
    let (rooms, room_data) = if own_room {
        (json!([room(1, "Hidden Library", json!([exit]))]), json!([]))
    } else {
        (
            json!([]),
            json!([{ "room_number": 1, "properties": [], "tags": [], "exits": [exit] }]),
        )
    };
    json!({
        "source": SECRET, "name": "Bookshelf", "ownership": "owner", "rev": 2,
        "actions": ["read", "add", "edit", "remove"],
        "rooms": rooms, "room_data": room_data,
        "connections": [connection], "labels": []
    })
}

/// The Keep's Secret Bookshelf keeps a one-way link down into the
/// Catacombs' map room 7: from the Keep's map room 1, or (`own_room`) from
/// its own room 1. Nothing of it is in either map's own content.
async fn secret_link_maps(own_room: bool) -> Mapper {
    let keep: AreaWithDetails = serde_json::from_value(json!({
        "id": KEEP, "user_id": null, "atlas_id": null, "name": "Keep",
        "created_at": "2026-10-06T00:00:00Z",
        "format_version": smudgy_cloud::AREA_FORMAT_VERSION,
        "properties": [], "labels": [], "shapes": [], "connections": [],
        "rooms": [room(1, "Hall", json!([]))],
        "sources": [down_to_the_ossuary(own_room)]
    }))
    .expect("the Keep");
    let catacombs: AreaWithDetails = serde_json::from_value(json!({
        "id": CATACOMBS, "user_id": null, "atlas_id": null, "name": "Catacombs",
        "created_at": "2026-10-06T00:00:00Z",
        "format_version": smudgy_cloud::AREA_FORMAT_VERSION,
        "properties": [], "labels": [], "shapes": [], "connections": [],
        "rooms": [room(7, "Ossuary", json!([]))],
        "sources": []
    }))
    .expect("the Catacombs");
    serving(vec![keep, catacombs]).await
}

/// A link the Bookshelf keeps from a map room into the Catacombs stays
/// one-way: its way back would be the Catacombs' own exit, which every
/// Catacombs reader sees. Two-way is refused with the reason, and nothing
/// is written.
#[tokio::test]
async fn a_secrets_link_from_a_map_room_into_another_map_stays_one_way() {
    let mapper = secret_link_maps(false).await;
    let atlas = mapper.get_current_atlas();
    let keep = atlas.get_area(&area(KEEP)).expect("loaded");
    let view = link_view(
        &atlas,
        &keep,
        link(C_SECRET_DOWN),
        Some(smudgy_cloud::RoomAddress::map(RoomNumber(1))),
    )
    .expect("the link");
    assert_eq!(view.place, secret());
    assert!(!view.two_way());
    assert_eq!(
        link_commands::way_back_shows_more(&view),
        Some(RoomId {
            map: area(CATACOMBS),
            place: SourceId::Map,
            number: RoomNumber(7),
        })
    );
    assert!(link_commands::two_way(&atlas, area(KEEP), &view).is_none());

    let mut window = super::test_window(mapper, area(KEEP));
    window
        .editor
        .select_link_from(link(C_SECRET_DOWN), PlacedRoom::map(RoomNumber(1)).into());
    window.inspector.resync(&window.mapper, &window.editor);
    let _ = window.update(Message::Links(LinkMessage::TwoWay(true)));
    assert_eq!(
        window
            .editor_notice
            .as_ref()
            .map(|(_, notice)| notice.clone()),
        Some(crate::i18n::t!("link-two-way-would-show", "map" => "Catacombs"))
    );
    let catacombs = window
        .mapper
        .get_current_atlas()
        .get_area(&area(CATACOMBS))
        .expect("loaded");
    assert!(
        catacombs
            .get_room(&RoomNumber(7))
            .is_some_and(|room| room.get_exits().is_empty()),
        "nothing written into the Catacombs"
    );
}

/// A link the Bookshelf keeps from its own room into the Catacombs comes
/// back by the Catacombs' exit into that room, which names the Bookshelf:
/// only the Bookshelf's readers are ever shown it.
#[tokio::test]
async fn a_secrets_link_from_its_own_room_comes_back_into_the_secret() {
    let mapper = secret_link_maps(true).await;
    let atlas = mapper.get_current_atlas();
    let keep = atlas.get_area(&area(KEEP)).expect("loaded");
    let view = link_view(
        &atlas,
        &keep,
        link(C_SECRET_DOWN),
        Some(smudgy_cloud::RoomAddress::map(RoomNumber(1))),
    )
    .expect("the link");
    assert_eq!(
        view.from.room.room().map(|room| room.place),
        Some(secret()),
        "from the Secret's own room"
    );
    assert_eq!(link_commands::way_back_shows_more(&view), None);
    let command = link_commands::two_way(&atlas, area(KEEP), &view).expect("two-way");
    let [
        Mutation::AreaBatch {
            area_id,
            operations,
            ..
        },
    ] = command.redo_mutations()
    else {
        panic!("one write: {:?}", command.redo_mutations());
    };
    assert_eq!(*area_id, area(CATACOMBS));
    let [AreaMutation::CreateExit { body, .. }] = &operations[..] else {
        panic!("{operations:?}");
    };
    assert_eq!(
        (body.to_area_id, body.to_room_number),
        (Some(area(SECRET)), Some(RoomNumber(1))),
        "into the Bookshelf's own room, named by the Bookshelf"
    );
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    assert_eq!(stack.take_last_error(), None);
    let atlas = mapper.get_current_atlas();
    let keep = atlas.get_area(&area(KEEP)).expect("loaded");
    assert!(
        link_view(
            &atlas,
            &keep,
            link(C_SECRET_DOWN),
            Some(smudgy_cloud::RoomAddress::map(RoomNumber(1)))
        )
        .expect("the link")
        .two_way()
    );
}

/// A link kept in Private additions into another map stays one-way.
#[tokio::test]
async fn a_private_link_into_another_map_stays_one_way() {
    let mapper = maps().await;
    let ossuary = RoomId {
        map: area(CATACOMBS),
        place: SourceId::Map,
        number: RoomNumber(7),
    };
    let (command, id) = link_commands::create(
        area(KEEP),
        keep_room(3),
        ExitDirection::Down,
        ossuary,
        SourceId::Private,
    )
    .expect("a private link");
    let mut stack = CommandStack::default();
    let _ = stack.push_and_apply(&mapper, command);
    assert_eq!(stack.take_last_error(), None);
    let atlas = mapper.get_current_atlas();
    let keep = atlas.get_area(&area(KEEP)).expect("loaded");
    let view = link_view(&atlas, &keep, id, None).expect("the link");
    assert_eq!(view.place, SourceId::Private);
    assert_eq!(link_commands::way_back_shows_more(&view), Some(ossuary));
    assert!(link_commands::two_way(&atlas, area(KEEP), &view).is_none());
}
