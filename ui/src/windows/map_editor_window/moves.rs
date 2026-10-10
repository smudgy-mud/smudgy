//! Moving content between a map's places. The inspector's "In" field and
//! the context menu's "Move to" run the same move: one place to one place,
//! on the server as one transaction. A room keeps its number unless the
//! destination already uses it, when the server gives it the destination's
//! next one and says so; selection and undo follow the room either way.
//! Undo and redo ask for the numbers the rooms had where they return to, so
//! a renumbered room comes back under its old number while that is free.
//! The links touching the rooms travel with them.

use std::collections::BTreeMap;

use iced::alignment::Vertical;
use iced::widget::{pick_list, row, text};
use iced::{Padding, Task};
use smudgy_cloud::Uuid;
use smudgy_cloud::access_review::ReviewedMove;
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::mutation::{
    PropertyAddress, PropertyChoice, PropertyResolution, RoomRenumbering,
};
use smudgy_cloud::{AreaId, CloudError, MovedContent, RoomNumber, SourceId};
use smudgy_map_widget::map_editor::EntityId;
use smudgy_map_widget::sources;

use crate::components::cloud_errors::display_error;
use crate::theme::Element as ThemedElement;
use crate::update::Update;

use super::secrets::{Place, can_add, can_remove, dot, place_name};
use super::{Event, MapEditorWindow, Message, modals};

#[derive(Debug, Clone)]
pub enum MoveMessage {
    /// Move the selection to this place (after any confirmation).
    Requested(SourceId),
    ReviewLoaded(Uuid, Result<Box<ReviewedMove>, String>),
    ReviewConfirmed,
    ResolveProperty(PropertyResolution),
    PropertyRequested {
        from: SourceId,
        to: SourceId,
        property: PropertyAddress,
    },
    /// The move landed, renumbering these rooms, or failed with this message.
    Finished(Result<Vec<RoomRenumbering>, String>),
}

/// A move that happened: content went from `from` to `to` on a map.
#[derive(Debug, Clone)]
pub struct MoveRecord {
    pub area_id: AreaId,
    pub from: SourceId,
    pub to: SourceId,
    /// The content, its rooms under their numbers in `from`.
    pub content: MovedContent,
    /// The same rooms, in the same order, under their numbers in `to`.
    pub landed: Vec<RoomNumber>,
    pub preserves_undo: bool,
}

impl MoveRecord {
    /// The content as it stands in `to`, for moving it back: each room asks
    /// for the number it had in `from`, so a room the move renumbered comes
    /// back under its old number when that is free.
    fn landed_content(&self) -> MovedContent {
        MovedContent {
            rooms: self.landed.clone(),
            asked: asking(&self.landed, &self.content.rooms),
            ..self.content.clone()
        }
    }

    /// The content as it stands in `from`, for moving it again: each room
    /// asks for the number it had in `to`.
    fn content_again(&self) -> MovedContent {
        MovedContent {
            asked: asking(&self.content.rooms, &self.landed),
            ..self.content.clone()
        }
    }

    /// Brings the record up to date once it ran as `kind` and the move
    /// renumbered `renumbering`: a forward run says where the rooms landed in
    /// `to`, an undo where they came back to in `from`. Undo and redo then
    /// name the rooms wherever they are.
    fn follow(&mut self, kind: MoveKind, renumbering: &[RoomRenumbering]) {
        let follow = |rooms: &[RoomNumber]| -> Vec<RoomNumber> {
            rooms
                .iter()
                .map(|number| renumbered(*number, renumbering))
                .collect()
        };
        match kind {
            MoveKind::Undo => self.content.rooms = follow(&self.landed),
            MoveKind::Asked { .. } | MoveKind::Redo => self.landed = follow(&self.content.rooms),
        }
    }
}

/// The number each of `rooms` asks for, pairing it with the same room in
/// `wanted`, where the two differ.
fn asking(rooms: &[RoomNumber], wanted: &[RoomNumber]) -> BTreeMap<RoomNumber, RoomNumber> {
    rooms
        .iter()
        .zip(wanted)
        .filter(|(room, wanted)| room != wanted)
        .map(|(room, wanted)| (*room, *wanted))
        .collect()
}

/// Room `number` as a move that renumbered `renumbered` left it.
fn renumbered(number: RoomNumber, renumbered: &[RoomRenumbering]) -> RoomNumber {
    renumbered
        .iter()
        .find(|renumbering| renumbering.from == number)
        .map_or(number, |renumbering| renumbering.to)
}

/// The last move that lost nothing, below the command stack's history:
/// a move clears that history, so every entry in it is newer. Undo takes
/// the stack's entries first, then moves the content back.
#[derive(Debug, Default)]
pub struct MoveHistory {
    pub undo: Option<MoveRecord>,
    pub redo: Option<MoveRecord>,
}

/// Why a move runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveKind {
    /// Asked for; `undoable` when it loses nothing.
    Asked {
        undoable: bool,
    },
    Undo,
    Redo,
}

/// Where the selected content lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionPlace {
    One(SourceId),
    Several,
}

fn muted(theme: &crate::Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

impl MapEditorWindow {
    /// The place of each selected room, label and shape; links travel with
    /// their rooms and don't count. `None` when nothing movable is selected.
    pub(super) fn selection_place(&self, area: &AreaCache) -> Option<SelectionPlace> {
        let mut places = self
            .editor
            .selection()
            .iter()
            .filter_map(|entity| match entity {
                EntityId::Room(_) => Some(SourceId::Map),
                EntityId::SourceRoom(source, _) => Some(source),
                EntityId::Label(id) => area
                    .find_label(&id)
                    .map(|(layer, _)| layer.map_or(SourceId::Map, |layer| layer.source())),
                EntityId::Shape(id) => area
                    .find_shape(&id)
                    .map(|(layer, _)| layer.map_or(SourceId::Map, |layer| layer.source())),
                EntityId::Connection(id) => area
                    .find_connection(id)
                    .map(|(layer, _)| layer.map_or(SourceId::Map, |layer| layer.source())),
            });
        let first = places.next()?;
        Some(if places.all(|place| place == first) {
            SelectionPlace::One(first)
        } else {
            SelectionPlace::Several
        })
    }

    /// The places the selection can move to: elsewhere it may be added, when
    /// the viewer may take it from where it is.
    pub(super) fn move_targets(&self, area: &AreaCache, from: SourceId) -> Vec<Place> {
        if self.moving
            || !can_remove(area, from)
            || self.mapper.area_storage(area.get_id()) != smudgy_cloud::MapStorage::Cloud
        {
            return Vec::new();
        }
        let mut sources = vec![SourceId::Map];
        sources.extend(
            super::secrets::secrets(area)
                .iter()
                .map(|bundle| bundle.source),
        );
        sources.push(SourceId::Private);
        sources
            .into_iter()
            .filter(|source| *source != from && can_add(area, *source))
            .map(|source| Place {
                source,
                name: place_name(area, source),
            })
            .collect()
    }

    pub(super) fn selected_content(&self, area: &AreaCache) -> MovedContent {
        let mut content = MovedContent::default();
        for entity in self.editor.selection().iter() {
            match entity {
                EntityId::Room(number) | EntityId::SourceRoom(_, number) => {
                    content.rooms.push(number);
                }
                EntityId::Label(id) if area.find_label(&id).is_some() => content.labels.push(id),
                EntityId::Shape(id) if area.find_shape(&id).is_some() => content.shapes.push(id),
                EntityId::Connection(id) if area.find_connection(id).is_some() => {
                    content.connections.push(id)
                }
                _ => {}
            }
        }
        content
    }

    pub(super) fn update_move(&mut self, message: MoveMessage) -> Update<Message, Event> {
        // A move lands whatever the editor shows by then.
        if let MoveMessage::Finished(result) = message {
            return self.finish_move(result);
        }
        if let MoveMessage::ReviewLoaded(id, result) = message {
            if !matches!(&self.modal, Some(modals::Modal::ReviewMove { id: current, .. }) if *current == id)
            {
                return Update::none();
            }
            match result {
                Ok(review) => {
                    let required = review.review.requires_confirmation;
                    if let Some(modals::Modal::ReviewMove { reviewed, .. }) = &mut self.modal {
                        *reviewed = Some(review);
                    }
                    if !required {
                        return self.commit_move_review();
                    }
                }
                Err(error) => {
                    self.modal = None;
                    return self.finish_move(Err(error));
                }
            }
            return Update::none();
        }
        if let MoveMessage::ResolveProperty(choice) = message {
            return self.resolve_move_property(choice);
        }
        if matches!(message, MoveMessage::ReviewConfirmed) {
            return self.commit_move_review();
        }
        if self.moving {
            return Update::none();
        }
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&area_id) else {
            return Update::none();
        };
        match message {
            MoveMessage::PropertyRequested { from, to, property } => {
                if !self
                    .move_targets(&area, from)
                    .iter()
                    .any(|place| place.source == to)
                {
                    return Update::none();
                }
                self.start_move(
                    from,
                    to,
                    MovedContent {
                        properties: vec![property],
                        ..MovedContent::default()
                    },
                    MoveKind::Asked { undoable: true },
                )
            }
            MoveMessage::Requested(to) => {
                let Some(SelectionPlace::One(from)) = self.selection_place(&area) else {
                    return Update::none();
                };
                if !self
                    .move_targets(&area, from)
                    .iter()
                    .any(|place| place.source == to)
                {
                    return Update::none();
                }
                let content = self.selected_content(&area);
                if content.is_empty() {
                    return Update::none();
                }
                self.start_move(from, to, content, MoveKind::Asked { undoable: true })
            }
            MoveMessage::Finished(_)
            | MoveMessage::ReviewLoaded(..)
            | MoveMessage::ReviewConfirmed
            | MoveMessage::ResolveProperty(_) => Update::none(),
        }
    }

    fn finish_move(
        &mut self,
        result: Result<Vec<RoomRenumbering>, String>,
    ) -> Update<Message, Event> {
        self.moving = false;
        self.editor.set_editable(self.canvas_editable());
        let moved = self.moved_rooms.take();
        let running = self.running_move.take();
        // Only the map the move ran on is affected; the editor may show
        // another one by now.
        let here = moved
            .as_ref()
            .is_some_and(|(area_id, _, _)| self.editor.area_id() == Some(*area_id));
        match result {
            Ok(renumbering) => {
                if let Some(id) = self.clipboard_cut_in_flight.take()
                    && self
                        .clipboard
                        .load()
                        .cut
                        .as_ref()
                        .is_some_and(|cut| cut.id == id)
                {
                    self.clipboard.store(std::sync::Arc::new(
                        super::commands::EntityClipboard::default(),
                    ));
                }
                // The move announced itself to every holder of the mapper;
                // this window keeps its own record instead.
                let _ = self.room_remaps.take();
                if here {
                    // The history's edits name the places content was in; a
                    // move makes them wrong.
                    self.stack.clear();
                    let drawings: Vec<_> = running
                        .as_ref()
                        .into_iter()
                        .flat_map(|(record, _)| {
                            record
                                .content
                                .labels
                                .iter()
                                .copied()
                                .map(EntityId::Label)
                                .chain(record.content.shapes.iter().copied().map(EntityId::Shape))
                                .chain(
                                    record
                                        .content
                                        .connections
                                        .iter()
                                        .copied()
                                        .map(EntityId::Connection),
                                )
                        })
                        .collect();
                    if let Some((mut record, kind)) = running {
                        record.follow(kind, &renumbering);
                        if !record.preserves_undo {
                            self.move_history = MoveHistory::default();
                        } else {
                            match kind {
                                MoveKind::Asked { undoable } => {
                                    self.move_history.undo = undoable.then_some(record);
                                    self.move_history.redo = None;
                                }
                                MoveKind::Undo => self.move_history.redo = Some(record),
                                MoveKind::Redo => self.move_history.undo = Some(record),
                            }
                        }
                    }
                    self.editor_notice = None;
                    if let Some((_, to, rooms)) = moved {
                        self.editor.clear_selection();
                        for entity in drawings {
                            self.editor.add_to_selection(entity);
                        }
                        for number in rooms {
                            let number = renumbered(number, &renumbering);
                            self.editor.add_to_selection(if to.is_map() {
                                EntityId::Room(number)
                            } else {
                                EntityId::SourceRoom(to, number)
                            });
                        }
                        self.selection_reset();
                    }
                }
            }
            Err(error) => {
                self.clipboard_cut_in_flight = None;
                match running {
                    Some((record, MoveKind::Undo)) => self.move_history.undo = Some(record),
                    Some((record, MoveKind::Redo)) => self.move_history.redo = Some(record),
                    _ => {}
                }
                self.editor_notice = Some((std::time::Instant::now(), error));
            }
        }
        self.inspector.resync(&self.mapper, &self.editor);
        Update::none()
    }

    /// Moves link `connection` from place `from` to place `to` on its own,
    /// with its exits, as one move. Into the map it asks first, since
    /// everyone who reads the map will see it; it loses nothing either way,
    /// so undo moves it back.
    pub(super) fn move_link(
        &mut self,
        connection: smudgy_cloud::ConnectionId,
        from: SourceId,
        to: SourceId,
    ) -> Update<Message, Event> {
        if from == to || self.moving {
            return Update::none();
        }
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        let Some(area) = self.mapper.get_current_atlas().get_area(&area_id) else {
            return Update::none();
        };
        if !can_remove(&area, from) || !can_add(&area, to) {
            return Update::none();
        }
        let content = MovedContent {
            connections: vec![connection],
            ..MovedContent::default()
        };
        self.start_move(from, to, content, MoveKind::Asked { undoable: true })
    }

    /// Undo and redo use the same server disclosure review as a new move.
    pub(super) fn replay_move(&mut self, redo: bool) -> Update<Message, Event> {
        let record = if redo {
            self.move_history.redo.take()
        } else {
            self.move_history.undo.take()
        };
        let Some(record) = record else {
            return Update::none();
        };
        if self.editor.area_id() != Some(record.area_id) {
            return Update::none();
        }
        let (from, to, kind) = if redo {
            (record.from, record.to, MoveKind::Redo)
        } else {
            (record.to, record.from, MoveKind::Undo)
        };
        // Undo moves the rooms back from where they landed, and redo moves
        // them again from where undo left them; each asks for the numbers
        // the rooms had where they are going.
        let content = if redo {
            record.content_again()
        } else {
            record.landed_content()
        };
        let update = self.start_move(from, to, content, kind);
        // Undo and redo keep the move as it first happened, so the other
        // can replay it.
        if let Some(running) = &mut self.running_move {
            running.0 = record;
        }
        update
    }

    pub(super) fn start_move(
        &mut self,
        from: SourceId,
        to: SourceId,
        content: MovedContent,
        kind: MoveKind,
    ) -> Update<Message, Event> {
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        self.running_move = Some((
            MoveRecord {
                area_id,
                from,
                to,
                landed: content.rooms.clone(),
                preserves_undo: true,
                content: content.clone(),
            },
            kind,
        ));
        self.moving = true;
        self.editor.set_editable(false);
        self.editor_notice = Some((std::time::Instant::now(), crate::i18n::t!("mapper-moving")));
        let id = Uuid::new_v4();
        self.modal = Some(modals::Modal::ReviewMove {
            id,
            from,
            to,
            content: content.clone(),
            destination: self
                .mapper
                .get_current_atlas()
                .get_area(&area_id)
                .map(|area| place_name(&area, to))
                .unwrap_or_default(),
            reviewed: None,
            source_names: self
                .mapper
                .get_current_atlas()
                .get_area(&area_id)
                .map(|area| {
                    std::iter::once(SourceId::Map)
                        .chain(std::iter::once(SourceId::Private))
                        .chain(
                            super::secrets::secrets(&area)
                                .iter()
                                .map(|source| source.source),
                        )
                        .map(|source| (source, place_name(&area, source)))
                        .collect()
                })
                .unwrap_or_default(),
        });
        let mapper = self.mapper.clone();
        Update::with_task(Task::perform(
            async move {
                mapper
                    .review_move_content(area_id, from, to, content)
                    .await
                    .map(Box::new)
                    .map_err(|error| display_error(&error))
            },
            move |result| Message::Move(MoveMessage::ReviewLoaded(id, result)),
        ))
    }

    pub(super) fn cancel_move_review(&mut self) {
        if !matches!(self.modal, Some(modals::Modal::ReviewMove { .. })) {
            return;
        }
        self.modal = None;
        self.abandon_move_review();
    }

    pub(super) fn abandon_move_review(&mut self) {
        self.clipboard_cut_in_flight = None;
        if let Some((record, kind)) = self.running_move.take()
            && self.editor.area_id() == Some(record.area_id)
        {
            match kind {
                MoveKind::Undo => self.move_history.undo = Some(record),
                MoveKind::Redo => self.move_history.redo = Some(record),
                MoveKind::Asked { .. } => {}
            }
        }
        self.moving = false;
        self.moved_rooms = None;
        self.editor_notice = None;
        self.editor.set_editable(self.canvas_editable());
    }

    fn resolve_move_property(&mut self, choice: PropertyResolution) -> Update<Message, Event> {
        let Some(modals::Modal::ReviewMove {
            id,
            from,
            to,
            content,
            reviewed,
            ..
        }) = &mut self.modal
        else {
            return Update::none();
        };
        let allowed = reviewed.as_ref().is_some_and(|reviewed| {
            reviewed.review.property_conflicts.iter().any(|conflict| {
                conflict.property == choice.property
                    && (choice.keep == PropertyChoice::Destination || conflict.can_replace)
            })
        });
        if !allowed {
            return Update::none();
        }
        let Some((record, _)) = &self.running_move else {
            return Update::none();
        };
        let area_id = record.area_id;
        content
            .property_resolutions
            .retain(|old| old.property != choice.property);
        content.property_resolutions.push(choice);
        *reviewed = None;
        *id = Uuid::new_v4();
        let (id, from, to, content) = (*id, *from, *to, content.clone());
        let mapper = self.mapper.clone();
        Update::with_task(Task::perform(
            async move {
                mapper
                    .review_move_content(area_id, from, to, content)
                    .await
                    .map(Box::new)
                    .map_err(|error| display_error(&error))
            },
            move |result| Message::Move(MoveMessage::ReviewLoaded(id, result)),
        ))
    }

    fn commit_move_review(&mut self) -> Update<Message, Event> {
        if !matches!(
            &self.modal,
            Some(modals::Modal::ReviewMove {
                reviewed: Some(reviewed),
                ..
            }) if reviewed.properties_resolved()
        ) {
            return Update::none();
        }
        let Some(modals::Modal::ReviewMove {
            from,
            to,
            content,
            reviewed: Some(reviewed),
            ..
        }) = self.modal.take()
        else {
            unreachable!()
        };
        let Some((record, _)) = &mut self.running_move else {
            return Update::none();
        };
        record.preserves_undo = reviewed.review.preserves_undo;
        // Resolution choices apply only to this request. Undo/redo must review
        // the then-current values rather than replay a previous discard choice.
        record.content.property_resolutions.clear();
        let area_id = record.area_id;
        // Keep the selection while committing. A refusal must leave it intact.
        self.moved_rooms = Some((area_id, to, content.rooms.clone()));
        let (from_name, to_name) = self
            .mapper
            .get_current_atlas()
            .get_area(&area_id)
            .map(|area| (place_name(&area, from), place_name(&area, to)))
            .unwrap_or_default();
        let mapper = self.mapper.clone();
        Update::with_task(Task::perform(
            async move {
                mapper
                    .commit_reviewed_move(*reviewed)
                    .await
                    .map(|moved| moved.renumbered)
                    .map_err(|error| move_error(&error, &from_name, &to_name))
            },
            |result| Message::Move(MoveMessage::Finished(result)),
        ))
    }
}

/// What a failed move from `from` to `to` tells the user. A revision
/// conflict means one of the two places was changed elsewhere first; the
/// mapper shows that change by the time the move returns, so moving again
/// stands on it.
fn move_error(error: &CloudError, from: &str, to: &str) -> String {
    match error {
        CloudError::RevisionConflict { .. } => crate::i18n::t!(
            "mapper-move-conflict",
            "from" => from,
            "to" => to
        ),
        _ => display_error(error),
    }
}

/// The inspector's "In" field: where the selection lives, as a picker of
/// the places it can move to; plain text where it can't move; "Several"
/// for a selection spread over places. `None` off cloud maps.
pub fn in_field(window: &MapEditorWindow) -> Option<ThemedElement<'_, Message>> {
    if !window.secrets_apply() {
        return None;
    }
    let area = window
        .mapper
        .get_current_atlas()
        .get_area(&window.editor.area_id()?)?;
    let label = text(crate::i18n::t!("inspector-in")).size(13).style(muted);
    let from = match window.selection_place(&area)? {
        SelectionPlace::Several => {
            return Some(
                row![
                    label,
                    text(crate::i18n::t!("mapper-place-several")).size(13)
                ]
                .spacing(8)
                .align_y(Vertical::Center)
                .into(),
            );
        }
        SelectionPlace::One(from) => from,
    };
    let current = Place {
        source: from,
        name: place_name(&area, from),
    };
    let mut field = row![label].spacing(8).align_y(Vertical::Center);
    if let Some(color) = sources::source_color(&area, from) {
        field = field.push(dot(Some(color)));
    }
    let targets = window.move_targets(&area, from);
    if targets.is_empty() {
        return Some(field.push(text(current.name).size(13)).into());
    }
    let mut options = vec![current.clone()];
    options.extend(targets);
    field = field.push(
        pick_list(options, Some(current), |place: Place| {
            Message::Move(MoveMessage::Requested(place.source))
        })
        .text_size(13.0)
        .padding(Padding {
            top: 3.0,
            bottom: 3.0,
            left: 8.0,
            right: 6.0,
        }),
    );
    Some(field.into())
}

/// A property moves under its own source's Remove and the destination's Add.
/// The picker is independent of whether the actor may edit the property's value.
pub(super) fn property_move<'a>(
    window: &'a MapEditorWindow,
    from: SourceId,
    property: PropertyAddress,
) -> Option<ThemedElement<'a, Message>> {
    if !window.secrets_apply() {
        return None;
    }
    let area = window
        .mapper
        .get_current_atlas()
        .get_area(&window.editor.area_id()?)?;
    let targets = window.move_targets(&area, from);
    if targets.is_empty() {
        return None;
    }
    Some(
        pick_list(targets, None::<Place>, move |place| {
            Message::Move(MoveMessage::PropertyRequested {
                from,
                to: place.source,
                property: property.clone(),
            })
        })
        .placeholder(crate::i18n::t!("move-property-to"))
        .text_size(12.0)
        .padding(4)
        .into(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::mutation::MovedRoom;

    #[tokio::test]
    async fn properties_can_move_with_remove_without_edit_and_cancellation_keeps_the_original() {
        use super::super::links::fixture::{KEEP, area, maps_where, secret};
        let mut window =
            super::super::test_window(maps_where(&["read", "remove"]).await, area(KEEP));
        let property = PropertyAddress {
            name: "note".into(),
            room_number: Some(RoomNumber(1)),
            room_source: secret(),
        };
        assert!(property_move(&window, secret(), property.clone()).is_some());
        let _ = window.update_move(MoveMessage::PropertyRequested {
            from: secret(),
            to: SourceId::Private,
            property: property.clone(),
        });
        assert!(
            matches!(&window.modal, Some(modals::Modal::ReviewMove { content, .. }) if content.properties == vec![property])
        );
        window.cancel_move_review();
        assert!(!window.moving);
        assert!(window.running_move.is_none());
        assert!(
            window
                .mapper
                .get_current_atlas()
                .get_area(&area(KEEP))
                .is_some()
        );
    }

    #[test]
    fn a_move_conflict_names_both_places_and_says_to_move_again() {
        let conflict = CloudError::RevisionConflict {
            id: smudgy_cloud::Uuid::nil(),
            expected_rev: 1,
            current_rev: 2,
        };
        let rendered = move_error(&conflict, "Bookcase", "Cellar");
        assert!(
            rendered.contains("Bookcase") && rendered.contains("Cellar"),
            "{rendered}"
        );
        assert_ne!(rendered, display_error(&conflict));
        for catalog in smudgy_i18n::available_catalogs() {
            let translator = smudgy_i18n::Translator::for_tag(catalog.tag).unwrap();
            let translated = smudgy_i18n::t!(
                translator,
                "mapper-move-conflict",
                "from" => "Bookcase",
                "to" => "Cellar"
            );
            assert!(
                translated.contains("Bookcase")
                    && translated.contains("Cellar")
                    && !translated.contains('⟦'),
                "{}: {translated}",
                catalog.tag
            );
        }

        let busy = CloudError::StructuralConflict("move_busy".to_string());
        assert_eq!(move_error(&busy, "Map", "Cellar"), display_error(&busy));
    }

    /// Undoing a move that brought rooms into the map takes them out of it
    /// again: when that strands exits leading in, undo asks first, as the
    /// move would have, and keeps the move undoable until it is confirmed.
    #[tokio::test]
    async fn undoing_a_move_out_of_the_map_warns_like_a_fresh_move() {
        use super::super::links::fixture::{KEEP, area, maps, secret};
        let mut window = super::super::test_window(maps().await, area(KEEP));
        window.move_history.undo = Some(MoveRecord {
            area_id: area(KEEP),
            from: secret(),
            to: SourceId::Map,
            content: MovedContent {
                rooms: vec![RoomNumber(9)],
                ..MovedContent::default()
            },
            // The Catacombs lead into the map's room 1.
            landed: vec![RoomNumber(1)],
            preserves_undo: true,
        });
        let _ = window.replay_move(false);
        assert!(window.moving, "the server review is loading");
        assert!(
            matches!(
                &window.modal,
                Some(modals::Modal::ReviewMove { from, reviewed: None, .. })
                    if from.is_map()
            ),
            "undo asks first"
        );
        assert!(window.running_move.is_some());
        window.cancel_move_review();
        assert!(
            window.move_history.undo.is_some(),
            "cancelling restores undo"
        );
        assert!(window.can_undo());
    }

    /// A move that renumbered a room records where it landed, so undo moves it
    /// back from there and redo takes it from wherever undo left it.
    #[test]
    fn undo_and_redo_follow_renumbered_rooms() {
        let secret = SourceId::Secret(smudgy_cloud::Uuid::from_u128(7));
        let rooms = |numbers: &[i32]| numbers.iter().copied().map(RoomNumber).collect::<Vec<_>>();
        let renumbering = |from, to| RoomRenumbering {
            from: RoomNumber(from),
            to: RoomNumber(to),
        };
        let mut record = MoveRecord {
            area_id: AreaId(smudgy_cloud::Uuid::from_u128(1)),
            from: SourceId::Map,
            to: secret,
            content: MovedContent {
                rooms: rooms(&[3, 2]),
                ..MovedContent::default()
            },
            landed: rooms(&[3, 2]),
            preserves_undo: true,
        };

        // The Secret already used 3, so map room 3 became its room 4.
        record.follow(MoveKind::Asked { undoable: true }, &[renumbering(3, 4)]);
        assert_eq!(record.landed, rooms(&[4, 2]));
        assert_eq!(record.landed_content().rooms, rooms(&[4, 2]));
        assert_eq!(record.content.rooms, rooms(&[3, 2]));
        // Undo asks for the Secret's room 4 to be the map's 3 again.
        assert_eq!(
            record.landed_content().moved_rooms(),
            vec![
                MovedRoom {
                    room_number: RoomNumber(4),
                    asks: Some(RoomNumber(3)),
                },
                MovedRoom::plain(RoomNumber(2)),
            ]
        );

        // Back on the map, 3 was free and 2 had been taken meanwhile.
        record.follow(MoveKind::Undo, &[renumbering(4, 3), renumbering(2, 5)]);
        assert_eq!(record.content.rooms, rooms(&[3, 5]));
        // Redo asks for each room's number in the Secret.
        assert_eq!(
            record.content_again().moved_rooms(),
            vec![
                MovedRoom {
                    room_number: RoomNumber(3),
                    asks: Some(RoomNumber(4)),
                },
                MovedRoom {
                    room_number: RoomNumber(5),
                    asks: Some(RoomNumber(2)),
                },
            ]
        );

        record.follow(MoveKind::Redo, &[renumbering(3, 4), renumbering(5, 2)]);
        assert_eq!(record.landed, rooms(&[4, 2]));
    }
}
