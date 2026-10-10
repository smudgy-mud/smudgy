//! The map editor window: a toolbar over a resizable three-pane layout
//! (area list | canvas | inspector). The canvas is a
//! [`smudgy_map_widget::MapEditor`]; this module owns window chrome, pane
//! layout, the undo stack, and the mutation funnel — every entity edit
//! flows through [`commands::CommandStack`].

mod area_list;
mod atlas_panel;
mod automatic_routing;
mod clan_map_share;
mod clan_maps;
mod clan_secret_share;
mod clan_secrets;
mod clan_share;
mod clipboard;
pub mod commands;
mod context_menu;
mod default_atlases;
mod document;
mod folder_picker;
mod inspector;
mod legend;
mod link_commands;
mod link_panel;
mod links;
mod local_move;
// TEMPORARY(0.6.x): files maps made outside a folder by earlier builds.
mod access_review;
mod filing;
#[cfg(test)]
mod link_edit_tests;
mod loose_maps_migration;
mod map_panel;
mod modals;
mod moves;
mod multi_select;
mod panels;
mod place_fields;
mod secrets;
mod source_rooms;
mod tags;
mod toolbar;

use std::collections::{HashMap, HashSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering as AtomicOrdering},
};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::cloud_account::CloudHandles;
use crate::components::cloud_errors::display_error;
use crate::theme::{self, Element as ThemedElement};
use crate::update::Update;
use iced::alignment::Vertical;
use iced::event::Event as IcedEvent;
use iced::keyboard::{self, key::Named};
use iced::widget::{
    PaneGrid, button, center, column, container, mouse_area, opaque, pane_grid, row, space, stack,
    text,
};
use iced::{Length, Subscription, Task, Vector, window};
use smudgy_cloud::cloud_api::{AtlasCopyReport, CopyAreaRequest, ShareDirection, ShareGrantRow};
use smudgy_cloud::mapper::{AtlasCache, area_cache::AreaCache};
use smudgy_cloud::{
    Area, AreaAccess, AreaId, AtlasId, AtlasListItem, AtlasRelocation, CloudError, ConnectionId,
    ConnectionRouting, ConnectionUpdates, MapDestination, MapStorage, Mapper, PortMode,
    RelocationMode, RoomNumber, RoomSide, SegmentShape, SourceId,
    automatic_routing::{AutoRouteResult, RouteValidation},
    mapper::RoomKey,
    mutation::OperationId,
};

use area_list::SharerIndex;
use smudgy_core::models::map_scopes::{
    HostEntry, MapScopes, ScopeDelta, ScopeState, match_host_hints,
};
use smudgy_map_widget::map_editor::{
    self, EntityId, ExitTarget, MapEditor, MutationRequest, PlacedRoom, SelectedConnectionHandle,
    Tool,
};

/// How long a notice stays in the footer (expired by the periodic
/// [`Message::Tick`]).
const NOTICE_TTL: Duration = Duration::from_secs(5);

/// The copy/paste clipboard, swappable so every editor window can be handed
/// one shared instance.
pub type SharedClipboard = Arc<ArcSwap<commands::EntityClipboard>>;

#[derive(Debug)]
struct PendingAutomaticRoute {
    generation: u64,
    snapshot: automatic_routing::Snapshot,
    cancel: Arc<AtomicBool>,
}

#[derive(Debug, Clone)]
struct AutomaticRoutePreview {
    snapshot: automatic_routing::Snapshot,
    route_points: Vec<smudgy_cloud::MapPoint>,
}

/// A keyboard action routed to the editor at window level. Only fires for
/// events no focused widget captured (so text inputs keep their keys).
#[derive(Debug, Clone, Copy)]
pub enum Hotkey {
    Delete,
    Nudge(i32, i32, f32),
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    Escape,
    LevelUp,
    LevelDown,
    MoveLevelUp,
    MoveLevelDown,
    /// Shift+F10 or the Menu key: the context menu for the selection.
    ContextMenu,
    /// T: the selected rooms' tag input.
    FocusTags,
    /// Tab: completes the tag input with its first suggestion when it has
    /// focus (a focused input leaves Tab to the window); otherwise moves to
    /// the inspector's next input.
    Tab,
    /// Shift+Tab: the inspector's previous input.
    TabBack,
}

#[derive(Debug, Clone)]
pub enum Message {
    Editor(map_editor::Message),
    PaneResized(pane_grid::ResizeEvent),
    AreaSelected(AreaId),
    ToolSelected(Tool),
    /// Keep-theirs finished for a Link gesture whose proposed new room
    /// number was taken while its CAS envelope was in flight.
    NewRoomLinkConflictResolved {
        operation_id: OperationId,
        result: Result<(), String>,
    },
    CopyIncludeBoundaryChanged(bool),
    CopySelectionConfirmed,
    LevelUp,
    LevelDown,
    Undo,
    Redo,
    Hotkey(window::Id, Hotkey),
    Inspector(inspector::Message),
    AutomaticRouteSolved {
        generation: u64,
        snapshot: automatic_routing::Snapshot,
        result: AutoRouteResult,
    },
    AutomaticRouteAccepted,
    AutomaticRouteCancelled,
    Tick,
    CommandCompleted(commands::Outcome),
    CutRepositionCompleted {
        id: smudgy_cloud::Uuid,
        acknowledged: bool,
    },
    SetCurrentLocation(AreaId, Option<i32>),
    NewAreaRequested,
    CreateAreaNameChanged(String),
    /// Whose a new map in a clan's folder is.
    CreateAreaOwnership(smudgy_cloud::clan_maps::MapOwnership),
    CreateAreaConfirmed,
    AreaCreated(Result<AreaId, String>),
    /// The folder field of the open New map or Copy dialog.
    FolderPicker(folder_picker::PickerMessage),
    /// A folder named in a New map, Copy, or Move dialog was made (or not);
    /// the dialog carries on into it.
    NewFolderMade(Result<folder_picker::OwnFolder, String>),
    /// The Move dialog's new-folder form.
    MoveAreaNewFolder(folder_picker::NewFolderMessage),
    /// TEMPORARY(0.6.x): the viewer's loose maps were filed.
    LooseMaps(loose_maps_migration::Done),
    RenameAreaStarted(AreaId),
    RenameAreaChanged(String),
    RenameAreaCommitted,
    RenameAreaCompleted(Result<(), String>),
    DeleteAreaRequested(AreaId),
    DeleteAreaConfirmed,
    DeleteAreaCompleted {
        area_id: AreaId,
        result: Result<(), String>,
    },
    ModalDismissed,
    /// Open the share dialog for the active area (owner or re-sharer only).
    ShareDialogRequested,
    /// The Share dialog, opened on one of the map's Secrets.
    ShareSecretRequested(SourceId),
    /// The toolbar's ⋯ menu of map actions opened or closed.
    MapMenuToggled(bool),
    /// "Add to", and a Secret's create/rename/delete.
    Secrets(secrets::SecretsMessage),
    /// The map's and atlases' panels in the inspector.
    Panel(panels::PanelMessage),
    /// Moving the selection to another place.
    Move(moves::MoveMessage),
    /// The link editor: a room's Exits, a link's ends, doors and look.
    Links(link_panel::LinkMessage),
    /// Several maps and folders chosen in the map list, and their actions.
    Multi(multi_select::MultiMessage),
    /// The canvas context menu closed without a pick.
    ContextMenuClosed,
    /// Up/Down in the open context menu.
    ContextMenuStep(i32),
    /// Enter in the open context menu: its highlighted row.
    ContextMenuActivated,
    /// A context menu entry.
    ContextAction(context_menu::ContextAction),
    /// A folder's ⋯ menu opened (`Some`) or closed.
    FolderMenuToggled(Option<AtlasId>),
    /// An entry of a ⋯ menu: closes the menu, then runs the action.
    MenuPicked(Box<Message>),
    /// Share-dialog internals, routed to [`modals::update_share`].
    Share(modals::ShareMessage),
    /// Open the copy-to-my-maps modal for the active shared area
    /// (`can_copy` grantees only).
    CopyAreaRequested,
    CopyAreaNameChanged(String),
    CopyAreaConfirmed,
    /// `POST /areas/{id}/copy` finished; `Ok` carries the clone's id.
    CopyAreaCompleted {
        result: Result<AreaId, CloudError>,
        /// Carried from the dialog at request time so the completion handler
        /// doesn't depend on the modal still being open (it may have been
        /// dismissed mid-copy).
        duplicate: bool,
    },
    /// "Copy whole atlas…" pressed inside the copy modal.
    CopyAtlasRequested,
    CopyAtlasCompleted(Result<AtlasCopyReport, CloudError>),
    /// The signed-out banner's CTA; bubbles up as [`Event::OpenSettings`].
    OpenSettingsRequested,
    /// The signed-out banner's close affordance; hides the banner and persists
    /// the dismissal against the current client version.
    DismissSigninBanner,
    /// The toolbar sync indicator, pressed while idle; wakes the mapper for an
    /// immediate sync (the engine has no periodic poll).
    SyncNowRequested,
    KeepMineRequested,
    KeepTheirsRequested,
    RetrySaveRequested,
    DiscardFailedSaveRequested,
    SaveResolutionCompleted {
        result: Result<(), String>,
        discarded_operation: Option<OperationId>,
    },
    /// Toggle whether `area_id` participates in room identification/routing.
    ToggleAreaEnabled(AreaId),
    /// Make `area_id` the active copy of its copy-family: enable it and
    /// disable every other family member.
    SetActiveCopy(AreaId),
    /// Received grants + the area list loaded together on the sync tick
    /// (signed-in only); rebuilds [`Self::sharers`] from the grants and
    /// [`Self::family_index`] from the areas' `family_token`s.
    IndicesLoaded {
        auth_projection_revision: u64,
        grants: Result<Vec<ShareGrantRow>, CloudError>,
        areas: Result<Vec<Area>, CloudError>,
    },
    /// Owner self-copy ("Duplicate"); bubbles like [`Message::CopyAreaRequested`]
    /// but produces an inactive clone.
    DuplicateAreaRequested,

    // ===== atlases (folders for your own maps) =====
    /// The owned-atlas inventory finished loading (refreshed on the sync tick).
    AtlasesLoaded {
        auth_projection_revision: u64,
        result: Result<Vec<AtlasListItem>, CloudError>,
    },
    /// Open the create-folder modal.
    NewAtlasRequested,
    CreateAtlasNameChanged(String),
    /// Pick the new folder's explicit storage tier.
    CreateAtlasTierChanged(MapStorage),
    CreateAtlasConfirmed,
    AtlasCreated(Result<AtlasId, String>),
    /// Move a whole owned atlas between local and cloud storage.
    MoveAtlasStorageRequested(AtlasId),
    MoveAtlasStorageConfirmed,
    MoveAtlasStorageCompleted(Result<AtlasRelocation, String>),
    LocalMoveReviewed(
        local_move::Request,
        Result<Vec<smudgy_cloud::relocation::LocalMoveReview>, CloudError>,
    ),
    LocalMoveConfirmed,
    /// Begin an inline rename of a folder header.
    RenameAtlasStarted(AtlasId),
    RenameAtlasChanged(String),
    RenameAtlasCommitted,
    AtlasRenamed(Result<(), String>),
    /// Open the gentle-delete confirmation for a folder.
    DeleteAtlasRequested(AtlasId),
    DeleteAtlasConfirmed,
    AtlasDeleted(Result<(), String>),
    /// Open the create-area modal pre-targeted at a folder.
    NewAreaInAtlas(AtlasId),
    /// Open the "move to folder" picker for an owned area.
    MoveAreaRequested(AreaId),
    /// Move an owned area to an explicit storage/folder destination.
    MoveAreaTo {
        area: AreaId,
        destination: MapDestination,
    },
    MoveAreaCompleted(Result<AreaId, String>),
    FilingReviewed(smudgy_cloud::Uuid, filing::Prepared),
    FilingConfirmed,
    /// Collapse/expand a folder in the area list (pure view state).
    ToggleFolderCollapsed(FolderKey),
    /// Open the atlas-scoped "Share folder…" dialog.
    ShareAtlasRequested(AtlasId),
    /// Share-folder dialog internals, routed to [`modals::update_share_atlas`].
    ShareAtlas(modals::ShareAtlasMessage),
    /// Open the transfer-ownership offer for the active area (owner-only).
    TransferOwnershipRequested,
    /// Open the transfer offer for a specific area (area-list row).
    /// Open the transfer offer for a folder (folder header).
    TransferAtlasOwnershipRequested(AtlasId),
    /// Make one of the viewer's own folders where this editor's server's
    /// scripts put new maps in its storage.
    UseForNewMaps(AtlasId),
    /// The server's default folder was changed, or couldn't be.
    DefaultAtlasSet(Result<(), String>),
    /// Transfer-offer dialog internals, routed to [`modals::update_transfer`].
    Transfer(modals::TransferMessage),
    /// Clans: their folders and menus, Incoming maps, and sharing with
    /// groups, routed to [`clan_maps::update`].
    Clan(clan_maps::ClanMessage),
    /// A clan map's Share dialog.
    ClanMapShare(Box<clan_map_share::ClanMapShareMessage>),
    /// "Put in a clan…".
    PutInClan(clan_map_share::PutInClanMessage),
    /// Opens a clan map's Share dialog, as a shortcut from elsewhere does.
    MapAccessRequested(AreaId),

    // ===== cloud-map scope (per-server atlas visibility) =====
    /// The scope control: `true` = All atlases, `false` = This server.
    ScopeAllToggled(bool),
    ScopeMenuToggled(bool),
    MapListFilterChanged(String),
    /// Open the "Servers…" checklist for an atlas or atlas-less area.
    ServersChecklistRequested(ScopeTarget),
    /// Show/hide the checklist's target on one server entry.
    ScopeServerToggled {
        entry: String,
        show: bool,
    },
    /// The daemon mirrored an updated scope store into this editor (another
    /// editor changed an association).
    ScopesReplaced(MapScopes),
}

#[derive(Debug, Clone)]
pub enum Event {
    /// Ask the daemon to open (or focus) the settings window on the Account
    /// tab so the user can sign in or create an account.
    OpenSettings,
    /// The set of disabled areas changed; the daemon persists it and fans it
    /// out to every live mapper.
    DisabledAreasChanged(std::collections::HashSet<AreaId>),
    /// The cloud-map scope associations changed (a "Servers…" edit, a creation,
    /// bind-on-use, or newly observed/first-sight-homed atlases). Carried as
    /// targeted deltas — never a whole-store snapshot — so the daemon replays
    /// them against its authoritative copy without clobbering a concurrent
    /// write. The editor has already applied them optimistically to its own
    /// snapshot; the daemon persists, recomputes each server's exclusions, and
    /// mirrors the corrected store back into every editor.
    ScopeAssociationsChanged(Vec<ScopeDelta>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneKind {
    AreaList,
    Canvas,
    Inspector,
}

/// `event::listen_with` filter mapping uncaptured keyboard events to editor
/// hotkeys, tagged with the window the event happened in.
fn editor_hotkeys(
    event: IcedEvent,
    status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    if status != iced::event::Status::Ignored {
        return None;
    }

    let IcedEvent::Keyboard(keyboard::Event::KeyPressed {
        key,
        modifiers,
        physical_key,
        ..
    }) = event
    else {
        return None;
    };

    let hotkey = match key.as_ref() {
        keyboard::Key::Named(Named::Delete | Named::Backspace) => Hotkey::Delete,
        keyboard::Key::Named(Named::ArrowLeft) => Hotkey::Nudge(-1, 0, nudge_step(modifiers)),
        keyboard::Key::Named(Named::ArrowRight) => Hotkey::Nudge(1, 0, nudge_step(modifiers)),
        keyboard::Key::Named(Named::ArrowUp) => Hotkey::Nudge(0, -1, nudge_step(modifiers)),
        keyboard::Key::Named(Named::ArrowDown) => Hotkey::Nudge(0, 1, nudge_step(modifiers)),
        keyboard::Key::Named(Named::Escape) => Hotkey::Escape,
        keyboard::Key::Named(Named::ContextMenu) => Hotkey::ContextMenu,
        keyboard::Key::Named(Named::F10) if modifiers.shift() => Hotkey::ContextMenu,
        keyboard::Key::Named(Named::PageUp) if modifiers.command() => Hotkey::MoveLevelUp,
        keyboard::Key::Named(Named::PageDown) if modifiers.command() => Hotkey::MoveLevelDown,
        keyboard::Key::Named(Named::PageUp) => Hotkey::LevelUp,
        keyboard::Key::Named(Named::PageDown) => Hotkey::LevelDown,
        keyboard::Key::Character(c)
            if modifiers.command() && modifiers.shift() && c.eq_ignore_ascii_case("z") =>
        {
            Hotkey::Redo
        }
        keyboard::Key::Character(c) if modifiers.command() && c.eq_ignore_ascii_case("z") => {
            Hotkey::Undo
        }
        keyboard::Key::Character(c) if modifiers.command() && c.eq_ignore_ascii_case("y") => {
            Hotkey::Redo
        }
        keyboard::Key::Character(c) if modifiers.command() && c.eq_ignore_ascii_case("c") => {
            Hotkey::Copy
        }
        keyboard::Key::Character(c) if modifiers.command() && c.eq_ignore_ascii_case("x") => {
            Hotkey::Cut
        }
        keyboard::Key::Character(c) if modifiers.command() && c.eq_ignore_ascii_case("v") => {
            Hotkey::Paste
        }
        // The key where T is on a Latin layout, whatever the layout.
        keyboard::Key::Character(_)
            if !modifiers.command()
                && !modifiers.alt()
                && key
                    .to_latin(physical_key)
                    .is_some_and(|c| c.eq_ignore_ascii_case(&'t')) =>
        {
            Hotkey::FocusTags
        }
        keyboard::Key::Named(Named::Tab) if modifiers.is_empty() => Hotkey::Tab,
        keyboard::Key::Named(Named::Tab) if modifiers == keyboard::Modifiers::SHIFT => {
            Hotkey::TabBack
        }
        _ => return None,
    };

    Some(Message::Hotkey(window_id, hotkey))
}

/// The revision of each of `area`'s places besides the map.
fn place_revs(area: &AreaCache) -> Vec<(SourceId, i64)> {
    area.meta()
        .sources
        .iter()
        .filter(|bundle| !bundle.source.is_map())
        .map(|bundle| (bundle.source, bundle.rev))
        .collect()
}

/// Map-space keyboard adjustment: Alt is fine, Shift is coarse.
fn nudge_step(modifiers: keyboard::Modifiers) -> f32 {
    if modifiers.shift() {
        1.0
    } else if modifiers.alt() {
        0.05
    } else {
        0.25
    }
}

/// Identifies one folder in the map list for collapse/expand state. A named
/// atlas, or the "Not in a folder" bucket of one storage's own maps, for the
/// viewer's maps whose folder the inventory doesn't list (yet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderKey {
    Atlas(AtlasId),
    Unfiled(MapStorage),
    /// The "Unassigned" group in the This-server scope: atlases with no
    /// server-entry association yet. Collapsed by default.
    Unassigned,
}

/// A cloud-map scope association target: a whole atlas, or a genuinely
/// atlas-less area. The unit the "Servers…" checklist writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeTarget {
    Atlas(AtlasId),
    Area(AreaId),
}

pub struct MapEditorWindow {
    window_id: window::Id,
    mapper: Mapper,
    /// App-global cloud handles: sign-in state and the API client the
    /// sharing, copy, and transfer dialogs talk to directly.
    cloud: CloudHandles,
    panes: pane_grid::State<PaneKind>,
    editor: MapEditor,
    stack: commands::CommandStack,
    inspector: inspector::State,
    /// The open map's tags place by place, for the Tags section, the Rooms
    /// filter and the tag input's suggestions; rebuilt per revision.
    tag_index: tags::IndexCache,
    last_seen_rev: Option<i64>,
    /// The open map's other places' revisions as last seen: a change another
    /// writer makes in a Secret or Private moves these, not the map's.
    last_seen_place_revs: Vec<(SourceId, i64)>,
    /// A creation command whose entities should be selected as their async
    /// creates resolve (drag-rect creation, paste).
    pending_select: Option<commands::CommandId>,
    /// Copied entities awaiting paste, with a counter cascading the
    /// same-area paste offset. Behind `ArcSwap` so one clipboard can be
    /// shared across editor windows; each window gets its own
    /// (see [`Self::with_clipboard`]).
    clipboard: SharedClipboard,
    clipboard_cut_in_flight: Option<smudgy_cloud::Uuid>,
    clipboard_reposition_in_flight: Option<smudgy_cloud::Uuid>,
    consecutive_pastes: u32,
    /// When the rooms-excluded-from-copy notice was shown; renders as a
    /// transient banner under the toolbar until the tick expires it.
    /// Short non-modal feedback for collaboration-driven selection changes.
    editor_notice: Option<(Instant, String)>,
    automatic_route_generation: u64,
    pending_automatic_route: Option<PendingAutomaticRoute>,
    automatic_route_preview: Option<AutomaticRoutePreview>,
    /// Automatic routes touched by an endpoint move or an external area
    /// revision. This is intentionally UI state: the v2 wire schema has no
    /// persisted staleness bit, and collision is recomputed from geometry.
    automatic_routes_maybe_stale: HashSet<ConnectionId>,
    /// New-room Link gestures keyed by their CAS operation, so a collision
    /// on the new room's number can make the link again under another.
    pending_new_room_links: HashMap<OperationId, modals::LinkDraft>,
    /// A number-collision operation being discarded before its link is made
    /// again against the fresh projection.
    recovering_new_room_link: Option<(OperationId, modals::LinkDraft, RoomNumber)>,
    modal: Option<modals::Modal>,
    /// In-progress inline rename in the area list.
    renaming_area: Option<(AreaId, String)>,
    /// Whether the toolbar's ⋯ menu of map actions is open.
    map_menu_open: bool,
    /// The folder whose ⋯ menu is open.
    folder_menu: Option<AtlasId>,
    /// The maps and folders chosen in the map list.
    multi: multi_select::MultiSelect,
    /// Where new content goes, and a Secret's create/rename/delete.
    secrets: secrets::SecretsState,
    /// The map's and atlases' panels in the inspector.
    panel: panels::PanelState,
    /// The canvas context menu, while open.
    context_menu: Option<context_menu::ContextMenu>,
    /// A move is in flight: the canvas takes no edits until it lands.
    moving: bool,
    /// The move in flight, as undo or redo will record it.
    running_move: Option<(moves::MoveRecord, moves::MoveKind)>,
    /// The last lossless move, below the command history.
    move_history: moves::MoveHistory,
    /// The rooms a move in flight carries and where, so the selection can
    /// follow them.
    moved_rooms: Option<(AreaId, smudgy_cloud::SourceId, Vec<RoomNumber>)>,
    /// A clone we created whose area hasn't landed in the cache yet; the
    /// periodic tick selects it as soon as sync delivers it.
    pending_copied_area: Option<AreaId>,
    /// Sharer attribution for shared rows, resolved from received grants
    /// (the grantor handle rides on each row — no friends join). `None`
    /// while signed out or before the first fetch lands.
    sharers: Option<SharerIndex>,
    /// Per-viewer copy-family buckets from the list-only [`Area::family_token`],
    /// refreshed on the same tick as [`Self::sharers`]. Empty while
    /// signed out or before the first fetch. Used **in-memory only** to group
    /// rows for the current list; never persisted or cross-referenced.
    family_index: FamilyIndex,
    /// The mapper sync revision the sharer index was last refreshed at; a
    /// change (background sync swapped the cache) triggers a refetch.
    last_seen_sync_revision: Option<u64>,
    /// Credential-boundary revision. A change clears cloud atlas metadata
    /// before any fallible identity/atlas request can retain stale names.
    last_seen_auth_projection_revision: Option<u64>,
    /// Owned-atlas inventory (the only source of atlas *names*), refreshed on
    /// the same tick as [`Self::sharers`]. Empty while signed out / no
    /// credential. Drives the "My maps" folder labels.
    atlases: Vec<AtlasListItem>,
    /// Folders the user collapsed in the area list. Pure view state.
    collapsed_folders: HashSet<FolderKey>,
    /// In-progress inline rename of a folder header.
    renaming_atlas: Option<(AtlasId, String)>,
    /// Snapshot of which atlases belong to the local (never-synced) tier,
    /// refreshed on the tick. Cloud-only affordances (Share folder) are gated
    /// off local folders, and the move picker keeps targets same-tier.
    local_atlas_ids: HashSet<AtlasId>,
    /// Whether the signed-out CTA banner is hidden because the user dismissed
    /// it on the current client version (mirrors the main window's upgrade
    /// prompt). Seeded from settings at construction; upgrading the client
    /// surfaces the banner once more.
    signin_banner_dismissed: bool,
    /// The server entry this editor was opened from — the cloud-map scope
    /// context. `None` when the editor has no session context (the scope
    /// control is then hidden and everything is shown).
    server_name: Option<String>,
    /// A snapshot of the per-user cloud-map scope associations. The daemon owns
    /// the authoritative copy; this window reads it to filter the session tree
    /// and drive the "Servers…" checklist, writes into it optimistically, and
    /// bubbles every change up via [`Event::ScopeAssociationsChanged`].
    map_scopes: MapScopes,
    /// The scope control: `false` = This server (the session tree, filtered to
    /// this entry), `true` = All atlases (every atlas, unfiltered). Defaults to
    /// This server when a server context exists, All otherwise.
    scope_all: bool,
    scope_menu_open: bool,
    map_list_filter: String,
    /// The viewer's clans, the maps offered to them, and where listed maps
    /// were put in clan folders.
    clans: clan_maps::ClanState,
    /// Where [`Self::server_name`]'s scripts put new maps that name no
    /// folder, in each storage.
    default_atlases: default_atlases::DefaultAtlases,
    /// Rooms moved between places, by this window or any other holding the
    /// mapper: another window's move leaves this one's history naming rooms
    /// where they no longer are.
    room_remaps: Arc<smudgy_cloud::mapper::pending::RoomRemapSubscription>,
}

fn reciprocal_pair_candidate(
    area: &AreaCache,
    area_id: AreaId,
    from: smudgy_cloud::RoomAddress,
    to: smudgy_cloud::RoomAddress,
    from_direction: smudgy_cloud::ExitDirection,
    to_direction: smudgy_cloud::ExitDirection,
) -> Option<ConnectionId> {
    let mut candidates = area
        .get_connections()
        .iter()
        .filter_map(|connection| {
            let members: Vec<_> = area
                .document_rooms()
                .flat_map(|room| {
                    room.get_exits()
                        .iter()
                        .map(move |exit| (room.address(), exit))
                })
                .filter(|(_, exit)| exit.connection_id == connection.id)
                .collect();
            (members.len() == 1
                && from != to
                && members[0].0 == to
                && members[0].1.destination_address()
                    == Some(smudgy_cloud::MapRoomAddress {
                        map: area_id,
                        room: from,
                    })
                && members[0].1.from_direction == to_direction
                && members[0]
                    .1
                    .to_direction
                    .is_none_or(|direction| direction == from_direction))
            .then_some(connection.id)
        })
        .take(2)
        .collect::<Vec<_>>();
    (candidates.len() == 1).then(|| candidates.remove(0))
}

impl MapEditorWindow {
    fn clear_automatic_route_state(&mut self) -> bool {
        let had_state =
            self.pending_automatic_route.is_some() || self.automatic_route_preview.is_some();
        if let Some(pending) = self.pending_automatic_route.take() {
            pending.cancel.store(true, AtomicOrdering::Relaxed);
        }
        self.automatic_route_preview = None;
        self.editor.set_automatic_route_preview(None);
        had_state
    }

    pub(super) fn start_automatic_route(
        &mut self,
        connection_id: ConnectionId,
    ) -> Update<Message, Event> {
        if !self.can_edit_active_area() {
            return Update::none();
        }
        let atlas = self.mapper.get_current_atlas();
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        let Some(area) = atlas.get_area(&area_id) else {
            return Update::none();
        };
        let (snapshot, request) = match automatic_routing::capture(&area, connection_id) {
            Ok(captured) => captured,
            Err(key) => {
                self.editor_notice = Some((Instant::now(), crate::i18n::translate(key)));
                return Update::none();
            }
        };

        self.clear_automatic_route_state();
        self.automatic_route_generation = self.automatic_route_generation.wrapping_add(1);
        let generation = self.automatic_route_generation;
        let cancel = Arc::new(AtomicBool::new(false));
        self.pending_automatic_route = Some(PendingAutomaticRoute {
            generation,
            snapshot: snapshot.clone(),
            cancel: cancel.clone(),
        });
        self.editor_notice = Some((Instant::now(), crate::i18n::t!("mapper-route-finding")));

        let callback_snapshot = snapshot.clone();
        Update::with_task(Task::perform(
            async move {
                tokio::task::spawn_blocking(move || {
                    smudgy_cloud::automatic_routing::solve(&request, cancel.as_ref())
                })
                .await
                .unwrap_or(AutoRouteResult::LimitReached)
            },
            move |result| Message::AutomaticRouteSolved {
                generation,
                snapshot: callback_snapshot.clone(),
                result,
            },
        ))
    }

    pub(super) fn automatic_route_validation(
        &self,
        connection_id: ConnectionId,
    ) -> Option<RouteValidation> {
        let area_id = self.editor.area_id()?;
        let atlas = self.mapper.get_current_atlas();
        let area = atlas.get_area(&area_id)?;
        let connection = area.get_connection(connection_id)?;
        let (_, request) = automatic_routing::capture(&area, connection_id).ok()?;
        Some(smudgy_cloud::automatic_routing::validate_route(
            &request,
            &connection.route_points,
        ))
    }

    pub(super) fn automatic_route_is_stale(&self, connection_id: ConnectionId) -> bool {
        self.automatic_routes_maybe_stale.contains(&connection_id)
    }

    fn mark_moved_automatic_routes_stale(&mut self) {
        let Some(area_id) = self.editor.area_id() else {
            return;
        };
        let moved_rooms: HashSet<_> = self
            .editor
            .selection()
            .iter()
            .filter_map(|entity| match entity {
                EntityId::Room(number) => Some(PlacedRoom::map(number)),
                EntityId::SourceRoom(source, number) => Some(PlacedRoom::new(source, number)),
                _ => None,
            })
            .collect();
        if moved_rooms.is_empty() {
            return;
        }
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&area_id) else {
            return;
        };
        for connection in area.get_connections() {
            if connection.routing != ConnectionRouting::Automatic {
                continue;
            }
            let Some(endpoint_b) = connection.endpoint_b else {
                continue;
            };
            let a_moved = moved_rooms.contains(&connection.endpoint_a.address());
            let b_moved = moved_rooms.contains(&endpoint_b.address());
            if a_moved ^ b_moved {
                self.automatic_routes_maybe_stale.insert(connection.id);
            }
        }
    }

    fn mark_external_automatic_routes_stale(&mut self) {
        let Some(area_id) = self.editor.area_id() else {
            return;
        };
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&area_id) else {
            return;
        };
        self.automatic_routes_maybe_stale.extend(
            area.get_connections()
                .iter()
                .filter(|connection| connection.routing == ConnectionRouting::Automatic)
                .map(|connection| connection.id),
        );
    }

    /// Best-effort final flush for the deliberately in-session pending queue.
    /// The OS close request cannot be held open by iced on every platform, so
    /// the toolbar warns while work is pending and this method wakes the sync
    /// worker before the window releases its mapper clone. Follow-ups still
    /// waiting for their writes' acknowledgement are queued first, beside
    /// those writes, since no window is left to wait for it.
    pub fn prepare_to_close(&mut self) {
        self.stack.queue_waiting_follow_ups(&self.mapper);
        let pending = self.mapper.get_sync_stats().pending_operations();
        let unsaved_areas = self
            .mapper
            .get_current_atlas()
            .areas()
            .filter(|area| {
                !matches!(
                    self.mapper.area_save_status(*area.get_id()),
                    smudgy_cloud::mapper::AreaSaveStatus::Saved
                )
            })
            .count();
        if pending > 0 || unsaved_areas > 0 {
            log::warn!(
                "Map editor is closing with {pending} active operation(s) across {unsaved_areas} unsaved area(s); attempting final sync"
            );
            self.mapper.sync_now();
        }
    }

    /// Builds the window with an injected clipboard, so every editor window
    /// can share one app-global clipboard (for the two-window merge workflow).
    pub fn with_clipboard(
        window_id: window::Id,
        mapper: Mapper,
        cloud: CloudHandles,
        clipboard: SharedClipboard,
        server_name: String,
        map_scopes: MapScopes,
        location: Option<(AreaId, i32)>,
    ) -> Self {
        // Open where the player is, when the mapper knows the room; else the
        // list's first map.
        let atlas = mapper.get_current_atlas();
        let here = location.and_then(|(area_id, room_number)| {
            let room = atlas.get_room(&RoomKey::new(area_id, RoomNumber(room_number)))?;
            // A Secret's room opens its map.
            let map = atlas.map_of(&area_id).unwrap_or(area_id);
            Some((map, area_id, room))
        });
        let first_area = here
            .as_ref()
            .map(|(map, _, _)| *map)
            .or_else(|| area_list::first_area_id(&atlas, &mapper.session_area_ids()));

        let (mut panes, area_list_pane) = pane_grid::State::new(PaneKind::AreaList);
        let mapper_for_remaps = mapper.clone();

        if let Some((canvas_pane, split)) =
            panes.split(pane_grid::Axis::Vertical, area_list_pane, PaneKind::Canvas)
        {
            panes.resize(split, 0.18);

            if let Some((_, split)) =
                panes.split(pane_grid::Axis::Vertical, canvas_pane, PaneKind::Inspector)
            {
                panes.resize(split, 0.72);
            }
        }

        let mut window = Self {
            window_id,
            editor: MapEditor::new(mapper.clone(), first_area),
            mapper,
            cloud,
            panes,
            stack: commands::CommandStack::default(),
            inspector: inspector::State::default(),
            tag_index: tags::IndexCache::default(),
            last_seen_rev: None,
            last_seen_place_revs: Vec::new(),
            pending_select: None,
            clipboard,
            clipboard_cut_in_flight: None,
            clipboard_reposition_in_flight: None,
            consecutive_pastes: 0,
            editor_notice: None,
            automatic_route_generation: 0,
            pending_automatic_route: None,
            automatic_route_preview: None,
            automatic_routes_maybe_stale: HashSet::new(),
            pending_new_room_links: HashMap::new(),
            recovering_new_room_link: None,
            modal: None,
            renaming_area: None,
            map_menu_open: false,
            folder_menu: None,
            multi: multi_select::MultiSelect::default(),
            secrets: secrets::SecretsState::default(),
            panel: panels::PanelState::default(),
            context_menu: None,
            moving: false,
            running_move: None,
            move_history: moves::MoveHistory::default(),
            moved_rooms: None,
            pending_copied_area: None,
            sharers: None,
            family_index: FamilyIndex::default(),
            // Seeded None so the first Tick fetches the sharer index (the
            // mapper's revision will differ from this).
            last_seen_sync_revision: None,
            last_seen_auth_projection_revision: None,
            atlases: Vec::new(),
            collapsed_folders: HashSet::new(),
            renaming_atlas: None,
            local_atlas_ids: HashSet::new(),
            signin_banner_dismissed: smudgy_core::models::settings::load_settings()
                .dismissed_signin_banner_version
                .as_deref()
                == Some(env!("CARGO_PKG_VERSION")),
            server_name: (!server_name.is_empty()).then_some(server_name),
            map_scopes,
            // The Unassigned group starts collapsed per the plan.
            scope_all: false,
            scope_menu_open: false,
            map_list_filter: String::new(),
            clans: clan_maps::ClanState::default(),
            default_atlases: default_atlases::DefaultAtlases::default(),
            room_remaps: mapper_for_remaps.subscribe_room_remaps(),
        };
        window.refresh_default_atlases();
        // Default to This-server scope only when a server context exists.
        window.scope_all = window.server_name.is_none();
        window.collapsed_folders.insert(FolderKey::Unassigned);
        let editable = window.canvas_editable();
        window.editor.set_editable(editable);
        // Centered on the player's room, with its marker shown.
        if let Some((_, area_id, room)) = here {
            window
                .editor
                .set_player_location(Some(RoomKey::new(area_id, room.get_room_number())));
            window.editor.center_on(
                iced::Point::new(room.get_x(), room.get_y()),
                room.get_level(),
            );
        }
        window.inspector.resync(&window.mapper, &window.editor);
        window
    }

    /// The server entry this editor scopes to (for the daemon's per-entry
    /// exclusion fan-out), or `None` when it has no session context.
    #[must_use]
    pub fn server_name(&self) -> Option<&str> {
        self.server_name.as_deref()
    }

    /// Reads the server's default folders again from its settings.
    fn refresh_default_atlases(&mut self) {
        self.default_atlases = default_atlases::DefaultAtlases::load(self.server_name.as_deref());
    }

    /// Creation-associates: a cloud atlas created from a session-scoped editor is
    /// associated with that session's entry (nothing user-created starts
    /// unassigned). Local atlases stay entry-isolated. Returns the change event
    /// so the daemon persists and fans it out.
    fn associate_new_atlas(&mut self, atlas_id: AtlasId) -> Option<Event> {
        let server = self.server_name.clone()?;
        // Query the mapper (not the tick-refreshed cache) so a just-created
        // local atlas is recognized immediately and left entry-isolated.
        if self.mapper.local_atlas_ids().contains(&atlas_id) {
            return None;
        }
        let delta = ScopeDelta::SetAtlasEntry {
            atlas_id,
            entry: server,
            show: true,
        };
        self.map_scopes.apply(&delta);
        Some(Event::ScopeAssociationsChanged(vec![delta]))
    }

    /// Creation-associates: a cloud *atlas-less* area created from a
    /// session-scoped editor gets an area-level association (an area filed into
    /// an atlas is scoped by its atlas, so it needs none). Local/ephemeral areas
    /// stay entry-isolated.
    fn associate_new_area(&mut self, area_id: AreaId) -> Option<Event> {
        let server = self.server_name.clone()?;
        let atlas = self.mapper.get_current_atlas();
        let has_atlas = atlas
            .get_area(&area_id)
            .and_then(|area| area.meta().atlas_id)
            .is_some();
        if has_atlas
            || self.mapper.area_storage(&area_id) == MapStorage::Session
            || self.mapper.local_area_ids().contains(&area_id)
        {
            return None;
        }
        let delta = ScopeDelta::SetAreaEntry {
            area_id,
            entry: server,
            show: true,
        };
        self.map_scopes.apply(&delta);
        Some(Event::ScopeAssociationsChanged(vec![delta]))
    }

    /// Bind-on-use (editor signal): opening an area of an *unassigned* cloud
    /// atlas from a session-scoped editor's This-server tree associates that
    /// atlas (or atlas-less area) with the session's entry. Only in the
    /// This-server scope — opening from the All view is browsing, not homing —
    /// and only for a currently-unassigned target, so it self-limits (a second
    /// open is already Here). No toast: the editor's own tree makes the move
    /// visible, and the Servers checklist is the immediate undo.
    fn associate_opened_area(&mut self, area_id: AreaId) -> Option<Event> {
        let server = self.server_name.clone()?;
        if self.scope_all {
            return None;
        }
        if self.mapper.area_storage(&area_id) == MapStorage::Session
            || self.mapper.local_area_ids().contains(&area_id)
        {
            return None;
        }
        let atlas_id = self
            .mapper
            .get_current_atlas()
            .get_area(&area_id)
            .and_then(|area| area.meta().atlas_id);
        if let Some(atlas_id) = atlas_id
            && self.mapper.local_atlas_ids().contains(&atlas_id)
        {
            return None;
        }
        let unassigned = match atlas_id {
            Some(atlas_id) => {
                self.map_scopes.atlas_scope(&atlas_id, &server) == ScopeState::Unassigned
            }
            None => self.map_scopes.area_scope(&area_id, &server) == ScopeState::Unassigned,
        };
        if !unassigned {
            return None;
        }
        let delta = match atlas_id {
            Some(atlas_id) => ScopeDelta::SetAtlasEntry {
                atlas_id,
                entry: server,
                show: true,
            },
            None => ScopeDelta::SetAreaEntry {
                area_id,
                entry: server,
                show: true,
            },
        };
        self.map_scopes.apply(&delta);
        Some(Event::ScopeAssociationsChanged(vec![delta]))
    }

    /// Fetches received grants (for the sharer index) and the area list (for
    /// the copy-family index) to rebuild both. No-op when signed out — these
    /// endpoints require auth, and `family_token` is cloud-only anyway.
    fn fetch_sharers(&self) -> Task<Message> {
        if !self.cloud.snapshot.get().signed_in {
            return Task::none();
        }
        let client = self.cloud.client.clone();
        let mapper = self.mapper.clone();
        let auth_projection_revision = self.mapper.auth_projection_revision();
        Task::perform(
            async move {
                let grants = client.shares(ShareDirection::Received).await;
                // The area list is the only carrier of `family_token`
                // (`get_area`/cache never include it), so we refetch it here
                // rather than reading the geometry cache.
                let areas = mapper.list_areas().await;
                (grants, areas)
            },
            move |(grants, areas)| Message::IndicesLoaded {
                auth_projection_revision,
                grants,
                areas,
            },
        )
    }

    /// §5 recipient homing: on first sight of a shared atlas (or genuinely
    /// atlas-less shared area) the viewer has no association for, match the
    /// covering grants' grantor-authored `host_hints` against the local server
    /// entries and, on ≥1 match, associate it with those entries **silently**.
    /// No match leaves it Unassigned; the §3 convergence machinery takes over.
    /// Defaults apply only on first sight and never overwrite an existing local
    /// association (§5.4). Runs here — the sole point holding both the received
    /// grant rows (with `host_hints`) and the area inventory (for the
    /// area→atlas mapping that lets an area-scope grant home its atlas).
    /// Returns the deltas applied (empty when nothing homed), applying each to
    /// this editor's own snapshot and handing the same list to the daemon so it
    /// replays them against the authoritative copy — never a whole-store
    /// snapshot, which would clobber a concurrent write.
    fn apply_recipient_homing(
        &mut self,
        grants: &[ShareGrantRow],
        areas: &[Area],
    ) -> Vec<ScopeDelta> {
        // The local server entries are the homing evidence (§5.1). Hosts are
        // consumed here once and never stored as keys.
        let entries: Vec<HostEntry> = smudgy_core::models::server::list_servers()
            .unwrap_or_default()
            .into_iter()
            .map(|server| HostEntry {
                name: server.name,
                host: server.config.host,
                port: server.config.port,
            })
            .collect();

        // area id -> atlas id, so an area-scope grant can home the *atlas* its
        // area belongs to (the §6 walkthrough: area grants from "Cities" home
        // the Cities folder).
        let area_atlas: std::collections::HashMap<AreaId, AtlasId> = areas
            .iter()
            .filter_map(|area| area.atlas_id.map(|atlas_id| (area.id, atlas_id)))
            .collect();

        // Aggregate the covering grants' host hints per homing target.
        let mut atlas_hints: std::collections::HashMap<AtlasId, Vec<String>> =
            std::collections::HashMap::new();
        let mut area_hints: std::collections::HashMap<AreaId, Vec<String>> =
            std::collections::HashMap::new();
        for row in grants {
            let hints = row.grant.host_hints.clone().unwrap_or_default();
            match (row.grant.atlas_id, row.grant.area_id) {
                (Some(atlas_id), _) => atlas_hints.entry(atlas_id).or_default().extend(hints),
                (None, Some(area_id)) => match area_atlas.get(&area_id) {
                    Some(atlas_id) => atlas_hints.entry(*atlas_id).or_default().extend(hints),
                    None => area_hints.entry(area_id).or_default().extend(hints),
                },
                (None, None) => {}
            }
        }

        let mut deltas = Vec::new();
        for (atlas_id, hints) in atlas_hints {
            // First sight only. The MarkSeen delta consumes it in every branch,
            // so a no-match atlas isn't re-evaluated once its entries drift.
            if self.map_scopes.has_seen(&atlas_id) {
                continue;
            }
            if self.map_scopes.atlas_entries(&atlas_id).is_empty() {
                let matched = match_host_hints(&hints, &entries);
                if !matched.is_empty() {
                    deltas.push(ScopeDelta::SetAtlasEntries {
                        atlas_id,
                        entries: matched,
                    });
                }
            }
            deltas.push(ScopeDelta::MarkSeen { atlas_id });
        }
        for (area_id, hints) in area_hints {
            // Atlas-less areas have no first-seen ledger; the "no existing
            // association" guard stands in — homing applies once (setting a
            // record) and thereafter the record itself blocks re-homing, so a
            // later user edit is never overwritten.
            if self.map_scopes.area_entries(&area_id).is_empty() {
                let matched = match_host_hints(&hints, &entries);
                if !matched.is_empty() {
                    deltas.push(ScopeDelta::SetAreaEntries {
                        area_id,
                        entries: matched,
                    });
                }
            }
        }
        // Apply optimistically to this editor's snapshot so its tree reflects
        // the homing before the daemon's mirrored store returns.
        for delta in &deltas {
            self.map_scopes.apply(delta);
        }
        deltas
    }

    /// Refetches the owned-atlas inventory (folder names + counts). Resolves
    /// against the session's mapper, so it works for both cloud atlases (signed
    /// in) and local atlases. The caller gates on
    /// [`Mapper::has_credential`] so a signed-out cloud-only session doesn't
    /// 401 on every tick.
    fn fetch_atlases(&self) -> Task<Message> {
        let mapper = self.mapper.clone();
        let auth_projection_revision = self.mapper.auth_projection_revision();
        Task::perform(async move { mapper.list_atlases().await }, move |result| {
            Message::AtlasesLoaded {
                auth_projection_revision,
                result,
            }
        })
    }

    /// The copy-family of `area_id`: the connected component over **both** the
    /// cache's `copied_from` edges (owner-only provenance) **and** the
    /// per-viewer `family_token` buckets (which also link *received*
    /// copies the viewer can't see provenance for). Always contains `area_id`;
    /// a lone area yields just itself.
    pub(super) fn copy_family(&self, area_id: AreaId) -> Vec<AreaId> {
        let atlas = self.mapper.get_current_atlas();
        copy_family_in(
            &copied_from_edges(&atlas),
            &self.family_index.token_by_area,
            area_id,
        )
    }

    /// The set of cache-renderable areas that belong to a copy-family with ≥2
    /// members (over `copied_from` edges + `family_token`), for the area list's
    /// family badge. A `family_token` is only served when the viewer can see
    /// ≥2 members, so any token-bearing area is in a family by construction.
    pub(super) fn family_members(&self) -> HashSet<AreaId> {
        let atlas = self.mapper.get_current_atlas();
        family_members_in(&copied_from_edges(&atlas), &self.family_index.token_by_area)
    }

    /// Attribution line for a shared area, enriched from the sharer index:
    /// "Shared by {sharer} · owned by {owner}" when the sharer differs from
    /// the owner, "Shared by {sharer}" otherwise. Falls back to the area's
    /// own `owner_nickname` when the index hasn't loaded.
    pub(super) fn sharer_attribution(&self, area_id: AreaId) -> String {
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&area_id) else {
            return crate::i18n::t!("mapper-shared-by-friend");
        };
        let meta = area.meta();
        let owner_label = meta
            .owner_nickname
            .clone()
            .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend"));

        let resolved = self
            .sharers
            .as_ref()
            .and_then(|index| index.sharer_for(area_id, meta.atlas_id));

        match resolved {
            Some(sharer) => {
                let sharer_label = sharer
                    .nickname
                    .clone()
                    .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend"));
                // Re-share: name both the sharer and the underlying owner.
                if meta
                    .owner_id
                    .is_some_and(|owner_id| owner_id != sharer.user_id)
                {
                    // Owner handle: prefer GET /areas (meta), fall back to the
                    // grant's owner_nickname, then to "a friend".
                    let owner_label = meta
                        .owner_nickname
                        .clone()
                        .or_else(|| sharer.owner_nickname.clone())
                        .unwrap_or_else(|| crate::i18n::t!("mapper-a-friend"));
                    crate::i18n::t!(
                        "mapper-shared-by-owner-pair",
                        "sharer" => sharer_label,
                        "owner" => owner_label
                    )
                } else {
                    crate::i18n::t!("mapper-shared-by-person", "person" => sharer_label)
                }
            }
            // Index not loaded / scope not covered: keep today's owner-handle
            // attribution as the fallback.
            None => crate::i18n::t!("mapper-shared-by-person", "person" => owner_label),
        }
    }

    /// This window's mapper (shared, cheaply cloneable). The daemon uses it
    /// to fan disabled-area changes out across windows.
    pub fn mapper(&self) -> &Mapper {
        &self.mapper
    }

    pub fn can_undo(&self) -> bool {
        self.stack.can_undo()
            || (self.stack.next_undo().is_none() && self.move_history.undo.is_some())
    }

    pub fn can_redo(&self) -> bool {
        self.stack.can_redo()
            || (self.stack.next_redo().is_none() && self.move_history.redo.is_some())
    }

    /// Drops all history, the command stack's and the last move's.
    fn clear_history(&mut self) {
        self.stack.clear();
        self.move_history = moves::MoveHistory::default();
    }

    /// Lets go of the history when rooms of the open map moved between its
    /// places outside this window's own moves (which drain their own
    /// announcements when they land): the history names rooms where they no
    /// longer are, so undo could edit the wrong room. Says so when there was
    /// any. A move of this window's in flight leaves the queue to it.
    fn forget_history_moved_elsewhere(&mut self, atlas: &AtlasCache) {
        if self.moving {
            return;
        }
        let events = self.room_remaps.take();
        let Some(open) = self.editor.area_id() else {
            return;
        };
        let moved_here = rooms_moved_on(atlas, open, &events);
        let had_history = !self.stack.is_empty()
            || self.move_history.undo.is_some()
            || self.move_history.redo.is_some();
        if moved_here && had_history {
            self.clear_history();
            self.editor_notice = Some((
                Instant::now(),
                crate::i18n::t!("mapper-history-cleared-elsewhere"),
            ));
        }
    }

    /// The window title shown in the OS titlebar.
    pub fn title(&self) -> String {
        let atlas = self.mapper.get_current_atlas();
        self.editor
            .area_id()
            .and_then(|id| atlas.get_area(&id))
            .map_or_else(
                || crate::i18n::t!("mapper-window-title"),
                |area| {
                    crate::i18n::t!(
                        "mapper-window-area-title",
                        "area" => area.get_name()
                    )
                },
            )
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::event::listen_with(editor_hotkeys),
            iced::event::listen_with(multi_select::modifier_events),
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
        ])
    }

    /// Drops what is selected but no longer on the open map (deleted by
    /// another writer), saying so; whether anything was dropped.
    fn drop_missing_selection(&mut self, atlas: &AtlasCache) -> bool {
        let Some(area_id) = self.editor.area_id() else {
            return false;
        };
        let missing: Vec<_> = self
            .editor
            .selection()
            .iter()
            .filter(|entity| {
                let Some(area) = atlas.get_area(&area_id) else {
                    return true;
                };
                match entity {
                    EntityId::Room(number) => area.get_room(number).is_none(),
                    EntityId::SourceRoom(source, number) => {
                        smudgy_map_widget::sources::source_room(&area, *source, *number).is_none()
                    }
                    // A Secret's links and drawings stay selectable like the
                    // map's.
                    EntityId::Connection(id) => area.find_connection(*id).is_none(),
                    EntityId::Label(id) => area.find_label(id).is_none(),
                    EntityId::Shape(id) => area.find_shape(id).is_none(),
                }
            })
            .collect();
        if missing.is_empty() {
            return false;
        }
        for entity in missing {
            self.editor.remove_from_selection(entity);
        }
        self.editor_notice = Some((Instant::now(), crate::i18n::t!("mapper-selection-removed")));
        true
    }

    /// Re-reads the active area's revision, marking the current cache state
    /// as already seen so the next [`Message::Tick`] doesn't treat our own
    /// writes as external changes.
    fn refresh_seen_rev(&mut self) {
        let atlas = self.mapper.get_current_atlas();
        let area = self.editor.area_id().and_then(|id| atlas.get_area(&id));
        self.last_seen_rev = area.as_ref().map(|area| area.get_rev());
        self.last_seen_place_revs = area.as_deref().map(place_revs).unwrap_or_default();
    }

    /// The viewer's effective capabilities on the active area; `None` when
    /// no area is active.
    fn active_access(&self) -> Option<AreaAccess> {
        let atlas = self.mapper.get_current_atlas();
        self.editor
            .area_id()
            .and_then(|id| atlas.get_area(&id))
            .map(|area| area.effective_access())
    }

    /// Whether mutations are allowed in the active area. View-only shared
    /// areas (and "no area") gate every mutation entry point through this.
    fn can_edit_active_area(&self) -> bool {
        self.active_access().is_some_and(|access| access.can_edit)
    }

    /// The number a new room in `place` of `area_id` takes (see
    /// [`source_rooms::new_room_number`]): every placing gesture asks here
    /// rather than the place's own maximum. `None` when the area is gone,
    /// or, with a notice saying so, when it has no room numbers left.
    fn new_room_number(&mut self, area_id: AreaId, place: SourceId) -> Option<RoomNumber> {
        match source_rooms::new_room_number(&self.mapper, area_id, place) {
            Ok(number) => Some(number),
            Err(CloudError::AreaNotFound(_)) => None,
            Err(_) => {
                self.editor_notice =
                    Some((Instant::now(), crate::i18n::t!("mapper-no-room-numbers")));
                None
            }
        }
    }

    /// Whether the share dialog applies to the active area: the viewer may
    /// share the map (its owner, or a grantee holding `can_reshare`), or
    /// manages access to one of its Secrets.
    fn can_share_active_area(&self) -> bool {
        let Some(area_id) = self.editor.area_id() else {
            return false;
        };
        self.mapper
            .get_current_atlas()
            .get_area(&area_id)
            .is_some_and(|area| modals::may_share(&area))
    }

    /// Whether "Copy to my maps" applies to the active area: a shared (not
    /// owned) area whose grant includes `can_copy`. Owned areas never offer
    /// it — copying your own map is just creating an area.
    fn can_copy_active_area(&self) -> bool {
        self.active_access()
            .is_some_and(|access| !access.is_owner && access.can_copy)
    }

    /// Whether the viewer owns `area_id`. Rename/delete are owner-only
    /// (`PUT`/`DELETE /areas` uniform-404 otherwise).
    fn area_owned(&self, area_id: AreaId) -> bool {
        self.mapper
            .get_current_atlas()
            .get_area(&area_id)
            .is_some_and(|area| area.is_owned())
    }

    /// Whether every one of `mutations` writes somewhere the viewer may
    /// write: the map needs edit access; a Secret its own actions; Private
    /// only read access to the map.
    fn may_write(&self, mutations: &[commands::Mutation]) -> bool {
        self.write_refusal(mutations).is_none()
    }

    /// Why one of `mutations` writes somewhere the viewer may not, as the
    /// notice to show; `None` when every one may land. Each operation needs
    /// its own action where it writes (`add` to create, `remove` to delete,
    /// `edit` otherwise), on whichever map it writes. A Secret the viewer no
    /// longer reads is "no longer available", never named.
    fn write_refusal(&self, mutations: &[commands::Mutation]) -> Option<String> {
        let atlas = self.mapper.get_current_atlas();
        mutations.iter().find_map(|mutation| {
            let (area_id, actions) = commands::needed_actions(mutation);
            let Some(area) = atlas.get_area(&area_id) else {
                return Some(crate::i18n::t!("cloud-error-secret-unavailable"));
            };
            actions.iter().find_map(|(place, action)| {
                let source = match place {
                    commands::Writes::Place(source) => *source,
                    commands::Writes::Label(id) => place_of(&area, EntityId::Label(*id)),
                    commands::Writes::Shape(id) => place_of(&area, EntityId::Shape(*id)),
                };
                write_refusal_in(&area, source, action)
            })
        })
    }

    /// Whether the viewer may change every selected item where it lives: a
    /// Secret's or Private's content follows that place's actions, the map's
    /// the map's edit access. False with nothing selected.
    pub(super) fn selection_writable(&self) -> bool {
        let selection = self.editor.selection();
        !selection.is_empty() && selection.iter().all(|entity| self.entity_writable(entity))
    }

    /// Whether the viewer may change `entity` where it lives.
    fn entity_writable(&self, entity: EntityId) -> bool {
        self.editor
            .area_id()
            .and_then(|area_id| self.mapper.get_current_atlas().get_area(&area_id))
            .is_some_and(|area| secrets::can_write(&area, place_of(&area, entity)))
    }

    /// Whether undo may revert the next entry where it wrote (a move back
    /// is checked by the server).
    fn may_undo(&self) -> bool {
        match self.stack.next_undo() {
            Some(command) => self.may_write(command.undo_mutations()),
            None => self.move_history.undo.is_some() && !self.moving,
        }
    }

    /// Whether redo may apply the next entry where it writes.
    fn may_redo(&self) -> bool {
        match self.stack.next_redo() {
            Some(command) => self.may_write(command.redo_mutations()),
            None => self.move_history.redo.is_some() && !self.moving,
        }
    }

    /// Says why the next undo (or redo) entry can't replay where it wrote:
    /// a place now view only, or a Secret no longer available.
    fn notice_history_refusal(&mut self, redo: bool) {
        let mutations = if redo {
            self.stack
                .next_redo()
                .map(|command| command.redo_mutations())
        } else {
            self.stack
                .next_undo()
                .map(|command| command.undo_mutations())
        };
        if let Some(refusal) = mutations.and_then(|mutations| self.write_refusal(mutations)) {
            self.editor_notice = Some((Instant::now(), refusal));
        }
    }

    /// What a selection change resets. The canvas reports its own changes
    /// (`SelectionChanged`); selections made in code call this after.
    fn selection_reset(&mut self) {
        self.secrets.place_menu_open = false;
        self.secrets.place_started = None;
    }

    /// Whether the canvas takes edits at all: on a map the viewer can edit,
    /// or where "Add to" lets them add (Private on a view-only map). Each
    /// request is still checked against the place it writes.
    fn canvas_editable(&self) -> bool {
        !self.moving && (self.can_edit_active_area() || self.can_add_here())
    }

    /// Shows `area_id` on the canvas, leaving what the map list has chosen.
    fn open_area(&mut self, area_id: AreaId) -> Update<Message, Event> {
        if self.editor.area_id() == Some(area_id) {
            return Update::none();
        }
        self.secrets.add_to = None;
        self.secrets.viewing = None;
        self.secrets.page = None;
        self.secrets.color_picker = None;
        self.secrets.draft = None;
        self.secrets.confirming_delete = false;
        self.secrets.error = None;
        self.context_menu = None;
        self.clear_automatic_route_state();
        self.automatic_routes_maybe_stale.clear();
        self.editor.set_area(Some(area_id));
        let editable = self.canvas_editable();
        self.editor.set_editable(editable);
        // Undo history is area-local by design.
        self.clear_history();
        // Creation tools are meaningless in a view-only area.
        if !self.canvas_editable() && self.editor.tool() != Tool::Select {
            self.editor.set_tool(Tool::Select);
        }
        self.refresh_seen_rev();
        self.inspector.resync(&self.mapper, &self.editor);
        // Bind-on-use: opening an unassigned atlas from this session's
        // This-server tree homes it here.
        let event = self.associate_opened_area(area_id);
        Update::new(self.panel_fetches(), event)
    }

    /// After the open map is deleted: the first map left, if any.
    fn show_first_area(&mut self) {
        let next_area = area_list::first_area_id(
            &self.mapper.get_current_atlas(),
            &self.mapper.session_area_ids(),
        );
        self.editor.set_area(next_area);
        let editable = self.canvas_editable();
        self.editor.set_editable(editable);
        if !self.canvas_editable() && self.editor.tool() != Tool::Select {
            self.editor.set_tool(Tool::Select);
        }
    }

    /// The viewer's own folders, on this device and in the cloud,
    /// name-sorted: where the maps they make and move can be filed.
    fn own_folders(&self) -> Vec<folder_picker::OwnFolder> {
        folder_picker::own_folders(&self.atlases, |atlas_id| {
            self.mapper.atlas_storage(atlas_id)
        })
    }

    /// The folder of the open map, for a new map to default to.
    fn open_map_folder(&self) -> Option<AtlasId> {
        let area_id = self.editor.area_id()?;
        self.mapper
            .get_current_atlas()
            .get_area(&area_id)
            .and_then(|area| area.meta().atlas_id)
    }

    /// The destinations a map can move to: each of the viewer's folders
    /// (signed out, only those on this device), name-sorted. A map always
    /// moves into a folder.
    fn folder_destinations(&self) -> Vec<(MapDestination, String)> {
        let signed_in = self.cloud.snapshot.get().signed_in;
        self.own_folders()
            .into_iter()
            .filter(|folder| signed_in || folder.storage == MapStorage::Local)
            .map(|folder| {
                (
                    MapDestination::in_atlas(folder.storage, folder.id),
                    format!(
                        "{} — {}",
                        folder.name,
                        match folder.storage {
                            MapStorage::Cloud => crate::i18n::t!("mapper-save-cloud"),
                            _ => crate::i18n::t!("mapper-save-local"),
                        }
                    ),
                )
            })
            .collect()
    }

    /// Builds, applies, and records a mutation command, mapping its async
    /// completions back into window messages.
    ///
    /// This is the central capability gate: every entity edit (inspector,
    /// canvas, hotkeys, paste) funnels through here, and each mutation is
    /// checked against the place it writes, so nothing lands where the
    /// viewer can't write even if some UI affordance slips through.
    fn push_command(&mut self, command: Option<commands::Command>) -> Update<Message, Event> {
        self.push_command_tracked(command).0
    }

    /// The mutation funnel plus the operation ids assigned to compound CAS
    /// envelopes. Most callers do not need the ids; new-room Link gestures
    /// retain one so a number collision can restore their preview.
    fn push_command_tracked(
        &mut self,
        command: Option<commands::Command>,
    ) -> (Update<Message, Event>, Vec<OperationId>) {
        match command {
            Some(command) => {
                if let Some(refusal) = self.write_refusal(command.redo_mutations()) {
                    log::info!("map editor: ignoring mutation — not writable here");
                    self.editor_notice = Some((Instant::now(), refusal));
                    return (Update::none(), Vec::new());
                }
                // A new edit ends what redo could bring back.
                self.move_history.redo = None;
                // Every map mutation supersedes the immutable solver
                // snapshot; signal cancellation before touching the cache.
                self.clear_automatic_route_state();
                let (task, operation_ids) =
                    self.stack.push_and_apply_tracked(&self.mapper, command);
                if let Some(error) = self.stack.take_last_error() {
                    self.editor_notice = Some((Instant::now(), error));
                }
                self.refresh_seen_rev();
                (
                    Update::with_task(task.map(Message::CommandCompleted)),
                    operation_ids,
                )
            }
            None => (Update::none(), Vec::new()),
        }
    }

    /// Detect the special CAS conflict where another editor claimed the
    /// proposed room number. Discard that optimistic command first; its
    /// completion recreates the link under a fresh number.
    fn begin_new_room_link_conflict_recovery(&mut self, atlas: &Arc<AtlasCache>) -> Task<Message> {
        if self.recovering_new_room_link.is_some() {
            return Task::none();
        }
        let Some(area_id) = self.editor.area_id() else {
            return Task::none();
        };
        let Some(operation_id) = self.mapper.conflicted_operation_id(area_id) else {
            return Task::none();
        };
        let Some(draft) = self.pending_new_room_links.get(&operation_id).cloned() else {
            return Task::none();
        };
        let commands::NewExitTarget::NewRoom { room_number, .. } = draft.target else {
            return Task::none();
        };
        let Some(area) = atlas.get_area(&area_id) else {
            return Task::none();
        };
        if area.get_room(&room_number).is_none() {
            // A different structural dependency failed; leave the ordinary
            // conflict review UI in control.
            return Task::none();
        }

        self.pending_new_room_links.remove(&operation_id);
        self.stack.discard_operation(operation_id);
        self.recovering_new_room_link = Some((operation_id, draft, room_number));
        let mapper = self.mapper.clone();
        Task::perform(
            async move {
                let result = mapper
                    .resolve_conflict(area_id, false)
                    .await
                    .map_err(|error| display_error(&error));
                (operation_id, result)
            },
            |(operation_id, result)| Message::NewRoomLinkConflictResolved {
                operation_id,
                result,
            },
        )
    }

    /// Recreate a number-collision link under a fresh room number once the
    /// conflict has been resolved. If the user opened a dialog while the
    /// refetch ran, the periodic tick retries after it closes.
    fn finish_new_room_link_conflict_recovery(
        &mut self,
        expected: Option<OperationId>,
    ) -> Task<Message> {
        let Some((operation_id, draft, _)) = &self.recovering_new_room_link else {
            return Task::none();
        };
        let area_id = draft.area_id;
        if expected.is_some_and(|expected| expected != *operation_id)
            || self.mapper.is_operation_pending(area_id, *operation_id)
            || self.modal.is_some()
            || self.editor.area_id() != Some(area_id)
            || !self.can_edit_active_area()
        {
            return Task::none();
        }
        if self.mapper.get_current_atlas().get_area(&area_id).is_none() {
            self.recovering_new_room_link = None;
            self.editor_notice = Some((Instant::now(), crate::i18n::t!("mapper-link-area-gone")));
            return Task::none();
        }
        let Some(new_number) = self.new_room_number(area_id, SourceId::Map) else {
            self.recovering_new_room_link = None;
            return Task::none();
        };
        let Some((_, mut draft, old_number)) = self.recovering_new_room_link.take() else {
            return Task::none();
        };
        let commands::NewExitTarget::NewRoom { room_number, .. } = &mut draft.target else {
            return Task::none();
        };
        *room_number = new_number;
        let task = self.create_link(draft, SourceId::Map, SourceId::Map).task;
        self.editor_notice = Some((
            Instant::now(),
            crate::i18n::t!(
                "mapper-link-room-taken",
                "old" => old_number.to_string(),
                "new" => new_number.to_string()
            ),
        ));
        task
    }

    /// Creates a Link-tool link in `place` and selects it (or the room it
    /// made, which goes into `place` too). `from_source` says whose room
    /// the draft's `from` is. A new map room's link is remembered so a
    /// number collision can recreate it.
    fn create_link(
        &mut self,
        draft: modals::LinkDraft,
        place: SourceId,
        from_source: SourceId,
    ) -> Update<Message, Event> {
        let pending = draft.clone();
        let target = draft.target;
        let link = commands::NewLink {
            area_id: draft.area_id,
            place,
            from: PlacedRoom {
                source: from_source,
                number: draft.from,
            },
            from_direction: draft.from_direction,
            to: target,
            to_direction: draft.to_direction,
        };
        let Some((command, connection_id)) = commands::create_link(
            &link,
            commands::NewLinkOptions {
                one_way: draft.one_way,
                pair_with: draft
                    .pair_with_candidate
                    .then_some(draft.pair_candidate)
                    .flatten(),
                ..commands::NewLinkOptions::default()
            },
        ) else {
            return Update::none();
        };
        let (update, operation_ids) = self.push_command_tracked(Some(command));
        if let commands::NewExitTarget::NewRoom { room_number, .. } = target {
            if let Some(operation_id) = operation_ids.first().copied() {
                debug_assert_eq!(operation_ids.len(), 1);
                // The collision recovery recreates map rooms; a Secret's
                // new room keeps the number it was given.
                if place.is_map() {
                    self.pending_new_room_links.insert(operation_id, pending);
                }
                self.editor.select(
                    PlacedRoom {
                        source: place,
                        number: room_number,
                    }
                    .into(),
                );
                self.selection_reset();
            } else {
                self.editor_notice =
                    Some((Instant::now(), crate::i18n::t!("mapper-link-not-queued")));
            }
        } else if self
            .mapper
            .get_current_atlas()
            .get_area(&draft.area_id)
            .is_some_and(|area| area.find_connection(connection_id).is_some())
        {
            self.editor.select(EntityId::Connection(connection_id));
            self.selection_reset();
        }
        self.inspector.resync(&self.mapper, &self.editor);
        update
    }

    fn handle_mutation_request(&mut self, request: MutationRequest) -> Update<Message, Event> {
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        let creates = matches!(
            request,
            MutationRequest::PlaceRoom { .. }
                | MutationRequest::CreateLabel { .. }
                | MutationRequest::CreateShape { .. }
        );
        // New content goes where "Add to" points; everything else edits in
        // the place it lives.
        let allowed = match &request {
            _ if creates => self.can_add_here(),
            MutationRequest::MoveSelection { .. } => self.selection_writable(),
            MutationRequest::ResizeEntity { entity, .. } => self.entity_writable(*entity),
            MutationRequest::UpdateConnection { connection_id, .. }
            | MutationRequest::DeleteWaypoint { connection_id, .. } => {
                self.entity_writable(EntityId::Connection(*connection_id))
            }
            // A link's place is decided below from its ends; the funnel
            // then checks that place, and says why when it refuses.
            MutationRequest::CreateExit { .. } => true,
            _ => self.can_edit_active_area(),
        };
        if !allowed {
            log::info!("map editor: ignoring mutation request — not writable here");
            return Update::none();
        }
        let add_to = self.add_to();

        match request {
            MutationRequest::MoveSelection { offset } => {
                self.mark_moved_automatic_routes_stale();
                let update = self.push_command(commands::move_selection(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    self.editor.selection(),
                    offset,
                ));
                // Canvas-originated edits refresh the inspector's view of
                // the moved entities.
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            MutationRequest::PlaceRoom { at } => {
                let Some(room_number) = self.new_room_number(area_id, add_to) else {
                    return Update::none();
                };
                let (command, entity) = if add_to.is_map() {
                    (
                        commands::create_room(area_id, room_number, at, self.editor.level()),
                        EntityId::Room(room_number),
                    )
                } else {
                    (
                        source_rooms::create_room(
                            area_id,
                            add_to,
                            room_number,
                            at,
                            self.editor.level(),
                        ),
                        EntityId::SourceRoom(add_to, room_number),
                    )
                };
                let update = self.push_command(Some(command));
                self.editor.select(entity);
                self.selection_reset();
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            MutationRequest::CreateExit {
                from,
                from_direction,
                to,
                to_direction,
                one_way,
            } => {
                // A link goes into the Secret (or Private) at either end;
                // between map rooms, or to a new room, it goes where "Add
                // to" points, so a Secret's passage never lands in the map.
                let secret_end = |room: PlacedRoom| (!room.source.is_map()).then_some(room.source);
                let to_secret = match to {
                    ExitTarget::Room(room) => secret_end(room),
                    ExitTarget::Empty(_) | ExitTarget::Dangling(_) => None,
                };
                let place = match (secret_end(from), to_secret) {
                    (Some(from_place), Some(to_place)) if from_place != to_place => {
                        self.editor_notice =
                            Some((Instant::now(), crate::i18n::t!("mapper-link-two-secrets")));
                        return Update::none();
                    }
                    (Some(place), _) | (None, Some(place)) => place,
                    (None, None) => add_to,
                };
                let atlas = self.mapper.get_current_atlas();
                let pair_candidate = if let ExitTarget::Room(to_room) = to {
                    let Some(area) = atlas.get_area(&area_id) else {
                        return Update::none();
                    };
                    // A reciprocal to pair with lives in the link's place.
                    document::Document::of(&area, place).and_then(|document| {
                        reciprocal_pair_candidate(
                            document.content(),
                            area_id,
                            document.room_of(from)?,
                            document.room_of(to_room)?,
                            from_direction,
                            to_direction,
                        )
                    })
                } else {
                    None
                };
                let target = match to {
                    ExitTarget::Room(room) => commands::NewExitTarget::Room(room),
                    ExitTarget::Empty(at) => {
                        let Some(room_number) = self.new_room_number(area_id, place) else {
                            return Update::none();
                        };
                        commands::NewExitTarget::NewRoom {
                            room_number,
                            at,
                            level: self.editor.level(),
                        }
                    }
                    ExitTarget::Dangling(_) => commands::NewExitTarget::Dangling,
                };
                self.create_link(
                    modals::LinkDraft {
                        area_id,
                        from: from.number,
                        target,
                        from_direction,
                        to_direction,
                        one_way,
                        pair_candidate,
                        pair_with_candidate: pair_candidate.is_some(),
                    },
                    place,
                    from.source,
                )
            }
            MutationRequest::CreateLabel { rect } if !add_to.is_map() => {
                let (command, label_id) = source_rooms::create_label(
                    area_id,
                    add_to,
                    commands::new_label_args(rect, self.editor.level()),
                );
                let update = self.push_command(Some(command));
                self.editor.select(EntityId::Label(label_id));
                self.selection_reset();
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            MutationRequest::CreateShape { rect } if !add_to.is_map() => {
                let (command, shape_id) = source_rooms::create_shape(
                    area_id,
                    add_to,
                    commands::new_shape_args(rect, self.editor.level()),
                );
                let update = self.push_command(Some(command));
                self.editor.select(EntityId::Shape(shape_id));
                self.selection_reset();
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            MutationRequest::CreateLabel { rect } => {
                let update = self.push_command(Some(commands::create_label(
                    area_id,
                    rect,
                    self.editor.level(),
                )));
                self.pending_select = self.stack.last_command_id();
                self.editor.clear_selection();
                update
            }
            MutationRequest::CreateShape { rect } => {
                let update = self.push_command(Some(commands::create_shape(
                    area_id,
                    rect,
                    self.editor.level(),
                )));
                self.pending_select = self.stack.last_command_id();
                self.editor.clear_selection();
                update
            }
            MutationRequest::ResizeEntity { entity, rect } => {
                let update = self.push_command(commands::resize_entity(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    entity,
                    rect,
                ));
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            MutationRequest::UpdateConnection {
                connection_id,
                updates,
                description,
            } => {
                // Canvas port drags are endpoint edits: they coalesce with
                // the inspector's endpoint field (not with waypoint edits)
                // and leave an Automatic route stale exactly like the
                // inspector path does.
                let endpoint_edit = updates.endpoint_a.is_some() || updates.endpoint_b.is_some();
                let field = if endpoint_edit {
                    commands::FieldId::Endpoint
                } else {
                    commands::FieldId::RoutePoints
                };
                if endpoint_edit
                    && self
                        .mapper
                        .get_current_atlas()
                        .get_area(&area_id)
                        .is_some_and(|area| {
                            area.get_connection(connection_id)
                                .is_some_and(|connection| {
                                    connection.routing == smudgy_cloud::ConnectionRouting::Automatic
                                })
                        })
                {
                    self.automatic_routes_maybe_stale.insert(connection_id);
                }
                let update = self.push_command(commands::edit_connection(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    connection_id,
                    field,
                    updates,
                    description,
                ));
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            MutationRequest::DeleteWaypoint {
                connection_id,
                index,
            } => {
                let update = self.push_command(commands::delete_waypoint(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    connection_id,
                    index,
                ));
                self.editor.clear_selected_waypoint();
                update
            }
        }
    }

    fn delete_selection(&mut self) -> Update<Message, Event> {
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        if !self.selection_writable() {
            return Update::none();
        }
        if let Some((connection_id, index)) = self.editor.selected_waypoint() {
            return self.handle_mutation_request(MutationRequest::DeleteWaypoint {
                connection_id,
                index,
            });
        }
        let atlas = self.mapper.get_current_atlas();
        let command = match self.editor.selection().single() {
            Some(EntityId::Connection(connection_id)) => {
                commands::delete_connection(&atlas, area_id, connection_id)
            }
            _ => commands::delete_selection(&atlas, area_id, self.editor.selection()),
        };
        self.editor.clear_selection();
        let update = self.push_command(command);
        self.inspector.resync(&self.mapper, &self.editor);
        update
    }

    /// Copies one source's selected content only with its Copy permission.
    /// A refusal retains the existing clipboard.
    fn copy_selection(&mut self, include_boundary_links: bool) -> bool {
        let Some((snapshot, _)) = self.source_copy_snapshot(include_boundary_links) else {
            return false;
        };
        if snapshot.is_empty() {
            return false;
        }
        self.clipboard.store(Arc::new(snapshot));
        self.consecutive_pastes = 0;
        true
    }

    fn cut_copied_selection(&mut self) -> Update<Message, Event> {
        self.stage_cut()
    }

    fn request_copy_selection(&mut self, cut_after_copy: bool) -> Update<Message, Event> {
        if cut_after_copy {
            return self.stage_cut();
        }
        let Some((_, boundary_count)) = self.source_copy_snapshot(false) else {
            return Update::none();
        };
        if boundary_count > 0 {
            self.modal = Some(modals::Modal::ConfirmCopySelection {
                boundary_count,
                include_boundary_links: false,
                cut_after_copy,
            });
            return Update::none();
        }
        if !self.copy_selection(false) {
            return Update::none();
        }
        if cut_after_copy {
            self.cut_copied_selection()
        } else {
            Update::none()
        }
    }

    /// Pastes the clipboard: with its center at `at` (snapped to the grid),
    /// or, without a point, where the shortcut's cascade puts it.
    fn paste_clipboard(&mut self, at: Option<iced::Point>) -> Update<Message, Event> {
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        let clipboard = self.clipboard.load_full();
        if let Some(cut) = &clipboard.cut {
            return self.paste_cut(cut.clone(), at);
        }
        if clipboard.is_empty() || !self.can_add_here() {
            return Update::none();
        }
        if !self.can_paste_copy(&clipboard) {
            return Update::none();
        }
        let source = self.add_to();
        let atlas = self.mapper.get_current_atlas();
        let target = atlas.get_area(&area_id).and_then(|area| {
            if source.is_map() {
                Some(area_id)
            } else {
                area.source_layers()
                    .iter()
                    .find(|layer| layer.source() == source)
                    .map(|layer| layer.area_id())
            }
        });
        let next_room_number = if clipboard.rooms.is_empty() {
            None
        } else {
            let Some(number) = self.new_room_number(area_id, source) else {
                return Update::none();
            };
            Some(number)
        };

        // Same-area pastes cascade so copies don't land exactly on their
        // sources; cross-area pastes preserve exact positions (and source
        // room numbers where vacant) so merged-back changes line up.
        let same_area = target.is_some() && clipboard.source_area_id == target;
        let offset = if let Some(at) = at {
            let center = commands::clipboard_center(&clipboard).unwrap_or(at);
            smudgy_map_widget::viewport::snap_offset(at - center)
        } else if same_area {
            self.consecutive_pastes += 1;
            #[allow(clippy::cast_precision_loss)]
            let step = self.consecutive_pastes as f32;
            Vector::new(step, step)
        } else {
            Vector::new(0.0, 0.0)
        };

        let (command, pasted_rooms, skipped_connections) = commands::paste_into_source(
            &self.mapper.get_current_atlas(),
            area_id,
            source,
            &clipboard,
            self.editor.level(),
            offset,
            next_room_number,
        );
        if skipped_connections > 0 {
            self.editor_notice = Some((
                Instant::now(),
                crate::i18n::t!("mapper-paste-links-skipped", "count" => skipped_connections),
            ));
        }
        let Some(command) = command else {
            if skipped_connections == 0 {
                // Not a skip: the paste itself couldn't be built (too many
                // operations for one envelope). Say so instead of nothing.
                self.editor_notice =
                    Some((Instant::now(), crate::i18n::t!("mapper-paste-too-large")));
            }
            return Update::none();
        };
        let pasted_entities: Vec<_> = command
            .redo_mutations()
            .iter()
            .flat_map(|mutation| match mutation {
                commands::Mutation::AreaBatch { operations, .. }
                | commands::Mutation::SourceBatch { operations, .. } => operations.as_slice(),
                _ => &[],
            })
            .filter_map(|operation| match operation {
                smudgy_cloud::mutation::AreaMutation::CreateLabel { body } => {
                    body.id.map(EntityId::Label)
                }
                smudgy_cloud::mutation::AreaMutation::CreateShape { body } => {
                    body.id.map(EntityId::Shape)
                }
                _ => None,
            })
            .collect();
        let (update, applied) = self.push_command_tracked(Some(command));
        if applied.is_empty() {
            return update;
        }

        // All identities are minted before the atomic paste is queued.
        self.pending_select = None;
        self.editor.clear_selection();
        for room_number in pasted_rooms {
            self.editor.add_to_selection(if source.is_map() {
                EntityId::Room(room_number)
            } else {
                EntityId::SourceRoom(source, room_number)
            });
        }
        for entity in pasted_entities {
            self.editor.add_to_selection(entity);
        }
        self.selection_reset();
        self.inspector.resync(&self.mapper, &self.editor);
        update
    }

    fn handle_hotkey(&mut self, hotkey: Hotkey) -> Update<Message, Event> {
        // Any shortcut closes the context menu (its own keys never get here).
        self.context_menu = None;
        // Several maps chosen: the canvas is hidden, so its edits wait;
        // Delete offers to delete what is chosen.
        if self.multi.selection.is_multi() {
            match hotkey {
                Hotkey::Delete if self.modal.is_none() => {
                    return self.update_multi(multi_select::MultiMessage::Requested(
                        multi_select::Action::Delete,
                    ));
                }
                Hotkey::Delete
                | Hotkey::Nudge(..)
                | Hotkey::Copy
                | Hotkey::Cut
                | Hotkey::Paste
                | Hotkey::ContextMenu
                | Hotkey::MoveLevelUp
                | Hotkey::MoveLevelDown
                | Hotkey::FocusTags
                | Hotkey::Tab
                | Hotkey::TabBack => return Update::none(),
                Hotkey::Undo
                | Hotkey::Redo
                | Hotkey::Escape
                | Hotkey::LevelUp
                | Hotkey::LevelDown => {}
            }
        }
        match hotkey {
            // Uncaptured, so no text input has focus: go to the tag input
            // where the selected rooms have one.
            Hotkey::FocusTags => {
                if self.tag_input_place().is_some() {
                    Update::with_task(iced::widget::operation::focus(tags::input_id(
                        self.window_id,
                    )))
                } else {
                    Update::none()
                }
            }
            Hotkey::Tab => {
                if self.tag_suggestions().is_empty() {
                    Update::with_task(inspector::focus_step(self.window_id, false))
                } else {
                    Update::with_task(
                        iced::widget::operation::is_focused(tags::input_id(self.window_id)).map(
                            |focused| Message::Inspector(inspector::Message::TagCompleted(focused)),
                        ),
                    )
                }
            }
            Hotkey::TabBack => Update::with_task(inspector::focus_step(self.window_id, true)),
            Hotkey::Delete => self.delete_selection(),
            Hotkey::Copy => self.request_copy_selection(false),
            Hotkey::Cut => self.request_copy_selection(true),
            Hotkey::Paste => self.paste_clipboard(None),
            Hotkey::ContextMenu => {
                if let Some((at, map)) = self.editor.canvas_center() {
                    let menu = context_menu::ContextMenu::keyboard(at, map);
                    self.context_menu = context_menu::applies(self, menu).then_some(menu);
                }
                Update::none()
            }
            Hotkey::Nudge(dx, dy, step) => {
                if let Some((connection_id, handle)) = self.editor.selected_connection_handle() {
                    let atlas = self.mapper.get_current_atlas();
                    let Some(area_id) = self.editor.area_id() else {
                        return Update::none();
                    };
                    let Some(map) = atlas.get_area(&area_id) else {
                        return Update::none();
                    };
                    // A Secret's link moves in its Secret's document.
                    let Some((area, _)) = map.connection_document(connection_id) else {
                        return Update::none();
                    };
                    let Some(connection) = area.get_connection(connection_id) else {
                        return Update::none();
                    };
                    let (updates, description) = match handle {
                        SelectedConnectionHandle::Waypoint(index) => {
                            let Some(point) = connection.route_points.get(index).copied() else {
                                return Update::none();
                            };
                            let target = smudgy_cloud::MapPoint::new(
                                point.x + dx as f32 * step,
                                point.y + dy as f32 * step,
                            );
                            let points = if connection.segment_shape == SegmentShape::Orthogonal {
                                let Some(render) =
                                    area.get_room_connections().iter().find(|item| {
                                        item.connection_id == connection_id
                                            && item.from_level == self.editor.level()
                                    })
                                else {
                                    return Update::none();
                                };
                                let Some(points) =
                                    smudgy_cloud::connection_geometry::reroute_for_waypoint_move(
                                        &connection.route_points,
                                        index,
                                        render.geometry.stub_tip_a,
                                        render.geometry.stub_tip_b,
                                        target,
                                    )
                                else {
                                    return Update::none();
                                };
                                points
                            } else {
                                let mut points = connection.route_points.clone();
                                points[index] = target;
                                points
                            };
                            (
                                ConnectionUpdates {
                                    routing: Some(ConnectionRouting::Manual),
                                    route_points: Some(points),
                                    ..ConnectionUpdates::default()
                                },
                                "Nudge connection waypoint",
                            )
                        }
                        SelectedConnectionHandle::PortA | SelectedConnectionHandle::PortB => {
                            let mut endpoint = match handle {
                                SelectedConnectionHandle::PortA => connection.endpoint_a,
                                SelectedConnectionHandle::PortB => {
                                    let Some(endpoint) = connection.endpoint_b else {
                                        return Update::none();
                                    };
                                    endpoint
                                }
                                SelectedConnectionHandle::Waypoint(_) => unreachable!(),
                            };
                            // The stored selection can outlive the handle
                            // (an up/down endpoint has no port); refuse to
                            // nudge a port the geometry no longer offers.
                            let port_exists = area
                                .get_room_connections()
                                .iter()
                                .find(|item| {
                                    item.connection_id == connection_id
                                        && item.from_level == self.editor.level()
                                })
                                .is_some_and(|item| {
                                    item.geometry.handles.iter().any(|offered| {
                                        use smudgy_cloud::connection_geometry::Handle;
                                        match handle {
                                            SelectedConnectionHandle::PortA => {
                                                matches!(offered, Handle::PortA(_))
                                            }
                                            SelectedConnectionHandle::PortB => {
                                                matches!(offered, Handle::PortB(_))
                                            }
                                            SelectedConnectionHandle::Waypoint(_) => false,
                                        }
                                    })
                                });
                            if !port_exists {
                                return Update::none();
                            }
                            // Port offsets are normalized wall coordinates.
                            // Keep the same fine/default/coarse relationship
                            // as waypoint nudging without jumping an entire
                            // room edge at once.
                            let along_wall = match endpoint.side {
                                RoomSide::North | RoomSide::South => dx as f32,
                                RoomSide::East | RoomSide::West => dy as f32,
                            };
                            if along_wall == 0.0 {
                                return Update::none();
                            }
                            endpoint.port_offset =
                                (endpoint.port_offset + along_wall * step * 0.1).clamp(0.0, 1.0);
                            endpoint.port_mode = PortMode::Manual;
                            let Some(updates) = inspector::endpoint_updates(
                                area,
                                connection_id,
                                endpoint,
                                handle == SelectedConnectionHandle::PortB,
                            ) else {
                                return Update::none();
                            };
                            (updates, "Nudge connection port")
                        }
                    };
                    return self.handle_mutation_request(MutationRequest::UpdateConnection {
                        connection_id,
                        updates,
                        description,
                    });
                }
                if self.editor.selection().is_empty() {
                    return Update::none();
                }
                #[allow(clippy::cast_precision_loss)]
                let offset = Vector::new(dx as f32, dy as f32);
                self.handle_mutation_request(MutationRequest::MoveSelection { offset })
            }
            Hotkey::Undo => {
                // Undo replays mutations through the Mapper, so it honors
                // the same per-place gate as push_command: history recorded
                // before a permission downgrade must not replay where the
                // viewer can no longer write. (Message::Undo, the toolbar
                // button, routes here too.)
                if !self.may_undo() {
                    self.notice_history_refusal(false);
                    return Update::none();
                }
                self.clear_automatic_route_state();
                if self.stack.next_undo().is_none() {
                    return self.replay_move(false);
                }
                let task = self.stack.undo(&self.mapper).map(Message::CommandCompleted);
                if let Some(error) = self.stack.take_last_error() {
                    self.editor_notice = Some((Instant::now(), error));
                }
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::with_task(task)
            }
            Hotkey::Redo => {
                // Same per-place gate as Hotkey::Undo above.
                if !self.may_redo() {
                    self.notice_history_refusal(true);
                    return Update::none();
                }
                self.clear_automatic_route_state();
                if self.stack.next_redo().is_none() {
                    return self.replay_move(true);
                }
                let task = self.stack.redo(&self.mapper).map(Message::CommandCompleted);
                if let Some(error) = self.stack.take_last_error() {
                    self.editor_notice = Some((Instant::now(), error));
                }
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::with_task(task)
            }
            Hotkey::Escape => {
                if self.modal.is_some() {
                    self.cancel_move_review();
                    self.modal = None;
                } else if self.close_link_picker() {
                    // A room was being picked for a link; it isn't now.
                } else if self.clear_automatic_route_state() {
                    // A route being found or previewed is cancelled.
                } else if self.renaming_area.is_some() {
                    self.renaming_area = None;
                } else if self.renaming_atlas.is_some() {
                    self.renaming_atlas = None;
                } else if self.panel.atlas_rename.is_some() {
                    self.panel.atlas_rename = None;
                } else if self.multi.confirming_delete {
                    self.multi.confirming_delete = false;
                } else if self.multi.selection.is_chosen() {
                    // Back to the open map alone.
                    self.multi.selection.clear();
                } else if self.secrets.draft.is_some() || self.secrets.confirming_delete {
                    return self.update_secrets(secrets::SecretsMessage::Cancelled);
                } else if self.editor.clear_selected_connection_handle() {
                    self.inspector.resync(&self.mapper, &self.editor);
                } else if self.editor.tool() == Tool::Select {
                    self.editor.clear_selection();
                } else {
                    self.editor.set_tool(Tool::Select);
                }
                Update::none()
            }
            Hotkey::LevelUp => {
                self.editor.set_level(self.editor.level() + 1);
                Update::none()
            }
            Hotkey::LevelDown => {
                self.editor.set_level(self.editor.level() - 1);
                Update::none()
            }
            Hotkey::MoveLevelUp | Hotkey::MoveLevelDown => {
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let delta = if matches!(hotkey, Hotkey::MoveLevelUp) {
                    1
                } else {
                    -1
                };
                let update = self.push_command(commands::shift_selection_level(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    self.editor.selection(),
                    delta,
                ));
                // Follow the rooms to their new level so the selection
                // stays visible.
                if !self.editor.selection().is_empty() {
                    self.editor
                        .set_level_keeping_selection(self.editor.level() + delta);
                }
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
        }
    }

    pub fn update(&mut self, message: Message) -> Update<Message, Event> {
        let changing_access = self.access_dialog_open();
        let reviewing_move = matches!(self.modal, Some(modals::Modal::ReviewMove { .. }));
        let mut update = self.update_window(message);
        if reviewing_move
            && self.moving
            && self.moved_rooms.is_none()
            && !matches!(self.modal, Some(modals::Modal::ReviewMove { .. }))
        {
            // Opening another dialog abandons the preview, but a commit
            // already sent to the service continues to its acknowledgement.
            self.abandon_move_review();
        }
        // Who has access may have changed: the panels list it again.
        if changing_access && !self.access_dialog_open() {
            update.task = Task::batch([update.task, self.reload_panel_access()]);
        }
        // Anything selected on the canvas, however it came to be, lets go
        // of an atlas chosen in the map list. A name typed in an atlas's
        // panel lasts while that atlas is chosen.
        if !self.editor.selection().is_empty() {
            self.multi.selection.drop_folder();
        }
        if self
            .panel
            .atlas_rename
            .as_ref()
            .is_some_and(|(atlas_id, _)| self.multi.selection.folder() != Some(*atlas_id))
        {
            self.panel.atlas_rename = None;
        }
        // A picked place that went away says so, and a tag typed for a
        // place the input no longer writes goes, whatever message changed
        // where content goes.
        self.notice_gone_place();
        self.drop_stale_tag_input();
        // Where rooms overlap, the canvas resolves toward "Add to"'s; it
        // learns the current place after every message, before the next
        // pointer event reaches it.
        self.editor.set_add_to(self.add_to());
        // While the link editor's picker is open the canvas picks rooms.
        self.sync_link_picking();
        update
    }

    fn update_window(&mut self, message: Message) -> Update<Message, Event> {
        match message {
            Message::Editor(message) => {
                let update = self.editor.update(message).map_message(Message::Editor);

                let mut result = Update::with_task(update.task);

                if let Some(event) = update.event {
                    match event {
                        map_editor::Event::HoveredRoomChanged(_) => {}
                        map_editor::Event::ContextMenu { at, map } => {
                            self.selection_reset();
                            self.inspector.resync(&self.mapper, &self.editor);
                            let menu = context_menu::ContextMenu::new(at, map);
                            self.context_menu = context_menu::applies(self, menu).then_some(menu);
                        }
                        map_editor::Event::SelectionChanged => {
                            self.selection_reset();
                            self.inspector.resync(&self.mapper, &self.editor);
                        }
                        map_editor::Event::RequestMutation(request) => {
                            result = self.handle_mutation_request(request);
                        }
                        map_editor::Event::RoomPicked(room) => {
                            result = self.room_picked(room);
                        }
                    }
                }

                result
            }
            Message::PaneResized(event) => {
                self.panes.resize(event.split, event.ratio);
                Update::none()
            }
            Message::AreaSelected(area_id) => {
                // Opening a map shows it alone.
                self.multi.selection.clear();
                self.multi.confirming_delete = false;
                self.open_area(area_id)
            }
            Message::ToolSelected(tool) => {
                self.context_menu = None;
                if tool != Tool::Select && !self.canvas_editable() {
                    return Update::none();
                }
                self.editor.set_tool(tool);
                Update::none()
            }
            Message::LevelUp => self.handle_hotkey(Hotkey::LevelUp),
            Message::LevelDown => self.handle_hotkey(Hotkey::LevelDown),
            Message::Undo => self.handle_hotkey(Hotkey::Undo),
            Message::Redo => self.handle_hotkey(Hotkey::Redo),
            Message::Hotkey(window_id, hotkey) => {
                if window_id == self.window_id {
                    self.handle_hotkey(hotkey)
                } else {
                    Update::none()
                }
            }
            Message::Tick => {
                // External writers (sessions, other windows) bump the area
                // rev; receiving this message is itself what schedules the
                // repaint. Our own commits call refresh_seen_rev, so a
                // mismatch here means an external change worth resyncing
                // the inspector for.
                let atlas = self.mapper.get_current_atlas();
                self.pending_new_room_links.retain(|operation_id, draft| {
                    self.mapper
                        .is_operation_pending(draft.area_id, *operation_id)
                });

                // A background sync (cache swap) bumps the mapper's revision;
                // refetch the sharer index and atlas inventory then (and on
                // the first tick). Both are no-ops while signed out.
                let sync_rev = self.mapper.sync_revision();
                let mut sharer_task = Task::none();
                clan_map_share::dismiss_stale(self);
                let auth_projection_rev = self.mapper.auth_projection_revision();
                if self.last_seen_auth_projection_revision != Some(auth_projection_rev) {
                    self.last_seen_auth_projection_revision = Some(auth_projection_rev);
                    self.local_atlas_ids = self.mapper.local_atlas_ids();
                    self.atlases
                        .retain(|atlas| self.local_atlas_ids.contains(&atlas.id));
                    self.sharers = None;
                    self.family_index = FamilyIndex::default();
                    self.clans = clan_maps::ClanState::default();
                }
                if self.last_seen_sync_revision != Some(sync_rev) {
                    self.last_seen_sync_revision = Some(sync_rev);
                    sharer_task = self.fetch_sharers();
                    // Refresh which folders are local-tier, so the view can
                    // gate cloud-only affordances (cheap clone of a small set).
                    self.local_atlas_ids = self.mapper.local_atlas_ids();
                    // The mapper reports a credential when *any* of its
                    // backends can serve atlases (always true once a local
                    // tier exists); gating here keeps a signed-out cloud-only
                    // session from 401-ing every tick and clears stale folders.
                    if self.mapper.has_credential() {
                        sharer_task = Task::batch([
                            sharer_task,
                            self.fetch_atlases(),
                            clan_maps::fetch(self),
                        ]);
                    } else if !self.atlases.is_empty() {
                        self.atlases.clear();
                    }
                }

                self.notice_gone_place();
                self.forget_history_moved_elsewhere(&atlas);
                sharer_task = Task::batch([sharer_task, self.refresh_secret_page()]);
                // A notice expires on its own; "Moving…" stays until the
                // move lands.
                if !self.moving
                    && self
                        .editor_notice
                        .as_ref()
                        .is_some_and(|(shown, _)| shown.elapsed() >= NOTICE_TTL)
                {
                    self.editor_notice = None;
                }
                let recovery_errors = self.mapper.take_mutation_recovery_errors();
                if !recovery_errors.is_empty() {
                    self.editor_notice = Some((
                        Instant::now(),
                        crate::i18n::t!("mapper-edits-not-recovered"),
                    ));
                }

                // A clone we requested selects itself once sync lands it.
                if let Some(pending) = self.pending_copied_area
                    && atlas.get_area(&pending).is_some()
                {
                    self.pending_copied_area = None;
                    let mut update = self.update(Message::AreaSelected(pending));
                    update.task = Task::batch([update.task, sharer_task]);
                    return update;
                }

                // A mid-session permission downgrade (the owner lowered
                // can_edit; sync flipped the access fingerprint) makes the
                // recorded history unreplayable — drop it so undo/redo
                // can't mutate a now view-only area's cache.
                if !self.moving && !self.stack.is_empty() && !self.canvas_editable() {
                    self.clear_history();
                }
                let editable = self.canvas_editable();
                if !self.moving && !editable && self.clear_automatic_route_state() {
                    self.editor_notice = Some((
                        Instant::now(),
                        crate::i18n::t!("mapper-route-access-changed"),
                    ));
                }
                self.editor.set_editable(editable);

                let rev = self
                    .editor
                    .area_id()
                    .and_then(|id| atlas.get_area(&id))
                    .map(|area| area.get_rev());
                if rev != self.last_seen_rev {
                    let discarded_route = self.clear_automatic_route_state();
                    self.mark_external_automatic_routes_stale();
                    if discarded_route {
                        self.editor_notice =
                            Some((Instant::now(), crate::i18n::t!("mapper-route-map-changed")));
                    }
                    self.last_seen_rev = rev;
                    self.drop_missing_selection(&atlas);
                    self.inspector.resync(&self.mapper, &self.editor);
                }
                // A Secret or Private changed alone (another writer, or the
                // server taking the viewer's own write): selected rooms it
                // deleted leave the selection, and the selection's tags are
                // read again. Nothing else is, so no draft is lost.
                let seen_places = self
                    .editor
                    .area_id()
                    .and_then(|id| atlas.get_area(&id))
                    .map(|area| place_revs(&area))
                    .unwrap_or_default();
                if seen_places != self.last_seen_place_revs {
                    self.last_seen_place_revs = seen_places;
                    if self.drop_missing_selection(&atlas) {
                        self.inspector.resync(&self.mapper, &self.editor);
                    } else {
                        self.inspector.reread_tags(&self.mapper, &self.editor);
                    }
                }
                let recovery_task = self.begin_new_room_link_conflict_recovery(&atlas);
                let retry_task = self.finish_new_room_link_conflict_recovery(None);
                // Chosen maps and folders deleted elsewhere leave the
                // selection.
                let pruned = self.prune_multi_selection();
                // The panels' access lists follow the open map, the chosen
                // atlas and the sign-in.
                let access_task = self.panel_fetches();
                Update::new(
                    Task::batch([
                        sharer_task,
                        recovery_task,
                        retry_task,
                        pruned.task,
                        access_task,
                    ]),
                    pruned.event,
                )
            }
            Message::Inspector(message) => self.update_inspector(message),
            Message::Links(message) => self.update_links(message),
            Message::AutomaticRouteSolved {
                generation,
                snapshot,
                result,
            } => {
                let matches_pending =
                    self.pending_automatic_route
                        .as_ref()
                        .is_some_and(|pending| {
                            pending.generation == generation && pending.snapshot == snapshot
                        });
                if !matches_pending {
                    return Update::none();
                }
                self.pending_automatic_route = None;
                if result == AutoRouteResult::Cancelled {
                    return Update::none();
                }

                let atlas = self.mapper.get_current_atlas();
                let current = atlas.get_area(&snapshot.area_id).and_then(|area| {
                    automatic_routing::capture(&area, snapshot.connection_id).ok()
                });
                let Some((current_snapshot, request)) = current else {
                    self.clear_automatic_route_state();
                    self.editor_notice =
                        Some((Instant::now(), crate::i18n::t!("mapper-route-link-changed")));
                    return Update::none();
                };
                if current_snapshot != snapshot {
                    self.clear_automatic_route_state();
                    self.editor_notice =
                        Some((Instant::now(), crate::i18n::t!("mapper-route-map-changed")));
                    return Update::none();
                }

                match result {
                    AutoRouteResult::Solved { route_points, .. } => {
                        let Some(geometry) = smudgy_cloud::automatic_routing::validated_geometry(
                            &request,
                            &route_points,
                        ) else {
                            self.editor_notice =
                                Some((Instant::now(), crate::i18n::t!("mapper-route-invalid")));
                            return Update::none();
                        };
                        self.automatic_route_preview = Some(AutomaticRoutePreview {
                            snapshot: snapshot.clone(),
                            route_points,
                        });
                        self.editor.set_automatic_route_preview(Some((
                            snapshot.connection_id,
                            Arc::new(geometry),
                        )));
                        self.editor_notice = None;
                    }
                    AutoRouteResult::NoRoute => {
                        self.editor_notice =
                            Some((Instant::now(), crate::i18n::t!("mapper-route-none")));
                    }
                    AutoRouteResult::LimitReached => {
                        self.editor_notice =
                            Some((Instant::now(), crate::i18n::t!("mapper-route-limit")));
                    }
                    AutoRouteResult::Cancelled => {}
                }
                Update::none()
            }
            Message::AutomaticRouteAccepted => {
                let Some(preview) = self.automatic_route_preview.clone() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let current = atlas.get_area(&preview.snapshot.area_id).and_then(|area| {
                    automatic_routing::capture(&area, preview.snapshot.connection_id).ok()
                });
                let Some((current_snapshot, request)) = current else {
                    self.clear_automatic_route_state();
                    self.editor_notice =
                        Some((Instant::now(), crate::i18n::t!("mapper-route-link-changed")));
                    return Update::none();
                };
                if current_snapshot != preview.snapshot
                    || smudgy_cloud::automatic_routing::validate_route(
                        &request,
                        &preview.route_points,
                    ) != RouteValidation::Valid
                {
                    self.clear_automatic_route_state();
                    self.editor_notice =
                        Some((Instant::now(), crate::i18n::t!("mapper-route-map-changed")));
                    return Update::none();
                }

                let area_id = preview.snapshot.area_id;
                let connection_id = preview.snapshot.connection_id;
                self.clear_automatic_route_state();
                self.automatic_routes_maybe_stale.remove(&connection_id);
                self.push_command(commands::accept_automatic_route(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    connection_id,
                    preview.route_points,
                ))
            }
            Message::AutomaticRouteCancelled => {
                self.clear_automatic_route_state();
                self.editor_notice = None;
                Update::none()
            }
            Message::CutRepositionCompleted { id, acknowledged } => {
                self.finish_cut_reposition(id, acknowledged);
                Update::none()
            }
            Message::CommandCompleted(outcome) => {
                // Drag-rect creations and pastes select their entities as
                // the creates complete. The marker stays set (command ids
                // are unique) so multi-entity pastes accumulate; a nice side
                // effect is that redoing the command re-selects its
                // recreations.
                if let Some(pending) = self.pending_select {
                    match &outcome {
                        commands::Outcome::Label {
                            command,
                            result: Ok(id),
                            ..
                        } if *command == pending => {
                            self.editor.add_to_selection(EntityId::Label(*id));
                        }
                        commands::Outcome::Shape {
                            command,
                            result: Ok(id),
                            ..
                        } if *command == pending => {
                            self.editor.add_to_selection(EntityId::Shape(*id));
                        }
                        _ => {}
                    }
                }

                let task = if let commands::Outcome::Acknowledged {
                    command,
                    application,
                    acknowledged,
                } = outcome
                {
                    // Another map's write follows this map's, once it is
                    // acknowledged.
                    let task =
                        self.stack
                            .follow_up(&self.mapper, command, application, acknowledged);
                    if let Some(error) = self.stack.take_last_error() {
                        self.editor_notice = Some((Instant::now(), error));
                    }
                    task.map(Message::CommandCompleted)
                } else {
                    self.stack.resolve(outcome);
                    Task::none()
                };
                // Creates land in the cache only on completion (backend
                // assigns the id), so dependent UI refreshes now.
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::with_task(task)
            }
            Message::SetCurrentLocation(area_id, room_number) => {
                // The editor never auto-switches area when the player moves;
                // only the marker updates.
                let location = room_number.map(|room_number| RoomKey {
                    area_id,
                    room_number: RoomNumber(room_number),
                });
                if self.editor.set_player_location(location) {
                    // The canvas isn't animated, so nothing else requests a
                    // redraw when the marker moves; without this the move only
                    // shows on the next incidental repaint (a Tick, a hover, or
                    // the gameplay map's pan animation).
                    Update::with_task(request_repaint())
                } else {
                    Update::none()
                }
            }
            Message::NewAreaRequested => {
                // Every new map goes in a folder: the open map's when it is
                // one of the viewer's, else one in the default storage, else
                // a new one they name.
                let folder = folder_picker::FolderPicker::for_new_map(
                    self.own_folders(),
                    self.cloud.snapshot.get().signed_in,
                    self.open_map_folder(),
                );
                self.modal = Some(modals::Modal::CreateArea {
                    name: String::new(),
                    error: None,
                    folder,
                    busy: false,
                    ownership: None,
                });
                Update::none()
            }
            Message::CreateAreaNameChanged(value) => {
                if let Some(modals::Modal::CreateArea { name, .. }) = &mut self.modal {
                    *name = value;
                }
                Update::none()
            }
            Message::CreateAreaOwnership(picked) => {
                if let Some(modals::Modal::CreateArea {
                    ownership: Some(choice),
                    ..
                }) = &mut self.modal
                    && choice.clan_allowed
                {
                    choice.picked = picked;
                }
                Update::none()
            }
            Message::FolderPicker(message) => {
                match &mut self.modal {
                    Some(modals::Modal::CreateArea { folder, .. }) => folder.update(message),
                    Some(modals::Modal::CopyArea(dialog)) => dialog.folder.update(message),
                    Some(modals::Modal::ConfirmDeleteAtlas {
                        folder: Some(folder),
                        ..
                    }) => folder.update(message),
                    _ => {}
                }
                Update::none()
            }
            Message::CreateAreaConfirmed => {
                let Some(modals::Modal::CreateArea {
                    name,
                    error,
                    folder,
                    busy,
                    ownership,
                }) = &mut self.modal
                else {
                    return Update::none();
                };
                let name = name.trim().to_string();
                if name.is_empty() || *busy || !folder.ready() {
                    return Update::none();
                }
                *busy = true;
                *error = None;
                let storage = folder.storage();
                let mapper = self.mapper.clone();
                // A new folder is made first; its arrival carries on here.
                let Some(atlas_id) = folder.chosen() else {
                    let folder_name = folder.new_folder_name().unwrap_or_default();
                    return Update::with_task(folder_picker::make_folder(
                        mapper,
                        folder_name,
                        storage,
                    ));
                };
                let ownership = ownership.map(|choice| choice.picked);
                Update::with_task(Task::perform(
                    async move {
                        let destination = MapDestination::in_atlas(storage, atlas_id);
                        match ownership {
                            Some(ownership) => {
                                mapper
                                    .create_clan_area_at(name, destination, ownership)
                                    .await
                            }
                            None => mapper.create_area_at(name, destination).await,
                        }
                    },
                    |result| Message::AreaCreated(result.map_err(|error| display_error(&error))),
                ))
            }
            Message::AreaCreated(result) => {
                match result {
                    Ok(area_id) => {
                        self.modal = None;
                        // Creation-associates: a cloud atlas-less area gets an
                        // area-level association with this session's entry
                        // (unconditional — unlike the editor-open signal, which
                        // only fires in the This-server scope).
                        let created = self.associate_new_area(area_id);
                        let mut update = self.update(Message::AreaSelected(area_id));
                        update.event = created.or(update.event);
                        return update;
                    }
                    Err(failure) => {
                        if let Some(modals::Modal::CreateArea { error, busy, .. }) = &mut self.modal
                        {
                            *error = Some(failure);
                            *busy = false;
                        }
                    }
                }
                Update::none()
            }
            Message::NewFolderMade(result) => self.new_folder_made(result),
            Message::MoveAreaNewFolder(message) => self.update_move_new_folder(message),
            // TEMPORARY(0.6.x): the loose-maps migration finished.
            Message::LooseMaps(done) => loose_maps_migration::finished(self, done),
            Message::RenameAreaStarted(area_id) => {
                // Rename is the owner's, or on a clan's map its actions';
                // never offer it otherwise.
                if !self.may_manage_area(area_id, smudgy_cloud::clans::action::RENAME_AREA) {
                    return Update::none();
                }
                let atlas = self.mapper.get_current_atlas();
                let name = atlas
                    .get_area(&area_id)
                    .map(|area| area.get_name().to_string())
                    .unwrap_or_default();
                self.renaming_area = Some((area_id, name));
                Update::none()
            }
            Message::RenameAreaChanged(value) => {
                if let Some((_, name)) = &mut self.renaming_area {
                    *name = value;
                }
                Update::none()
            }
            Message::RenameAreaCommitted => {
                if let Some((area_id, name)) = self.renaming_area.take() {
                    let name = name.trim().to_string();
                    if !name.is_empty()
                        && self.may_manage_area(area_id, smudgy_cloud::clans::action::RENAME_AREA)
                    {
                        // Area management deliberately bypasses the undo
                        // stack, but waits for backend acknowledgement.
                        let mapper = self.mapper.clone();
                        return Update::with_task(Task::perform(
                            async move {
                                mapper
                                    .rename_area(area_id, &name)
                                    .await
                                    .map_err(|error| display_error(&error))
                            },
                            Message::RenameAreaCompleted,
                        ));
                    }
                }
                Update::none()
            }
            Message::RenameAreaCompleted(result) => {
                if let Err(error) = result {
                    self.editor_notice = Some((Instant::now(), error));
                }
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::none()
            }
            Message::DeleteAreaRequested(area_id) => {
                let atlas = self.mapper.get_current_atlas();
                if let Some(area) = atlas.get_area(&area_id)
                    && self.may_manage_area(area_id, smudgy_cloud::clans::action::DELETE_AREA)
                {
                    self.modal = Some(modals::Modal::ConfirmDeleteArea {
                        area_id,
                        name: area.get_name().to_string(),
                        room_count: map_panel::room_count(&area),
                    });
                }
                Update::none()
            }
            Message::DeleteAreaConfirmed => {
                let Some(modals::Modal::ConfirmDeleteArea { area_id, .. }) = self.modal.take()
                else {
                    return Update::none();
                };
                if !self.may_manage_area(area_id, smudgy_cloud::clans::action::DELETE_AREA) {
                    return Update::none();
                }

                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move {
                        let result = mapper
                            .delete_area(area_id)
                            .await
                            .map_err(|error| display_error(&error));
                        (area_id, result)
                    },
                    |(area_id, result)| Message::DeleteAreaCompleted { area_id, result },
                ))
            }
            Message::DeleteAreaCompleted { area_id, result } => {
                if let Err(error) = result {
                    self.editor_notice = Some((Instant::now(), error));
                    return Update::none();
                }
                self.clear_history();
                if self.editor.area_id() == Some(area_id) {
                    self.show_first_area();
                }
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::none()
            }
            Message::ModalDismissed => {
                self.cancel_move_review();
                self.modal = None;
                Update::none()
            }
            Message::OpenSettingsRequested => Update::with_event(Event::OpenSettings),
            Message::DismissSigninBanner => {
                self.signin_banner_dismissed = true;
                if let Err(error) =
                    smudgy_core::models::settings::set_dismissed_signin_banner_version(env!(
                        "CARGO_PKG_VERSION"
                    ))
                {
                    log::warn!(
                        "map editor: failed to persist signed-out banner dismissal: {error}"
                    );
                }
                Update::none()
            }
            Message::SyncNowRequested => {
                self.mapper.sync_now();
                Update::none()
            }
            Message::CopyIncludeBoundaryChanged(value) => {
                if let Some(modals::Modal::ConfirmCopySelection {
                    include_boundary_links,
                    ..
                }) = &mut self.modal
                {
                    *include_boundary_links = value;
                }
                Update::none()
            }
            Message::CopySelectionConfirmed => {
                let Some(modals::Modal::ConfirmCopySelection {
                    include_boundary_links,
                    cut_after_copy,
                    ..
                }) = self.modal.take()
                else {
                    return Update::none();
                };
                if !self.copy_selection(include_boundary_links) {
                    return Update::none();
                }
                if cut_after_copy {
                    self.cut_copied_selection()
                } else {
                    Update::none()
                }
            }
            Message::NewRoomLinkConflictResolved {
                operation_id,
                result,
            } => {
                if let Err(error) = result {
                    if let Some((recovery_id, draft, _)) = self.recovering_new_room_link.take() {
                        self.pending_new_room_links.insert(recovery_id, draft);
                    }
                    self.editor_notice = Some((Instant::now(), error));
                } else {
                    return Update::with_task(
                        self.finish_new_room_link_conflict_recovery(Some(operation_id)),
                    );
                }
                Update::none()
            }
            resolution @ (Message::KeepMineRequested | Message::KeepTheirsRequested) => {
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let keep_mine = matches!(resolution, Message::KeepMineRequested);
                let discarded_operation = (!keep_mine)
                    .then(|| self.mapper.conflicted_operation_id(area_id))
                    .flatten();
                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move {
                        mapper
                            .resolve_conflict(area_id, keep_mine)
                            .await
                            .map_err(|error| display_error(&error))
                    },
                    move |result| Message::SaveResolutionCompleted {
                        result,
                        discarded_operation,
                    },
                ))
            }
            resolution @ (Message::RetrySaveRequested | Message::DiscardFailedSaveRequested) => {
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let retry = matches!(resolution, Message::RetrySaveRequested);
                let discarded_operation = (!retry)
                    .then(|| self.mapper.failed_operation_id(area_id))
                    .flatten();
                // A discarded write takes the rest of its gesture with it,
                // taken off the queue before the queue moves on: a gesture
                // never half-applies (a link made again elsewhere never
                // loses its old self without its new one).
                let rest = discarded_operation
                    .map(|operation| self.stack.operations_after(operation))
                    .unwrap_or_default();
                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move {
                        for operation in rest {
                            if let Err(error) = mapper.cancel_pending(area_id, operation).await {
                                log::warn!(
                                    "could not take back the rest of a discarded edit: {error}"
                                );
                            }
                        }
                        mapper
                            .resolve_failed(area_id, retry)
                            .await
                            .map_err(|error| display_error(&error))
                    },
                    move |result| Message::SaveResolutionCompleted {
                        result,
                        discarded_operation,
                    },
                ))
            }
            Message::SaveResolutionCompleted {
                result,
                discarded_operation,
            } => {
                match result {
                    Ok(()) => {
                        if let Some(operation_id) = discarded_operation {
                            self.stack.discard_operation(operation_id);
                            self.pending_new_room_links.remove(&operation_id);
                        }
                    }
                    Err(error) => {
                        self.editor_notice = Some((Instant::now(), error));
                    }
                }
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::none()
            }
            Message::IndicesLoaded {
                auth_projection_revision,
                grants,
                areas,
            } => {
                if auth_projection_revision != self.mapper.auth_projection_revision() {
                    return Update::none();
                }
                // §5 first-sight homing needs BOTH halves (the grants carry the
                // host hints; the areas map an area-scope grant to its atlas).
                // Bubble the resulting scope change up to the central flow so
                // it persists, fans exclusions to every mapper, and mirrors into
                // the other editors — once, consistently.
                let mut event = None;
                if let (Ok(grants), Ok(areas)) = (&grants, &areas) {
                    let deltas = self.apply_recipient_homing(grants, areas);
                    if !deltas.is_empty() {
                        event = Some(Event::ScopeAssociationsChanged(deltas));
                    }
                }
                // Each index is rebuilt independently: a transient failure on
                // one keeps the prior value rather than dropping attribution
                // or family grouping.
                match grants {
                    Ok(grants) => self.sharers = Some(SharerIndex::build(&grants)),
                    Err(error) => {
                        log::warn!("map editor: received-grants fetch failed: {error}");
                    }
                }
                match areas {
                    Ok(areas) => {
                        self.clans.index_rows(&areas);
                        self.family_index = FamilyIndex::build(&areas);
                    }
                    Err(error) => {
                        log::warn!("map editor: area-list fetch failed: {error}");
                    }
                }
                event.map_or_else(Update::none, Update::with_event)
            }
            Message::ToggleAreaEnabled(area_id) => {
                let enabled = self.mapper.is_area_enabled(&area_id);
                self.mapper.set_area_enabled(area_id, !enabled);
                Update::with_event(Event::DisabledAreasChanged(self.mapper.disabled_areas()))
            }
            Message::SetActiveCopy(area_id) => {
                // Enable the chosen member; disable every other family member.
                let family = self.copy_family(area_id);
                for member in family {
                    self.mapper.set_area_enabled(member, member == area_id);
                }
                Update::with_event(Event::DisabledAreasChanged(self.mapper.disabled_areas()))
            }
            Message::DuplicateAreaRequested => {
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(area) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                // Owner self-copy only; shared areas use "Copy to my maps".
                if !area.is_owned() {
                    return Update::none();
                }
                // The duplicate is a cloud map, filed beside its source unless
                // another cloud folder is chosen.
                let folder = folder_picker::FolderPicker::in_storage(
                    self.own_folders(),
                    MapStorage::Cloud,
                    area.meta().atlas_id,
                );
                self.modal = Some(modals::Modal::CopyArea(modals::CopyAreaDialog {
                    source: area_id,
                    source_name: area.get_name().to_string(),
                    name: format!("{} (copy)", area.get_name()),
                    // No atlas option on a duplicate (it's already yours).
                    atlas_id: None,
                    busy: false,
                    error: None,
                    atlas_report: None,
                    duplicate: true,
                    folder,
                    secrets: modals::SecretsAlong::of(&area),
                }));
                Update::none()
            }
            Message::ShareDialogRequested => match self.editor.area_id() {
                Some(area_id) if clan_map_share::is_clan_map(self, area_id) => {
                    clan_map_share::open(self, area_id)
                }
                _ => modals::open_share_dialog(self),
            },
            Message::MapAccessRequested(area_id) => clan_map_share::open(self, area_id),
            Message::ShareSecretRequested(source) => modals::open_share_dialog_on(self, source),
            Message::MapMenuToggled(open) => {
                self.map_menu_open = open;
                Update::none()
            }
            Message::Secrets(message) => self.update_secrets(message),
            Message::Panel(message) => self.update_panel(message),
            Message::Move(message) => self.update_move(message),
            Message::Multi(message) => self.update_multi(message),
            Message::ContextMenuClosed => {
                self.context_menu = None;
                Update::none()
            }
            Message::ContextMenuStep(step) => {
                if let Some(menu) = self.context_menu {
                    let rows = context_menu::actions(self, menu).len();
                    if rows > 0 {
                        let current = menu.cursor.map_or(-1, |cursor| cursor as i64);
                        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
                        let next = (current + i64::from(step)).rem_euclid(rows as i64) as usize;
                        if let Some(menu) = &mut self.context_menu {
                            menu.cursor = Some(next);
                        }
                    }
                }
                Update::none()
            }
            Message::ContextMenuActivated => {
                let action = self.context_menu.and_then(|menu| {
                    let cursor = menu.cursor?;
                    context_menu::actions(self, menu).into_iter().nth(cursor)
                });
                match action {
                    Some(action) => self.update(Message::ContextAction(action)),
                    None => Update::none(),
                }
            }
            Message::ContextAction(action) => {
                // Changing pages keeps the menu open.
                if let Some(page) = action.page() {
                    if let Some(menu) = &mut self.context_menu {
                        menu.page = page;
                        menu.cursor = menu.keyboard.then_some(0);
                    }
                    return Update::none();
                }
                self.context_menu = None;
                match action {
                    context_menu::ContextAction::MoveTo(to) => {
                        self.update_move(moves::MoveMessage::Requested(to))
                    }
                    context_menu::ContextAction::MoveToPage | context_menu::ContextAction::Back => {
                        Update::none()
                    }
                    context_menu::ContextAction::Cut => self.request_copy_selection(true),
                    context_menu::ContextAction::Copy => self.request_copy_selection(false),
                    context_menu::ContextAction::Delete
                    | context_menu::ContextAction::RemovePoint => self.delete_selection(),
                    context_menu::ContextAction::PasteHere(at) => self.paste_clipboard(Some(at)),
                    context_menu::ContextAction::AddPointHere(connection_id, at) => {
                        self.update(Message::Editor(map_editor::Message::InsertWaypointAt {
                            connection_id,
                            at,
                        }))
                    }
                }
            }
            Message::FolderMenuToggled(folder) => {
                self.folder_menu = folder;
                self.clans.menu = None;
                Update::none()
            }
            Message::MenuPicked(action) => {
                self.map_menu_open = false;
                self.folder_menu = None;
                self.clans.menu = None;
                self.update(*action)
            }
            Message::Share(message) => modals::update_share(self, message),
            Message::CopyAreaRequested => {
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                let atlas = self.mapper.get_current_atlas();
                let Some(area) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                let access = area.effective_access();
                // Shared-with-copy areas only; owned maps never offer this.
                if access.is_owner || !access.can_copy {
                    return Update::none();
                }
                // The copy is the viewer's cloud map, filed in one of their
                // cloud folders.
                let folder = folder_picker::FolderPicker::in_storage(
                    self.own_folders(),
                    MapStorage::Cloud,
                    None,
                );
                self.modal = Some(modals::Modal::CopyArea(modals::CopyAreaDialog {
                    source: area_id,
                    source_name: area.get_name().to_string(),
                    name: format!("{} (copy)", area.get_name()),
                    atlas_id: area.meta().atlas_id,
                    busy: false,
                    error: None,
                    atlas_report: None,
                    duplicate: false,
                    folder,
                    secrets: modals::SecretsAlong::of(&area),
                }));
                Update::none()
            }
            Message::CopyAreaNameChanged(value) => {
                if let Some(modals::Modal::CopyArea(dialog)) = &mut self.modal {
                    dialog.name = value;
                }
                Update::none()
            }
            Message::CopyAreaConfirmed => {
                let Some(modals::Modal::CopyArea(dialog)) = &mut self.modal else {
                    return Update::none();
                };
                let name = dialog.name.trim().to_string();
                if dialog.busy || name.is_empty() || !dialog.folder.ready() {
                    return Update::none();
                }
                dialog.busy = true;
                dialog.error = None;
                // A new folder is made first; its arrival carries on here.
                let Some(atlas_id) = dialog.folder.chosen() else {
                    let folder_name = dialog.folder.new_folder_name().unwrap_or_default();
                    return Update::with_task(folder_picker::make_folder(
                        self.mapper.clone(),
                        folder_name,
                        MapStorage::Cloud,
                    ));
                };
                let source = dialog.source;
                // Captured now so the completion handler is independent of
                // whether the modal is still open (it can be dismissed
                // mid-copy).
                let duplicate = dialog.duplicate;
                let request = CopyAreaRequest {
                    name: Some(name),
                    atlas_id: Some(atlas_id),
                };
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move { client.copy_area(source, &request).await.map(|area| area.id) },
                    move |result| Message::CopyAreaCompleted { result, duplicate },
                ))
            }
            Message::CopyAreaCompleted { result, duplicate } => {
                match result {
                    Ok(area_id) => {
                        // The clone is owned and arrives via sync; the tick
                        // handler selects it the moment it lands.
                        self.modal = None;
                        self.pending_copied_area = Some(area_id);
                        // A duplicate starts inactive so it doesn't compete
                        // with its source for room identification (disabling
                        // an unknown id is safe — it's preserved until sync
                        // lands the area).
                        if duplicate {
                            self.mapper.set_area_enabled(area_id, false);
                            self.mapper.sync_now();
                            return Update::with_event(Event::DisabledAreasChanged(
                                self.mapper.disabled_areas(),
                            ));
                        }
                        self.mapper.sync_now();
                    }
                    Err(error) => {
                        if let Some(modals::Modal::CopyArea(dialog)) = &mut self.modal {
                            dialog.busy = false;
                            dialog.error = Some(copy_error_message(&error));
                        }
                    }
                }
                Update::none()
            }
            Message::CopyAtlasRequested => {
                let Some(modals::Modal::CopyArea(dialog)) = &mut self.modal else {
                    return Update::none();
                };
                let Some(atlas_id) = dialog.atlas_id else {
                    return Update::none();
                };
                if dialog.busy {
                    return Update::none();
                }
                dialog.busy = true;
                dialog.error = None;
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move { client.copy_atlas(atlas_id, None).await },
                    Message::CopyAtlasCompleted,
                ))
            }
            Message::CopyAtlasCompleted(result) => {
                if let Some(modals::Modal::CopyArea(dialog)) = &mut self.modal {
                    dialog.busy = false;
                    match result {
                        Ok(report) => {
                            dialog.atlas_report = Some(crate::i18n::t!(
                                "mapper-copy-report",
                                "copied" => report.copied.len(),
                                "skipped" => report.skipped.len()
                            ));
                            // Select the first clone when sync lands it.
                            self.pending_copied_area = report.copied.first().copied();
                            self.mapper.sync_now();
                        }
                        Err(error) => dialog.error = Some(copy_error_message(&error)),
                    }
                }
                Update::none()
            }

            // ===== atlases (folders) =====
            Message::AtlasesLoaded {
                auth_projection_revision,
                result,
            } => {
                if auth_projection_revision != self.mapper.auth_projection_revision() {
                    return Update::none();
                }
                let mut deltas = Vec::new();
                let mut task = Task::none();
                match result {
                    Ok(atlases) => {
                        // Record first sight of *owned* atlases only. GET /atlases
                        // is owned-OR-administered, so a `can_admin` atlas-share
                        // can arrive here on the same sync tick that §5 homing
                        // runs in IndicesLoaded; marking it seen first would
                        // suppress its recipient homing. Every shared atlas —
                        // administered or not — is left for the homing path,
                        // which marks it seen itself after deciding.
                        for item in &atlases {
                            if item.is_owner && self.map_scopes.mark_seen(item.id) {
                                deltas.push(ScopeDelta::MarkSeen { atlas_id: item.id });
                            }
                        }
                        self.atlases = atlases;
                        // A script may have made a default folder since.
                        self.refresh_default_atlases();
                        // TEMPORARY(0.6.x): with the folder inventory in hand,
                        // file the viewer's loose maps (once a session).
                        task = loose_maps_migration::start(self);
                    }
                    // Signed out / unverified: no cloud folders to show.
                    Err(CloudError::Unauthorized(_) | CloudError::EmailNotVerified) => {
                        self.atlases.clear();
                    }
                    // Keep the prior inventory on a transient failure.
                    Err(error) => log::warn!("map editor: atlas list fetch failed: {error}"),
                }
                let event = (!deltas.is_empty()).then_some(Event::ScopeAssociationsChanged(deltas));
                Update::new(task, event)
            }
            Message::NewAtlasRequested => {
                // Cloud is the default tier when signed in; a signed-out
                // session can only create local folders.
                let signed_in = self.cloud.snapshot.get().signed_in;
                self.modal = Some(modals::Modal::CreateAtlas {
                    name: String::new(),
                    error: None,
                    storage: if signed_in {
                        MapStorage::Cloud
                    } else {
                        MapStorage::Local
                    },
                    cloud_available: signed_in,
                });
                Update::none()
            }
            Message::CreateAtlasNameChanged(value) => {
                if let Some(modals::Modal::CreateAtlas { name, .. }) = &mut self.modal {
                    *name = value;
                }
                Update::none()
            }
            Message::CreateAtlasTierChanged(storage) => {
                if let Some(modals::Modal::CreateAtlas {
                    storage: slot,
                    cloud_available,
                    ..
                }) = &mut self.modal
                {
                    // Cloud can't be chosen when it isn't available.
                    *slot = if storage == MapStorage::Cloud && !*cloud_available {
                        MapStorage::Local
                    } else {
                        storage
                    };
                }
                Update::none()
            }
            Message::CreateAtlasConfirmed => {
                let Some(modals::Modal::CreateAtlas { name, storage, .. }) = &self.modal else {
                    return Update::none();
                };
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Update::none();
                }
                let storage = *storage;
                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move { mapper.create_atlas_at(name, storage).await },
                    |result| {
                        Message::AtlasCreated(
                            result.map(|atlas| atlas.id).map_err(|e| display_error(&e)),
                        )
                    },
                ))
            }
            Message::AtlasCreated(result) => {
                match result {
                    Ok(atlas_id) => {
                        self.modal = None;
                        // Ensure the new folder is expanded, then refetch so it
                        // appears with its real name and count.
                        self.collapsed_folders.remove(&FolderKey::Atlas(atlas_id));
                        // Creation-associates: a cloud atlas created from a
                        // session-scoped editor is homed on this session's entry.
                        let assoc = self.associate_new_atlas(atlas_id);
                        return Update::new(self.fetch_atlases(), assoc);
                    }
                    Err(error) => {
                        if let Some(modals::Modal::CreateAtlas { error: slot, .. }) =
                            &mut self.modal
                        {
                            *slot = Some(error);
                        }
                    }
                }
                Update::none()
            }
            Message::LocalMoveReviewed(request, result) => {
                self.local_move_reviewed(request, result)
            }
            Message::LocalMoveConfirmed => self.confirm_local_move(),
            Message::FilingReviewed(id, result) => self.filing_reviewed(id, result),
            Message::FilingConfirmed => self.confirm_filing(),
            Message::MoveAtlasStorageRequested(atlas_id) => {
                if clan_maps::is_clan_folder(self, atlas_id) {
                    return Update::none();
                }
                let Some(storage) = self.mapper.atlas_storage(&atlas_id) else {
                    return Update::with_task(self.fetch_atlases());
                };
                let destination = match storage {
                    MapStorage::Local if self.cloud.snapshot.get().signed_in => MapStorage::Cloud,
                    MapStorage::Cloud => MapStorage::Local,
                    MapStorage::Local | MapStorage::Session => return Update::none(),
                };
                let Some(atlas) = self.atlases.iter().find(|atlas| atlas.id == atlas_id) else {
                    return Update::none();
                };
                self.modal = Some(modals::Modal::MoveAtlasStorage {
                    atlas_id,
                    name: atlas.name.clone(),
                    area_count: atlas.area_count,
                    source: storage,
                    destination,
                });
                Update::none()
            }
            Message::MoveAtlasStorageConfirmed => {
                let Some(modals::Modal::MoveAtlasStorage {
                    atlas_id,
                    destination,
                    ..
                }) = self.modal.take()
                else {
                    return Update::none();
                };
                if destination == MapStorage::Local {
                    return self.review_local_move(local_move::Request::Atlas(atlas_id));
                }
                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move {
                        mapper
                            .relocate_atlas(atlas_id, destination, RelocationMode::Move)
                            .await
                            .map_err(|failure| match &failure.completed {
                                // The copied atlas is complete: name it, so the
                                // user resolves the duplicate rather than
                                // retrying (which would mint another copy).
                                Some(completed) => crate::i18n::t!(
                                    "mapper-relocation-duplicate-notice",
                                    "error" => display_error(&failure.error),
                                    "name" => completed.destination_atlas_name.clone()
                                ),
                                None => failure.to_string(),
                            })
                    },
                    Message::MoveAtlasStorageCompleted,
                ))
            }
            Message::MoveAtlasStorageCompleted(result) => match result {
                Ok(relocation) => {
                    self.collapsed_folders
                        .remove(&FolderKey::Atlas(relocation.source_atlas_id));
                    self.collapsed_folders
                        .remove(&FolderKey::Atlas(relocation.destination_atlas_id));
                    let selected = self.editor.area_id().and_then(|selected| {
                        relocation
                            .areas
                            .source_ids
                            .iter()
                            .position(|source| *source == selected)
                            .map(|index| relocation.areas.destination_ids[index])
                    });
                    let assoc = self.associate_new_atlas(relocation.destination_atlas_id);
                    let mut update = selected.map_or_else(Update::none, |area_id| {
                        self.update(Message::AreaSelected(area_id))
                    });
                    update.task = Task::batch([update.task, self.fetch_atlases()]);
                    update.event = assoc.or(update.event);
                    update
                }
                Err(error) => {
                    self.editor_notice = Some((Instant::now(), error));
                    Update::none()
                }
            },
            Message::RenameAtlasStarted(atlas_id) => {
                let name = self
                    .atlases
                    .iter()
                    .find(|atlas| atlas.id == atlas_id)
                    .map(|atlas| atlas.name.clone())
                    .unwrap_or_default();
                self.renaming_atlas = Some((atlas_id, name));
                Update::none()
            }
            Message::RenameAtlasChanged(value) => {
                if let Some((_, name)) = &mut self.renaming_atlas {
                    *name = value;
                }
                Update::none()
            }
            Message::RenameAtlasCommitted => {
                let Some((atlas_id, name)) = self.renaming_atlas.take() else {
                    return Update::none();
                };
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Update::none();
                }
                // Optimistic local rename; a failure refetches to correct it.
                if let Some(atlas) = self.atlases.iter_mut().find(|atlas| atlas.id == atlas_id) {
                    atlas.name = name.clone();
                }
                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move { mapper.rename_atlas(atlas_id, name).await },
                    |result| {
                        Message::AtlasRenamed(result.map(|_| ()).map_err(|e| display_error(&e)))
                    },
                ))
            }
            Message::AtlasRenamed(result) => {
                if let Err(error) = result {
                    log::warn!("map editor: atlas rename failed: {error}");
                    return Update::with_task(self.fetch_atlases());
                }
                Update::none()
            }
            Message::DeleteAtlasRequested(atlas_id) => {
                let Some(atlas) = self.atlases.iter().find(|atlas| atlas.id == atlas_id) else {
                    return Update::none();
                };
                let name = atlas.name.clone();
                let maps: Vec<AreaId> = self
                    .mapper
                    .get_current_atlas()
                    .areas()
                    .filter(|area| area.meta().atlas_id == Some(atlas_id))
                    .map(|area| *area.get_id())
                    .collect();
                // No map is left outside a folder: its maps go to another
                // folder in the same storage, or a new one.
                let folder = match self.mapper.atlas_storage(&atlas_id) {
                    Some(storage) if !maps.is_empty() => {
                        let others: Vec<_> = self
                            .own_folders()
                            .into_iter()
                            .filter(|folder| folder.id != atlas_id)
                            .collect();
                        Some(folder_picker::FolderPicker::in_storage(
                            others, storage, None,
                        ))
                    }
                    _ => None,
                };
                self.modal = Some(modals::Modal::ConfirmDeleteAtlas {
                    atlas_id,
                    name,
                    maps,
                    folder,
                    busy: false,
                    error: None,
                });
                Update::none()
            }
            Message::DeleteAtlasConfirmed => {
                let Some(modals::Modal::ConfirmDeleteAtlas {
                    atlas_id,
                    maps,
                    folder,
                    busy,
                    error,
                    ..
                }) = &mut self.modal
                else {
                    return Update::none();
                };
                if *busy || folder.as_ref().is_some_and(|picker| !picker.ready()) {
                    return Update::none();
                }
                *busy = true;
                *error = None;
                let atlas_id = *atlas_id;
                let maps = maps.clone();
                let mapper = self.mapper.clone();
                let destination = match folder {
                    None => None,
                    Some(picker) => match picker.chosen() {
                        Some(destination) => Some(destination),
                        // A new folder is made first; its arrival carries on
                        // here.
                        None => {
                            let name = picker.new_folder_name().unwrap_or_default();
                            return Update::with_task(folder_picker::make_folder(
                                mapper,
                                name,
                                picker.storage(),
                            ));
                        }
                    },
                };
                if let Some(destination) = destination
                    && self.mapper.atlas_storage(&atlas_id) == Some(MapStorage::Cloud)
                    && !maps.is_empty()
                {
                    return self.review_filing(filing::Request::EmptyAtlas {
                        id: atlas_id,
                        maps,
                        destination,
                    });
                }
                Update::with_task(Task::perform(
                    async move {
                        if let Some(destination) = destination {
                            for area_id in maps {
                                mapper
                                    .move_area_to_atlas(area_id, Some(destination))
                                    .await?;
                            }
                        }
                        mapper.delete_atlas(atlas_id).await
                    },
                    |result| Message::AtlasDeleted(result.map_err(|e| display_error(&e))),
                ))
            }
            Message::AtlasDeleted(result) => {
                let refetch = self.fetch_atlases();
                match result {
                    Ok(()) => {
                        if let Some(modals::Modal::ConfirmDeleteAtlas { atlas_id, .. }) =
                            self.modal.take()
                        {
                            self.atlases.retain(|atlas| atlas.id != atlas_id);
                            self.collapsed_folders.remove(&FolderKey::Atlas(atlas_id));
                        }
                    }
                    // The dialog stays open saying why; maps already moved
                    // stay where they went.
                    Err(failure) => {
                        log::warn!("map editor: atlas delete failed: {failure}");
                        self.editor_notice = Some((std::time::Instant::now(), failure.clone()));
                        if let Some(modals::Modal::ConfirmDeleteAtlas { busy, error, .. }) =
                            &mut self.modal
                        {
                            *busy = false;
                            *error = Some(failure);
                        }
                    }
                }
                Update::with_task(refetch)
            }
            Message::NewAreaInAtlas(atlas_id) => {
                let Some(storage) = self.mapper.atlas_storage(&atlas_id) else {
                    return Update::with_task(self.fetch_atlases());
                };
                let name = self
                    .atlases
                    .iter()
                    .find(|atlas| atlas.id == atlas_id)
                    .map(|atlas| atlas.name.clone())
                    .unwrap_or_default();
                let ownership = self
                    .atlases
                    .iter()
                    .find(|atlas| atlas.id == atlas_id && atlas.clan_id.is_some())
                    .and_then(|atlas| modals::NewMapOwnership::for_folder(&atlas.actions));
                self.modal = Some(modals::Modal::CreateArea {
                    name: String::new(),
                    error: None,
                    folder: folder_picker::FolderPicker::in_folder(folder_picker::OwnFolder {
                        id: atlas_id,
                        name,
                        storage,
                    }),
                    busy: false,
                    ownership,
                });
                Update::none()
            }
            Message::MoveAreaRequested(area_id) => {
                // A clan's map moves between the clan's own folders.
                if let Some(clan_id) = self.area_clan(area_id) {
                    return clan_maps::open_refile(self, area_id, clan_id);
                }
                // Owned areas only — the same-owner rule means you can only
                // file your own maps into your own folders.
                if !self.area_owned(area_id) {
                    return Update::none();
                }
                let atlas = self.mapper.get_current_atlas();
                let area_name = atlas
                    .get_area(&area_id)
                    .map(|area| area.get_name().to_string())
                    .unwrap_or_default();
                let current_atlas = atlas
                    .get_area(&area_id)
                    .and_then(|area| area.meta().atlas_id);
                let current = MapDestination {
                    storage: self.mapper.area_storage(&area_id),
                    atlas_id: current_atlas,
                };
                let targets = self.folder_destinations();
                // With no folder to pick, the dialog opens on naming one.
                let new_folder = targets.is_empty().then(|| {
                    folder_picker::NewFolderForm::new(
                        current.storage,
                        self.cloud.snapshot.get().signed_in,
                    )
                });
                self.modal = Some(modals::Modal::MoveArea {
                    area_id,
                    area_name,
                    current,
                    targets,
                    make_folder: true,
                    new_folder,
                });
                Update::none()
            }
            Message::MoveAreaTo { area, destination } => {
                self.modal = None;
                if self.area_clan(area).is_some() {
                    return clan_maps::refile(self, area, destination.atlas_id);
                }
                // A map always moves into a folder.
                if destination.atlas_id.is_none() {
                    return Update::none();
                }
                if self.area_owned(area) {
                    if self.mapper.area_storage(&area) == MapStorage::Cloud
                        && destination.storage == MapStorage::Cloud
                    {
                        return self.review_filing(filing::Request::Maps {
                            ids: vec![area],
                            destination,
                            multi: false,
                            clan: false,
                        });
                    }
                    if self.mapper.area_storage(&area) == MapStorage::Cloud
                        && destination.storage == MapStorage::Local
                    {
                        return self.review_local_move(local_move::Request::Maps {
                            ids: vec![area],
                            destination,
                            multi: false,
                        });
                    }
                    let mapper = self.mapper.clone();
                    return Update::with_task(Task::perform(
                        async move {
                            match mapper
                                .relocate_areas(vec![area], destination, RelocationMode::Move)
                                .await
                            {
                                Ok(result) => {
                                    result.destination_ids.into_iter().next().ok_or_else(|| {
                                        display_error(&CloudError::InvalidInput(
                                            "move returned no map".into(),
                                        ))
                                    })
                                }
                                // The copy at the destination is complete: name
                                // it, so the user resolves the duplicate rather
                                // than retrying (which would mint another copy).
                                Err(failure) => Err(match &failure.completed {
                                    Some(completed) => {
                                        let atlas = mapper.get_current_atlas();
                                        let name = completed
                                            .destination_ids
                                            .first()
                                            .and_then(|id| atlas.get_area(id))
                                            .map(|area| area.get_name().to_string())
                                            .unwrap_or_default();
                                        crate::i18n::t!(
                                            "mapper-relocation-duplicate-notice",
                                            "error" => display_error(&failure.error),
                                            "name" => name
                                        )
                                    }
                                    None => failure.to_string(),
                                }),
                            }
                        },
                        Message::MoveAreaCompleted,
                    ));
                }
                Update::none()
            }
            Message::MoveAreaCompleted(result) => {
                match result {
                    Ok(area_id) => {
                        let assoc = self.associate_new_area(area_id);
                        let mut update = self.update(Message::AreaSelected(area_id));
                        // A same-tier re-file keeps the destination id equal to
                        // the viewed area's, so `AreaSelected` no-ops; the move
                        // still bumped the area rev, which must not read as an
                        // external edit on the next tick. Resync unconditionally.
                        self.refresh_seen_rev();
                        self.inspector.resync(&self.mapper, &self.editor);
                        update.event = assoc.or(update.event);
                        return update;
                    }
                    Err(error) => self.editor_notice = Some((Instant::now(), error)),
                }
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::none()
            }
            Message::ToggleFolderCollapsed(key) => {
                if !self.collapsed_folders.remove(&key) {
                    self.collapsed_folders.insert(key);
                }
                Update::none()
            }
            Message::ShareAtlasRequested(atlas_id) => {
                modals::open_share_atlas_dialog(self, atlas_id)
            }
            Message::ShareAtlas(message) => modals::update_share_atlas(self, message),
            Message::Clan(message) => clan_maps::update(self, message),
            Message::ClanMapShare(message) => clan_map_share::update(self, *message),
            Message::PutInClan(message) => clan_map_share::update_put_in_clan(self, message),
            Message::TransferOwnershipRequested => {
                let atlas = self.mapper.get_current_atlas();
                match self
                    .editor
                    .area_id()
                    .filter(|id| self.area_clan(*id).is_none())
                    .and_then(|id| atlas.get_area(&id).map(|a| (id, a.get_name().to_string())))
                {
                    Some((id, name)) => {
                        modals::open_transfer_dialog(self, modals::TransferSubject::Area(id, name))
                    }
                    None => Update::none(),
                }
            }
            Message::UseForNewMaps(atlas_id) => {
                Update::with_task(default_atlases::use_for_new_maps(self, atlas_id))
            }
            Message::DefaultAtlasSet(result) => {
                default_atlases::set(self, result);
                Update::none()
            }
            Message::TransferAtlasOwnershipRequested(atlas_id) => {
                if clan_maps::is_clan_folder(self, atlas_id) {
                    return Update::none();
                }
                let name = self
                    .atlases
                    .iter()
                    .find(|a| a.id == atlas_id)
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                modals::open_transfer_dialog(self, modals::TransferSubject::Atlas(atlas_id, name))
            }
            Message::Transfer(message) => modals::update_transfer(self, message),
            Message::ScopeAllToggled(all) => {
                self.scope_all = all;
                self.scope_menu_open = false;
                Update::none()
            }
            Message::ScopeMenuToggled(open) => {
                self.scope_menu_open = open;
                self.folder_menu = None;
                self.clans.menu = None;
                Update::none()
            }
            Message::MapListFilterChanged(filter) => {
                self.map_list_filter = filter;
                self.scope_menu_open = false;
                self.folder_menu = None;
                self.clans.menu = None;
                Update::none()
            }
            Message::ServersChecklistRequested(target) => {
                // The checklist writes associations; it needs a server
                // inventory. Local atlases and ephemeral areas never get here
                // (no affordance is drawn for them).
                let name = match target {
                    ScopeTarget::Atlas(atlas_id) => self
                        .atlases
                        .iter()
                        .find(|a| a.id == atlas_id)
                        .map(|a| a.name.clone())
                        .unwrap_or_default(),
                    ScopeTarget::Area(area_id) => self
                        .mapper
                        .get_current_atlas()
                        .get_area(&area_id)
                        .map(|a| a.get_name().to_string())
                        .unwrap_or_default(),
                };
                let servers = smudgy_core::models::server::list_servers()
                    .map(|servers| servers.into_iter().map(|s| s.name).collect::<Vec<_>>())
                    .unwrap_or_default();
                let checked = match target {
                    ScopeTarget::Atlas(atlas_id) => self.map_scopes.atlas_entries(&atlas_id),
                    ScopeTarget::Area(area_id) => self.map_scopes.area_entries(&area_id),
                };
                self.modal = Some(modals::Modal::ServersChecklist {
                    target,
                    name,
                    servers,
                    checked,
                });
                Update::none()
            }
            Message::ScopeServerToggled { entry, show } => {
                let Some(modals::Modal::ServersChecklist {
                    target, checked, ..
                }) = &mut self.modal
                else {
                    return Update::none();
                };
                if show {
                    checked.insert(entry.clone());
                } else {
                    checked.remove(&entry);
                }
                let delta = match *target {
                    ScopeTarget::Atlas(atlas_id) => ScopeDelta::SetAtlasEntry {
                        atlas_id,
                        entry,
                        show,
                    },
                    ScopeTarget::Area(area_id) => ScopeDelta::SetAreaEntry {
                        area_id,
                        entry,
                        show,
                    },
                };
                self.map_scopes.apply(&delta);
                Update::with_event(Event::ScopeAssociationsChanged(vec![delta]))
            }
            Message::ScopesReplaced(scopes) => {
                self.map_scopes = scopes;
                // Refresh an open "Servers…" checklist: its `checked` buffer was
                // snapshotted at open, so a concurrent write mirrored back here
                // would otherwise leave stale ticks on screen. Rebuild it from
                // the fresh store for the modal's target.
                if let Some(modals::Modal::ServersChecklist {
                    target, checked, ..
                }) = &mut self.modal
                {
                    *checked = match *target {
                        ScopeTarget::Atlas(atlas_id) => self.map_scopes.atlas_entries(&atlas_id),
                        ScopeTarget::Area(area_id) => self.map_scopes.area_entries(&area_id),
                    };
                }
                self.refresh_multi_servers();
                Update::none()
            }
        }
    }

    /// The center panel: the sign-in banner, the toolbar (with one map open),
    /// the canvas, or with several maps or folders chosen what can be done
    /// to them all, and the status footer.
    fn center_panel(&self) -> ThemedElement<'_, Message> {
        let mut panel = column![];

        // Signed-out CTA: local maps still save to this device; signing in
        // adds cloud maps that sync across devices and can be shared. Mirrors
        // the main window's verify-email banner; the button forwards to the
        // settings window's Account tab, where sign-in/sign-up lives. The close
        // affordance hides it until the next client version (see
        // [`Self::signin_banner_dismissed`]).
        if !self.cloud.snapshot.get().signed_in && !self.signin_banner_dismissed {
            panel = panel.push(
                container(
                    row![
                        text(crate::i18n::t!("mapper-local-maps-signin")).size(13),
                        button(text(crate::i18n::t!("mapper-sign-in-create")).size(12))
                            .style(theme::builtins::button::primary)
                            .padding([2, 8])
                            .on_press(Message::OpenSettingsRequested),
                        space::horizontal(),
                        button(text("\u{D7}").size(14))
                            .style(theme::builtins::button::subtle)
                            .padding([2, 8])
                            .on_press(Message::DismissSigninBanner),
                    ]
                    .spacing(12)
                    .align_y(Vertical::Center),
                )
                .width(Length::Fill)
                .padding([6, 12])
                .style(theme::builtins::container::modal_title_bar),
            );
        }

        let multi = self.multi.selection.is_multi();
        if !multi && self.editor.area_id().is_some() {
            panel = panel.push(toolbar::view(self));
        }

        let body: ThemedElement<'_, Message> = if multi {
            // Several maps chosen: what can be done to them all.
            multi_select::pane(self)
        } else {
            let canvas = context_menu::view(self, self.editor.view().map(Message::Editor));
            // A map is empty when no place the viewer reads holds a room.
            let empty = self.editor.area_id().is_some_and(|area_id| {
                self.mapper
                    .get_current_atlas()
                    .get_area(&area_id)
                    .is_some_and(|area| map_panel::room_count(&area) == 0)
            });
            if empty && self.can_edit_active_area() {
                stack(vec![
                    canvas,
                    center(
                        column![
                            button(text(crate::i18n::t!("mapper-create-first-room")).size(14))
                                .style(theme::builtins::button::primary)
                                .on_press(Message::ToolSelected(Tool::AddRoom)),
                            text(crate::i18n::t!("mapper-create-first-room-help")).size(12),
                        ]
                        .spacing(6)
                        .align_x(iced::Alignment::Center),
                    )
                    .into(),
                ])
                .into()
            } else {
                canvas
            }
        };

        panel
            .push(container(body).width(Length::Fill).height(Length::Fill))
            .push(legend::view(
                self.editor.legend_items(),
                self.editor_notice
                    .as_ref()
                    .map(|(_, notice)| notice.clone()),
                self.automatic_route_preview.is_some(),
            ))
            .into()
    }

    pub fn view(&self) -> ThemedElement<'_, Message> {
        // The map list and the inspector stand beside the center panel, which
        // holds the toolbar, the canvas and the footer.
        let panes = PaneGrid::new(&self.panes, |_pane, kind, _maximized| {
            pane_grid::Content::new(match kind {
                PaneKind::AreaList => area_list::view(self),
                PaneKind::Canvas => self.center_panel(),
                PaneKind::Inspector => inspector::view(self),
            })
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(4)
        .on_resize(8, Message::PaneResized);

        let main_layout: ThemedElement<'_, Message> = container(panes)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();

        if let Some(modal) = &self.modal {
            stack(vec![
                main_layout,
                opaque(
                    mouse_area(
                        center(opaque(modal.view())).style(theme::builtins::container::overlay),
                    )
                    .on_press(Message::ModalDismissed),
                ),
            ])
            .into()
        } else {
            main_layout
        }
    }
}

/// The place `entity` lives in on `area`: the map, a Secret, or Private.
/// Why the viewer can't do `action` in `source` of `area`, as the notice to
/// show; `None` when they can.
fn write_refusal_in(area: &AreaCache, source: SourceId, action: &str) -> Option<String> {
    if !secrets::can_write(area, source) {
        return Some(match source {
            SourceId::Map => crate::i18n::t!("mapper-map-view-only"),
            SourceId::Secret(_) if secrets::bundle(area, source).is_none() => {
                crate::i18n::t!("cloud-error-secret-unavailable")
            }
            _ => crate::i18n::t!("cloud-error-secret-view-only"),
        });
    }
    if secrets::can(area, source, action) {
        return None;
    }
    Some(match (source, action) {
        (SourceId::Map, "add") => crate::i18n::t!("mapper-map-cannot-add"),
        (SourceId::Map, "remove") => crate::i18n::t!("mapper-map-cannot-remove"),
        (SourceId::Map, _) => crate::i18n::t!("mapper-map-cannot-edit"),
        (_, "add") => crate::i18n::t!("cloud-error-secret-cannot-add"),
        (_, "remove") => crate::i18n::t!("cloud-error-secret-cannot-remove"),
        _ => crate::i18n::t!("cloud-error-secret-cannot-edit"),
    })
}

fn place_of(area: &AreaCache, entity: EntityId) -> smudgy_cloud::SourceId {
    let layer_source = |layer: Option<&smudgy_cloud::mapper::area_cache::SourceLayer>| {
        layer.map_or(smudgy_cloud::SourceId::Map, |layer| layer.source())
    };
    match entity {
        EntityId::Room(_) => smudgy_cloud::SourceId::Map,
        EntityId::SourceRoom(source, _) => source,
        EntityId::Label(id) => area
            .find_label(&id)
            .map_or(smudgy_cloud::SourceId::Map, |(layer, _)| {
                layer_source(layer)
            }),
        EntityId::Shape(id) => area
            .find_shape(&id)
            .map_or(smudgy_cloud::SourceId::Map, |(layer, _)| {
                layer_source(layer)
            }),
        EntityId::Connection(id) => area
            .find_connection(id)
            .map_or(smudgy_cloud::SourceId::Map, |(layer, _)| {
                layer_source(layer)
            }),
    }
}

/// Whether any of `events` moved rooms to, from or between the places of
/// map `open`.
fn rooms_moved_on(
    atlas: &AtlasCache,
    open: AreaId,
    events: &[smudgy_cloud::mapper::MapperEvent],
) -> bool {
    let map_of = |id: &AreaId| {
        atlas
            .place_map(id)
            .or_else(|| atlas.map_of(id))
            .unwrap_or(*id)
    };
    events.iter().any(|event| match event {
        smudgy_cloud::mapper::MapperEvent::AreasMerged {
            into,
            deleted,
            rooms,
        } => {
            map_of(into) == open
                || deleted.iter().any(|id| map_of(id) == open)
                || rooms.iter().any(|room| map_of(&room.from.area_id) == open)
        }
        _ => false,
    })
}

/// A signed-in window on `area`, for tests that drive the editor.
#[cfg(test)]
pub(super) fn test_window(mapper: Mapper, area: AreaId) -> MapEditorWindow {
    let mut window = MapEditorWindow::with_clipboard(
        window::Id::unique(),
        mapper,
        crate::cloud_account::test_handles_signed_in("tester"),
        Arc::new(ArcSwap::from_pointee(commands::EntityClipboard::default())),
        "test".to_string(),
        MapScopes::default(),
        None,
    );
    let _ = window.open_area(area);
    window
}

/// Queues an immediate redraw. iced 0.14 exposes no task to redraw a single
/// window, so this redraws every open window; player movement is infrequent
/// and the main window is already repainting while its map pans, so the extra
/// cost is negligible.
fn request_repaint() -> Task<Message> {
    iced_runtime::task::effect(iced_runtime::Action::Window(
        iced_runtime::window::Action::RedrawAll,
    ))
}

/// Builds the `copied_from` adjacency for the cache: each area maps to its
/// clone source, but only when that source is *also* resident in the cache
/// (so a clone of a now-deleted source contributes no edge).
fn copied_from_edges(atlas: &AtlasCache) -> std::collections::HashMap<AreaId, Option<AreaId>> {
    let present: std::collections::HashSet<AreaId> =
        atlas.areas().map(|area| *area.get_id()).collect();
    atlas
        .areas()
        .map(|area| {
            let id = *area.get_id();
            let source = area
                .meta()
                .copied_from_area_id
                .filter(|src| present.contains(src));
            (id, source)
        })
        .collect()
}

/// Per-viewer copy-family buckets built from the list-only
/// [`Area::family_token`]. Held **in memory only** for the current list:
/// the token is a per-viewer HMAC — stable for this user but a different value
/// for every other user and meaningless outside this user's `GET /areas`
/// response — so it must never be persisted or cross-referenced (see the field
/// docs). Grouping is by **exact string equality** of the token.
#[derive(Debug, Clone, Default)]
pub struct FamilyIndex {
    /// `area_id -> family_token`, only for areas the server emitted a token
    /// for (i.e. areas with ≥2 visible family members).
    token_by_area: std::collections::HashMap<AreaId, String>,
}

impl FamilyIndex {
    /// Builds the index from a `GET /areas` response. Areas without a
    /// `family_token` (singletons, or anything the server chose to omit) are
    /// skipped — absence means "no grouping to show," never "not a fork."
    #[must_use]
    pub fn build(areas: &[Area]) -> Self {
        let token_by_area = areas
            .iter()
            .filter_map(|area| area.family_token.clone().map(|token| (area.id, token)))
            .collect();
        Self { token_by_area }
    }
}

/// Undirected family adjacency over both `copied_from` edges (owner-only
/// provenance) and `family_token` cliques (per-viewer grouping of areas that
/// share an origin, including received copies with no visible provenance).
/// Token members are linked to a per-token representative (a star, which still
/// merges the whole bucket into one component) to stay linear.
fn family_adjacency(
    edges: &std::collections::HashMap<AreaId, Option<AreaId>>,
    tokens: &std::collections::HashMap<AreaId, String>,
) -> std::collections::HashMap<AreaId, Vec<AreaId>> {
    let mut adj: std::collections::HashMap<AreaId, Vec<AreaId>> = std::collections::HashMap::new();
    for (&from, &maybe_to) in edges {
        if let Some(to) = maybe_to {
            adj.entry(from).or_default().push(to);
            adj.entry(to).or_default().push(from);
        }
    }

    // Link every area sharing a token to that token's first-seen representative.
    let mut representative: std::collections::HashMap<&str, AreaId> =
        std::collections::HashMap::new();
    for (&area_id, token) in tokens {
        match representative.get(token.as_str()) {
            None => {
                representative.insert(token.as_str(), area_id);
            }
            Some(&rep) if rep != area_id => {
                adj.entry(area_id).or_default().push(rep);
                adj.entry(rep).or_default().push(area_id);
            }
            Some(_) => {}
        }
    }

    adj
}

/// Connected component of `area_id` over the combined family adjacency. The
/// result always contains `area_id`; an area with no clone/token links yields
/// a single-element `vec![area_id]`. Sorted by the area id's uuid for a
/// deterministic order.
fn copy_family_in(
    edges: &std::collections::HashMap<AreaId, Option<AreaId>>,
    tokens: &std::collections::HashMap<AreaId, String>,
    area_id: AreaId,
) -> Vec<AreaId> {
    let adj = family_adjacency(edges, tokens);

    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![area_id];
    seen.insert(area_id);
    while let Some(node) = stack.pop() {
        if let Some(neighbors) = adj.get(&node) {
            for &next in neighbors {
                if seen.insert(next) {
                    stack.push(next);
                }
            }
        }
    }

    let mut family: Vec<AreaId> = seen.into_iter().collect();
    family.sort_by_key(|id| id.0);
    family
}

/// Every area that belongs to a copy-family of ≥2 members over the combined
/// adjacency — the set the area list badges as "copy." Any area touched by a
/// `copied_from` edge or a shared `family_token` qualifies.
fn family_members_in(
    edges: &std::collections::HashMap<AreaId, Option<AreaId>>,
    tokens: &std::collections::HashMap<AreaId, String>,
) -> std::collections::HashSet<AreaId> {
    // Any node with at least one neighbor in the combined adjacency is in a
    // multi-member family (the adjacency only records real links).
    family_adjacency(edges, tokens)
        .into_iter()
        .filter_map(|(area_id, neighbors)| (!neighbors.is_empty()).then_some(area_id))
        .collect()
}

/// UX mapping for clone failures: the server uniform-404s every denial
/// (source invisible, no `can_copy`, atlas not owned) — never distinguish.
fn copy_error_message(error: &CloudError) -> String {
    match error {
        CloudError::NotFoundOrNoAccess => crate::i18n::t!("mapper-copy-unavailable"),
        other => display_error(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::{
        ExitDirection, RoomUpdates, Uuid, backends::EphemeralBackend, mapper::RoomKey,
    };

    fn area(n: u128) -> AreaId {
        AreaId(Uuid::from_u128(n))
    }

    fn no_tokens() -> std::collections::HashMap<AreaId, String> {
        std::collections::HashMap::new()
    }

    /// 3-area chain A<-B<-C plus an unrelated solo D.
    fn chain_edges() -> std::collections::HashMap<AreaId, Option<AreaId>> {
        let (a, b, c, d) = (area(1), area(2), area(3), area(4));
        [(a, None), (b, Some(a)), (c, Some(b)), (d, None)]
            .into_iter()
            .collect()
    }

    /// Undo replays a command's inverse, whose operations may need other
    /// actions than the command did: undoing a link made in a Secret deletes
    /// it, which needs `remove` there.
    #[tokio::test]
    async fn history_needs_the_action_each_operation_needs() {
        use super::links::fixture::{C_LIBRARY, KEEP, area, link, maps_where, secret};
        let mapper = maps_where(&["read", "add", "edit"]).await;
        let keep = mapper
            .get_current_atlas()
            .get_area(&area(KEEP))
            .expect("loaded");
        let delete = commands::Mutation::SourceBatch {
            area_id: area(KEEP),
            source: secret(),
            operations: vec![smudgy_cloud::mutation::AreaMutation::DeleteLink {
                connection_id: link(C_LIBRARY),
            }],
            description: "Undo link creation".to_string(),
            split_paired_exit: false,
        };
        assert_eq!(
            commands::needed_actions(&delete),
            (
                area(KEEP),
                vec![(commands::Writes::Place(secret()), "remove")]
            )
        );
        assert_eq!(
            write_refusal_in(&keep, secret(), "remove"),
            Some(crate::i18n::t!("cloud-error-secret-cannot-remove"))
        );
        assert_eq!(write_refusal_in(&keep, secret(), "add"), None);
        assert_eq!(write_refusal_in(&keep, SourceId::Private, "remove"), None);
        let room_delete = commands::Mutation::DeleteRoom(RoomKey::new(area(KEEP), RoomNumber(1)));
        assert_eq!(
            commands::needed_actions(&room_delete).1,
            vec![(commands::Writes::Place(SourceId::Map), "remove")]
        );
    }

    /// A move announced for the open map, from any of its places, lets
    /// go of this window's history; one on another map leaves it.
    #[tokio::test]
    async fn another_windows_move_on_this_map_clears_its_history() {
        use super::links::fixture::{CATACOMBS, KEEP, area, maps, secret};
        let mapper = maps().await;
        let atlas = mapper.get_current_atlas();
        let secret_area = AreaId(match secret() {
            SourceId::Secret(id) => id,
            _ => unreachable!("a Secret"),
        });
        let moved = |into: AreaId, from: AreaId| smudgy_cloud::mapper::MapperEvent::AreasMerged {
            into,
            deleted: Vec::new(),
            rooms: vec![smudgy_cloud::RoomRemap {
                from: RoomKey::new(from, RoomNumber(1)),
                to: RoomNumber(6),
            }],
        };
        assert!(rooms_moved_on(
            &atlas,
            area(KEEP),
            &[moved(secret_area, area(KEEP))]
        ));
        assert!(rooms_moved_on(
            &atlas,
            area(KEEP),
            &[moved(area(KEEP), secret_area)]
        ));
        assert!(!rooms_moved_on(
            &atlas,
            area(KEEP),
            &[moved(area(CATACOMBS), area(CATACOMBS))]
        ));

        let mut window = test_window(mapper, area(KEEP));
        let _ = window.stack.push_and_apply(
            &window.mapper.clone(),
            commands::create_room(area(KEEP), RoomNumber(20), iced::Point::new(9.0, 9.0), 0),
        );
        assert!(window.can_undo());
        // Nothing announced: the history stays.
        window.forget_history_moved_elsewhere(&atlas);
        assert!(window.can_undo());
    }

    #[test]
    fn family_spans_the_whole_clone_chain() {
        let edges = chain_edges();
        let (a, b, c) = (area(1), area(2), area(3));
        assert_eq!(copy_family_in(&edges, &no_tokens(), b), vec![a, b, c]);
        // Any member resolves to the same family.
        assert_eq!(copy_family_in(&edges, &no_tokens(), a), vec![a, b, c]);
        assert_eq!(copy_family_in(&edges, &no_tokens(), c), vec![a, b, c]);
    }

    #[test]
    fn unrelated_area_is_its_own_family() {
        let edges = chain_edges();
        let d = area(4);
        assert_eq!(copy_family_in(&edges, &no_tokens(), d), vec![d]);
    }

    #[test]
    fn dangling_source_contributes_no_edge() {
        // B claims to be copied from A, but A is absent from the cache, so
        // copied_from_edges would record None — model that here.
        let (b, c) = (area(2), area(3));
        let edges: std::collections::HashMap<AreaId, Option<AreaId>> =
            [(b, None), (c, Some(b))].into_iter().collect();
        assert_eq!(copy_family_in(&edges, &no_tokens(), b), vec![b, c]);
    }

    #[test]
    fn family_token_groups_areas_without_provenance() {
        // Two received copies from different friends: no copied_from edges
        // (provenance is owner-only), grouped purely by a shared token.
        let (x, y) = (area(10), area(11));
        let no_edges: std::collections::HashMap<AreaId, Option<AreaId>> =
            [(x, None), (y, None)].into_iter().collect();
        let tokens: std::collections::HashMap<AreaId, String> =
            [(x, "f_abc".to_string()), (y, "f_abc".to_string())]
                .into_iter()
                .collect();
        assert_eq!(copy_family_in(&no_edges, &tokens, x), vec![x, y]);
        assert_eq!(copy_family_in(&no_edges, &tokens, y), vec![x, y]);
    }

    #[test]
    fn distinct_tokens_do_not_merge() {
        let (x, y) = (area(10), area(11));
        let no_edges: std::collections::HashMap<AreaId, Option<AreaId>> =
            [(x, None), (y, None)].into_iter().collect();
        // Exact string equality only — different tokens are different families.
        let tokens: std::collections::HashMap<AreaId, String> =
            [(x, "f_abc".to_string()), (y, "f_def".to_string())]
                .into_iter()
                .collect();
        assert_eq!(copy_family_in(&no_edges, &tokens, x), vec![x]);
    }

    #[test]
    fn edges_and_tokens_union_into_one_family() {
        // Owned chain A<-B (provenance) plus a received copy E that shares B's
        // token: all three are one family even though E has no edge.
        let (a, b, e) = (area(1), area(2), area(5));
        let edges: std::collections::HashMap<AreaId, Option<AreaId>> =
            [(a, None), (b, Some(a)), (e, None)].into_iter().collect();
        let tokens: std::collections::HashMap<AreaId, String> =
            [(b, "f_x".to_string()), (e, "f_x".to_string())]
                .into_iter()
                .collect();
        assert_eq!(copy_family_in(&edges, &tokens, e), vec![a, b, e]);
    }

    #[test]
    fn family_members_flags_only_multi_member_families() {
        let edges = chain_edges();
        let tokens = no_tokens();
        let members = family_members_in(&edges, &tokens);
        // A,B,C are a family; D is solo.
        assert!(members.contains(&area(1)));
        assert!(members.contains(&area(2)));
        assert!(members.contains(&area(3)));
        assert!(!members.contains(&area(4)));
    }

    #[tokio::test]
    async fn reciprocal_pair_candidate_tracks_edited_directions() {
        let mapper = Mapper::new(
            Arc::new(EphemeralBackend::new()),
            std::env::temp_dir().join(format!("smudgy-pair-candidate-{}", Uuid::new_v4())),
        );
        let area_id = mapper
            .create_area_at(
                "Pair candidates".to_string(),
                MapDestination::loose(MapStorage::Session),
            )
            .await
            .expect("create area");
        for room_number in [RoomNumber(1), RoomNumber(2)] {
            mapper
                .upsert_room(RoomKey::new(area_id, room_number), RoomUpdates::default())
                .expect("stage room");
        }

        // Existing one-way traversal: room 2 west -> room 1 east.
        let command = commands::create_exit_with_options(
            area_id,
            RoomNumber(2),
            ExitDirection::West,
            &commands::NewExitTarget::Room(PlacedRoom::map(RoomNumber(1))),
            ExitDirection::East,
            commands::NewLinkOptions {
                one_way: true,
                ..commands::NewLinkOptions::default()
            },
        );
        let mut stack = commands::CommandStack::default();
        let _ = stack.push_and_apply(&mapper, command);
        let atlas = mapper.get_current_atlas();
        let area = atlas.get_area(&area_id).expect("area cached");
        let expected = area.get_connections()[0].id;

        assert_eq!(
            reciprocal_pair_candidate(
                &area,
                area_id,
                RoomNumber(1).into(),
                RoomNumber(2).into(),
                ExitDirection::East,
                ExitDirection::West,
            ),
            Some(expected)
        );
        assert_eq!(
            reciprocal_pair_candidate(
                &area,
                area_id,
                RoomNumber(1).into(),
                RoomNumber(2).into(),
                ExitDirection::North,
                ExitDirection::West,
            ),
            None,
            "editing the proposed source direction invalidates the suggestion"
        );
    }
}
