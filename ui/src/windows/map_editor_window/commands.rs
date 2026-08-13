//! The map editor's mutation funnel and undo/redo stack.
//!
//! Every entity mutation the editor performs flows through
//! [`CommandStack::push_and_apply`] as a [`Command`]: a list of redo
//! [`Mutation`]s plus the inverse list captured from the cache *before*
//! applying. Undo/redo replay the appropriate list through the [`Mapper`]
//! (instant cache write, background cloud sync).
//!
//! Entity ids are client-minted before durable enqueue. Mutations reference
//! created entities through [`IdRef::Slot`]: an index into the command's
//! resolved-id table. Deletion commands pre-seed their slots with the
//! original ids, so the first redo targets the existing entity and later
//! redos target whatever the undo most recently recreated.
//!
//! Area create/rename/delete intentionally bypass this stack (not
//! undoable), and the stack is cleared when the edited area changes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use iced::{Task, Vector};
use smudgy_cloud::{
    AreaId, ConnectionArgs, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionRouting,
    ConnectionUpdates, CornerStyle, DEFAULT_CONNECTION_COLOR, DEFAULT_CONNECTION_THICKNESS,
    ExitArgs, ExitDirection, ExitId, ExitUpdates, LabelArgs, LabelId, LabelUpdates, Mapper,
    PortMode, RoomNumber, RoomUpdates, SegmentShape, ShapeArgs, ShapeId, ShapeUpdates, Uuid,
    default_anchor_for_direction,
    mapper::{AreaMutationBatch, AtlasCache, MutationSubmission, RoomKey},
    mutation::{AreaMutation, MAX_MUTATION_OPERATIONS, OperationId},
};
use smudgy_map_widget::map_editor::{EntityId, Selection};

use crate::components::cloud_errors::display_error;

pub type CommandId = u64;
pub type SlotId = usize;

/// How many commands the undo stack retains before dropping the oldest.
const MAX_DEPTH: usize = 100;

/// A reference to an entity id that may not exist yet: either known up
/// front, or the value of a resolved-id slot on the owning command.
#[derive(Debug, Clone, Copy)]
pub enum IdRef<T> {
    Known(T),
    Slot(SlotId),
}

/// A backend-assigned id stored in a command's slot table.
#[derive(Debug, Clone, Copy)]
pub enum ResolvedId {
    Exit(ExitId),
    Label(LabelId),
    Shape(ShapeId),
}

/// One primitive mutation, 1:1 with a [`Mapper`] write.
#[derive(Debug, Clone)]
pub enum Mutation {
    /// One invariant-sensitive gesture sent as one CAS envelope.
    AreaBatch {
        area_id: AreaId,
        operations: Vec<AreaMutation>,
        description: String,
    },
    UpsertRooms(AreaId, Vec<(RoomNumber, RoomUpdates)>),
    DeleteRoom(RoomKey),
    SetRoomProperty(RoomKey, String, String),
    DeleteRoomProperty(RoomKey, String),
    AddRoomTag(RoomKey, String),
    RemoveRoomTag(RoomKey, String),
    SetAreaProperty(AreaId, String, String),
    DeleteAreaProperty(AreaId, String),
    CreateExit {
        room_key: RoomKey,
        args: ExitArgs,
        /// Applied once the create resolves; restores state `ExitArgs`
        /// cannot express (e.g. an explicitly cleared destination on an
        /// undo recreation).
        follow_up: Option<ExitUpdates>,
        slot: SlotId,
    },
    UpdateExit {
        room_key: RoomKey,
        id: IdRef<ExitId>,
        updates: ExitUpdates,
    },
    DeleteExit {
        room_key: RoomKey,
        id: IdRef<ExitId>,
    },
    CreateLabel {
        area_id: AreaId,
        args: LabelArgs,
        slot: SlotId,
    },
    UpdateLabel {
        area_id: AreaId,
        id: IdRef<LabelId>,
        updates: LabelUpdates,
    },
    DeleteLabel {
        area_id: AreaId,
        id: IdRef<LabelId>,
    },
    CreateShape {
        area_id: AreaId,
        args: ShapeArgs,
        slot: SlotId,
    },
    UpdateShape {
        area_id: AreaId,
        id: IdRef<ShapeId>,
        updates: ShapeUpdates,
    },
    DeleteShape {
        area_id: AreaId,
        id: IdRef<ShapeId>,
    },
}

impl Mutation {
    /// The number of slots this mutation requires (max referenced + 1).
    fn slot_requirement(&self) -> usize {
        match self {
            Mutation::CreateExit { slot, .. }
            | Mutation::CreateLabel { slot, .. }
            | Mutation::CreateShape { slot, .. } => slot + 1,
            Mutation::UpdateExit {
                id: IdRef::Slot(slot),
                ..
            }
            | Mutation::DeleteExit {
                id: IdRef::Slot(slot),
                ..
            }
            | Mutation::UpdateLabel {
                id: IdRef::Slot(slot),
                ..
            }
            | Mutation::DeleteLabel {
                id: IdRef::Slot(slot),
                ..
            }
            | Mutation::UpdateShape {
                id: IdRef::Slot(slot),
                ..
            }
            | Mutation::DeleteShape {
                id: IdRef::Slot(slot),
                ..
            } => slot + 1,
            _ => 0,
        }
    }
}

/// The entity a coalescable field edit targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityRef {
    Area(AreaId),
    Room(RoomKey),
    Exit(AreaId, ExitId),
    Connection(AreaId, ConnectionId),
    Label(AreaId, LabelId),
    Shape(AreaId, ShapeId),
}

/// A field on an entity, for coalescing rapid consecutive edits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldId {
    Title,
    Description,
    Level,
    Position,
    Color,
    BackgroundColor,
    Text,
    FontSize,
    FontWeight,
    HorizontalAlignment,
    VerticalAlignment,
    Bounds,
    ShapeType,
    BorderRadius,
    StrokeColor,
    StrokeWidth,
    FromDirection,
    Destination,
    Path,
    Weight,
    Command,
    Flags,
    Routing,
    SegmentShape,
    CornerStyle,
    DashStyle,
    Thickness,
    Endpoint,
    RoutePoints,
    /// A key-value property; the key lives in [`CoalesceKey::detail`].
    Property,
}

/// Edits with equal keys collapse into one undo entry (the first prior
/// state wins, the latest new state wins).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoalesceKey {
    pub entity: EntityRef,
    pub field: FieldId,
    pub detail: Option<String>,
}

impl CoalesceKey {
    pub fn new(entity: EntityRef, field: FieldId) -> Self {
        Self {
            entity,
            field,
            detail: None,
        }
    }

    pub fn with_detail(entity: EntityRef, field: FieldId, detail: impl Into<String>) -> Self {
        Self {
            entity,
            field,
            detail: Some(detail.into()),
        }
    }
}

/// An undoable group of mutations, applied and inverted atomically from
/// the user's point of view.
#[derive(Debug)]
pub struct Command {
    id: CommandId,
    redo: Vec<Mutation>,
    undo: Vec<Mutation>,
    coalesce: Option<CoalesceKey>,
    resolved_ids: Vec<Option<ResolvedId>>,
    pending: usize,
    operation_ids: Vec<OperationId>,
    application_error: Option<String>,
}

impl Command {
    #[must_use]
    pub fn new(redo: Vec<Mutation>, undo: Vec<Mutation>) -> Self {
        let slots = redo
            .iter()
            .chain(undo.iter())
            .map(Mutation::slot_requirement)
            .max()
            .unwrap_or(0);

        Self {
            id: 0,
            redo,
            undo,
            coalesce: None,
            resolved_ids: vec![None; slots],
            pending: 0,
            operation_ids: Vec::new(),
            application_error: None,
        }
    }

    /// Marks this command as a coalescable field edit.
    #[must_use]
    pub fn coalescing(mut self, key: CoalesceKey) -> Self {
        self.coalesce = Some(key);
        self
    }

    /// Seeds a slot with an entity's current id, so slot references work
    /// before any undo has recreated the entity.
    #[must_use]
    pub fn seed_slot(mut self, slot: SlotId, id: ResolvedId) -> Self {
        self.resolved_ids[slot] = Some(id);
        self
    }

    fn exit_id(&self, id: IdRef<ExitId>) -> Option<ExitId> {
        match id {
            IdRef::Known(id) => Some(id),
            IdRef::Slot(slot) => match self.resolved_ids.get(slot)? {
                Some(ResolvedId::Exit(id)) => Some(*id),
                _ => None,
            },
        }
    }

    fn label_id(&self, id: IdRef<LabelId>) -> Option<LabelId> {
        match id {
            IdRef::Known(id) => Some(id),
            IdRef::Slot(slot) => match self.resolved_ids.get(slot)? {
                Some(ResolvedId::Label(id)) => Some(*id),
                _ => None,
            },
        }
    }

    fn shape_id(&self, id: IdRef<ShapeId>) -> Option<ShapeId> {
        match id {
            IdRef::Known(id) => Some(id),
            IdRef::Slot(slot) => match self.resolved_ids.get(slot)? {
                Some(ResolvedId::Shape(id)) => Some(*id),
                _ => None,
            },
        }
    }
}

/// The completion of an asynchronous create issued by a command.
#[derive(Debug, Clone)]
pub enum Outcome {
    Exit {
        command: CommandId,
        slot: SlotId,
        room_key: RoomKey,
        follow_up: Option<ExitUpdates>,
        result: Result<ExitId, String>,
    },
    Label {
        command: CommandId,
        slot: SlotId,
        result: Result<LabelId, String>,
    },
    Shape {
        command: CommandId,
        slot: SlotId,
        result: Result<ShapeId, String>,
    },
}

#[derive(Clone, Copy)]
enum Direction {
    Redo,
    Undo,
}

#[derive(Debug, Default)]
pub struct CommandStack {
    undo: VecDeque<Command>,
    redo: Vec<Command>,
    next_id: CommandId,
    last_error: Option<String>,
}

impl CommandStack {
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.undo.back().is_some_and(|command| command.pending == 0)
    }

    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.redo.last().is_some_and(|command| command.pending == 0)
    }

    /// Drops all history (used when the edited area changes or is deleted,
    /// or when the viewer loses edit access to it).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Whether the stack holds no history in either direction.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.undo.is_empty() && self.redo.is_empty()
    }

    /// Takes the most recent synchronous validation or durable-enqueue error.
    ///
    /// The command stack keeps the error alongside its history decision so
    /// UI callers cannot accidentally record an edit that was never staged.
    pub fn take_last_error(&mut self) -> Option<String> {
        self.last_error.take()
    }

    /// The id assigned to the most recently pushed command (e.g. to match
    /// its create-completion [`Outcome`]s later).
    #[must_use]
    pub fn last_command_id(&self) -> Option<CommandId> {
        self.next_id.checked_sub(1)
    }

    /// Applies a new command's redo mutations and records it for undo.
    /// Clears the redo stack; coalesces into the top entry when keys match.
    #[cfg(test)]
    pub fn push_and_apply(&mut self, mapper: &Mapper, command: Command) -> Task<Outcome> {
        self.push_and_apply_tracked(mapper, command).0
    }

    /// Applies and records a command while returning the CAS operation ids
    /// enqueued by its compound area mutations.
    pub fn push_and_apply_tracked(
        &mut self,
        mapper: &Mapper,
        mut command: Command,
    ) -> (Task<Outcome>, Vec<OperationId>) {
        self.redo.clear();
        self.last_error = None;

        command.id = self.next_id;
        self.next_id += 1;

        let (task, applied) = Self::apply(mapper, &mut command, Direction::Redo);
        if !applied {
            self.last_error = command.application_error.take();
            return (task, Vec::new());
        }
        let operation_ids = command.operation_ids.clone();
        let application_error = command.application_error.take();

        let coalesced = command.coalesce.is_some()
            && command.pending == 0
            && self
                .undo
                .back()
                .is_some_and(|top| top.pending == 0 && top.coalesce == command.coalesce);

        if coalesced {
            if let Some(top) = self.undo.back_mut() {
                // Keep the original prior state; only the latest new state
                // matters for redo.
                top.redo = command.redo;
                top.operation_ids.extend(command.operation_ids);
            }
        } else {
            self.undo.push_back(command);
            if self.undo.len() > MAX_DEPTH {
                self.undo.pop_front();
            }
        }

        self.last_error = application_error;
        (task, operation_ids)
    }

    /// Removes the undo/redo entry that submitted a discarded CAS operation.
    /// This keeps a server-rejected optimistic command from being replayed.
    pub fn discard_operation(&mut self, operation_id: OperationId) -> bool {
        if let Some(position) = self
            .undo
            .iter()
            .position(|command| command.operation_ids.contains(&operation_id))
        {
            self.undo.remove(position);
            return true;
        }
        if let Some(position) = self
            .redo
            .iter()
            .position(|command| command.operation_ids.contains(&operation_id))
        {
            self.redo.remove(position);
            return true;
        }
        false
    }

    pub fn undo(&mut self, mapper: &Mapper) -> Task<Outcome> {
        self.last_error = None;
        if !self.can_undo() {
            return Task::none();
        }
        let Some(mut command) = self.undo.pop_back() else {
            return Task::none();
        };
        let (task, applied) = Self::apply(mapper, &mut command, Direction::Undo);
        self.last_error = command.application_error.take();
        if applied {
            self.redo.push(command);
        } else {
            self.undo.push_back(command);
        }
        task
    }

    pub fn redo(&mut self, mapper: &Mapper) -> Task<Outcome> {
        self.last_error = None;
        if !self.can_redo() {
            return Task::none();
        }
        let Some(mut command) = self.redo.pop() else {
            return Task::none();
        };
        let (task, applied) = Self::apply(mapper, &mut command, Direction::Redo);
        self.last_error = command.application_error.take();
        if applied {
            self.undo.push_back(command);
        } else {
            self.redo.push(command);
        }
        task
    }

    /// Settles the UI completion marker for a synchronously staged create.
    ///
    /// The id slot and operation id are recorded before the command enters
    /// history; this completion only unblocks undo and drives selection.
    pub fn resolve(&mut self, mapper: &Mapper, outcome: Outcome) {
        match outcome {
            Outcome::Exit {
                command,
                slot,
                room_key,
                follow_up,
                result,
            } => {
                let Some(command) = self.find_mut(command) else {
                    return;
                };
                command.pending = command.pending.saturating_sub(1);
                match result {
                    Ok(id) => {
                        command.resolved_ids[slot] = Some(ResolvedId::Exit(id));
                        if let Some(follow_up) = follow_up {
                            let result = mapper.update_exit(room_key, id, follow_up);
                            Self::record_submission(command, result, "exit follow-up update");
                        }
                    }
                    Err(error) => log::warn!("exit create failed: {error}"),
                }
            }
            Outcome::Label {
                command,
                slot,
                result,
            } => {
                let Some(command) = self.find_mut(command) else {
                    return;
                };
                command.pending = command.pending.saturating_sub(1);
                match result {
                    Ok(id) => command.resolved_ids[slot] = Some(ResolvedId::Label(id)),
                    Err(error) => log::warn!("label create failed: {error}"),
                }
            }
            Outcome::Shape {
                command,
                slot,
                result,
            } => {
                let Some(command) = self.find_mut(command) else {
                    return;
                };
                command.pending = command.pending.saturating_sub(1);
                match result {
                    Ok(id) => command.resolved_ids[slot] = Some(ResolvedId::Shape(id)),
                    Err(error) => log::warn!("shape create failed: {error}"),
                }
            }
        }
    }

    fn find_mut(&mut self, id: CommandId) -> Option<&mut Command> {
        self.undo
            .iter_mut()
            .chain(self.redo.iter_mut())
            .find(|command| command.id == id)
    }

    fn record_submission(
        command: &mut Command,
        result: smudgy_cloud::CloudResult<MutationSubmission>,
        context: &str,
    ) -> bool {
        match result {
            Ok(submission) => {
                if let Some(operation_id) = submission.operation_id() {
                    command.operation_ids.push(operation_id);
                }
                true
            }
            Err(error) => {
                let message = format!(
                    "{context} failed validation or durable enqueue: {}",
                    display_error(&error)
                );
                log::warn!("{message}");
                command.application_error = Some(message);
                false
            }
        }
    }

    /// Compiles one direction into a private batch, then durably stages every
    /// envelope before publishing any optimistic state. Create ids are still
    /// client-minted up front, but their completion tasks are emitted only
    /// after the complete gesture commits.
    fn apply(
        mapper: &Mapper,
        command: &mut Command,
        direction: Direction,
    ) -> (Task<Outcome>, bool) {
        command.operation_ids.clear();
        command.application_error = None;
        let mutations = match direction {
            Direction::Redo => command.redo.clone(),
            Direction::Undo => command.undo.clone(),
        };

        let resolved_before = command.resolved_ids.clone();
        let pending_before = command.pending;
        let mut tasks = Vec::new();
        let mut batches = Vec::new();

        for mutation in mutations {
            match mutation {
                Mutation::AreaBatch {
                    area_id,
                    operations,
                    description,
                } => batches.push(AreaMutationBatch::strict(area_id, operations, description)),
                Mutation::UpsertRooms(area_id, updates) => {
                    let description = if updates.len() == 1 {
                        format!("Update room {}", updates[0].0)
                    } else {
                        format!("Update {} rooms", updates.len())
                    };
                    let mut operations: Vec<_> = updates
                        .into_iter()
                        .map(|(room_number, body)| AreaMutation::UpsertRoom { room_number, body })
                        .collect();
                    while operations.len() > MAX_MUTATION_OPERATIONS {
                        let rest = operations.split_off(MAX_MUTATION_OPERATIONS);
                        batches.push(AreaMutationBatch::strict(
                            area_id,
                            operations,
                            description.clone(),
                        ));
                        operations = rest;
                    }
                    batches.push(AreaMutationBatch::strict(area_id, operations, description));
                }
                Mutation::DeleteRoom(room_key) => {
                    batches.push(AreaMutationBatch::strict(
                        room_key.area_id,
                        vec![AreaMutation::DeleteRoom {
                            room_number: room_key.room_number,
                        }],
                        format!("Delete room {}", room_key.room_number),
                    ));
                }
                Mutation::SetRoomProperty(room_key, name, value) => {
                    let description =
                        format!("Set property {name} on room {}", room_key.room_number);
                    batches.push(AreaMutationBatch::strict(
                        room_key.area_id,
                        vec![AreaMutation::UpsertRoomProperty {
                            room_number: room_key.room_number,
                            name,
                            value,
                            is_secret: None,
                        }],
                        description,
                    ));
                }
                Mutation::DeleteRoomProperty(room_key, name) => {
                    let description =
                        format!("Delete property {name} on room {}", room_key.room_number);
                    batches.push(AreaMutationBatch::strict(
                        room_key.area_id,
                        vec![AreaMutation::DeleteRoomProperty {
                            room_number: room_key.room_number,
                            name,
                        }],
                        description,
                    ));
                }
                Mutation::AddRoomTag(room_key, tag) => {
                    batches.push(AreaMutationBatch::strict(
                        room_key.area_id,
                        vec![AreaMutation::AddRoomTag {
                            room_number: room_key.room_number,
                            tag,
                        }],
                        format!("Add tag to room {}", room_key.room_number),
                    ));
                }
                Mutation::RemoveRoomTag(room_key, tag) => {
                    batches.push(AreaMutationBatch::strict(
                        room_key.area_id,
                        vec![AreaMutation::RemoveRoomTag {
                            room_number: room_key.room_number,
                            tag,
                        }],
                        format!("Remove tag from room {}", room_key.room_number),
                    ));
                }
                Mutation::SetAreaProperty(area_id, name, value) => {
                    let description = format!("Set area property {name}");
                    batches.push(AreaMutationBatch::strict(
                        area_id,
                        vec![AreaMutation::UpsertAreaProperty {
                            name,
                            value,
                            is_secret: None,
                        }],
                        description,
                    ));
                }
                Mutation::DeleteAreaProperty(area_id, name) => {
                    let description = format!("Delete area property {name}");
                    batches.push(AreaMutationBatch::strict(
                        area_id,
                        vec![AreaMutation::DeleteAreaProperty { name }],
                        description,
                    ));
                }
                Mutation::CreateExit {
                    room_key,
                    mut args,
                    follow_up,
                    slot,
                } => {
                    let command_id = command.id;
                    let id = args.id.unwrap_or_else(ExitId::new);
                    args.id = Some(id);
                    batches.push(AreaMutationBatch::strict(
                        room_key.area_id,
                        vec![AreaMutation::CreateExit {
                            room_number: room_key.room_number,
                            body: args,
                        }],
                        format!("Create exit from room {}", room_key.room_number),
                    ));
                    if let Some(follow_up) = follow_up {
                        batches.push(AreaMutationBatch::splitting_paired_exit(
                            room_key.area_id,
                            vec![AreaMutation::UpdateExit {
                                exit_id: id,
                                body: follow_up,
                            }],
                            "Restore exit details",
                        ));
                    }
                    command.resolved_ids[slot] = Some(ResolvedId::Exit(id));
                    command.pending += 1;
                    tasks.push(Task::done(Outcome::Exit {
                        command: command_id,
                        slot,
                        room_key,
                        follow_up: None,
                        result: Ok(id),
                    }));
                }
                Mutation::UpdateExit {
                    room_key,
                    id,
                    updates,
                } => {
                    if let Some(exit_id) = command.exit_id(id) {
                        batches.push(AreaMutationBatch::splitting_paired_exit(
                            room_key.area_id,
                            vec![AreaMutation::UpdateExit {
                                exit_id,
                                body: updates,
                            }],
                            "Update exit",
                        ));
                    }
                }
                Mutation::DeleteExit { room_key, id } => {
                    if let Some(exit_id) = command.exit_id(id) {
                        batches.push(AreaMutationBatch::strict(
                            room_key.area_id,
                            vec![AreaMutation::DeleteExit { exit_id }],
                            "Delete exit",
                        ));
                    }
                }
                Mutation::CreateLabel {
                    area_id,
                    mut args,
                    slot,
                } => {
                    let command_id = command.id;
                    let id = args.id.unwrap_or_else(|| LabelId(Uuid::new_v4()));
                    args.id = Some(id);
                    batches.push(AreaMutationBatch::strict(
                        area_id,
                        vec![AreaMutation::CreateLabel { body: args }],
                        "Create label",
                    ));
                    command.resolved_ids[slot] = Some(ResolvedId::Label(id));
                    command.pending += 1;
                    tasks.push(Task::done(Outcome::Label {
                        command: command_id,
                        slot,
                        result: Ok(id),
                    }));
                }
                Mutation::UpdateLabel {
                    area_id,
                    id,
                    updates,
                } => {
                    if let Some(label_id) = command.label_id(id) {
                        batches.push(AreaMutationBatch::strict(
                            area_id,
                            vec![AreaMutation::UpdateLabel {
                                label_id,
                                body: updates,
                            }],
                            "Update label",
                        ));
                    }
                }
                Mutation::DeleteLabel { area_id, id } => {
                    if let Some(label_id) = command.label_id(id) {
                        batches.push(AreaMutationBatch::strict(
                            area_id,
                            vec![AreaMutation::DeleteLabel { label_id }],
                            "Delete label",
                        ));
                    }
                }
                Mutation::CreateShape {
                    area_id,
                    mut args,
                    slot,
                } => {
                    let command_id = command.id;
                    let id = args.id.unwrap_or_else(|| ShapeId(Uuid::new_v4()));
                    args.id = Some(id);
                    batches.push(AreaMutationBatch::strict(
                        area_id,
                        vec![AreaMutation::CreateShape { body: args }],
                        "Create shape",
                    ));
                    command.resolved_ids[slot] = Some(ResolvedId::Shape(id));
                    command.pending += 1;
                    tasks.push(Task::done(Outcome::Shape {
                        command: command_id,
                        slot,
                        result: Ok(id),
                    }));
                }
                Mutation::UpdateShape {
                    area_id,
                    id,
                    updates,
                } => {
                    if let Some(shape_id) = command.shape_id(id) {
                        batches.push(AreaMutationBatch::strict(
                            area_id,
                            vec![AreaMutation::UpdateShape {
                                shape_id,
                                body: updates,
                            }],
                            "Update shape",
                        ));
                    }
                }
                Mutation::DeleteShape { area_id, id } => {
                    if let Some(shape_id) = command.shape_id(id) {
                        batches.push(AreaMutationBatch::strict(
                            area_id,
                            vec![AreaMutation::DeleteShape { shape_id }],
                            "Delete shape",
                        ));
                    }
                }
            }
        }

        match mapper.mutate_batches(batches) {
            Ok(submissions) => {
                command.operation_ids.extend(
                    submissions
                        .into_iter()
                        .filter_map(MutationSubmission::operation_id),
                );
                (Task::batch(tasks), true)
            }
            Err(error) => {
                command.resolved_ids = resolved_before;
                command.pending = pending_before;
                let message = format!(
                    "map gesture failed validation or durable enqueue: {}",
                    display_error(&error)
                );
                log::warn!("{message}");
                command.application_error = Some(message);
                (Task::none(), false)
            }
        }
    }
}

// ===== Command builders =====
//
// Builders read the *current* cache snapshot to capture inverse state, so
// they must run before the command is applied.

/// Moves every selected entity by a map-space offset.
#[must_use]
pub fn move_selection(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    selection: &Selection,
    offset: Vector,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;

    let mut room_redo = Vec::new();
    let mut room_undo = Vec::new();
    let mut redo = Vec::new();
    let mut undo = Vec::new();

    for room_number in selection.rooms() {
        let Some(room) = area.get_room(&room_number) else {
            continue;
        };
        room_redo.push((
            room_number,
            RoomUpdates {
                x: Some(room.get_x() + offset.x),
                y: Some(room.get_y() + offset.y),
                ..Default::default()
            },
        ));
        room_undo.push((
            room_number,
            RoomUpdates {
                x: Some(room.get_x()),
                y: Some(room.get_y()),
                ..Default::default()
            },
        ));
    }

    if !room_redo.is_empty() {
        redo.push(Mutation::UpsertRooms(area_id, room_redo));
        undo.push(Mutation::UpsertRooms(area_id, room_undo));
    }

    for label_id in selection.labels() {
        let Some(label) = area.get_label(&label_id) else {
            continue;
        };
        redo.push(Mutation::UpdateLabel {
            area_id,
            id: IdRef::Known(label_id),
            updates: LabelUpdates {
                x: Some(label.x + offset.x),
                y: Some(label.y + offset.y),
                ..Default::default()
            },
        });
        undo.push(Mutation::UpdateLabel {
            area_id,
            id: IdRef::Known(label_id),
            updates: LabelUpdates {
                x: Some(label.x),
                y: Some(label.y),
                ..Default::default()
            },
        });
    }

    for shape_id in selection.shapes() {
        let Some(shape) = area.get_shape(&shape_id) else {
            continue;
        };
        redo.push(Mutation::UpdateShape {
            area_id,
            id: IdRef::Known(shape_id),
            updates: ShapeUpdates {
                x: Some(shape.x + offset.x),
                y: Some(shape.y + offset.y),
                ..Default::default()
            },
        });
        undo.push(Mutation::UpdateShape {
            area_id,
            id: IdRef::Known(shape_id),
            updates: ShapeUpdates {
                x: Some(shape.x),
                y: Some(shape.y),
                ..Default::default()
            },
        });
    }

    if redo.is_empty() {
        None
    } else {
        Some(Command::new(redo, undo))
    }
}

/// Deletes every selected entity. Undo restores rooms with their
/// properties and outgoing exits, and recreates labels/shapes (with fresh
/// backend ids, re-tracked through slots).
#[must_use]
pub fn delete_selection(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    selection: &Selection,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    // Secrecy flags are restorable only when the viewer may send them; a
    // non-cleared viewer's projection has no secret entities anyway.
    let cleared = area.effective_access().is_cleared_for_secrets();

    let mut redo = Vec::new();
    let mut undo_rooms = Vec::new();
    let mut undo_late = Vec::new();
    let mut seeds = Vec::new();
    let mut next_slot: SlotId = 0;

    // Explicitly selected Connections delete as links in their own right —
    // except those whose every member exit rides a selected room's cascade
    // delete, which stay on the room path exactly as before (their exits
    // are restored by the room undo and a fresh link derives). The link
    // deletes run before the room deletes (a cascaded-away link can't be
    // deleted twice) and their restores run after every room is back.
    let selected_room_set: HashSet<RoomNumber> = selection.rooms().collect();
    let mut restored_connections: HashSet<ConnectionId> = HashSet::new();
    let mut link_deletes = Vec::new();
    let mut link_restores = Vec::new();
    let mut selected_connections: Vec<ConnectionId> = selection.connections().collect();
    selected_connections.sort();
    for connection_id in selected_connections {
        let Some(connection) = area.get_connection(connection_id) else {
            continue;
        };
        let mut members = Vec::new();
        for room in area.get_rooms() {
            for exit in room.get_exits() {
                if exit.connection_id == connection_id {
                    members.push((room.get_room_number(), exit));
                }
            }
        }
        if !members.is_empty()
            && members
                .iter()
                .all(|(room_number, _)| selected_room_set.contains(room_number))
        {
            continue;
        }
        members.sort_by_key(|(_, exit)| exit.id.0);
        restored_connections.insert(connection_id);
        link_deletes.push(AreaMutation::DeleteLink { connection_id });
        let mut restore = vec![AreaMutation::CreateConnection {
            body: ConnectionArgs::from(connection),
        }];
        for (room_number, exit) in &members {
            restore.push(AreaMutation::CreateExit {
                room_number: *room_number,
                body: restore_exit_args(exit, connection_id, cleared),
            });
        }
        link_restores.push(Mutation::AreaBatch {
            area_id,
            operations: restore,
            description: "Restore deleted link".to_string(),
        });
    }
    if !link_deletes.is_empty() {
        redo.push(Mutation::AreaBatch {
            area_id,
            operations: link_deletes,
            description: "Delete links".to_string(),
        });
    }

    for room_number in selection.rooms() {
        let Some(room) = area.get_room(&room_number) else {
            continue;
        };
        let room_key = RoomKey::new(area_id, room_number);

        redo.push(Mutation::DeleteRoom(room_key.clone()));

        undo_rooms.push((
            room_number,
            RoomUpdates {
                is_secret: cleared.then_some(room.is_secret()),
                title: Some(room.get_title().to_string()),
                description: Some(room.get_description().to_string()),
                level: Some(room.get_level()),
                x: Some(room.get_x()),
                y: Some(room.get_y()),
                color: Some(room.get_color().to_string()),
                external_id: room.get_external_id().map(|id| Some(id.to_string())),
            },
        ));

        // KNOWN GAP: the property PUT body has no secrecy channel, so a
        // property that was secret-marked is restored as *public* — re-marking
        // it needs a separate POST /secret-marks the undo stack can't express
        // today. The room/exit/label/shape is_secret flags ARE restored.
        let mut properties: Vec<(String, String)> = room
            .properties()
            .map(|(name, value)| (name.to_string(), value.to_string()))
            .collect();
        properties.sort();
        for (name, value) in properties {
            undo_late.push(Mutation::SetRoomProperty(room_key.clone(), name, value));
        }

        for exit in room.get_exits() {
            // This exit's whole link is being deleted and restored (with
            // its identities) by the explicit-connection path above; a
            // second recreation here would duplicate it.
            if restored_connections.contains(&exit.connection_id) {
                continue;
            }
            if exit.to_unknown {
                // The destination was redacted ("Unknown map") and is
                // unknowable client-side, but the room delete cascades the
                // exit anyway. Undo recreates it DANGLING (args carry
                // to_* = None): the cross-area link is lost at delete time
                // and cannot be restored from here.
                log::warn!(
                    "map editor: deleting room {room_number} discards an exit to an \
                     unshared map; undo will recreate it without its destination"
                );
            }
            let slot = next_slot;
            next_slot += 1;
            seeds.push((slot, ResolvedId::Exit(exit.id)));

            undo_late.push(Mutation::CreateExit {
                room_key: room_key.clone(),
                args: exit_args_from_cache(exit, cleared),
                follow_up: Some(exit_updates_from_cache(exit)),
                slot,
            });
        }
    }

    // Deleting a room nulls the destination of every exit that pointed at it
    // (the server cascades this and `Mapper::delete_room` mirrors it in the
    // cache). Capture an `UpdateExit` restore for each such inbound exit so
    // undo re-links it. Exits hosted by a room that is *also* being deleted
    // are restored by that room's own exit recreation above, so they are
    // skipped here.
    let deleted_rooms: HashSet<RoomKey> = selection
        .rooms()
        .map(|room_number| RoomKey::new(area_id, room_number))
        .collect();
    for host_area in atlas.areas() {
        let host_area_id = *host_area.get_id();
        for host_room in host_area.get_rooms() {
            let host_key = RoomKey::new(host_area_id, host_room.get_room_number());
            if deleted_rooms.contains(&host_key) {
                continue;
            }
            for exit in host_room.get_exits() {
                // Members of an explicitly deleted link don't survive the
                // delete at all — their restore (with destination) rides the
                // link's own CreateExit batch. A relink here would enqueue
                // an UpdateExit for an exit that no longer exists, wedging
                // the sync queue on ExitNotFound.
                if restored_connections.contains(&exit.connection_id) {
                    continue;
                }
                let (Some(to_area_id), Some(to_room_number)) =
                    (exit.to_area_id, exit.to_room_number)
                else {
                    continue;
                };
                if deleted_rooms.contains(&RoomKey::new(to_area_id, to_room_number)) {
                    undo_late.push(Mutation::UpdateExit {
                        room_key: host_key.clone(),
                        id: IdRef::Known(exit.id),
                        updates: exit_updates_from_cache(exit),
                    });
                }
            }
        }
    }

    for label_id in selection.labels() {
        let Some(label) = area.get_label(&label_id) else {
            continue;
        };
        let slot = next_slot;
        next_slot += 1;
        seeds.push((slot, ResolvedId::Label(label_id)));

        redo.push(Mutation::DeleteLabel {
            area_id,
            id: IdRef::Slot(slot),
        });
        undo_late.push(Mutation::CreateLabel {
            area_id,
            args: LabelArgs {
                // Recreation mints a fresh identity at apply time.
                id: None,
                is_secret: cleared.then_some(label.is_secret),
                level: label.level,
                x: label.x,
                y: label.y,
                width: label.width,
                height: label.height,
                horizontal_alignment: label.horizontal_alignment.clone(),
                vertical_alignment: label.vertical_alignment.clone(),
                text: label.text.clone(),
                color: label.color.clone(),
                // Always explicit — `Some("")` means transparent, while an
                // absent value invites server-side creation defaults.
                background_color: Some(label.background_color.clone()),
                font_size: label.font_size,
                font_weight: label.font_weight,
            },
            slot,
        });
    }

    for shape_id in selection.shapes() {
        let Some(shape) = area.get_shape(&shape_id) else {
            continue;
        };
        let slot = next_slot;
        next_slot += 1;
        seeds.push((slot, ResolvedId::Shape(shape_id)));

        redo.push(Mutation::DeleteShape {
            area_id,
            id: IdRef::Slot(slot),
        });
        undo_late.push(Mutation::CreateShape {
            area_id,
            args: ShapeArgs {
                // Recreation mints a fresh identity at apply time.
                id: None,
                is_secret: cleared.then_some(shape.is_secret),
                level: shape.level,
                x: shape.x,
                y: shape.y,
                width: shape.width,
                height: shape.height,
                // Always explicit — `Some("")` means no fill/stroke, while
                // an absent value invites server-side creation defaults.
                background_color: Some(shape.background_color.clone().unwrap_or_default()),
                stroke_color: Some(shape.stroke_color.clone().unwrap_or_default()),
                shape_type: shape.shape_type.clone(),
                border_radius: shape.border_radius,
                stroke_width: Some(shape.stroke_width),
            },
            slot,
        });
    }

    if redo.is_empty() {
        return None;
    }

    // Rooms must exist again before their properties and exits restore,
    // and both before explicitly-deleted links reattach to them.
    let mut undo = Vec::new();
    if !undo_rooms.is_empty() {
        undo.push(Mutation::UpsertRooms(area_id, undo_rooms));
    }
    undo.extend(undo_late);
    undo.extend(link_restores);

    let mut command = Command::new(redo, undo);
    for (slot, id) in seeds {
        command = command.seed_slot(slot, id);
    }
    Some(command)
}

/// `ExitArgs` recreating a cached exit (everything `ExitArgs` can express).
/// `restore_secrecy` carries the cached `is_secret` flag into the create
/// body — pass it only when the viewer is cleared for secrets (the server
/// uniform-404s the field otherwise); recreation then defaults to public,
/// which is the most the viewer's projection can know.
fn exit_args_from_cache(
    exit: &smudgy_cloud::mapper::exit_cache::ExitCache,
    restore_secrecy: bool,
) -> ExitArgs {
    ExitArgs {
        // Recreation mints a fresh identity at apply time.
        id: None,
        connection_id: None,
        new_connection_id: None,
        is_secret: restore_secrecy.then_some(exit.is_secret),
        from_direction: exit.from_direction,
        to_area_id: exit.to_area_id,
        to_room_number: exit.to_room_number,
        to_direction: exit.to_direction,
        path: exit.path.clone(),
        is_hidden: exit.is_hidden,
        is_closed: exit.is_closed,
        is_locked: exit.is_locked,
        weight: exit.weight,
        command: exit.command.clone(),
    }
}

/// A full-field `ExitUpdates` snapshot of a cached exit.
///
/// `ExitUpdates::apply` and the backend MERGE the destination fields
/// (`None`/omitted means "unchanged"); the only way to null a destination
/// is `clear_to`. A faithful snapshot of a destination-less exit must
/// therefore carry `clear_to: Some(true)`, or replaying it would silently
/// keep whatever destination is current. Redacted destinations
/// (`to_unknown`) are left untouched: the server still holds the real
/// link, and `clear_to` would destroy it.
fn exit_updates_from_cache(exit: &smudgy_cloud::mapper::exit_cache::ExitCache) -> ExitUpdates {
    let destination_empty = exit.to_area_id.is_none()
        && exit.to_room_number.is_none()
        && exit.to_direction.is_none()
        && !exit.to_unknown;
    ExitUpdates {
        is_secret: None,
        clear_to: destination_empty.then_some(true),
        from_direction: Some(exit.from_direction),
        to_area_id: exit.to_area_id,
        to_room_number: exit.to_room_number,
        to_direction: exit.to_direction,
        path: exit.path.clone(),
        is_hidden: Some(exit.is_hidden),
        is_closed: Some(exit.is_closed),
        is_locked: Some(exit.is_locked),
        weight: Some(exit.weight),
        command: exit.command.clone(),
    }
}

/// Where a new exit should land.
#[derive(Debug, Clone, Copy)]
pub enum NewExitTarget {
    /// An existing room.
    Room(RoomNumber),
    /// A new room created at this position/level as part of the command.
    NewRoom {
        room_number: RoomNumber,
        at: iced::Point,
        level: i32,
    },
    /// An outbound traversal with no destination room.
    Dangling,
}

/// Editable values from the Link-tool confirmation popover.
#[derive(Debug, Clone)]
pub struct NewLinkOptions {
    pub one_way: bool,
    pub from_command: Option<String>,
    pub to_command: Option<String>,
    pub routing: ConnectionRouting,
    pub dash: ConnectionDash,
    pub color: String,
    pub thickness: f32,
    /// When present, add the reciprocal traversal to this existing
    /// one-member Connection instead of creating a second visual route.
    pub pair_with: Option<ConnectionId>,
}

impl Default for NewLinkOptions {
    fn default() -> Self {
        Self {
            one_way: false,
            from_command: None,
            to_command: None,
            routing: ConnectionRouting::Simple,
            dash: ConnectionDash::Solid,
            color: DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: DEFAULT_CONNECTION_THICKNESS,
            pair_with: None,
        }
    }
}

/// Creates a Link-tool draft as one compound mutation. IDs are allocated
/// before enqueue so room + Connection + traversal creation is atomic and
/// retry-safe.
#[must_use]
pub fn create_exit_with_options(
    area_id: AreaId,
    from: RoomNumber,
    from_direction: smudgy_cloud::ExitDirection,
    to: &NewExitTarget,
    to_direction: smudgy_cloud::ExitDirection,
    options: NewLinkOptions,
) -> Command {
    let connection_id = ConnectionId::new();
    let forward_id = ExitId::new();
    let dangling = matches!(to, NewExitTarget::Dangling);
    let one_way = options.one_way || dangling || options.pair_with.is_some();
    let reverse_id = (!one_way).then(ExitId::new);
    let mut operations = Vec::new();
    let to_room = match *to {
        NewExitTarget::Room(room_number) => Some(room_number),
        NewExitTarget::NewRoom {
            room_number,
            at,
            level,
        } => {
            operations.push(AreaMutation::UpsertRoom {
                room_number,
                body: RoomUpdates {
                    is_secret: None,
                    title: Some(String::new()),
                    description: Some(String::new()),
                    level: Some(level),
                    x: Some(at.x),
                    y: Some(at.y),
                    color: Some(String::new()),
                    external_id: None,
                },
            });
            Some(room_number)
        }
        NewExitTarget::Dangling => None,
    };
    if options.pair_with.is_none() {
        let (from_side, from_offset) = default_anchor_for_direction(from_direction, None);
        let mut endpoint_a = ConnectionEndpoint {
            room_number: from,
            side: from_side,
            port_offset: from_offset,
            port_mode: PortMode::AutoPinned,
        };
        let mut endpoint_b = to_room.map(|to_room| {
            let (to_side, to_offset) = default_anchor_for_direction(to_direction, None);
            ConnectionEndpoint {
                room_number: to_room,
                side: to_side,
                port_offset: to_offset,
                port_mode: PortMode::AutoPinned,
            }
        });
        if endpoint_b.is_some_and(|endpoint| endpoint_a.room_number > endpoint.room_number) {
            std::mem::swap(
                &mut endpoint_a,
                endpoint_b.as_mut().expect("checked endpoint B"),
            );
        }
        operations.push(AreaMutation::CreateConnection {
            body: ConnectionArgs {
                id: connection_id,
                endpoint_a,
                endpoint_b,
                routing: options.routing,
                segment_shape: SegmentShape::Direct,
                corner: CornerStyle::Sharp,
                route_points: Vec::new(),
                dash: options.dash,
                color: options.color.clone(),
                thickness: options.thickness,
            },
        });
    }
    let attached_connection_id = options.pair_with.unwrap_or(connection_id);
    operations.push(AreaMutation::CreateExit {
        room_number: from,
        body: ExitArgs {
            id: Some(forward_id),
            connection_id: Some(attached_connection_id),
            from_direction,
            to_area_id: to_room.map(|_| area_id),
            to_room_number: to_room,
            to_direction: to_room.map(|_| to_direction),
            command: options.from_command.clone(),
            weight: 1.0,
            ..Default::default()
        },
    });
    if let Some(reverse_id) = reverse_id {
        operations.push(AreaMutation::CreateExit {
            room_number: to_room.expect("a bidirectional link has a destination room"),
            body: ExitArgs {
                id: Some(reverse_id),
                connection_id: Some(connection_id),
                from_direction: to_direction,
                to_area_id: Some(area_id),
                to_room_number: Some(from),
                to_direction: Some(from_direction),
                command: options.to_command.clone(),
                weight: 1.0,
                ..Default::default()
            },
        });
    }

    let intent = if options.pair_with.is_some() {
        "Pair reciprocal traversal".to_string()
    } else {
        match (to, one_way) {
            (NewExitTarget::NewRoom { room_number, .. }, false) => {
                format!("Create room {room_number} and bidirectional link")
            }
            (NewExitTarget::NewRoom { room_number, .. }, true) => {
                format!("Create room {room_number} and one-way link")
            }
            (_, false) => "Create bidirectional link".to_string(),
            (_, true) => "Create one-way link".to_string(),
        }
    };
    let redo = Mutation::AreaBatch {
        area_id,
        operations,
        description: intent,
    };
    let mut inverse = if options.pair_with.is_some() {
        vec![AreaMutation::DeleteExit {
            exit_id: forward_id,
        }]
    } else {
        vec![AreaMutation::DeleteLink { connection_id }]
    };
    if let NewExitTarget::NewRoom { room_number, .. } = to {
        inverse.push(AreaMutation::DeleteRoom {
            room_number: *room_number,
        });
    }
    let undo = Mutation::AreaBatch {
        area_id,
        operations: inverse,
        description: "Undo link creation".to_string(),
    };
    Command::new(vec![redo], vec![undo])
}

/// Edits shared Connection geometry/appearance through one semantic
/// envelope and captures exactly the touched fields for undo.
#[must_use]
pub fn edit_connection(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    connection_id: ConnectionId,
    field: FieldId,
    updates: ConnectionUpdates,
    description: impl Into<String>,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let current = area.get_connection(connection_id)?;
    // `ConnectionUpdates` deliberately cannot clear endpoint B (topology
    // changes travel through the semantic link operations), so an edit that
    // would *set* it on a connection without one has no expressible inverse.
    // Refuse it rather than record an undo that silently keeps the endpoint.
    if updates.endpoint_b.is_some() && current.endpoint_b.is_none() {
        return None;
    }
    let inverse = ConnectionUpdates {
        endpoint_a: updates.endpoint_a.map(|_| current.endpoint_a),
        endpoint_b: updates.endpoint_b.and(current.endpoint_b),
        routing: updates.routing.map(|_| current.routing),
        segment_shape: updates.segment_shape.map(|_| current.segment_shape),
        corner: updates.corner.map(|_| current.corner),
        route_points: updates
            .route_points
            .as_ref()
            .map(|_| current.route_points.clone()),
        dash: updates.dash.map(|_| current.dash),
        color: updates.color.as_ref().map(|_| current.color.clone()),
        thickness: updates.thickness.map(|_| current.thickness),
    };
    let description = description.into();
    // Coalescing keeps the first command's undo and the last redo, which
    // only inverts correctly when every merged command touches the same
    // fields. Endpoint edits carry exactly one endpoint, so edits to
    // different endpoints must not merge — key them apart.
    let key = match (updates.endpoint_a.is_some(), updates.endpoint_b.is_some()) {
        (true, false) => CoalesceKey::with_detail(
            EntityRef::Connection(area_id, connection_id),
            field,
            "endpoint-a",
        ),
        (false, true) => CoalesceKey::with_detail(
            EntityRef::Connection(area_id, connection_id),
            field,
            "endpoint-b",
        ),
        _ => CoalesceKey::new(EntityRef::Connection(area_id, connection_id), field),
    };
    Some(
        Command::new(
            vec![Mutation::AreaBatch {
                area_id,
                operations: vec![AreaMutation::UpdateConnection {
                    connection_id,
                    body: updates,
                }],
                description: description.clone(),
            }],
            vec![Mutation::AreaBatch {
                area_id,
                operations: vec![AreaMutation::UpdateConnection {
                    connection_id,
                    body: inverse,
                }],
                description: format!("Undo {description}"),
            }],
        )
        .coalescing(key),
    )
}

/// Commits one accepted solver preview as exactly one undoable area CAS
/// mutation. Keeping this semantic operation named makes it difficult for UI
/// changes to accidentally persist the mode and points separately.
#[must_use]
pub fn accept_automatic_route(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    connection_id: ConnectionId,
    route_points: Vec<smudgy_cloud::MapPoint>,
) -> Option<Command> {
    edit_connection(
        atlas,
        area_id,
        connection_id,
        FieldId::RoutePoints,
        ConnectionUpdates {
            routing: Some(ConnectionRouting::Automatic),
            segment_shape: Some(SegmentShape::Orthogonal),
            route_points: Some(route_points),
            ..ConnectionUpdates::default()
        },
        "Accept automatic route",
    )
}

/// Applies a previewed group of Connection edits as one undoable area
/// mutation. Wall-port redistribution uses this so every affected endpoint
/// and orthogonal elbow moves in one CAS envelope.
#[must_use]
pub fn edit_connections(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    edits: Vec<(ConnectionId, ConnectionUpdates)>,
    description: impl Into<String>,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    if edits.is_empty() {
        return None;
    }
    let mut redo = Vec::with_capacity(edits.len());
    let mut undo = Vec::with_capacity(edits.len());
    for (connection_id, updates) in edits {
        let current = area.get_connection(connection_id)?;
        // Same endpoint-B inverse rule as `edit_connection` above.
        if updates.endpoint_b.is_some() && current.endpoint_b.is_none() {
            return None;
        }
        let inverse = ConnectionUpdates {
            endpoint_a: updates.endpoint_a.map(|_| current.endpoint_a),
            endpoint_b: updates.endpoint_b.and(current.endpoint_b),
            routing: updates.routing.map(|_| current.routing),
            segment_shape: updates.segment_shape.map(|_| current.segment_shape),
            corner: updates.corner.map(|_| current.corner),
            route_points: updates
                .route_points
                .as_ref()
                .map(|_| current.route_points.clone()),
            dash: updates.dash.map(|_| current.dash),
            color: updates.color.as_ref().map(|_| current.color.clone()),
            thickness: updates.thickness.map(|_| current.thickness),
        };
        redo.push(AreaMutation::UpdateConnection {
            connection_id,
            body: updates,
        });
        undo.push(AreaMutation::UpdateConnection {
            connection_id,
            body: inverse,
        });
    }
    let description = description.into();
    Some(Command::new(
        vec![Mutation::AreaBatch {
            area_id,
            operations: redo,
            description: description.clone(),
        }],
        vec![Mutation::AreaBatch {
            area_id,
            operations: undo,
            description: format!("Undo {description}"),
        }],
    ))
}

#[must_use]
pub fn delete_waypoint(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    connection_id: ConnectionId,
    index: usize,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let connection = area.get_connection(connection_id)?;
    if index >= connection.route_points.len() {
        return None;
    }
    let mut points = connection.route_points.clone();
    points.remove(index);
    if connection.segment_shape == SegmentShape::Orthogonal
        && matches!(
            connection.routing,
            ConnectionRouting::Manual | ConnectionRouting::Automatic
        )
    {
        let render = area.get_room_connections().iter().find(|render| {
            render.connection_id == connection_id && render.geometry.stub_tip_b.is_some()
        })?;
        points = smudgy_cloud::connection_geometry::orthogonalize_route(
            render.geometry.stub_tip_a,
            &points,
            render.geometry.stub_tip_b?,
        )?;
    }
    edit_connection(
        atlas,
        area_id,
        connection_id,
        FieldId::RoutePoints,
        ConnectionUpdates {
            routing: Some(ConnectionRouting::Manual),
            route_points: Some(points),
            ..ConnectionUpdates::default()
        },
        "Delete connection waypoint",
    )
}

fn restore_exit_args(
    exit: &smudgy_cloud::mapper::exit_cache::ExitCache,
    connection_id: ConnectionId,
    restore_secrecy: bool,
) -> ExitArgs {
    ExitArgs {
        id: Some(exit.id),
        connection_id: Some(connection_id),
        new_connection_id: None,
        is_secret: restore_secrecy.then_some(exit.is_secret),
        from_direction: exit.from_direction,
        to_area_id: exit.to_area_id,
        to_room_number: exit.to_room_number,
        to_direction: exit.to_direction,
        path: exit.path.clone(),
        is_hidden: exit.is_hidden,
        is_closed: exit.is_closed,
        is_locked: exit.is_locked,
        weight: exit.weight,
        command: exit.command.clone(),
    }
}

/// Delete a selected visual link and every traversal it owns. Undo restores
/// the same stable Connection and Exit identities in one envelope.
#[must_use]
pub fn delete_connection(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    connection_id: ConnectionId,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let connection = area.get_connection(connection_id)?;
    let cleared = area.effective_access().is_cleared_for_secrets();
    let mut restore = vec![AreaMutation::CreateConnection {
        body: ConnectionArgs::from(connection),
    }];
    let mut members = Vec::new();
    for room in area.get_rooms() {
        for exit in room.get_exits() {
            if exit.connection_id == connection_id {
                members.push((room.get_room_number(), exit));
            }
        }
    }
    members.sort_by_key(|(_, exit)| exit.id.0);
    for (room_number, exit) in &members {
        restore.push(AreaMutation::CreateExit {
            room_number: *room_number,
            body: restore_exit_args(exit, connection_id, cleared),
        });
    }
    Some(Command::new(
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![AreaMutation::DeleteLink { connection_id }],
            description: if members.len() == 2 {
                "Delete bidirectional link".to_string()
            } else {
                "Delete link".to_string()
            },
        }],
        vec![Mutation::AreaBatch {
            area_id,
            operations: restore,
            description: "Restore deleted link".to_string(),
        }],
    ))
}

#[must_use]
pub fn unlink_exit(area_id: AreaId, exit_id: ExitId, old_connection_id: ConnectionId) -> Command {
    let new_connection_id = ConnectionId::new();
    Command::new(
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![AreaMutation::Unlink {
                exit_id,
                new_connection_id,
            }],
            description: "Unlink selected direction".to_string(),
        }],
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![AreaMutation::Pair {
                keep_connection_id: old_connection_id,
                merge_connection_id: new_connection_id,
            }],
            description: "Restore linked directions".to_string(),
        }],
    )
}

/// Makes a one-way link two-way: creates the reciprocal exit on the
/// destination room, attached to the same Connection (whose kind the
/// backend re-derives from the final member topology). The new exit's
/// direction is the stored return direction, or the opposite of the
/// forward direction. Undo deletes exactly that exit.
///
/// Refuses links that are not exactly one member, have no same-area
/// destination (dangling/external), or whose destination was redacted.
#[must_use]
pub fn add_return_exit(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    connection_id: ConnectionId,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let connection = area.get_connection(connection_id)?;
    // A two-member self-loop is invalid membership; the loop arc already
    // covers both senses visually.
    if connection.kind == smudgy_cloud::ConnectionKind::SelfLoop {
        return None;
    }
    let mut members = area.get_rooms().iter().flat_map(|room| {
        room.get_exits()
            .iter()
            .filter(|exit| exit.connection_id == connection_id)
            .map(move |exit| (room.get_room_number(), exit))
    });
    let (from_room, exit) = members.next()?;
    if members.next().is_some() || exit.to_unknown {
        return None;
    }
    let to_room = exit.to_room_number?;
    if exit.to_area_id != Some(area_id) {
        return None;
    }
    let destination = area.get_room(&to_room)?;
    let return_direction = exit
        .to_direction
        .unwrap_or_else(|| exit.from_direction.opposite());
    // Refuse when the destination already answers: an exit in the return
    // direction would collide, and an existing exit back toward the origin
    // is a reciprocal that should be Paired instead of duplicated.
    if destination.get_exits().iter().any(|other| {
        other.from_direction == return_direction
            || (other.to_area_id == Some(area_id) && other.to_room_number == Some(from_room))
    }) {
        return None;
    }

    let cleared = area.effective_access().is_cleared_for_secrets();
    let new_id = ExitId::new();
    let body = ExitArgs {
        id: Some(new_id),
        connection_id: Some(connection_id),
        new_connection_id: None,
        // The return of a secret/closed/locked passage is the same
        // passage: mirror those, but not the direction-specific
        // path/command.
        is_secret: (cleared && exit.is_secret).then_some(true),
        from_direction: return_direction,
        to_area_id: Some(area_id),
        to_room_number: Some(from_room),
        to_direction: Some(exit.from_direction),
        path: None,
        is_hidden: exit.is_hidden,
        is_closed: exit.is_closed,
        is_locked: exit.is_locked,
        weight: exit.weight,
        command: None,
    };
    Some(Command::new(
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![AreaMutation::CreateExit {
                room_number: to_room,
                body,
            }],
            description: "Add return direction".to_string(),
        }],
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![AreaMutation::DeleteExit { exit_id: new_id }],
            description: "Remove return direction".to_string(),
        }],
    ))
}

/// Pair two reciprocal one-member links, keeping the selected visual route.
/// Undo semantically splits the moved member, then restores its old visuals.
#[must_use]
pub fn pair_connections(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    keep_connection_id: ConnectionId,
    merge_connection_id: ConnectionId,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let merge = area.get_connection(merge_connection_id)?.clone();
    let moved_exit = area
        .get_rooms()
        .iter()
        .flat_map(|room| room.get_exits())
        .find(|exit| exit.connection_id == merge_connection_id)?
        .id;
    Some(Command::new(
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![AreaMutation::Pair {
                keep_connection_id,
                merge_connection_id,
            }],
            description: "Pair reciprocal connections".to_string(),
        }],
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![
                AreaMutation::Unlink {
                    exit_id: moved_exit,
                    new_connection_id: merge_connection_id,
                },
                AreaMutation::UpdateConnection {
                    connection_id: merge_connection_id,
                    body: ConnectionUpdates {
                        endpoint_a: Some(merge.endpoint_a),
                        endpoint_b: merge.endpoint_b,
                        routing: Some(merge.routing),
                        segment_shape: Some(merge.segment_shape),
                        corner: Some(merge.corner),
                        route_points: Some(merge.route_points),
                        dash: Some(merge.dash),
                        color: Some(merge.color),
                        thickness: Some(merge.thickness),
                    },
                },
            ],
            description: "Unpair reciprocal connections".to_string(),
        }],
    ))
}

/// Adds a default unconnected exit to a room (edited in the inspector).
#[must_use]
pub fn add_default_exit(area_id: AreaId, room_number: RoomNumber) -> Command {
    let room_key = RoomKey::new(area_id, room_number);
    Command::new(
        vec![Mutation::CreateExit {
            room_key: room_key.clone(),
            args: ExitArgs {
                from_direction: smudgy_cloud::ExitDirection::Special,
                weight: 1.0,
                ..Default::default()
            },
            follow_up: None,
            slot: 0,
        }],
        vec![Mutation::DeleteExit {
            room_key,
            id: IdRef::Slot(0),
        }],
    )
}

/// Edits an exit by mutating a full-field snapshot of its current state;
/// coalesces with consecutive edits to the same field. Updates are always
/// full snapshots, and because `ExitUpdates::apply` (and the backend) MERGE
/// the destination fields (`None` = unchanged, nulling requires `clear_to`),
/// `clear_to` is recomputed after the edit: set when the resulting
/// destination is empty (and the prior one wasn't merely redacted), dropped
/// when the edit establishes one (`clear_to` overrides `to_*` on the wire).
#[must_use]
pub fn edit_exit_field(
    atlas: &Arc<AtlasCache>,
    room_key: RoomKey,
    exit_id: ExitId,
    field: FieldId,
    change: impl FnOnce(&mut ExitUpdates),
) -> Option<Command> {
    let area = atlas.get_area(&room_key.area_id)?;
    let room = area.get_room(&room_key.room_number)?;
    let exit = room.get_exits().iter().find(|exit| exit.id == exit_id)?;

    let prior = exit_updates_from_cache(exit);
    let mut updates = prior.clone();
    change(&mut updates);
    let destination_expressed = updates.to_area_id.is_some()
        || updates.to_room_number.is_some()
        || updates.to_direction.is_some();
    updates.clear_to = (!destination_expressed && !exit.to_unknown).then_some(true);
    let area_id = room_key.area_id;

    Some(
        Command::new(
            vec![Mutation::UpdateExit {
                room_key: room_key.clone(),
                id: IdRef::Known(exit_id),
                updates,
            }],
            vec![Mutation::UpdateExit {
                room_key,
                id: IdRef::Known(exit_id),
                updates: prior,
            }],
        )
        .coalescing(CoalesceKey {
            entity: EntityRef::Exit(area_id, exit_id),
            field,
            detail: None,
        }),
    )
}

/// Edits one exit field and applies a Connection endpoint edit in the same
/// atomic `AreaBatch` — the direction-change path, where the exit's new
/// direction re-anchors the owning endpoint to its home slot. One validated
/// envelope, one undo unit. Deliberately NOT coalescing: this two-mutation
/// shape must never merge with the single-mutation commands sharing the
/// exit-field coalescing keys, or one side's undo/redo gets discarded.
#[must_use]
pub fn edit_exit_with_endpoint(
    atlas: &Arc<AtlasCache>,
    room_key: RoomKey,
    exit_id: ExitId,
    change: impl FnOnce(&mut ExitUpdates),
    connection_id: ConnectionId,
    connection_updates: ConnectionUpdates,
) -> Option<Command> {
    let area = atlas.get_area(&room_key.area_id)?;
    let room = area.get_room(&room_key.room_number)?;
    let exit = room.get_exits().iter().find(|exit| exit.id == exit_id)?;

    let prior = exit_updates_from_cache(exit);
    let mut updates = prior.clone();
    change(&mut updates);
    let destination_expressed = updates.to_area_id.is_some()
        || updates.to_room_number.is_some()
        || updates.to_direction.is_some();
    updates.clear_to = (!destination_expressed && !exit.to_unknown).then_some(true);
    let area_id = room_key.area_id;

    let current = area.get_connection(connection_id)?;
    // Same endpoint-B inverse rule as `edit_connection`.
    if connection_updates.endpoint_b.is_some() && current.endpoint_b.is_none() {
        return None;
    }
    let inverse = ConnectionUpdates {
        endpoint_a: connection_updates.endpoint_a.map(|_| current.endpoint_a),
        endpoint_b: connection_updates.endpoint_b.and(current.endpoint_b),
        routing: connection_updates.routing.map(|_| current.routing),
        segment_shape: connection_updates
            .segment_shape
            .map(|_| current.segment_shape),
        corner: connection_updates.corner.map(|_| current.corner),
        route_points: connection_updates
            .route_points
            .as_ref()
            .map(|_| current.route_points.clone()),
        dash: connection_updates.dash.map(|_| current.dash),
        color: connection_updates
            .color
            .as_ref()
            .map(|_| current.color.clone()),
        thickness: connection_updates.thickness.map(|_| current.thickness),
    };

    Some(Command::new(
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![
                AreaMutation::UpdateExit {
                    exit_id,
                    body: updates,
                },
                AreaMutation::UpdateConnection {
                    connection_id,
                    body: connection_updates,
                },
            ],
            description: "Change exit direction".to_string(),
        }],
        vec![Mutation::AreaBatch {
            area_id,
            operations: vec![
                AreaMutation::UpdateExit {
                    exit_id,
                    body: prior,
                },
                AreaMutation::UpdateConnection {
                    connection_id,
                    body: inverse,
                },
            ],
            description: "Undo change exit direction".to_string(),
        }],
    ))
}

/// Deletes one exit; undo recreates it (with a fresh backend id tracked
/// through a slot).
///
/// Refuses exits whose destination was redacted (`to_unknown`): the real
/// destination never reached this client, so an undo could only recreate
/// the exit dangling — silently destroying the owner's cross-area link
/// while claiming to have restored it. The inspector hides the delete
/// affordance on those rows; this guards any other path.
#[must_use]
pub fn delete_exit(atlas: &Arc<AtlasCache>, room_key: RoomKey, exit_id: ExitId) -> Option<Command> {
    let area = atlas.get_area(&room_key.area_id)?;
    let room = area.get_room(&room_key.room_number)?;
    let exit = room.get_exits().iter().find(|exit| exit.id == exit_id)?;

    if exit.to_unknown {
        log::warn!(
            "map editor: refusing to delete exit {exit_id} — its destination is an \
             unshared map and could not be restored on undo"
        );
        return None;
    }

    let cleared = area.effective_access().is_cleared_for_secrets();
    Some(
        Command::new(
            vec![Mutation::DeleteExit {
                room_key: room_key.clone(),
                id: IdRef::Slot(0),
            }],
            vec![Mutation::CreateExit {
                room_key,
                args: exit_args_from_cache(exit, cleared),
                follow_up: Some(exit_updates_from_cache(exit)),
                slot: 0,
            }],
        )
        .seed_slot(0, ResolvedId::Exit(exit_id)),
    )
}

/// Creates a room at a map-space point on the given level.
#[must_use]
pub fn create_room(
    area_id: AreaId,
    room_number: RoomNumber,
    at: iced::Point,
    level: i32,
) -> Command {
    Command::new(
        vec![Mutation::UpsertRooms(
            area_id,
            vec![(
                room_number,
                RoomUpdates {
                    is_secret: None,
                    title: Some(String::new()),
                    description: Some(String::new()),
                    level: Some(level),
                    x: Some(at.x),
                    y: Some(at.y),
                    color: Some(String::new()),
                    external_id: None,
                },
            )],
        )],
        vec![Mutation::DeleteRoom(RoomKey::new(area_id, room_number))],
    )
}

/// Applies the same field updates to every selected room as one undo step
/// (used for bulk color/level edits).
#[must_use]
pub fn bulk_edit_rooms(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    selection: &Selection,
    updates: &RoomUpdates,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;

    let mut redo = Vec::new();
    let mut undo = Vec::new();

    for room_number in selection.rooms() {
        let Some(room) = area.get_room(&room_number) else {
            continue;
        };
        redo.push((room_number, updates.clone()));
        undo.push((
            room_number,
            RoomUpdates {
                is_secret: None,
                title: updates.title.as_ref().map(|_| room.get_title().to_string()),
                description: updates
                    .description
                    .as_ref()
                    .map(|_| room.get_description().to_string()),
                level: updates.level.map(|_| room.get_level()),
                x: updates.x.map(|_| room.get_x()),
                y: updates.y.map(|_| room.get_y()),
                color: updates.color.as_ref().map(|_| room.get_color().to_string()),
                external_id: updates
                    .external_id
                    .as_ref()
                    .map(|_| room.get_external_id().map(str::to_string)),
            },
        ));
    }

    if redo.is_empty() {
        None
    } else {
        Some(Command::new(
            vec![Mutation::UpsertRooms(area_id, redo)],
            vec![Mutation::UpsertRooms(area_id, undo)],
        ))
    }
}

/// Moves every selected room (and label/shape) up or down by whole levels
/// as one undo step.
#[must_use]
pub fn shift_selection_level(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    selection: &Selection,
    delta: i32,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;

    let mut room_redo = Vec::new();
    let mut room_undo = Vec::new();
    let mut redo = Vec::new();
    let mut undo = Vec::new();

    for room_number in selection.rooms() {
        let Some(room) = area.get_room(&room_number) else {
            continue;
        };
        room_redo.push((
            room_number,
            RoomUpdates {
                level: Some(room.get_level() + delta),
                ..Default::default()
            },
        ));
        room_undo.push((
            room_number,
            RoomUpdates {
                level: Some(room.get_level()),
                ..Default::default()
            },
        ));
    }

    if !room_redo.is_empty() {
        redo.push(Mutation::UpsertRooms(area_id, room_redo));
        undo.push(Mutation::UpsertRooms(area_id, room_undo));
    }

    for label_id in selection.labels() {
        let Some(label) = area.get_label(&label_id) else {
            continue;
        };
        redo.push(Mutation::UpdateLabel {
            area_id,
            id: IdRef::Known(label_id),
            updates: LabelUpdates {
                level: Some(label.level + delta),
                ..Default::default()
            },
        });
        undo.push(Mutation::UpdateLabel {
            area_id,
            id: IdRef::Known(label_id),
            updates: LabelUpdates {
                level: Some(label.level),
                ..Default::default()
            },
        });
    }

    for shape_id in selection.shapes() {
        let Some(shape) = area.get_shape(&shape_id) else {
            continue;
        };
        redo.push(Mutation::UpdateShape {
            area_id,
            id: IdRef::Known(shape_id),
            updates: ShapeUpdates {
                level: Some(shape.level + delta),
                ..Default::default()
            },
        });
        undo.push(Mutation::UpdateShape {
            area_id,
            id: IdRef::Known(shape_id),
            updates: ShapeUpdates {
                level: Some(shape.level),
                ..Default::default()
            },
        });
    }

    if redo.is_empty() {
        None
    } else {
        Some(Command::new(redo, undo))
    }
}

/// Sets one room property; coalesces with consecutive edits to the same
/// key on the same room.
#[must_use]
pub fn set_room_property(
    atlas: &Arc<AtlasCache>,
    room_key: RoomKey,
    name: String,
    value: String,
) -> Option<Command> {
    let area = atlas.get_area(&room_key.area_id)?;
    let room = area.get_room(&room_key.room_number)?;

    let undo = match room.get_property(&name) {
        Some(prior) => Mutation::SetRoomProperty(room_key.clone(), name.clone(), prior.to_string()),
        None => Mutation::DeleteRoomProperty(room_key.clone(), name.clone()),
    };

    Some(
        Command::new(
            vec![Mutation::SetRoomProperty(
                room_key.clone(),
                name.clone(),
                value,
            )],
            vec![undo],
        )
        .coalescing(CoalesceKey::with_detail(
            EntityRef::Room(room_key),
            FieldId::Property,
            name,
        )),
    )
}

/// Deletes one room property.
#[must_use]
pub fn delete_room_property(
    atlas: &Arc<AtlasCache>,
    room_key: RoomKey,
    name: String,
) -> Option<Command> {
    let area = atlas.get_area(&room_key.area_id)?;
    let room = area.get_room(&room_key.room_number)?;
    let prior = room.get_property(&name)?.to_string();

    Some(Command::new(
        vec![Mutation::DeleteRoomProperty(room_key.clone(), name.clone())],
        vec![Mutation::SetRoomProperty(room_key, name, prior)],
    ))
}

/// Adds one tag to a room. Returns `None` (no undo entry) when the normalized tag
/// is empty or the room already carries it, so an idempotent add is not recorded.
#[must_use]
pub fn add_room_tag(atlas: &Arc<AtlasCache>, room_key: RoomKey, tag: String) -> Option<Command> {
    let tag = smudgy_cloud::mapper::normalize_tag(&tag);
    if tag.is_empty() {
        return None;
    }
    let area = atlas.get_area(&room_key.area_id)?;
    let room = area.get_room(&room_key.room_number)?;
    if room.has_tag(&tag) {
        return None;
    }

    Some(Command::new(
        vec![Mutation::AddRoomTag(room_key.clone(), tag.clone())],
        vec![Mutation::RemoveRoomTag(room_key, tag)],
    ))
}

/// Removes one tag from a room. Returns `None` when the room does not carry it.
#[must_use]
pub fn remove_room_tag(atlas: &Arc<AtlasCache>, room_key: RoomKey, tag: String) -> Option<Command> {
    let tag = smudgy_cloud::mapper::normalize_tag(&tag);
    let area = atlas.get_area(&room_key.area_id)?;
    let room = area.get_room(&room_key.room_number)?;
    if !room.has_tag(&tag) {
        return None;
    }

    Some(Command::new(
        vec![Mutation::RemoveRoomTag(room_key.clone(), tag.clone())],
        vec![Mutation::AddRoomTag(room_key, tag)],
    ))
}

/// Sets one area property; coalesces with consecutive edits to the same key.
#[must_use]
pub fn set_area_property(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    name: String,
    value: String,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;

    let undo = match area.get_property(&name) {
        Some(prior) => Mutation::SetAreaProperty(area_id, name.clone(), prior.to_string()),
        None => Mutation::DeleteAreaProperty(area_id, name.clone()),
    };

    Some(
        Command::new(
            vec![Mutation::SetAreaProperty(area_id, name.clone(), value)],
            vec![undo],
        )
        .coalescing(CoalesceKey::with_detail(
            EntityRef::Area(area_id),
            FieldId::Property,
            name,
        )),
    )
}

/// Deletes one area property.
#[must_use]
pub fn delete_area_property(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    name: String,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let prior = area.get_property(&name)?.to_string();

    Some(Command::new(
        vec![Mutation::DeleteAreaProperty(area_id, name.clone())],
        vec![Mutation::SetAreaProperty(area_id, name, prior)],
    ))
}

/// Creates a label covering a map-space rect on the given level, with
/// legible defaults for inspector refinement.
#[must_use]
pub fn create_label(area_id: AreaId, rect: iced::Rectangle, level: i32) -> Command {
    Command::new(
        vec![Mutation::CreateLabel {
            area_id,
            args: LabelArgs {
                level,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                text: crate::i18n::t!("inspector-label"),
                color: "#c8c8c8".to_string(),
                // Explicitly transparent: an absent background invites
                // server-side creation defaults (historically white).
                background_color: Some(String::new()),
                font_size: 16,
                font_weight: 400,
                ..Default::default()
            },
            slot: 0,
        }],
        vec![Mutation::DeleteLabel {
            area_id,
            id: IdRef::Slot(0),
        }],
    )
}

/// Creates a shape covering a map-space rect on the given level.
#[must_use]
pub fn create_shape(area_id: AreaId, rect: iced::Rectangle, level: i32) -> Command {
    Command::new(
        vec![Mutation::CreateShape {
            area_id,
            args: ShapeArgs {
                level,
                x: rect.x,
                y: rect.y,
                width: rect.width,
                height: rect.height,
                background_color: Some("#32323c".to_string()),
                stroke_color: Some(String::new()),
                ..Default::default()
            },
            slot: 0,
        }],
        vec![Mutation::DeleteShape {
            area_id,
            id: IdRef::Slot(0),
        }],
    )
}

/// Sets a label's or shape's bounds (one undo step per resize drag).
#[must_use]
pub fn resize_entity(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    entity: EntityId,
    rect: iced::Rectangle,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;

    match entity {
        EntityId::Label(label_id) => {
            let label = area.get_label(&label_id)?;
            Some(Command::new(
                vec![Mutation::UpdateLabel {
                    area_id,
                    id: IdRef::Known(label_id),
                    updates: LabelUpdates {
                        x: Some(rect.x),
                        y: Some(rect.y),
                        width: Some(rect.width),
                        height: Some(rect.height),
                        ..Default::default()
                    },
                }],
                vec![Mutation::UpdateLabel {
                    area_id,
                    id: IdRef::Known(label_id),
                    updates: LabelUpdates {
                        x: Some(label.x),
                        y: Some(label.y),
                        width: Some(label.width),
                        height: Some(label.height),
                        ..Default::default()
                    },
                }],
            ))
        }
        EntityId::Shape(shape_id) => {
            let shape = area.get_shape(&shape_id)?;
            Some(Command::new(
                vec![Mutation::UpdateShape {
                    area_id,
                    id: IdRef::Known(shape_id),
                    updates: ShapeUpdates {
                        x: Some(rect.x),
                        y: Some(rect.y),
                        width: Some(rect.width),
                        height: Some(rect.height),
                        ..Default::default()
                    },
                }],
                vec![Mutation::UpdateShape {
                    area_id,
                    id: IdRef::Known(shape_id),
                    updates: ShapeUpdates {
                        x: Some(shape.x),
                        y: Some(shape.y),
                        width: Some(shape.width),
                        height: Some(shape.height),
                        ..Default::default()
                    },
                }],
            ))
        }
        EntityId::Room(_) | EntityId::Connection(_) => None,
    }
}

/// A snapshot of one copied room: identity, geometry, styling, properties,
/// and the exits it owns.
#[derive(Debug, Clone)]
pub struct RoomClip {
    /// The room's number in the *source* area; paste remaps it.
    pub room_number: RoomNumber,
    pub title: String,
    pub description: String,
    pub level: i32,
    pub x: f32,
    pub y: f32,
    pub color: String,
    pub is_secret: bool,
    /// Server-global room id (GMCP/MSDP identity); rides copy/paste so the
    /// merge workflow's cut+paste keeps bindings.
    pub external_id: Option<String>,
    /// Sorted by name for deterministic paste mutation order. Secrecy
    /// marks don't survive the trip: the property PUT body has no secrecy
    /// channel (same gap as `delete_selection`'s undo).
    pub properties: Vec<(String, String)>,
    pub exits: Vec<ExitClip>,
}

/// `ExitCache`-shaped data for an exit owned by a copied room.
/// `to_area_token` is deliberately not carried: it's a per-viewer
/// projection artifact and must never be written back.
#[derive(Debug, Clone)]
pub struct ExitClip {
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    pub path: Option<String>,
    pub is_hidden: bool,
    pub is_closed: bool,
    pub is_locked: bool,
    pub weight: f32,
    pub command: Option<String>,
    pub is_secret: bool,
    /// Destination redacted ("Unknown map"); always pastes dangling.
    pub to_unknown: bool,
}

/// One fully-contained copied Connection. Stored route points are relative
/// to [`EntityClipboard::connection_origin`], so cross-area paste preserves
/// exact geometry and same-area paste applies the normal cascade offset.
#[derive(Debug, Clone)]
pub struct ConnectionClip {
    pub body: ConnectionArgs,
    pub members: Vec<(RoomNumber, ExitClip)>,
}

/// A snapshot of copied entities, held by the editor window between copy
/// and paste. Positions/levels are kept from the source; same-area pastes
/// apply a cascading offset, cross-area pastes preserve them exactly.
#[derive(Debug, Clone, Default)]
pub struct EntityClipboard {
    /// The area the snapshot came from; decides same-area (fresh room
    /// numbers, cascading offset) vs cross-area (numbers preserved where
    /// vacant, exact positions) paste semantics.
    pub source_area_id: Option<AreaId>,
    pub rooms: Vec<RoomClip>,
    pub connections: Vec<ConnectionClip>,
    pub connection_origin: Option<smudgy_cloud::MapPoint>,
    pub labels: Vec<LabelArgs>,
    pub shapes: Vec<ShapeArgs>,
}

impl EntityClipboard {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
            && self.connections.is_empty()
            && self.labels.is_empty()
            && self.shapes.is_empty()
    }
}

/// Snapshots the selected entities for the clipboard. Rooms (and their
/// outgoing exits) are included only when `allow_rooms` — the owner must
/// have granted `can_copy` (or the viewer owns the area).
#[must_use]
pub fn snapshot_selection(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    selection: &Selection,
    allow_rooms: bool,
    include_boundary_links: bool,
) -> EntityClipboard {
    let Some(area) = atlas.get_area(&area_id) else {
        return EntityClipboard::default();
    };

    let mut rooms = Vec::new();
    let selected_rooms: HashSet<_> = selection.rooms().collect();
    let connection_origin = selected_rooms
        .iter()
        .filter_map(|number| area.get_room(number))
        .fold(None, |origin: Option<smudgy_cloud::MapPoint>, room| {
            Some(origin.map_or_else(
                || smudgy_cloud::MapPoint::new(room.get_x(), room.get_y()),
                |origin| {
                    smudgy_cloud::MapPoint::new(
                        origin.x.min(room.get_x()),
                        origin.y.min(room.get_y()),
                    )
                },
            ))
        });
    // Fully-contained links ride the room copy; explicitly selected
    // connections join on their own (paste attaches them to same-numbered
    // rooms when their rooms aren't part of the snapshot).
    let explicitly_selected: HashSet<ConnectionId> = selection.connections().collect();
    let eligible_connections: HashSet<_> = area
        .get_connections()
        .iter()
        .filter(|connection| {
            // Explicitly selected links copy whatever their shape —
            // dangling and external ones included (their paste degrades
            // per-member); room-implied links still need both ends inside
            // the selection.
            explicitly_selected.contains(&connection.id)
                || connection.endpoint_b.is_some_and(|endpoint_b| {
                    selected_rooms.contains(&connection.endpoint_a.room_number)
                        && selected_rooms.contains(&endpoint_b.room_number)
                })
        })
        .map(|connection| connection.id)
        .collect();
    if allow_rooms {
        let mut numbers: Vec<RoomNumber> = selection.rooms().collect();
        numbers.sort_unstable_by_key(|number| number.0);
        for number in numbers {
            let Some(room) = area.get_room(&number) else {
                continue;
            };
            let mut properties: Vec<(String, String)> = room
                .properties()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect();
            properties.sort();
            // Boundary links are omitted by default. Fully-contained links
            // are represented once in `ConnectionClip`, not duplicated on
            // each room.
            let exits = Vec::new();
            rooms.push(RoomClip {
                room_number: number,
                title: room.get_title().to_string(),
                description: room.get_description().to_string(),
                level: room.get_level(),
                x: room.get_x(),
                y: room.get_y(),
                color: room.get_color().to_string(),
                is_secret: room.is_secret(),
                external_id: room.get_external_id().map(str::to_string),
                properties,
                exits,
            });
        }
    }

    let mut connections = Vec::new();
    if allow_rooms {
        for connection in area
            .get_connections()
            .iter()
            .filter(|connection| eligible_connections.contains(&connection.id))
        {
            let mut body = ConnectionArgs::from(connection);
            if let Some(origin) = connection_origin {
                for point in &mut body.route_points {
                    point.x -= origin.x;
                    point.y -= origin.y;
                }
            }
            let mut members = Vec::new();
            for room in area.get_rooms() {
                for exit in room.get_exits() {
                    if exit.connection_id == connection.id {
                        members.push((
                            room.get_room_number(),
                            ExitClip {
                                from_direction: exit.from_direction,
                                to_area_id: exit.to_area_id,
                                to_room_number: exit.to_room_number,
                                to_direction: exit.to_direction,
                                path: exit.path.clone(),
                                is_hidden: exit.is_hidden,
                                is_closed: exit.is_closed,
                                is_locked: exit.is_locked,
                                weight: exit.weight,
                                command: exit.command.clone(),
                                is_secret: exit.is_secret,
                                to_unknown: exit.to_unknown,
                            },
                        ));
                    }
                }
            }
            members.sort_by_key(|(room_number, _)| room_number.0);
            connections.push(ConnectionClip { body, members });
        }

        if include_boundary_links {
            for connection in area
                .get_connections()
                .iter()
                .filter(|connection| !eligible_connections.contains(&connection.id))
            {
                let selected_member = area.get_rooms().iter().find_map(|room| {
                    selected_rooms
                        .contains(&room.get_room_number())
                        .then(|| {
                            room.get_exits()
                                .iter()
                                .find(|exit| exit.connection_id == connection.id)
                                .map(|exit| (room.get_room_number(), exit))
                        })
                        .flatten()
                });
                let Some((from_room, exit)) = selected_member else {
                    continue;
                };
                let Some(endpoint) = [connection.endpoint_a]
                    .into_iter()
                    .chain(connection.endpoint_b)
                    .find(|endpoint| endpoint.room_number == from_room)
                else {
                    continue;
                };
                let mut body = ConnectionArgs::from(connection);
                body.endpoint_a = endpoint;
                body.endpoint_b = None;
                body.route_points.clear();
                if matches!(
                    body.routing,
                    ConnectionRouting::Manual | ConnectionRouting::Automatic
                ) {
                    body.routing = ConnectionRouting::Simple;
                }
                body.segment_shape = SegmentShape::Direct;
                connections.push(ConnectionClip {
                    body,
                    members: vec![(
                        from_room,
                        ExitClip {
                            from_direction: exit.from_direction,
                            to_area_id: None,
                            to_room_number: None,
                            to_direction: None,
                            path: exit.path.clone(),
                            is_hidden: exit.is_hidden,
                            is_closed: exit.is_closed,
                            is_locked: exit.is_locked,
                            weight: exit.weight,
                            command: exit.command.clone(),
                            is_secret: exit.is_secret,
                            to_unknown: false,
                        },
                    )],
                });
            }
        }
    }

    let labels = selection
        .labels()
        .filter_map(|label_id| area.get_label(&label_id))
        .map(|label| LabelArgs {
            // Clipboard entries carry no identity; each paste mints its own.
            id: None,
            is_secret: None,
            level: label.level,
            x: label.x,
            y: label.y,
            width: label.width,
            height: label.height,
            horizontal_alignment: label.horizontal_alignment.clone(),
            vertical_alignment: label.vertical_alignment.clone(),
            text: label.text.clone(),
            color: label.color.clone(),
            // Always explicit — `Some("")` means transparent, while an
            // absent value invites server-side creation defaults.
            background_color: Some(label.background_color.clone()),
            font_size: label.font_size,
            font_weight: label.font_weight,
        })
        .collect();

    let shapes = selection
        .shapes()
        .filter_map(|shape_id| area.get_shape(&shape_id))
        .map(|shape| ShapeArgs {
            // Clipboard entries carry no identity; each paste mints its own.
            id: None,
            is_secret: None,
            level: shape.level,
            x: shape.x,
            y: shape.y,
            width: shape.width,
            height: shape.height,
            // Always explicit — `Some("")` means no fill/stroke, while an
            // absent value invites server-side creation defaults.
            background_color: Some(shape.background_color.clone().unwrap_or_default()),
            stroke_color: Some(shape.stroke_color.clone().unwrap_or_default()),
            shape_type: shape.shape_type.clone(),
            border_radius: shape.border_radius,
            stroke_width: Some(shape.stroke_width),
        })
        .collect();

    EntityClipboard {
        source_area_id: Some(area_id),
        rooms,
        connections,
        connection_origin,
        labels,
        shapes,
    }
}

/// Number of links that leave the selected room set and can be copied as
/// explicit dangling one-way links. Used by the clipboard confirmation so
/// omission is visible rather than silent.
#[must_use]
pub fn boundary_link_count(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    selection: &Selection,
) -> usize {
    let Some(area) = atlas.get_area(&area_id) else {
        return 0;
    };
    let selected_rooms: HashSet<_> = selection.rooms().collect();
    area.get_connections()
        .iter()
        .filter(|connection| {
            let fully_contained = connection.endpoint_b.is_some_and(|endpoint_b| {
                selected_rooms.contains(&connection.endpoint_a.room_number)
                    && selected_rooms.contains(&endpoint_b.room_number)
            });
            !fully_contained
                && area.get_rooms().iter().any(|room| {
                    selected_rooms.contains(&room.get_room_number())
                        && room
                            .get_exits()
                            .iter()
                            .any(|exit| exit.connection_id == connection.id)
                })
        })
        .count()
}

/// Maps copied room numbers onto numbers vacant in the target area.
///
/// Cross-area pastes (`preserve_numbers`) keep each source number when it
/// is vacant — not occupied and not already claimed by this paste — so a
/// merge-back lands on the same identities. Collisions, and every
/// same-area paste, allocate fresh numbers counting up from `first_fresh`
/// and skipping anything occupied or claimed.
fn remap_room_numbers(
    source: &[RoomNumber],
    occupied: &HashSet<RoomNumber>,
    first_fresh: RoomNumber,
    preserve_numbers: bool,
) -> HashMap<RoomNumber, RoomNumber> {
    let mut claimed = occupied.clone();
    let mut next = first_fresh.0;
    let mut mapping = HashMap::with_capacity(source.len());

    for &number in source {
        let target = if preserve_numbers && !claimed.contains(&number) {
            number
        } else {
            while claimed.contains(&RoomNumber(next)) {
                next += 1;
            }
            let fresh = RoomNumber(next);
            next += 1;
            fresh
        };
        claimed.insert(target);
        mapping.insert(number, target);
    }

    mapping
}

/// Where a pasted exit points.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PastedExitDestination {
    /// Both ends were copied: the destination follows the room-number
    /// remap into the paste target.
    Remapped(RoomNumber),
    /// A live link into another area, kept as-is (mirrors the server's
    /// clone semantics).
    Live(AreaId, RoomNumber),
    /// The destination can't be carried over; pasted unconnected.
    Dangling,
}

/// Classifies a copied exit's destination for pasting:
/// - intra-selection (source-area destination covered by `mapping`) →
///   remapped into the paste target,
/// - another area present in the atlas cache → kept as a live link,
/// - anything else (non-selected source-area room, redacted destination,
///   area missing from the cache, no destination) → dangling.
fn classify_pasted_exit(
    exit: &ExitClip,
    source_area_id: AreaId,
    mapping: &HashMap<RoomNumber, RoomNumber>,
    area_in_cache: impl Fn(AreaId) -> bool,
) -> PastedExitDestination {
    if exit.to_unknown {
        return PastedExitDestination::Dangling;
    }
    let Some(to_room) = exit.to_room_number else {
        return PastedExitDestination::Dangling;
    };
    // Same-area destinations are written as `Some(source)` throughout the
    // codebase (see `create_exit`), but tolerate a bare room number
    // meaning "this area".
    let to_area = exit.to_area_id.unwrap_or(source_area_id);
    if to_area == source_area_id {
        return mapping
            .get(&to_room)
            .map_or(PastedExitDestination::Dangling, |remapped| {
                PastedExitDestination::Remapped(*remapped)
            });
    }
    if area_in_cache(to_area) {
        PastedExitDestination::Live(to_area, to_room)
    } else {
        PastedExitDestination::Dangling
    }
}

/// Pastes the clipboard into `target_area_id` as one undo step: rooms in a
/// single [`Mutation::UpsertRooms`] batch (one cache rebuild), then their
/// properties and exits, then labels/shapes.
///
/// Same-area pastes (`source_area_id == target`) allocate fresh room
/// numbers and apply `offset`/`level` like label/shape paste always has;
/// cross-area pastes keep vacant source numbers and exact x/y/level so
/// merged-back changes line up (the caller passes a zero offset).
///
/// Returns the command plus the pasted rooms' (new) numbers so the caller
/// can select them. All entities apply synchronously; labels and shapes emit
/// completion messages so the editor can select their client-minted ids.
///
/// # Panics
///
/// Panics if the room-number remap targets an occupied number (an
/// invariant of [`remap_room_numbers`]; pasting must never overwrite an
/// existing room).
#[must_use]
pub fn paste_clipboard(
    atlas: &Arc<AtlasCache>,
    target_area_id: AreaId,
    clipboard: &EntityClipboard,
    level: i32,
    offset: Vector,
    // Reservation-aware allocation base from the Mapper (skips numbers held
    // by open scripted mutators); `None` falls back to the cache maximum.
    next_room_number: Option<RoomNumber>,
) -> (Option<Command>, Vec<RoomNumber>, usize) {
    if clipboard.is_empty() {
        return (None, Vec::new(), 0);
    }
    let Some(area) = atlas.get_area(&target_area_id) else {
        return (None, Vec::new(), 0);
    };
    let same_area = clipboard.source_area_id == Some(target_area_id);
    // Secrecy flags may only be sent when the viewer is cleared on the
    // *target* (the server uniform-404s otherwise); an uncleared viewer's
    // clipboard holds no secret entities anyway.
    let cleared = area.effective_access().is_cleared_for_secrets();
    let source_area_id = clipboard.source_area_id.unwrap_or(target_area_id);

    let mut redo = Vec::new();
    let mut undo = Vec::new();
    let mut next_slot: SlotId = 0;
    let mut pasted_rooms = Vec::new();
    let mut skipped_connections = 0usize;

    let mut compound = Vec::new();
    // Links pasted onto *existing* rooms aren't covered by the room-delete
    // cascade on undo; they need explicit deletes.
    let mut undo_links = Vec::new();
    let mut mapping: HashMap<RoomNumber, RoomNumber> = HashMap::new();

    if !clipboard.rooms.is_empty() {
        let occupied: HashSet<RoomNumber> = area
            .get_rooms()
            .iter()
            .map(|room| room.get_room_number())
            .collect();
        let source_numbers: Vec<RoomNumber> = clipboard
            .rooms
            .iter()
            .map(|room| room.room_number)
            .collect();
        mapping = remap_room_numbers(
            &source_numbers,
            &occupied,
            next_room_number.unwrap_or_else(|| area.next_room_number()),
            !same_area,
        );

        let mut legacy_exits = Vec::new();
        for room in &clipboard.rooms {
            let number = mapping[&room.room_number];
            assert!(
                !occupied.contains(&number),
                "paste remap produced an occupied room number"
            );
            pasted_rooms.push(number);
            compound.push(AreaMutation::UpsertRoom {
                room_number: number,
                body: RoomUpdates {
                    is_secret: cleared.then_some(room.is_secret),
                    title: Some(room.title.clone()),
                    description: Some(room.description.clone()),
                    // Rooms keep their source level in both modes: a
                    // multi-level structure flattened onto the current
                    // level would collapse its up/down geometry.
                    level: Some(room.level),
                    x: Some(room.x + offset.x),
                    y: Some(room.y + offset.y),
                    color: Some(room.color.clone()),
                    // Bindings ride the paste (cut+paste is the merge-workflow
                    // move); duplicates resolve best-effort, own-map-first.
                    external_id: room.external_id.clone().map(Some),
                },
            });

            for (name, value) in &room.properties {
                compound.push(AreaMutation::UpsertRoomProperty {
                    room_number: number,
                    name: name.clone(),
                    value: value.clone(),
                    is_secret: None,
                });
            }
            for exit in &room.exits {
                legacy_exits.push((number, exit));
            }
        }
        for (room_number, exit) in legacy_exits {
            let destination = classify_pasted_exit(exit, source_area_id, &mapping, |id| {
                atlas.get_area(&id).is_some()
            });
            let (to_area_id, to_room_number, to_direction) = match destination {
                PastedExitDestination::Remapped(number) => {
                    (Some(target_area_id), Some(number), exit.to_direction)
                }
                PastedExitDestination::Live(area_id, number) => {
                    (Some(area_id), Some(number), exit.to_direction)
                }
                PastedExitDestination::Dangling => (None, None, None),
            };
            compound.push(AreaMutation::CreateExit {
                room_number,
                body: ExitArgs {
                    id: Some(ExitId::new()),
                    connection_id: None,
                    new_connection_id: None,
                    is_secret: cleared.then_some(exit.is_secret),
                    from_direction: exit.from_direction,
                    to_area_id,
                    to_room_number,
                    to_direction,
                    path: exit.path.clone(),
                    is_hidden: exit.is_hidden,
                    is_closed: exit.is_closed,
                    is_locked: exit.is_locked,
                    weight: exit.weight,
                    command: exit.command.clone(),
                },
            });
        }
    }

    // Connections paste with or without their rooms: endpoints resolve
    // through the paste mapping first, then to the same-numbered existing
    // room. Links attached to existing rooms lose their stored route (it
    // belongs to the source layout) and are skipped entirely when a member
    // direction is already taken there — a same-area duplicate paste is a
    // deliberate no-op, not an ambiguous second exit.
    let origin = clipboard.connection_origin.unwrap_or_default();
    for connection in &clipboard.connections {
        let resolve = |number: RoomNumber| {
            mapping
                .get(&number)
                .copied()
                .or_else(|| area.get_room(&number).is_some().then_some(number))
        };
        let source_a = connection.body.endpoint_a.room_number;
        let Some(endpoint_a_room) = resolve(source_a) else {
            skipped_connections += 1;
            continue;
        };
        let endpoint_b_room = match connection.body.endpoint_b {
            Some(endpoint) => match resolve(endpoint.room_number) {
                Some(number) => Some((endpoint.room_number, number)),
                None => {
                    skipped_connections += 1;
                    continue;
                }
            },
            None => None,
        };
        // Any endpoint attached to a pre-existing room means (a) the stored
        // route belongs to the source layout and must be dropped, and (b)
        // the room-delete cascade won't clean the link up on undo, so it
        // needs its own DeleteLink.
        let any_existing = !mapping.contains_key(&source_a)
            || endpoint_b_room.is_some_and(|(source, _)| !mapping.contains_key(&source));

        let mut members = Vec::new();
        let mut viable = true;
        for (from_room, exit) in &connection.members {
            let Some(room_number) = resolve(*from_room) else {
                viable = false;
                break;
            };
            if !mapping.contains_key(from_room)
                && area.get_room(&room_number).is_some_and(|room| {
                    room.get_exits()
                        .iter()
                        .any(|other| other.from_direction == exit.from_direction)
                })
            {
                viable = false;
                break;
            }
            // Members of an internal link point at its other endpoint;
            // resolve those through the same room resolution. A genuinely
            // cross-area destination (an explicit External clip) keeps its
            // area when it's live in the atlas, and dangles otherwise —
            // never silently rewritten into the target area.
            let (to_area_id, to_room_number, to_direction) = match exit.to_area_id {
                Some(destination_area) if destination_area != source_area_id => {
                    if atlas.get_area(&destination_area).is_some() {
                        (
                            Some(destination_area),
                            exit.to_room_number,
                            exit.to_direction,
                        )
                    } else {
                        (None, None, None)
                    }
                }
                _ => match exit.to_room_number.and_then(resolve) {
                    Some(number) => (Some(target_area_id), Some(number), exit.to_direction),
                    None => (None, None, None),
                },
            };
            members.push((room_number, exit, to_area_id, to_room_number, to_direction));
        }
        if !viable {
            skipped_connections += 1;
            continue;
        }

        let new_connection_id = ConnectionId::new();
        let mut body = connection.body.clone();
        body.id = new_connection_id;
        body.endpoint_a.room_number = endpoint_a_room;
        if let (Some(endpoint), Some((_, number))) = (body.endpoint_b.as_mut(), endpoint_b_room) {
            endpoint.room_number = number;
        }
        if any_existing {
            body.route_points.clear();
            if matches!(
                body.routing,
                ConnectionRouting::Manual | ConnectionRouting::Automatic
            ) {
                body.routing = ConnectionRouting::Simple;
            }
            body.segment_shape = SegmentShape::Direct;
            undo_links.push(AreaMutation::DeleteLink {
                connection_id: new_connection_id,
            });
        } else {
            for point in &mut body.route_points {
                point.x += origin.x + offset.x;
                point.y += origin.y + offset.y;
            }
        }
        compound.push(AreaMutation::CreateConnection { body });
        for (room_number, exit, to_area_id, to_room_number, to_direction) in members {
            compound.push(AreaMutation::CreateExit {
                room_number,
                body: ExitArgs {
                    id: Some(ExitId::new()),
                    connection_id: Some(new_connection_id),
                    new_connection_id: None,
                    is_secret: cleared.then_some(exit.is_secret),
                    from_direction: exit.from_direction,
                    to_area_id,
                    to_room_number,
                    to_direction,
                    path: exit.path.clone(),
                    is_hidden: exit.is_hidden,
                    is_closed: exit.is_closed,
                    is_locked: exit.is_locked,
                    weight: exit.weight,
                    command: exit.command.clone(),
                },
            });
        }
    }

    if !compound.is_empty() {
        if compound.len() > smudgy_cloud::MAX_MUTATION_OPERATIONS {
            return (None, Vec::new(), 0);
        }
        redo.push(Mutation::AreaBatch {
            area_id: target_area_id,
            operations: compound,
            description: if pasted_rooms.is_empty() {
                "Paste links".to_string()
            } else {
                format!("Paste {} rooms and contained links", pasted_rooms.len())
            },
        });
        let mut undo_ops = undo_links;
        undo_ops.extend(
            pasted_rooms
                .iter()
                .map(|room_number| AreaMutation::DeleteRoom {
                    room_number: *room_number,
                }),
        );
        undo.push(Mutation::AreaBatch {
            area_id: target_area_id,
            operations: undo_ops,
            description: "Undo paste".to_string(),
        });
    }

    for label in &clipboard.labels {
        let slot = next_slot;
        next_slot += 1;
        redo.push(Mutation::CreateLabel {
            area_id: target_area_id,
            args: LabelArgs {
                level: if same_area { level } else { label.level },
                x: label.x + offset.x,
                y: label.y + offset.y,
                ..label.clone()
            },
            slot,
        });
        undo.push(Mutation::DeleteLabel {
            area_id: target_area_id,
            id: IdRef::Slot(slot),
        });
    }

    for shape in &clipboard.shapes {
        let slot = next_slot;
        next_slot += 1;
        redo.push(Mutation::CreateShape {
            area_id: target_area_id,
            args: ShapeArgs {
                level: if same_area { level } else { shape.level },
                x: shape.x + offset.x,
                y: shape.y + offset.y,
                ..shape.clone()
            },
            slot,
        });
        undo.push(Mutation::DeleteShape {
            area_id: target_area_id,
            id: IdRef::Slot(slot),
        });
    }

    let command = (!redo.is_empty()).then(|| Command::new(redo, undo));
    (command, pasted_rooms, skipped_connections)
}

/// Edits one label field; coalesces with consecutive edits to the same
/// field of the same label.
#[must_use]
pub fn edit_label_field(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    label_id: LabelId,
    field: FieldId,
    updates: LabelUpdates,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let label = area.get_label(&label_id)?;

    let prior = LabelUpdates {
        is_secret: None,
        level: updates.level.map(|_| label.level),
        x: updates.x.map(|_| label.x),
        y: updates.y.map(|_| label.y),
        width: updates.width.map(|_| label.width),
        height: updates.height.map(|_| label.height),
        horizontal_alignment: updates
            .horizontal_alignment
            .as_ref()
            .map(|_| label.horizontal_alignment.clone()),
        vertical_alignment: updates
            .vertical_alignment
            .as_ref()
            .map(|_| label.vertical_alignment.clone()),
        text: updates.text.as_ref().map(|_| label.text.clone()),
        color: updates.color.as_ref().map(|_| label.color.clone()),
        background_color: updates
            .background_color
            .as_ref()
            .map(|_| label.background_color.clone()),
        font_size: updates.font_size.map(|_| label.font_size),
        font_weight: updates.font_weight.map(|_| label.font_weight),
    };

    Some(
        Command::new(
            vec![Mutation::UpdateLabel {
                area_id,
                id: IdRef::Known(label_id),
                updates,
            }],
            vec![Mutation::UpdateLabel {
                area_id,
                id: IdRef::Known(label_id),
                updates: prior,
            }],
        )
        .coalescing(CoalesceKey::new(EntityRef::Label(area_id, label_id), field)),
    )
}

/// Edits one shape field; coalesces with consecutive edits to the same
/// field of the same shape.
#[must_use]
pub fn edit_shape_field(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    shape_id: ShapeId,
    field: FieldId,
    updates: ShapeUpdates,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let shape = area.get_shape(&shape_id)?;

    let prior = ShapeUpdates {
        is_secret: None,
        level: updates.level.map(|_| shape.level),
        x: updates.x.map(|_| shape.x),
        y: updates.y.map(|_| shape.y),
        width: updates.width.map(|_| shape.width),
        height: updates.height.map(|_| shape.height),
        background_color: updates
            .background_color
            .as_ref()
            .map(|_| shape.background_color.clone().unwrap_or_default()),
        stroke_color: updates
            .stroke_color
            .as_ref()
            .map(|_| shape.stroke_color.clone().unwrap_or_default()),
        shape_type: updates
            .shape_type
            .as_ref()
            .map(|_| shape.shape_type.clone()),
        border_radius: updates.border_radius.map(|_| shape.border_radius),
        stroke_width: updates.stroke_width.map(|_| shape.stroke_width),
    };

    Some(
        Command::new(
            vec![Mutation::UpdateShape {
                area_id,
                id: IdRef::Known(shape_id),
                updates,
            }],
            vec![Mutation::UpdateShape {
                area_id,
                id: IdRef::Known(shape_id),
                updates: prior,
            }],
        )
        .coalescing(CoalesceKey::new(EntityRef::Shape(area_id, shape_id), field)),
    )
}

/// Edits one room field; coalesces with consecutive edits to the same
/// field of the same room.
#[must_use]
pub fn edit_room_field(
    atlas: &Arc<AtlasCache>,
    room_key: RoomKey,
    field: FieldId,
    updates: RoomUpdates,
) -> Option<Command> {
    let area = atlas.get_area(&room_key.area_id)?;
    let room = area.get_room(&room_key.room_number)?;

    let prior = RoomUpdates {
        is_secret: None,
        title: updates.title.as_ref().map(|_| room.get_title().to_string()),
        description: updates
            .description
            .as_ref()
            .map(|_| room.get_description().to_string()),
        level: updates.level.map(|_| room.get_level()),
        x: updates.x.map(|_| room.get_x()),
        y: updates.y.map(|_| room.get_y()),
        color: updates.color.as_ref().map(|_| room.get_color().to_string()),
        external_id: updates
            .external_id
            .as_ref()
            .map(|_| room.get_external_id().map(str::to_string)),
    };

    let area_id = room_key.area_id;
    let room_number = room_key.room_number;

    Some(
        Command::new(
            vec![Mutation::UpsertRooms(area_id, vec![(room_number, updates)])],
            vec![Mutation::UpsertRooms(area_id, vec![(room_number, prior)])],
        )
        .coalescing(CoalesceKey::new(EntityRef::Room(room_key), field)),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use smudgy_cloud::mapper::RoomKey;
    use smudgy_cloud::{
        Area, AreaUpdates, AreaWithDetails, CloudError, CloudResult, CreateAreaRequest,
        ExitDirection, MapDestination, MapStorage, MapperBackend, RoomSide, Uuid,
    };
    use smudgy_map_widget::map_editor::{EntityId, Selection};

    /// A backend that fabricates ids and accepts every operation.
    #[derive(Default)]
    struct MockBackend {
        next_rev: std::sync::atomic::AtomicI64,
    }

    #[async_trait]
    impl MapperBackend for MockBackend {
        async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
            Ok(Area {
                id: AreaId(Uuid::new_v4()),
                user_id: None,
                atlas_id: None,
                name: request.name,
                created_at: chrono::Utc::now(),
                rev: 0,
                access: None,
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                atlas_name: None,
            })
        }

        async fn create_area_at(
            &self,
            request: CreateAreaRequest,
            storage: MapStorage,
        ) -> CloudResult<Area> {
            // The command tests only route cloud-tier creates here; honoring
            // the trait's "prove or reject" contract keeps a silently
            // mis-tiered request from passing.
            assert_eq!(storage, MapStorage::Cloud);
            self.create_area(request).await
        }

        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            Ok(vec![])
        }

        async fn get_area(&self, _area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            Err(CloudError::InternalError("not supported".into()))
        }

        async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_area(&self, _area_id: &AreaId) -> CloudResult<()> {
            Ok(())
        }

        async fn execute_mutation(
            &self,
            area_id: &AreaId,
            envelope: &smudgy_cloud::mutation::MutationEnvelope,
        ) -> CloudResult<smudgy_cloud::mutation::MutationResult> {
            // The command-stack tests exercise ordering and undo against the
            // optimistic cache; the backend just acknowledges with a moving
            // revision and no echoes (the dispatch path ignores `data`).
            let rev = self
                .next_rev
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                + 1;
            Ok(smudgy_cloud::mutation::MutationResult {
                operation_id: envelope.operation_id,
                versions: vec![smudgy_cloud::mutation::VersionInfo {
                    resource: smudgy_cloud::mutation::ResourceKind::Area,
                    id: area_id.0,
                    rev,
                    deleted: false,
                }],
                data: Vec::new(),
            })
        }
    }

    fn test_mapper() -> Mapper {
        let dir = std::env::temp_dir().join(format!("smudgy-test-{}", Uuid::new_v4()));
        Mapper::new(std::sync::Arc::new(MockBackend::default()), dir)
    }

    fn resolved_id(stack: &CommandStack, command_id: CommandId, slot: SlotId) -> ResolvedId {
        stack
            .undo
            .iter()
            .chain(stack.redo.iter())
            .find(|command| command.id == command_id)
            .and_then(|command| command.resolved_ids.get(slot))
            .and_then(|id| *id)
            .expect("create id was minted before enqueue")
    }

    /// Settles the ready create-completion tasks that an iced runtime would
    /// normally feed back to the command stack.
    fn drive_create_completions(
        mapper: &Mapper,
        stack: &mut CommandStack,
        command_id: CommandId,
        mutations: Vec<Mutation>,
    ) {
        for mutation in mutations {
            match mutation {
                Mutation::CreateExit { room_key, slot, .. } => {
                    let ResolvedId::Exit(id) = resolved_id(stack, command_id, slot) else {
                        panic!("exit slot held the wrong entity kind");
                    };
                    stack.resolve(
                        mapper,
                        Outcome::Exit {
                            command: command_id,
                            slot,
                            room_key,
                            follow_up: None,
                            result: Ok(id),
                        },
                    );
                }
                Mutation::CreateLabel { slot, .. } => {
                    let ResolvedId::Label(id) = resolved_id(stack, command_id, slot) else {
                        panic!("label slot held the wrong entity kind");
                    };
                    stack.resolve(
                        mapper,
                        Outcome::Label {
                            command: command_id,
                            slot,
                            result: Ok(id),
                        },
                    );
                }
                Mutation::CreateShape { slot, .. } => {
                    let ResolvedId::Shape(id) = resolved_id(stack, command_id, slot) else {
                        panic!("shape slot held the wrong entity kind");
                    };
                    stack.resolve(
                        mapper,
                        Outcome::Shape {
                            command: command_id,
                            slot,
                            result: Ok(id),
                        },
                    );
                }
                _ => {}
            }
        }
    }

    async fn area_with_rooms(mapper: &Mapper, rooms: &[(i32, f32, f32)]) -> AreaId {
        let area_id = mapper
            .create_area_at("Test".into(), MapDestination::loose(MapStorage::Cloud))
            .await
            .expect("area");
        for (number, x, y) in rooms {
            mapper
                .upsert_room(
                    RoomKey::new(area_id, RoomNumber(*number)),
                    RoomUpdates {
                        title: Some(format!("Room {number}")),
                        x: Some(*x),
                        y: Some(*y),
                        ..Default::default()
                    },
                )
                .expect("stage room");
        }
        area_id
    }

    fn select_rooms(numbers: &[i32]) -> Selection {
        numbers
            .iter()
            .map(|n| EntityId::Room(RoomNumber(*n)))
            .collect()
    }

    fn room_pos(mapper: &Mapper, area_id: AreaId, number: i32) -> (f32, f32) {
        let atlas = mapper.get_current_atlas();
        let room = atlas
            .get_area(&area_id)
            .and_then(|area| area.get_room(&RoomNumber(number)).cloned())
            .expect("room");
        (room.get_x(), room.get_y())
    }

    #[tokio::test]
    async fn move_then_undo_restores_positions() {
        let mapper = test_mapper();
        let area_id =
            area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 1.0, 0.0), (3, 2.0, 5.0)]).await;

        let mut stack = CommandStack::default();
        let selection = select_rooms(&[1, 2, 3]);

        let command = move_selection(
            &mapper.get_current_atlas(),
            area_id,
            &selection,
            Vector::new(2.0, -1.0),
        )
        .expect("command");
        let _ = stack.push_and_apply(&mapper, command);

        assert_eq!(room_pos(&mapper, area_id, 1), (2.0, -1.0));
        assert_eq!(room_pos(&mapper, area_id, 3), (4.0, 4.0));
        assert!(stack.can_undo());

        let _ = stack.undo(&mapper);
        assert_eq!(room_pos(&mapper, area_id, 1), (0.0, 0.0));
        assert_eq!(room_pos(&mapper, area_id, 2), (1.0, 0.0));
        assert_eq!(room_pos(&mapper, area_id, 3), (2.0, 5.0));
        assert!(stack.can_redo());

        let _ = stack.redo(&mapper);
        assert_eq!(room_pos(&mapper, area_id, 1), (2.0, -1.0));
    }

    #[tokio::test]
    async fn push_clears_redo() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0)]).await;
        let mut stack = CommandStack::default();
        let selection = select_rooms(&[1]);

        let atlas = mapper.get_current_atlas();
        let command =
            move_selection(&atlas, area_id, &selection, Vector::new(1.0, 0.0)).expect("command");
        let _ = stack.push_and_apply(&mapper, command);
        let _ = stack.undo(&mapper);
        assert!(stack.can_redo());

        let atlas = mapper.get_current_atlas();
        let command =
            move_selection(&atlas, area_id, &selection, Vector::new(0.0, 1.0)).expect("command");
        let _ = stack.push_and_apply(&mapper, command);
        assert!(!stack.can_redo());
    }

    #[tokio::test]
    async fn tracked_area_operation_can_be_removed_from_history_after_discard() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0)]).await;
        let mut stack = CommandStack::default();
        let target = NewExitTarget::NewRoom {
            room_number: RoomNumber(2),
            at: iced::Point::new(2.0, 0.0),
            level: 0,
        };
        let command = create_exit_with_options(
            area_id,
            RoomNumber(1),
            ExitDirection::East,
            &target,
            ExitDirection::West,
            NewLinkOptions::default(),
        );

        let (_task, operation_ids) = stack.push_and_apply_tracked(&mapper, command);
        assert_eq!(operation_ids.len(), 1);
        assert!(stack.can_undo());
        assert!(stack.discard_operation(operation_ids[0]));
        assert!(!stack.can_undo());
        assert!(!stack.discard_operation(operation_ids[0]));
    }

    #[tokio::test]
    async fn field_edits_coalesce_keeping_first_prior() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0)]).await;
        let key = RoomKey::new(area_id, RoomNumber(1));
        let mut stack = CommandStack::default();

        for title in ["a", "ab", "abc"] {
            let command = edit_room_field(
                &mapper.get_current_atlas(),
                key.clone(),
                FieldId::Title,
                RoomUpdates {
                    title: Some(title.to_string()),
                    ..Default::default()
                },
            )
            .expect("command");
            let _ = stack.push_and_apply(&mapper, command);
        }

        assert_eq!(stack.undo.len(), 1, "rapid edits collapse to one entry");

        let _ = stack.undo(&mapper);
        let atlas = mapper.get_current_atlas();
        let title = atlas
            .get_area(&area_id)
            .and_then(|area| area.get_room(&RoomNumber(1)).cloned())
            .map(|room| room.get_title().to_string())
            .expect("room");
        assert_eq!(title, "Room 1", "undo returns to the pre-burst title");
    }

    #[tokio::test]
    async fn delete_and_undo_restores_room_properties_and_exits() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 1.0, 0.0)]).await;
        let key = RoomKey::new(area_id, RoomNumber(1));

        mapper
            .set_room_property(key.clone(), "zone".into(), "docks".into())
            .expect("stage property");
        let exit_id = mapper
            .create_exit(
                key.clone(),
                ExitArgs {
                    from_direction: ExitDirection::East,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(2)),
                    to_direction: Some(ExitDirection::West),
                    weight: 1.0,
                    ..Default::default()
                },
            )
            .await
            .expect("exit");

        let mut stack = CommandStack::default();
        let selection = select_rooms(&[1]);

        let command =
            delete_selection(&mapper.get_current_atlas(), area_id, &selection).expect("command");
        let _ = stack.push_and_apply(&mapper, command);

        {
            let atlas = mapper.get_current_atlas();
            assert!(
                atlas
                    .get_area(&area_id)
                    .and_then(|area| area.get_room(&RoomNumber(1)).cloned())
                    .is_none(),
                "room deleted"
            );
        }

        // Undo stages the recreation synchronously; settle the ready UI
        // completion by hand because this test has no iced runtime.
        let _ = stack.undo(&mapper);

        let new_exit_id = {
            let undone = stack.redo.last().expect("undone command");
            let command_id = undone.id;
            let mutations = undone.undo.clone();
            let slot = mutations
                .iter()
                .find_map(|mutation| match mutation {
                    Mutation::CreateExit { slot, .. } => Some(*slot),
                    _ => None,
                })
                .expect("exit recreation");
            let ResolvedId::Exit(id) = resolved_id(&stack, command_id, slot) else {
                panic!("exit slot held the wrong entity kind");
            };
            drive_create_completions(&mapper, &mut stack, command_id, mutations);
            id
        };
        assert_ne!(new_exit_id, exit_id, "recreated exit gets a fresh id");

        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        let room = area.get_room(&RoomNumber(1)).expect("room restored");
        assert_eq!(room.get_title(), "Room 1");
        assert_eq!(room.get_property("zone"), Some("docks"));
        assert_eq!(room.get_exits().len(), 1);
        assert_eq!(
            room.get_exits()[0].to_room_number,
            Some(RoomNumber(2)),
            "exit destination restored"
        );
    }

    fn exit_destination(
        mapper: &Mapper,
        key: &RoomKey,
        exit_id: ExitId,
    ) -> (Option<AreaId>, Option<RoomNumber>) {
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&key.area_id).expect("area");
        let room = area.get_room(&key.room_number).expect("room");
        let exit = room
            .get_exits()
            .iter()
            .find(|exit| exit.id == exit_id)
            .expect("exit");
        (exit.to_area_id, exit.to_room_number)
    }

    #[tokio::test]
    async fn clearing_exit_destination_sets_clear_to_and_undo_restores() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 1.0, 0.0)]).await;
        let key = RoomKey::new(area_id, RoomNumber(1));
        let exit_id = mapper
            .create_exit(
                key.clone(),
                ExitArgs {
                    from_direction: ExitDirection::East,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(2)),
                    to_direction: Some(ExitDirection::West),
                    weight: 1.0,
                    ..Default::default()
                },
            )
            .await
            .expect("exit");

        let command = edit_exit_field(
            &mapper.get_current_atlas(),
            key.clone(),
            exit_id,
            FieldId::Destination,
            |updates| {
                updates.to_area_id = None;
                updates.to_room_number = None;
                updates.to_direction = None;
            },
        )
        .expect("command");

        // The backend merges destination fields (omitted = unchanged), so a
        // clear that doesn't say clear_to would silently revert server-side.
        let Mutation::UpdateExit { updates, .. } = &command.redo[0] else {
            panic!("expected an exit update");
        };
        assert_eq!(updates.clear_to, Some(true), "clearing must be explicit");

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        assert_eq!(
            exit_destination(&mapper, &key, exit_id),
            (None, None),
            "destination cleared locally"
        );

        let _ = stack.undo(&mapper);
        assert_eq!(
            exit_destination(&mapper, &key, exit_id),
            (Some(area_id), Some(RoomNumber(2))),
            "undo restores the destination"
        );
    }

    #[tokio::test]
    async fn deleting_a_room_clears_then_restores_inbound_exits() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 1.0, 0.0)]).await;

        // Room 2 keeps an exit pointing at room 1.
        let host_key = RoomKey::new(area_id, RoomNumber(2));
        let inbound = mapper
            .create_exit(
                host_key.clone(),
                ExitArgs {
                    from_direction: ExitDirection::West,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(1)),
                    to_direction: Some(ExitDirection::East),
                    weight: 1.0,
                    ..Default::default()
                },
            )
            .await
            .expect("exit");

        let mut stack = CommandStack::default();
        let command = delete_selection(&mapper.get_current_atlas(), area_id, &select_rooms(&[1]))
            .expect("command");
        let _ = stack.push_and_apply(&mapper, command);

        assert_eq!(
            exit_destination(&mapper, &host_key, inbound),
            (None, None),
            "deleting room 1 clears the exit that pointed at it"
        );

        // Restoring room 1 (UpsertRooms) and re-linking the inbound exit
        // (UpdateExit) are both synchronous — room 1 had no outgoing exits to
        // recreate, so no create-completion work is needed here.
        let _ = stack.undo(&mapper);
        assert_eq!(
            exit_destination(&mapper, &host_key, inbound),
            (Some(area_id), Some(RoomNumber(1))),
            "undo re-links the inbound exit"
        );
    }

    #[tokio::test]
    async fn setting_destination_then_undo_clears_it_again() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 1.0, 0.0)]).await;
        let key = RoomKey::new(area_id, RoomNumber(1));
        let exit_id = mapper
            .create_exit(
                key.clone(),
                ExitArgs {
                    from_direction: smudgy_cloud::ExitDirection::Special,
                    weight: 1.0,
                    ..Default::default()
                },
            )
            .await
            .expect("exit");

        let command = edit_exit_field(
            &mapper.get_current_atlas(),
            key.clone(),
            exit_id,
            FieldId::Destination,
            |updates| {
                updates.to_area_id = Some(area_id);
                updates.to_room_number = Some(RoomNumber(2));
            },
        )
        .expect("command");

        // Under merge semantics the prior snapshot of an unconnected exit
        // must clear explicitly, and the redo (which establishes a
        // destination) must not carry clear_to (it overrides to_* on the
        // wire).
        let Mutation::UpdateExit { updates: redo, .. } = &command.redo[0] else {
            panic!("expected an exit update");
        };
        assert_eq!(redo.clear_to, None);
        let Mutation::UpdateExit { updates: prior, .. } = &command.undo[0] else {
            panic!("expected an exit update");
        };
        assert_eq!(prior.clear_to, Some(true));

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        assert_eq!(
            exit_destination(&mapper, &key, exit_id),
            (Some(area_id), Some(RoomNumber(2))),
            "destination set"
        );

        let _ = stack.undo(&mapper);
        assert_eq!(
            exit_destination(&mapper, &key, exit_id),
            (None, None),
            "undo unlinks the exit again"
        );
    }

    #[tokio::test]
    async fn delete_and_undo_restores_secrecy_flags() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 1.0, 0.0)]).await;
        let key = RoomKey::new(area_id, RoomNumber(1));

        let exit_id = mapper
            .create_exit(
                key.clone(),
                ExitArgs {
                    from_direction: ExitDirection::East,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(2)),
                    to_direction: Some(ExitDirection::West),
                    weight: 1.0,
                    ..Default::default()
                },
            )
            .await
            .expect("exit");
        let label_id = mapper
            .create_label(
                area_id,
                LabelArgs {
                    text: "hideout".into(),
                    color: "#fff".into(),
                    width: 2.0,
                    height: 1.0,
                    font_size: 16,
                    font_weight: 400,
                    ..Default::default()
                },
            )
            .await
            .expect("label");

        // Mark everything secret (an owned area is always cleared).
        mapper.apply_local_secret_marks(
            area_id,
            true,
            &[RoomNumber(1)],
            &[exit_id],
            &[label_id],
            &[],
            &[],
            &[],
        );

        let selection: Selection = [EntityId::Room(RoomNumber(1)), EntityId::Label(label_id)]
            .into_iter()
            .collect();
        let command =
            delete_selection(&mapper.get_current_atlas(), area_id, &selection).expect("command");

        // The recreate bodies must carry the cached secrecy flags: omitted
        // is_secret defaults to false on insert, which would silently
        // republish the entities to non-secret grantees.
        for mutation in &command.undo {
            match mutation {
                Mutation::UpsertRooms(_, rooms) => {
                    assert_eq!(rooms[0].1.is_secret, Some(true), "room keeps secrecy");
                }
                Mutation::CreateExit { args, .. } => {
                    assert_eq!(args.is_secret, Some(true), "exit keeps secrecy");
                }
                Mutation::CreateLabel { args, .. } => {
                    assert_eq!(args.is_secret, Some(true), "label keeps secrecy");
                }
                other => panic!("unexpected undo mutation: {other:?}"),
            }
        }

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        let _ = stack.undo(&mapper);

        // Settle the ready create-completion tasks dropped by this test.
        let command_id = stack.redo.last().expect("undone").id;
        let mutations = stack.redo.last().expect("undone").undo.clone();
        drive_create_completions(&mapper, &mut stack, command_id, mutations);

        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        let room = area.get_room(&RoomNumber(1)).expect("room restored");
        assert!(room.is_secret(), "room secrecy restored");
        assert!(room.get_exits()[0].is_secret, "exit secrecy restored");
        assert!(area.get_labels()[0].is_secret, "label secrecy restored");
    }

    /// Links room 1 → room 2 (two-way) and returns the connection id.
    async fn link_rooms(
        mapper: &Mapper,
        stack: &mut CommandStack,
        area_id: AreaId,
        from: i32,
        to: i32,
    ) -> ConnectionId {
        let command = create_exit_with_options(
            area_id,
            RoomNumber(from),
            ExitDirection::East,
            &NewExitTarget::Room(RoomNumber(to)),
            ExitDirection::West,
            NewLinkOptions::default(),
        );
        let _ = stack.push_and_apply(mapper, command);
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        area.get_connections()
            .iter()
            .find(|connection| {
                connection.endpoint_a.room_number == RoomNumber(from)
                    || connection
                        .endpoint_b
                        .is_some_and(|endpoint| endpoint.room_number == RoomNumber(from))
            })
            .expect("connection")
            .id
    }

    #[tokio::test]
    async fn multi_delete_removes_selected_connection_and_undo_restores_it() {
        let mapper = test_mapper();
        let area_id =
            area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 4.0, 0.0), (3, 8.0, 0.0)]).await;
        let mut stack = CommandStack::default();
        let connection_id = link_rooms(&mapper, &mut stack, area_id, 1, 2).await;

        // Room 3 plus the link — a mixed selection whose connection used to
        // be silently skipped.
        let selection: Selection = [
            EntityId::Room(RoomNumber(3)),
            EntityId::Connection(connection_id),
        ]
        .into_iter()
        .collect();
        let command =
            delete_selection(&mapper.get_current_atlas(), area_id, &selection).expect("command");
        let _ = stack.push_and_apply(&mapper, command);

        {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&area_id).expect("area");
            assert!(area.get_room(&RoomNumber(3)).is_none(), "room deleted");
            assert!(
                area.get_connection(connection_id).is_none(),
                "explicitly selected link deleted"
            );
            assert!(
                area.get_room(&RoomNumber(1))
                    .is_some_and(|room| room.get_exits().is_empty()),
                "member exits deleted with the link"
            );
        }

        let _ = stack.undo(&mapper);
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        assert!(area.get_room(&RoomNumber(3)).is_some(), "room restored");
        assert!(
            area.get_connection(connection_id).is_some(),
            "link restored with its identity"
        );
        let exits: Vec<_> = area
            .get_rooms()
            .iter()
            .flat_map(|room| room.get_exits())
            .filter(|exit| exit.connection_id == connection_id)
            .collect();
        assert_eq!(exits.len(), 2, "both member exits restored exactly once");
    }

    #[tokio::test]
    async fn deleting_a_link_with_one_of_its_rooms_restores_cleanly() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 4.0, 0.0)]).await;
        let mut stack = CommandStack::default();
        let connection_id = link_rooms(&mapper, &mut stack, area_id, 1, 2).await;

        // One endpoint room plus the link: the surviving room's member exit
        // is restored by the link path, so the undo must NOT also carry an
        // inbound-exit relink for it — that UpdateExit would target an exit
        // the DeleteLink removed and wedge the sync queue.
        let selection: Selection = [
            EntityId::Room(RoomNumber(1)),
            EntityId::Connection(connection_id),
        ]
        .into_iter()
        .collect();
        let command =
            delete_selection(&mapper.get_current_atlas(), area_id, &selection).expect("command");
        assert!(
            !command
                .undo
                .iter()
                .any(|mutation| matches!(mutation, Mutation::UpdateExit { .. })),
            "no doomed relink for a link-restored exit"
        );
        let _ = stack.push_and_apply(&mapper, command);
        {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&area_id).expect("area");
            assert!(area.get_room(&RoomNumber(1)).is_none());
            assert!(area.get_connection(connection_id).is_none());
            assert!(
                area.get_room(&RoomNumber(2))
                    .is_some_and(|room| room.get_exits().is_empty()),
                "surviving room's member exit deleted with the link"
            );
        }

        let _ = stack.undo(&mapper);
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        assert!(area.get_room(&RoomNumber(1)).is_some(), "room restored");
        assert!(
            area.get_connection(connection_id).is_some(),
            "link restored"
        );
        let exits: Vec<_> = area
            .get_rooms()
            .iter()
            .flat_map(|room| room.get_exits())
            .filter(|exit| exit.connection_id == connection_id)
            .collect();
        assert_eq!(exits.len(), 2, "both member exits restored exactly once");
        assert!(
            exits.iter().all(|exit| exit.to_room_number.is_some()),
            "restored exits keep their destinations"
        );
    }

    #[tokio::test]
    async fn connection_only_paste_attaches_to_same_numbered_rooms_once() {
        let mapper = test_mapper();
        let source = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 4.0, 0.0)]).await;
        let mut stack = CommandStack::default();
        let connection_id = link_rooms(&mapper, &mut stack, source, 1, 2).await;

        let selection: Selection = [EntityId::Connection(connection_id)].into_iter().collect();
        let clipboard =
            snapshot_selection(&mapper.get_current_atlas(), source, &selection, true, false);
        assert!(clipboard.rooms.is_empty());
        assert_eq!(
            clipboard.connections.len(),
            1,
            "an explicitly selected link snapshots without its rooms"
        );

        let target = area_with_rooms(&mapper, &[(1, 100.0, 0.0), (2, 104.0, 0.0)]).await;
        let (command, pasted, skipped) = paste_clipboard(
            &mapper.get_current_atlas(),
            target,
            &clipboard,
            0,
            Vector::new(0.0, 0.0),
            None,
        );
        assert!(pasted.is_empty());
        assert_eq!(skipped, 0);
        let _ = stack.push_and_apply(&mapper, command.expect("command"));
        {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&target).expect("area");
            assert_eq!(area.get_connections().len(), 1, "link attached");
            let exits: Vec<_> = area
                .get_rooms()
                .iter()
                .flat_map(|room| room.get_exits())
                .collect();
            assert_eq!(exits.len(), 2, "both traversals attached");
        }

        // Pasting again would collide with the directions just created:
        // the link skips (with a count) instead of duplicating exits.
        let (command, _, skipped) = paste_clipboard(
            &mapper.get_current_atlas(),
            target,
            &clipboard,
            0,
            Vector::new(0.0, 0.0),
            None,
        );
        assert!(command.is_none(), "nothing pastes");
        assert_eq!(skipped, 1);
    }

    #[tokio::test]
    async fn paste_creates_offset_copies_and_undo_removes_them() {
        let mapper = test_mapper();
        let area_id = mapper
            .create_area_at("Test".into(), MapDestination::loose(MapStorage::Cloud))
            .await
            .expect("area");

        let clipboard = EntityClipboard {
            source_area_id: Some(area_id),
            rooms: vec![],
            connections: vec![],
            connection_origin: None,
            labels: vec![LabelArgs {
                level: 0,
                x: 1.0,
                y: 2.0,
                width: 3.0,
                height: 1.0,
                text: "dock".into(),
                color: "#fff".into(),
                font_size: 16,
                font_weight: 400,
                ..Default::default()
            }],
            shapes: vec![ShapeArgs {
                level: 0,
                x: 5.0,
                y: 5.0,
                width: 2.0,
                height: 2.0,
                background_color: Some("#333".into()),
                ..Default::default()
            }],
        };

        let mut stack = CommandStack::default();
        let (command, pasted_rooms, _) = paste_clipboard(
            &mapper.get_current_atlas(),
            area_id,
            &clipboard,
            3,
            Vector::new(1.0, 1.0),
            None,
        );
        let command = command.expect("command");
        assert!(pasted_rooms.is_empty());
        let (_task, operation_ids) = stack.push_and_apply_tracked(&mapper, command);
        assert_eq!(
            operation_ids.len(),
            2,
            "both creates are represented in durable undo history"
        );

        assert!(!stack.can_undo(), "pending creates block undo");

        // Settle the ready completion tasks dropped by this test.
        let command_id = stack.undo.back().expect("pushed").id;
        let mutations = stack.undo.back().expect("pushed").redo.clone();
        drive_create_completions(&mapper, &mut stack, command_id, mutations);

        {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&area_id).expect("area");
            let label = &area.get_labels()[0];
            assert_eq!((label.x, label.y), (2.0, 3.0), "label pasted at offset");
            assert_eq!(label.level, 3, "label pasted onto the current level");
            assert_eq!(label.text, "dock", "styling survives the round trip");
            let shape = &area.get_shapes()[0];
            assert_eq!((shape.x, shape.y), (6.0, 6.0), "shape pasted at offset");
        }

        assert!(stack.can_undo(), "resolution unblocks undo");
        let _ = stack.undo(&mapper);

        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        assert!(area.get_labels().is_empty(), "undo removes pasted label");
        assert!(area.get_shapes().is_empty(), "undo removes pasted shape");
    }

    #[tokio::test]
    async fn transparent_styling_survives_create_snapshot_and_paste() {
        let mapper = test_mapper();
        let area_id = mapper
            .create_area_at("Test".into(), MapDestination::loose(MapStorage::Cloud))
            .await
            .expect("area");

        // The drag-rect builder must request transparency explicitly: the
        // mock (like the deployed server) turns absent backgrounds white.
        let command = create_label(
            area_id,
            iced::Rectangle {
                x: 0.0,
                y: 0.0,
                width: 4.0,
                height: 1.0,
            },
            0,
        );
        let Mutation::CreateLabel { args, .. } = command.redo[0].clone() else {
            panic!("expected a label create");
        };
        let label_id = mapper.create_label(area_id, args).await.expect("label");

        {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&area_id).expect("area");
            let label = area.get_label(&label_id).expect("label");
            assert_eq!(
                label.background_color, "",
                "new labels default to a transparent background"
            );
        }

        // Snapshot keeps transparency explicit so paste re-creates it.
        let selection: Selection = [EntityId::Label(label_id)].into_iter().collect();
        let clipboard = snapshot_selection(
            &mapper.get_current_atlas(),
            area_id,
            &selection,
            false,
            false,
        );
        assert_eq!(
            clipboard.labels[0].background_color.as_deref(),
            Some(""),
            "snapshot must not erase the transparent background"
        );

        let (command, _, _) = paste_clipboard(
            &mapper.get_current_atlas(),
            area_id,
            &clipboard,
            0,
            Vector::new(1.0, 1.0),
            None,
        );
        let command = command.expect("paste command");
        let Mutation::CreateLabel { args, .. } = command.redo[0].clone() else {
            panic!("expected a label create");
        };
        let pasted_id = mapper.create_label(area_id, args).await.expect("pasted");

        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        assert_eq!(
            area.get_label(&pasted_id).expect("pasted").background_color,
            "",
            "pasted labels keep their transparent background"
        );
    }

    #[test]
    fn cross_area_remap_keeps_vacant_numbers_and_reallocates_collisions() {
        let occupied: HashSet<RoomNumber> = [RoomNumber(2)].into_iter().collect();
        let mapping = remap_room_numbers(
            &[RoomNumber(3), RoomNumber(2), RoomNumber(10)],
            &occupied,
            RoomNumber(3),
            true,
        );

        assert_eq!(mapping[&RoomNumber(3)], RoomNumber(3), "vacant number kept");
        assert_eq!(
            mapping[&RoomNumber(2)],
            RoomNumber(4),
            "occupied number reallocates, skipping the kept 3"
        );
        assert_eq!(
            mapping[&RoomNumber(10)],
            RoomNumber(10),
            "vacant number kept"
        );
    }

    #[test]
    fn cross_area_remap_allocations_skip_numbers_claimed_by_the_paste() {
        let occupied: HashSet<RoomNumber> = [RoomNumber(1)].into_iter().collect();
        let mapping = remap_room_numbers(
            &[RoomNumber(2), RoomNumber(1)],
            &occupied,
            RoomNumber(2),
            true,
        );

        assert_eq!(mapping[&RoomNumber(2)], RoomNumber(2));
        assert_eq!(
            mapping[&RoomNumber(1)],
            RoomNumber(3),
            "allocation skips the number the paste already claimed"
        );
    }

    #[test]
    fn same_area_remap_always_allocates_fresh_numbers() {
        let occupied: HashSet<RoomNumber> = [RoomNumber(1), RoomNumber(2)].into_iter().collect();
        let source = [RoomNumber(1), RoomNumber(2)];
        let mapping = remap_room_numbers(&source, &occupied, RoomNumber(3), false);

        assert_eq!(mapping[&RoomNumber(1)], RoomNumber(3));
        assert_eq!(mapping[&RoomNumber(2)], RoomNumber(4));
        for target in mapping.values() {
            assert!(!occupied.contains(target), "paste never overwrites a room");
        }
    }

    fn exit_clip(
        to_area_id: Option<AreaId>,
        to_room_number: Option<RoomNumber>,
        to_unknown: bool,
    ) -> ExitClip {
        ExitClip {
            from_direction: ExitDirection::North,
            to_area_id,
            to_room_number,
            to_direction: Some(ExitDirection::South),
            path: None,
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: None,
            is_secret: false,
            to_unknown,
        }
    }

    #[test]
    fn pasted_exits_classify_per_destination() {
        let source = AreaId(Uuid::from_u128(1));
        let third = AreaId(Uuid::from_u128(2));
        let missing = AreaId(Uuid::from_u128(3));
        let mapping: HashMap<RoomNumber, RoomNumber> =
            [(RoomNumber(1), RoomNumber(7))].into_iter().collect();
        let in_cache = |id: AreaId| id == source || id == third;

        // (a) intra-selection: remapped through the mapping...
        assert_eq!(
            classify_pasted_exit(
                &exit_clip(Some(source), Some(RoomNumber(1)), false),
                source,
                &mapping,
                in_cache,
            ),
            PastedExitDestination::Remapped(RoomNumber(7)),
        );
        // ...including a bare room number meaning "same area".
        assert_eq!(
            classify_pasted_exit(
                &exit_clip(None, Some(RoomNumber(1)), false),
                source,
                &mapping,
                in_cache,
            ),
            PastedExitDestination::Remapped(RoomNumber(7)),
        );
        // (b) a cached third area stays a live link, untouched.
        assert_eq!(
            classify_pasted_exit(
                &exit_clip(Some(third), Some(RoomNumber(9)), false),
                source,
                &mapping,
                in_cache,
            ),
            PastedExitDestination::Live(third, RoomNumber(9)),
        );
        // (c) a non-selected room in the source area pastes dangling.
        assert_eq!(
            classify_pasted_exit(
                &exit_clip(Some(source), Some(RoomNumber(2)), false),
                source,
                &mapping,
                in_cache,
            ),
            PastedExitDestination::Dangling,
        );
        // (c) a redacted destination pastes dangling even when its room
        // number would remap.
        assert_eq!(
            classify_pasted_exit(
                &exit_clip(Some(source), Some(RoomNumber(1)), true),
                source,
                &mapping,
                in_cache,
            ),
            PastedExitDestination::Dangling,
        );
        // (c) a destination area absent from the cache pastes dangling.
        assert_eq!(
            classify_pasted_exit(
                &exit_clip(Some(missing), Some(RoomNumber(9)), false),
                source,
                &mapping,
                in_cache,
            ),
            PastedExitDestination::Dangling,
        );
        // (c) unconnected exits stay unconnected.
        assert_eq!(
            classify_pasted_exit(&exit_clip(None, None, false), source, &mapping, in_cache),
            PastedExitDestination::Dangling,
        );
    }

    /// Settles the ready exit-create completions from a just-pushed paste.
    fn drive_paste_exit_creates(mapper: &Mapper, stack: &mut CommandStack) {
        let command_id = stack.undo.back().expect("pushed").id;
        let mutations = stack.undo.back().expect("pushed").redo.clone();
        drive_create_completions(mapper, stack, command_id, mutations);
    }

    #[tokio::test]
    async fn cross_area_paste_preserves_vacant_numbers_and_remaps_exits() {
        let mapper = test_mapper();
        let source = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 3.0, 0.0), (3, 6.0, 0.0)]).await;
        // Room 2 is taken in the target; room 1 is vacant there.
        let target = area_with_rooms(&mapper, &[(2, 50.0, 50.0)]).await;

        mapper
            .set_room_property(
                RoomKey::new(source, RoomNumber(1)),
                "zone".into(),
                "docks".into(),
            )
            .expect("stage property");
        // 1 → 2: both ends copied. 1 → 3 is a boundary link and is omitted.
        for (direction, to) in [(ExitDirection::East, 2), (ExitDirection::North, 3)] {
            mapper
                .create_exit(
                    RoomKey::new(source, RoomNumber(1)),
                    ExitArgs {
                        from_direction: direction,
                        to_area_id: Some(source),
                        to_room_number: Some(RoomNumber(to)),
                        to_direction: Some(ExitDirection::West),
                        weight: 1.0,
                        ..Default::default()
                    },
                )
                .await
                .expect("exit");
        }
        let contained_connection_id = {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&source).expect("source");
            area.get_room(&RoomNumber(1))
                .expect("room 1")
                .get_exits()
                .iter()
                .find(|exit| exit.from_direction == ExitDirection::East)
                .expect("contained exit")
                .connection_id
        };
        mapper
            .mutate_area(
                source,
                vec![AreaMutation::UpdateConnection {
                    connection_id: contained_connection_id,
                    body: ConnectionUpdates {
                        routing: Some(ConnectionRouting::Manual),
                        segment_shape: Some(SegmentShape::Direct),
                        route_points: Some(vec![smudgy_cloud::MapPoint::new(1.5, 1.0)]),
                        ..ConnectionUpdates::default()
                    },
                }],
                "Route contained clipboard connection",
            )
            .expect("route update");

        let clipboard = snapshot_selection(
            &mapper.get_current_atlas(),
            source,
            &select_rooms(&[1, 2]),
            true,
            false,
        );
        assert_eq!(clipboard.source_area_id, Some(source));
        assert_eq!(clipboard.rooms.len(), 2);
        assert_eq!(clipboard.connections.len(), 1);
        assert_eq!(
            clipboard.connections[0].body.route_points,
            vec![smudgy_cloud::MapPoint::new(1.5, 1.0)],
            "route geometry is stored relative to the selected-room origin"
        );
        assert_eq!(
            boundary_link_count(&mapper.get_current_atlas(), source, &select_rooms(&[1, 2])),
            1
        );
        let with_boundary = snapshot_selection(
            &mapper.get_current_atlas(),
            source,
            &select_rooms(&[1, 2]),
            true,
            true,
        );
        assert_eq!(with_boundary.connections.len(), 2);
        let dangling = with_boundary
            .connections
            .iter()
            .find(|clip| clip.members[0].1.from_direction == ExitDirection::North)
            .expect("included boundary link");
        assert!(dangling.body.endpoint_b.is_none());
        assert!(dangling.body.route_points.is_empty());
        assert_eq!(dangling.members[0].1.to_area_id, None);
        assert_eq!(dangling.members[0].1.to_room_number, None);

        let (command, pasted, _) = paste_clipboard(
            &mapper.get_current_atlas(),
            target,
            &clipboard,
            0,
            Vector::new(0.0, 0.0),
            None,
        );
        let command = command.expect("command");
        // Room 1 keeps its number (vacant in the target); room 2 collides
        // with the target's own room 2 and reallocates.
        assert_eq!(pasted, vec![RoomNumber(1), RoomNumber(3)]);

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        drive_paste_exit_creates(&mapper, &mut stack);

        {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&target).expect("target");
            let room = area.get_room(&RoomNumber(1)).expect("pasted room 1");
            assert_eq!(room.get_title(), "Room 1");
            assert_eq!(
                (room.get_x(), room.get_y()),
                (0.0, 0.0),
                "cross-area paste keeps exact positions"
            );
            assert_eq!(
                room.get_property("zone"),
                Some("docks"),
                "properties recreated on the copy"
            );

            let exits = room.get_exits();
            assert_eq!(exits.len(), 1);
            let to_copied = exits
                .iter()
                .find(|exit| exit.from_direction == ExitDirection::East)
                .expect("east exit");
            assert_eq!(
                (to_copied.to_area_id, to_copied.to_room_number),
                (Some(target), Some(RoomNumber(3))),
                "intra-selection exit remapped to the pasted copy"
            );
            assert!(
                exits
                    .iter()
                    .all(|exit| exit.from_direction != ExitDirection::North),
                "boundary exits are omitted unless the user explicitly includes them"
            );

            let existing = area.get_room(&RoomNumber(2)).expect("target room 2");
            assert_eq!(
                (existing.get_x(), existing.get_y()),
                (50.0, 50.0),
                "the target's own room is untouched"
            );
            let pasted_connection = area
                .get_connections()
                .iter()
                .find(|connection| connection.id != contained_connection_id)
                .expect("pasted connection");
            assert_eq!(
                pasted_connection.route_points,
                vec![smudgy_cloud::MapPoint::new(1.5, 1.0)],
                "cross-area paste restores absolute route geometry"
            );
        }

        // One undo removes the entire paste; pre-existing rooms survive.
        assert!(stack.can_undo());
        let _ = stack.undo(&mapper);
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&target).expect("target");
        assert!(area.get_room(&RoomNumber(1)).is_none());
        assert!(area.get_room(&RoomNumber(3)).is_none());
        assert!(area.get_room(&RoomNumber(2)).is_some());
    }

    #[tokio::test]
    async fn same_area_paste_allocates_fresh_numbers_and_links_inside_the_copy() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 1.0, 0.0)]).await;
        mapper
            .create_exit(
                RoomKey::new(area_id, RoomNumber(1)),
                ExitArgs {
                    from_direction: ExitDirection::East,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(2)),
                    to_direction: Some(ExitDirection::West),
                    weight: 1.0,
                    ..Default::default()
                },
            )
            .await
            .expect("exit");

        let clipboard = snapshot_selection(
            &mapper.get_current_atlas(),
            area_id,
            &select_rooms(&[1, 2]),
            true,
            false,
        );
        let (command, pasted, _) = paste_clipboard(
            &mapper.get_current_atlas(),
            area_id,
            &clipboard,
            0,
            Vector::new(1.0, 1.0),
            None,
        );
        let command = command.expect("command");
        assert_eq!(
            pasted,
            vec![RoomNumber(3), RoomNumber(4)],
            "same-area paste never reuses source numbers"
        );

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        drive_paste_exit_creates(&mapper, &mut stack);

        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        let copy = area.get_room(&RoomNumber(3)).expect("copy of room 1");
        assert_eq!(
            (copy.get_x(), copy.get_y()),
            (1.0, 1.0),
            "the cascading offset applies to rooms"
        );
        let exit = &copy.get_exits()[0];
        assert_eq!(
            (exit.to_area_id, exit.to_room_number),
            (Some(area_id), Some(RoomNumber(4))),
            "the copied link points inside the copy"
        );
        assert_eq!(
            area.get_room(&RoomNumber(1)).expect("original").get_exits()[0].to_room_number,
            Some(RoomNumber(2)),
            "the original link is untouched"
        );
    }

    #[tokio::test]
    async fn pending_create_blocks_undo() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0)]).await;

        let mut command = Command::new(
            vec![Mutation::CreateLabel {
                area_id,
                args: LabelArgs::default(),
                slot: 0,
            }],
            vec![Mutation::DeleteLabel {
                area_id,
                id: IdRef::Slot(0),
            }],
        );
        let mut stack = CommandStack::default();
        let _ = CommandStack::apply(&mapper, &mut command, Direction::Redo);
        assert_eq!(command.pending, 1);
        command.id = 7;
        stack.undo.push_back(command);

        assert!(!stack.can_undo(), "pending create blocks undo");

        let ResolvedId::Label(id) = resolved_id(&stack, 7, 0) else {
            panic!("label slot held the wrong entity kind");
        };
        stack.resolve(
            &mapper,
            Outcome::Label {
                command: 7,
                slot: 0,
                result: Ok(id),
            },
        );
        assert!(stack.can_undo(), "resolution unblocks undo");
    }

    #[tokio::test]
    async fn port_redistribution_is_previewed_as_one_undoable_batch() {
        let mapper = test_mapper();
        let area_id =
            area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, -3.0, -3.0), (3, 3.0, -3.0)]).await;
        for to in [2, 3] {
            mapper
                .create_exit(
                    RoomKey::new(area_id, RoomNumber(1)),
                    ExitArgs {
                        from_direction: ExitDirection::North,
                        to_area_id: Some(area_id),
                        to_room_number: Some(RoomNumber(to)),
                        to_direction: Some(ExitDirection::South),
                        ..ExitArgs::default()
                    },
                )
                .await
                .expect("exit");
        }
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        let edits = super::super::inspector::redistribute_port_updates(
            &area,
            RoomNumber(1),
            RoomSide::North,
            false,
        );
        assert_eq!(edits.len(), 2);
        let mut offsets = edits
            .iter()
            .filter_map(|(_, update)| update.endpoint_a.map(|endpoint| endpoint.port_offset))
            .collect::<Vec<_>>();
        offsets.sort_by(f32::total_cmp);
        assert!((offsets[0] - smudgy_cloud::CORNER_INSET).abs() < 1.0e-6);
        assert!((offsets[1] - (1.0 - smudgy_cloud::CORNER_INSET)).abs() < 1.0e-6);

        let command = edit_connections(&atlas, area_id, edits, "Redistribute ports")
            .expect("one compound command");
        assert!(matches!(
            &command.redo[..],
            [Mutation::AreaBatch { operations, .. }] if operations.len() == 2
        ));
        assert!(matches!(
            &command.undo[..],
            [Mutation::AreaBatch { operations, .. }] if operations.len() == 2
        ));
    }

    #[tokio::test]
    async fn accepted_automatic_route_is_one_atomic_update() {
        let mapper = test_mapper();
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 4.0, 0.0)]).await;
        mapper
            .create_exit(
                RoomKey::new(area_id, RoomNumber(1)),
                ExitArgs {
                    from_direction: ExitDirection::East,
                    to_area_id: Some(area_id),
                    to_room_number: Some(RoomNumber(2)),
                    to_direction: Some(ExitDirection::West),
                    ..ExitArgs::default()
                },
            )
            .await
            .expect("exit");
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        let connection_id = area.get_connections()[0].id;
        let points = vec![smudgy_cloud::MapPoint::new(2.0, 0.0)];
        let command = accept_automatic_route(&atlas, area_id, connection_id, points.clone())
            .expect("route command");

        assert!(matches!(
            &command.redo[..],
            [Mutation::AreaBatch { operations, .. }]
                if matches!(
                    &operations[..],
                    [AreaMutation::UpdateConnection { connection_id: id, body }]
                        if *id == connection_id
                            && body.routing == Some(ConnectionRouting::Automatic)
                            && body.segment_shape == Some(SegmentShape::Orthogonal)
                            && body.route_points.as_ref() == Some(&points)
                )
        ));
        assert!(matches!(
            &command.undo[..],
            [Mutation::AreaBatch { operations, .. }] if operations.len() == 1
        ));
    }
}
