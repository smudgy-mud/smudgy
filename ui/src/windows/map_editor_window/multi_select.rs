//! Maps and folders chosen in the map list. A plain click on a folder chooses
//! that folder alone, and the inspector shows its panel. Ctrl+click adds or
//! removes a row, Shift+click takes the rows between the last one pressed and
//! this one, and while more than one is chosen the canvas pane lists what can
//! be done to all of them: move, share, transfer, servers, delete. Each
//! action applies to the items it can and names the rest.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashSet};

use crate::widgets::dialog::scrolling_body;

use iced::alignment::Vertical;
use iced::event::Event as IcedEvent;
use iced::widget::{
    Column, button, checkbox, column, container, radio, row, rule, scrollable, space, text,
    text_input,
};
use iced::{Length, Padding, Task, keyboard, window};
use smudgy_cloud::access_review::ReviewedFiling;
use smudgy_cloud::cloud_api::{CreateShareRequest, FriendView, ShareScope, TransferRecipient};
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::{
    AreaId, AtlasId, CloudError, MapDestination, MapStorage, Mapper, RelocationMode, Uuid,
};
use smudgy_core::models::map_scopes::{MapScopes, ScopeDelta};

use crate::components::cloud_errors::display_error;
use crate::theme::Element as ThemedElement;
use crate::theme::builtins;
use crate::update::Update;

use super::folder_picker::{self, FolderPicker, NewFolderForm, NewFolderMessage, PickerMessage};
use super::{Event, FolderKey, MapEditorWindow, Message, ScopeTarget, modals};

// ===========================================================================
// The selection
// ===========================================================================

/// One row of the map list that can be chosen: a map, or a folder's header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListItem {
    Map(AreaId),
    Folder(AtlasId),
}

/// What a press in the list asks of the window once the selection is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pressed {
    /// Show this map alone: a plain click, or a selection down to one map.
    Open(AreaId),
    /// This folder alone is chosen: a plain click, or a selection down to
    /// one folder.
    Folder(AtlasId),
    Nothing,
}

/// The chosen rows of the map list.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Chosen rows, in the order they were chosen. Empty means the open map
    /// alone, which is how the list starts and where a plain click returns.
    items: Vec<ListItem>,
    /// Where a Shift+click range starts: the last row pressed without Shift.
    anchor: Option<ListItem>,
}

impl Selection {
    /// The chosen rows, the open map standing in when none were chosen.
    #[must_use]
    pub fn items(&self, open: Option<AreaId>) -> Vec<ListItem> {
        if self.items.is_empty() {
            open.map(ListItem::Map).into_iter().collect()
        } else {
            self.items.clone()
        }
    }

    /// Whether `item` shows as selected in the list.
    #[must_use]
    pub fn contains(&self, item: ListItem, open: Option<AreaId>) -> bool {
        if self.items.is_empty() {
            open.map(ListItem::Map) == Some(item)
        } else {
            self.items.contains(&item)
        }
    }

    /// More than one row is chosen: the canvas pane lists actions.
    #[must_use]
    pub fn is_multi(&self) -> bool {
        self.items.len() > 1
    }

    /// Rows are chosen apart from the open map, so Esc has something to
    /// clear.
    #[must_use]
    pub fn is_chosen(&self) -> bool {
        !self.items.is_empty()
    }

    /// Back to the open map alone.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// The folder chosen alone, whose panel the inspector shows.
    #[must_use]
    pub fn folder(&self) -> Option<AtlasId> {
        match self.items.as_slice() {
            [ListItem::Folder(atlas_id)] => Some(*atlas_id),
            _ => None,
        }
    }

    /// Lets go of a folder chosen alone, as a selection on the canvas does;
    /// several chosen rows stay. Whether one was let go.
    pub fn drop_folder(&mut self) -> bool {
        let chosen = self.folder().is_some();
        if chosen {
            self.items.clear();
        }
        chosen
    }

    /// A press on `item` with `modifiers` held. `order` is the list's
    /// selectable rows as drawn, top to bottom.
    pub fn press(
        &mut self,
        item: ListItem,
        modifiers: keyboard::Modifiers,
        open: Option<AreaId>,
        order: &[ListItem],
    ) -> Pressed {
        if modifiers.shift() {
            let range = self
                .anchor
                .or(open.map(ListItem::Map))
                .and_then(|anchor| range(order, anchor, item))
                .unwrap_or_else(|| vec![item]);
            let mut items = if modifiers.command() {
                self.items(open)
            } else {
                Vec::new()
            };
            for item in range {
                if !items.contains(&item) {
                    items.push(item);
                }
            }
            self.items = items;
            return self.settle(open);
        }
        if modifiers.command() {
            let mut items = self.items(open);
            if let Some(index) = items.iter().position(|chosen| *chosen == item) {
                items.remove(index);
            } else {
                items.push(item);
            }
            self.items = items;
            self.anchor = Some(item);
            return self.settle(open);
        }
        self.anchor = Some(item);
        match item {
            ListItem::Map(area_id) => {
                self.items.clear();
                Pressed::Open(area_id)
            }
            ListItem::Folder(atlas_id) => {
                self.items = vec![item];
                Pressed::Folder(atlas_id)
            }
        }
    }

    /// Takes `item` out of the selection (the × in the pane).
    pub fn remove(&mut self, item: ListItem, open: Option<AreaId>) -> Pressed {
        self.items.retain(|chosen| *chosen != item);
        self.settle(open)
    }

    /// Keeps only the rows `keep` accepts, as after a delete.
    pub fn retain(&mut self, keep: impl Fn(ListItem) -> bool, open: Option<AreaId>) -> Pressed {
        let before = self.items.len();
        self.items.retain(|item| keep(*item));
        if self.anchor.is_some_and(|anchor| !keep(anchor)) {
            self.anchor = None;
        }
        if self.items.len() == before {
            return Pressed::Nothing;
        }
        self.settle(open)
    }

    /// A map moved to another storage tier comes back under a new id.
    pub fn replace_map(&mut self, from: AreaId, to: AreaId) {
        for item in self
            .items
            .iter_mut()
            .chain(self.anchor.iter_mut())
            .filter(|item| **item == ListItem::Map(from))
        {
            *item = ListItem::Map(to);
        }
    }

    /// One map left is the normal single view of that map; one folder left
    /// is that folder chosen alone.
    fn settle(&mut self, open: Option<AreaId>) -> Pressed {
        match self.items.as_slice() {
            [ListItem::Map(area_id)] => {
                let area_id = *area_id;
                self.items.clear();
                if open == Some(area_id) {
                    Pressed::Nothing
                } else {
                    Pressed::Open(area_id)
                }
            }
            [ListItem::Folder(atlas_id)] => Pressed::Folder(*atlas_id),
            _ => Pressed::Nothing,
        }
    }
}

/// The rows from `from` to `to`, both included, top to bottom; `None` when
/// either isn't in the list as drawn (inside a closed folder, say).
fn range(order: &[ListItem], from: ListItem, to: ListItem) -> Option<Vec<ListItem>> {
    let start = order.iter().position(|item| *item == from)?;
    let end = order.iter().position(|item| *item == to)?;
    let (low, high) = if start <= end {
        (start, end)
    } else {
        (end, start)
    };
    Some(order[low..=high].to_vec())
}

/// The window's multi-selection: what is chosen, what the keyboard holds,
/// and the pane's own state.
#[derive(Debug, Default)]
pub struct MultiSelect {
    pub selection: Selection,
    /// The modifiers held in this window; a button press doesn't report
    /// them.
    pub modifiers: keyboard::Modifiers,
    /// The list's selectable rows in the order last drawn, for Shift
    /// ranges. The list's view refills it.
    pub order: RefCell<Vec<ListItem>>,
    /// The pane's inline "Delete …?" is showing.
    pub confirming_delete: bool,
    /// A move or delete is running.
    pub busy: bool,
}

impl MultiSelect {
    /// Starts a drawing of the list.
    pub fn begin_order(&self) {
        self.order.borrow_mut().clear();
    }

    /// Notes a row as the list draws it.
    pub fn record(&self, item: ListItem) {
        self.order.borrow_mut().push(item);
    }
}

/// Tracks the modifiers held in each editor window: iced buttons don't say
/// whether Ctrl or Shift was down. A window that loses focus holds none.
pub fn modifier_events(
    event: IcedEvent,
    _status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    let modifiers = match event {
        IcedEvent::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => modifiers,
        IcedEvent::Window(window::Event::Unfocused) => keyboard::Modifiers::default(),
        _ => return None,
    };
    Some(Message::Multi(MultiMessage::Modifiers(
        window_id, modifiers,
    )))
}

// ===========================================================================
// Actions and what they apply to
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Move,
    Share,
    Transfer,
    Servers,
    Delete,
}

/// What the actions need to know about one chosen item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Facts {
    pub item: ListItem,
    pub name: String,
    /// A folder is cloud or local.
    pub storage: MapStorage,
    /// A map the viewer owns, or a folder in the viewer's own list (owned or
    /// administered).
    pub owned: bool,
    /// The viewer owns it outright; transfer is the owner's alone.
    pub is_owner: bool,
    /// A clan's map or folder, which is never transferred.
    pub clan_owned: bool,
    /// A map the viewer may share: its owner, or a re-sharer.
    pub may_share: bool,
    /// The folder a map is filed in; a filed map shows where its folder does.
    pub atlas_id: Option<AtlasId>,
    /// The map's owner, never a share recipient.
    pub owner_id: Option<Uuid>,
    /// A folder holding maps outside the choice.
    pub keeps_maps: bool,
}

impl Action {
    /// Whether this action applies to the item, gated as the map's and the
    /// folder's ⋯ menus gate it.
    #[must_use]
    pub fn applies(self, facts: &Facts) -> bool {
        let cloud = facts.storage == MapStorage::Cloud;
        match (self, facts.item) {
            (Self::Move, ListItem::Map(_)) => {
                facts.owned && !facts.clan_owned && facts.storage != MapStorage::Session
            }
            // Folders don't nest.
            (Self::Move, ListItem::Folder(_)) => false,
            (Self::Share, ListItem::Map(_)) => cloud && facts.may_share,
            (Self::Share, ListItem::Folder(_)) => cloud && facts.owned,
            (Self::Transfer, _) => cloud && facts.is_owner && !facts.clan_owned,
            (Self::Servers, ListItem::Map(_)) => cloud && facts.atlas_id.is_none(),
            (Self::Servers, ListItem::Folder(_)) => cloud && !facts.clan_owned,
            (Self::Delete, _) => facts.owned,
        }
    }
}

/// How many maps and folders.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub maps: usize,
    pub folders: usize,
}

impl Counts {
    pub fn of<'a>(facts: impl IntoIterator<Item = &'a Facts>) -> Self {
        let mut counts = Self::default();
        for facts in facts {
            match facts.item {
                ListItem::Map(_) => counts.maps += 1,
                ListItem::Folder(_) => counts.folders += 1,
            }
        }
        counts
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.maps == 0 && self.folders == 0
    }
}

/// The items `action` applies to, and the ones it skips.
fn split(action: Action, facts: &[Facts]) -> (Vec<&Facts>, Vec<&Facts>) {
    facts.iter().partition(|facts| action.applies(facts))
}

// ===========================================================================
// Copy
// ===========================================================================

fn join_counts(
    counts: Counts,
    maps: impl Fn(usize) -> String,
    folders: impl Fn(usize) -> String,
) -> String {
    match (counts.maps, counts.folders) {
        (0, count) => folders(count),
        (count, 0) => maps(count),
        (map_count, folder_count) => crate::i18n::t!(
            "mapper-multi-and",
            "first" => maps(map_count),
            "second" => folders(folder_count)
        ),
    }
}

/// "3 maps and 1 folder", standing alone.
#[must_use]
pub fn heading(counts: Counts) -> String {
    join_counts(
        counts,
        |count| crate::i18n::t!("mapper-multi-maps", "count" => count),
        |count| crate::i18n::t!("mapper-multi-folders", "count" => count),
    )
}

/// "3 maps and 1 folder" as what an action acts on; some languages inflect
/// it.
#[must_use]
pub fn object(counts: Counts) -> String {
    join_counts(
        counts,
        |count| crate::i18n::t!("mapper-multi-maps-object", "count" => count),
        |count| crate::i18n::t!("mapper-multi-folders-object", "count" => count),
    )
}

/// A button's label: "Transfer 3 maps…", or the bare action when it applies
/// to none.
#[must_use]
pub fn action_label(action: Action, counts: Counts) -> String {
    if counts.is_empty() {
        return match action {
            Action::Move => crate::i18n::t!("mapper-menu-move-to-folder"),
            Action::Share => crate::i18n::t!("area-list-share-action"),
            Action::Transfer => crate::i18n::t!("mapper-transfer-action"),
            Action::Servers => crate::i18n::t!("area-list-servers-action"),
            Action::Delete => crate::i18n::t!("mapper-multi-delete-none"),
        };
    }
    let items = object(counts);
    match action {
        Action::Move => crate::i18n::t!("mapper-multi-move", "items" => items),
        Action::Share => crate::i18n::t!("mapper-multi-share", "items" => items),
        Action::Transfer => crate::i18n::t!("mapper-multi-transfer", "items" => items),
        Action::Servers => crate::i18n::t!("mapper-multi-servers", "items" => items),
        Action::Delete => crate::i18n::t!("mapper-multi-delete", "items" => items),
    }
}

/// The small muted word after an item's name in the pane.
fn kind(facts: &Facts) -> String {
    match (facts.item, facts.storage) {
        (ListItem::Map(_), MapStorage::Session) => crate::i18n::t!("mapper-multi-kind-session-map"),
        (ListItem::Map(_), MapStorage::Local) => crate::i18n::t!("mapper-multi-kind-local-map"),
        (ListItem::Map(_), MapStorage::Cloud) if facts.owned => {
            crate::i18n::t!("mapper-multi-kind-map")
        }
        (ListItem::Map(_), MapStorage::Cloud) => crate::i18n::t!("mapper-multi-kind-shared-map"),
        (ListItem::Folder(_), MapStorage::Local) => {
            crate::i18n::t!("mapper-multi-kind-local-folder")
        }
        (ListItem::Folder(_), _) if facts.owned => crate::i18n::t!("mapper-multi-kind-folder"),
        (ListItem::Folder(_), _) => crate::i18n::t!("mapper-multi-kind-shared-folder"),
    }
}

/// Names in the locale's quotes, listed.
fn name_list<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    let separator = crate::i18n::t!("mapper-multi-list-separator");
    names
        .into_iter()
        .map(|name| crate::i18n::t!("mapper-multi-quoted", "name" => name))
        .collect::<Vec<_>>()
        .join(&separator)
}

/// "Skipped: “Forest”, “Cities”".
fn skipped_line(skipped: &[String]) -> Option<String> {
    (!skipped.is_empty()).then(|| {
        crate::i18n::t!(
            "mapper-multi-skipped",
            "names" => name_list(skipped.iter().map(String::as_str))
        )
    })
}

fn muted(theme: &crate::Theme) -> text::Style {
    text::Style {
        color: Some(theme.styles.text.normal.scale_alpha(0.6)),
    }
}

fn multi(message: MultiMessage) -> Message {
    Message::Multi(message)
}

fn dialog_message(message: DialogMessage) -> Message {
    Message::Multi(MultiMessage::Dialog(message))
}

// ===========================================================================
// Messages and dialogs
// ===========================================================================

#[derive(Debug, Clone)]
pub enum MultiMessage {
    /// The modifiers held in a window changed.
    Modifiers(window::Id, keyboard::Modifiers),
    /// A map row or folder header pressed in the list.
    Pressed(ListItem),
    /// The × beside an item in the pane.
    Removed(ListItem),
    Cleared,
    /// An action's button: a dialog, or Delete's inline confirm.
    Requested(Action),
    DeleteCancelled,
    DeleteConfirmed,
    Deleted(DeleteReport),
    /// A destination picked in the move dialog.
    MoveTo(MapDestination),
    /// The move dialog's new-folder form.
    NewFolder(NewFolderMessage),
    /// Each moved map's id before and after.
    Moved(Result<Vec<(AreaId, AreaId)>, String>),
    Dialog(DialogMessage),
}

#[derive(Debug, Clone)]
pub enum DialogMessage {
    FriendsLoaded(Result<Vec<FriendView>, CloudError>),
    FilterChanged(String),
    /// Whose the maps become in the clan they are given to.
    OwnershipPicked(smudgy_cloud::clan_maps::MapOwnership),
    /// A friend checked or unchecked in the share dialog.
    RecipientToggled(Uuid, bool),
    /// The friend or clan the transfer offers go to.
    RecipientPicked(TransferRecipient),
    TransferFolderPicked(super::clan_maps::FolderChoice),
    FlagToggled(modals::GrantFlag, bool),
    Submit,
    Shared(Vec<ShareOutcome>),
    Offered(Vec<(String, Result<(), String>)>),
    ServerToggled(String, bool),
    /// What becomes of a deleted folder's other maps.
    LeftoversPicked(Leftovers),
    /// The folder picker for one storage's leftover maps.
    Destination(usize, PickerMessage),
}

/// How a delete went: what went, and what didn't with why.
#[derive(Debug, Clone, Default)]
pub struct DeleteReport {
    pub maps: Vec<AreaId>,
    pub folders: Vec<AtlasId>,
    pub failures: Vec<(String, String)>,
    /// Folders made to take a deleted folder's other maps.
    pub made: Vec<AtlasId>,
}

/// How sharing one item went: the friends it failed for.
#[derive(Debug, Clone)]
pub struct ShareOutcome {
    pub name: String,
    pub failures: Vec<(String, CloudError)>,
}

/// One item a dialog acts on.
#[derive(Debug, Clone)]
pub struct Target {
    pub item: ListItem,
    pub name: String,
    pub owner_id: Option<Uuid>,
    pub filed: bool,
}

impl Target {
    fn of(facts: &Facts) -> Self {
        Self {
            item: facts.item,
            name: facts.name.clone(),
            owner_id: facts.owner_id,
            filed: facts.atlas_id.is_some(),
        }
    }
}

/// A dialog acting on the chosen items an action applies to.
#[derive(Debug, Clone)]
pub enum MultiDialog {
    Delete(DeleteDialog),
    Move(MoveDialog),
    Share(ShareDialog),
    Transfer(TransferDialog),
    Servers(ServersDialog),
}

/// What becomes of the maps a deleted folder holds outside the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Leftovers {
    Move,
    Delete,
}

/// A chosen folder holding maps outside the choice.
#[derive(Debug, Clone)]
pub struct Holding {
    atlas_id: AtlasId,
    name: String,
    storage: MapStorage,
    /// The maps outside the choice, name-sorted.
    maps: Vec<(AreaId, String)>,
}

/// Delete's confirmation when a chosen folder holds maps outside the
/// choice: it names them and asks whether they move or go too.
#[derive(Debug, Clone)]
pub struct DeleteDialog {
    counts: Counts,
    skipped: Vec<String>,
    maps: Vec<(AreaId, String)>,
    folders: Vec<(AtlasId, String)>,
    holding: Vec<Holding>,
    leftovers: Leftovers,
    /// Where they move: a picker for each storage the folders are in.
    destinations: Vec<FolderPicker>,
    busy: bool,
}

impl DeleteDialog {
    /// Cloud leftovers need a review before any selected map or folder goes.
    pub(super) fn cloud_leftovers(&self) -> Vec<AreaId> {
        if self.leftovers != Leftovers::Move {
            return Vec::new();
        }
        self.holding
            .iter()
            .filter(|held| held.storage == MapStorage::Cloud)
            .flat_map(|held| held.maps.iter().map(|(id, _)| *id))
            .collect()
    }

    pub(super) fn cloud_destination(&self) -> (Option<AtlasId>, Option<String>) {
        self.destinations
            .iter()
            .find(|picker| picker.storage() == MapStorage::Cloud)
            .map_or((None, None), |picker| {
                (picker.chosen(), picker.new_folder_name())
            })
    }

    pub(super) fn deletion_notice(&self) -> String {
        crate::i18n::t!("mapper-multi-delete-question", "items" => object(self.counts))
    }

    pub(super) fn deletion_names(&self) -> impl Iterator<Item = &str> {
        self.maps
            .iter()
            .map(|(_, name)| name.as_str())
            .chain(self.folders.iter().map(|(_, name)| name.as_str()))
    }

    /// Whether Delete can go ahead: moving needs a folder for each storage.
    fn ready(&self) -> bool {
        !self.busy
            && (self.leftovers == Leftovers::Delete
                || self.destinations.iter().all(FolderPicker::ready))
    }
}

#[derive(Debug, Clone)]
pub struct MoveDialog {
    counts: Counts,
    skipped: Vec<String>,
    /// Each map and where it is now.
    maps: Vec<(AreaId, MapDestination)>,
    targets: Vec<(MapDestination, String)>,
    /// A folder being named to move them into.
    pub(super) new_folder: Option<NewFolderForm>,
}

#[derive(Debug, Clone)]
pub struct ShareDialog {
    counts: Counts,
    skipped: Vec<String>,
    targets: Vec<Target>,
    /// `None` while loading.
    friends: Option<Result<Vec<FriendView>, String>>,
    filter: String,
    selected: HashSet<Uuid>,
    can_edit: bool,
    can_reshare: bool,
    can_copy: bool,
    submitting: bool,
    results: Vec<ShareOutcome>,
}

#[derive(Debug, Clone)]
pub struct TransferDialog {
    counts: Counts,
    skipped: Vec<String>,
    targets: Vec<Target>,
    friends: Option<Result<Vec<FriendView>, String>>,
    /// Destinations where the viewer can complete every selected transfer.
    destinations: super::clan_maps::TransferDestinations,
    operations: Vec<Uuid>,
    filter: String,
    selected: Option<TransferRecipient>,
    /// Whose the maps become in a picked clan.
    ownership: smudgy_cloud::clan_maps::MapOwnership,
    submitting: bool,
    /// One line per item once the offers went out.
    results: Vec<(String, Result<(), String>)>,
}

/// Whether the chosen items show on one server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerState {
    All,
    Mixed,
    Off,
}

#[derive(Debug, Clone)]
pub struct ServersDialog {
    counts: Counts,
    skipped: Vec<String>,
    targets: Vec<ScopeTarget>,
    servers: Vec<(String, ServerState)>,
}

fn scope_entries(scopes: &MapScopes, target: ScopeTarget) -> BTreeSet<String> {
    match target {
        ScopeTarget::Atlas(atlas_id) => scopes.atlas_entries(&atlas_id),
        ScopeTarget::Area(area_id) => scopes.area_entries(&area_id),
    }
}

/// Whether all, some or none of `targets` show on `server`.
fn server_state(scopes: &MapScopes, targets: &[ScopeTarget], server: &str) -> ServerState {
    let on = targets
        .iter()
        .filter(|target| scope_entries(scopes, **target).contains(server))
        .count();
    match on {
        0 => ServerState::Off,
        on if on == targets.len() => ServerState::All,
        _ => ServerState::Mixed,
    }
}

/// The changes that show (or hide) every one of `targets` on `server`; none
/// for those already so.
fn server_deltas(
    scopes: &MapScopes,
    targets: &[ScopeTarget],
    server: &str,
    show: bool,
) -> Vec<ScopeDelta> {
    targets
        .iter()
        .filter(|target| scope_entries(scopes, **target).contains(server) != show)
        .map(|target| match *target {
            ScopeTarget::Atlas(atlas_id) => ScopeDelta::SetAtlasEntry {
                atlas_id,
                entry: server.to_string(),
                show,
            },
            ScopeTarget::Area(area_id) => ScopeDelta::SetAreaEntry {
                area_id,
                entry: server.to_string(),
                show,
            },
        })
        .collect()
}

impl ServersDialog {
    fn refresh(&mut self, scopes: &MapScopes) {
        for (server, state) in &mut self.servers {
            *state = server_state(scopes, &self.targets, server);
        }
    }
}

fn friend_label(friend: &FriendView) -> String {
    friend
        .nickname
        .clone()
        .unwrap_or_else(|| friend.user_id.to_string())
}

fn transfer_error(error: &CloudError, to: TransferRecipient) -> String {
    match error {
        CloudError::NotFoundOrNoAccess if matches!(to, TransferRecipient::Clan(..)) => {
            crate::i18n::t!("mapper-transfer-owner-clan-only")
        }
        CloudError::NotFoundOrNoAccess => crate::i18n::t!("mapper-transfer-owner-friend-only"),
        other => display_error(other),
    }
}

// ===========================================================================
// The window side
// ===========================================================================

impl MapEditorWindow {
    /// What the actions need to know about `item`; `None` once it's gone.
    fn item_facts(&self, item: ListItem, atlas: &AtlasCache) -> Option<Facts> {
        match item {
            ListItem::Map(area_id) => {
                let area = atlas.get_area(&area_id)?;
                let access = area.effective_access();
                let meta = area.meta();
                Some(Facts {
                    item,
                    name: area.get_name().to_string(),
                    storage: self.mapper.area_storage(&area_id),
                    owned: area.is_owned(),
                    is_owner: area.is_owned(),
                    clan_owned: meta.clan_id.is_some(),
                    may_share: access.is_owner || access.can_reshare,
                    atlas_id: meta.atlas_id,
                    owner_id: meta.owner_id,
                    keeps_maps: false,
                })
            }
            ListItem::Folder(atlas_id) => {
                let storage = if self.local_atlas_ids.contains(&atlas_id) {
                    MapStorage::Local
                } else {
                    MapStorage::Cloud
                };
                if let Some(own) = self.atlases.iter().find(|folder| folder.id == atlas_id) {
                    // A clan's folder is the clan's, never the viewer's own:
                    // its own menu follows what the clan lets them do there.
                    let clan_folder = own.clan_id.is_some();
                    return Some(Facts {
                        item,
                        name: own.name.clone(),
                        storage,
                        owned: !clan_folder,
                        is_owner: own.is_owner && !clan_folder,
                        clan_owned: own.clan_id.is_some(),
                        may_share: false,
                        atlas_id: None,
                        owner_id: None,
                        keeps_maps: false,
                    });
                }
                // A folder shared with the viewer is known by its maps.
                let shared = atlas
                    .areas()
                    .find(|area| !area.is_owned() && area.meta().atlas_id == Some(atlas_id))?;
                Some(Facts {
                    item,
                    name: shared
                        .meta()
                        .atlas_name
                        .clone()
                        .filter(|name| !name.is_empty())
                        .unwrap_or_else(|| crate::i18n::t!("area-list-shared-folder")),
                    storage,
                    owned: false,
                    is_owner: false,
                    clan_owned: false,
                    may_share: false,
                    atlas_id: None,
                    owner_id: shared.meta().owner_id,
                    keeps_maps: false,
                })
            }
        }
    }

    /// The chosen items still there, in the list's order.
    pub(super) fn multi_facts(&self) -> Vec<Facts> {
        let atlas = self.mapper.get_current_atlas();
        let mut facts: Vec<Facts> = self
            .multi
            .selection
            .items(self.editor.area_id())
            .into_iter()
            .filter_map(|item| self.item_facts(item, &atlas))
            .collect();
        let deleted: HashSet<AreaId> = facts
            .iter()
            .filter(|facts| Action::Delete.applies(facts))
            .filter_map(|facts| match facts.item {
                ListItem::Map(area_id) => Some(area_id),
                ListItem::Folder(_) => None,
            })
            .collect();
        for facts in &mut facts {
            if let ListItem::Folder(atlas_id) = facts.item {
                facts.keeps_maps = atlas.areas().any(|area| {
                    area.meta().atlas_id == Some(atlas_id) && !deleted.contains(area.get_id())
                });
            }
        }
        let order = self.multi.order.borrow();
        facts.sort_by_key(|facts| {
            order
                .iter()
                .position(|item| *item == facts.item)
                .unwrap_or(usize::MAX)
        });
        facts
    }

    fn apply_pressed(&mut self, pressed: Pressed) -> Update<Message, Event> {
        match pressed {
            Pressed::Open(area_id) => self.update(Message::AreaSelected(area_id)),
            Pressed::Folder(_) => self.atlas_chosen(),
            Pressed::Nothing => Update::none(),
        }
    }

    /// Drops chosen items that have gone (deleted elsewhere, access lost).
    pub(super) fn prune_multi_selection(&mut self) -> Update<Message, Event> {
        if self.multi.busy || !self.multi.selection.is_chosen() {
            return Update::none();
        }
        let atlas = self.mapper.get_current_atlas();
        let open = self.editor.area_id();
        let gone: Vec<ListItem> = self
            .multi
            .selection
            .items(open)
            .into_iter()
            .filter(|item| self.item_facts(*item, &atlas).is_none())
            .collect();
        if gone.is_empty() {
            return Update::none();
        }
        let pressed = self
            .multi
            .selection
            .retain(|item| !gone.contains(&item), open);
        self.apply_pressed(pressed)
    }

    /// Keeps an open servers dialog in step with the scope store.
    pub(super) fn refresh_multi_servers(&mut self) {
        if let Some(modals::Modal::Multi(dialog)) = &mut self.modal
            && let MultiDialog::Servers(servers) = dialog.as_mut()
        {
            servers.refresh(&self.map_scopes);
        }
    }

    pub(super) fn update_multi(&mut self, message: MultiMessage) -> Update<Message, Event> {
        match message {
            MultiMessage::Modifiers(window_id, modifiers) => {
                if window_id == self.window_id {
                    self.multi.modifiers = modifiers;
                }
                Update::none()
            }
            MultiMessage::Pressed(item) => {
                let order = self.multi.order.borrow().clone();
                let open = self.editor.area_id();
                let modifiers = self.multi.modifiers;
                let pressed = self.multi.selection.press(item, modifiers, open, &order);
                self.multi.confirming_delete = false;
                if self.multi.selection.is_multi() {
                    self.context_menu = None;
                    self.map_menu_open = false;
                }
                self.apply_pressed(pressed)
            }
            MultiMessage::Removed(item) => {
                let pressed = self.multi.selection.remove(item, self.editor.area_id());
                self.multi.confirming_delete = false;
                self.apply_pressed(pressed)
            }
            MultiMessage::Cleared => {
                self.multi.selection.clear();
                self.multi.confirming_delete = false;
                Update::none()
            }
            MultiMessage::Requested(action) => self.multi_requested(action),
            MultiMessage::DeleteCancelled => {
                self.multi.confirming_delete = false;
                Update::none()
            }
            MultiMessage::DeleteConfirmed => self.multi_delete(),
            MultiMessage::Deleted(report) => self.multi_deleted(report),
            MultiMessage::MoveTo(destination) => self.multi_move(destination),
            MultiMessage::NewFolder(message) => self.multi_new_folder(message),
            MultiMessage::Moved(result) => self.multi_moved(result),
            MultiMessage::Dialog(message) => self.update_multi_dialog(message),
        }
    }

    /// Opens the action's dialog, or Delete's inline confirm, for the items
    /// it applies to.
    fn multi_requested(&mut self, action: Action) -> Update<Message, Event> {
        if self.multi.busy || !self.multi.selection.is_multi() {
            return Update::none();
        }
        let facts = self.multi_facts();
        let (applies, skipped) = split(action, &facts);
        if applies.is_empty() {
            return Update::none();
        }
        let counts = Counts::of(applies.iter().copied());
        let skipped: Vec<String> = skipped.iter().map(|facts| facts.name.clone()).collect();
        let dialog = match action {
            Action::Delete => {
                // A folder holding maps outside the choice asks what becomes
                // of them; otherwise the pane's own confirm asks.
                let holding = self.holding_maps(&applies);
                if holding.is_empty() {
                    self.multi.confirming_delete = true;
                    return Update::none();
                }
                let deleting: HashSet<AtlasId> = holding.iter().map(|held| held.atlas_id).collect();
                let folders: Vec<_> = self
                    .own_folders()
                    .into_iter()
                    .filter(|folder| !deleting.contains(&folder.id))
                    .collect();
                let mut storages: Vec<MapStorage> =
                    holding.iter().map(|held| held.storage).collect();
                storages.sort_by_key(|storage| *storage != MapStorage::Cloud);
                storages.dedup();
                let mut maps = Vec::new();
                let mut chosen_folders = Vec::new();
                for facts in &applies {
                    match facts.item {
                        ListItem::Map(area_id) => maps.push((area_id, facts.name.clone())),
                        ListItem::Folder(atlas_id) => {
                            chosen_folders.push((atlas_id, facts.name.clone()));
                        }
                    }
                }
                self.multi.confirming_delete = false;
                MultiDialog::Delete(DeleteDialog {
                    counts,
                    skipped,
                    maps,
                    folders: chosen_folders,
                    destinations: storages
                        .into_iter()
                        .map(|storage| FolderPicker::in_storage(folders.clone(), storage, None))
                        .collect(),
                    holding,
                    leftovers: Leftovers::Move,
                    busy: false,
                })
            }
            Action::Move => MultiDialog::Move(MoveDialog {
                // With no folder to pick, the form opens at once.
                new_folder: self.folder_destinations().is_empty().then(|| {
                    NewFolderForm::new(
                        self.mapper.default_storage(),
                        self.cloud.snapshot.get().signed_in,
                    )
                }),
                counts,
                skipped,
                maps: applies
                    .iter()
                    .filter_map(|facts| match facts.item {
                        ListItem::Map(area_id) => Some((
                            area_id,
                            MapDestination {
                                storage: facts.storage,
                                atlas_id: facts.atlas_id,
                            },
                        )),
                        ListItem::Folder(_) => None,
                    })
                    .collect(),
                targets: self.folder_destinations(),
            }),
            Action::Share => MultiDialog::Share(ShareDialog {
                counts,
                skipped,
                targets: applies.iter().map(|facts| Target::of(facts)).collect(),
                friends: None,
                filter: String::new(),
                selected: HashSet::new(),
                can_edit: false,
                can_reshare: false,
                can_copy: false,
                submitting: false,
                results: Vec::new(),
            }),
            Action::Transfer => MultiDialog::Transfer(TransferDialog {
                counts,
                skipped,
                targets: applies.iter().map(|facts| Target::of(facts)).collect(),
                friends: None,
                destinations: super::clan_maps::TransferDestinations::new(
                    self,
                    applies
                        .iter()
                        .any(|facts| matches!(facts.item, ListItem::Map(_))),
                    applies
                        .iter()
                        .any(|facts| matches!(facts.item, ListItem::Folder(_))),
                ),
                operations: applies.iter().map(|_| Uuid::new_v4()).collect(),
                filter: String::new(),
                ownership: smudgy_cloud::clan_maps::MapOwnership::Clan,
                selected: None,
                submitting: false,
                results: Vec::new(),
            }),
            Action::Servers => {
                let targets: Vec<ScopeTarget> = applies
                    .iter()
                    .map(|facts| match facts.item {
                        ListItem::Map(area_id) => ScopeTarget::Area(area_id),
                        ListItem::Folder(atlas_id) => ScopeTarget::Atlas(atlas_id),
                    })
                    .collect();
                let servers = smudgy_core::models::server::list_servers()
                    .map(|servers| {
                        servers
                            .into_iter()
                            .map(|server| {
                                let state = server_state(&self.map_scopes, &targets, &server.name);
                                (server.name, state)
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                MultiDialog::Servers(ServersDialog {
                    counts,
                    skipped,
                    targets,
                    servers,
                })
            }
        };
        let fetch_friends = matches!(dialog, MultiDialog::Share(_) | MultiDialog::Transfer(_));
        self.modal = Some(modals::Modal::Multi(Box::new(dialog)));
        if !fetch_friends {
            return Update::none();
        }
        let client = self.cloud.client.clone();
        Update::with_task(Task::perform(
            async move { client.friends().await },
            |result| dialog_message(DialogMessage::FriendsLoaded(result)),
        ))
    }

    /// The move dialog's new-folder form: open it, fill it in, and make the
    /// folder; the move follows once it exists.
    fn multi_new_folder(&mut self, message: NewFolderMessage) -> Update<Message, Event> {
        let cloud_available = self.cloud.snapshot.get().signed_in;
        let storage = self.mapper.default_storage();
        let Some(modals::Modal::Multi(dialog)) = &mut self.modal else {
            return Update::none();
        };
        let MultiDialog::Move(dialog) = dialog.as_mut() else {
            return Update::none();
        };
        match message {
            NewFolderMessage::Opened => {
                if dialog.new_folder.is_none() {
                    dialog.new_folder = Some(NewFolderForm::new(storage, cloud_available));
                }
            }
            NewFolderMessage::Name(name) => {
                if let Some(form) = &mut dialog.new_folder {
                    form.name = name;
                }
            }
            NewFolderMessage::Storage(storage) => {
                if let Some(form) = &mut dialog.new_folder {
                    form.set_storage(storage);
                }
            }
            NewFolderMessage::Confirmed => {
                let Some(form) = &mut dialog.new_folder else {
                    return Update::none();
                };
                let Some(name) = form.name().filter(|_| !form.busy) else {
                    return Update::none();
                };
                form.busy = true;
                form.error = None;
                return Update::with_task(folder_picker::make_folder(
                    self.mapper.clone(),
                    name,
                    form.storage,
                ));
            }
        }
        Update::none()
    }

    fn multi_move(&mut self, destination: MapDestination) -> Update<Message, Event> {
        let Some(modals::Modal::Multi(dialog)) = self.modal.take() else {
            return Update::none();
        };
        let MultiDialog::Move(dialog) = *dialog else {
            return Update::none();
        };
        let maps: Vec<AreaId> = dialog
            .maps
            .iter()
            .filter(|(area_id, current)| *current != destination && self.area_owned(*area_id))
            .map(|(area_id, _)| *area_id)
            .collect();
        if maps.is_empty() {
            return Update::none();
        }
        if destination.storage == MapStorage::Cloud
            && maps
                .iter()
                .any(|id| self.mapper.area_storage(id) == MapStorage::Cloud)
        {
            return self.review_filing(super::filing::Request::Maps {
                ids: maps,
                destination,
                multi: true,
                clan: false,
            });
        }
        if destination.storage == MapStorage::Local
            && maps
                .iter()
                .any(|id| self.mapper.area_storage(id) == MapStorage::Cloud)
        {
            return self.review_local_move(super::local_move::Request::Maps {
                ids: maps,
                destination,
                multi: true,
            });
        }
        self.multi.busy = true;
        self.multi.confirming_delete = false;
        let mapper = self.mapper.clone();
        // One relocation carries every map, so links between them survive a
        // move to the other storage tier.
        Update::with_task(Task::perform(
            async move {
                match mapper
                    .relocate_areas(maps, destination, RelocationMode::Move)
                    .await
                {
                    Ok(moved) => Ok(moved
                        .source_ids
                        .into_iter()
                        .zip(moved.destination_ids)
                        .collect()),
                    Err(failure) => Err(match &failure.completed {
                        Some(completed) => {
                            let atlas = mapper.get_current_atlas();
                            let names: Vec<String> = completed
                                .destination_ids
                                .iter()
                                .filter_map(|id| atlas.get_area(id))
                                .map(|area| area.get_name().to_string())
                                .collect();
                            crate::i18n::t!(
                                "mapper-relocation-duplicate-notice",
                                "error" => display_error(&failure.error),
                                "name" => names.join(&crate::i18n::t!("mapper-multi-list-separator"))
                            )
                        }
                        None => failure.to_string(),
                    }),
                }
            },
            |result| multi(MultiMessage::Moved(result)),
        ))
    }

    fn multi_moved(
        &mut self,
        result: Result<Vec<(AreaId, AreaId)>, String>,
    ) -> Update<Message, Event> {
        self.multi.busy = false;
        let moved = match result {
            Ok(moved) => moved,
            Err(error) => {
                self.editor_notice = Some((std::time::Instant::now(), error));
                return Update::none();
            }
        };
        let open = self.editor.area_id();
        let mut reopen = None;
        let mut deltas = Vec::new();
        for (from, to) in moved {
            if from != to {
                self.multi.selection.replace_map(from, to);
                if open == Some(from) {
                    reopen = Some(to);
                }
            }
            if let Some(Event::ScopeAssociationsChanged(more)) = self.associate_new_area(to) {
                deltas.extend(more);
            }
        }
        let mut update = reopen.map_or_else(Update::none, |area_id| self.open_area(area_id));
        // A move bumps the maps' revisions; they aren't someone else's edits.
        self.refresh_seen_rev();
        self.inspector.resync(&self.mapper, &self.editor);
        if let Some(Event::ScopeAssociationsChanged(more)) = update.event.take() {
            deltas.extend(more);
        }
        if !deltas.is_empty() {
            update.event = Some(Event::ScopeAssociationsChanged(deltas));
        }
        update
    }

    fn multi_delete(&mut self) -> Update<Message, Event> {
        if self.multi.busy || !self.multi.confirming_delete {
            return Update::none();
        }
        let facts = self.multi_facts();
        let (applies, _) = split(Action::Delete, &facts);
        let mut maps = Vec::new();
        let mut folders = Vec::new();
        for facts in applies {
            match facts.item {
                ListItem::Map(area_id) => maps.push((area_id, facts.name.clone())),
                ListItem::Folder(atlas_id) => folders.push((atlas_id, facts.name.clone())),
            }
        }
        self.multi.confirming_delete = false;
        if maps.is_empty() && folders.is_empty() {
            return Update::none();
        }
        self.multi.busy = true;
        let mapper = self.mapper.clone();
        Update::with_task(Task::perform(
            async move {
                let mut report = DeleteReport::default();
                for (area_id, name) in maps {
                    match mapper.delete_area(area_id).await {
                        Ok(()) => report.maps.push(area_id),
                        Err(error) => report.failures.push((name, display_error(&error))),
                    }
                }
                // Folders go last, emptied by the maps above.
                for (atlas_id, name) in folders {
                    match mapper.delete_atlas(atlas_id).await {
                        Ok(()) => report.folders.push(atlas_id),
                        Err(error) => report.failures.push((name, display_error(&error))),
                    }
                }
                report
            },
            |report| multi(MultiMessage::Deleted(report)),
        ))
    }

    /// The chosen folders holding maps outside the choice, with those maps.
    fn holding_maps(&self, applies: &[&Facts]) -> Vec<Holding> {
        let atlas = self.mapper.get_current_atlas();
        let chosen: HashSet<AreaId> = applies
            .iter()
            .filter_map(|facts| match facts.item {
                ListItem::Map(area_id) => Some(area_id),
                ListItem::Folder(_) => None,
            })
            .collect();
        applies
            .iter()
            .filter(|facts| facts.keeps_maps)
            .filter_map(|facts| {
                let ListItem::Folder(atlas_id) = facts.item else {
                    return None;
                };
                let mut maps: Vec<(AreaId, String)> = atlas
                    .areas()
                    .filter(|area| {
                        area.meta().atlas_id == Some(atlas_id) && !chosen.contains(area.get_id())
                    })
                    .map(|area| (*area.get_id(), area.get_name().to_string()))
                    .collect();
                maps.sort_by_cached_key(|(_, name)| name.to_lowercase());
                Some(Holding {
                    atlas_id,
                    name: facts.name.clone(),
                    storage: facts.storage,
                    maps,
                })
            })
            .collect()
    }

    fn multi_deleted(&mut self, report: DeleteReport) -> Update<Message, Event> {
        self.multi.busy = false;
        if matches!(&self.modal, Some(modals::Modal::Multi(dialog)) if matches!(dialog.as_ref(), MultiDialog::Delete(_)))
        {
            self.modal = None;
        }
        // A folder made for the leftovers shows on this server, like any new one.
        let mut associated = None;
        for atlas_id in &report.made {
            associated = self.associate_new_atlas(*atlas_id).or(associated);
        }
        for atlas_id in &report.folders {
            self.collapsed_folders.remove(&FolderKey::Atlas(*atlas_id));
        }
        self.atlases
            .retain(|folder| !report.folders.contains(&folder.id));
        if !report.maps.is_empty() {
            self.clear_history();
        }
        let open = self
            .editor
            .area_id()
            .filter(|area_id| !report.maps.contains(area_id));
        let pressed = self.multi.selection.retain(
            |item| match item {
                ListItem::Map(area_id) => !report.maps.contains(&area_id),
                ListItem::Folder(atlas_id) => !report.folders.contains(&atlas_id),
            },
            open,
        );
        let mut update = self.apply_pressed(pressed);
        if self
            .editor
            .area_id()
            .is_some_and(|area_id| report.maps.contains(&area_id))
        {
            self.show_first_area();
        }
        self.refresh_seen_rev();
        self.inspector.resync(&self.mapper, &self.editor);
        if let Some((_, error)) = report.failures.first() {
            self.editor_notice = Some((
                std::time::Instant::now(),
                crate::i18n::t!(
                    "mapper-multi-delete-error",
                    "names" => name_list(report.failures.iter().map(|(name, _)| name.as_str())),
                    "error" => error.clone()
                ),
            ));
        }
        if !report.folders.is_empty() || !report.made.is_empty() {
            update.task = Task::batch([update.task, self.fetch_atlases()]);
        }
        update.event = associated.or(update.event);
        update
    }

    fn update_multi_dialog(&mut self, message: DialogMessage) -> Update<Message, Event> {
        let Some(modals::Modal::Multi(dialog)) = &mut self.modal else {
            return Update::none();
        };
        match (dialog.as_mut(), message) {
            (MultiDialog::Share(dialog), DialogMessage::FriendsLoaded(result)) => {
                dialog.friends = Some(result.map_err(|error| display_error(&error)));
            }
            (MultiDialog::Transfer(dialog), DialogMessage::FriendsLoaded(result)) => {
                dialog.friends = Some(result.map_err(|error| display_error(&error)));
            }
            (MultiDialog::Share(dialog), DialogMessage::FilterChanged(value)) => {
                dialog.filter = value;
            }
            (MultiDialog::Transfer(dialog), DialogMessage::FilterChanged(value)) => {
                dialog.filter = value;
            }
            (MultiDialog::Share(dialog), DialogMessage::RecipientToggled(user_id, checked)) => {
                if checked {
                    dialog.selected.insert(user_id);
                } else {
                    dialog.selected.remove(&user_id);
                }
            }
            (MultiDialog::Transfer(dialog), DialogMessage::RecipientPicked(recipient)) => {
                if !dialog.submitting && dialog.results.is_empty() {
                    dialog.selected = Some(recipient);
                    dialog.destinations.select(recipient);
                    dialog.operations = dialog.targets.iter().map(|_| Uuid::new_v4()).collect();
                }
            }
            (MultiDialog::Transfer(dialog), DialogMessage::TransferFolderPicked(folder)) => {
                if !dialog.submitting && dialog.results.is_empty() {
                    dialog.destinations.folder = Some(folder);
                    dialog.operations = dialog.targets.iter().map(|_| Uuid::new_v4()).collect();
                }
            }
            (MultiDialog::Transfer(dialog), DialogMessage::OwnershipPicked(ownership)) => {
                if !dialog.submitting && dialog.results.is_empty() {
                    dialog.operations = dialog.targets.iter().map(|_| Uuid::new_v4()).collect();
                    dialog.ownership = ownership;
                    if let Some(TransferRecipient::Clan(clan, _)) = dialog.selected {
                        dialog.selected = Some(TransferRecipient::Clan(clan, ownership));
                    }
                }
            }
            (MultiDialog::Share(dialog), DialogMessage::FlagToggled(flag, value)) => match flag {
                modals::GrantFlag::Edit => dialog.can_edit = value,
                modals::GrantFlag::Reshare => dialog.can_reshare = value,
                modals::GrantFlag::Copy => dialog.can_copy = value,
                // Admin is given one map or folder at a time.
                modals::GrantFlag::Admin => {}
            },
            (MultiDialog::Delete(dialog), DialogMessage::LeftoversPicked(leftovers)) => {
                dialog.leftovers = leftovers;
            }
            (MultiDialog::Delete(dialog), DialogMessage::Destination(index, message)) => {
                if let Some(picker) = dialog.destinations.get_mut(index) {
                    picker.update(message);
                }
            }
            (MultiDialog::Delete(dialog), DialogMessage::Submit) => {
                if !dialog.ready() {
                    return Update::none();
                }
                if !dialog.cloud_leftovers().is_empty() {
                    let dialog = Box::new(dialog.clone());
                    return self.review_filing(super::filing::Request::DeleteSelection { dialog });
                }
                dialog.busy = true;
                self.multi.busy = true;
                return delete_submit(dialog, self.mapper.clone(), Vec::new());
            }
            (MultiDialog::Share(dialog), DialogMessage::Submit) => {
                return share_submit(dialog, &self.cloud.client);
            }
            (MultiDialog::Transfer(dialog), DialogMessage::Submit) => {
                return transfer_submit(dialog, &self.cloud.client);
            }
            (MultiDialog::Share(dialog), DialogMessage::Shared(results)) => {
                dialog.submitting = false;
                dialog.results = results;
            }
            (MultiDialog::Transfer(dialog), DialogMessage::Offered(results)) => {
                dialog.submitting = false;
                dialog.results = results;
                self.mapper.sync_now();
            }
            (MultiDialog::Servers(dialog), DialogMessage::ServerToggled(server, show)) => {
                let deltas = server_deltas(&self.map_scopes, &dialog.targets, &server, show);
                for delta in &deltas {
                    self.map_scopes.apply(delta);
                }
                dialog.refresh(&self.map_scopes);
                if !deltas.is_empty() {
                    return Update::with_event(Event::ScopeAssociationsChanged(deltas));
                }
            }
            // A late answer for a dialog that has since closed or changed.
            _ => {}
        }
        Update::none()
    }
}

/// Shares each item with each checked friend; one result line per item.
fn share_submit(
    dialog: &mut ShareDialog,
    client: &smudgy_cloud::CloudApiClient,
) -> Update<Message, Event> {
    if dialog.submitting {
        return Update::none();
    }
    let Some(Ok(friends)) = &dialog.friends else {
        return Update::none();
    };
    let recipients: Vec<(String, Uuid)> = friends
        .iter()
        .filter(|friend| dialog.selected.contains(&friend.user_id))
        .map(|friend| (friend_label(friend), friend.user_id))
        .collect();
    if recipients.is_empty() {
        return Update::none();
    }
    dialog.submitting = true;
    dialog.results.clear();
    let targets = dialog.targets.clone();
    let (can_edit, can_reshare, can_copy) = (dialog.can_edit, dialog.can_reshare, dialog.can_copy);
    let client = client.clone();
    Update::with_task(Task::perform(
        async move {
            let mut outcomes = Vec::with_capacity(targets.len());
            for target in targets {
                let scope = match target.item {
                    ListItem::Map(area_id) => ShareScope::Area { area_id },
                    ListItem::Folder(atlas_id) => ShareScope::Atlas { atlas_id },
                };
                let mut failures = Vec::new();
                for (label, friend) in &recipients {
                    // The owner already has it.
                    if Some(*friend) == target.owner_id {
                        continue;
                    }
                    let request = CreateShareRequest {
                        grantee_id: *friend,
                        scope,
                        can_edit,
                        can_reshare,
                        can_copy,
                        can_admin: false,
                        host_hints: None,
                    };
                    if let Err(error) = client.create_share(request).await {
                        failures.push((label.clone(), error));
                    }
                }
                outcomes.push(ShareOutcome {
                    name: target.name,
                    failures,
                });
            }
            outcomes
        },
        |outcomes| dialog_message(DialogMessage::Shared(outcomes)),
    ))
}

/// Offers each item to a friend or transfers it directly into a clan.
fn transfer_submit(
    dialog: &mut TransferDialog,
    client: &smudgy_cloud::CloudApiClient,
) -> Update<Message, Event> {
    let Some(to) = dialog.selected else {
        return Update::none();
    };
    if dialog.submitting
        || !dialog.results.is_empty()
        || !dialog.destinations.ready(
            to,
            dialog
                .targets
                .iter()
                .any(|target| matches!(target.item, ListItem::Map(_))),
        )
    {
        return Update::none();
    }
    dialog.submitting = true;
    let targets = dialog.targets.clone();
    let operations = dialog.operations.clone();
    let folder = dialog.destinations.folder.as_ref().map(|folder| folder.id);
    let client = client.clone();
    Update::with_task(Task::perform(
        async move {
            let mut results = Vec::with_capacity(targets.len());
            for (target, operation) in targets.into_iter().zip(operations) {
                let result = match (target.item, to) {
                    (ListItem::Map(area), TransferRecipient::Clan(clan, ownership)) => client
                        .transfer_area_to_clan(
                            area,
                            clan,
                            ownership,
                            folder.expect("authorized folder"),
                            operation,
                        )
                        .await
                        .map(|_| ()),
                    (ListItem::Folder(atlas), TransferRecipient::Clan(clan, ownership)) => client
                        .transfer_atlas_to_clan(atlas, clan, ownership, operation)
                        .await
                        .map(|_| ()),
                    (ListItem::Map(area), to) => {
                        client.offer_area_transfer(area, to).await.map(|_| ())
                    }
                    (ListItem::Folder(atlas), to) => {
                        client.offer_atlas_transfer(atlas, to).await.map(|_| ())
                    }
                };
                results.push((
                    target.name,
                    result.map_err(|error| transfer_error(&error, to)),
                ));
            }
            results
        },
        |results| dialog_message(DialogMessage::Offered(results)),
    ))
}

// ===========================================================================
// Views
// ===========================================================================

/// The canvas pane while several items are chosen: what they are, and what
/// can be done to them.
pub fn pane(window: &MapEditorWindow) -> ThemedElement<'_, Message> {
    let state = &window.multi;
    let facts = window.multi_facts();

    let header = row![
        text(heading(Counts::of(&facts))).size(16),
        space::horizontal(),
        button(text(crate::i18n::t!("mapper-multi-clear")).size(12))
            .style(builtins::button::subtle)
            .on_press(multi(MultiMessage::Cleared)),
    ]
    .spacing(8)
    .align_y(Vertical::Center);

    let mut items = Column::new().spacing(2);
    for facts in &facts {
        items = items.push(
            row![
                text(facts.name.clone()).size(13),
                text(kind(facts)).size(11).style(muted),
                space::horizontal(),
                button(text("\u{D7}").size(14))
                    .style(builtins::button::toolbar)
                    .padding([0, 6])
                    .on_press(multi(MultiMessage::Removed(facts.item))),
            ]
            .spacing(8)
            .align_y(Vertical::Center)
            .padding([2, 4]),
        );
    }

    let mut actions = Column::new().spacing(6);
    for action in [
        Action::Move,
        Action::Share,
        Action::Transfer,
        Action::Servers,
        Action::Delete,
    ] {
        let (applies, skipped) = split(action, &facts);
        let counts = Counts::of(applies.iter().copied());
        if action == Action::Delete && state.confirming_delete && !counts.is_empty() {
            actions = actions.push(delete_confirm(counts, &skipped, state.busy));
            continue;
        }
        actions = actions.push(
            button(text(action_label(action, counts)).size(12))
                .style(builtins::button::subtle)
                .padding([5, 10])
                .on_press_maybe(
                    (!state.busy && !counts.is_empty())
                        .then_some(multi(MultiMessage::Requested(action))),
                ),
        );
    }

    let content = column![
        header,
        container(scrollable(items)).max_height(240.0),
        rule::horizontal(1),
        actions,
    ]
    .spacing(14)
    .padding(16)
    .max_width(480);

    container(scrollable(content))
        .width(Length::Fill)
        .height(Length::Fill)
        .style(builtins::container::opaque)
        .into()
}

/// Delete's inline confirm, in the house idiom.
fn delete_confirm<'a>(
    counts: Counts,
    skipped: &[&Facts],
    busy: bool,
) -> ThemedElement<'a, Message> {
    let mut confirm = column![
        text(crate::i18n::t!(
            "mapper-multi-delete-question",
            "items" => object(counts)
        ))
        .size(13)
    ]
    .spacing(8);
    let skipped: Vec<String> = skipped.iter().map(|facts| facts.name.clone()).collect();
    if let Some(line) = skipped_line(&skipped) {
        confirm = confirm.push(text(line).size(12).style(muted));
    }
    confirm = confirm.push(
        row![
            space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(12))
                .style(builtins::button::secondary)
                .on_press(multi(MultiMessage::DeleteCancelled)),
            button(text(crate::i18n::t!("action-delete")).size(12))
                .style(builtins::button::primary)
                .on_press_maybe((!busy).then_some(multi(MultiMessage::DeleteConfirmed))),
        ]
        .spacing(8)
        .align_y(Vertical::Center),
    );
    container(confirm)
        .padding(8)
        .style(builtins::container::card)
        .into()
}

impl MultiDialog {
    pub fn title(&self) -> String {
        match self {
            Self::Delete(dialog) => crate::i18n::t!(
                "mapper-multi-delete-title",
                "items" => object(dialog.counts)
            ),
            Self::Move(_) => crate::i18n::t!("mapper-move-to-folder"),
            Self::Share(dialog) => crate::i18n::t!(
                "mapper-multi-share-title",
                "items" => object(dialog.counts)
            ),
            Self::Transfer(dialog) => crate::i18n::t!(
                "mapper-multi-transfer-title",
                "items" => object(dialog.counts)
            ),
            Self::Servers(_) => crate::i18n::t!("mapper-show-on-servers"),
        }
    }

    pub fn width(&self) -> f32 {
        match self {
            Self::Share(_) | Self::Transfer(_) => 460.0,
            Self::Delete(_) => 440.0,
            Self::Move(_) | Self::Servers(_) => 380.0,
        }
    }

    pub fn view(&self) -> ThemedElement<'_, Message> {
        match self {
            Self::Delete(dialog) => delete_view(dialog),
            Self::Move(dialog) => move_view(dialog),
            Self::Share(dialog) => share_view(dialog),
            Self::Transfer(dialog) => transfer_view(dialog),
            Self::Servers(dialog) => servers_view(dialog),
        }
    }
}

fn push_skipped<'a>(
    body: Column<'a, Message, crate::Theme>,
    skipped: &[String],
) -> Column<'a, Message, crate::Theme> {
    match skipped_line(skipped) {
        Some(line) => body.push(text(line).size(11).style(muted)),
        None => body,
    }
}

fn delete_view(dialog: &DeleteDialog) -> ThemedElement<'_, Message> {
    let mut body = column![
        text(crate::i18n::t!(
            "mapper-multi-delete-question",
            "items" => object(dialog.counts)
        ))
        .size(13)
    ]
    .spacing(10);
    for held in &dialog.holding {
        body = body.push(
            text(crate::i18n::t!(
                "mapper-multi-delete-holding",
                "folder" => held.name.clone(),
                "count" => held.maps.len(),
                "names" => name_list(held.maps.iter().map(|(_, name)| name.as_str()))
            ))
            .size(12),
        );
    }
    let pick = |leftovers| dialog_message(DialogMessage::LeftoversPicked(leftovers));
    body = body.push(
        radio(
            crate::i18n::t!("mapper-multi-leftovers-move"),
            Leftovers::Move,
            Some(dialog.leftovers),
            pick,
        )
        .size(14)
        .text_size(13),
    );
    if dialog.leftovers == Leftovers::Move {
        let several = dialog.destinations.len() > 1;
        for (index, picker) in dialog.destinations.iter().enumerate() {
            let mut field = column![].spacing(4);
            if several {
                field = field.push(
                    text(match picker.storage() {
                        MapStorage::Local => crate::i18n::t!("mapper-save-local"),
                        _ => crate::i18n::t!("mapper-save-cloud"),
                    })
                    .size(12)
                    .style(muted),
                );
            }
            let view = picker
                .view(dialog_message(DialogMessage::Submit))
                .map(move |message| match message {
                    Message::FolderPicker(message) => {
                        dialog_message(DialogMessage::Destination(index, message))
                    }
                    other => other,
                });
            body = body.push(container(field.push(view)).padding(Padding {
                left: 24.0,
                ..Padding::ZERO
            }));
        }
    }
    body = body.push(
        radio(
            crate::i18n::t!("mapper-multi-leftovers-delete"),
            Leftovers::Delete,
            Some(dialog.leftovers),
            pick,
        )
        .size(14)
        .text_size(13),
    );
    body = push_skipped(body, &dialog.skipped);
    let footer = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-cancel")).size(13))
            .style(builtins::button::secondary)
            .on_press_maybe((!dialog.busy).then_some(Message::ModalDismissed)),
        button(text(crate::i18n::t!("action-delete")).size(13))
            .style(builtins::button::primary)
            .on_press_maybe(
                dialog
                    .ready()
                    .then_some(dialog_message(DialogMessage::Submit))
            ),
    ]
    .spacing(10)
    .align_y(Vertical::Center);
    scrolling_body(body, footer)
}

/// Deletes what the dialog names: folders made for the leftovers first, then
/// the leftovers moved (or deleted), the chosen maps, and last the folders. A
/// folder whose leftovers didn't all move stays.
pub(super) fn delete_submit(
    dialog: &DeleteDialog,
    mapper: Mapper,
    reviews: Vec<ReviewedFiling>,
) -> Update<Message, Event> {
    let maps = dialog.maps.clone();
    let folders = dialog.folders.clone();
    let holding = dialog.holding.clone();
    let leftovers = dialog.leftovers;
    let destinations: Vec<(MapStorage, Option<AtlasId>, Option<String>)> = dialog
        .destinations
        .iter()
        .map(|picker| (picker.storage(), picker.chosen(), picker.new_folder_name()))
        .collect();
    Update::with_task(Task::perform(
        async move {
            let cloud_target = reviews.first().and_then(ReviewedFiling::atlas_id);
            let mut reviews: std::collections::HashMap<_, _> = reviews
                .into_iter()
                .map(|review| (review.area_id(), review))
                .collect();
            let mut report = DeleteReport::default();
            if leftovers == Leftovers::Move {
                for held in holding
                    .iter()
                    .filter(|held| held.storage == MapStorage::Cloud)
                {
                    for (area_id, name) in &held.maps {
                        if cloud_target.is_none()
                            || !reviews
                                .get(area_id)
                                .is_some_and(|review| review.atlas_id() == cloud_target)
                        {
                            report.failures.push((
                                name.clone(),
                                "Missing filing review; nothing was deleted".into(),
                            ));
                        }
                    }
                }
                if !report.failures.is_empty() {
                    return report;
                }
            }
            let mut kept: HashSet<AtlasId> = HashSet::new();
            let mut doomed: Vec<(AreaId, String)> = maps;
            if leftovers == Leftovers::Move {
                // Each storage's folder, made first when it is a new one.
                let mut targets: Vec<(MapStorage, Option<AtlasId>)> = Vec::new();
                for (storage, chosen, new_name) in destinations {
                    if storage == MapStorage::Cloud {
                        targets.push((storage, cloud_target));
                        continue;
                    }
                    let target = match (chosen, new_name) {
                        (Some(atlas_id), _) => Some(atlas_id),
                        (None, Some(name)) => {
                            match mapper.create_atlas_at(name.clone(), storage).await {
                                Ok(atlas) => {
                                    report.made.push(atlas.id);
                                    Some(atlas.id)
                                }
                                Err(error) => {
                                    report.failures.push((name, display_error(&error)));
                                    None
                                }
                            }
                        }
                        (None, None) => None,
                    };
                    targets.push((storage, target));
                }
                for held in &holding {
                    let target = targets
                        .iter()
                        .find(|(storage, _)| *storage == held.storage)
                        .and_then(|(_, target)| *target);
                    let Some(target) = target else {
                        kept.insert(held.atlas_id);
                        continue;
                    };
                    for (area_id, name) in &held.maps {
                        let moved = if held.storage == MapStorage::Cloud {
                            match reviews
                                .remove(area_id)
                                .filter(|review| review.atlas_id() == Some(target))
                            {
                                Some(review) => mapper.commit_reviewed_filing(review).await,
                                None => {
                                    Err(CloudError::InvalidInput("Missing filing review".into()))
                                }
                            }
                        } else {
                            mapper.move_area_to_atlas(*area_id, Some(target)).await
                        };
                        if let Err(error) = moved {
                            report.failures.push((name.clone(), display_error(&error)));
                            kept.insert(held.atlas_id);
                        }
                    }
                }
            } else {
                doomed.extend(holding.iter().flat_map(|held| held.maps.iter().cloned()));
            }
            for (area_id, name) in doomed {
                match mapper.delete_area(area_id).await {
                    Ok(()) => report.maps.push(area_id),
                    Err(error) => report.failures.push((name, display_error(&error))),
                }
            }
            for (atlas_id, name) in folders {
                if kept.contains(&atlas_id) {
                    continue;
                }
                match mapper.delete_atlas(atlas_id).await {
                    Ok(()) => report.folders.push(atlas_id),
                    Err(error) => report.failures.push((name, display_error(&error))),
                }
            }
            report
        },
        |report| multi(MultiMessage::Deleted(report)),
    ))
}

fn move_view(dialog: &MoveDialog) -> ThemedElement<'_, Message> {
    // A destination every map is already in moves nothing.
    let common = dialog
        .maps
        .first()
        .map(|(_, current)| *current)
        .filter(|current| dialog.maps.iter().all(|(_, other)| other == current));
    let mut list = column![
        text(crate::i18n::t!(
            "mapper-multi-move-to",
            "items" => object(dialog.counts)
        ))
        .size(13)
    ]
    .spacing(6);
    list = push_skipped(list, &dialog.skipped);
    for (destination, label) in &dialog.targets {
        list = list.push(modals::move_target_button(
            label.clone(),
            common == Some(*destination),
            multi(MultiMessage::MoveTo(*destination)),
        ));
    }
    list = list.push(match &dialog.new_folder {
        Some(form) => modals::new_folder_form(form, dialog.targets.is_empty(), false, |message| {
            multi(MultiMessage::NewFolder(message))
        }),
        None => button(text(crate::i18n::t!("mapper-folder-new-option")).size(13))
            .style(builtins::button::link)
            .on_press(multi(MultiMessage::NewFolder(NewFolderMessage::Opened)))
            .into(),
    });
    let footer = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-cancel")).size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
    ]
    .align_y(Vertical::Center);
    scrolling_body(list, footer)
}

/// The friends list, filtered; `row` draws one friend.
fn friends_list<'a>(
    friends: Option<&'a Result<Vec<FriendView>, String>>,
    filter: &str,
    empty: String,
    row: impl Fn(&'a FriendView, String) -> ThemedElement<'a, Message>,
) -> ThemedElement<'a, Message> {
    let mut list = Column::new().spacing(2);
    match friends {
        None => {
            list = list.push(
                text(crate::i18n::t!("mapper-loading-friends"))
                    .size(12)
                    .style(muted),
            );
        }
        Some(Err(error)) => {
            list = list.push(text(error.clone()).size(12).style(builtins::text::danger));
        }
        Some(Ok(friends)) if friends.is_empty() => {
            list = list.push(text(empty).size(12).style(muted));
        }
        Some(Ok(friends)) => {
            let filter = filter.trim().to_lowercase();
            let mut any = false;
            for friend in friends {
                let label = friend_label(friend);
                if !filter.is_empty() && !label.to_lowercase().contains(&filter) {
                    continue;
                }
                any = true;
                list = list.push(row(friend, label));
            }
            if !any {
                list = list.push(
                    text(crate::i18n::t!("mapper-no-friends-filter"))
                        .size(12)
                        .style(muted),
                );
            }
        }
    }
    container(scrollable(list))
        .max_height(160.0)
        .width(Length::Fill)
        .into()
}

fn section_label<'a>(label: String) -> iced::widget::Text<'a, crate::Theme> {
    text(label).size(11).style(muted)
}

fn share_view(dialog: &ShareDialog) -> ThemedElement<'_, Message> {
    let selected = &dialog.selected;
    let mut body = column![
        section_label(crate::i18n::t!("mapper-recipients")),
        text_input(
            crate::i18n::ts!("mapper-filter-handle-placeholder"),
            &dialog.filter
        )
        .size(13)
        .on_input(|value| dialog_message(DialogMessage::FilterChanged(value))),
        friends_list(
            dialog.friends.as_ref(),
            &dialog.filter,
            crate::i18n::t!("mapper-no-friends-share"),
            |friend, label| {
                let user_id = friend.user_id;
                checkbox(selected.contains(&user_id))
                    .label(label)
                    .size(14)
                    .text_size(13)
                    .on_toggle(move |checked| {
                        dialog_message(DialogMessage::RecipientToggled(user_id, checked))
                    })
                    .into()
            },
        ),
        section_label(crate::i18n::t!("mapper-they-can")),
        checkbox(dialog.can_edit)
            .label(crate::i18n::t!("mapper-can-edit-area"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| dialog_message(DialogMessage::FlagToggled(
                modals::GrantFlag::Edit,
                value
            ))),
        checkbox(dialog.can_reshare)
            .label(crate::i18n::t!("mapper-can-reshare"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| dialog_message(DialogMessage::FlagToggled(
                modals::GrantFlag::Reshare,
                value
            ))),
        checkbox(dialog.can_copy)
            .label(crate::i18n::t!("mapper-can-copy-area"))
            .size(14)
            .text_size(13)
            .on_toggle(|value| dialog_message(DialogMessage::FlagToggled(
                modals::GrantFlag::Copy,
                value
            ))),
    ]
    .spacing(6);
    body = push_skipped(body, &dialog.skipped);

    if !dialog.results.is_empty() {
        let mut results = Column::new().spacing(2);
        for outcome in &dialog.results {
            if outcome.failures.is_empty() {
                results = results.push(
                    text(crate::i18n::t!("mapper-multi-shared", "name" => outcome.name.as_str()))
                        .size(12)
                        .style(builtins::text::success),
                );
            }
            for (recipient, error) in &outcome.failures {
                let line = match error {
                    CloudError::NotFoundOrNoAccess => crate::i18n::t!(
                        "mapper-multi-share-failed",
                        "name" => outcome.name.as_str(),
                        "recipient" => recipient.as_str()
                    ),
                    other => crate::i18n::t!(
                        "mapper-multi-share-error",
                        "name" => outcome.name.as_str(),
                        "recipient" => recipient.as_str(),
                        "error" => display_error(other)
                    ),
                };
                results = results.push(text(line).size(12).style(builtins::text::danger));
            }
        }
        body = body.push(results);
    }

    let can_share = !dialog.submitting && !dialog.selected.is_empty();
    let buttons = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-close")).size(13))
            .style(builtins::button::secondary)
            .on_press(Message::ModalDismissed),
        button(
            text(if dialog.submitting {
                crate::i18n::t!("mapper-sharing")
            } else {
                crate::i18n::t!("mapper-share")
            })
            .size(13)
        )
        .style(builtins::button::primary)
        .on_press_maybe(can_share.then_some(dialog_message(DialogMessage::Submit))),
    ]
    .spacing(10)
    .align_y(Vertical::Center);

    scrolling_body(body, buttons)
}

/// A friend or clan to transfer to: primary while picked, fixed once sent.
fn recipient_button(
    label: String,
    recipient: TransferRecipient,
    selected: Option<TransferRecipient>,
    sent: bool,
) -> ThemedElement<'static, Message> {
    button(text(label).size(13))
        .style(if selected == Some(recipient) {
            builtins::button::primary
        } else {
            builtins::button::secondary
        })
        .width(Length::Fill)
        .on_press_maybe(
            (!sent).then_some(dialog_message(DialogMessage::RecipientPicked(recipient))),
        )
        .into()
}

fn transfer_view(dialog: &TransferDialog) -> ThemedElement<'_, Message> {
    let sent = !dialog.results.is_empty();
    let maps = dialog
        .targets
        .iter()
        .any(|target| matches!(target.item, ListItem::Map(_)));
    let to_clan = matches!(dialog.selected, Some(TransferRecipient::Clan(..)));
    let mut body = column![
        text(if to_clan {
            crate::i18n::t!("mapper-transfer-warning-clan")
        } else {
            crate::i18n::t!("mapper-transfer-warning")
        })
        .size(12)
        .style(builtins::text::danger),
    ]
    .spacing(8);
    if dialog
        .targets
        .iter()
        .any(|target| matches!(target.item, ListItem::Map(_)) && target.filed)
    {
        body = body.push(
            text(crate::i18n::t!("mapper-multi-transfer-leaves-folder"))
                .size(11)
                .style(muted),
        );
    }
    body = push_skipped(body, &dialog.skipped);
    body = body.push(section_label(crate::i18n::t!("mapper-transfer-to")));
    body = body.push(
        text_input(
            crate::i18n::ts!("mapper-filter-placeholder"),
            &dialog.filter,
        )
        .size(13)
        .on_input(|value| dialog_message(DialogMessage::FilterChanged(value))),
    );
    body = body.push(friends_list(
        dialog.friends.as_ref(),
        &dialog.filter,
        crate::i18n::t!("mapper-no-friends-transfer"),
        |friend, label| {
            recipient_button(
                label,
                TransferRecipient::User(friend.user_id),
                dialog.selected,
                sent,
            )
        },
    ));
    if !dialog.destinations.clans.is_empty() {
        body = body.push(section_label(crate::i18n::t!("mapper-transfer-clans")));
        let filter = dialog.filter.trim().to_lowercase();
        let mut clans = column![].spacing(4);
        for (id, name) in &dialog.destinations.clans {
            if filter.is_empty() || name.to_lowercase().contains(&filter) {
                clans = clans.push(recipient_button(
                    name.clone(),
                    TransferRecipient::Clan(*id, dialog.ownership),
                    dialog.selected,
                    sent,
                ));
            }
        }
        body = body.push(clans);
        if to_clan && !sent {
            let only_maps = dialog
                .targets
                .iter()
                .all(|target| matches!(target.item, ListItem::Map(_)));
            body = body.push(modals::ownership_choice(
                dialog.ownership,
                only_maps,
                |ownership| dialog_message(DialogMessage::OwnershipPicked(ownership)),
            ));
        }
    }

    if maps
        && !sent
        && let Some(TransferRecipient::Clan(clan, _)) = dialog.selected
    {
        body = body.push(modals::clan_transfer_folder(
            &dialog.destinations,
            clan,
            |folder| dialog_message(DialogMessage::TransferFolderPicked(folder)),
        ));
    }
    if sent {
        let mut results = Column::new().spacing(2);
        for (name, result) in &dialog.results {
            results = results.push(match result {
                Ok(()) => text(if to_clan {
                    crate::i18n::t!("mapper-multi-transferred", "name" => name.as_str())
                } else {
                    crate::i18n::t!("mapper-multi-offer-sent", "name" => name.as_str())
                })
                .size(12)
                .style(builtins::text::success),
                Err(error) => text(crate::i18n::t!(
                    "mapper-multi-offer-error",
                    "name" => name.as_str(),
                    "error" => error.as_str()
                ))
                .size(12)
                .style(builtins::text::danger),
            });
        }
        body = body.push(results);
    }

    let mut buttons = row![space::horizontal()]
        .spacing(10)
        .align_y(Vertical::Center);
    if sent {
        buttons = buttons.push(
            button(text(crate::i18n::t!("action-close")).size(13))
                .style(builtins::button::primary)
                .on_press(Message::ModalDismissed),
        );
    } else {
        buttons = buttons.push(
            button(text(crate::i18n::t!("action-cancel")).size(13))
                .style(builtins::button::secondary)
                .on_press(Message::ModalDismissed),
        );
        buttons = buttons.push(
            button(
                text(if dialog.submitting {
                    crate::i18n::t!("mapper-sending")
                } else if to_clan {
                    crate::i18n::t!("clan-share-put-action")
                } else {
                    crate::i18n::t!("mapper-multi-send-offers")
                })
                .size(13),
            )
            .style(builtins::button::primary)
            .on_press_maybe(
                (dialog
                    .selected
                    .is_some_and(|to| dialog.destinations.ready(to, maps))
                    && !dialog.submitting)
                    .then_some(dialog_message(DialogMessage::Submit)),
            ),
        );
    }

    scrolling_body(body, buttons)
}

fn servers_view(dialog: &ServersDialog) -> ThemedElement<'_, Message> {
    let mut list = column![
        text(crate::i18n::t!(
            "mapper-multi-show-on",
            "items" => object(dialog.counts)
        ))
        .size(13)
    ]
    .spacing(6);
    list = push_skipped(list, &dialog.skipped);
    if dialog.servers.is_empty() {
        list = list.push(
            text(crate::i18n::t!("mapper-no-server-entries"))
                .size(12)
                .style(muted),
        );
    } else {
        for (server, state) in &dialog.servers {
            let entry = server.clone();
            let mut line = row![
                checkbox(*state == ServerState::All)
                    .label(server.clone())
                    .size(14)
                    .text_size(13)
                    .on_toggle(move |show| {
                        dialog_message(DialogMessage::ServerToggled(entry.clone(), show))
                    })
            ]
            .spacing(8)
            .align_y(Vertical::Center);
            if *state == ServerState::Mixed {
                line = line.push(
                    text(crate::i18n::t!("mapper-multi-some"))
                        .size(11)
                        .style(muted),
                );
            }
            list = list.push(line);
        }
        list = list.push(
            text(crate::i18n::t!("mapper-unchecked-all-servers"))
                .size(11)
                .style(muted),
        );
    }
    let footer = row![
        space::horizontal(),
        button(text(crate::i18n::t!("action-done")).size(13))
            .style(builtins::button::primary)
            .on_press(Message::ModalDismissed),
    ]
    .align_y(Vertical::Center);
    scrolling_body(list, footer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bulk_dialog_footers_survive_long_lists() {
        let counts = Counts {
            maps: 2,
            folders: 0,
        };
        let skipped: Vec<_> = (0..40)
            .map(|n| format!("Uneditable map {n} with a long name"))
            .collect();
        let target = Target {
            item: map(1),
            name: "Grove".into(),
            owner_id: None,
            filed: true,
        };
        let dialogs = [
            (
                MultiDialog::Delete(DeleteDialog {
                    counts,
                    skipped: skipped.clone(),
                    maps: vec![],
                    folders: vec![],
                    holding: vec![],
                    leftovers: Leftovers::Delete,
                    destinations: vec![],
                    busy: false,
                }),
                "action-delete",
                "bulk-delete",
            ),
            (
                MultiDialog::Move(MoveDialog {
                    counts,
                    skipped: skipped.clone(),
                    maps: vec![],
                    targets: vec![],
                    new_folder: None,
                }),
                "action-cancel",
                "bulk-move",
            ),
            (
                MultiDialog::Share(ShareDialog {
                    counts,
                    skipped: skipped.clone(),
                    targets: vec![target.clone()],
                    friends: Some(Ok(vec![])),
                    filter: String::new(),
                    selected: [Uuid::from_u128(2)].into(),
                    can_edit: false,
                    can_reshare: false,
                    can_copy: false,
                    submitting: false,
                    results: vec![],
                }),
                "mapper-share",
                "bulk-share",
            ),
            (
                MultiDialog::Transfer(TransferDialog {
                    counts,
                    skipped: skipped.clone(),
                    targets: vec![target],
                    friends: Some(Ok(vec![])),
                    destinations: Default::default(),
                    operations: vec![Uuid::new_v4()],
                    filter: String::new(),
                    selected: Some(TransferRecipient::User(Uuid::from_u128(2))),
                    ownership: smudgy_cloud::clan_maps::MapOwnership::Clan,
                    submitting: false,
                    results: vec![],
                }),
                "mapper-multi-send-offers",
                "bulk-transfer",
            ),
            (
                MultiDialog::Servers(ServersDialog {
                    counts,
                    skipped,
                    targets: vec![],
                    servers: (0..50)
                        .map(|n| (format!("Server {n}"), ServerState::Off))
                        .collect(),
                }),
                "action-done",
                "bulk-servers",
            ),
        ];
        for (dialog, label, name) in dialogs {
            let modal = modals::Modal::Multi(Box::new(dialog));
            for size in [(380, 360), (600, 500)] {
                let messages = crate::widgets::dialog::tests::check_actions(
                    modal.view(),
                    size,
                    &[crate::i18n::translate(label)],
                    name,
                )
                .await;
                assert_eq!(messages.len(), 2, "{name}: both clicks reach the footer");
                assert!(messages.iter().all(|m| matches!(
                    m,
                    Message::ModalDismissed
                        | Message::Multi(MultiMessage::Dialog(DialogMessage::Submit))
                )));
            }
        }
    }

    fn map(n: u128) -> ListItem {
        ListItem::Map(AreaId(Uuid::from_u128(n)))
    }

    fn folder(n: u128) -> ListItem {
        ListItem::Folder(AtlasId(Uuid::from_u128(n)))
    }

    fn area(n: u128) -> AreaId {
        AreaId(Uuid::from_u128(n))
    }

    fn plain() -> keyboard::Modifiers {
        keyboard::Modifiers::default()
    }

    fn ctrl() -> keyboard::Modifiers {
        keyboard::Modifiers::COMMAND
    }

    fn shift() -> keyboard::Modifiers {
        keyboard::Modifiers::SHIFT
    }

    /// The list as drawn: folder 10 with maps 1-2, folder 20 with map 3,
    /// then map 4.
    fn order() -> Vec<ListItem> {
        vec![folder(10), map(1), map(2), folder(20), map(3), map(4)]
    }

    #[test]
    fn a_plain_click_opens_one_map_as_today() {
        let mut selection = Selection::default();
        let pressed = selection.press(map(2), plain(), Some(area(1)), &order());
        assert_eq!(pressed, Pressed::Open(area(2)));
        assert!(!selection.is_chosen());
        assert!(selection.contains(map(2), Some(area(2))));
        assert!(!selection.contains(map(1), Some(area(2))));
    }

    #[test]
    fn ctrl_click_adds_and_removes_rows_from_the_open_map() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        assert_eq!(
            selection.press(map(3), ctrl(), open, &order()),
            Pressed::Nothing
        );
        assert_eq!(selection.items(open), vec![map(1), map(3)]);
        assert!(selection.is_multi());

        selection.press(folder(20), ctrl(), open, &order());
        assert_eq!(selection.items(open), vec![map(1), map(3), folder(20)]);

        // Ctrl+click again takes a row out.
        selection.press(map(1), ctrl(), open, &order());
        assert_eq!(selection.items(open), vec![map(3), folder(20)]);
        assert!(selection.contains(folder(20), open));
        assert!(!selection.contains(map(1), open));
    }

    #[test]
    fn ctrl_click_down_to_one_map_shows_that_map() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        selection.press(map(3), ctrl(), open, &order());
        assert_eq!(
            selection.press(map(1), ctrl(), open, &order()),
            Pressed::Open(area(3))
        );
        assert!(!selection.is_chosen());
    }

    #[test]
    fn a_lone_folder_stays_chosen_without_the_pane() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        selection.press(folder(10), ctrl(), open, &order());
        assert_eq!(
            selection.press(map(1), ctrl(), open, &order()),
            Pressed::Folder(AtlasId(Uuid::from_u128(10)))
        );
        assert_eq!(selection.items(open), vec![folder(10)]);
        assert!(selection.is_chosen());
        assert!(!selection.is_multi());
        assert_eq!(selection.folder(), Some(AtlasId(Uuid::from_u128(10))));
    }

    #[test]
    fn shift_click_takes_the_visible_range_from_the_last_click() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        // The anchor is the open map until a row is pressed.
        selection.press(map(3), shift(), open, &order());
        assert_eq!(
            selection.items(open),
            vec![map(1), map(2), folder(20), map(3)]
        );

        // A plain click moves the anchor; Shift+click upward ranges back.
        selection.press(map(4), plain(), open, &order());
        selection.press(map(2), shift(), Some(area(4)), &order());
        assert_eq!(
            selection.items(Some(area(4))),
            vec![map(2), folder(20), map(3), map(4)]
        );

        // Another Shift+click re-ranges from the same anchor, replacing.
        selection.press(map(3), shift(), Some(area(4)), &order());
        assert_eq!(selection.items(Some(area(4))), vec![map(3), map(4)]);
    }

    #[test]
    fn ctrl_shift_click_adds_the_range() {
        let mut selection = Selection::default();
        let open = Some(area(4));
        selection.press(map(1), ctrl(), open, &order());
        let both = keyboard::Modifiers::COMMAND | keyboard::Modifiers::SHIFT;
        selection.press(folder(20), both, open, &order());
        assert_eq!(
            selection.items(open),
            vec![map(4), map(1), map(2), folder(20)]
        );
    }

    #[test]
    fn shift_click_with_the_anchor_hidden_takes_just_the_row() {
        let mut selection = Selection::default();
        let open = Some(area(9));
        // Map 9 is in a closed folder: not in the drawn order.
        assert_eq!(
            selection.press(map(3), shift(), open, &order()),
            Pressed::Open(area(3))
        );
    }

    #[test]
    fn a_plain_click_on_a_folder_chooses_it_alone() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        assert_eq!(
            selection.press(folder(20), plain(), open, &order()),
            Pressed::Folder(AtlasId(Uuid::from_u128(20)))
        );
        assert_eq!(selection.folder(), Some(AtlasId(Uuid::from_u128(20))));
        assert!(selection.contains(folder(20), open));
        // The open map stays open, but its row isn't the chosen one.
        assert!(!selection.contains(map(1), open));
        assert!(!selection.is_multi());

        // From several chosen rows, a plain click takes the folder alone.
        selection.press(map(3), ctrl(), open, &order());
        assert!(selection.is_multi());
        selection.press(folder(10), plain(), open, &order());
        assert_eq!(selection.items(open), vec![folder(10)]);
        assert_eq!(selection.folder(), Some(AtlasId(Uuid::from_u128(10))));
    }

    #[test]
    fn a_map_click_or_esc_lets_go_of_a_chosen_folder() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        selection.press(folder(20), plain(), open, &order());
        assert_eq!(
            selection.press(map(3), plain(), open, &order()),
            Pressed::Open(area(3))
        );
        assert_eq!(selection.folder(), None);
        assert!(!selection.is_chosen());

        selection.press(folder(20), plain(), Some(area(3)), &order());
        selection.clear();
        assert_eq!(selection.folder(), None);
    }

    #[test]
    fn a_canvas_selection_drops_a_lone_folder_but_not_several_rows() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        selection.press(folder(20), plain(), open, &order());
        assert!(selection.drop_folder());
        assert!(!selection.is_chosen());

        selection.press(folder(20), ctrl(), open, &order());
        assert!(selection.is_multi());
        assert!(!selection.drop_folder());
        assert_eq!(selection.items(open), vec![map(1), folder(20)]);
    }

    #[test]
    fn ctrl_and_shift_click_build_on_a_chosen_folder() {
        let mut selection = Selection::default();
        let open = Some(area(4));
        selection.press(folder(10), plain(), open, &order());
        // Ctrl+click adds to the folder, not to the open map.
        selection.press(map(3), ctrl(), open, &order());
        assert_eq!(selection.items(open), vec![folder(10), map(3)]);

        // The folder pressed last anchors a Shift range.
        let mut selection = Selection::default();
        selection.press(folder(10), plain(), open, &order());
        selection.press(map(2), shift(), open, &order());
        assert_eq!(selection.items(open), vec![folder(10), map(1), map(2)]);

        // Ctrl+click on the chosen folder goes back to the open map alone.
        let mut selection = Selection::default();
        selection.press(folder(10), plain(), open, &order());
        assert_eq!(
            selection.press(folder(10), ctrl(), open, &order()),
            Pressed::Nothing
        );
        assert!(!selection.is_chosen());
    }

    #[test]
    fn a_plain_click_on_a_map_goes_back_to_one() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        selection.press(map(3), ctrl(), open, &order());
        selection.press(map(4), ctrl(), open, &order());
        assert_eq!(
            selection.press(map(2), plain(), open, &order()),
            Pressed::Open(area(2))
        );
        assert!(!selection.is_chosen());
    }

    #[test]
    fn esc_clears_back_to_the_open_map() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        selection.press(map(3), ctrl(), open, &order());
        assert!(selection.is_chosen());
        selection.clear();
        assert!(!selection.is_chosen());
        assert_eq!(selection.items(open), vec![map(1)]);
    }

    #[test]
    fn removing_and_deleting_settle_to_the_single_view() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        selection.press(map(3), ctrl(), open, &order());
        selection.press(map(4), ctrl(), open, &order());
        assert_eq!(selection.remove(map(1), open), Pressed::Nothing);
        assert!(selection.is_multi());
        // Deleting map 3 leaves map 4 alone: show it.
        assert_eq!(
            selection.retain(|item| item != map(3), open),
            Pressed::Open(area(4))
        );
        assert!(!selection.is_chosen());
    }

    #[test]
    fn a_moved_map_keeps_its_place_in_the_selection() {
        let mut selection = Selection::default();
        let open = Some(area(1));
        selection.press(map(3), ctrl(), open, &order());
        selection.replace_map(area(3), area(30));
        assert_eq!(selection.items(open), vec![map(1), map(30)]);
    }

    fn facts(item: ListItem, storage: MapStorage, owned: bool) -> Facts {
        Facts {
            item,
            name: format!("{item:?}"),
            storage,
            owned,
            is_owner: owned,
            clan_owned: false,
            may_share: owned,
            atlas_id: None,
            owner_id: None,
            keeps_maps: false,
        }
    }

    #[test]
    fn deleting_waits_for_a_folder_to_move_the_rest_into() {
        let mut dialog = DeleteDialog {
            counts: Counts {
                maps: 0,
                folders: 1,
            },
            skipped: Vec::new(),
            maps: Vec::new(),
            folders: vec![(AtlasId(Uuid::from_u128(1)), "Roads".to_string())],
            holding: Vec::new(),
            leftovers: Leftovers::Move,
            // No other folder: a new one must be named first.
            destinations: vec![FolderPicker::in_storage(
                Vec::new(),
                MapStorage::Cloud,
                None,
            )],
            busy: false,
        };
        assert!(!dialog.ready());
        dialog.destinations[0].update(PickerMessage::NewName("Elsewhere".to_string()));
        assert!(dialog.ready());
        dialog.destinations[0].update(PickerMessage::NewName(String::new()));
        dialog.leftovers = Leftovers::Delete;
        assert!(dialog.ready(), "deleting them too needs no folder");
        dialog.busy = true;
        assert!(!dialog.ready());
    }

    #[tokio::test]
    async fn deleting_with_cloud_leftovers_reviews_before_deleting_anything() {
        use super::super::links::fixture::{KEEP, area, maps};
        use futures::StreamExt;

        let mapper = maps().await;
        let mut window = super::super::test_window(mapper.clone(), area(KEEP));
        let folder = AtlasId(Uuid::new_v4());
        let mut picker = FolderPicker::in_storage(Vec::new(), MapStorage::Cloud, None);
        picker.update(PickerMessage::NewName("Elsewhere".into()));
        let dialog = DeleteDialog {
            counts: Counts {
                maps: 1,
                folders: 1,
            },
            skipped: Vec::new(),
            maps: vec![(area(KEEP), "Selected map".into())],
            folders: vec![(folder, "Selected folder".into())],
            holding: vec![Holding {
                atlas_id: folder,
                name: "Selected folder".into(),
                storage: MapStorage::Cloud,
                maps: vec![(AreaId(Uuid::new_v4()), "Leftover map".into())],
            }],
            leftovers: Leftovers::Move,
            destinations: vec![picker],
            busy: false,
        };
        window.modal = Some(modals::Modal::Multi(Box::new(MultiDialog::Delete(
            dialog.clone(),
        ))));
        // Do not run the prepare task: cancellation during loading must be safe.
        let _ = window.update_multi_dialog(DialogMessage::Submit);
        assert!(matches!(window.modal, Some(modals::Modal::ReviewFiling(_))));
        assert!(!window.multi.busy);
        let _ = window.update(Message::ModalDismissed);
        assert!(window.modal.is_none());
        assert!(mapper.get_current_atlas().get_area(&area(KEEP)).is_some());

        // An alternate caller cannot omit reviews and still delete the explicitly
        // selected map before discovering the missing leftover-map review.
        let update = delete_submit(&dialog, mapper.clone(), Vec::new());
        let mut task = iced_runtime::task::into_stream(update.task).expect("delete task");
        let message = task.next().await.expect("delete result");
        let iced_runtime::Action::Output(Message::Multi(MultiMessage::Deleted(report))) = message
        else {
            panic!("expected a deletion report");
        };
        assert!(report.maps.is_empty() && report.folders.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert!(mapper.get_current_atlas().get_area(&area(KEEP)).is_some());
    }

    #[test]
    fn a_chosen_folder_is_deleted_whatever_it_holds() {
        let mut holding = facts(folder(1), MapStorage::Cloud, true);
        assert!(Action::Delete.applies(&holding));
        holding.keeps_maps = true;
        assert!(
            Action::Delete.applies(&holding),
            "the dialog asks about its maps"
        );
        assert!(!Action::Delete.applies(&facts(folder(2), MapStorage::Cloud, false)));
    }

    #[test]
    fn each_action_applies_where_the_single_menus_offer_it() {
        let own_cloud_map = facts(map(1), MapStorage::Cloud, true);
        let mut filed_map = facts(map(2), MapStorage::Cloud, true);
        filed_map.atlas_id = Some(AtlasId(Uuid::from_u128(10)));
        let local_map = facts(map(3), MapStorage::Local, true);
        let session_map = facts(map(4), MapStorage::Session, true);
        let shared_map = facts(map(5), MapStorage::Cloud, false);
        let mut reshareable = facts(map(6), MapStorage::Cloud, false);
        reshareable.may_share = true;
        let own_folder = facts(folder(10), MapStorage::Cloud, true);
        let local_folder = facts(folder(11), MapStorage::Local, true);
        let shared_folder = facts(folder(12), MapStorage::Cloud, false);
        let mut admin_folder = facts(folder(13), MapStorage::Cloud, true);
        admin_folder.is_owner = false;

        // Move: maps the viewer owns, not session maps; never folders.
        assert!(Action::Move.applies(&own_cloud_map));
        assert!(Action::Move.applies(&local_map));
        assert!(!Action::Move.applies(&session_map));
        assert!(!Action::Move.applies(&shared_map));
        assert!(!Action::Move.applies(&own_folder));

        // Share: cloud maps the viewer may share, the viewer's cloud folders.
        assert!(Action::Share.applies(&own_cloud_map));
        assert!(Action::Share.applies(&reshareable));
        assert!(!Action::Share.applies(&shared_map));
        assert!(!Action::Share.applies(&local_map));
        assert!(Action::Share.applies(&own_folder));
        assert!(!Action::Share.applies(&local_folder));
        assert!(!Action::Share.applies(&shared_folder));

        // Transfer: what the viewer owns outright, in the cloud, and never a
        // clan's map or folder.
        assert!(Action::Transfer.applies(&own_cloud_map));
        assert!(!Action::Transfer.applies(&local_map));
        assert!(!Action::Transfer.applies(&reshareable));
        assert!(Action::Transfer.applies(&own_folder));
        assert!(!Action::Transfer.applies(&admin_folder));
        let mut clan_map = facts(map(7), MapStorage::Cloud, true);
        clan_map.clan_owned = true;
        let mut clan_folder = facts(folder(14), MapStorage::Cloud, true);
        clan_folder.clan_owned = true;
        assert!(!Action::Transfer.applies(&clan_map));
        assert!(!Action::Transfer.applies(&clan_folder));
        // A clan's folder lists no server entries of the viewer's.
        assert!(!Action::Servers.applies(&clan_folder));

        // Servers: loose cloud maps (owned or not) and cloud folders.
        assert!(Action::Servers.applies(&own_cloud_map));
        assert!(Action::Servers.applies(&shared_map));
        assert!(!Action::Servers.applies(&filed_map));
        assert!(!Action::Servers.applies(&local_map));
        assert!(Action::Servers.applies(&shared_folder));
        assert!(!Action::Servers.applies(&local_folder));

        // Delete: what single delete allows.
        assert!(Action::Delete.applies(&session_map));
        assert!(Action::Delete.applies(&local_folder));
        assert!(!Action::Delete.applies(&shared_map));
        assert!(!Action::Delete.applies(&shared_folder));
    }

    #[test]
    fn counts_follow_what_each_action_applies_to() {
        let chosen = vec![
            facts(map(1), MapStorage::Cloud, true),
            facts(map(2), MapStorage::Local, true),
            facts(map(3), MapStorage::Cloud, false),
            facts(folder(10), MapStorage::Cloud, true),
        ];
        let counts = |action| {
            let (applies, _) = split(action, &chosen);
            Counts::of(applies.iter().copied())
        };
        assert_eq!(
            counts(Action::Move),
            Counts {
                maps: 2,
                folders: 0
            }
        );
        assert_eq!(
            counts(Action::Share),
            Counts {
                maps: 1,
                folders: 1
            }
        );
        assert_eq!(
            counts(Action::Transfer),
            Counts {
                maps: 1,
                folders: 1
            }
        );
        assert_eq!(
            counts(Action::Servers),
            Counts {
                maps: 2,
                folders: 1
            }
        );
        assert_eq!(
            counts(Action::Delete),
            Counts {
                maps: 2,
                folders: 1
            }
        );
        let (_, skipped) = split(Action::Move, &chosen);
        assert_eq!(
            skipped.iter().map(|facts| facts.item).collect::<Vec<_>>(),
            vec![map(3), folder(10)]
        );
    }

    #[test]
    fn the_pane_says_what_each_button_does_with_counts() {
        assert_eq!(
            heading(Counts {
                maps: 3,
                folders: 1
            }),
            "3 maps and 1 folder"
        );
        assert_eq!(
            heading(Counts {
                maps: 1,
                folders: 0
            }),
            "1 map"
        );
        assert_eq!(
            heading(Counts {
                maps: 0,
                folders: 2
            }),
            "2 folders"
        );
        assert_eq!(
            action_label(
                Action::Transfer,
                Counts {
                    maps: 3,
                    folders: 0
                }
            ),
            "Transfer 3 maps…"
        );
        assert_eq!(
            action_label(
                Action::Delete,
                Counts {
                    maps: 2,
                    folders: 1
                }
            ),
            "Delete 2 maps and 1 folder…"
        );
        assert_eq!(
            action_label(
                Action::Move,
                Counts {
                    maps: 1,
                    folders: 0
                }
            ),
            "Move 1 map to a folder…"
        );
        assert_eq!(
            action_label(
                Action::Share,
                Counts {
                    maps: 0,
                    folders: 1
                }
            ),
            "Share 1 folder…"
        );
        assert_eq!(
            action_label(
                Action::Servers,
                Counts {
                    maps: 2,
                    folders: 2
                }
            ),
            "Servers for 2 maps and 2 folders…"
        );
        // An action that applies to none keeps its bare name.
        assert_eq!(
            action_label(Action::Move, Counts::default()),
            "Move to folder…"
        );
        assert_eq!(action_label(Action::Delete, Counts::default()), "Delete…");
        assert_eq!(
            skipped_line(&["Forest".to_string(), "Cities".to_string()]).as_deref(),
            Some("Skipped: “Forest”, “Cities”")
        );
        assert_eq!(skipped_line(&[]), None);
        assert_eq!(
            crate::i18n::t!(
                "mapper-multi-delete-question",
                "items" => object(Counts { maps: 3, folders: 1 })
            ),
            "Delete 3 maps and 1 folder? You can't undo this."
        );
    }

    #[test]
    fn every_locale_counts_what_an_action_acts_on() {
        let render = |tag: &str, id: &'static str, count: usize| {
            let translator = smudgy_i18n::Translator::for_tag(tag).expect("built-in catalog");
            let mut args = smudgy_i18n::FluentArgs::new();
            args.set("count", count);
            args.set("items", "ITEMS");
            args.set("first", "A");
            args.set("second", "B");
            args.set("name", "N");
            args.set("names", "NS");
            translator.translate_with(id, &args)
        };
        // What an action acts on takes the accusative where a language has one.
        assert_eq!(render("pl-PL", "mapper-multi-maps", 1), "1 mapa");
        assert_eq!(render("pl-PL", "mapper-multi-maps-object", 1), "1 mapę");
        assert_eq!(render("pl-PL", "mapper-multi-maps-object", 3), "3 mapy");
        assert_eq!(render("pl-PL", "mapper-multi-maps-object", 5), "5 map");
        assert_eq!(render("pl-PL", "mapper-multi-folders", 5), "5 folderów");
        assert_eq!(render("uk-UA", "mapper-multi-folders", 1), "1 тека");
        assert_eq!(render("uk-UA", "mapper-multi-folders-object", 1), "1 теку");
        assert_eq!(render("uk-UA", "mapper-multi-maps-object", 11), "11 мап");
        assert_eq!(render("zh-TW", "mapper-multi-and", 0), "A和 B");
        assert_eq!(render("zh-TW", "mapper-multi-list-separator", 0), "、");
        assert_eq!(render("en-US", "mapper-multi-list-separator", 0), ", ");
        // A translation that fails to format falls back to English.
        for tag in ["pl-PL", "uk-UA", "zh-TW"] {
            for id in [
                "mapper-multi-maps",
                "mapper-multi-folders-object",
                "mapper-multi-delete-question",
                "mapper-multi-skipped",
                "mapper-multi-servers",
            ] {
                let rendered = render(tag, id, 2);
                assert_ne!(rendered, render("en-US", id, 2), "{tag} {id}");
            }
        }
    }

    #[test]
    fn the_pane_names_each_kind() {
        assert_eq!(kind(&facts(map(1), MapStorage::Cloud, true)), "map");
        assert_eq!(kind(&facts(map(1), MapStorage::Cloud, false)), "shared map");
        assert_eq!(kind(&facts(map(1), MapStorage::Local, true)), "local map");
        assert_eq!(
            kind(&facts(map(1), MapStorage::Session, true)),
            "session map"
        );
        assert_eq!(kind(&facts(folder(1), MapStorage::Cloud, true)), "folder");
        assert_eq!(
            kind(&facts(folder(1), MapStorage::Cloud, false)),
            "shared folder"
        );
        assert_eq!(
            kind(&facts(folder(1), MapStorage::Local, true)),
            "local folder"
        );
    }

    #[test]
    fn the_servers_checklist_starts_from_the_shared_state() {
        let atlas = AtlasId(Uuid::from_u128(10));
        let loose = area(1);
        let targets = [ScopeTarget::Atlas(atlas), ScopeTarget::Area(loose)];
        let mut scopes = MapScopes::default();
        for delta in [
            ScopeDelta::SetAtlasEntry {
                atlas_id: atlas,
                entry: "Arctic".to_string(),
                show: true,
            },
            ScopeDelta::SetAreaEntry {
                area_id: loose,
                entry: "Arctic".to_string(),
                show: true,
            },
            ScopeDelta::SetAtlasEntry {
                atlas_id: atlas,
                entry: "Nuke".to_string(),
                show: true,
            },
        ] {
            scopes.apply(&delta);
        }
        assert_eq!(server_state(&scopes, &targets, "Arctic"), ServerState::All);
        assert_eq!(server_state(&scopes, &targets, "Nuke"), ServerState::Mixed);
        assert_eq!(server_state(&scopes, &targets, "Other"), ServerState::Off);

        // Checking a mixed server shows every target on it, changing only
        // the one that wasn't.
        let deltas = server_deltas(&scopes, &targets, "Nuke", true);
        assert_eq!(
            deltas,
            vec![ScopeDelta::SetAreaEntry {
                area_id: loose,
                entry: "Nuke".to_string(),
                show: true,
            }]
        );
        for delta in &deltas {
            scopes.apply(delta);
        }
        assert_eq!(server_state(&scopes, &targets, "Nuke"), ServerState::All);
        assert_eq!(server_deltas(&scopes, &targets, "Arctic", false).len(), 2);
    }
}
