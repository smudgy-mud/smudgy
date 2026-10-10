//! The inspector's panels. The pane shows the panel of an atlas chosen alone
//! in the map list; else, with a map open and nothing selected on the
//! canvas, the map's own panel (or the page of a Secret or Private picked as
//! "Add to"); else the selection's view. A selection's view and a place's
//! page sit under a link back to the map's panel. Both panels stack sections
//! that open and close, each headed by its name and a count.

use std::collections::{HashMap, HashSet};

use iced::alignment::Vertical;
use iced::widget::{Column, button, column, container, row, rule, space, text};
use iced::{Length, Padding, Task};
use smudgy_cloud::cloud_api::{FriendView, GrantTreeNode, ShareDirection, ShareGrantRow};
use smudgy_cloud::{AreaId, AtlasId, CloudError, MapStorage, RoomNumber, SourceId, Uuid};
use smudgy_core::models::map_scopes::ScopeDelta;

use crate::components::cloud_errors::display_error;
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::{Event, MapEditorWindow, Message, modals};

// ===========================================================================
// Which panel
// ===========================================================================

/// What the inspector shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Panel {
    /// The panel of the atlas chosen alone in the map list.
    Atlas(AtlasId),
    /// No map is open.
    NoMap,
    /// The open map's panel.
    Map,
    /// The page of the Secret or Private picked as "Add to".
    Place(SourceId),
    /// The view of what is selected on the canvas.
    Selection,
}

/// The panel for the atlas chosen alone in the map list (`atlas`), whether
/// a map is open, whether anything is selected on the canvas, and the place
/// picked as "Add to" (see [`picked_place`]).
#[must_use]
pub fn panel(
    atlas: Option<AtlasId>,
    map_open: bool,
    selected: bool,
    place: Option<SourceId>,
) -> Panel {
    if let Some(atlas_id) = atlas {
        return Panel::Atlas(atlas_id);
    }
    if !map_open {
        return Panel::NoMap;
    }
    if selected {
        return Panel::Selection;
    }
    place.map_or(Panel::Map, Panel::Place)
}

/// The Secret or Private whose page shows on the open map (`open`): the one
/// picked as "Add to" on that map, while new content still goes there
/// (`current`). The map, a place picked on another map, or one the viewer
/// may no longer add to leaves the map's panel showing.
#[must_use]
pub fn picked_place(
    picked: Option<(AreaId, SourceId)>,
    open: Option<AreaId>,
    current: SourceId,
) -> Option<SourceId> {
    let (on, source) = picked?;
    (Some(on) == open && !source.is_map() && source == current).then_some(source)
}

/// The panel the window's state calls for; `map_open` says whether the
/// open map is at hand.
pub(super) fn shown(window: &MapEditorWindow, map_open: bool) -> Panel {
    panel(
        window.multi.selection.folder(),
        map_open,
        !window.editor.selection().is_empty(),
        viewed_place(window.secrets.viewing, window.editor.area_id())
            .filter(|source| still_readable(window, *source))
            .or_else(|| {
                picked_place(
                    window.secrets.add_to,
                    window.editor.area_id(),
                    window.add_to(),
                )
            }),
    )
}

/// Whether the open map still shows Secret `source` to the viewer.
fn still_readable(window: &MapEditorWindow, source: SourceId) -> bool {
    window.editor.area_id().is_some_and(|area_id| {
        window
            .mapper
            .get_current_atlas()
            .get_area(&area_id)
            .is_some_and(|area| {
                area.meta()
                    .sources
                    .iter()
                    .any(|bundle| bundle.source == source)
            })
    })
}

/// The Secret opened to view on the open map (`open`), whatever new content
/// goes to.
#[must_use]
pub fn viewed_place(viewing: Option<(AreaId, SourceId)>, open: Option<AreaId>) -> Option<SourceId> {
    let (on, source) = viewing?;
    (Some(on) == open && source.is_secret()).then_some(source)
}

// ===========================================================================
// State
// ===========================================================================

/// A section of the map's or an atlas's panel. Shares heads both panels
/// and opens or closes in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    /// The map's data fields.
    Data,
    /// The map's Secrets.
    Secrets,
    /// The map's tags, with their rooms in each place.
    Tags,
    /// The map's rooms.
    Rooms,
    /// Who has access to the map or the atlas.
    Shares,
    /// The atlas's maps.
    Maps,
    /// The servers the atlas shows on.
    Servers,
}

/// The panels' state, kept for this window only.
#[derive(Debug, Default)]
pub struct PanelState {
    /// The sections opened or closed in this window from how they start
    /// (see [`Section::starts_open`]).
    toggled: HashSet<Section>,
    /// The Rooms section's filter.
    pub rooms_filter: String,
    /// Who has access to the open map, for its Shares section.
    pub map_access: Option<MapAccess>,
    /// Who has access to the chosen atlas, for its Shares section.
    pub atlas_access: Option<AtlasAccess>,
    /// The chosen atlas's name being typed, while it is renamed.
    pub atlas_rename: Option<(AtlasId, String)>,
    /// The server entries the atlas panel's Servers section lists, read
    /// when an atlas is chosen.
    pub servers: Vec<String>,
}

impl Section {
    /// Whether the section starts open: all do but Tags, a summary for
    /// when it is wanted.
    #[must_use]
    pub fn starts_open(self) -> bool {
        self != Self::Tags
    }
}

impl PanelState {
    #[must_use]
    pub fn is_open(&self, section: Section) -> bool {
        section.starts_open() != self.toggled.contains(&section)
    }

    /// Opens a closed section, or closes an open one.
    pub fn toggle(&mut self, section: Section) {
        if !self.toggled.remove(&section) {
            self.toggled.insert(section);
        }
    }

    /// Opens `section` if it is closed.
    pub fn open(&mut self, section: Section) {
        if !self.is_open(section) {
            self.toggle(section);
        }
    }
}

/// Who has access to a map, as the Share dialog's Who has access lists it.
#[derive(Debug, Clone)]
pub struct MapAccess {
    pub area_id: AreaId,
    /// The grants reaching the map; `None` while loading.
    pub grants: Option<Result<Vec<GrantTreeNode>, String>>,
}

impl MapAccess {
    /// How many have access, once that is known: each grant.
    #[must_use]
    pub fn count(&self) -> Option<usize> {
        let Some(Ok(grants)) = &self.grants else {
            return None;
        };
        Some(grants.len())
    }
}

/// Who has access to one of the viewer's own cloud atlases: the folder
/// shares they gave, named from their friends.
#[derive(Debug, Clone)]
pub struct AtlasAccess {
    pub atlas_id: AtlasId,
    /// The atlas's own grants; `None` while loading.
    pub grants: Option<Result<Vec<ShareGrantRow>, String>>,
    /// Friends' handles by id, once the friends list arrives.
    pub handles: HashMap<Uuid, String>,
}

impl AtlasAccess {
    /// How many the atlas is shared with, once that is known.
    #[must_use]
    pub fn count(&self) -> Option<usize> {
        match &self.grants {
            Some(Ok(grants)) => Some(grants.len()),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum PanelMessage {
    /// A section's header: open or close it.
    SectionToggled(Section),
    /// The back link: from a selection's view or a place's page to the map's
    /// panel.
    Back,
    RoomsFilterChanged(String),
    /// A row of the Tags section: filter the Rooms section by that tag.
    TagPicked(String),
    /// A row of the Rooms section: select and show that room.
    RoomOpened(SourceId, RoomNumber),
    /// Results name what they were asked for: the panel may show another
    /// map or atlas by now.
    MapGrantsLoaded(AreaId, Result<Vec<GrantTreeNode>, CloudError>),
    AtlasGrantsLoaded(AtlasId, Result<Vec<ShareGrantRow>, CloudError>),
    AtlasFriendsLoaded(AtlasId, Result<Vec<FriendView>, CloudError>),
    RenameStarted(AtlasId),
    RenameChanged(String),
    RenameSubmitted,
    RenameCancelled,
    /// The atlas panel's Servers checklist: show the atlas on a server entry
    /// or not.
    ServerToggled {
        atlas_id: AtlasId,
        entry: String,
        show: bool,
    },
}

fn panel_message(message: PanelMessage) -> Message {
    Message::Panel(message)
}

// ===========================================================================
// Update
// ===========================================================================

impl MapEditorWindow {
    pub(super) fn update_panel(&mut self, message: PanelMessage) -> Update<Message, Event> {
        match message {
            PanelMessage::SectionToggled(section) => self.panel.toggle(section),
            PanelMessage::Back => {
                let map_open = self.editor.area_id().is_some_and(|area_id| {
                    self.mapper.get_current_atlas().get_area(&area_id).is_some()
                });
                match shown(self, map_open) {
                    // "Add to" goes back to the map.
                    Panel::Place(_) => self.secrets.leave_place(),
                    Panel::Selection => {
                        self.editor.clear_selection();
                        self.selection_reset();
                    }
                    Panel::Atlas(_) | Panel::NoMap | Panel::Map => {}
                }
                self.inspector.resync(&self.mapper, &self.editor);
            }
            PanelMessage::RoomsFilterChanged(filter) => self.panel.rooms_filter = filter,
            // The filter finds the tag's rooms on every level; selecting them
            // all could reach rooms on levels out of sight.
            PanelMessage::TagPicked(tag) => {
                self.panel.rooms_filter = tag;
                self.panel.open(Section::Rooms);
            }
            PanelMessage::RoomOpened(source, number) => self.open_room(source, number),
            PanelMessage::MapGrantsLoaded(area_id, result) => {
                if let Some(access) = &mut self.panel.map_access
                    && access.area_id == area_id
                {
                    access.grants = Some(result.map_err(|error| display_error(&error)));
                }
            }
            PanelMessage::AtlasGrantsLoaded(atlas_id, result) => {
                if let Some(access) = &mut self.panel.atlas_access
                    && access.atlas_id == atlas_id
                {
                    access.grants = Some(
                        result
                            .map(|rows| {
                                rows.into_iter()
                                    .filter(|row| row.grant.atlas_id == Some(atlas_id))
                                    .collect()
                            })
                            .map_err(|error| display_error(&error)),
                    );
                }
            }
            PanelMessage::AtlasFriendsLoaded(atlas_id, result) => {
                // Without the friends list, a grant names its friend by id.
                if let Some(access) = &mut self.panel.atlas_access
                    && access.atlas_id == atlas_id
                    && let Ok(friends) = result
                {
                    access.handles = modals::friend_handles(&friends);
                }
            }
            PanelMessage::RenameStarted(atlas_id) => {
                let name = self
                    .atlases
                    .iter()
                    .find(|atlas| atlas.id == atlas_id)
                    .map(|atlas| atlas.name.clone())
                    .unwrap_or_default();
                self.panel.atlas_rename = Some((atlas_id, name));
            }
            PanelMessage::RenameChanged(value) => {
                if let Some((_, name)) = &mut self.panel.atlas_rename {
                    *name = value;
                }
            }
            PanelMessage::RenameSubmitted => {
                if self
                    .panel
                    .atlas_rename
                    .as_ref()
                    .is_none_or(|(_, name)| name.trim().is_empty())
                {
                    return Update::none();
                }
                // The rename goes the way the folder's ⋯ menu sends it.
                self.renaming_atlas = self.panel.atlas_rename.take();
                return self.update(Message::RenameAtlasCommitted);
            }
            PanelMessage::RenameCancelled => self.panel.atlas_rename = None,
            PanelMessage::ServerToggled {
                atlas_id,
                entry,
                show,
            } => {
                let delta = ScopeDelta::SetAtlasEntry {
                    atlas_id,
                    entry,
                    show,
                };
                self.map_scopes.apply(&delta);
                return Update::with_event(Event::ScopeAssociationsChanged(vec![delta]));
            }
        }
        Update::none()
    }

    /// An atlas chosen alone in the map list: its panel shows, and the
    /// canvas lets go of its selection, so nothing behind the panel takes
    /// Delete or the arrow keys. The open map stays on the canvas.
    pub(super) fn atlas_chosen(&mut self) -> Update<Message, Event> {
        self.editor.clear_selection();
        self.selection_reset();
        self.context_menu = None;
        self.panel.servers = smudgy_core::models::server::list_servers()
            .map(|servers| servers.into_iter().map(|server| server.name).collect())
            .unwrap_or_default();
        self.inspector.resync(&self.mapper, &self.editor);
        Update::with_task(self.panel_fetches())
    }

    /// Whether `atlas_id` is one of the viewer's own folders (theirs, or one
    /// they administer), not a clan's.
    pub(super) fn own_atlas(&self, atlas_id: AtlasId) -> bool {
        self.atlases
            .iter()
            .any(|atlas| atlas.id == atlas_id && atlas.clan_id.is_none())
    }

    /// Whether `atlas_id` is kept on this device.
    pub(super) fn local_atlas(&self, atlas_id: AtlasId) -> bool {
        self.mapper.local_atlas_ids().contains(&atlas_id)
    }

    /// The open map whose access the map panel lists: a cloud map the viewer
    /// may share, while signed in.
    fn map_access_wanted(&self) -> Option<AreaId> {
        let area_id = self.editor.area_id()?;
        let listed = self.cloud.snapshot.get().signed_in
            && self.mapper.area_storage(&area_id) == MapStorage::Cloud
            && self
                .mapper
                .get_current_atlas()
                .get_area(&area_id)
                .is_some_and(|area| modals::shares_map(&area.effective_access()));
        listed.then_some(area_id)
    }

    /// The chosen atlas whose access the atlas panel lists: one of the
    /// viewer's own cloud folders, while signed in.
    fn atlas_access_wanted(&self) -> Option<AtlasId> {
        let atlas_id = self.multi.selection.folder()?;
        let listed = self.cloud.snapshot.get().signed_in
            && self.own_atlas(atlas_id)
            && !self.local_atlas(atlas_id);
        listed.then_some(atlas_id)
    }

    /// Starts loading who has access to the open map and to the chosen
    /// atlas, where their panels list it and it isn't loaded or on its way.
    pub(super) fn panel_fetches(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        match self.map_access_wanted() {
            None => self.panel.map_access = None,
            Some(area_id)
                if self
                    .panel
                    .map_access
                    .as_ref()
                    .is_some_and(|access| access.area_id == area_id) => {}
            Some(area_id) => {
                let client = self.cloud.client.clone();
                tasks.push(Task::perform(
                    async move { client.area_shares(area_id).await },
                    move |result| panel_message(PanelMessage::MapGrantsLoaded(area_id, result)),
                ));
                self.panel.map_access = Some(MapAccess {
                    area_id,
                    grants: None,
                });
            }
        }
        match self.atlas_access_wanted() {
            None => self.panel.atlas_access = None,
            Some(atlas_id)
                if self
                    .panel
                    .atlas_access
                    .as_ref()
                    .is_some_and(|access| access.atlas_id == atlas_id) => {}
            Some(atlas_id) => {
                let client = self.cloud.client.clone();
                tasks.push(Task::perform(
                    async move { client.shares(ShareDirection::Given).await },
                    move |result| panel_message(PanelMessage::AtlasGrantsLoaded(atlas_id, result)),
                ));
                let client = self.cloud.client.clone();
                tasks.push(Task::perform(
                    async move { client.friends().await },
                    move |result| panel_message(PanelMessage::AtlasFriendsLoaded(atlas_id, result)),
                ));
                self.panel.atlas_access = Some(AtlasAccess {
                    atlas_id,
                    grants: None,
                    handles: HashMap::new(),
                });
            }
        }
        Task::batch(tasks)
    }

    /// Loads both access lists again, as after a share dialog closes.
    pub(super) fn reload_panel_access(&mut self) -> Task<Message> {
        self.panel.map_access = None;
        self.panel.atlas_access = None;
        self.panel_fetches()
    }

    /// Whether a dialog that changes who has access is open: a share
    /// dialog, a clan dialog, or one acting on several items.
    pub(super) fn access_dialog_open(&self) -> bool {
        matches!(
            self.modal,
            Some(
                modals::Modal::Share(_)
                    | modals::Modal::ShareAtlas(_)
                    | modals::Modal::Clan(_)
                    | modals::Modal::ClanMapShare(_)
                    | modals::Modal::PutInClan(_)
                    | modals::Modal::Multi(_)
            )
        )
    }
}

// ===========================================================================
// View pieces
// ===========================================================================

pub(super) fn muted(theme: &crate::Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

/// A panel's head: the kind of thing it is ("MAP"), its name, and a muted
/// line saying where it is kept and how much it holds.
pub(super) fn header<'a>(
    kind: String,
    name: String,
    summary: String,
) -> Column<'a, Message, crate::Theme> {
    column![
        text(kind).size(11).style(muted),
        text(name).size(16),
        text(summary).size(12).style(muted),
    ]
    .spacing(2)
}

/// "In the cloud", "On this device" or "This session only".
pub(super) fn storage_label(storage: MapStorage) -> String {
    match storage {
        MapStorage::Cloud => crate::i18n::t!("mapper-panel-storage-cloud"),
        MapStorage::Local => crate::i18n::t!("mapper-panel-storage-local"),
        MapStorage::Session => crate::i18n::t!("mapper-panel-storage-session"),
    }
}

/// One section: a header that opens and closes it, naming it with a count
/// once that is known, and while open its `body`, built only then.
pub(super) fn section<'a>(
    window: &MapEditorWindow,
    section: Section,
    title: String,
    count: Option<usize>,
    body: impl FnOnce() -> ThemedElement<'a, Message>,
) -> ThemedElement<'a, Message> {
    let open = window.panel.is_open(section);
    // Triangles render in the regular font, as the map list's folders do.
    let disclosure = if open { "\u{25BE}" } else { "\u{25B8}" };
    let mut heading = row![text(disclosure).size(10).style(muted), text(title).size(13),]
        .spacing(6)
        .align_y(Vertical::Center);
    if let Some(count) = count {
        heading = heading.push(text(count.to_string()).size(11).style(muted));
    }
    heading = heading.push(space::horizontal());
    let mut content = column![
        rule::horizontal(1),
        button(heading)
            .style(builtins::button::list_item)
            .width(Length::Fill)
            .padding([4, 4])
            .on_press(panel_message(PanelMessage::SectionToggled(section))),
    ]
    .spacing(4);
    if open {
        content = content.push(container(body()).padding(Padding {
            top: 2.0,
            right: 0.0,
            bottom: 6.0,
            left: 4.0,
        }));
    }
    content.into()
}

/// The link above a selection's view or a place's page: "‹ <map name>",
/// back to the map's panel.
pub(super) fn back_link<'a>(map_name: &str) -> ThemedElement<'a, Message> {
    container(
        button(
            row![
                text("\u{2039}").size(16),
                text(map_name.to_string()).size(13),
            ]
            .spacing(6)
            .align_y(Vertical::Center),
        )
        .style(builtins::button::link)
        .padding([2, 6])
        .on_press(panel_message(PanelMessage::Back)),
    )
    .padding(Padding {
        top: 8.0,
        right: 12.0,
        bottom: 0.0,
        left: 6.0,
    })
    .into()
}

/// A muted line: loading, nothing yet, and the like.
pub(super) fn note<'a>(line: String) -> ThemedElement<'a, Message> {
    text(line).size(12).style(muted).into()
}

/// A person, group or clan with access, and a muted line of what they may
/// do, stepped in `depth` levels as a re-share is.
pub(super) fn access_row<'a>(
    name: String,
    detail: String,
    depth: i32,
) -> ThemedElement<'a, Message> {
    let indent = f32::from(u8::try_from(depth.clamp(0, 12)).unwrap_or(0)) * 12.0;
    container(column![text(name).size(13), text(detail).size(11).style(muted),].spacing(1))
        .padding(Padding::ZERO.left(indent))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas(n: u128) -> AtlasId {
        AtlasId(Uuid::from_u128(n))
    }

    fn area(n: u128) -> AreaId {
        AreaId(Uuid::from_u128(n))
    }

    fn secret(n: u128) -> SourceId {
        SourceId::Secret(Uuid::from_u128(n))
    }

    #[test]
    fn a_chosen_atlas_shows_over_everything() {
        assert_eq!(
            panel(Some(atlas(1)), true, true, Some(secret(2))),
            Panel::Atlas(atlas(1))
        );
        // With no map open too.
        assert_eq!(
            panel(Some(atlas(1)), false, false, None),
            Panel::Atlas(atlas(1))
        );
    }

    #[test]
    fn with_nothing_selected_the_map_shows_its_panel() {
        assert_eq!(panel(None, false, false, None), Panel::NoMap);
        assert_eq!(panel(None, true, false, None), Panel::Map);
        // A selection shows its view, back-linked to the map.
        assert_eq!(panel(None, true, true, None), Panel::Selection);
        assert_eq!(panel(None, true, true, Some(secret(2))), Panel::Selection);
        // A Secret picked as "Add to" shows its page.
        assert_eq!(
            panel(None, true, false, Some(secret(2))),
            Panel::Place(secret(2))
        );
        assert_eq!(
            panel(None, true, false, Some(SourceId::Private)),
            Panel::Place(SourceId::Private)
        );
    }

    #[test]
    fn a_place_page_shows_only_where_it_was_picked_and_still_applies() {
        let open = Some(area(1));
        // Picked on this map, and new content still goes there.
        assert_eq!(
            picked_place(Some((area(1), secret(2))), open, secret(2)),
            Some(secret(2))
        );
        // Nothing picked: the map's panel, even where new content falls
        // back to Private on a map the viewer can't edit.
        assert_eq!(picked_place(None, open, SourceId::Private), None);
        // The map picked back: the map's panel.
        assert_eq!(
            picked_place(Some((area(1), SourceId::Map)), open, SourceId::Map),
            None
        );
        // Picked on another map.
        assert_eq!(
            picked_place(Some((area(9), secret(2))), open, SourceId::Map),
            None
        );
        // Picked, but the viewer may no longer add there.
        assert_eq!(
            picked_place(Some((area(1), secret(2))), open, SourceId::Map),
            None
        );
    }

    #[test]
    fn a_viewed_secret_shows_only_on_its_own_map() {
        let open = Some(area(1));
        assert_eq!(
            viewed_place(Some((area(1), secret(2))), open),
            Some(secret(2))
        );
        assert_eq!(viewed_place(Some((area(3), secret(2))), open), None);
        assert_eq!(viewed_place(Some((area(1), SourceId::Private)), open), None);
        assert_eq!(viewed_place(None, open), None);
    }

    #[test]
    fn sections_start_open_and_toggle_on_their_own() {
        let mut state = PanelState::default();
        assert!(state.is_open(Section::Rooms));
        assert!(state.is_open(Section::Servers));
        state.toggle(Section::Rooms);
        assert!(!state.is_open(Section::Rooms));
        assert!(state.is_open(Section::Shares));
        state.toggle(Section::Rooms);
        assert!(state.is_open(Section::Rooms));
    }

    #[test]
    fn tags_start_closed_and_open_on_request() {
        let mut state = PanelState::default();
        assert!(!state.is_open(Section::Tags));
        state.toggle(Section::Tags);
        assert!(state.is_open(Section::Tags));
        state.toggle(Section::Tags);
        state.open(Section::Tags);
        state.open(Section::Tags);
        assert!(state.is_open(Section::Tags));
        // Opening an open section leaves it open.
        state.open(Section::Rooms);
        assert!(state.is_open(Section::Rooms));
    }

    fn node(n: u128) -> GrantTreeNode {
        GrantTreeNode {
            grant: smudgy_cloud::cloud_api::ShareGrant {
                id: Uuid::from_u128(n),
                owner_id: Uuid::from_u128(1),
                grantor_id: Uuid::from_u128(1),
                grantee_id: Uuid::from_u128(n),
                area_id: Some(area(1)),
                atlas_id: None,
                can_edit: false,
                can_reshare: false,
                can_copy: false,
                can_admin: false,
                parent_grant_id: None,
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
                grantor_nickname: None,
                owner_nickname: None,
                host_hints: None,
            },
            depth: 0,
            grantee_nickname: None,
        }
    }

    #[test]
    fn the_shares_count_waits_for_what_it_counts() {
        let mut access = MapAccess {
            area_id: area(1),
            grants: None,
        };
        assert_eq!(access.count(), None, "grants still loading");
        access.grants = Some(Err("offline".to_string()));
        assert_eq!(access.count(), None, "grants failed to load");
        access.grants = Some(Ok(vec![node(2), node(3)]));
        assert_eq!(access.count(), Some(2));

        let mut folder = AtlasAccess {
            atlas_id: atlas(5),
            grants: None,
            handles: HashMap::new(),
        };
        assert_eq!(folder.count(), None);
        folder.grants = Some(Ok(Vec::new()));
        assert_eq!(folder.count(), Some(0));
    }
}
