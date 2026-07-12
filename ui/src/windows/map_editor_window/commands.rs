//! The map editor's mutation funnel and undo/redo stack.
//!
//! Every entity mutation the editor performs flows through
//! [`CommandStack::push_and_apply`] as a [`Command`]: a list of redo
//! [`Mutation`]s plus the inverse list captured from the cache *before*
//! applying. Undo/redo replay the appropriate list through the [`Mapper`]
//! (instant cache write, background cloud sync).
//!
//! Entity creation is asynchronous (the backend assigns ids), so mutations
//! reference created entities through [`IdRef::Slot`]: an index into the
//! command's resolved-id table, filled in when the create completes
//! ([`CommandStack::resolve`]). Deletion commands pre-seed their slots with
//! the original ids, so the first redo targets the existing entity and
//! later redos target whatever the undo most recently recreated.
//!
//! Area create/rename/delete intentionally bypass this stack (not
//! undoable), and the stack is cleared when the edited area changes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use iced::{Task, Vector};
use smudgy_cloud::{
    AreaId, ExitArgs, ExitDirection, ExitId, ExitStyle, ExitUpdates, LabelArgs, LabelId,
    LabelUpdates, Mapper, RoomNumber, RoomUpdates, ShapeArgs, ShapeId, ShapeUpdates,
    mapper::{AtlasCache, RoomKey},
};
use smudgy_map_widget::map_editor::{EntityId, Selection};

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
        /// Applied once the create resolves; restores fields `ExitArgs`
        /// cannot express (style, color).
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
    ExitStyle,
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

    /// The id assigned to the most recently pushed command (e.g. to match
    /// its async create [`Outcome`]s later).
    #[must_use]
    pub fn last_command_id(&self) -> Option<CommandId> {
        self.next_id.checked_sub(1)
    }

    /// Applies a new command's redo mutations and records it for undo.
    /// Clears the redo stack; coalesces into the top entry when keys match.
    pub fn push_and_apply(&mut self, mapper: &Mapper, mut command: Command) -> Task<Outcome> {
        self.redo.clear();

        command.id = self.next_id;
        self.next_id += 1;

        let task = Self::apply(mapper, &mut command, Direction::Redo);

        let coalesced = command.coalesce.is_some()
            && command.pending == 0
            && self.undo.back().is_some_and(|top| {
                top.pending == 0 && top.coalesce == command.coalesce
            });

        if coalesced {
            if let Some(top) = self.undo.back_mut() {
                // Keep the original prior state; only the latest new state
                // matters for redo.
                top.redo = command.redo;
            }
        } else {
            self.undo.push_back(command);
            if self.undo.len() > MAX_DEPTH {
                self.undo.pop_front();
            }
        }

        task
    }

    pub fn undo(&mut self, mapper: &Mapper) -> Task<Outcome> {
        if !self.can_undo() {
            return Task::none();
        }
        let Some(mut command) = self.undo.pop_back() else {
            return Task::none();
        };
        let task = Self::apply(mapper, &mut command, Direction::Undo);
        self.redo.push(command);
        task
    }

    pub fn redo(&mut self, mapper: &Mapper) -> Task<Outcome> {
        if !self.can_redo() {
            return Task::none();
        }
        let Some(mut command) = self.redo.pop() else {
            return Task::none();
        };
        let task = Self::apply(mapper, &mut command, Direction::Redo);
        self.undo.push_back(command);
        task
    }

    /// Records the completion of an asynchronous create, filling the slot
    /// it targeted and applying any follow-up update.
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
                            mapper.update_exit(room_key, id, follow_up);
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

    /// Applies one direction's mutations: synchronous writes go straight to
    /// the mapper; creates spawn tasks whose outcomes are fed back through
    /// [`Self::resolve`].
    fn apply(mapper: &Mapper, command: &mut Command, direction: Direction) -> Task<Outcome> {
        let mutations = match direction {
            Direction::Redo => command.redo.clone(),
            Direction::Undo => command.undo.clone(),
        };

        let mut tasks = Vec::new();

        for mutation in mutations {
            match mutation {
                Mutation::UpsertRooms(area_id, updates) => mapper.upsert_rooms(area_id, updates),
                Mutation::DeleteRoom(room_key) => mapper.delete_room(room_key),
                Mutation::SetRoomProperty(room_key, name, value) => {
                    mapper.set_room_property(room_key, name, value);
                }
                Mutation::DeleteRoomProperty(room_key, name) => {
                    mapper.delete_room_property(room_key, name);
                }
                Mutation::AddRoomTag(room_key, tag) => {
                    mapper.add_room_tag(room_key, tag);
                }
                Mutation::RemoveRoomTag(room_key, tag) => {
                    mapper.remove_room_tag(room_key, tag);
                }
                Mutation::SetAreaProperty(area_id, name, value) => {
                    mapper.set_area_property(area_id, name, value);
                }
                Mutation::DeleteAreaProperty(area_id, name) => {
                    mapper.delete_area_property(area_id, name);
                }
                Mutation::CreateExit {
                    room_key,
                    args,
                    follow_up,
                    slot,
                } => {
                    command.pending += 1;
                    let command_id = command.id;
                    let mapper = mapper.clone();
                    let result_key = room_key.clone();
                    tasks.push(Task::perform(
                        {
                            let room_key = room_key.clone();
                            async move { mapper.create_exit(room_key, args).await }
                        },
                        move |result| Outcome::Exit {
                            command: command_id,
                            slot,
                            room_key: result_key.clone(),
                            follow_up: follow_up.clone(),
                            result: result.map_err(|error| error.to_string()),
                        },
                    ));
                }
                Mutation::UpdateExit {
                    room_key,
                    id,
                    updates,
                } => {
                    if let Some(exit_id) = command.exit_id(id) {
                        mapper.update_exit(room_key, exit_id, updates);
                    }
                }
                Mutation::DeleteExit { room_key, id } => {
                    if let Some(exit_id) = command.exit_id(id) {
                        mapper.delete_exit(room_key, exit_id);
                    }
                }
                Mutation::CreateLabel {
                    area_id,
                    args,
                    slot,
                } => {
                    command.pending += 1;
                    let command_id = command.id;
                    let mapper = mapper.clone();
                    tasks.push(Task::perform(
                        async move { mapper.create_label(area_id, args).await },
                        move |result| Outcome::Label {
                            command: command_id,
                            slot,
                            result: result.map_err(|error| error.to_string()),
                        },
                    ));
                }
                Mutation::UpdateLabel {
                    area_id,
                    id,
                    updates,
                } => {
                    if let Some(label_id) = command.label_id(id) {
                        mapper.update_label(area_id, label_id, updates);
                    }
                }
                Mutation::DeleteLabel { area_id, id } => {
                    if let Some(label_id) = command.label_id(id) {
                        mapper.delete_label(area_id, label_id);
                    }
                }
                Mutation::CreateShape {
                    area_id,
                    args,
                    slot,
                } => {
                    command.pending += 1;
                    let command_id = command.id;
                    let mapper = mapper.clone();
                    tasks.push(Task::perform(
                        async move { mapper.create_shape(area_id, args).await },
                        move |result| Outcome::Shape {
                            command: command_id,
                            slot,
                            result: result.map_err(|error| error.to_string()),
                        },
                    ));
                }
                Mutation::UpdateShape {
                    area_id,
                    id,
                    updates,
                } => {
                    if let Some(shape_id) = command.shape_id(id) {
                        mapper.update_shape(area_id, shape_id, updates);
                    }
                }
                Mutation::DeleteShape { area_id, id } => {
                    if let Some(shape_id) = command.shape_id(id) {
                        mapper.delete_shape(area_id, shape_id);
                    }
                }
            }
        }

        Task::batch(tasks)
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

    // Rooms must exist again before their properties and exits restore.
    let mut undo = Vec::new();
    if !undo_rooms.is_empty() {
        undo.push(Mutation::UpsertRooms(area_id, undo_rooms));
    }
    undo.extend(undo_late);

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
        style: Some(exit.style),
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
        style: Some(exit.style),
        color: exit.color.clone(),
    }
}

/// Where a new exit should land.
pub enum NewExitTarget {
    /// An existing room.
    Room(RoomNumber),
    /// A new room created at this position/level as part of the command.
    NewRoom {
        room_number: RoomNumber,
        at: iced::Point,
        level: i32,
    },
}

/// Creates an exit from `from` (two-way unless `one_way`), optionally
/// creating the destination room as part of the same undo step.
#[must_use]
pub fn create_exit(
    area_id: AreaId,
    from: RoomNumber,
    from_direction: smudgy_cloud::ExitDirection,
    to: &NewExitTarget,
    to_direction: smudgy_cloud::ExitDirection,
    one_way: bool,
) -> Command {
    let from_key = RoomKey::new(area_id, from);

    let mut redo = Vec::new();
    let mut undo = Vec::new();

    let to_room = match *to {
        NewExitTarget::Room(room_number) => room_number,
        NewExitTarget::NewRoom {
            room_number,
            at,
            level,
        } => {
            redo.push(Mutation::UpsertRooms(
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
                    },
                )],
            ));
            room_number
        }
    };
    let to_key = RoomKey::new(area_id, to_room);

    redo.push(Mutation::CreateExit {
        room_key: from_key.clone(),
        args: ExitArgs {
            from_direction,
            to_area_id: Some(area_id),
            to_room_number: Some(to_room),
            to_direction: Some(to_direction),
            weight: 1.0,
            ..Default::default()
        },
        follow_up: None,
        slot: 0,
    });

    if !one_way {
        redo.push(Mutation::CreateExit {
            room_key: to_key.clone(),
            args: ExitArgs {
                from_direction: to_direction,
                to_area_id: Some(area_id),
                to_room_number: Some(from),
                to_direction: Some(from_direction),
                weight: 1.0,
                ..Default::default()
            },
            follow_up: None,
            slot: 1,
        });
    }

    // Undo in reverse order; a created destination room takes its
    // reciprocal exit down with it.
    if !one_way {
        match to {
            NewExitTarget::Room(_) => undo.push(Mutation::DeleteExit {
                room_key: to_key,
                id: IdRef::Slot(1),
            }),
            NewExitTarget::NewRoom { .. } => {}
        }
    }
    undo.push(Mutation::DeleteExit {
        room_key: from_key,
        id: IdRef::Slot(0),
    });
    if let NewExitTarget::NewRoom { room_number, .. } = to {
        undo.push(Mutation::DeleteRoom(RoomKey::new(area_id, *room_number)));
    }

    Command::new(redo, undo)
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

/// Deletes one exit; undo recreates it (with a fresh backend id tracked
/// through a slot).
///
/// Refuses exits whose destination was redacted (`to_unknown`): the real
/// destination never reached this client, so an undo could only recreate
/// the exit dangling — silently destroying the owner's cross-area link
/// while claiming to have restored it. The inspector hides the delete
/// affordance on those rows; this guards any other path.
#[must_use]
pub fn delete_exit(
    atlas: &Arc<AtlasCache>,
    room_key: RoomKey,
    exit_id: ExitId,
) -> Option<Command> {
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
                text: "Label".to_string(),
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
        EntityId::Room(_) => None,
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
    pub style: ExitStyle,
    pub color: Option<String>,
    pub is_secret: bool,
    /// Destination redacted ("Unknown map"); always pastes dangling.
    pub to_unknown: bool,
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
    pub labels: Vec<LabelArgs>,
    pub shapes: Vec<ShapeArgs>,
}

impl EntityClipboard {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty() && self.labels.is_empty() && self.shapes.is_empty()
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
) -> EntityClipboard {
    let Some(area) = atlas.get_area(&area_id) else {
        return EntityClipboard::default();
    };

    let mut rooms = Vec::new();
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
            let exits = room
                .get_exits()
                .iter()
                .map(|exit| ExitClip {
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
                    style: exit.style,
                    color: exit.color.clone(),
                    is_secret: exit.is_secret,
                    to_unknown: exit.to_unknown,
                })
                .collect();
            rooms.push(RoomClip {
                room_number: number,
                title: room.get_title().to_string(),
                description: room.get_description().to_string(),
                level: room.get_level(),
                x: room.get_x(),
                y: room.get_y(),
                color: room.get_color().to_string(),
                is_secret: room.is_secret(),
                properties,
                exits,
            });
        }
    }

    let labels = selection
        .labels()
        .filter_map(|label_id| area.get_label(&label_id))
        .map(|label| LabelArgs {
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
        labels,
        shapes,
    }
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

/// The create mutation for one pasted exit. Style/color are inexpressible
/// in `ExitArgs`, so they ride a follow-up update applied once the create
/// resolves (skipped when they'd be the creation defaults anyway).
fn pasted_exit_create(
    room_key: RoomKey,
    exit: &ExitClip,
    destination: PastedExitDestination,
    cleared: bool,
    slot: SlotId,
) -> Mutation {
    let (to_area_id, to_room_number, to_direction) = match destination {
        PastedExitDestination::Remapped(number) => {
            (Some(room_key.area_id), Some(number), exit.to_direction)
        }
        PastedExitDestination::Live(area_id, number) => {
            (Some(area_id), Some(number), exit.to_direction)
        }
        PastedExitDestination::Dangling => (None, None, None),
    };

    let follow_up = (!matches!(exit.style, ExitStyle::Normal) || exit.color.is_some()).then(
        || ExitUpdates {
            style: Some(exit.style),
            color: exit.color.clone(),
            ..Default::default()
        },
    );

    Mutation::CreateExit {
        room_key,
        args: ExitArgs {
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
            style: Some(exit.style),
        },
        follow_up,
        slot,
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
/// can select them — room upserts apply synchronously, while labels and
/// shapes select as their async creates resolve.
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
) -> Option<(Command, Vec<RoomNumber>)> {
    if clipboard.is_empty() {
        return None;
    }
    let area = atlas.get_area(&target_area_id)?;
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
        let mapping = remap_room_numbers(
            &source_numbers,
            &occupied,
            area.next_room_number(),
            !same_area,
        );

        let mut upserts = Vec::with_capacity(clipboard.rooms.len());
        // Properties and exits, after the room batch creates their owners.
        let mut late = Vec::new();
        for room in &clipboard.rooms {
            let number = mapping[&room.room_number];
            assert!(
                !occupied.contains(&number),
                "paste remap produced an occupied room number"
            );
            pasted_rooms.push(number);
            upserts.push((
                number,
                RoomUpdates {
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
                },
            ));

            let room_key = RoomKey::new(target_area_id, number);
            for (name, value) in &room.properties {
                late.push(Mutation::SetRoomProperty(
                    room_key.clone(),
                    name.clone(),
                    value.clone(),
                ));
            }
            for exit in &room.exits {
                let destination = classify_pasted_exit(exit, source_area_id, &mapping, |id| {
                    atlas.get_area(&id).is_some()
                });
                let slot = next_slot;
                next_slot += 1;
                late.push(pasted_exit_create(
                    room_key.clone(),
                    exit,
                    destination,
                    cleared,
                    slot,
                ));
            }
        }

        redo.push(Mutation::UpsertRooms(target_area_id, upserts));
        redo.extend(late);
        // Undo deletes the pasted rooms; their properties and exits
        // cascade with them.
        for number in &pasted_rooms {
            undo.push(Mutation::DeleteRoom(RoomKey::new(target_area_id, *number)));
        }
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

    Some((Command::new(redo, undo), pasted_rooms))
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
        .coalescing(CoalesceKey::new(
            EntityRef::Label(area_id, label_id),
            field,
        )),
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
        shape_type: updates.shape_type.as_ref().map(|_| shape.shape_type.clone()),
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
        .coalescing(CoalesceKey::new(
            EntityRef::Shape(area_id, shape_id),
            field,
        )),
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
        Area, AreaUpdates, AreaWithDetails, CreateAreaRequest, Exit, ExitDirection, Label,
        CloudError, CloudResult, MapperBackend, Room, Shape, Uuid,
    };
    use smudgy_map_widget::map_editor::{EntityId, Selection};

    /// A backend that fabricates ids and accepts every operation.
    #[derive(Default)]
    struct MockBackend;

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
            })
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

        async fn set_area_property(
            &self,
            _area_id: &AreaId,
            _name: &str,
            _value: &str,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_area_property(&self, _area_id: &AreaId, _name: &str) -> CloudResult<()> {
            Ok(())
        }

        async fn update_room(&self, room_key: &RoomKey, updates: RoomUpdates) -> CloudResult<Room> {
            Ok(Room {
                area_id: room_key.area_id,
                room_number: room_key.room_number,
                title: updates.title.unwrap_or_default(),
                description: updates.description.unwrap_or_default(),
                level: updates.level.unwrap_or_default(),
                x: updates.x.unwrap_or_default(),
                y: updates.y.unwrap_or_default(),
                color: updates.color.unwrap_or_default(),
                created_at: chrono::Utc::now(),
                is_secret: updates.is_secret.unwrap_or_default(),
            })
        }

        async fn delete_room(&self, _room_key: &RoomKey) -> CloudResult<()> {
            Ok(())
        }

        async fn set_room_property(
            &self,
            _room_key: &RoomKey,
            _name: &str,
            _value: &str,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_room_property(&self, _room_key: &RoomKey, _name: &str) -> CloudResult<()> {
            Ok(())
        }

        async fn add_room_tag(&self, _room_key: &RoomKey, _tag: &str) -> CloudResult<()> {
            Ok(())
        }

        async fn remove_room_tag(&self, _room_key: &RoomKey, _tag: &str) -> CloudResult<()> {
            Ok(())
        }

        async fn create_room_exit(
            &self,
            _room_key: &RoomKey,
            exit_data: ExitArgs,
        ) -> CloudResult<Exit> {
            Ok(Exit {
                id: ExitId(Uuid::new_v4()),
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
                style: smudgy_cloud::ExitStyle::Normal,
                color: String::new(),
                to_unknown: false,
                to_area_token: None,
                is_secret: exit_data.is_secret.unwrap_or_default(),
            })
        }

        async fn update_exit(
            &self,
            _area_id: &AreaId,
            _exit_id: &ExitId,
            _updates: ExitUpdates,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_exit(&self, _area_id: &AreaId, _exit_id: &ExitId) -> CloudResult<()> {
            Ok(())
        }

        async fn create_label(&self, _area_id: &AreaId, label_data: LabelArgs) -> CloudResult<Label> {
            Ok(Label {
                id: LabelId(Uuid::new_v4()),
                level: label_data.level,
                x: label_data.x,
                y: label_data.y,
                width: label_data.width,
                height: label_data.height,
                horizontal_alignment: label_data.horizontal_alignment,
                vertical_alignment: label_data.vertical_alignment,
                text: label_data.text,
                color: label_data.color,
                // Mimic the deployed server, which fills in defaults for
                // absent colors on creation — clients must always send
                // explicit values or transparency turns white.
                background_color: label_data
                    .background_color
                    .unwrap_or_else(|| "white".to_string()),
                font_size: label_data.font_size,
                font_weight: label_data.font_weight,
                is_secret: label_data.is_secret.unwrap_or_default(),
            })
        }

        async fn update_label(
            &self,
            _area_id: &AreaId,
            _label_id: &LabelId,
            _updates: LabelUpdates,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_label(&self, _area_id: &AreaId, _label_id: &LabelId) -> CloudResult<()> {
            Ok(())
        }

        async fn create_shape(&self, _area_id: &AreaId, shape_data: ShapeArgs) -> CloudResult<Shape> {
            Ok(Shape {
                id: ShapeId(Uuid::new_v4()),
                level: shape_data.level,
                x: shape_data.x,
                y: shape_data.y,
                width: shape_data.width,
                height: shape_data.height,
                // Mimic the deployed server's creation defaults; see
                // create_label above.
                background_color: shape_data
                    .background_color
                    .or_else(|| Some("grey".to_string())),
                stroke_color: shape_data.stroke_color,
                shape_type: shape_data.shape_type,
                border_radius: shape_data.border_radius,
                stroke_width: shape_data.stroke_width.unwrap_or_default(),
                is_secret: shape_data.is_secret.unwrap_or_default(),
            })
        }

        async fn update_shape(
            &self,
            _area_id: &AreaId,
            _shape_id: &ShapeId,
            _updates: ShapeUpdates,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_shape(&self, _area_id: &AreaId, _shape_id: &ShapeId) -> CloudResult<()> {
            Ok(())
        }
    }

    fn test_mapper() -> Mapper {
        let dir = std::env::temp_dir().join(format!("smudgy-test-{}", Uuid::new_v4()));
        Mapper::new(std::sync::Arc::new(MockBackend), dir)
    }

    async fn area_with_rooms(mapper: &Mapper, rooms: &[(i32, f32, f32)]) -> AreaId {
        let area_id = mapper.create_area("Test".into()).await.expect("area");
        for (number, x, y) in rooms {
            mapper.upsert_room(
                RoomKey::new(area_id, RoomNumber(*number)),
                RoomUpdates {
                    title: Some(format!("Room {number}")),
                    x: Some(*x),
                    y: Some(*y),
                    ..Default::default()
                },
            );
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
        let area_id = area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 1.0, 0.0), (3, 2.0, 5.0)]).await;

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

        mapper.set_room_property(key.clone(), "zone".into(), "docks".into());
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

        // Undo: the room and property restore synchronously; the exit is an
        // async create we drive by hand instead of through an iced runtime.
        let _ = stack.undo(&mapper);

        let new_exit_id = {
            let undone = stack.redo.last().expect("undone command");
            let mut created = None;
            for mutation in undone.undo.clone() {
                if let Mutation::CreateExit {
                    room_key,
                    args,
                    follow_up,
                    slot,
                } = mutation
                {
                    let id = mapper
                        .create_exit(room_key.clone(), args)
                        .await
                        .expect("recreate exit");
                    stack.resolve(
                        &mapper,
                        Outcome::Exit {
                            command: stack.redo.last().expect("cmd").id,
                            slot,
                            room_key,
                            follow_up,
                            result: Ok(id),
                        },
                    );
                    created = Some(id);
                }
            }
            created.expect("exit recreated")
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
        // recreate, so no async create work is needed here.
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
        let command = delete_selection(&mapper.get_current_atlas(), area_id, &selection)
            .expect("command");

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

        // Drive the async recreates the dropped Task would have run.
        let command_id = stack.redo.last().expect("undone").id;
        let mutations = stack.redo.last().expect("undone").undo.clone();
        for mutation in mutations {
            match mutation {
                Mutation::CreateExit {
                    room_key,
                    args,
                    follow_up,
                    slot,
                } => {
                    let id = mapper
                        .create_exit(room_key.clone(), args)
                        .await
                        .expect("recreate exit");
                    stack.resolve(
                        &mapper,
                        Outcome::Exit {
                            command: command_id,
                            slot,
                            room_key,
                            follow_up,
                            result: Ok(id),
                        },
                    );
                }
                Mutation::CreateLabel { args, slot, .. } => {
                    let id = mapper
                        .create_label(area_id, args)
                        .await
                        .expect("recreate label");
                    stack.resolve(
                        &mapper,
                        Outcome::Label {
                            command: command_id,
                            slot,
                            result: Ok(id),
                        },
                    );
                }
                Mutation::UpsertRooms(..) => {}
                other => panic!("unexpected undo mutation: {other:?}"),
            }
        }

        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area");
        let room = area.get_room(&RoomNumber(1)).expect("room restored");
        assert!(room.is_secret(), "room secrecy restored");
        assert!(room.get_exits()[0].is_secret, "exit secrecy restored");
        assert!(area.get_labels()[0].is_secret, "label secrecy restored");
    }

    #[tokio::test]
    async fn paste_creates_offset_copies_and_undo_removes_them() {
        let mapper = test_mapper();
        let area_id = mapper.create_area("Test".into()).await.expect("area");

        let clipboard = EntityClipboard {
            source_area_id: Some(area_id),
            rooms: vec![],
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
        let (command, pasted_rooms) = paste_clipboard(
            &mapper.get_current_atlas(),
            area_id,
            &clipboard,
            3,
            Vector::new(1.0, 1.0),
        )
        .expect("command");
        assert!(pasted_rooms.is_empty());
        let _ = stack.push_and_apply(&mapper, command);

        assert!(!stack.can_undo(), "pending creates block undo");

        // Drive the async creates the dropped Task would have run.
        let command_id = stack.undo.back().expect("pushed").id;
        let mutations = stack.undo.back().expect("pushed").redo.clone();
        for mutation in mutations {
            match mutation {
                Mutation::CreateLabel { args, slot, .. } => {
                    let id = mapper.create_label(area_id, args).await.expect("label");
                    stack.resolve(
                        &mapper,
                        Outcome::Label {
                            command: command_id,
                            slot,
                            result: Ok(id),
                        },
                    );
                }
                Mutation::CreateShape { args, slot, .. } => {
                    let id = mapper.create_shape(area_id, args).await.expect("shape");
                    stack.resolve(
                        &mapper,
                        Outcome::Shape {
                            command: command_id,
                            slot,
                            result: Ok(id),
                        },
                    );
                }
                other => panic!("unexpected paste mutation: {other:?}"),
            }
        }

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
        let area_id = mapper.create_area("Test".into()).await.expect("area");

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
        let clipboard =
            snapshot_selection(&mapper.get_current_atlas(), area_id, &selection, false);
        assert_eq!(
            clipboard.labels[0].background_color.as_deref(),
            Some(""),
            "snapshot must not erase the transparent background"
        );

        let (command, _) = paste_clipboard(
            &mapper.get_current_atlas(),
            area_id,
            &clipboard,
            0,
            Vector::new(1.0, 1.0),
        )
        .expect("paste command");
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
        assert_eq!(mapping[&RoomNumber(10)], RoomNumber(10), "vacant number kept");
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
            style: ExitStyle::Normal,
            color: None,
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

    /// Runs the async exit creates the just-pushed paste command issued
    /// (the iced Task that would drive them is dropped in tests).
    async fn drive_paste_exit_creates(mapper: &Mapper, stack: &mut CommandStack) {
        let command_id = stack.undo.back().expect("pushed").id;
        let mutations = stack.undo.back().expect("pushed").redo.clone();
        for mutation in mutations {
            if let Mutation::CreateExit {
                room_key,
                args,
                follow_up,
                slot,
            } = mutation
            {
                let id = mapper
                    .create_exit(room_key.clone(), args)
                    .await
                    .expect("exit");
                stack.resolve(
                    mapper,
                    Outcome::Exit {
                        command: command_id,
                        slot,
                        room_key,
                        follow_up,
                        result: Ok(id),
                    },
                );
            }
        }
    }

    #[tokio::test]
    async fn cross_area_paste_preserves_vacant_numbers_and_remaps_exits() {
        let mapper = test_mapper();
        let source =
            area_with_rooms(&mapper, &[(1, 0.0, 0.0), (2, 3.0, 0.0), (3, 6.0, 0.0)]).await;
        // Room 2 is taken in the target; room 1 is vacant there.
        let target = area_with_rooms(&mapper, &[(2, 50.0, 50.0)]).await;

        mapper.set_room_property(
            RoomKey::new(source, RoomNumber(1)),
            "zone".into(),
            "docks".into(),
        );
        // 1 → 2: both ends copied. 1 → 3: room 3 stays behind.
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

        let clipboard = snapshot_selection(
            &mapper.get_current_atlas(),
            source,
            &select_rooms(&[1, 2]),
            true,
        );
        assert_eq!(clipboard.source_area_id, Some(source));
        assert_eq!(clipboard.rooms.len(), 2);

        let (command, pasted) = paste_clipboard(
            &mapper.get_current_atlas(),
            target,
            &clipboard,
            0,
            Vector::new(0.0, 0.0),
        )
        .expect("command");
        // Room 1 keeps its number (vacant in the target); room 2 collides
        // with the target's own room 2 and reallocates.
        assert_eq!(pasted, vec![RoomNumber(1), RoomNumber(3)]);

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        drive_paste_exit_creates(&mapper, &mut stack).await;

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
            assert_eq!(exits.len(), 2);
            let to_copied = exits
                .iter()
                .find(|exit| exit.from_direction == ExitDirection::East)
                .expect("east exit");
            assert_eq!(
                (to_copied.to_area_id, to_copied.to_room_number),
                (Some(target), Some(RoomNumber(3))),
                "intra-selection exit remapped to the pasted copy"
            );
            let left_behind = exits
                .iter()
                .find(|exit| exit.from_direction == ExitDirection::North)
                .expect("north exit");
            assert_eq!(
                (left_behind.to_area_id, left_behind.to_room_number),
                (None, None),
                "exit to a non-copied room pastes dangling"
            );

            let existing = area.get_room(&RoomNumber(2)).expect("target room 2");
            assert_eq!(
                (existing.get_x(), existing.get_y()),
                (50.0, 50.0),
                "the target's own room is untouched"
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
        );
        let (command, pasted) = paste_clipboard(
            &mapper.get_current_atlas(),
            area_id,
            &clipboard,
            0,
            Vector::new(1.0, 1.0),
        )
        .expect("command");
        assert_eq!(
            pasted,
            vec![RoomNumber(3), RoomNumber(4)],
            "same-area paste never reuses source numbers"
        );

        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        drive_paste_exit_creates(&mapper, &mut stack).await;

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

        stack.resolve(
            &mapper,
            Outcome::Label {
                command: 7,
                slot: 0,
                result: Ok(LabelId(Uuid::new_v4())),
            },
        );
        assert!(stack.can_undo(), "resolution unblocks undo");
    }
}
