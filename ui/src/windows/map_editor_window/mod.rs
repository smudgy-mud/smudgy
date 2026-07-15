//! The map editor window: a toolbar over a resizable three-pane layout
//! (area list | canvas | inspector). The canvas is a
//! [`smudgy_map_widget::MapEditor`]; this module owns window chrome, pane
//! layout, the undo stack, and the mutation funnel — every entity edit
//! flows through [`commands::CommandStack`].

mod area_list;
pub mod commands;
mod inspector;
mod modals;
mod toolbar;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;

use crate::cloud_account::CloudHandles;
use crate::theme::{self, Element as ThemedElement};
use crate::update::Update;
use iced::event::Event as IcedEvent;
use iced::keyboard::{self, key::Named};
use iced::alignment::Vertical;
use iced::widget::{
    PaneGrid, button, center, column, container, mouse_area, opaque, pane_grid, row, space, stack,
    text,
};
use iced::{Length, Subscription, Task, Vector, window};
use smudgy_cloud::cloud_api::{
    AtlasCopyReport, CopyAreaRequest, SecretEntity, SecretEntityKind, SecretMarksRequest,
    SecretMarksResult, ShareDirection, ShareGrantRow,
};
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::{
    Area, AreaAccess, AreaId, AtlasId, AtlasListItem, ExitId, LabelId, CloudError, Mapper,
    RoomNumber, ShapeId, mapper::RoomKey,
};

use area_list::SharerIndex;
use smudgy_core::models::map_scopes::{
    HostEntry, MapScopes, ScopeDelta, ScopeState, match_host_hints,
};
use smudgy_map_widget::map_editor::{
    self, EntityId, ExitTarget, MapEditor, MutationRequest, Tool,
};

/// Bootstrap-icons codepoints used by the secrecy UI; the font itself is
/// loaded app-wide (see `crate::assets::bootstrap_icons` for the rest).
const ICON_LOCK_FILL: &str = "\u{F47A}";
const ICON_UNLOCK: &str = "\u{F600}";

/// How long the rooms-not-copyable notice stays up (expired by the
/// periodic [`Message::Tick`]).
const ROOM_COPY_NOTICE_TTL: Duration = Duration::from_secs(5);

/// The copy/paste clipboard, swappable so every editor window can be handed
/// one shared instance.
pub type SharedClipboard = Arc<ArcSwap<commands::EntityClipboard>>;

/// A keyboard action routed to the editor at window level. Only fires for
/// events no focused widget captured (so text inputs keep their keys).
#[derive(Debug, Clone, Copy)]
pub enum Hotkey {
    Delete,
    Nudge(i32, i32),
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
}

#[derive(Debug, Clone)]
pub enum Message {
    Editor(map_editor::Message),
    PaneResized(pane_grid::ResizeEvent),
    AreaSelected(AreaId),
    ToolSelected(Tool),
    LevelUp,
    LevelDown,
    Undo,
    Redo,
    Hotkey(window::Id, Hotkey),
    Inspector(inspector::Message),
    Tick,
    CommandCompleted(commands::Outcome),
    SetCurrentLocation(AreaId, Option<i32>),
    NewAreaRequested,
    CreateAreaNameChanged(String),
    CreateAreaConfirmed,
    AreaCreated(Result<AreaId, String>),
    RenameAreaStarted(AreaId),
    RenameAreaChanged(String),
    RenameAreaCommitted,
    DeleteAreaRequested(AreaId),
    DeleteAreaConfirmed,
    ModalDismissed,
    /// Open the share dialog for the active area (owner or re-sharer only).
    ShareDialogRequested,
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
    SecretsAuditRequested,
    SecretsAuditLoaded(Result<Vec<SecretEntity>, String>),
    SecretsAuditJump(SecretEntity),
    SecretsAuditUnmark(SecretEntity),
    /// Carries the area the in-flight request targeted: the modal may have
    /// been dismissed (or reopened on another area) before the POST lands,
    /// and the optimistic local clear must be settled either way.
    SecretsAuditUnmarked {
        area_id: AreaId,
        request: SecretMarksRequest,
        result: Result<SecretMarksResult, String>,
    },
    /// The signed-out banner's CTA; bubbles up as [`Event::OpenSettings`].
    OpenSettingsRequested,
    /// The signed-out banner's close affordance; hides the banner and persists
    /// the dismissal against the current client version.
    DismissSigninBanner,
    /// The toolbar sync indicator, pressed while idle; wakes the mapper for an
    /// immediate sync (the engine has no periodic poll).
    SyncNowRequested,
    /// Toggle whether `area_id` participates in room identification/routing.
    ToggleAreaEnabled(AreaId),
    /// Make `area_id` the active copy of its copy-family: enable it and
    /// disable every other family member.
    SetActiveCopy(AreaId),
    /// Received grants + the area list loaded together on the sync tick
    /// (signed-in only); rebuilds [`Self::sharers`] from the grants and
    /// [`Self::family_index`] from the areas' `family_token`s.
    IndicesLoaded {
        grants: Result<Vec<ShareGrantRow>, CloudError>,
        areas: Result<Vec<Area>, CloudError>,
    },
    /// Owner self-copy ("Duplicate"); bubbles like [`Message::CopyAreaRequested`]
    /// but produces an inactive clone.
    DuplicateAreaRequested,

    // ===== atlases (folders for your own maps) =====
    /// The owned-atlas inventory finished loading (refreshed on the sync tick).
    AtlasesLoaded(Result<Vec<AtlasListItem>, CloudError>),
    /// Open the create-folder modal.
    NewAtlasRequested,
    CreateAtlasNameChanged(String),
    /// Pick the new folder's tier: `true` = local, `false` = cloud.
    CreateAtlasTierChanged(bool),
    CreateAtlasConfirmed,
    AtlasCreated(Result<AtlasId, String>),
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
    /// File an owned area into a folder (`Some`) or pull it loose (`None`).
    MoveAreaToAtlas {
        area: AreaId,
        atlas: Option<AtlasId>,
    },
    /// Collapse/expand a folder in the area list (pure view state).
    ToggleFolderCollapsed(FolderKey),
    /// Open the atlas-scoped "Share folder…" dialog.
    ShareAtlasRequested(AtlasId),
    /// Share-folder dialog internals, routed to [`modals::update_share_atlas`].
    ShareAtlas(modals::ShareAtlasMessage),
    /// Open the transfer-ownership offer for the active area (owner-only).
    TransferOwnershipRequested,
    /// Open the transfer offer for a specific area (area-list row).
    TransferAreaOwnershipRequested(AreaId),
    /// Open the transfer offer for a folder (folder header).
    TransferAtlasOwnershipRequested(AtlasId),
    /// Transfer-offer dialog internals, routed to [`modals::update_transfer`].
    Transfer(modals::TransferMessage),

    // ===== cloud-map scope (per-server atlas visibility) =====
    /// The scope control: `true` = All atlases, `false` = This server.
    ScopeAllToggled(bool),
    /// Open the "Servers…" checklist for an atlas or atlas-less area.
    ServersChecklistRequested(ScopeTarget),
    /// Show/hide the checklist's target on one server entry.
    ScopeServerToggled { entry: String, show: bool },
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

    let IcedEvent::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event else {
        return None;
    };

    let hotkey = match key.as_ref() {
        keyboard::Key::Named(Named::Delete | Named::Backspace) => Hotkey::Delete,
        keyboard::Key::Named(Named::ArrowLeft) => Hotkey::Nudge(-1, 0),
        keyboard::Key::Named(Named::ArrowRight) => Hotkey::Nudge(1, 0),
        keyboard::Key::Named(Named::ArrowUp) => Hotkey::Nudge(0, -1),
        keyboard::Key::Named(Named::ArrowDown) => Hotkey::Nudge(0, 1),
        keyboard::Key::Named(Named::Escape) => Hotkey::Escape,
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
        _ => return None,
    };

    Some(Message::Hotkey(window_id, hotkey))
}

/// Identifies one folder in the "My maps" tree for collapse/expand state. A
/// named atlas, or the catch-all "Loose" bucket for own areas filed under no
/// atlas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderKey {
    Atlas(AtlasId),
    Loose,
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
    /// App-global cloud handles; the secrecy UI talks to the API directly
    /// (secret marks are not mapper sync operations).
    cloud: CloudHandles,
    panes: pane_grid::State<PaneKind>,
    editor: MapEditor,
    stack: commands::CommandStack,
    inspector: inspector::State,
    hovered_room: Option<RoomKey>,
    last_seen_rev: Option<i64>,
    /// A creation command whose entities should be selected as their async
    /// creates resolve (drag-rect creation, paste).
    pending_select: Option<commands::CommandId>,
    /// Copied entities awaiting paste, with a counter cascading the
    /// same-area paste offset. Behind `ArcSwap` so one clipboard can be
    /// shared across editor windows; each window gets its own
    /// (see [`Self::with_clipboard`]).
    clipboard: SharedClipboard,
    consecutive_pastes: u32,
    /// When the rooms-excluded-from-copy notice was shown; renders as a
    /// transient banner under the toolbar until the tick expires it.
    room_copy_notice: Option<Instant>,
    modal: Option<modals::Modal>,
    /// In-progress inline rename in the area list.
    renaming_area: Option<(AreaId, String)>,
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
}

impl MapEditorWindow {
    /// Builds the window with an injected clipboard, so every editor window
    /// can share one app-global clipboard (for the two-window merge workflow).
    pub fn with_clipboard(
        window_id: window::Id,
        mapper: Mapper,
        cloud: CloudHandles,
        clipboard: SharedClipboard,
        server_name: String,
        map_scopes: MapScopes,
    ) -> Self {
        let first_area =
            area_list::first_area_id(&mapper.get_current_atlas(), &mapper.ephemeral_area_ids());

        let (mut panes, area_list_pane) = pane_grid::State::new(PaneKind::AreaList);

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
            hovered_room: None,
            last_seen_rev: None,
            pending_select: None,
            clipboard,
            consecutive_pastes: 0,
            room_copy_notice: None,
            modal: None,
            renaming_area: None,
            pending_copied_area: None,
            sharers: None,
            family_index: FamilyIndex::default(),
            // Seeded None so the first Tick fetches the sharer index (the
            // mapper's revision will differ from this).
            last_seen_sync_revision: None,
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
        };
        // Default to This-server scope only when a server context exists.
        window.scope_all = window.server_name.is_none();
        window.collapsed_folders.insert(FolderKey::Unassigned);
        window.inspector.resync(&window.mapper, &window.editor);
        window
    }

    /// The server entry this editor scopes to (for the daemon's per-entry
    /// exclusion fan-out), or `None` when it has no session context.
    #[must_use]
    pub fn server_name(&self) -> Option<&str> {
        self.server_name.as_deref()
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
            || self.mapper.is_ephemeral(&area_id)
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
        if self.mapper.is_ephemeral(&area_id) || self.mapper.local_area_ids().contains(&area_id) {
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
        Task::perform(
            async move {
                let grants = client.shares(ShareDirection::Received).await;
                // The area list is the only carrier of `family_token`
                // (`get_area`/cache never include it), so we refetch it here
                // rather than reading the geometry cache.
                let areas = mapper.list_areas().await;
                (grants, areas)
            },
            |(grants, areas)| Message::IndicesLoaded { grants, areas },
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
    fn apply_recipient_homing(&mut self, grants: &[ShareGrantRow], areas: &[Area]) -> Vec<ScopeDelta> {
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
        Task::perform(
            async move { mapper.list_atlases().await },
            Message::AtlasesLoaded,
        )
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
            return "Shared by a friend".to_string();
        };
        let meta = area.meta();
        let owner_label = meta
            .owner_nickname
            .clone()
            .unwrap_or_else(|| "a friend".to_string());

        let resolved = self
            .sharers
            .as_ref()
            .and_then(|index| index.sharer_for(area_id, meta.atlas_id));

        match resolved {
            Some(sharer) => {
                let sharer_label = sharer
                    .nickname
                    .clone()
                    .unwrap_or_else(|| "a friend".to_string());
                // Re-share: name both the sharer and the underlying owner.
                if meta.owner_id.is_some_and(|owner_id| owner_id != sharer.user_id) {
                    // Owner handle: prefer GET /areas (meta), fall back to the
                    // grant's owner_nickname, then to "a friend".
                    let owner_label = meta
                        .owner_nickname
                        .clone()
                        .or_else(|| sharer.owner_nickname.clone())
                        .unwrap_or_else(|| "a friend".to_string());
                    format!("Shared by {sharer_label} \u{00b7} owned by {owner_label}")
                } else {
                    format!("Shared by {sharer_label}")
                }
            }
            // Index not loaded / scope not covered: keep today's owner-handle
            // attribution as the fallback.
            None => format!("Shared by {owner_label}"),
        }
    }

    /// This window's mapper (shared, cheaply cloneable). The daemon uses it
    /// to fan disabled-area changes out across windows.
    pub fn mapper(&self) -> &Mapper {
        &self.mapper
    }

    pub fn can_undo(&self) -> bool {
        self.stack.can_undo()
    }

    pub fn can_redo(&self) -> bool {
        self.stack.can_redo()
    }

    /// The window title shown in the OS titlebar.
    pub fn title(&self) -> String {
        let atlas = self.mapper.get_current_atlas();
        self.editor
            .area_id()
            .and_then(|id| atlas.get_area(&id))
            .map_or_else(
                || "smudgy map editor".to_string(),
                |area| format!("smudgy map editor - {}", area.get_name()),
            )
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            iced::event::listen_with(editor_hotkeys),
            iced::time::every(Duration::from_millis(500)).map(|_| Message::Tick),
        ])
    }

    /// Re-reads the active area's revision, marking the current cache state
    /// as already seen so the next [`Message::Tick`] doesn't treat our own
    /// writes as external changes.
    fn refresh_seen_rev(&mut self) {
        let atlas = self.mapper.get_current_atlas();
        self.last_seen_rev = self
            .editor
            .area_id()
            .and_then(|id| atlas.get_area(&id))
            .map(|area| area.get_rev());
    }

    /// Whether the viewer may set or clear secret flags in the active area.
    /// When false the secrecy UI is hidden entirely (the server uniform-404s
    /// non-cleared attempts; we never tempt the user).
    fn secrets_cleared(&self) -> bool {
        let atlas = self.mapper.get_current_atlas();
        self.editor
            .area_id()
            .and_then(|id| atlas.get_area(&id))
            .is_some_and(|area| area.effective_access().is_cleared_for_secrets())
    }

    /// Whether the viewer owns the active area (the secrets audit is
    /// owner-only).
    fn area_is_owned(&self) -> bool {
        let atlas = self.mapper.get_current_atlas();
        self.editor
            .area_id()
            .and_then(|id| atlas.get_area(&id))
            .is_some_and(|area| area.is_owned())
    }

    /// Whether the active area is disabled for room identification (false
    /// when no area is active).
    fn active_area_disabled(&self) -> bool {
        self.editor
            .area_id()
            .is_some_and(|id| !self.mapper.is_area_enabled(&id))
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

    /// Whether the share dialog applies to the active area: owners always,
    /// plus grantees holding `can_reshare`.
    fn can_share_active_area(&self) -> bool {
        self.active_access()
            .is_some_and(|access| access.is_owner || access.can_reshare)
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

    /// Fetches the owner-only secrets audit list for `area_id`.
    fn fetch_secrets_audit(&self, area_id: AreaId) -> Task<Message> {
        let client = self.cloud.client.clone();
        Task::perform(async move { client.area_secrets(area_id).await }, |result| {
            Message::SecretsAuditLoaded(result.map_err(|error| error.to_string()))
        })
    }

    /// Selects an audited entity in the editor, following it to its level.
    /// Properties select their owning room; area properties show the area
    /// view (empty selection). Centering is left to the user's viewport.
    fn jump_to_secret(&mut self, entity: &SecretEntity) {
        let Some(area_id) = self.editor.area_id() else {
            return;
        };
        let atlas = self.mapper.get_current_atlas();
        let Some(area) = atlas.get_area(&area_id) else {
            return;
        };

        match entity.kind {
            SecretEntityKind::Room | SecretEntityKind::RoomProperty => {
                if let Some(room) = entity
                    .room_number
                    .map(RoomNumber)
                    .and_then(|number| area.get_room(&number).cloned())
                {
                    self.editor.set_level(room.get_level());
                    self.editor.select(EntityId::Room(room.get_room_number()));
                }
            }
            SecretEntityKind::Exit => {
                if let Some(exit_id) = entity.id.map(ExitId)
                    && let Some(room) = area
                        .get_rooms()
                        .iter()
                        .find(|room| room.get_exits().iter().any(|exit| exit.id == exit_id))
                {
                    self.editor.set_level(room.get_level());
                    self.editor.select(EntityId::Room(room.get_room_number()));
                }
            }
            SecretEntityKind::Label => {
                if let Some(label_id) = entity.id.map(LabelId)
                    && let Some(label) = area.get_label(&label_id)
                {
                    self.editor.set_level(label.level);
                    self.editor.select(EntityId::Label(label_id));
                }
            }
            SecretEntityKind::Shape => {
                if let Some(shape_id) = entity.id.map(ShapeId)
                    && let Some(shape) = area.get_shape(&shape_id)
                {
                    self.editor.set_level(shape.level);
                    self.editor.select(EntityId::Shape(shape_id));
                }
            }
            SecretEntityKind::AreaProperty => {
                self.editor.clear_selection();
            }
        }

        self.inspector.resync(&self.mapper, &self.editor);
    }

    /// Builds, applies, and records a mutation command, mapping its async
    /// completions back into window messages.
    ///
    /// This is the central capability gate: every entity edit (inspector,
    /// canvas, hotkeys, paste) funnels through here, so a view-only shared
    /// area can't be mutated even if some UI affordance slips through.
    fn push_command(&mut self, command: Option<commands::Command>) -> Update<Message, Event> {
        match command {
            Some(command) => {
                if !self.can_edit_active_area() {
                    log::info!("map editor: ignoring mutation — the active area is view-only");
                    return Update::none();
                }
                let task = self
                    .stack
                    .push_and_apply(&self.mapper, command)
                    .map(Message::CommandCompleted);
                self.refresh_seen_rev();
                Update::with_task(task)
            }
            None => Update::none(),
        }
    }

    fn handle_mutation_request(&mut self, request: MutationRequest) -> Update<Message, Event> {
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        if !self.can_edit_active_area() {
            log::info!("map editor: ignoring mutation request — the active area is view-only");
            return Update::none();
        }

        match request {
            MutationRequest::MoveSelection { offset } => {
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
                let atlas = self.mapper.get_current_atlas();
                let Some(area) = atlas.get_area(&area_id) else {
                    return Update::none();
                };
                let room_number = area.next_room_number();
                let update = self.push_command(Some(commands::create_room(
                    area_id,
                    room_number,
                    at,
                    self.editor.level(),
                )));
                self.editor.select(EntityId::Room(room_number));
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
                let target = match to {
                    ExitTarget::Room(room_number) => commands::NewExitTarget::Room(room_number),
                    ExitTarget::Empty(at) => {
                        let atlas = self.mapper.get_current_atlas();
                        let Some(area) = atlas.get_area(&area_id) else {
                            return Update::none();
                        };
                        commands::NewExitTarget::NewRoom {
                            room_number: area.next_room_number(),
                            at,
                            level: self.editor.level(),
                        }
                    }
                };

                let update = self.push_command(Some(commands::create_exit(
                    area_id,
                    from,
                    from_direction,
                    &target,
                    to_direction,
                    one_way,
                )));

                if let commands::NewExitTarget::NewRoom { room_number, .. } = target {
                    self.editor.select(EntityId::Room(room_number));
                }
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
        }
    }

    fn delete_selection(&mut self) -> Update<Message, Event> {
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        if !self.can_edit_active_area() {
            return Update::none();
        }
        let command = commands::delete_selection(
            &self.mapper.get_current_atlas(),
            area_id,
            self.editor.selection(),
        );
        self.editor.clear_selection();
        let update = self.push_command(command);
        self.inspector.resync(&self.mapper, &self.editor);
        update
    }

    /// Whether the viewer may copy rooms out of the active area: owners
    /// always, grantees only with `can_copy`. (Labels/shapes always copy.)
    fn allow_room_copy(&self) -> bool {
        self.active_access()
            .is_some_and(|access| access.is_owner || access.can_copy)
    }

    /// Copies the selection into the clipboard (rooms only where the owner
    /// allows it; see [`Self::allow_room_copy`]). Returns false when the
    /// selection holds nothing copyable (the clipboard is kept).
    fn copy_selection(&mut self) -> bool {
        let Some(area_id) = self.editor.area_id() else {
            return false;
        };
        let allow_rooms = self.allow_room_copy();
        if !allow_rooms && self.editor.selection().rooms().next().is_some() {
            // Labels/shapes still copy, but rooms silently staying behind
            // would be confusing — say why.
            self.room_copy_notice = Some(Instant::now());
        }
        let snapshot = commands::snapshot_selection(
            &self.mapper.get_current_atlas(),
            area_id,
            self.editor.selection(),
            allow_rooms,
        );
        if snapshot.is_empty() {
            return false;
        }
        self.clipboard.store(Arc::new(snapshot));
        self.consecutive_pastes = 0;
        true
    }

    fn paste_clipboard(&mut self) -> Update<Message, Event> {
        let Some(area_id) = self.editor.area_id() else {
            return Update::none();
        };
        let clipboard = self.clipboard.load_full();
        if clipboard.is_empty() || !self.can_edit_active_area() {
            return Update::none();
        }

        // Same-area pastes cascade so copies don't land exactly on their
        // sources; cross-area pastes preserve exact positions (and source
        // room numbers where vacant) so merged-back changes line up.
        let same_area = clipboard.source_area_id == Some(area_id);
        let offset = if same_area {
            self.consecutive_pastes += 1;
            #[allow(clippy::cast_precision_loss)]
            let step = self.consecutive_pastes as f32;
            Vector::new(step, step)
        } else {
            Vector::new(0.0, 0.0)
        };

        let Some((command, pasted_rooms)) = commands::paste_clipboard(
            &self.mapper.get_current_atlas(),
            area_id,
            &clipboard,
            self.editor.level(),
            offset,
        ) else {
            return Update::none();
        };
        let update = self.push_command(Some(command));

        // The pasted entities become the selection: rooms synchronously
        // (their numbers are known up front), labels/shapes as their async
        // creates resolve.
        self.pending_select = self.stack.last_command_id();
        self.editor.clear_selection();
        for room_number in pasted_rooms {
            self.editor.add_to_selection(EntityId::Room(room_number));
        }
        self.inspector.resync(&self.mapper, &self.editor);
        update
    }

    fn handle_hotkey(&mut self, hotkey: Hotkey) -> Update<Message, Event> {
        match hotkey {
            Hotkey::Delete => self.delete_selection(),
            Hotkey::Copy => {
                self.copy_selection();
                Update::none()
            }
            Hotkey::Cut => {
                // Cutting from a view-only area degrades to a plain copy.
                let allow_rooms = self.allow_room_copy();
                if !self.copy_selection() || !self.can_edit_active_area() {
                    return Update::none();
                }
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                // Delete only what was copied; rooms stay put when the
                // owner hasn't allowed copying them.
                let cut: map_editor::Selection = self
                    .editor
                    .selection()
                    .iter()
                    .filter(|entity| match entity {
                        EntityId::Label(_) | EntityId::Shape(_) => true,
                        EntityId::Room(_) => allow_rooms,
                    })
                    .collect();
                let command = commands::delete_selection(
                    &self.mapper.get_current_atlas(),
                    area_id,
                    &cut,
                );
                for entity in cut.iter() {
                    self.editor.remove_from_selection(entity);
                }
                let update = self.push_command(command);
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
            Hotkey::Paste => self.paste_clipboard(),
            Hotkey::Nudge(dx, dy) => {
                if self.editor.selection().is_empty() {
                    return Update::none();
                }
                #[allow(clippy::cast_precision_loss)]
                let offset = Vector::new(dx as f32, dy as f32);
                self.handle_mutation_request(MutationRequest::MoveSelection { offset })
            }
            Hotkey::Undo => {
                // Undo replays mutations through the Mapper, so it honors
                // the same capability gate as push_command: history recorded
                // before a mid-session permission downgrade must not replay
                // into a now view-only area. (Message::Undo, the toolbar
                // button, routes here too.)
                if !self.can_edit_active_area() {
                    return Update::none();
                }
                let task = self.stack.undo(&self.mapper).map(Message::CommandCompleted);
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::with_task(task)
            }
            Hotkey::Redo => {
                // Same capability gate as Hotkey::Undo above.
                if !self.can_edit_active_area() {
                    return Update::none();
                }
                let task = self.stack.redo(&self.mapper).map(Message::CommandCompleted);
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::with_task(task)
            }
            Hotkey::Escape => {
                if self.modal.is_some() {
                    self.modal = None;
                } else if self.renaming_area.is_some() {
                    self.renaming_area = None;
                } else if self.renaming_atlas.is_some() {
                    self.renaming_atlas = None;
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
                    self.editor.set_level_keeping_selection(self.editor.level() + delta);
                }
                self.inspector.resync(&self.mapper, &self.editor);
                update
            }
        }
    }

    pub fn update(&mut self, message: Message) -> Update<Message, Event> {
        match message {
            Message::Editor(message) => {
                let update = self.editor.update(message).map_message(Message::Editor);

                let mut result = Update::with_task(update.task);

                if let Some(event) = update.event {
                    match event {
                        map_editor::Event::HoveredRoomChanged(room_key) => {
                            self.hovered_room = room_key;
                        }
                        map_editor::Event::SelectionChanged => {
                            self.inspector.resync(&self.mapper, &self.editor);
                        }
                        map_editor::Event::RequestMutation(request) => {
                            result = self.handle_mutation_request(request);
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
                if self.editor.area_id() != Some(area_id) {
                    self.editor.set_area(Some(area_id));
                    self.hovered_room = None;
                    // Undo history is area-local by design.
                    self.stack.clear();
                    // Creation tools are meaningless in a view-only area.
                    if !self.can_edit_active_area() && self.editor.tool() != Tool::Select {
                        self.editor.set_tool(Tool::Select);
                    }
                    self.refresh_seen_rev();
                    self.inspector.resync(&self.mapper, &self.editor);
                    // Bind-on-use: opening an unassigned atlas from this
                    // session's This-server tree homes it here.
                    return Update::new(Task::none(), self.associate_opened_area(area_id));
                }
                Update::none()
            }
            Message::ToolSelected(tool) => {
                if tool != Tool::Select && !self.can_edit_active_area() {
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

                // A background sync (cache swap) bumps the mapper's revision;
                // refetch the sharer index and atlas inventory then (and on
                // the first tick). Both are no-ops while signed out.
                let sync_rev = self.mapper.sync_revision();
                let mut sharer_task = Task::none();
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
                        sharer_task = Task::batch([sharer_task, self.fetch_atlases()]);
                    } else if !self.atlases.is_empty() {
                        self.atlases.clear();
                    }
                }

                // The rooms-not-copyable notice expires on its own.
                if self
                    .room_copy_notice
                    .is_some_and(|shown| shown.elapsed() >= ROOM_COPY_NOTICE_TTL)
                {
                    self.room_copy_notice = None;
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
                if !self.stack.is_empty() && !self.can_edit_active_area() {
                    self.stack.clear();
                }

                let rev = self
                    .editor
                    .area_id()
                    .and_then(|id| atlas.get_area(&id))
                    .map(|area| area.get_rev());
                if rev != self.last_seen_rev {
                    self.last_seen_rev = rev;
                    self.inspector.resync(&self.mapper, &self.editor);
                }
                Update::with_task(sharer_task)
            }
            Message::Inspector(message) => self.update_inspector(message),
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

                self.stack.resolve(&self.mapper, outcome);
                // Creates land in the cache only on completion (backend
                // assigns the id), so dependent UI refreshes now.
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::none()
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
                self.modal = Some(modals::Modal::CreateArea {
                    name: String::new(),
                    error: None,
                    atlas_id: None,
                });
                Update::none()
            }
            Message::CreateAreaNameChanged(value) => {
                if let Some(modals::Modal::CreateArea { name, .. }) = &mut self.modal {
                    *name = value;
                }
                Update::none()
            }
            Message::CreateAreaConfirmed => {
                let Some(modals::Modal::CreateArea { name, atlas_id, .. }) = &self.modal else {
                    return Update::none();
                };
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Update::none();
                }
                let atlas_id = *atlas_id;
                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move { mapper.create_area_in(name, atlas_id).await },
                    |result| Message::AreaCreated(result.map_err(|error| error.to_string())),
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
                    Err(error) => {
                        if let Some(modals::Modal::CreateArea { error: slot, .. }) =
                            &mut self.modal
                        {
                            *slot = Some(error);
                        }
                    }
                }
                Update::none()
            }
            Message::RenameAreaStarted(area_id) => {
                // Rename is owner-only server-side; never offer it locally.
                if !self.area_owned(area_id) {
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
                    let name = name.trim();
                    if !name.is_empty() && self.area_owned(area_id) {
                        // Area management deliberately bypasses the undo
                        // stack.
                        self.mapper.rename_area(area_id, name);
                        self.refresh_seen_rev();
                    }
                }
                Update::none()
            }
            Message::DeleteAreaRequested(area_id) => {
                let atlas = self.mapper.get_current_atlas();
                if let Some(area) = atlas.get_area(&area_id)
                    && area.is_owned()
                {
                    self.modal = Some(modals::Modal::ConfirmDeleteArea {
                        area_id,
                        name: area.get_name().to_string(),
                        room_count: area.room_count(),
                    });
                }
                Update::none()
            }
            Message::DeleteAreaConfirmed => {
                let Some(modals::Modal::ConfirmDeleteArea { area_id, .. }) = self.modal.take()
                else {
                    return Update::none();
                };
                if !self.area_owned(area_id) {
                    return Update::none();
                }

                self.mapper.delete_area(area_id);
                self.stack.clear();

                let next_area = area_list::first_area_id(
                    &self.mapper.get_current_atlas(),
                    &self.mapper.ephemeral_area_ids(),
                );
                self.editor.set_area(next_area);
                self.hovered_room = None;
                if !self.can_edit_active_area() && self.editor.tool() != Tool::Select {
                    self.editor.set_tool(Tool::Select);
                }
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);
                Update::none()
            }
            Message::ModalDismissed => {
                self.modal = None;
                Update::none()
            }
            Message::OpenSettingsRequested => Update::with_event(Event::OpenSettings),
            Message::DismissSigninBanner => {
                self.signin_banner_dismissed = true;
                if let Err(error) = smudgy_core::models::settings::set_dismissed_signin_banner_version(
                    env!("CARGO_PKG_VERSION"),
                ) {
                    log::warn!("map editor: failed to persist signed-out banner dismissal: {error}");
                }
                Update::none()
            }
            Message::SyncNowRequested => {
                self.mapper.sync_now();
                Update::none()
            }
            Message::IndicesLoaded { grants, areas } => {
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
                    Ok(areas) => self.family_index = FamilyIndex::build(&areas),
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
                }));
                Update::none()
            }
            Message::ShareDialogRequested => modals::open_share_dialog(self),
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
                self.modal = Some(modals::Modal::CopyArea(modals::CopyAreaDialog {
                    source: area_id,
                    source_name: area.get_name().to_string(),
                    name: format!("{} (copy)", area.get_name()),
                    atlas_id: area.meta().atlas_id,
                    busy: false,
                    error: None,
                    atlas_report: None,
                    duplicate: false,
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
                if dialog.busy || name.is_empty() {
                    return Update::none();
                }
                dialog.busy = true;
                dialog.error = None;
                let source = dialog.source;
                // Captured now so the completion handler is independent of
                // whether the modal is still open (it can be dismissed
                // mid-copy).
                let duplicate = dialog.duplicate;
                let request = CopyAreaRequest {
                    name: Some(name),
                    atlas_id: None,
                };
                let client = self.cloud.client.clone();
                Update::with_task(Task::perform(
                    async move {
                        client
                            .copy_area(source, &request)
                            .await
                            .map(|area| area.id)
                    },
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
                            dialog.atlas_report = Some(format!(
                                "Copied {} areas; skipped {} (not copyable).",
                                report.copied.len(),
                                report.skipped.len()
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
            Message::SecretsAuditRequested => {
                let Some(area_id) = self.editor.area_id() else {
                    return Update::none();
                };
                self.modal = Some(modals::Modal::SecretsAudit {
                    area_id,
                    entries: None,
                    error: None,
                });
                Update::with_task(self.fetch_secrets_audit(area_id))
            }
            Message::SecretsAuditLoaded(result) => {
                if let Some(modals::Modal::SecretsAudit { entries, error, .. }) = &mut self.modal {
                    match result {
                        Ok(list) => {
                            *entries = Some(list);
                            *error = None;
                        }
                        Err(message) => *error = Some(message),
                    }
                }
                Update::none()
            }
            Message::SecretsAuditJump(entity) => {
                self.modal = None;
                self.jump_to_secret(&entity);
                Update::none()
            }
            Message::SecretsAuditUnmark(entity) => {
                let Some(modals::Modal::SecretsAudit { area_id, .. }) = &self.modal else {
                    return Update::none();
                };
                let area_id = *area_id;
                let request = secret_marks_request_for(&entity, false);

                // Optimistic local clear, reverted if the POST fails. Like
                // all secrecy edits this bypasses the undo stack (it mirrors
                // a server-side flag, not map geometry).
                inspector::apply_marks_locally(&self.mapper, area_id, &request, false);
                self.refresh_seen_rev();
                self.inspector.resync(&self.mapper, &self.editor);

                let client = self.cloud.client.clone();
                let echo = request.clone();
                Update::with_task(Task::perform(
                    async move { client.secret_marks(area_id, &request).await },
                    move |result| Message::SecretsAuditUnmarked {
                        area_id,
                        request: echo.clone(),
                        result: result.map_err(|error| error.to_string()),
                    },
                ))
            }
            Message::SecretsAuditUnmarked {
                area_id,
                request,
                result,
            } => {
                // Settle the optimistic clear unconditionally — the modal
                // may have closed (or moved to another area) mid-flight.
                // Only the modal's own error/entry refresh is conditional on
                // it still showing the same area.
                let modal_shows_area = matches!(
                    &self.modal,
                    Some(modals::Modal::SecretsAudit { area_id: open, .. }) if *open == area_id
                );
                match result {
                    Ok(_) => {
                        // The server bumped the rev; pull it promptly, and
                        // refresh the audit list if it's still on screen.
                        self.mapper.sync_now();
                        if modal_shows_area {
                            Update::with_task(self.fetch_secrets_audit(area_id))
                        } else {
                            Update::none()
                        }
                    }
                    Err(message) => {
                        // Revert the optimistic clear.
                        inspector::apply_marks_locally(&self.mapper, area_id, &request, true);
                        self.refresh_seen_rev();
                        self.inspector.resync(&self.mapper, &self.editor);
                        if modal_shows_area
                            && let Some(modals::Modal::SecretsAudit { error, .. }) =
                                &mut self.modal
                        {
                            *error = Some(message);
                        }
                        Update::none()
                    }
                }
            }

            // ===== atlases (folders) =====
            Message::AtlasesLoaded(result) => {
                let mut deltas = Vec::new();
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
                    }
                    // Signed out / unverified: no cloud folders to show.
                    Err(CloudError::Unauthorized(_) | CloudError::EmailNotVerified) => {
                        self.atlases.clear();
                    }
                    // Keep the prior inventory on a transient failure.
                    Err(error) => log::warn!("map editor: atlas list fetch failed: {error}"),
                }
                if deltas.is_empty() {
                    Update::none()
                } else {
                    Update::with_event(Event::ScopeAssociationsChanged(deltas))
                }
            }
            Message::NewAtlasRequested => {
                // Cloud is the default tier when signed in; a signed-out
                // session can only create local folders.
                let signed_in = self.cloud.snapshot.get().signed_in;
                self.modal = Some(modals::Modal::CreateAtlas {
                    name: String::new(),
                    error: None,
                    local: !signed_in,
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
            Message::CreateAtlasTierChanged(local) => {
                if let Some(modals::Modal::CreateAtlas {
                    local: slot,
                    cloud_available,
                    ..
                }) = &mut self.modal
                {
                    // Cloud can't be chosen when it isn't available.
                    *slot = local || !*cloud_available;
                }
                Update::none()
            }
            Message::CreateAtlasConfirmed => {
                let Some(modals::Modal::CreateAtlas { name, local, .. }) = &self.modal else {
                    return Update::none();
                };
                let name = name.trim().to_string();
                if name.is_empty() {
                    return Update::none();
                }
                let local = *local;
                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move { mapper.create_atlas_in(name, local).await },
                    |result| {
                        Message::AtlasCreated(
                            result.map(|atlas| atlas.id).map_err(|e| e.to_string()),
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
                        if let Some(modals::Modal::CreateAtlas { error: slot, .. }) = &mut self.modal
                        {
                            *slot = Some(error);
                        }
                    }
                }
                Update::none()
            }
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
                    |result| Message::AtlasRenamed(result.map(|_| ()).map_err(|e| e.to_string())),
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
                if let Some(atlas) = self.atlases.iter().find(|atlas| atlas.id == atlas_id) {
                    self.modal = Some(modals::Modal::ConfirmDeleteAtlas {
                        atlas_id,
                        name: atlas.name.clone(),
                        area_count: atlas.area_count,
                    });
                }
                Update::none()
            }
            Message::DeleteAtlasConfirmed => {
                let Some(modals::Modal::ConfirmDeleteAtlas { atlas_id, .. }) = self.modal.take()
                else {
                    return Update::none();
                };
                // Optimistic: drop the folder from the inventory. Its member
                // areas fall back to Loose on their own (grouping ignores
                // atlas ids absent from the inventory), matching the server's
                // gentle delete (member areas survive, atlas_id -> NULL).
                self.atlases.retain(|atlas| atlas.id != atlas_id);
                self.collapsed_folders.remove(&FolderKey::Atlas(atlas_id));
                let mapper = self.mapper.clone();
                Update::with_task(Task::perform(
                    async move { mapper.delete_atlas(atlas_id).await },
                    |result| Message::AtlasDeleted(result.map_err(|e| e.to_string())),
                ))
            }
            Message::AtlasDeleted(result) => {
                if let Err(error) = result {
                    log::warn!("map editor: atlas delete failed: {error}");
                    return Update::with_task(self.fetch_atlases());
                }
                Update::none()
            }
            Message::NewAreaInAtlas(atlas_id) => {
                self.modal = Some(modals::Modal::CreateArea {
                    name: String::new(),
                    error: None,
                    atlas_id: Some(atlas_id),
                });
                Update::none()
            }
            Message::MoveAreaRequested(area_id) => {
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
                let current_atlas = atlas.get_area(&area_id).and_then(|area| area.meta().atlas_id);
                // Only same-tier folders are valid targets: moving a map
                // between the local and cloud tiers is a migration, not a
                // re-file (and the composite would reject it). Loose is always
                // offered (handled by the modal).
                let area_is_local = self.mapper.local_area_ids().contains(&area_id);
                let mut folders: Vec<(AtlasId, String)> = self
                    .atlases
                    .iter()
                    .filter(|atlas| self.local_atlas_ids.contains(&atlas.id) == area_is_local)
                    .map(|atlas| (atlas.id, atlas.name.clone()))
                    .collect();
                folders.sort_by(|a, b| {
                    a.1.to_lowercase().cmp(&b.1.to_lowercase()).then_with(|| a.1.cmp(&b.1))
                });
                self.modal = Some(modals::Modal::MoveArea {
                    area_id,
                    area_name,
                    current_atlas,
                    folders,
                });
                Update::none()
            }
            Message::MoveAreaToAtlas { area, atlas } => {
                if self.area_owned(area) {
                    self.mapper.move_area_to_atlas(area, atlas);
                    self.refresh_seen_rev();
                }
                self.modal = None;
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
            Message::TransferOwnershipRequested => {
                let atlas = self.mapper.get_current_atlas();
                match self
                    .editor
                    .area_id()
                    .and_then(|id| atlas.get_area(&id).map(|a| (id, a.get_name().to_string())))
                {
                    Some((id, name)) => {
                        modals::open_transfer_dialog(self, modals::TransferSubject::Area(id, name))
                    }
                    None => Update::none(),
                }
            }
            Message::TransferAreaOwnershipRequested(area_id) => {
                let name = self
                    .mapper
                    .get_current_atlas()
                    .get_area(&area_id)
                    .map(|a| a.get_name().to_string())
                    .unwrap_or_default();
                modals::open_transfer_dialog(self, modals::TransferSubject::Area(area_id, name))
            }
            Message::TransferAtlasOwnershipRequested(atlas_id) => {
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
                if let Some(modals::Modal::ServersChecklist { target, checked, .. }) =
                    &mut self.modal
                {
                    *checked = match *target {
                        ScopeTarget::Atlas(atlas_id) => self.map_scopes.atlas_entries(&atlas_id),
                        ScopeTarget::Area(area_id) => self.map_scopes.area_entries(&area_id),
                    };
                }
                Update::none()
            }
        }
    }

    pub fn view(&self) -> ThemedElement<'_, Message> {
        let panes = PaneGrid::new(&self.panes, |_pane, kind, _maximized| {
            pane_grid::Content::new(match kind {
                PaneKind::AreaList => area_list::view(self),
                PaneKind::Canvas => self.editor.view().map(Message::Editor),
                PaneKind::Inspector => inspector::view(self),
            })
        })
        .width(Length::Fill)
        .height(Length::Fill)
        .spacing(4)
        .on_resize(8, Message::PaneResized);

        let mut layout = column![toolbar::view(self)];

        // Transient non-modal notice (rooms were excluded from a copy);
        // the periodic tick expires it.
        if self.room_copy_notice.is_some() {
            layout = layout.push(
                container(
                    text("This map's owner hasn't allowed copying rooms.").size(13),
                )
                .width(Length::Fill)
                .padding([6, 12])
                .style(theme::builtins::container::modal_title_bar),
            );
        }

        // Signed-out CTA: local maps still save to this device; signing in
        // adds cloud maps that sync across devices and can be shared. Mirrors
        // the main window's verify-email banner; the button forwards to the
        // settings window's Account tab, where sign-in/sign-up lives. The close
        // affordance hides it until the next client version (see
        // [`Self::signin_banner_dismissed`]).
        if !self.cloud.snapshot.get().signed_in && !self.signin_banner_dismissed {
            layout = layout.push(
                container(
                    row![
                        text(
                            "Local maps are saved on this device. Sign in to also use \
                             cloud maps that sync across devices and can be shared.",
                        )
                        .size(13),
                        button(text("Sign in or create account").size(12))
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

        let main_layout: ThemedElement<'_, Message> = layout
            .push(container(panes).width(Length::Fill).height(Length::Fill))
            .into();

        if let Some(modal) = &self.modal {
            stack(vec![
                main_layout,
                opaque(
                    mouse_area(
                        center(opaque(modal.view(&self.mapper)))
                            .style(theme::builtins::container::overlay),
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
    let mut representative: std::collections::HashMap<&str, AreaId> = std::collections::HashMap::new();
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
        CloudError::NotFoundOrNoAccess => "Copying isn't available for this map.".to_string(),
        other => other.to_string(),
    }
}

/// A one-entity secret-marks request, e.g. for the audit panel's per-row
/// "Unmark" button. Entities missing their identifying fields produce an
/// empty (no-op) request.
fn secret_marks_request_for(entity: &SecretEntity, secret: bool) -> SecretMarksRequest {
    let mut request = inspector::empty_secret_marks_request(secret);
    match entity.kind {
        SecretEntityKind::Room => {
            if let Some(number) = entity.room_number {
                request.rooms.push(number);
            }
        }
        SecretEntityKind::Exit => {
            if let Some(id) = entity.id {
                request.exits.push(ExitId(id));
            }
        }
        SecretEntityKind::Label => {
            if let Some(id) = entity.id {
                request.labels.push(LabelId(id));
            }
        }
        SecretEntityKind::Shape => {
            if let Some(id) = entity.id {
                request.shapes.push(ShapeId(id));
            }
        }
        SecretEntityKind::RoomProperty => {
            if let (Some(number), Some(name)) = (entity.room_number, entity.name.clone()) {
                request
                    .room_properties
                    .push(smudgy_cloud::cloud_api::RoomPropertyRef {
                        room_number: number,
                        name,
                    });
            }
        }
        SecretEntityKind::AreaProperty => {
            if let Some(name) = entity.name.clone() {
                request.area_properties.push(name);
            }
        }
    }
    request
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    fn area(n: u128) -> AreaId {
        AreaId(Uuid::from_u128(n))
    }

    fn no_tokens() -> std::collections::HashMap<AreaId, String> {
        std::collections::HashMap::new()
    }

    /// 3-area chain A<-B<-C plus an unrelated solo D.
    fn chain_edges() -> std::collections::HashMap<AreaId, Option<AreaId>> {
        let (a, b, c, d) = (area(1), area(2), area(3), area(4));
        [
            (a, None),
            (b, Some(a)),
            (c, Some(b)),
            (d, None),
        ]
        .into_iter()
        .collect()
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
}
