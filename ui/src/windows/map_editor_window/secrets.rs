//! A map's Secrets in the editor: the "Add to" picker that decides where
//! new rooms, labels and shapes go, the map panel's Secrets section that
//! lists and creates them, and a Secret's own page with Rename and Delete.
//! A Clan Secret's owner choices and page sections are in
//! [`super::clan_secrets`].

use std::fmt;

use iced::alignment::Vertical;
use iced::widget::{Column, button, column, container, pick_list, row, space, text, text_input};
use iced::{Color, Length, Padding, Task};
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{AreaId, MapStorage, RoomNumber, SourceBundle, SourceId};
use smudgy_map_widget::map_editor::EntityId;
use smudgy_map_widget::sources;

use crate::assets::{bootstrap_icons, fonts};
use crate::components::cloud_errors::display_error;
use crate::components::color_picker::{self, ColorPicker};
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::clan_secrets::{self, OwnerPick, SecretPage};
use super::panels::{self, Section};
use super::place_fields;
use super::{Event, MapEditorWindow, Message};

/// The server's limit on a Secret's name, in characters.
const NAME_LIMIT: usize = 255;

#[derive(Debug, Clone)]
pub enum SecretsMessage {
    /// "Add to" picked a place.
    AddToPicked(Place),
    /// A row of the Secrets section: add to that Secret.
    Opened(SourceId),
    /// A row of a Secret the viewer may not add to: show its page.
    Viewed(SourceId),
    NewStarted,
    /// The owners a new Secret may have on the map they were asked for.
    OwnersLoaded(AreaId, Result<Vec<clan_secrets::ClanOwners>, String>),
    OwnerPicked(clan_secrets::OwnerOption),
    /// A Clan Secret page's access and ownership sections.
    Clan(clan_secrets::PageMessage),
    NameChanged(String),
    NewSubmitted,
    /// Results name the map they ran on: the editor may show another by now.
    /// A new Secret, and whether the viewer may add to it.
    Created(AreaId, Result<(SourceId, bool), String>),
    RenameStarted,
    RenameSubmitted,
    Renamed(AreaId, Result<(), String>),
    DeleteStarted,
    DeleteConfirmed,
    Deleted(AreaId, Result<(), String>),
    /// Cancel any inline create, rename or delete.
    Cancelled,
    /// A row of a place's "Map rooms": select and show that map room.
    RoomOpened(SourceId, RoomNumber),
    /// The Secret page's color swatch: open or close the picker.
    ColorPickerToggled,
    ColorPicked(color_picker::Message),
    /// Back to the palette's color.
    ColorReset,
    Recolored(AreaId, Result<(), String>),
    /// The page's data fields: a field's new value, by name.
    FieldChanged(String, String),
    FieldRemoved(String),
    NewFieldNameChanged(String),
    NewFieldValueChanged(String),
    FieldAdded,
}

/// Where new content goes, and the inline create/rename/delete in progress.
#[derive(Debug, Default)]
pub struct SecretsState {
    /// The name being typed for a new Secret (in the Secrets section) or a
    /// rename (on a Secret's page).
    pub draft: Option<Draft>,
    pub confirming_delete: bool,
    pub busy: bool,
    pub error: Option<String>,
    /// Where new content goes, for the map it was picked on.
    pub add_to: Option<(AreaId, SourceId)>,
    /// The picked place's name, to say so if it goes away.
    pub add_to_name: String,
    /// The selected map room's "Add" menu of places to keep data in.
    pub place_menu_open: bool,
    /// A place picked from that menu, shown before it keeps anything.
    pub place_started: Option<SourceId>,
    /// The open color picker and the Secret it colors.
    pub color_picker: Option<(SourceId, ColorPicker)>,
    /// A Secret whose page shows without new content going there: one the
    /// viewer reads but may not add to.
    pub viewing: Option<(AreaId, SourceId)>,
    /// The New Secret form's owner choices.
    pub owners: OwnerPick,
    /// The open Clan Secret page's access and ownership.
    pub page: Option<SecretPage>,
    /// The name and value of a data field being added on the page.
    pub new_field: (String, String),
}

impl SecretsState {
    /// Leaves a Secret's or Private's page for the map's panel: new content
    /// goes back to the map (or, on a map the viewer can't edit, Private),
    /// and whatever the page had open closes.
    pub fn leave_place(&mut self) {
        self.add_to = None;
        self.add_to_name.clear();
        self.draft = None;
        self.confirming_delete = false;
        self.error = None;
        self.color_picker = None;
        self.viewing = None;
        self.page = None;
        self.new_field = (String::new(), String::new());
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftKind {
    New,
    Rename,
}

#[derive(Debug)]
pub struct Draft {
    pub kind: DraftKind,
    pub name: String,
}

/// A place new content can go: the map, one of its Secrets, or Private.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    pub source: SourceId,
    pub name: String,
}

impl fmt::Display for Place {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

/// The map's readable Secrets in color order (alphabetical, as layers are).
pub fn secrets(area: &AreaCache) -> Vec<&SourceBundle> {
    let mut list: Vec<_> = area
        .meta()
        .sources
        .iter()
        .filter(|bundle| bundle.source.is_secret())
        .collect();
    list.sort_by_cached_key(|bundle| bundle.name.as_deref().unwrap_or_default().to_lowercase());
    list
}

pub(super) fn bundle(area: &AreaCache, source: SourceId) -> Option<&SourceBundle> {
    area.meta()
        .sources
        .iter()
        .find(|bundle| bundle.source == source)
}

/// A place's name as the editor shows it.
pub fn place_name(area: &AreaCache, source: SourceId) -> String {
    match source {
        SourceId::Map => crate::i18n::t!("mapper-place-map"),
        SourceId::Private => crate::i18n::t!("mapper-place-private"),
        SourceId::Secret(_) => bundle(area, source)
            .and_then(|bundle| bundle.name.clone())
            .unwrap_or_default(),
    }
}

/// Whether the viewer holds the map content `action` (`area.add`,
/// `area.edit` or `area.remove_content`): ownership and an editing share
/// give all three; clan authority gives the ones its grants hold.
fn map_can(area: &AreaCache, action: &str) -> bool {
    let access = area.effective_access();
    access.can_edit
        && (access.is_owner
            || area
                .meta()
                .actions
                .as_ref()
                .is_none_or(|actions| actions.contains(action)))
}

/// Whether the viewer may add content to `source` on `area`. Private
/// needs only read access to the map; the server creates it on the first
/// write.
pub fn can_add(area: &AreaCache, source: SourceId) -> bool {
    match source {
        SourceId::Map => map_can(area, "area.add"),
        SourceId::Private => true,
        SourceId::Secret(_) => {
            bundle(area, source).is_some_and(|bundle| bundle.actions.contains("add"))
        }
    }
}

/// Whether the viewer may remove content from `source`, such as one of its
/// tags (the server's removals need `remove` there).
pub fn can_remove(area: &AreaCache, source: SourceId) -> bool {
    match source {
        SourceId::Map => map_can(area, "area.remove_content"),
        SourceId::Private => true,
        SourceId::Secret(_) => {
            bundle(area, source).is_some_and(|bundle| bundle.actions.contains("remove"))
        }
    }
}

/// Whether the viewer holds `action` (`add`, `edit` or `remove`, as
/// [`smudgy_cloud::mutation::AreaMutation::required_action`] names what an
/// operation needs) in `source`: on the map its content action, in a Secret
/// its own action; Private allows every one.
pub fn can(area: &AreaCache, source: SourceId, action: &str) -> bool {
    match source {
        SourceId::Map if action == "copy" => area.effective_access().can_copy,
        SourceId::Map => map_can(
            area,
            match action {
                "add" => "area.add",
                "remove" => "area.remove_content",
                _ => "area.edit",
            },
        ),
        SourceId::Private => true,
        SourceId::Secret(_) => {
            bundle(area, source).is_some_and(|bundle| bundle.actions.contains(action))
        }
    }
}

/// Whether the viewer may change anything in `source`.
pub fn can_write(area: &AreaCache, source: SourceId) -> bool {
    match source {
        SourceId::Map => area.effective_access().can_edit,
        SourceId::Private => true,
        SourceId::Secret(_) => bundle(area, source).is_some_and(|bundle| {
            ["add", "edit", "remove"]
                .iter()
                .any(|action| bundle.actions.contains(*action))
        }),
    }
}

/// A pill naming a place's kind ("Secret" or "Private") in the place's ring
/// color, so editing one reads plainly; nothing for the map.
pub(super) fn place_badge<'a>(
    source: SourceId,
    color: Option<Color>,
) -> ThemedElement<'a, Message> {
    let label = match source {
        SourceId::Map => return space::horizontal().width(0).into(),
        SourceId::Private => crate::i18n::t!("mapper-badge-private"),
        SourceId::Secret(_) => crate::i18n::t!("mapper-badge-secret"),
    };
    container(text(label).size(11))
        .padding(Padding {
            top: 1.0,
            bottom: 1.0,
            left: 7.0,
            right: 7.0,
        })
        .style(move |theme: &crate::Theme| {
            let tint = color.unwrap_or(theme.styles.general.accent);
            container::Style {
                background: Some(Color { a: 0.18, ..tint }.into()),
                border: iced::Border {
                    color: tint,
                    width: 1.0,
                    radius: 8.0.into(),
                },
                text_color: Some(theme.styles.text.normal),
                ..Default::default()
            }
        })
        .into()
}

/// The Secret page's color well: opens and closes the picker.
fn color_swatch<'a>(color: Option<Color>) -> ThemedElement<'a, Message> {
    let well = container(space::horizontal().width(0.0))
        .width(18.0)
        .height(18.0)
        .style(move |theme: &crate::Theme| container::Style {
            background: color.map(Into::into),
            border: iced::border::color(theme.styles.general.border).width(1.0),
            ..Default::default()
        });
    button(well)
        .style(builtins::button::toolbar)
        .padding(2)
        .on_press(Message::Secrets(SecretsMessage::ColorPickerToggled))
        .into()
}

/// A dot in a place's ring color.
pub(super) fn dot<'a>(color: Option<Color>) -> ThemedElement<'a, Message> {
    text("\u{25CF}")
        .size(14)
        .style(move |_theme: &crate::Theme| text::Style { color })
        .into()
}

fn muted(theme: &crate::Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

impl MapEditorWindow {
    /// Whether the active map supports additional content sources. Local
    /// maps support Private additions without requiring a cloud account.
    pub(super) fn secrets_apply(&self) -> bool {
        self.editor
            .area_id()
            .is_some_and(|area_id| match self.mapper.area_storage(&area_id) {
                MapStorage::Local => true,
                MapStorage::Cloud => self.cloud.snapshot.get().signed_in,
                MapStorage::Session => false,
            })
    }

    /// The places new content can go on the active map, in picker order:
    /// the map, each Secret the viewer can add to, Private.
    fn places(&self, area: &AreaCache) -> Vec<Place> {
        let mut places = Vec::new();
        if area.effective_access().can_edit {
            places.push(Place {
                source: SourceId::Map,
                name: place_name(area, SourceId::Map),
            });
        }
        for bundle in secrets(area) {
            if bundle.actions.contains("add") {
                places.push(Place {
                    source: bundle.source,
                    name: bundle.name.clone().unwrap_or_default(),
                });
            }
        }
        places.push(Place {
            source: SourceId::Private,
            name: place_name(area, SourceId::Private),
        });
        places
    }

    /// Where new rooms, labels and shapes go on the active map: the picked
    /// place, else the map, or Private on a map the viewer can't edit. A
    /// picked Secret stays the answer after it goes away or stops taking
    /// additions, until [`Self::notice_gone_place`] lets it go and says so:
    /// a write in between is refused there rather than landing, unseen, on
    /// the map.
    pub(super) fn add_to(&self) -> SourceId {
        let Some(area_id) = self.editor.area_id() else {
            return SourceId::Map;
        };
        if !self.secrets_apply() {
            return SourceId::Map;
        }
        let Some(area) = self.mapper.get_current_atlas().get_area(&area_id) else {
            return SourceId::Map;
        };
        match self.secrets.add_to {
            Some((picked_on, source))
                if picked_on == area_id && (source.is_secret() || can_add(&area, source)) =>
            {
                source
            }
            _ if area.effective_access().can_edit => SourceId::Map,
            _ => SourceId::Private,
        }
    }

    /// When the picked Secret has gone (deleted elsewhere, or access lost),
    /// new content falls back to the map or Private; say so once.
    pub(super) fn notice_gone_place(&mut self) {
        let Some((on, source)) = self.secrets.add_to else {
            return;
        };
        if self.editor.area_id() != Some(on) || !source.is_secret() {
            return;
        }
        let gone = self
            .mapper
            .get_current_atlas()
            .get_area(&on)
            .is_some_and(|area| !can_add(&area, source));
        if gone {
            self.secrets.add_to = None;
            self.inspector.clear_tag_input();
            self.editor_notice = Some((
                std::time::Instant::now(),
                crate::i18n::t!("mapper-place-gone", "name" => self.secrets.add_to_name.clone()),
            ));
        }
    }

    /// The place whose page shows: a Secret opened to view, else where new
    /// content goes.
    pub(super) fn page_source(&self) -> SourceId {
        match self.secrets.viewing {
            Some((on, source)) if self.editor.area_id() == Some(on) => source,
            _ => self.add_to(),
        }
    }

    /// Whether the viewer may add content where new content goes.
    pub(super) fn can_add_here(&self) -> bool {
        let Some(area_id) = self.editor.area_id() else {
            return false;
        };
        self.mapper
            .get_current_atlas()
            .get_area(&area_id)
            .is_some_and(|area| can_add(&area, self.add_to()))
    }

    pub(super) fn update_secrets(&mut self, message: SecretsMessage) -> Update<Message, Event> {
        let message = match message {
            SecretsMessage::Created(on, result) => return self.secret_created(on, result),
            SecretsMessage::Clan(message) => return self.update_clan_page(message),
            SecretsMessage::OwnersLoaded(on, result) => {
                if self.secrets.owners.area == Some(on) {
                    self.secrets.owners.clans = Some(result);
                }
                return Update::none();
            }
            SecretsMessage::OwnerPicked(option) => {
                self.secrets.owners.picked = Some(option.kind);
                return Update::none();
            }
            SecretsMessage::FieldChanged(..)
            | SecretsMessage::FieldRemoved(_)
            | SecretsMessage::FieldAdded => return self.write_field(message),
            SecretsMessage::NewFieldNameChanged(name) => {
                self.secrets.new_field.0 = name;
                return Update::none();
            }
            SecretsMessage::NewFieldValueChanged(value) => {
                self.secrets.new_field.1 = value;
                return Update::none();
            }
            SecretsMessage::Renamed(on, result) => return self.secret_changed(on, false, result),
            SecretsMessage::Deleted(on, result) => return self.secret_changed(on, true, result),
            SecretsMessage::Recolored(on, result) => {
                if let Err(error) = result {
                    if self.editor.area_id() == Some(on) {
                        self.secrets.error = Some(error);
                    } else {
                        self.editor_notice = Some((std::time::Instant::now(), error));
                    }
                }
                return Update::none();
            }
            message => message,
        };
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        let state = &mut self.secrets;
        match message {
            SecretsMessage::AddToPicked(place) => {
                let source = place.source;
                state.add_to = Some((area_id, source));
                state.add_to_name = place.name;
                state.draft = None;
                state.confirming_delete = false;
                state.error = None;
                state.viewing = None;
                // Selected map rooms stay selected, so their tags can go
                // into the place just picked; the tag input starts over.
                self.inspector.clear_tag_input();
                if self.editor.selection().rooms().next().is_none() {
                    self.editor.clear_selection();
                }
                let task = self.open_secret_page(area_id, source);
                self.inspector.resync(&self.mapper, &self.editor);
                return Update::with_task(task);
            }
            SecretsMessage::Viewed(source) => {
                state.viewing = Some((area_id, source));
                state.draft = None;
                state.confirming_delete = false;
                state.error = None;
                state.color_picker = None;
                self.editor.clear_selection();
                let task = self.open_secret_page(area_id, source);
                self.inspector.resync(&self.mapper, &self.editor);
                return Update::with_task(task);
            }
            SecretsMessage::Opened(source) => {
                state.add_to = Some((area_id, source));
                state.add_to_name = self
                    .mapper
                    .get_current_atlas()
                    .get_area(&area_id)
                    .map(|area| place_name(&area, source))
                    .unwrap_or_default();
                let state = &mut self.secrets;
                state.draft = None;
                state.confirming_delete = false;
                state.error = None;
                state.viewing = None;
                self.editor.clear_selection();
                let task = self.open_secret_page(area_id, source);
                self.inspector.resync(&self.mapper, &self.editor);
                return Update::with_task(task);
            }
            SecretsMessage::NewStarted => {
                state.draft = Some(Draft {
                    kind: DraftKind::New,
                    name: String::new(),
                });
                state.error = None;
                let atlas = self.mapper.get_current_atlas();
                let task = atlas
                    .get_area(&area_id)
                    .map_or_else(Task::none, |area| self.start_owner_pick(&area));
                return Update::with_task(task);
            }
            SecretsMessage::OwnersLoaded(..)
            | SecretsMessage::OwnerPicked(_)
            | SecretsMessage::Clan(_)
            | SecretsMessage::FieldChanged(..)
            | SecretsMessage::FieldRemoved(_)
            | SecretsMessage::NewFieldNameChanged(_)
            | SecretsMessage::NewFieldValueChanged(_)
            | SecretsMessage::FieldAdded => {}
            SecretsMessage::RenameStarted => {
                let atlas = self.mapper.get_current_atlas();
                let name = atlas
                    .get_area(&area_id)
                    .map(|area| place_name(&area, self.page_source()))
                    .unwrap_or_default();
                let state = &mut self.secrets;
                state.draft = Some(Draft {
                    kind: DraftKind::Rename,
                    name,
                });
                state.confirming_delete = false;
                state.error = None;
            }
            SecretsMessage::NameChanged(name) => {
                if let Some(draft) = &mut state.draft {
                    draft.name = name.chars().take(NAME_LIMIT).collect();
                }
                state.error = None;
            }
            SecretsMessage::NewSubmitted | SecretsMessage::RenameSubmitted => {
                let source = self.page_source();
                let Some(name) = self.draft_name_if_valid() else {
                    return Update::none();
                };
                let owner = if matches!(message, SecretsMessage::NewSubmitted) {
                    // Choices asked for another map are asked again for this one.
                    if self.secrets.owners.area != Some(area_id) {
                        let atlas = self.mapper.get_current_atlas();
                        let task = atlas
                            .get_area(&area_id)
                            .map_or_else(Task::none, |area| self.start_owner_pick(&area));
                        return Update::with_task(task);
                    }
                    let Some(owner) = self.secrets.owners.new_owner() else {
                        return Update::none();
                    };
                    owner
                } else {
                    smudgy_cloud::clan_secrets::NewSecretOwner::Me
                };
                self.secrets.busy = true;
                let mapper = self.mapper.clone();
                return if matches!(message, SecretsMessage::NewSubmitted) {
                    let secret = smudgy_cloud::clan_secrets::NewSecret {
                        name,
                        color: None,
                        owner,
                    };
                    Update::with_task(Task::perform(
                        async move {
                            mapper
                                .create_secret_as(area_id, &secret)
                                .await
                                .map(|summary| (summary.source, summary.actions.contains("add")))
                                .map_err(|error| display_error(&error))
                        },
                        move |result| Message::Secrets(SecretsMessage::Created(area_id, result)),
                    ))
                } else {
                    Update::with_task(Task::perform(
                        async move {
                            mapper
                                .rename_secret(area_id, &source, &name)
                                .await
                                .map(|_| ())
                                .map_err(|error| display_error(&error))
                        },
                        move |result| Message::Secrets(SecretsMessage::Renamed(area_id, result)),
                    ))
                };
            }
            SecretsMessage::Created(..)
            | SecretsMessage::Renamed(..)
            | SecretsMessage::Deleted(..)
            | SecretsMessage::Recolored(..) => {}
            SecretsMessage::DeleteStarted => {
                state.confirming_delete = true;
                state.draft = None;
                state.error = None;
            }
            SecretsMessage::DeleteConfirmed => {
                let source = self.page_source();
                if !source.is_secret() {
                    return Update::none();
                }
                self.secrets.busy = true;
                let mapper = self.mapper.clone();
                return Update::with_task(Task::perform(
                    async move {
                        mapper
                            .delete_secret(area_id, &source)
                            .await
                            .map_err(|error| display_error(&error))
                    },
                    move |result| Message::Secrets(SecretsMessage::Deleted(area_id, result)),
                ));
            }
            SecretsMessage::Cancelled => {
                state.draft = None;
                state.confirming_delete = false;
                state.error = None;
            }
            SecretsMessage::ColorPickerToggled => {
                let source = self.page_source();
                let open = self
                    .secrets
                    .color_picker
                    .as_ref()
                    .is_some_and(|(open, _)| *open == source);
                let color = self
                    .mapper
                    .get_current_atlas()
                    .get_area(&area_id)
                    .and_then(|area| sources::source_color(&area, source))
                    .unwrap_or(Color::from_rgb8(128, 128, 128));
                self.secrets.error = None;
                self.secrets.color_picker =
                    (!open).then(|| (source, ColorPicker::from_color(color)));
            }
            SecretsMessage::ColorPicked(message) => {
                let Some((source, picker)) = &mut state.color_picker else {
                    return Update::none();
                };
                let source = *source;
                // Mid-drag the swatch previews; the color writes on release.
                if let color_picker::Event::Committed(color) = picker.update(message) {
                    return self.recolor(area_id, source, Some(color_picker::to_hex(color)));
                }
            }
            SecretsMessage::ColorReset => {
                let source = self.page_source();
                self.secrets.color_picker = None;
                return self.recolor(area_id, source, None);
            }
            SecretsMessage::RoomOpened(source, room_number) => {
                let atlas = self.mapper.get_current_atlas();
                if let Some(room) = atlas.get_area(&area_id).and_then(|area| {
                    if source.is_map() {
                        area.get_room(&room_number).cloned()
                    } else {
                        sources::source_room(&area, source, room_number).cloned()
                    }
                }) {
                    self.editor.select(if source.is_map() {
                        EntityId::Room(room_number)
                    } else {
                        EntityId::SourceRoom(source, room_number)
                    });
                    self.selection_reset();
                    self.editor.center_on(
                        iced::Point::new(room.get_x(), room.get_y()),
                        room.get_level(),
                    );
                }
            }
        }
        self.inspector.resync(&self.mapper, &self.editor);
        Update::none()
    }

    /// Writes one of the shown page's data fields: a changed value, a field
    /// taken away, or the one being added.
    fn write_field(&mut self, message: SecretsMessage) -> Update<Message, Event> {
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        let Some(area) = self.mapper.get_current_atlas().get_area(&area_id) else {
            return Update::none();
        };
        let source = self.page_source();
        let command = match message {
            SecretsMessage::FieldChanged(name, value) => {
                place_fields::set_field(&area, source, name, value)
            }
            SecretsMessage::FieldRemoved(name) => place_fields::delete_field(&area, source, name),
            SecretsMessage::FieldAdded => {
                let name = self.secrets.new_field.0.trim().to_string();
                if name.is_empty() {
                    return Update::none();
                }
                let value = std::mem::take(&mut self.secrets.new_field).1;
                place_fields::set_field(&area, source, name, value)
            }
            _ => None,
        };
        self.push_command(command)
    }

    /// A new Secret on map `on`: when that map is still shown, new content
    /// goes into it.
    fn secret_created(
        &mut self,
        on: AreaId,
        result: Result<(SourceId, bool), String>,
    ) -> Update<Message, Event> {
        let here = self.editor.area_id() == Some(on);
        let state = &mut self.secrets;
        state.busy = false;
        match result {
            // New content goes into a new Secret the viewer may add to; one
            // they only read opens to view.
            Ok((source, addable)) => {
                let name = state
                    .draft
                    .take()
                    .map(|draft| draft.name.trim().to_string());
                if addable {
                    if let Some(name) = name {
                        state.add_to_name = name;
                    }
                    state.add_to = Some((on, source));
                    state.viewing = None;
                } else {
                    state.viewing = Some((on, source));
                }
                if here {
                    self.editor.clear_selection();
                    let task = self.open_secret_page(on, source);
                    self.inspector.resync(&self.mapper, &self.editor);
                    return Update::with_task(task);
                }
            }
            Err(error) if here => state.error = Some(error),
            Err(error) => self.editor_notice = Some((std::time::Instant::now(), error)),
        }
        self.inspector.resync(&self.mapper, &self.editor);
        Update::none()
    }

    /// A rename or delete on map `on` finished.
    /// Writes Secret `source`'s color, or with `None` gives it back to the
    /// palette.
    fn recolor(
        &mut self,
        area_id: AreaId,
        source: SourceId,
        color: Option<String>,
    ) -> Update<Message, Event> {
        if !source.is_secret() {
            return Update::none();
        }
        self.secrets.error = None;
        let mapper = self.mapper.clone();
        Update::with_task(Task::perform(
            async move {
                mapper
                    .recolor_secret(area_id, &source, color.as_deref())
                    .await
                    .map(|_| ())
                    .map_err(|error| display_error(&error))
            },
            move |result| Message::Secrets(SecretsMessage::Recolored(area_id, result)),
        ))
    }

    fn secret_changed(
        &mut self,
        on: AreaId,
        deleted: bool,
        result: Result<(), String>,
    ) -> Update<Message, Event> {
        let here = self.editor.area_id() == Some(on);
        let state = &mut self.secrets;
        state.busy = false;
        match result {
            Ok(()) => {
                state.draft = None;
                state.confirming_delete = false;
                // Older edits may write into the deleted Secret, so they
                // leave the history with it; history is per map.
                if deleted && here {
                    self.clear_history();
                    self.secrets.add_to = None;
                    self.secrets.viewing = None;
                    self.secrets.page = None;
                }
            }
            Err(error) if here => state.error = Some(error),
            Err(error) => self.editor_notice = Some((std::time::Instant::now(), error)),
        }
        self.inspector.resync(&self.mapper, &self.editor);
        Update::none()
    }

    /// The draft's trimmed name when it can be submitted, else `None` with
    /// the reason shown under the input.
    fn draft_name_if_valid(&mut self) -> Option<String> {
        let area_id = self.editor.area_id()?;
        let area = self.mapper.get_current_atlas().get_area(&area_id)?;
        let draft = self.secrets.draft.as_ref()?;
        let name = draft.name.trim().to_string();
        if name.is_empty() || self.secrets.busy {
            return None;
        }
        let current = (draft.kind == DraftKind::Rename).then(|| self.page_source());
        let taken = secrets(&area).iter().any(|bundle| {
            Some(bundle.source) != current
                && bundle
                    .name
                    .as_deref()
                    .is_some_and(|other| other.to_lowercase() == name.to_lowercase())
        });
        if taken {
            self.secrets.error = Some(crate::i18n::t!("editor-name-in-use"));
            return None;
        }
        Some(name)
    }
}

/// The toolbar's "Add to" picker; `None` where there is only the map.
pub fn add_to_picker(window: &MapEditorWindow) -> Option<ThemedElement<'_, Message>> {
    if !window.secrets_apply() {
        return None;
    }
    let area = window
        .mapper
        .get_current_atlas()
        .get_area(&window.editor.area_id()?)?;
    let current = window.add_to();
    let places = window.places(&area);
    let selected = places.iter().find(|place| place.source == current).cloned();
    let color = sources::source_color(&area, current);
    let mut picker = row![
        text(crate::i18n::t!("mapper-add-to"))
            .size(13.0)
            .style(muted),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);
    if color.is_some() {
        picker = picker.push(dot(color));
    }
    let badge = (!current.is_map()).then(|| place_badge(current, color));
    picker = picker.push(
        pick_list(places, selected, |place| {
            Message::Secrets(SecretsMessage::AddToPicked(place))
        })
        .text_size(13.0)
        .padding(Padding {
            top: 3.0,
            bottom: 3.0,
            left: 8.0,
            right: 6.0,
        }),
    );
    if let Some(badge) = badge {
        picker = picker.push(badge);
    }
    Some(picker.into())
}

/// The name input with its submit and cancel buttons and any error.
fn name_form<'a>(
    window: &'a MapEditorWindow,
    draft: &'a Draft,
    submit_label: String,
    submit: SecretsMessage,
) -> ThemedElement<'a, Message> {
    let state = &window.secrets;
    let ready = !draft.name.trim().is_empty() && !state.busy;
    let mut input = text_input(
        crate::i18n::ts!("mapper-secret-name-placeholder"),
        &draft.name,
    )
    .size(13)
    .padding([5, 8]);
    if !state.busy {
        input = input
            .on_input(|name| Message::Secrets(SecretsMessage::NameChanged(name)))
            .on_submit(Message::Secrets(submit.clone()));
    }
    let mut form = column![input].spacing(6);
    if draft.kind == DraftKind::New
        && let Some(owner) = clan_secrets::owner_line(window)
    {
        form = form.push(owner);
    }
    let ready = ready
        && (draft.kind == DraftKind::Rename
            || (window.secrets.owners.area == window.editor.area_id()
                && window.secrets.owners.new_owner().is_some()));
    let mut form = form.push(
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(12))
                .style(builtins::button::secondary)
                .on_press_maybe(
                    (!state.busy).then_some(Message::Secrets(SecretsMessage::Cancelled))
                ),
            button(text(submit_label).size(12))
                .style(builtins::button::primary)
                .on_press_maybe(ready.then_some(Message::Secrets(submit))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );
    if let Some(error) = &state.error {
        form = form.push(text(error).size(12).style(builtins::text::danger));
    }
    form.into()
}

/// Whether the map panel has a Secrets section: on a cloud map while signed
/// in (`applies`), for whoever can make Secrets there (`creates`: the owner
/// of a user's map, or a clan member with a creation action), or when the
/// viewer can read one.
#[must_use]
pub fn section_applies(applies: bool, creates: bool, secrets: usize) -> bool {
    applies && (creates || secrets > 0)
}

/// The Secrets section of the map panel: the map's Secrets, each opening
/// its page, and New Secret for whoever can make one there.
pub fn section<'a>(
    window: &'a MapEditorWindow,
    area: &AreaCache,
) -> Column<'a, Message, crate::Theme> {
    let state = &window.secrets;
    let mut rows = Column::new().spacing(2);
    for bundle in secrets(area) {
        let source = bundle.source;
        let addable = bundle.actions.contains("add");
        let mut line = row![
            dot(sources::layer_color(area, source)),
            text(bundle.name.clone().unwrap_or_default())
                .size(13)
                .width(Length::Fill),
            text(crate::i18n::t!("mapper-room-count", "count" => bundle.rooms.len()))
                .size(11)
                .style(muted),
        ]
        .spacing(8)
        .align_y(Vertical::Center);
        line = line.push(if addable {
            text("\u{203A}").size(14).style(muted)
        } else {
            text(crate::i18n::t!("mapper-view-only"))
                .size(11)
                .style(muted)
        });
        rows = rows.push(
            button(line)
                .style(builtins::button::list_item)
                .width(Length::Fill)
                .padding([6, 8])
                .on_press(Message::Secrets(if addable {
                    SecretsMessage::Opened(source)
                } else {
                    SecretsMessage::Viewed(source)
                })),
        );
    }
    let content = Column::new().spacing(8).push(rows);

    if !clan_secrets::can_create(area) {
        return content;
    }
    if let Some(draft) = state
        .draft
        .as_ref()
        .filter(|draft| draft.kind == DraftKind::New)
    {
        return content.push(name_form(
            window,
            draft,
            crate::i18n::t!("action-create"),
            SecretsMessage::NewSubmitted,
        ));
    }
    content.push(
        button(
            row![
                text(bootstrap_icons::PLUS_LG)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(10.0)
                    .style(muted),
                text(crate::i18n::t!("mapper-new-secret")).size(11.0),
            ]
            .spacing(6.0)
            .align_y(Vertical::Center),
        )
        .style(builtins::button::subtle)
        .padding(Padding {
            top: 4.0,
            bottom: 4.0,
            left: 9.0,
            right: 9.0,
        })
        .on_press(Message::Secrets(SecretsMessage::NewStarted)),
    )
}

/// A Secret's or Private's own page, shown while it is picked as "Add to"
/// (or, for a Secret the viewer only reads, opened from its row) and
/// nothing is selected, under a link back to the map's panel. A Clan
/// Secret's page adds who has access and its ownership.
pub fn place_page<'a>(
    window: &'a MapEditorWindow,
    area: &AreaCache,
    source: SourceId,
) -> Column<'a, Message, crate::Theme> {
    place_page_core(window, area, source).push(clan_secrets::sections(window, area, source))
}

fn place_page_core<'a>(
    window: &'a MapEditorWindow,
    area: &AreaCache,
    source: SourceId,
) -> Column<'a, Message, crate::Theme> {
    let state = &window.secrets;
    let viewing = state.viewing.is_some_and(|(_, viewed)| viewed == source);
    let name = place_name(area, source);
    let rooms = bundle(area, source).map_or(0, |bundle| bundle.rooms.len());
    let picker = state
        .color_picker
        .as_ref()
        .filter(|(open, _)| *open == source)
        .map(|(_, picker)| picker);
    let color = picker
        .map(ColorPicker::color)
        .or_else(|| sources::source_color(area, source));
    let mut content = Column::new().spacing(10).padding(12).push(
        text(if viewing {
            crate::i18n::t!("mapper-now-viewing")
        } else {
            crate::i18n::t!("mapper-now-editing")
        })
        .size(12)
        .style(muted),
    );
    content = content.push(
        row![
            dot(color),
            text(name.clone()).size(16),
            place_badge(source, color)
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );
    let summary = if source.is_secret() {
        crate::i18n::t!("mapper-room-count", "count" => rooms)
    } else {
        crate::i18n::t!("mapper-private-help")
    };
    content = content.push(text(summary).size(12).style(muted));
    if source.is_secret() {
        content = content.push(clan_secrets::page_header(window, area, source));
    }

    // Whoever may rename a Secret chooses its color: the map's owner on an
    // owner Secret, and whoever holds `rename` on any Secret.
    let owner_secret =
        bundle(area, source).is_some_and(|bundle| bundle.clan_id.is_none()) && area.is_owned();
    let may_rename = source.is_secret()
        && (owner_secret || bundle(area, source).is_some_and(|bundle| bundle.can("rename")));
    let may_delete = source.is_secret()
        && (owner_secret || bundle(area, source).is_some_and(|bundle| bundle.can("delete")));
    if may_rename {
        let chosen = bundle(area, source).is_some_and(|bundle| bundle.color.is_some());
        let mut line = row![
            text(crate::i18n::t!("mapper-secret-color")).size(13),
            color_swatch(color),
        ]
        .spacing(8)
        .align_y(Vertical::Center);
        if chosen {
            line = line.push(
                button(text(crate::i18n::t!("mapper-secret-color-automatic")).size(12))
                    .style(builtins::button::subtle)
                    .on_press(Message::Secrets(SecretsMessage::ColorReset)),
            );
        }
        content = content.push(line);
        if let Some(picker) = picker {
            content = content.push(
                picker
                    .view()
                    .map(|message| Message::Secrets(SecretsMessage::ColorPicked(message))),
            );
        }
        if let Some(error) = state
            .error
            .as_ref()
            .filter(|_| state.draft.is_none() && !state.confirming_delete)
        {
            content = content.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
    }

    if place_fields::section_applies(area, source) {
        content = content.push(panels::section(
            window,
            Section::Data,
            crate::i18n::t!("mapper-panel-data-fields"),
            Some(place_fields::fields(area, source).len()),
            || place_fields::view(window, area, source),
        ));
    }

    // The map rooms this place keeps something for.
    let kept: Vec<(SourceId, RoomNumber)> = bundle(area, source)
        .map(|bundle| {
            bundle
                .room_data
                .iter()
                .filter(|data| {
                    !data.properties.is_empty() || !data.tags.is_empty() || !data.exits.is_empty()
                })
                .map(|data| (data.room_source.unwrap_or(SourceId::Map), data.room_number))
                .collect()
        })
        .unwrap_or_default();
    if !kept.is_empty() {
        let mut rooms = Column::new().spacing(2).push(
            text(crate::i18n::t!("mapper-attached-rooms"))
                .size(12)
                .style(muted),
        );
        for (anchor, room_number) in kept {
            let room = if anchor.is_map() {
                area.get_room(&room_number)
            } else {
                sources::source_room(area, anchor, room_number)
            };
            let Some(room) = room else {
                continue;
            };
            let title = room.get_title();
            let source_name = place_name(area, anchor);
            rooms = rooms.push(
                button(text(format!("{source_name} · #{room_number} {title}")).size(13))
                    .style(builtins::button::list_item)
                    .width(Length::Fill)
                    .padding([4, 8])
                    .on_press(Message::Secrets(SecretsMessage::RoomOpened(
                        anchor,
                        room_number,
                    ))),
            );
        }
        content = content.push(rooms);
    }

    // Renaming and deleting follow the same authority.
    if !may_rename && !may_delete {
        return content;
    }
    if let Some(draft) = state
        .draft
        .as_ref()
        .filter(|draft| draft.kind == DraftKind::Rename)
    {
        return content.push(name_form(
            window,
            draft,
            crate::i18n::t!("package-save-name"),
            SecretsMessage::RenameSubmitted,
        ));
    }
    if state.confirming_delete {
        let mut confirm = column![
            text(crate::i18n::t!("mapper-delete-secret-question", "name" => name)).size(13),
            row![
                space::horizontal(),
                button(text(crate::i18n::t!("action-cancel")).size(12))
                    .style(builtins::button::secondary)
                    .on_press_maybe(
                        (!state.busy).then_some(Message::Secrets(SecretsMessage::Cancelled))
                    ),
                button(text(crate::i18n::t!("action-delete")).size(12))
                    .style(builtins::button::primary)
                    .on_press_maybe(
                        (!state.busy).then_some(Message::Secrets(SecretsMessage::DeleteConfirmed))
                    ),
            ]
            .spacing(8)
            .align_y(Vertical::Center),
        ]
        .spacing(8);
        if let Some(error) = &state.error {
            confirm = confirm.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        return content.push(
            container(confirm)
                .padding(8)
                .style(builtins::container::card),
        );
    }
    let mut actions = row![].spacing(8);
    if may_rename {
        actions = actions.push(
            button(text(crate::i18n::t!("mapper-menu-rename")).size(12))
                .style(builtins::button::subtle)
                .on_press(Message::Secrets(SecretsMessage::RenameStarted)),
        );
    }
    if may_delete {
        actions = actions.push(
            button(text(crate::i18n::t!("mapper-delete-secret")).size(12))
                .style(builtins::button::secondary)
                .on_press(Message::Secrets(SecretsMessage::DeleteStarted)),
        );
    }
    content.push(actions)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    #[tokio::test]
    async fn local_private_picker_works_without_a_cloud_account() {
        use smudgy_cloud::{CreateAreaRequest, LocalBackend, Mapper, MapperBackend};
        let root = std::env::temp_dir().join(format!("smudgy-local-private-ui-{}", Uuid::new_v4()));
        let backend = std::sync::Arc::new(LocalBackend::new(root.join("local")));
        let area = backend
            .create_area(CreateAreaRequest {
                name: "Local".into(),
                atlas_id: None,
                clan_id: None,
                ownership: None,
                ephemeral: false,
                properties: Default::default(),
            })
            .await
            .unwrap();
        let mapper = Mapper::new(backend, root.join("cache"));
        mapper.load_all_areas().await.unwrap();
        let mut window = super::super::test_window(mapper, area.id);
        window.cloud = crate::cloud_account::test_handles();
        assert!(!window.cloud.snapshot.get().signed_in);
        assert!(window.secrets_apply());
        assert!(add_to_picker(&window).is_some());
        window.secrets.add_to = Some((area.id, SourceId::Private));
        assert_eq!(window.add_to(), SourceId::Private);
        let area = window
            .mapper
            .get_current_atlas()
            .get_area(&area.id)
            .unwrap();
        assert!(
            window.move_targets(&area, SourceId::Private).is_empty(),
            "cloud-only source moves are not offered"
        );
        drop(window);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn leaving_a_place_returns_new_content_to_the_map() {
        let secret = SourceId::Secret(Uuid::from_u128(7));
        let mut state = SecretsState {
            add_to: Some((AreaId(Uuid::from_u128(1)), secret)),
            add_to_name: "Bookcase".to_string(),
            draft: Some(Draft {
                kind: DraftKind::Rename,
                name: "Shelf".to_string(),
            }),
            confirming_delete: true,
            error: Some("taken".to_string()),
            ..SecretsState::default()
        };
        state.leave_place();
        assert_eq!(state.add_to, None);
        assert!(state.add_to_name.is_empty());
        assert!(state.draft.is_none());
        assert!(!state.confirming_delete);
        assert!(state.error.is_none());
        assert!(state.color_picker.is_none());
    }

    #[test]
    fn the_secrets_section_shows_for_a_creator_or_a_readable_secret() {
        // A local map, or signed out: never.
        assert!(!section_applies(false, true, 3));
        // Whoever makes Secrets sees the section with none yet.
        assert!(section_applies(true, true, 0));
        // Someone else sees it only when they can read one.
        assert!(!section_applies(true, false, 0));
        assert!(section_applies(true, false, 1));
    }
}
