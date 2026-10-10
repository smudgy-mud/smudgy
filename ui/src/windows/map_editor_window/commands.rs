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

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use iced::{Task, Vector};
use smudgy_cloud::{
    AreaId, ConnectionArgs, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionRouting,
    ConnectionUpdates, CornerStyle, DEFAULT_CONNECTION_COLOR, DEFAULT_CONNECTION_THICKNESS,
    ExitArgs, ExitDirection, ExitId, ExitUpdates, LabelArgs, LabelId, LabelUpdates, Mapper,
    PortMode, RoomAddress, RoomNumber, RoomUpdates, SegmentShape, ShapeArgs, ShapeId, ShapeUpdates,
    Uuid, default_anchor_for_direction,
    mapper::{AreaMutationBatch, AtlasCache, MutationSubmission, RoomKey},
    mutation::{AreaMutation, MAX_MUTATION_OPERATIONS, OperationId},
};
use smudgy_map_widget::map_editor::{EntityId, PlacedRoom, Selection};

use super::document::Document;
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
    /// The same, written to one of the map's other sources (a Secret or
    /// Private additions). Longer than one envelope, it continues in order.
    SourceBatch {
        area_id: AreaId,
        source: smudgy_cloud::SourceId,
        operations: Vec<AreaMutation>,
        description: String,
        /// An exit edit that changes one end of a two-way link splits the
        /// link first, as [`Mutation::UpdateExit`] does on the map.
        split_paired_exit: bool,
    },
    UpsertRooms(AreaId, Vec<(RoomNumber, RoomUpdates)>),
    DeleteRoom(RoomKey),
    SetRoomProperty(RoomKey, String, String),
    DeleteRoomProperty(RoomKey, String),
    SetAreaProperty(AreaId, String, String),
    DeleteAreaProperty(AreaId, String),
    /// One of the map's exits; a direction edit on one end of a two-way
    /// link splits the link. Exits are created and deleted in batches.
    UpdateExit {
        room_key: RoomKey,
        id: ExitId,
        updates: ExitUpdates,
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
            Mutation::CreateLabel { slot, .. } | Mutation::CreateShape { slot, .. } => slot + 1,
            Mutation::UpdateLabel {
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

/// Where an operation writes, as [`needed_actions`] reports it: a place,
/// or the place holding a label or shape (read from the map when judged).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writes {
    Place(smudgy_cloud::SourceId),
    Label(LabelId),
    Shape(ShapeId),
}

/// The map `mutation` writes, and each action its operations need there
/// (`add`, `edit` or `remove`, as the server judges each operation), with
/// where it needs it.
#[must_use]
pub fn needed_actions(mutation: &Mutation) -> (AreaId, Vec<(Writes, &'static str)>) {
    use smudgy_cloud::SourceId;
    let map = |action| vec![(Writes::Place(SourceId::Map), action)];
    match mutation {
        Mutation::AreaBatch {
            area_id,
            operations,
            ..
        } => (
            *area_id,
            operations
                .iter()
                .map(|operation| (Writes::Place(SourceId::Map), operation.required_action()))
                .collect(),
        ),
        Mutation::SourceBatch {
            area_id,
            source,
            operations,
            ..
        } => (
            *area_id,
            operations
                .iter()
                .map(|operation| (Writes::Place(*source), operation.required_action()))
                .collect(),
        ),
        Mutation::UpsertRooms(area_id, _)
        | Mutation::SetAreaProperty(area_id, ..)
        | Mutation::UpdateLabel {
            area_id,
            id: IdRef::Slot(_),
            ..
        }
        | Mutation::UpdateShape {
            area_id,
            id: IdRef::Slot(_),
            ..
        } => (*area_id, map("edit")),
        Mutation::SetRoomProperty(room_key, ..) | Mutation::UpdateExit { room_key, .. } => {
            (room_key.area_id, map("edit"))
        }
        Mutation::DeleteRoom(room_key) | Mutation::DeleteRoomProperty(room_key, _) => {
            (room_key.area_id, map("remove"))
        }
        Mutation::DeleteAreaProperty(area_id, _)
        | Mutation::DeleteLabel {
            area_id,
            id: IdRef::Slot(_),
        }
        | Mutation::DeleteShape {
            area_id,
            id: IdRef::Slot(_),
        } => (*area_id, map("remove")),
        Mutation::CreateLabel { area_id, .. } | Mutation::CreateShape { area_id, .. } => {
            (*area_id, map("add"))
        }
        Mutation::UpdateLabel {
            area_id,
            id: IdRef::Known(id),
            ..
        } => (*area_id, vec![(Writes::Label(*id), "edit")]),
        Mutation::DeleteLabel {
            area_id,
            id: IdRef::Known(id),
        } => (*area_id, vec![(Writes::Label(*id), "remove")]),
        Mutation::UpdateShape {
            area_id,
            id: IdRef::Known(id),
            ..
        } => (*area_id, vec![(Writes::Shape(*id), "edit")]),
        Mutation::DeleteShape {
            area_id,
            id: IdRef::Known(id),
        } => (*area_id, vec![(Writes::Shape(*id), "remove")]),
    }
}

/// The entity a coalescable field edit targets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EntityRef {
    Area(AreaId),
    Room(RoomKey),
    SourceRoom(AreaId, smudgy_cloud::SourceId, RoomNumber),
    /// A source's data on a qualified room owned by another source.
    RoomData(
        AreaId,
        smudgy_cloud::SourceId,
        smudgy_cloud::SourceId,
        RoomNumber,
    ),
    /// A Secret's or Private's own fields, on its map.
    Place(AreaId, smudgy_cloud::SourceId),
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
    Weight,
    Command,
    Flags,
    /// A door's name.
    DoorName,
    /// The command that opens a door.
    DoorOpensWith,
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
    /// How many of the last writes of each direction are follow-ups
    /// ([`Command::then`]): writes to other maps made only once the writes
    /// before them are acknowledged.
    follow_ups: usize,
    /// Whether the other maps hold the redo follow-ups (rather than their
    /// undo, or neither yet).
    followed: bool,
    /// The application whose follow-ups wait for its own writes.
    awaiting: Option<Awaiting>,
    /// Counts applications, so an acknowledgement answers only its own.
    applications: u64,
}

/// An application of a command waiting for its own writes to be
/// acknowledged before its follow-ups go.
#[derive(Debug, Clone)]
struct Awaiting {
    application: u64,
    direction: Direction,
    areas: Vec<AreaId>,
    operations: Vec<OperationId>,
}

impl Command {
    /// The mutations applying the command writes.
    #[must_use]
    pub fn redo_mutations(&self) -> &[Mutation] {
        &self.redo
    }

    /// The mutations undoing the command writes.
    #[must_use]
    pub fn undo_mutations(&self) -> &[Mutation] {
        &self.undo
    }

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
            follow_ups: 0,
            followed: false,
            awaiting: None,
            applications: 0,
        }
    }

    /// The command with one more write after its own (`redo`), undone
    /// after its own undo (`undo`); it coalesces as before.
    #[must_use]
    pub fn also(mut self, redo: Mutation, undo: Mutation) -> Self {
        self.redo.insert(self.redo.len() - self.follow_ups, redo);
        self.undo.insert(self.undo.len() - self.follow_ups, undo);
        self
    }

    /// The command with a follow-up: a write to another map (`redo`) made
    /// only once the command's own writes are acknowledged, and its undo
    /// (`undo`) made likewise once the command's own undo is. A write of
    /// the command's own that is refused and discarded never leaves the
    /// other map's write to go alone; an undo or redo before the
    /// acknowledgement leaves the other map as it is.
    #[must_use]
    pub fn then(mut self, redo: Mutation, undo: Mutation) -> Self {
        self.redo.push(redo);
        self.undo.push(undo);
        self.follow_ups += 1;
        self
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
    /// Whether one application's own writes were all acknowledged, for
    /// [`CommandStack::follow_up`].
    Acknowledged {
        command: CommandId,
        application: u64,
        acknowledged: bool,
    },
}

#[derive(Debug, Clone, Copy)]
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

    /// The command undo would revert next.
    #[must_use]
    pub fn next_undo(&self) -> Option<&Command> {
        self.undo.back()
    }

    /// The command redo would apply next.
    #[must_use]
    pub fn next_redo(&self) -> Option<&Command> {
        self.redo.last()
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
            && command.follow_ups == 0
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

    /// The operations the command that submitted `operation_id` submitted
    /// after it: the rest of that gesture, which goes too when that
    /// operation is discarded.
    #[must_use]
    pub fn operations_after(&self, operation_id: OperationId) -> Vec<OperationId> {
        self.undo
            .iter()
            .chain(self.redo.iter())
            .find_map(|command| {
                let position = command
                    .operation_ids
                    .iter()
                    .position(|id| *id == operation_id)?;
                Some(command.operation_ids[position + 1..].to_vec())
            })
            .unwrap_or_default()
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
    pub fn resolve(&mut self, outcome: Outcome) {
        match outcome {
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
            // Settled by [`Self::follow_up`], which writes.
            Outcome::Acknowledged { .. } => {}
        }
    }

    /// Makes the follow-ups ([`Command::then`]) of application
    /// `application` of command `command` once its own writes are
    /// `acknowledged`. Without the acknowledgement (a write refused and
    /// discarded, or cancelled), or once the command was undone, redone or
    /// dropped from history since, the other maps stay as they are.
    pub fn follow_up(
        &mut self,
        mapper: &Mapper,
        command: CommandId,
        application: u64,
        acknowledged: bool,
    ) -> Task<Outcome> {
        self.last_error = None;
        let Some(entry) = self.find_mut(command) else {
            return Task::none();
        };
        let Some(awaiting) = entry
            .awaiting
            .take_if(|awaiting| awaiting.application == application)
        else {
            return Task::none();
        };
        if !acknowledged {
            return Task::none();
        }
        let mutations = match awaiting.direction {
            Direction::Redo => &entry.redo,
            Direction::Undo => &entry.undo,
        };
        let mut writes = Command::new(
            mutations[mutations.len() - entry.follow_ups..].to_vec(),
            Vec::new(),
        );
        let (task, applied) = Self::apply(mapper, &mut writes, Direction::Redo);
        if applied {
            entry.operation_ids.extend(writes.operation_ids);
            entry.followed = matches!(awaiting.direction, Direction::Redo);
        }
        self.last_error = writes.application_error.take();
        task
    }

    /// Queues every follow-up still waiting for an acknowledgement at once,
    /// beside the writes it follows, as the window closes: no window is left
    /// to wait, and a follow-up never made would leave the other map as it
    /// was for good. Its map's durable queue then holds it like any write.
    pub fn queue_waiting_follow_ups(&mut self, mapper: &Mapper) {
        let waiting: Vec<(CommandId, u64)> = self
            .undo
            .iter()
            .chain(&self.redo)
            .filter_map(|command| {
                let awaiting = command.awaiting.as_ref()?;
                Some((command.id, awaiting.application))
            })
            .collect();
        for (command, application) in waiting {
            // The window closes: what the writes would report is answered
            // by no one.
            let _ = self.follow_up(mapper, command, application, true);
            if let Some(error) = self.last_error.take() {
                log::warn!("a follow-up was not queued as the window closed: {error}");
            }
        }
    }

    /// The acknowledgements the follow-ups of commands in history wait
    /// for, as the window's runtime runs them.
    #[cfg(test)]
    pub fn acknowledgements(&self, mapper: &Mapper) -> Vec<Acknowledgement> {
        self.undo
            .iter()
            .chain(&self.redo)
            .filter_map(|command| {
                let awaiting = command.awaiting.clone()?;
                let future: Acknowledgement =
                    Box::pin(acknowledgement(mapper.clone(), command.id, awaiting));
                Some(future)
            })
            .collect()
    }

    /// Waits for every acknowledgement follow-ups wait for, and makes the
    /// follow-ups, as the window does.
    #[cfg(test)]
    pub async fn settle(&mut self, mapper: &Mapper) {
        for waiting in self.acknowledgements(mapper) {
            if let Outcome::Acknowledged {
                command,
                application,
                acknowledged,
            } = waiting.await
            {
                let _ = self.follow_up(mapper, command, application, acknowledged);
            }
        }
    }

    fn find_mut(&mut self, id: CommandId) -> Option<&mut Command> {
        self.undo
            .iter_mut()
            .chain(self.redo.iter_mut())
            .find(|command| command.id == id)
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
        let mut mutations = match direction {
            Direction::Redo => command.redo.clone(),
            Direction::Undo => command.undo.clone(),
        };
        let follow_ups = mutations.split_off(mutations.len() - command.follow_ups);
        let mut areas: Vec<AreaId> = mutations
            .iter()
            .map(|mutation| needed_actions(mutation).0)
            .collect();
        areas.sort_unstable_by_key(|area| area.0);
        areas.dedup();

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
                Mutation::SourceBatch {
                    area_id,
                    source,
                    mut operations,
                    description,
                    split_paired_exit,
                } => {
                    let batch = |operations, description| {
                        if split_paired_exit {
                            AreaMutationBatch::splitting_paired_exit(
                                area_id,
                                operations,
                                description,
                            )
                        } else {
                            AreaMutationBatch::strict(area_id, operations, description)
                        }
                        .in_source(source)
                    };
                    while operations.len() > MAX_MUTATION_OPERATIONS {
                        let rest = operations.split_off(MAX_MUTATION_OPERATIONS);
                        batches.push(batch(operations, description.clone()));
                        operations = rest;
                    }
                    batches.push(batch(operations, description));
                }
                Mutation::UpsertRooms(area_id, updates) => {
                    let description = if updates.len() == 1 {
                        format!("Update room {}", updates[0].0)
                    } else {
                        format!("Update {} rooms", updates.len())
                    };
                    let mut operations: Vec<_> = updates
                        .into_iter()
                        .map(|(room_number, body)| AreaMutation::UpsertRoom {
                            room_source: None,
                            room_number,
                            body,
                        })
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
                            room_source: None,
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
                            room_source: None,
                            room_number: room_key.room_number,
                            name,
                            value,
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
                            room_source: None,
                            room_number: room_key.room_number,
                            name,
                        }],
                        description,
                    ));
                }
                Mutation::SetAreaProperty(area_id, name, value) => {
                    let description = format!("Set area property {name}");
                    batches.push(AreaMutationBatch::strict(
                        area_id,
                        vec![AreaMutation::UpsertAreaProperty { name, value }],
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
                Mutation::UpdateExit {
                    room_key,
                    id,
                    updates,
                } => {
                    batches.push(AreaMutationBatch::splitting_paired_exit(
                        room_key.area_id,
                        vec![AreaMutation::UpdateExit {
                            exit_id: id,
                            body: updates,
                        }],
                        "Update exit",
                    ));
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
                        batches.push(in_owner(
                            AreaMutationBatch::strict(
                                area_id,
                                vec![AreaMutation::UpdateLabel {
                                    label_id,
                                    body: updates,
                                }],
                                "Update label",
                            ),
                            label_source(mapper, area_id, label_id),
                        ));
                    }
                }
                Mutation::DeleteLabel { area_id, id } => {
                    if let Some(label_id) = command.label_id(id) {
                        batches.push(in_owner(
                            AreaMutationBatch::strict(
                                area_id,
                                vec![AreaMutation::DeleteLabel { label_id }],
                                "Delete label",
                            ),
                            label_source(mapper, area_id, label_id),
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
                        batches.push(in_owner(
                            AreaMutationBatch::strict(
                                area_id,
                                vec![AreaMutation::UpdateShape {
                                    shape_id,
                                    body: updates,
                                }],
                                "Update shape",
                            ),
                            shape_source(mapper, area_id, shape_id),
                        ));
                    }
                }
                Mutation::DeleteShape { area_id, id } => {
                    if let Some(shape_id) = command.shape_id(id) {
                        batches.push(in_owner(
                            AreaMutationBatch::strict(
                                area_id,
                                vec![AreaMutation::DeleteShape { shape_id }],
                                "Delete shape",
                            ),
                            shape_source(mapper, area_id, shape_id),
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
                // The other maps follow where they still hold the other
                // direction's state; an earlier wait is answered no more.
                command.applications += 1;
                let wanted = match direction {
                    Direction::Redo => !command.followed,
                    Direction::Undo => command.followed,
                };
                command.awaiting = (!follow_ups.is_empty() && wanted).then(|| Awaiting {
                    application: command.applications,
                    direction,
                    areas,
                    operations: command.operation_ids.clone(),
                });
                if let Some(awaiting) = command.awaiting.clone() {
                    tasks.push(Task::perform(
                        acknowledgement(mapper.clone(), command.id, awaiting),
                        std::convert::identity,
                    ));
                }
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

/// A wait for one application's acknowledgement, run by hand in tests.
#[cfg(test)]
pub type Acknowledgement = std::pin::Pin<Box<dyn std::future::Future<Output = Outcome> + Send>>;

/// How often a wait for an acknowledgement looks again at a write parked
/// for the viewer's Retry or Discard.
const PARKED_POLL: std::time::Duration = std::time::Duration::from_millis(250);

/// Whether every write of `awaiting` is acknowledged, as an
/// [`Outcome::Acknowledged`] for `command`. A write parked for the
/// viewer's Retry or Discard is waited out: retried, it may yet go
/// through; discarded or cancelled, it never will.
async fn acknowledgement(mapper: Mapper, command: CommandId, awaiting: Awaiting) -> Outcome {
    let parked = |operation| {
        awaiting
            .areas
            .iter()
            .any(|area| mapper.is_operation_pending(*area, operation))
    };
    let mut acknowledged = true;
    'operations: for &operation in &awaiting.operations {
        loop {
            if mapper.wait_for_mutation(operation).await.is_ok() {
                break;
            }
            if parked(operation) {
                tokio::time::sleep(PARKED_POLL).await;
                continue;
            }
            // An acknowledgement leaves the queue just before it is
            // recorded; one more look tells it from a discard.
            tokio::task::yield_now().await;
            if mapper.wait_for_mutation(operation).await.is_err() {
                acknowledged = false;
                break 'operations;
            }
            break;
        }
    }
    Outcome::Acknowledged {
        command,
        application: awaiting.application,
        acknowledged,
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
        let Some((_, label)) = area.find_label(&label_id) else {
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
        let Some((_, shape)) = area.find_shape(&shape_id) else {
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

    let (source_redo, source_undo) = super::source_rooms::move_rooms(&area, selection, offset);
    redo.extend(source_redo);
    undo.extend(source_undo);

    if redo.is_empty() {
        None
    } else {
        Some(Command::new(redo, undo))
    }
}

/// One place's part of a delete, with qualified room addresses.
#[derive(Default)]
struct DeletePart {
    /// The selected links held here, then the place's selected rooms.
    redo: Vec<AreaMutation>,
    /// Self-contained steps, in order: each room with its data, each link.
    undo: Vec<Vec<AreaMutation>>,
    /// Exits on rooms that stay, led into a deleted room, and ride no
    /// restored link: each one's full prior state, by its room.
    inbound: Vec<(RoomAddress, ExitId, ExitUpdates)>,
}

/// What deleting rooms and links does to one document, and how undo brings
/// it back. `own` are rooms the delete names; `cascade` are rooms that go
/// with them (attachments on deleted map rooms: the server drops
/// every place's data on a deleted map room). `selected` are the selected
/// links; those held here are deleted whole.
fn delete_in(
    document: &Document<'_>,
    own: &BTreeSet<RoomAddress>,
    cascade: &BTreeSet<RoomAddress>,
    selected: &HashSet<ConnectionId>,
) -> DeletePart {
    let content = document.content();
    let area_id = document.area_id();
    let doomed = |room: RoomAddress| own.contains(&room) || cascade.contains(&room);
    let mut part = DeletePart::default();

    let mut picked: Vec<ConnectionId> = content
        .get_connections()
        .iter()
        .map(|connection| connection.id)
        .filter(|id| selected.contains(id))
        .collect();
    picked.sort();
    part.redo.extend(
        picked
            .iter()
            .map(|&connection_id| AreaMutation::DeleteLink { connection_id }),
    );
    part.redo
        .extend(own.iter().map(|&room_number| AreaMutation::DeleteRoom {
            room_number: room_number.number,
            room_source: room_number.wire_source(),
        }));

    // Rooms come back first, with the place's data on them.
    for &number in own.iter().chain(cascade) {
        let Some(room) = content.get_room_at(number) else {
            continue;
        };
        let mut step = Vec::new();
        if own.contains(&number) {
            step.push(document.recreate_room(number.number, full_fields(room)));
        }
        let mut properties: Vec<(&str, &str)> = room.properties().collect();
        properties.sort_unstable();
        step.extend(
            properties
                .into_iter()
                .map(|(name, value)| AreaMutation::UpsertRoomProperty {
                    room_number: number.number,
                    room_source: number.wire_source(),
                    name: name.to_string(),
                    value: value.to_string(),
                }),
        );
        step.extend(room.tags().map(|tag| AreaMutation::AddRoomTag {
            room_number: number.number,
            room_source: number.wire_source(),
            tag: tag.to_string(),
        }));
        // An exit whose link row is missing returns under its id and
        // finds its link again.
        step.extend(
            room.get_exits()
                .iter()
                .filter(|exit| content.get_connection(exit.connection_id).is_none())
                .map(|exit| AreaMutation::CreateExit {
                    room_number: number.number,
                    room_source: number.wire_source(),
                    body: ExitArgs {
                        id: Some(exit.id),
                        ..exit_args_from_cache(exit)
                    },
                }),
        );
        if !step.is_empty() {
            part.undo.push(step);
        }
    }

    // Every selected link, and every link touching a deleted room, comes
    // back by its id with its route and style.
    let mut restored = HashSet::new();
    for connection in content.get_connections() {
        let members = link_members(content, connection.id);
        let picked = selected.contains(&connection.id);
        let touches = doomed(connection.endpoint_a.address())
            || connection
                .endpoint_b
                .is_some_and(|endpoint| doomed(endpoint.address()))
            || members.iter().any(|(room, exit)| {
                doomed(*room)
                    || exit.destination_address().is_some_and(|destination| {
                        destination.map == area_id && doomed(destination.room)
                    })
            });
        if !picked && !touches {
            continue;
        }
        if members.iter().any(|(_, exit)| exit.to_unknown) {
            // The destination was redacted ("Unknown map") and is
            // unknowable client-side: undo brings the exit back without it.
            log::warn!(
                "map editor: a deleted link leads to an unshared map; undo will \
                 recreate it without its destination"
            );
        }
        // A member on a room that stays keeps an unselected link alive,
        // one-ended and with its route cleared: undo takes that down first.
        let remnant = !picked && members.iter().any(|(room, _)| !doomed(*room));
        restored.insert(connection.id);
        part.undo.push(restore_link(content, connection, remnant));
    }

    for room in content.document_rooms() {
        let number = room.address();
        if doomed(number) {
            continue;
        }
        for exit in room.get_exits() {
            if !restored.contains(&exit.connection_id)
                && exit.destination_address().is_some_and(|destination| {
                    destination.map == area_id && doomed(destination.room)
                })
            {
                part.inbound
                    .push((number, exit.id, exit_updates_from_cache(exit)));
            }
        }
    }
    part
}

/// `steps` as few envelopes as fit, never splitting a step.
fn pack(steps: Vec<Vec<AreaMutation>>) -> Vec<Vec<AreaMutation>> {
    let mut envelopes: Vec<Vec<AreaMutation>> = Vec::new();
    for step in steps {
        match envelopes.last_mut() {
            Some(last) if last.len() + step.len() <= MAX_MUTATION_OPERATIONS => last.extend(step),
            _ => envelopes.push(step),
        }
    }
    envelopes
}

/// Every field of a room, for recreating it.
fn full_fields(room: &smudgy_cloud::mapper::room_cache::RoomCache) -> RoomUpdates {
    RoomUpdates {
        title: Some(room.get_title().to_string()),
        description: Some(room.get_description().to_string()),
        level: Some(room.get_level()),
        x: Some(room.get_x()),
        y: Some(room.get_y()),
        color: Some(room.get_color().to_string()),
        external_id: room.get_external_id().map(|id| Some(id.to_string())),
    }
}

/// One place's writes for a delete or its undo: `steps` packed into
/// envelopes, then the exit relinks.
fn part_writes(
    document: &Document<'_>,
    steps: Vec<Vec<AreaMutation>>,
    inbound: Vec<(RoomAddress, ExitId, ExitUpdates)>,
    description: &str,
) -> Vec<Mutation> {
    let mut writes: Vec<Mutation> = pack(steps)
        .into_iter()
        .map(|operations| document.batch(operations, description))
        .collect();
    writes.extend(
        inbound
            .into_iter()
            .map(|(room, exit_id, updates)| document.update_exit(room, exit_id, updates)),
    );
    writes
}

/// Deletes every selected entity, each in the place that holds it.
///
/// Undo brings back every place's part, the map's first so a Secret's data
/// on a map room has its room to return to: rooms with their fields,
/// properties and tags; every selected link and every link touching a
/// deleted room, by its id with its route, style and member exits; exits
/// elsewhere that led into a deleted room; labels and shapes. Deleting a
/// map room takes each Secret's (and Private's) data and exits on it too,
/// and undo returns those to every place the viewer may write.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn delete_selection(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    selection: &Selection,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let map_rooms: BTreeSet<RoomNumber> = selection
        .rooms()
        .filter(|number| area.get_room(number).is_some())
        .collect();
    let selected: HashSet<ConnectionId> = selection.connections().collect();

    let mut redo = Vec::new();
    let mut source_undo = Vec::new();

    // Each Secret's and Private's part goes first, so its selected links
    // are still whole when a map room's delete reaches its data.
    let mut drawings = super::source_rooms::drawings_by_source(&area, selection);
    for layer in area.source_layers() {
        let source = layer.source();
        let Some(document) = Document::of(&area, source) else {
            continue;
        };
        let own: BTreeSet<RoomAddress> = selection
            .source_rooms()
            .filter(|(of, number)| *of == source && layer.own_room(*number).is_some())
            .map(|(_, number)| RoomAddress::new(source, number))
            .collect();
        let cascade: BTreeSet<RoomAddress> = map_rooms
            .iter()
            .map(|number| RoomAddress::map(*number))
            .filter(|address| layer.attachment(*address).is_some())
            .collect();
        let mut part = delete_in(&document, &own, &cascade, &selected);
        let (drawing_deletes, drawing_restores) = drawings.remove(&source).unwrap_or_default();
        part.redo.extend(drawing_deletes);
        part.undo.extend(
            drawing_restores
                .into_iter()
                .map(|operation| vec![operation]),
        );
        if !part.redo.is_empty() {
            redo.extend(part_writes(
                &document,
                part.redo
                    .into_iter()
                    .map(|operation| vec![operation])
                    .collect(),
                Vec::new(),
                "Delete selection",
            ));
        }
        // A place the viewer can't write keeps what the delete took.
        if super::secrets::can_write(&area, source) {
            source_undo.extend(part_writes(
                &document,
                part.undo,
                part.inbound,
                "Restore deleted selection",
            ));
        }
    }

    let document = Document::map(&area);
    let part = delete_in(
        &document,
        &map_rooms.iter().copied().map(RoomAddress::map).collect(),
        &BTreeSet::new(),
        &selected,
    );
    redo.extend(part_writes(
        &document,
        part.redo
            .into_iter()
            .map(|operation| vec![operation])
            .collect(),
        Vec::new(),
        "Delete selection",
    ));
    let mut undo = part_writes(
        &document,
        part.undo,
        part.inbound,
        "Restore deleted selection",
    );

    // Another map's exits into a deleted room lose their destination with
    // it (the server cascades this, and `Mapper::delete_room` clears the
    // same exits in their own maps); undo re-links each.
    for host_area in atlas.areas() {
        let host_area_id = *host_area.get_id();
        if host_area_id == area_id {
            continue;
        }
        for host_room in host_area.get_rooms() {
            let host_key = RoomKey::new(host_area_id, host_room.get_room_number());
            for exit in host_room.get_exits() {
                if exit.to_area_id == Some(area_id)
                    && exit
                        .to_room_number
                        .is_some_and(|number| map_rooms.contains(&number))
                {
                    undo.push(Mutation::UpdateExit {
                        room_key: host_key.clone(),
                        id: exit.id,
                        updates: exit_updates_from_cache(exit),
                    });
                }
            }
        }
    }

    let mut seeds = Vec::new();
    let mut next_slot: SlotId = 0;
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
        undo.push(Mutation::CreateLabel {
            area_id,
            args: LabelArgs {
                // Recreation mints a fresh identity at apply time.
                id: None,
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
        undo.push(Mutation::CreateShape {
            area_id,
            args: ShapeArgs {
                // Recreation mints a fresh identity at apply time.
                id: None,
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
    undo.extend(source_undo);

    let mut command = Command::new(redo, undo);
    for (slot, id) in seeds {
        command = command.seed_slot(slot, id);
    }
    Some(command)
}

/// A label's or shape's update and delete go to whichever source holds it,
/// so every edit to an existing drawing works the same for a Secret's.
fn in_owner(batch: AreaMutationBatch, owner: Option<smudgy_cloud::SourceId>) -> AreaMutationBatch {
    match owner {
        Some(source) => batch.in_source(source),
        None => batch,
    }
}

/// The source holding a label, when it is not the map itself.
fn label_source(
    mapper: &Mapper,
    area_id: AreaId,
    label_id: LabelId,
) -> Option<smudgy_cloud::SourceId> {
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&area_id)?;
    let (layer, _) = area.find_label(&label_id)?;
    layer.map(smudgy_cloud::mapper::area_cache::SourceLayer::source)
}

/// The source holding a shape, when it is not the map itself.
fn shape_source(
    mapper: &Mapper,
    area_id: AreaId,
    shape_id: ShapeId,
) -> Option<smudgy_cloud::SourceId> {
    let atlas = mapper.get_current_atlas();
    let area = atlas.get_area(&area_id)?;
    let (layer, _) = area.find_shape(&shape_id)?;
    layer.map(smudgy_cloud::mapper::area_cache::SourceLayer::source)
}

/// `ExitArgs` recreating a cached exit (everything `ExitArgs` can express).
fn exit_args_from_cache(exit: &smudgy_cloud::mapper::exit_cache::ExitCache) -> ExitArgs {
    ExitArgs {
        to_source: exit.to_exit().to_source,
        // Recreation mints a fresh identity at apply time.
        id: None,
        connection_id: None,
        new_connection_id: None,
        from_direction: exit.from_direction,
        to_area_id: exit.to_exit().to_area_id,
        to_room_number: exit.to_room_number,
        to_direction: exit.to_direction,
        path: exit.path.clone(),
        is_hidden: exit.is_hidden,
        door: exit.door.clone(),
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
/// link, and `clear_to` would destroy it. The same holds for `command` and
/// `path`: an exit without one is written `""` (none), never omitted, so
/// replaying the snapshot takes away one written since.
pub(super) fn exit_updates_from_cache(
    exit: &smudgy_cloud::mapper::exit_cache::ExitCache,
) -> ExitUpdates {
    let destination_empty = exit.to_area_id.is_none()
        && exit.to_room_number.is_none()
        && exit.to_direction.is_none()
        && !exit.to_unknown;
    ExitUpdates {
        to_source: Some(exit.to_exit().to_source),
        clear_to: destination_empty.then_some(true),
        from_direction: Some(exit.from_direction),
        to_area_id: exit.to_exit().to_area_id,
        to_room_number: exit.to_room_number,
        to_direction: exit.to_direction,
        path: Some(exit.path.clone().unwrap_or_default()),
        is_hidden: Some(exit.is_hidden),
        door: Some(exit.door.clone()),
        weight: Some(exit.weight),
        command: Some(exit.command.clone().unwrap_or_default()),
    }
}

/// Where a new exit should land.
#[derive(Debug, Clone, Copy)]
pub enum NewExitTarget {
    /// An existing room.
    Room(PlacedRoom),
    /// A new room created at this position/level as part of the command,
    /// in the place the link goes.
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

/// A Link-tool link: the place it is written to, and its ends.
#[derive(Debug, Clone, Copy)]
pub struct NewLink {
    pub area_id: AreaId,
    /// The map, or the Secret (or Private) the link goes into. Each end is
    /// a map room or one of this place's own rooms.
    pub place: smudgy_cloud::SourceId,
    pub from: PlacedRoom,
    pub from_direction: smudgy_cloud::ExitDirection,
    pub to: NewExitTarget,
    pub to_direction: smudgy_cloud::ExitDirection,
}

/// [`create_link`] between map rooms, into the map.
#[cfg(test)]
#[must_use]
pub fn create_exit_with_options(
    area_id: AreaId,
    from: RoomNumber,
    from_direction: smudgy_cloud::ExitDirection,
    to: &NewExitTarget,
    to_direction: smudgy_cloud::ExitDirection,
    options: NewLinkOptions,
) -> Command {
    let link = NewLink {
        area_id,
        place: smudgy_cloud::SourceId::Map,
        from: PlacedRoom::map(from),
        from_direction,
        to: *to,
        to_direction,
    };
    create_link(&link, options)
        .map(|(command, _)| command)
        .expect("a link between map rooms goes into the map")
}

/// Creates a Link-tool link where it goes, as one compound mutation, with
/// the id of the link it makes (or pairs with). IDs are allocated before
/// enqueue so room + Connection + traversal creation is atomic and
/// retry-safe. Built in wire form, so an end may be a map room the place
/// keeps nothing for yet. `None` when an end is a room of another place.
#[must_use]
pub fn create_link(link: &NewLink, options: NewLinkOptions) -> Option<(Command, ConnectionId)> {
    let place = link.place;
    // A room on the wire: a map room, or one of `place`'s own rooms.
    let wire = |room: PlacedRoom| {
        if room.source.is_map() {
            Some((room.number, None))
        } else if room.source == place {
            Some((room.number, Some(place)))
        } else {
            None
        }
    };
    let own = (!place.is_map()).then_some(place);
    let from = wire(link.from)?;
    let to = match link.to {
        NewExitTarget::Room(room) => Some(wire(room)?),
        NewExitTarget::NewRoom { room_number, .. } => Some((room_number, own)),
        NewExitTarget::Dangling => None,
    };
    let (area_id, from_direction, to_direction) =
        (link.area_id, link.from_direction, link.to_direction);
    let connection_id = ConnectionId::new();
    let forward_id = ExitId::new();
    let dangling = matches!(link.to, NewExitTarget::Dangling);
    let one_way = options.one_way || dangling || options.pair_with.is_some();
    let reverse_id = (!one_way).then(ExitId::new);
    let mut operations = Vec::new();
    if let NewExitTarget::NewRoom {
        room_number,
        at,
        level,
    } = link.to
    {
        let body = RoomUpdates {
            title: Some(String::new()),
            description: Some(String::new()),
            level: Some(level),
            x: Some(at.x),
            y: Some(at.y),
            color: Some(String::new()),
            external_id: None,
        };
        operations.push(if own.is_some() {
            AreaMutation::CreateRoom {
                room_source: own,
                room_number,
                body,
            }
        } else {
            AreaMutation::UpsertRoom {
                room_source: None,
                room_number,
                body,
            }
        });
    }
    if options.pair_with.is_none() {
        let (from_side, from_offset) = default_anchor_for_direction(from_direction, None);
        let mut endpoint_a = ConnectionEndpoint {
            source: from.1,
            room_number: from.0,
            side: from_side,
            port_offset: from_offset,
            port_mode: PortMode::AutoPinned,
        };
        let mut endpoint_b = to.map(|(room_number, source)| {
            let (to_side, to_offset) = default_anchor_for_direction(to_direction, None);
            ConnectionEndpoint {
                source,
                room_number,
                side: to_side,
                port_offset: to_offset,
                port_mode: PortMode::AutoPinned,
            }
        });
        if endpoint_b.is_some_and(|endpoint| {
            endpoint_a.address().connection_order_key() > endpoint.address().connection_order_key()
        }) {
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
        room_source: from.1,
        room_number: from.0,
        body: ExitArgs {
            id: Some(forward_id),
            connection_id: Some(attached_connection_id),
            from_direction,
            to_area_id: to.map(|_| area_id),
            to_room_number: to.map(|(number, _)| number),
            to_source: to.and_then(|(_, source)| source),
            to_direction: to.map(|_| to_direction),
            command: options.from_command.clone(),
            weight: 1.0,
            ..Default::default()
        },
    });
    if let Some(reverse_id) = reverse_id {
        let (to_room, to_source) = to.expect("a bidirectional link has a destination room");
        operations.push(AreaMutation::CreateExit {
            room_source: to_source,
            room_number: to_room,
            body: ExitArgs {
                id: Some(reverse_id),
                connection_id: Some(connection_id),
                from_direction: to_direction,
                to_area_id: Some(area_id),
                to_room_number: Some(from.0),
                to_source: from.1,
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
        match (link.to, one_way) {
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
    let mut inverse = if options.pair_with.is_some() {
        vec![AreaMutation::DeleteExit {
            exit_id: forward_id,
        }]
    } else {
        vec![AreaMutation::DeleteLink { connection_id }]
    };
    if let NewExitTarget::NewRoom { room_number, .. } = link.to {
        inverse.push(AreaMutation::DeleteRoom {
            room_source: own,
            room_number,
        });
    }
    let batch = |operations, description: String| match own {
        Some(source) => Mutation::SourceBatch {
            area_id,
            source,
            operations,
            description,
            split_paired_exit: false,
        },
        None => Mutation::AreaBatch {
            area_id,
            operations,
            description,
        },
    };
    let command = Command::new(
        vec![batch(operations, intent)],
        vec![batch(inverse, "Undo link creation".to_string())],
    );
    Some((command, attached_connection_id))
}

/// The inverse of `updates` against `current`: exactly the touched fields.
fn connection_inverse(
    current: &smudgy_cloud::Connection,
    updates: &ConnectionUpdates,
) -> ConnectionUpdates {
    ConnectionUpdates {
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
    }
}

/// Edits shared Connection geometry/appearance through one semantic
/// envelope and captures exactly the touched fields for undo. A Secret's
/// link is edited in its Secret; `updates` name its rooms in that
/// qualified room addresses.
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
    let document = Document::of_connection(&area, connection_id)?;
    let current = document.content().get_connection(connection_id)?;
    // `ConnectionUpdates` deliberately cannot clear endpoint B (topology
    // changes travel through the semantic link operations), so an edit that
    // would *set* it on a connection without one has no expressible inverse.
    // Refuse it rather than record an undo that silently keeps the endpoint.
    if updates.endpoint_b.is_some() && current.endpoint_b.is_none() {
        return None;
    }
    let inverse = connection_inverse(current, &updates);
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
            vec![document.batch(
                vec![AreaMutation::UpdateConnection {
                    connection_id,
                    body: updates,
                }],
                description.clone(),
            )],
            vec![document.batch(
                vec![AreaMutation::UpdateConnection {
                    connection_id,
                    body: inverse,
                }],
                format!("Undo {description}"),
            )],
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
/// and orthogonal elbow moves in one CAS envelope. Every edited link lives
/// in one document: the map's, or one Secret's.
#[must_use]
pub fn edit_connections(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    edits: Vec<(ConnectionId, ConnectionUpdates)>,
    description: impl Into<String>,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let document = Document::of_connection(&area, edits.first()?.0)?;
    let mut redo = Vec::with_capacity(edits.len());
    let mut undo = Vec::with_capacity(edits.len());
    for (connection_id, updates) in edits {
        let current = document.content().get_connection(connection_id)?;
        // Same endpoint-B inverse rule as `edit_connection` above.
        if updates.endpoint_b.is_some() && current.endpoint_b.is_none() {
            return None;
        }
        let inverse = connection_inverse(current, &updates);
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
        vec![document.batch(redo, description.clone())],
        vec![document.batch(undo, format!("Undo {description}"))],
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
    let document = Document::of_connection(&area, connection_id)?;
    let content = document.content();
    let connection = content.get_connection(connection_id)?;
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
        let render = content.get_room_connections().iter().find(|render| {
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

pub(super) fn restore_exit_args(
    exit: &smudgy_cloud::mapper::exit_cache::ExitCache,
    connection_id: ConnectionId,
) -> ExitArgs {
    ExitArgs {
        to_source: exit.to_exit().to_source,
        id: Some(exit.id),
        connection_id: Some(connection_id),
        new_connection_id: None,
        from_direction: exit.from_direction,
        to_area_id: exit.to_exit().to_area_id,
        to_room_number: exit.to_room_number,
        to_direction: exit.to_direction,
        path: exit.path.clone(),
        is_hidden: exit.is_hidden,
        door: exit.door.clone(),
        weight: exit.weight,
        command: exit.command.clone(),
    }
}

/// Every member exit of `connection_id` in `content`, with its room, in a
/// stable (id) order.
pub(super) fn link_members(
    content: &smudgy_cloud::mapper::area_cache::AreaCache,
    connection_id: ConnectionId,
) -> Vec<(RoomAddress, &smudgy_cloud::mapper::exit_cache::ExitCache)> {
    let mut members: Vec<_> = content
        .document_rooms()
        .flat_map(|room| {
            room.get_exits()
                .iter()
                .filter(|exit| exit.connection_id == connection_id)
                .map(move |exit| (room.address(), exit))
        })
        .collect();
    members.sort_by_key(|(_, exit)| exit.id.0);
    members
}

/// The "Restore deleted link" recipe, with qualified room addresses: the
/// link's row by its id, with its route and style, then each member exit
/// under its own id. `remnant` first deletes what a delete left of it (a
/// surviving member keeps a one-ended link alive, its route cleared).
pub(super) fn restore_link(
    content: &smudgy_cloud::mapper::area_cache::AreaCache,
    connection: &smudgy_cloud::Connection,
    remnant: bool,
) -> Vec<AreaMutation> {
    let mut operations = Vec::new();
    if remnant {
        operations.push(AreaMutation::DeleteLink {
            connection_id: connection.id,
        });
    }
    operations.push(AreaMutation::CreateConnection {
        body: ConnectionArgs::from(connection),
    });
    for (room_number, exit) in link_members(content, connection.id) {
        operations.push(AreaMutation::CreateExit {
            room_source: room_number.wire_source(),
            room_number: room_number.number,
            body: restore_exit_args(exit, connection.id),
        });
    }
    operations
}

/// Delete a selected visual link and every traversal it owns. Undo restores
/// the same stable Connection and Exit identities in one envelope. A
/// Secret's link is deleted in its Secret.
#[must_use]
pub fn delete_connection(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    connection_id: ConnectionId,
) -> Option<Command> {
    let area = atlas.get_area(&area_id)?;
    let document = Document::of_connection(&area, connection_id)?;
    let content = document.content();
    let connection = content.get_connection(connection_id)?;
    let members = link_members(content, connection_id).len();
    Some(Command::new(
        vec![document.batch(
            vec![AreaMutation::DeleteLink { connection_id }],
            if members == 2 {
                "Delete bidirectional link"
            } else {
                "Delete link"
            },
        )],
        vec![document.batch(
            restore_link(content, connection, false),
            "Restore deleted link",
        )],
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
    let document = Document::of_connection(&area, keep_connection_id)?;
    let content = document.content();
    let merge = content.get_connection(merge_connection_id)?.clone();
    let moved_exit = link_members(content, merge_connection_id).first()?.1.id;
    Some(Command::new(
        vec![document.batch(
            vec![AreaMutation::Pair {
                keep_connection_id,
                merge_connection_id,
            }],
            "Pair reciprocal connections",
        )],
        vec![document.batch(
            vec![
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
            "Unpair reciprocal connections",
        )],
    ))
}

/// An exit as the editor addresses it: the place whose document holds it,
/// its qualified anchor room, and its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitRef {
    pub area_id: AreaId,
    pub place: smudgy_cloud::SourceId,
    pub room: RoomAddress,
    pub id: ExitId,
}

/// The document holding `exit`, with the cached exit.
fn exit_in<'a>(
    area: &'a smudgy_cloud::mapper::area_cache::AreaCache,
    exit: ExitRef,
) -> Option<(
    Document<'a>,
    &'a smudgy_cloud::mapper::exit_cache::ExitCache,
)> {
    let document = Document::of(area, exit.place)?;
    let cached = document
        .content()
        .get_room_at(exit.room)?
        .get_exits()
        .iter()
        .find(|cached| cached.id == exit.id)?;
    Some((document, cached))
}

/// Edits an exit by mutating a full-field snapshot of its current state;
/// coalesces with consecutive edits to the same field. Updates are always
/// full snapshots, and because `ExitUpdates::apply` (and the backend) MERGE
/// the destination fields (`None` = unchanged, nulling requires `clear_to`),
/// `clear_to` is recomputed after the edit: set when the resulting
/// destination is empty (and the prior one wasn't merely redacted), dropped
/// when the edit establishes one (`clear_to` overrides `to_*` on the wire).
///
/// `change` sees the snapshot as the editor shows it: a destination in this
/// map names its room by its number on the map.
#[must_use]
pub fn edit_exit_field(
    atlas: &Arc<AtlasCache>,
    exit_ref: ExitRef,
    field: FieldId,
    change: impl FnOnce(&mut ExitUpdates),
) -> Option<Command> {
    let area = atlas.get_area(&exit_ref.area_id)?;
    let (document, exit) = exit_in(&area, exit_ref)?;

    let prior = exit_updates_from_cache(exit);
    let mut updates = prior.clone();
    change(&mut updates);
    let destination_expressed = updates.to_area_id.is_some()
        || updates.to_room_number.is_some()
        || updates.to_direction.is_some();
    updates.clear_to = (!destination_expressed && !exit.to_unknown).then_some(true);

    Some(
        Command::new(
            vec![document.update_exit(exit_ref.room, exit_ref.id, updates)],
            vec![document.update_exit(exit_ref.room, exit_ref.id, prior)],
        )
        .coalescing(CoalesceKey {
            entity: EntityRef::Exit(exit_ref.area_id, exit_ref.id),
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
    exit_ref: ExitRef,
    change: impl FnOnce(&mut ExitUpdates),
    connection_id: ConnectionId,
    connection_updates: ConnectionUpdates,
) -> Option<Command> {
    let area = atlas.get_area(&exit_ref.area_id)?;
    let (document, exit) = exit_in(&area, exit_ref)?;

    let prior = exit_updates_from_cache(exit);
    let mut updates = prior.clone();
    change(&mut updates);
    let destination_expressed = updates.to_area_id.is_some()
        || updates.to_room_number.is_some()
        || updates.to_direction.is_some();
    updates.clear_to = (!destination_expressed && !exit.to_unknown).then_some(true);

    let current = document.content().get_connection(connection_id)?;
    // Same endpoint-B inverse rule as `edit_connection`.
    if connection_updates.endpoint_b.is_some() && current.endpoint_b.is_none() {
        return None;
    }
    let inverse = connection_inverse(current, &connection_updates);

    // Both edits retain their wire-qualified room addresses.
    let operations = |exit_body: ExitUpdates, link_body: ConnectionUpdates| {
        let mut operations = vec![AreaMutation::UpdateExit {
            exit_id: exit_ref.id,
            body: exit_body,
        }];
        operations.push(AreaMutation::UpdateConnection {
            connection_id,
            body: link_body,
        });
        operations
    };
    Some(Command::new(
        vec![document.batch(
            operations(updates, connection_updates),
            "Change exit direction",
        )],
        vec![document.batch(operations(prior, inverse), "Undo change exit direction")],
    ))
}

// Creates a room at a map-space point on the given level.
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

    let (mut redo_all, mut undo_all) = (Vec::new(), Vec::new());
    if !redo.is_empty() {
        redo_all.push(Mutation::UpsertRooms(area_id, redo));
        undo_all.push(Mutation::UpsertRooms(area_id, undo));
    }
    // A Secret's or Private's selected rooms change in their own place.
    let (source_redo, source_undo) = super::source_rooms::edit_rooms(&area, selection, updates);
    redo_all.extend(source_redo);
    undo_all.extend(source_undo);
    (!redo_all.is_empty()).then(|| Command::new(redo_all, undo_all))
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

    let (source_redo, source_undo) =
        super::source_rooms::shift_rooms_level(&area, selection, delta);
    redo.extend(source_redo);
    undo.extend(source_undo);

    for label_id in selection.labels() {
        let Some((_, label)) = area.find_label(&label_id) else {
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
        let Some((_, shape)) = area.find_shape(&shape_id) else {
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
            args: new_label_args(rect, level),
            slot: 0,
        }],
        vec![Mutation::DeleteLabel {
            area_id,
            id: IdRef::Slot(0),
        }],
    )
}

/// A new label's fields: a placeholder text covering `rect` on `level`.
#[must_use]
pub fn new_label_args(rect: iced::Rectangle, level: i32) -> LabelArgs {
    LabelArgs {
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
    }
}

/// A new shape's fields: a filled rectangle covering `rect` on `level`.
#[must_use]
pub fn new_shape_args(rect: iced::Rectangle, level: i32) -> ShapeArgs {
    ShapeArgs {
        level,
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
        background_color: Some("#32323c".to_string()),
        stroke_color: Some(String::new()),
        ..Default::default()
    }
}

/// Creates a shape covering a map-space rect on the given level.
#[must_use]
pub fn create_shape(area_id: AreaId, rect: iced::Rectangle, level: i32) -> Command {
    Command::new(
        vec![Mutation::CreateShape {
            area_id,
            args: new_shape_args(rect, level),
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
            let (_, label) = area.find_label(&label_id)?;
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
            let (_, shape) = area.find_shape(&shape_id)?;
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
        EntityId::Room(_) | EntityId::SourceRoom(..) | EntityId::Connection(_) => None,
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
    /// Server-global room id (GMCP/MSDP identity); rides copy/paste so the
    /// merge workflow's cut+paste keeps bindings.
    pub external_id: Option<String>,
    /// Sorted by name for deterministic paste mutation order.
    pub properties: Vec<(String, String)>,
    pub tags: Vec<String>,
    pub exits: Vec<ExitClip>,
}

/// `ExitCache`-shaped data for an exit owned by a copied room.
/// `to_area_token` is deliberately not carried: it's a per-viewer
/// projection artifact and must never be written back.
#[derive(Debug, Clone)]
pub struct ExitClip {
    /// An attachment on another source's room; None means the copied source.
    pub from_source: Option<smudgy_cloud::SourceId>,
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    pub path: Option<String>,
    pub is_hidden: bool,
    pub door: Option<smudgy_cloud::Door>,
    pub weight: f32,
    pub command: Option<String>,
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
    /// A Cut is a deferred move; staging it never removes the original.
    pub cut: Option<CutSelection>,
    /// Authority that authorized this copy, rechecked before each paste.
    pub copy_from: Option<(AreaId, smudgy_cloud::SourceId, u64)>,
    /// The area the snapshot came from; decides same-area (fresh room
    /// numbers, cascading offset) vs cross-area (numbers preserved where
    /// vacant, exact positions) paste semantics.
    pub source_area_id: Option<AreaId>,
    pub source_map_id: Option<AreaId>,
    pub rooms: Vec<RoomClip>,
    pub connections: Vec<ConnectionClip>,
    pub connection_origin: Option<smudgy_cloud::MapPoint>,
    pub labels: Vec<LabelArgs>,
    pub shapes: Vec<ShapeArgs>,
}

/// Selected identities waiting for an atomic move, rather than copied content.
#[derive(Debug, Clone)]
pub struct CutSelection {
    pub id: Uuid,
    pub map: AreaId,
    pub source: smudgy_cloud::SourceId,
    pub revision: i64,
    pub auth_revision: u64,
    pub selection: Vec<EntityId>,
    pub content: smudgy_cloud::MovedContent,
    pub center: Option<iced::Point>,
}

/// The middle of everything on the clipboard, for placing a paste.
#[must_use]
pub fn clipboard_center(clipboard: &EntityClipboard) -> Option<iced::Point> {
    let points = clipboard
        .rooms
        .iter()
        .map(|room| (room.x, room.y))
        .chain(
            clipboard
                .labels
                .iter()
                .map(|label| (label.x + label.width / 2.0, label.y + label.height / 2.0)),
        )
        .chain(
            clipboard
                .shapes
                .iter()
                .map(|shape| (shape.x + shape.width / 2.0, shape.y + shape.height / 2.0)),
        );
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for (x, y) in points {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    min_x
        .is_finite()
        .then(|| iced::Point::new((min_x + max_x) / 2.0, (min_y + max_y) / 2.0))
}

impl EntityClipboard {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cut.is_none()
            && self.rooms.is_empty()
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

    snapshot_document(&area, selection, allow_rooms, include_boundary_links)
}

pub(super) fn snapshot_document(
    area: &smudgy_cloud::mapper::area_cache::AreaCache,
    selection: &Selection,
    allow_rooms: bool,
    include_boundary_links: bool,
) -> EntityClipboard {
    let area_id = *area.get_id();
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
                    connection.endpoint_a.address().source == area.document_source()
                        && selected_rooms.contains(&connection.endpoint_a.room_number)
                        && endpoint_b.address().source == area.document_source()
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
                external_id: room.get_external_id().map(str::to_string),
                properties,
                tags: room.tags().map(str::to_string).collect(),
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
            for endpoint in std::iter::once(&mut body.endpoint_a).chain(body.endpoint_b.as_mut()) {
                endpoint.source = (endpoint.address().source != area.document_source())
                    .then_some(endpoint.address().source);
            }
            if let Some(origin) = connection_origin {
                for point in &mut body.route_points {
                    point.x -= origin.x;
                    point.y -= origin.y;
                }
            }
            let mut members = Vec::new();
            for room in area.document_rooms() {
                for exit in room.get_exits() {
                    if exit.connection_id == connection.id {
                        members.push((
                            room.get_room_number(),
                            ExitClip {
                                from_source: (room.address().source != area.document_source())
                                    .then_some(room.address().source),
                                from_direction: exit.from_direction,
                                to_area_id: exit.to_area_id,
                                to_room_number: exit.to_room_number,
                                to_direction: exit.to_direction,
                                path: exit.path.clone(),
                                is_hidden: exit.is_hidden,
                                door: exit.door.clone(),
                                weight: exit.weight,
                                command: exit.command.clone(),
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
                    .find(|endpoint| {
                        endpoint.address() == RoomAddress::new(area.document_source(), from_room)
                    })
                else {
                    continue;
                };
                let mut body = ConnectionArgs::from(connection);
                body.endpoint_a = ConnectionEndpoint {
                    source: None,
                    ..endpoint
                };
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
                            from_source: None,
                            from_direction: exit.from_direction,
                            to_area_id: None,
                            to_room_number: None,
                            to_direction: None,
                            path: exit.path.clone(),
                            is_hidden: exit.is_hidden,
                            door: exit.door.clone(),
                            weight: exit.weight,
                            command: exit.command.clone(),
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
        cut: None,
        copy_from: None,
        source_area_id: Some(area_id),
        source_map_id: Some(area_id),
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
#[cfg(test)]
pub fn boundary_link_count(
    atlas: &Arc<AtlasCache>,
    area_id: AreaId,
    selection: &Selection,
) -> usize {
    let Some(area) = atlas.get_area(&area_id) else {
        return 0;
    };
    boundary_links_in_document(&area, selection)
}

pub(super) fn boundary_links_in_document(
    area: &smudgy_cloud::mapper::area_cache::AreaCache,
    selection: &Selection,
) -> usize {
    let selected_rooms: HashSet<_> = selection.rooms().collect();
    area.get_connections()
        .iter()
        .filter(|connection| {
            let fully_contained = connection.endpoint_b.is_some_and(|endpoint_b| {
                connection.endpoint_a.address().source == area.document_source()
                    && selected_rooms.contains(&connection.endpoint_a.room_number)
                    && endpoint_b.address().source == area.document_source()
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
/// `taken` holds the numbers the target area cannot give out: its rooms'
/// and those its exits lead to without a room there. Cross-area pastes
/// (`preserve_numbers`) keep each source number when it is vacant — not
/// taken and not already claimed by this paste — so a merge-back lands on
/// the same identities. Collisions, and every same-area paste, allocate
/// fresh numbers counting up from `first_fresh` and skipping anything taken
/// or claimed.
fn remap_room_numbers(
    source: &[RoomNumber],
    taken: &HashSet<RoomNumber>,
    first_fresh: RoomNumber,
    preserve_numbers: bool,
) -> HashMap<RoomNumber, RoomNumber> {
    let mut claimed = taken.clone();
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

/// Pastes the clipboard into the ordinary map as one atomic undo step.
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
#[cfg(test)]
#[must_use]
pub fn paste_clipboard(
    atlas: &Arc<AtlasCache>,
    target_area_id: AreaId,
    clipboard: &EntityClipboard,
    level: i32,
    offset: Vector,
    // The first number a pasted room may take when it cannot keep its own:
    // the Mapper's reservation-aware allocation, one above the target's
    // highest room. `None` when the target has no room numbers
    // left; a clipboard holding rooms then pastes nothing.
    next_room_number: Option<RoomNumber>,
) -> (Option<Command>, Vec<RoomNumber>, usize) {
    paste_into_source(
        atlas,
        target_area_id,
        smudgy_cloud::SourceId::Map,
        clipboard,
        level,
        offset,
        next_room_number,
    )
}

/// Pastes into a named source, including an as-yet empty Private source.
/// All content is one envelope; no label or room can be left as a partial paste.
#[must_use]
pub fn paste_into_source(
    atlas: &Arc<AtlasCache>,
    map_id: AreaId,
    source: smudgy_cloud::SourceId,
    clipboard: &EntityClipboard,
    level: i32,
    offset: Vector,
    next_room_number: Option<RoomNumber>,
) -> (Option<Command>, Vec<RoomNumber>, usize) {
    if clipboard.is_empty() {
        return (None, Vec::new(), 0);
    }
    let Some(map) = atlas.get_area(&map_id) else {
        return (None, Vec::new(), 0);
    };
    let layer = map
        .source_layers()
        .iter()
        .find(|layer| layer.source() == source);
    let area = if source.is_map() {
        Some(map.as_ref())
    } else {
        layer.map(|layer| layer.area().as_ref())
    };
    if area.is_none() && source != smudgy_cloud::SourceId::Private {
        return (None, Vec::new(), 0);
    }
    // This id is used only to classify clipboard references. Wire addresses
    // always use the parent map and an explicit source.
    let target_area_id = area.map_or_else(|| AreaId(Uuid::new_v4()), |area| *area.get_id());
    let same_area = clipboard.source_area_id == Some(target_area_id);
    let source_area_id = clipboard.source_area_id.unwrap_or(target_area_id);
    let room_source = (!source.is_map()).then_some(source);
    let wire_destination = |id: AreaId| {
        atlas.get_area(&id).map_or((id, None), |area| {
            (
                area.map_id().unwrap_or(id),
                (!area.place().is_map()).then_some(area.place()),
            )
        })
    };
    let mut undo = Vec::new();
    let mut pasted_rooms = Vec::new();
    let mut skipped_connections = 0usize;

    let mut compound = Vec::new();
    // Links pasted onto *existing* rooms aren't covered by the room-delete
    // cascade on undo; they need explicit deletes.
    let mut undo_links = Vec::new();
    let mut mapping: HashMap<RoomNumber, RoomNumber> = HashMap::new();

    if !clipboard.rooms.is_empty() {
        let Some(first_fresh) = next_room_number else {
            return (None, Vec::new(), 0);
        };
        let occupied: HashSet<RoomNumber> = area
            .into_iter()
            .flat_map(|area| area.get_rooms())
            .map(|room| room.get_room_number())
            .collect();
        // A pasted room that kept a number an exit already leads to would
        // silently become that exit's destination.
        let taken: HashSet<RoomNumber> = occupied
            .iter()
            .copied()
            .chain(atlas.vacant_exit_targets(&target_area_id))
            .collect();
        let source_numbers: Vec<RoomNumber> = clipboard
            .rooms
            .iter()
            .map(|room| room.room_number)
            .collect();
        mapping = remap_room_numbers(&source_numbers, &taken, first_fresh, !same_area);

        let mut legacy_exits = Vec::new();
        for room in &clipboard.rooms {
            let number = mapping[&room.room_number];
            assert!(
                !occupied.contains(&number),
                "paste remap produced an occupied room number"
            );
            pasted_rooms.push(number);
            compound.push(AreaMutation::CreateRoom {
                room_source,
                room_number: number,
                body: RoomUpdates {
                    title: Some(room.title.clone()),
                    description: Some(room.description.clone()),
                    // Rooms keep their source level in both modes: a
                    // multi-level structure flattened onto the current
                    // level would collapse its up/down geometry.
                    level: Some(room.level),
                    x: Some(room.x + offset.x),
                    y: Some(room.y + offset.y),
                    color: Some(room.color.clone()),
                    // Copies retain the game-server binding. Cut uses a
                    // separate identity-preserving move.
                    external_id: room.external_id.clone().map(Some),
                },
            });

            for (name, value) in &room.properties {
                compound.push(AreaMutation::UpsertRoomProperty {
                    room_source,
                    room_number: number,
                    name: name.clone(),
                    value: value.clone(),
                });
            }
            for tag in &room.tags {
                compound.push(AreaMutation::AddRoomTag {
                    room_source,
                    room_number: number,
                    tag: tag.clone(),
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
            let (to_area_id, to_source, to_room_number, to_direction) = match destination {
                PastedExitDestination::Remapped(number) => {
                    (Some(map_id), room_source, Some(number), exit.to_direction)
                }
                PastedExitDestination::Live(area_id, number) => {
                    let (map, source) = wire_destination(area_id);
                    (Some(map), source, Some(number), exit.to_direction)
                }
                PastedExitDestination::Dangling => (None, None, None, None),
            };
            compound.push(AreaMutation::CreateExit {
                room_source,
                room_number,
                body: ExitArgs {
                    to_source,
                    id: Some(ExitId::new()),
                    connection_id: None,
                    new_connection_id: None,
                    from_direction: exit.from_direction,
                    to_area_id,
                    to_room_number,
                    to_direction,
                    path: exit.path.clone(),
                    is_hidden: exit.is_hidden,
                    door: exit.door.clone(),
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
        let resolve = |number: RoomNumber, anchor: Option<smudgy_cloud::SourceId>| {
            if let Some(anchor) = anchor {
                if clipboard.source_map_id != Some(map_id) {
                    return None;
                }
                let exists = if anchor.is_map() {
                    map.get_room(&number).is_some()
                } else {
                    map.source_layers()
                        .iter()
                        .find(|layer| layer.source() == anchor)
                        .and_then(|layer| layer.own_room(number))
                        .is_some()
                };
                exists.then_some((number, (!anchor.is_map()).then_some(anchor), false))
            } else {
                mapping
                    .get(&number)
                    .copied()
                    .map(|number| (number, room_source, true))
                    .or_else(|| {
                        area.and_then(|area| area.get_room(&number))
                            .map(|_| (number, room_source, false))
                    })
            }
        };
        let a = connection.body.endpoint_a;
        let Some((endpoint_a_room, endpoint_a_source, a_copied)) = resolve(a.room_number, a.source)
        else {
            skipped_connections += 1;
            continue;
        };
        let endpoint_b_room = match connection.body.endpoint_b {
            Some(endpoint) => match resolve(endpoint.room_number, endpoint.source) {
                Some(target) => Some(target),
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
        let any_existing = !a_copied || endpoint_b_room.is_some_and(|(_, _, copied)| !copied);

        let mut members = Vec::new();
        let mut viable = true;
        for (from_room, exit) in &connection.members {
            let Some((room_number, from_source, copied)) = resolve(*from_room, exit.from_source)
            else {
                viable = false;
                break;
            };
            let anchor = from_source.unwrap_or(smudgy_cloud::SourceId::Map);
            let occupied = if anchor == source {
                area.and_then(|area| area.get_room(&room_number))
                    .is_some_and(|room| {
                        room.get_exits()
                            .iter()
                            .any(|other| other.from_direction == exit.from_direction)
                    })
            } else if source.is_map() {
                map.meta()
                    .room_data
                    .iter()
                    .filter(|data| {
                        data.room_source.unwrap_or(smudgy_cloud::SourceId::Map) == anchor
                            && data.room_number == room_number
                    })
                    .flat_map(|data| &data.exits)
                    .any(|other| other.from_direction == exit.from_direction)
            } else {
                layer
                    .and_then(|layer| layer.attachment(RoomAddress::new(anchor, room_number)))
                    .is_some_and(|room| {
                        room.get_exits()
                            .iter()
                            .any(|other| other.from_direction == exit.from_direction)
                    })
            };
            if !copied && occupied {
                viable = false;
                break;
            }
            // Members of an internal link point at its other endpoint;
            // resolve those through the same room resolution. A genuinely
            // cross-area destination (an explicit External clip) keeps its
            // area when it's live in the atlas, and dangles otherwise —
            // never silently rewritten into the target area.
            let (to_area_id, to_source, to_room_number, to_direction) =
                match exit.to_area_id.filter(|_| !exit.to_unknown) {
                    Some(destination_area) if destination_area != source_area_id => {
                        if atlas.get_area(&destination_area).is_some() {
                            let (map, source) = wire_destination(destination_area);
                            (Some(map), source, exit.to_room_number, exit.to_direction)
                        } else {
                            (None, None, None, None)
                        }
                    }
                    Some(_) => match exit.to_room_number.and_then(|number| resolve(number, None)) {
                        Some((number, source, _)) => {
                            (Some(map_id), source, Some(number), exit.to_direction)
                        }
                        None => (None, None, None, None),
                    },
                    None => (None, None, None, None),
                };
            members.push((
                room_number,
                from_source,
                exit,
                to_area_id,
                to_source,
                to_room_number,
                to_direction,
            ));
        }
        if !viable {
            skipped_connections += 1;
            continue;
        }

        let new_connection_id = ConnectionId::new();
        let mut body = connection.body.clone();
        body.id = new_connection_id;
        body.endpoint_a.room_number = endpoint_a_room;
        body.endpoint_a.source = endpoint_a_source;
        if let (Some(endpoint), Some((number, source, _))) =
            (body.endpoint_b.as_mut(), endpoint_b_room)
        {
            endpoint.room_number = number;
            endpoint.source = source;
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
        for (room_number, from_source, exit, to_area_id, to_source, to_room_number, to_direction) in
            members
        {
            compound.push(AreaMutation::CreateExit {
                room_source: from_source,
                room_number,
                body: ExitArgs {
                    to_source,
                    id: Some(ExitId::new()),
                    connection_id: Some(new_connection_id),
                    new_connection_id: None,
                    from_direction: exit.from_direction,
                    to_area_id,
                    to_room_number,
                    to_direction,
                    path: exit.path.clone(),
                    is_hidden: exit.is_hidden,
                    door: exit.door.clone(),
                    weight: exit.weight,
                    command: exit.command.clone(),
                },
            });
        }
    }

    undo.extend(undo_links);
    undo.extend(
        pasted_rooms
            .iter()
            .map(|room_number| AreaMutation::DeleteRoom {
                room_source,
                room_number: *room_number,
            }),
    );
    for label in &clipboard.labels {
        let id = LabelId(Uuid::new_v4());
        compound.push(AreaMutation::CreateLabel {
            body: LabelArgs {
                id: Some(id),
                level: if same_area { level } else { label.level },
                x: label.x + offset.x,
                y: label.y + offset.y,
                ..label.clone()
            },
        });
        undo.push(AreaMutation::DeleteLabel { label_id: id });
    }
    for shape in &clipboard.shapes {
        let id = ShapeId(Uuid::new_v4());
        compound.push(AreaMutation::CreateShape {
            body: ShapeArgs {
                id: Some(id),
                level: if same_area { level } else { shape.level },
                x: shape.x + offset.x,
                y: shape.y + offset.y,
                ..shape.clone()
            },
        });
        undo.push(AreaMutation::DeleteShape { shape_id: id });
    }
    if compound.is_empty() || compound.len() > MAX_MUTATION_OPERATIONS {
        return (None, Vec::new(), skipped_connections);
    }
    let batch = |operations, description: &str| {
        if source.is_map() {
            Mutation::AreaBatch {
                area_id: map_id,
                operations,
                description: description.into(),
            }
        } else {
            Mutation::SourceBatch {
                area_id: map_id,
                source,
                operations,
                description: description.into(),
                split_paired_exit: false,
            }
        }
    };
    (
        Some(Command::new(
            vec![batch(compound, "Paste content")],
            vec![batch(undo, "Undo paste")],
        )),
        pasted_rooms,
        skipped_connections,
    )
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
    let (_, label) = area.find_label(&label_id)?;

    let prior = LabelUpdates {
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
    let (_, shape) = area.find_shape(&shape_id)?;

    let prior = ShapeUpdates {
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
                clan_id: None,
                clan_name: None,
                actions: None,
                clan_ownership: smudgy_cloud::clan_maps::ClanOwnership::default(),
                atlas_name: None,
                projection_token: None,
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
                versions: vec![smudgy_cloud::mutation::VersionInfo::map_source(
                    area_id.0, rev,
                )],
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
        stack: &mut CommandStack,
        command_id: CommandId,
        mutations: Vec<Mutation>,
    ) {
        for mutation in mutations {
            match mutation {
                Mutation::CreateLabel { slot, .. } => {
                    let ResolvedId::Label(id) = resolved_id(stack, command_id, slot) else {
                        panic!("label slot held the wrong entity kind");
                    };
                    stack.resolve(Outcome::Label {
                        command: command_id,
                        slot,
                        result: Ok(id),
                    });
                }
                Mutation::CreateShape { slot, .. } => {
                    let ResolvedId::Shape(id) = resolved_id(stack, command_id, slot) else {
                        panic!("shape slot held the wrong entity kind");
                    };
                    stack.resolve(Outcome::Shape {
                        command: command_id,
                        slot,
                        result: Ok(id),
                    });
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
        mapper
            .add_room_tag(key.clone(), "dock".into())
            .expect("stage tag");
        let link = {
            let atlas = mapper.get_current_atlas();
            let area = atlas.get_area(&area_id).expect("area");
            area.get_room(&RoomNumber(1)).expect("room").get_exits()[0].connection_id
        };

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

        // Undo is synchronous: every id is known up front.
        let _ = stack.undo(&mapper);

        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        let room = area.get_room(&RoomNumber(1)).expect("room restored");
        assert_eq!(room.get_title(), "Room 1");
        assert_eq!(room.get_property("zone"), Some("docks"));
        assert!(room.has_tag("DOCK"), "tags come back too");
        assert_eq!(room.get_exits().len(), 1);
        let exit = &room.get_exits()[0];
        assert_eq!(exit.id, exit_id, "the exit returns under its own id");
        assert_eq!(exit.connection_id, link, "on its own link");
        assert_eq!(
            exit.to_room_number,
            Some(RoomNumber(2)),
            "exit destination restored"
        );
        assert!(area.get_connection(link).is_some(), "the link row is back");
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
            ExitRef {
                area_id,
                place: smudgy_cloud::SourceId::Map,
                room: key.room_number.into(),
                id: exit_id,
            },
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
            ExitRef {
                area_id,
                place: smudgy_cloud::SourceId::Map,
                room: key.room_number.into(),
                id: exit_id,
            },
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
            &NewExitTarget::Room(PlacedRoom::map(RoomNumber(to))),
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
            cut: None,
            copy_from: None,
            source_area_id: Some(area_id),
            source_map_id: Some(area_id),
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
            1,
            "all copied entities share one atomic durable operation"
        );

        assert!(
            stack.can_undo(),
            "paste identities are known before enqueue"
        );

        // Settle the ready completion tasks dropped by this test.
        let command_id = stack.undo.back().expect("pushed").id;
        let mutations = stack.undo.back().expect("pushed").redo.clone();
        drive_create_completions(&mut stack, command_id, mutations);

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
        let Mutation::AreaBatch { operations, .. } = &command.redo[0] else {
            panic!("expected an atomic paste");
        };
        let AreaMutation::CreateLabel { body } = &operations[0] else {
            panic!("expected a label create");
        };
        let pasted_id = body.id.expect("paste mints the label identity");
        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);

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
            from_source: None,
            from_direction: ExitDirection::North,
            to_area_id,
            to_room_number,
            to_direction: Some(ExitDirection::South),
            path: None,
            is_hidden: false,
            door: None,
            weight: 1.0,
            command: None,
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
    fn drive_paste_exit_creates(stack: &mut CommandStack) {
        let command_id = stack.undo.back().expect("pushed").id;
        let mutations = stack.undo.back().expect("pushed").redo.clone();
        drive_create_completions(stack, command_id, mutations);
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
            mapper.next_room_number(&target),
        );
        let command = command.expect("command");
        // Room 1 keeps its number (vacant in the target); room 2 collides
        // with the target's own room 2 and reallocates.
        assert_eq!(pasted, vec![RoomNumber(1), RoomNumber(3)]);

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        drive_paste_exit_creates(&mut stack);

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

    /// The target holds room 2, and another map's exit leads to its room 1,
    /// which no longer exists. A pasted room 1 does not keep that number, so
    /// the exit keeps leading nowhere instead of into the pasted room.
    #[tokio::test]
    async fn cross_area_paste_never_keeps_a_number_a_stale_exit_leads_to() {
        let mapper = test_mapper();
        let source = area_with_rooms(&mapper, &[(1, 0.0, 0.0)]).await;
        let target = area_with_rooms(&mapper, &[(2, 50.0, 50.0)]).await;
        let elsewhere = area_with_rooms(&mapper, &[(1, 0.0, 0.0)]).await;
        mapper
            .create_exit(
                RoomKey::new(elsewhere, RoomNumber(1)),
                ExitArgs {
                    from_direction: ExitDirection::East,
                    to_area_id: Some(target),
                    to_room_number: Some(RoomNumber(1)),
                    weight: 1.0,
                    ..Default::default()
                },
            )
            .await
            .expect("an exit to a room that is gone");
        let clipboard = snapshot_selection(
            &mapper.get_current_atlas(),
            source,
            &select_rooms(&[1]),
            true,
            false,
        );

        let (command, pasted, _) = paste_clipboard(
            &mapper.get_current_atlas(),
            target,
            &clipboard,
            0,
            Vector::new(0.0, 0.0),
            mapper.next_room_number(&target),
        );

        assert!(command.is_some());
        assert_eq!(pasted, vec![RoomNumber(3)]);
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
            mapper.next_room_number(&area_id),
        );
        let command = command.expect("command");
        assert_eq!(
            pasted,
            vec![RoomNumber(3), RoomNumber(4)],
            "same-area paste never reuses source numbers"
        );

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        drive_paste_exit_creates(&mut stack);

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
        stack.resolve(Outcome::Label {
            command: 7,
            slot: 0,
            result: Ok(id),
        });
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
            RoomNumber(1).into(),
            RoomSide::North,
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
