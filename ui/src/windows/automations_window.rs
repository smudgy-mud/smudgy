//! The **Automations** window — a separate desktop window where a player
//! manages everything that reacts to or augments their MUD session: aliases,
//! triggers, hotkeys, folders, modules, and packages.
//!
//! Structure: a fixed left **sidebar** (New menu + search + filter chips +
//! status-dotted tree + footer) and a flexible **main** column (a top action
//! bar over one content pane at a time). A Ctrl/⌘+P command palette overlays both.
//!
//! Uses the on-disk model (`aliases.json` / `triggers.json` / `hotkeys.json` /
//! `packages.json`, `modules/`, `packages/`, `smudgy.lock.json`) and the cloud clients.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::event::{Event as IcedEvent, Status};
use iced::keyboard::{self, key::Named};
use iced::widget::{markdown, text_editor};
use iced::{Subscription, Task};
use smudgy_cloud::cloud_api::FriendView;
use smudgy_cloud::package_api::{
    CommentView, PackageDetail, PackageGrantView, PackageSearchResult, ResolvedPackageWire,
    VersionListItem,
};
use smudgy_cloud::{CloudError, Uuid};
use smudgy_core::models::local_packages::{LocalPackage, PublishSummary};
use smudgy_core::models::modules::ModuleFile;
use smudgy_core::models::packages::{self as core_packages, PackageTree};
use smudgy_core::models::server;
use smudgy_core::models::shared_packages::{LockedPackage, PackagePermissions, UpdateMode};
use smudgy_core::models::{ScriptLang, aliases, hotkeys};
use smudgy_core::session::SessionId;
use smudgy_core::session::runtime::catalogue::{CatalogueEvent, CatalogueSnapshot};
use smudgy_core::session::runtime::{AutomationEvent, AutomationKind};

use crate::cloud_account::CloudHandles;
use crate::keymap::MaybePhysicalKey;
use crate::theme::Element as ThemedElement;
use crate::update::Update;

mod common;
mod dashboard;
mod editors;
mod manifest;
mod model;
mod packages;
mod palette;
mod param_values;
mod sidebar;
mod store_inspector;
mod topbar;

use manifest::{ManifestDraft, ManifestEdit, ManifestTab};
use model::{LiveAutomations, PackageGraph, PatternKind, Script, ScriptKey};
use packages::{
    ConsentPrompt, DetailSeq, FilePreview, ForkActivation, InstallResolution, InstallSeq,
    InstalledFileTab, ParamConfig, ParamPrompt, StaleInstallCheck, UpdateDelta,
};

/// Convenience alias for this window's themed elements.
pub(crate) type Elem<'a> = ThemedElement<'a, Message>;

/// Events bubbled up to the daemon so live sessions reload when scripts change.
#[derive(Debug, Clone)]
pub enum Event {
    ScriptsChanged { server_name: String },
}

/// Create vs. edit, shared by the script and folder editors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    Create,
    Edit,
}

/// The single-select filter chips above the tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chip {
    All,
    Aliases,
    Triggers,
    Hotkeys,
    Folders,
    Modules,
    Packages,
}

/// The Discover scope radios — a host-aware view over the wire `(host, SearchCategory)` pair
/// (translated in [`AutomationsWindow::discover_search`]). The host is this profile's MUD host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DiscoverScope {
    /// Aligned to this profile's MUD host *plus* universal packages — the useful default
    /// (`host` + `category=both`). With no profile host, this is equivalent to [`Self::All`].
    #[default]
    Relevant,
    /// Only packages aligned to this profile's MUD host (`host` + `category=mud`).
    HostOnly,
    /// Only host-agnostic (universal) packages (`category=universal`).
    Universal,
    /// Every public package, regardless of MUD alignment (no host + `category=both`).
    All,
}

/// The body of a script editor — the per-kind editable fields. The code body
/// lives in [`AutomationsWindow::editor_content`].
#[derive(Debug, Clone)]
pub enum EditNode {
    Alias(aliases::AliasDefinition),
    Hotkey(hotkeys::HotkeyDefinition),
    Trigger {
        enabled: bool,
        language: ScriptLang,
        prompt: bool,
        priority: i32,
        fallthrough: bool,
        package: Option<String>,
        /// The unified, ordered pattern list (Match/Anti/Raw per row).
        rows: Vec<(PatternKind, String)>,
    },
}

/// State for the open script editor pane.
#[derive(Debug, Clone)]
pub struct EditorState {
    pub mode: EditorMode,
    pub original_name: Option<String>,
    pub name: String,
    pub node: EditNode,
    pub error: Option<String>,
}

/// State for the folder editor pane.
#[derive(Debug, Clone)]
pub struct FolderState {
    pub mode: EditorMode,
    pub original_path: Option<String>,
    pub path: String,
    pub enabled: bool,
    pub error: Option<String>,
}

/// View vs. create, for the module pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleMode {
    View,
    Create,
}

/// State for the module pane (a local, non-shareable helper file).
#[derive(Debug, Clone)]
pub struct ModuleState {
    pub mode: ModuleMode,
    pub subpath: String,
    pub path: Option<PathBuf>,
    pub name: String,
    pub error: Option<String>,
}

/// Exactly one content pane shows at a time.
#[derive(Default, Debug, Clone)]
pub enum Pane {
    #[default]
    Dashboard,
    Error(Arc<Vec<String>>),
    Editor(EditorState),
    Folder(FolderState),
    Module(ModuleState),
    /// The author view of a package you own (source + dependents + versions +
    /// sharing). Data lives in `self.local_package` / share-state fields.
    OwnedPackage,
    /// The create-a-package form.
    NewPackage {
        name: String,
        error: Option<String>,
    },
    /// The consumer view of an installed package (deps + README + actions).
    InstalledPackage,
    /// The read-only detail of a script-created automation (pattern + body). Data is read live
    /// from `self.live` keyed by these fields, so the pane just carries the lookup key.
    CreatorAutomation {
        creator_id: String,
        kind: AutomationKind,
        name: String,
    },
    Discover,
    Shared,
    /// The live session-store inspector (`docs/interop.md` §10): the store tree
    /// per producer plus the interop catalogue (declared/observed handles with recent
    /// samples and inferred shapes). Data streams in via [`Message::CatalogueEvent`] while
    /// this pane is open.
    StoreInspector,
}

/// Which tree node is currently selected (drives highlighting + breadcrumb).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Selection {
    None,
    Script(ScriptKey),
    Folder(String),
    Module(String),
    OwnedPackage(String),
    InstalledPackage(String),
    /// A dependency *reference* row nested under `parent` (an installed/local package). Distinct
    /// from [`Selection::InstalledPackage`] so that selecting the reference highlights only the
    /// clicked row — not the same package's own top-level row, when it has one.
    Dependency {
        parent: String,
        spec: String,
    },
    /// A script-created (package/module) automation leaf, keyed by its creator tree node
    /// (`module:<subpath>` / `package:<spec>`), kind, and name. Drives the read-only detail pane.
    CreatorAutomation {
        creator_id: String,
        kind: AutomationKind,
        name: String,
    },
    Discover,
    Shared,
    Dashboard,
    StoreInspector,
}

#[derive(Debug, Clone)]
pub enum Message {
    // ---- loading -----------------------------------------------------------
    ScriptsLoaded(BTreeMap<String, Script>, Arc<Vec<String>>),
    LoadFolders,
    LoadModules,
    LoadLocalPackages,
    LoadInstalledPackages,

    // ---- navigation / selection -------------------------------------------
    ShowDashboard,
    SelectScript(ScriptKey),
    SelectFolder(String),
    SelectModule(String),
    SelectOwnedPackage(String),
    SelectInstalledPackage(String),
    /// Open an installed package via a nested dependency-reference row (keeps the clicked row,
    /// not the package's top-level row, as the highlighted selection).
    SelectDependency {
        parent: String,
        spec: String,
    },
    /// Open the read-only detail pane for a script-created automation.
    SelectCreatorAutomation {
        creator_id: String,
        kind: AutomationKind,
        name: String,
    },
    ToggleFolderExpanded(String),

    // ---- sidebar controls --------------------------------------------------
    ToggleNewMenu,
    SearchChanged(String),
    ClearSearch,
    SelectChip(Chip),

    // ---- create ------------------------------------------------------------
    NewAlias,
    NewTrigger,
    NewHotkey,
    NewFolder,
    NewModule,
    NewPackage,

    // ---- editor fields -----------------------------------------------------
    SetName(String),
    SetAliasPattern(String),
    /// Move the open script to a folder (`None` = top level). Also dispatched by
    /// the palette's "Move to…" group for the selected script.
    SetScriptFolder(Option<String>),
    SetBehavior(ScriptLang),
    AdjustPriority(i32),
    ToggleFallthrough,
    ScriptEditorAction(text_editor::Action),
    SetTestInput(String),
    ToggleEnabled,
    MarkHotkeyState(Vec<MaybePhysicalKey>),
    // trigger patterns
    AddPattern,
    RemovePattern(usize),
    SetPatternKind(usize, PatternKind),
    SetPatternText(usize, String),

    // ---- save bar ----------------------------------------------------------
    Save,
    Discard,
    Delete,
    ConfirmDiscardNav,
    CancelDiscardNav,

    // ---- folder ------------------------------------------------------------
    SetFolderPath(String),
    SaveFolder,
    RequestDeleteFolder,
    CancelDeleteFolder,
    ConfirmDeleteFolder(bool),

    // ---- module ------------------------------------------------------------
    SaveModule,
    SetNewModuleName(String),
    CreateModule,

    // ---- owned (local) package --------------------------------------------
    SelectOwnedFile(String),
    SaveOwnedFile,
    /// A field-level edit to the open package's manifest draft (the rich manifest editor for
    /// the package's `smudgy.package.json`).
    EditManifest(ManifestEdit),
    SelectManifestTab(ManifestTab),
    ManifestBeginEdit,
    SaveManifest,
    RevertManifest,
    PublishOwned,
    PublishFinished(Result<PublishSummary, String>),
    RequestDeleteOwned,
    CancelDeleteOwned,
    DeleteOwned,
    SetNewPackageName(String),
    CreatePackage,
    // owned sharing / versions
    SetVisibility(bool),
    VisibilityUpdated(Result<bool, CloudError>),
    YankVersion {
        version: String,
        yanked: bool,
    },
    DeleteVersion(String),
    VersionsUpdated(Result<Vec<VersionListItem>, CloudError>),
    ShareWithFriend(Uuid),
    GrantsUpdated(Result<Vec<PackageGrantView>, CloudError>),
    #[allow(clippy::type_complexity)]
    OwnedShareLoaded(
        Result<
            (
                Uuid,
                bool,
                Vec<FriendView>,
                Vec<PackageGrantView>,
                Vec<VersionListItem>,
            ),
            CloudError,
        >,
    ),

    // ---- installed package -------------------------------------------------
    /// The [`DetailSeq`] is the manage-pane detail generation captured when the load started; a
    /// stale result (the open package changed, navigation, uninstall, or a re-resolve) is discarded.
    InstalledDetailLoaded(DetailSeq, Result<packages::InstalledDetail, CloudError>),
    InstalledResolvedForGraph(
        String,
        Result<(ResolvedPackageWire, PackagePermissions), CloudError>,
    ),
    SetInstalledUpdateMode(UpdateMode),
    TogglePackageEnabled(String),
    /// Make `target_spec` the active member of a same-name group (enable it, disabling siblings).
    SetActiveMember {
        target_spec: String,
        siblings: Vec<String>,
    },
    /// Enable/disable a lone (non-colliding) local package from the tree.
    ToggleLocalEnabled(String),
    SelectInstalledFile(String),
    /// Switch the installed-package "README & source" area between its README and Source tabs.
    SelectInstalledFileTab(InstalledFileTab),
    /// A source-browser module body finished fetching for the open installed package, keyed by its
    /// `content_hash`. Content-addressed, so a late result just fills the cache and is matched to
    /// the selected file by hash — no staleness token needed.
    InstalledSourceLoaded {
        hash: String,
        result: Result<FilePreview, CloudError>,
    },
    RequestUninstall,
    /// The apt-style removal plan finished for the requested uninstall: `breaks` are the installed
    /// packages that `require` the open one (removed with it, forced); `orphans` are the
    /// auto-installed required roots nothing else would need once it's gone (offered).
    UninstallPlanComputed {
        breaks: Vec<String>,
        orphans: Vec<String>,
    },
    /// "Keep them": keep the offered orphans (clears only the orphan set; forced breaks still go).
    UninstallKeepOrphans,
    CancelUninstall,
    ConfirmUninstall,
    ForkPackage,
    ForkFinished(Result<(String, ForkActivation), String>),
    /// An async cloud check of account-owned installs finished (`delete_owned`'s post-delete
    /// check, or the installed-list sweep): stale entries were pruned, a parked entry was
    /// restored, or nothing changed.
    StaleAccountInstallsChecked(StaleInstallCheck),
    RevealPackageFolder,
    StartRenameOwned,
    RenameOwnedChanged(String),
    CommitRenameOwned,
    CancelRenameOwned,
    // trust toggle
    RequestTrust,
    CancelTrust,
    SetTrusted(bool),
    // owned (local) package: jump into the manifest's Capabilities tab; develop-unsandboxed toggle
    EditOwnedCapabilities,
    SetLocalUnsandboxed(bool),
    // update re-prompt
    GrantUpdate,
    DismissUpdate,
    // rating (a cloud package the user has installed): set the caller's 1–5 star rating, and the
    // fresh `PackageDetail` (rating average/count) the server returns for it.
    RateInstalledPackage(i16),
    InstalledRatingUpdated(Result<PackageDetail, CloudError>),

    // ---- discover ----------------------------------------------------------
    OpenDiscover,
    /// Loads the dashboard "Discover" teaser (a default-scope empty-query search).
    LoadFeaturedDiscover,
    FeaturedDiscoverLoaded(Result<Vec<PackageSearchResult>, CloudError>),
    DiscoverQueryChanged(String),
    DiscoverSearch,
    DiscoverScopeChanged(DiscoverScope),
    DiscoverResultsLoaded(Result<Vec<PackageSearchResult>, CloudError>),
    DiscoverSelect {
        package_id: Uuid,
        owner: String,
    },
    /// Install a search result directly (the result-card "Install" / dashboard teaser): routes to
    /// the Discover pane (so the consent window shows) and begins the install for `owner/name`.
    DiscoverInstallResult {
        owner: String,
        name: String,
    },
    DiscoverDetailLoaded(Result<PackageDetail, CloudError>),
    DiscoverCommentsLoaded(Result<Vec<CommentView>, CloudError>),
    DiscoverBack,
    RatePackage(i16),
    RatingUpdated(Result<PackageDetail, CloudError>),
    CommentInputChanged(String),
    AddComment,
    CommentAdded(Result<CommentView, CloudError>),
    OpenReadmeLink(markdown::Uri),
    DiscoverInstall,
    /// The [`InstallSeq`] is the install generation captured at `begin_install`; a stale result
    /// (the user navigated away / clicked Back / started another install) is discarded.
    InstallResolved(InstallSeq, Result<InstallResolution, CloudError>),
    // install-time consent confirmation; `enable` = "Install & enable" vs "Install, don't
    // enable" (both record the same consent — they differ only in turning the package on now).
    ConsentGrant {
        enable: bool,
    },
    ConsentCancel,
    // One edit to a parameter's value, routed by `ParamTarget` to the install-time prompt or the
    // in-pane config editor. The `String` is the parameter key; `ParamValueEdit` is the addressed
    // change (a scalar edit, or a list/table row op). Shared by both value-entry surfaces.
    ParamValueEdit(
        param_values::ParamTarget,
        String,
        param_values::ParamValueEdit,
    ),
    ParamPromptSubmit,
    ParamPromptCancel,
    // in-pane param-value editor (installed & owned package panes): save all, or clear a stored
    // secret. Distinct from the install-time `ParamPrompt*` gate above.
    ParamConfigSave,
    ParamConfigClearSecret(String),

    // ---- private & shared --------------------------------------------------
    OpenShared,
    SharedLoaded(Result<Vec<PackageDetail>, CloudError>),
    /// The caller's own cloud packages (`GET /packages/mine`), shown alongside the
    /// shared-with-me list in the "Private & Shared" pane — including private ones with
    /// no local copy on this machine, which appear in no other surface.
    MyCloudLoaded(Result<Vec<PackageDetail>, CloudError>),
    InstallShared {
        owner: String,
        name: String,
    },

    // ---- top action bar ----------------------------------------------------
    Reload,
    Inspect,

    // ---- command palette ---------------------------------------------------
    OpenPalette,
    ClosePalette,
    PaletteInput(String),
    PaletteMove(i32),
    PaletteRun,
    PaletteRunItem(usize),

    // ---- toast -------------------------------------------------------------
    DismissToast(u64),

    // ---- live (script-created) automations --------------------------------
    AutomationEvent(AutomationEvent),
    ToggleCreator(String),
    ToggleCreatorShowAll(String),

    // ---- session-store inspector -------------------------------------------
    OpenStoreInspector,
    CatalogueEvent(CatalogueEvent),
    /// Flip one store-tree node between expanded and collapsed (keyed by producer + path).
    ToggleStoreNode(String),
}

/// The Automations window. One per (server, session) the user opens it for.
pub struct AutomationsWindow {
    pub(super) server_name: String,
    pub(super) cloud: CloudHandles,
    pub(super) session_id: SessionId,
    pub(super) mud_host: Option<String>,
    /// Whether advanced scripting features are unlocked (settings `advanced_scripting_features`):
    /// the "Remove sandbox" package action and the script inspector. Read at construction and
    /// refreshed on Reload — toggling it in Settings takes effect on the next reload/reopen.
    pub(super) advanced_features: bool,

    // ---- script tree -------------------------------------------------------
    pub(super) scripts: BTreeMap<String, Script>,
    pub(super) packages: PackageTree,
    pub(super) modules: Vec<ModuleFile>,
    pub(super) local_packages: Vec<String>,
    pub(super) installed_packages: Vec<LockedPackage>,

    // ---- live (script-created) automations --------------------------------
    /// Streamed from this session's automation broadcast; rendered nested under each
    /// creating module/package node in the tree.
    pub(super) live: LiveAutomations,
    /// Creators whose nested automations are expanded (collapsed by default — a bulk package
    /// can create tens of thousands).
    pub(super) expanded_creators: HashSet<String>,
    /// Creators showing all their automations rather than the first `CREATOR_SHOW_LIMIT`.
    pub(super) show_all_creators: HashSet<String>,

    // ---- session-store inspector -------------------------------------------
    /// The latest catalogue snapshot, streamed from this session's catalogue broadcast while
    /// the store pane is open (the subscription exists only then, so a closed pane costs the
    /// runtime nothing). `None` before the first snapshot.
    pub(super) catalogue: Option<Arc<CatalogueSnapshot>>,
    /// Store-tree nodes whose expansion the user flipped (keyed producer + NUL + path). The
    /// default is expanded near the root and collapsed deeper; membership here inverts it.
    pub(super) store_toggled: HashSet<String>,

    pub(super) selection: Selection,
    pub(super) collapsed_folders: HashSet<String>,
    pub(super) pane: Pane,

    // ---- sidebar -----------------------------------------------------------
    pub(super) search: String,
    pub(super) chip: Chip,
    pub(super) new_menu_open: bool,

    // ---- shared editor buffers --------------------------------------------
    pub(super) editor_content: text_editor::Content,
    pub(super) hotkey_state: Vec<MaybePhysicalKey>,
    pub(super) test_input: String,
    pub(super) dirty: bool,
    pub(super) pending_nav: Option<Box<Message>>,
    pub(super) confirm_folder_delete: bool,

    // ---- package dependency graph ------------------------------------------
    pub(super) graph: PackageGraph,
    /// Installed-package specifiers whose newest resolvable version's closure permission union
    /// exceeds the consented grant — the engine holds them at an older fitting version (or won't
    /// load them), so the tree flags them orange and the manage pane shows "update blocked"
    /// (`PACKAGE-ISOLATES-CONSENT-TRUST.md`). Populated by the background graph resolve.
    pub(super) blocked_updates: HashSet<String>,

    // ---- owned (local) package state --------------------------------------
    pub(super) local_package: Option<Box<LocalPackage>>,
    pub(super) local_readme: Option<markdown::Content>,
    pub(super) owned_selected_file: Option<String>,
    /// Inline rename buffer for the open local package (the folder name is its identity). `Some`
    /// while the rename field is showing; `None` otherwise.
    pub(super) rename_buffer: Option<String>,
    /// The editable manifest form for the open owned package (the rich editor for its
    /// `smudgy.package.json`). Seeded on open + after a Save; `None` off-pane.
    pub(super) manifest_draft: Option<ManifestDraft>,
    /// Whether the manifest draft has unsaved edits (independent of the script-editor `dirty`
    /// flag, which guards a different pane).
    pub(super) manifest_dirty: bool,
    /// Whether the manifest section is in the structured editor (vs the default read-only summary).
    pub(super) manifest_editing: bool,
    /// Which manifest-editor tab is showing (view-only; reset to `Settings` when a package opens).
    pub(super) manifest_tab: ManifestTab,
    pub(super) authoring_busy: bool,
    pub(super) authoring_feedback: Option<String>,
    pub(super) confirm_delete_local: bool,
    pub(super) share_package_id: Option<Uuid>,
    pub(super) share_is_public: bool,
    pub(super) share_friends: Vec<FriendView>,
    pub(super) share_grants: Vec<PackageGrantView>,
    pub(super) share_versions: Vec<VersionListItem>,
    pub(super) share_busy: bool,
    pub(super) share_feedback: Option<String>,

    // ---- installed package state ------------------------------------------
    pub(super) installed_open: Option<Box<LockedPackage>>,
    pub(super) installed_detail: Option<Box<ResolvedPackageWire>>,
    /// The cloud package metadata (rating, install count) for the open installed package, fetched
    /// best-effort alongside the detail resolve. `None` for a local/owned package, while loading, or
    /// when the fetch failed — gating the rating UI on `Some` keeps it to real cloud packages.
    /// Replaced by the fresh `PackageDetail` the server returns when the user rates.
    pub(super) installed_rating: Option<Box<PackageDetail>>,
    pub(super) installed_versions: Vec<String>,
    pub(super) installed_selected_file: Option<String>,
    /// Which tab of the installed-package "README & source" area is showing (README vs Source).
    pub(super) installed_file_tab: InstalledFileTab,
    /// On-demand source for the installed-package source browser, keyed by module `content_hash`
    /// (content-addressed, so identical blobs share an entry and a late fetch is self-validating).
    /// Populated lazily when a file is selected; cleared when a different installed package opens.
    pub(super) installed_source: HashMap<String, FilePreview>,
    pub(super) manage_busy: bool,
    pub(super) manage_feedback: Option<String>,
    pub(super) confirm_uninstall: bool,
    /// The auto-installed required roots that would become **orphans** if the open package were
    /// uninstalled — apt-style, surfaced in the uninstall confirmation so the user can remove them
    /// too (`script/REQUIRED-PACKAGES.md`). Computed asynchronously when uninstall is requested
    /// (resolving the installed packages' `requires`); empty when nothing would be orphaned.
    pub(super) uninstall_orphans: Vec<String>,
    /// The installed packages that **`require`** the open package and would break if it were removed
    /// — they are removed alongside it (forced, never kept). Computed with `uninstall_orphans` from
    /// `SharedPackageLock::plan_removal` when uninstall is requested (`script/REQUIRED-PACKAGES.md`).
    pub(super) uninstall_breaks: Vec<String>,
    /// Two-step confirm gate for the heavy Trust action.
    pub(super) confirm_trust: bool,
    /// A pending update re-prompt for the open installed package: the new version's added
    /// permission asks beyond the consented baseline. `None` when there's nothing new to grant.
    pub(super) update_delta: Option<UpdateDelta>,

    // ---- discover state ----------------------------------------------------
    pub(super) discover_query: String,
    pub(super) discover_scope: DiscoverScope,
    pub(super) discover_results: Vec<PackageSearchResult>,
    /// The dashboard "Discover" teaser: the top results of a default ([`DiscoverScope::Relevant`])
    /// empty-query search, loaded on window init. Kept separate from `discover_results` so it stays
    /// stable regardless of how the user later searches/filters inside the Discover pane.
    pub(super) featured_packages: Vec<PackageSearchResult>,
    pub(super) discover_owner: Option<String>,
    pub(super) discover_detail: Option<Box<PackageDetail>>,
    pub(super) discover_readme: Option<markdown::Content>,
    pub(super) discover_comments: Vec<CommentView>,
    pub(super) discover_comment_input: String,
    pub(super) discover_busy: bool,
    pub(super) discover_error: Option<String>,
    /// The always-shown install confirmation; shown before any lock entry is written.
    pub(super) consent_prompt: Option<ConsentPrompt>,
    /// Monotonic generation for the in-flight install resolve; bumped on `begin_install` and on any
    /// action that abandons a pending install, so a late async result that no longer matches is
    /// discarded instead of popping a stale consent window.
    pub(super) install_seq: InstallSeq,
    /// Monotonic generation for the in-flight manage-pane detail load; bumped when the open package
    /// changes (`clear_selection`), is re-resolved (update-mode change), or is uninstalled, so a late
    /// async result that no longer matches is discarded instead of repainting a superseded package.
    pub(super) detail_seq: DetailSeq,
    pub(super) param_prompt: Option<ParamPrompt>,
    /// The remaining install-time required-params prompts to show after the current one, in order:
    /// a required install configures the chosen package and each co-installed required root in turn,
    /// so this holds the not-yet-shown prompts (`script/REQUIRED-PACKAGES.md`). Empty when the
    /// current prompt (if any) is the last. Drained by `advance_param_prompt_queue`.
    pub(super) param_prompt_queue: Vec<ParamPrompt>,
    /// The inline param-value editor for the open package pane (installed or owned). Seeded when a
    /// package that declares params opens; `None` otherwise. Independent of `param_prompt`, which is
    /// the install-time required-params gate.
    pub(super) param_config: Option<ParamConfig>,

    // ---- private & shared --------------------------------------------------
    pub(super) shared_with_me: Option<Vec<PackageDetail>>,
    /// The caller's own cloud packages (`GET /packages/mine`), public and private. `None`
    /// until the "Private & Shared" pane loads them. Surfaces packages the owner has no
    /// other way to see — e.g. a private package published from another machine.
    pub(super) my_cloud_packages: Option<Vec<PackageDetail>>,

    // ---- command palette ---------------------------------------------------
    pub(super) palette_open: bool,
    pub(super) palette_query: String,
    pub(super) palette_cursor: usize,

    // ---- toast -------------------------------------------------------------
    pub(super) toast: Option<String>,
    pub(super) toast_gen: u64,
}

/// A subscription stream of this session's script-created automation updates: waits for the
/// session runtime to exist, subscribes to its automation broadcast, and yields events
/// (skipping lag, ending when the session shuts down).
fn automation_stream(session_id: SessionId) -> impl iced::futures::Stream<Item = AutomationEvent> {
    use tokio::sync::broadcast::error::RecvError;

    enum State {
        Connecting,
        Streaming(tokio::sync::broadcast::Receiver<AutomationEvent>),
    }

    iced::futures::stream::unfold(State::Connecting, move |state| async move {
        let mut rx = match state {
            State::Streaming(rx) => rx,
            State::Connecting => loop {
                if let Some(runtime) = smudgy_core::session::registry::get_runtime(session_id) {
                    break runtime.subscribe_automations();
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            },
        };
        loop {
            match rx.recv().await {
                Ok(event) => return Some((event, State::Streaming(rx))),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    })
}

/// A subscription stream of this session's runtime-catalogue snapshots (the store-inspector
/// pane's data): waits for the session runtime, subscribes to its catalogue broadcast, and
/// yields snapshots. On lag it just continues — every message is a full snapshot, so the
/// latest one is all that matters.
fn catalogue_stream(session_id: SessionId) -> impl iced::futures::Stream<Item = CatalogueEvent> {
    use tokio::sync::broadcast::error::RecvError;

    enum State {
        Connecting,
        Streaming(tokio::sync::broadcast::Receiver<CatalogueEvent>),
    }

    iced::futures::stream::unfold(State::Connecting, move |state| async move {
        let mut rx = match state {
            State::Streaming(rx) => rx,
            State::Connecting => loop {
                if let Some(runtime) = smudgy_core::session::registry::get_runtime(session_id) {
                    break runtime.subscribe_catalogue();
                }
                tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            },
        };
        loop {
            match rx.recv().await {
                Ok(event) => return Some((event, State::Streaming(rx))),
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return None,
            }
        }
    })
}

impl AutomationsWindow {
    pub fn new(server_name: String, cloud: CloudHandles, session_id: SessionId) -> Self {
        let mud_host = server::load_server(&server_name)
            .ok()
            .map(|server| server.config.host);
        let advanced_features =
            smudgy_core::models::settings::load_settings().advanced_scripting_features;
        Self {
            server_name,
            cloud,
            session_id,
            mud_host,
            advanced_features,
            scripts: BTreeMap::new(),
            packages: PackageTree::new(),
            modules: Vec::new(),
            local_packages: Vec::new(),
            installed_packages: Vec::new(),
            live: LiveAutomations::default(),
            expanded_creators: HashSet::new(),
            show_all_creators: HashSet::new(),
            catalogue: None,
            store_toggled: HashSet::new(),
            selection: Selection::Dashboard,
            collapsed_folders: HashSet::new(),
            pane: Pane::Dashboard,
            search: String::new(),
            chip: Chip::All,
            new_menu_open: false,
            editor_content: text_editor::Content::new(),
            hotkey_state: Vec::new(),
            test_input: String::new(),
            dirty: false,
            pending_nav: None,
            confirm_folder_delete: false,
            graph: PackageGraph::default(),
            blocked_updates: HashSet::new(),
            local_package: None,
            local_readme: None,
            owned_selected_file: None,
            rename_buffer: None,
            manifest_draft: None,
            manifest_dirty: false,
            manifest_editing: false,
            manifest_tab: ManifestTab::default(),
            authoring_busy: false,
            authoring_feedback: None,
            confirm_delete_local: false,
            share_package_id: None,
            share_is_public: false,
            share_friends: Vec::new(),
            share_grants: Vec::new(),
            share_versions: Vec::new(),
            share_busy: false,
            share_feedback: None,
            installed_open: None,
            installed_detail: None,
            installed_rating: None,
            installed_versions: Vec::new(),
            installed_selected_file: None,
            installed_file_tab: InstalledFileTab::default(),
            installed_source: HashMap::new(),
            manage_busy: false,
            manage_feedback: None,
            confirm_uninstall: false,
            uninstall_orphans: Vec::new(),
            uninstall_breaks: Vec::new(),
            confirm_trust: false,
            update_delta: None,
            discover_query: String::new(),
            discover_scope: DiscoverScope::default(),
            discover_results: Vec::new(),
            featured_packages: Vec::new(),
            discover_owner: None,
            discover_detail: None,
            discover_readme: None,
            discover_comments: Vec::new(),
            discover_comment_input: String::new(),
            discover_busy: false,
            discover_error: None,
            consent_prompt: None,
            install_seq: InstallSeq::default(),
            detail_seq: DetailSeq::default(),
            param_prompt: None,
            param_prompt_queue: Vec::new(),
            param_config: None,
            shared_with_me: None,
            my_cloud_packages: None,
            palette_open: false,
            palette_query: String::new(),
            palette_cursor: 0,
            toast: None,
            toast_gen: 0,
        }
    }

    pub fn init(&self) -> Task<Message> {
        Task::batch([
            Task::done(self.load_scripts_message()),
            Task::done(Message::LoadFolders),
            Task::done(Message::LoadModules),
            Task::done(Message::LoadLocalPackages),
            Task::done(Message::LoadInstalledPackages),
            Task::done(Message::LoadFeaturedDiscover),
        ])
    }

    pub fn server_name(&self) -> &str {
        &self.server_name
    }

    /// Ctrl/⌘+P opens the palette; arrows/enter/escape drive it while open.
    /// Navigation keys only act on events no focused widget captured, so they
    /// don't fight text inputs elsewhere.
    pub fn subscription(&self) -> Subscription<Message> {
        let keyboard = iced::event::listen_with(|event, status, _window| {
            let IcedEvent::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) = event
            else {
                return None;
            };
            match (key.as_ref(), status) {
                (keyboard::Key::Character("p"), _) if modifiers.command() => {
                    Some(Message::OpenPalette)
                }
                (keyboard::Key::Named(Named::Escape), Status::Ignored) => {
                    Some(Message::ClosePalette)
                }
                (keyboard::Key::Named(Named::ArrowDown), Status::Ignored) => {
                    Some(Message::PaletteMove(1))
                }
                (keyboard::Key::Named(Named::ArrowUp), Status::Ignored) => {
                    Some(Message::PaletteMove(-1))
                }
                (keyboard::Key::Named(Named::Enter), Status::Ignored) => Some(Message::PaletteRun),
                _ => None,
            }
        });
        // Stream this session's script-created automation updates, keyed by session id so
        // iced keeps a single broadcast subscription (one runtime receiver) across renders.
        let automations =
            Subscription::run_with(self.session_id, |session_id| automation_stream(*session_id))
                .map(Message::AutomationEvent);
        let mut subscriptions = vec![keyboard, automations];
        // The catalogue broadcast is subscribed only while the store pane is showing: the
        // runtime builds snapshots only while receivers exist, so a closed pane costs it
        // nothing, and re-opening gets a fresh snapshot (the new-subscriber resync).
        if matches!(self.pane, Pane::StoreInspector) {
            subscriptions.push(
                Subscription::run_with(self.session_id, |session_id| catalogue_stream(*session_id))
                    .map(Message::CatalogueEvent),
            );
        }
        Subscription::batch(subscriptions)
    }

    /// Pops a toast and schedules its auto-dismiss (~2.2s).
    pub(super) fn show_toast(&mut self, message: impl Into<String>) -> Task<Message> {
        self.toast_gen += 1;
        let toast_id = self.toast_gen;
        self.toast = Some(message.into());
        Task::perform(
            async move { tokio::time::sleep(Duration::from_millis(2200)).await },
            move |()| Message::DismissToast(toast_id),
        )
    }

    pub fn update(&mut self, message: Message) -> Update<Message, Event> {
        // Unsaved-changes guard: defer navigation away from a dirty editor or an edited but
        // unsaved manifest draft (the rich manifest editor tracks its own dirty flag).
        if (self.dirty || self.manifest_dirty) && Self::is_guarded_navigation(&message) {
            self.pending_nav = Some(Box::new(message));
            return Update::none();
        }
        if Self::is_edit_message(&message) {
            self.dirty = true;
        }
        match message {
            // -------- loading ----------------------------------------------
            Message::ScriptsLoaded(scripts, errors) => {
                self.scripts = scripts;
                self.merge_folders();
                if errors.is_empty() {
                    Update::none()
                } else {
                    self.pane = Pane::Error(errors);
                    Update::none()
                }
            }
            Message::LoadFolders => {
                self.packages =
                    core_packages::load_packages(&self.server_name).unwrap_or_else(|e| {
                        log::warn!("Failed to load folders for {}: {e}", self.server_name);
                        PackageTree::new()
                    });
                self.merge_folders();
                Update::none()
            }
            Message::LoadModules => {
                self.modules = smudgy_core::models::modules::list_modules(&self.server_name)
                    .unwrap_or_else(|e| {
                        log::warn!("Failed to list modules for {}: {e}", self.server_name);
                        Vec::new()
                    });
                Update::none()
            }
            Message::LoadLocalPackages => {
                self.local_packages =
                    smudgy_core::models::local_packages::list_local_packages(&self.server_name)
                        .unwrap_or_else(|e| {
                            log::warn!("Failed to list local packages: {e}");
                            Vec::new()
                        });
                self.rebuild_graph();
                Update::none()
            }
            Message::LoadInstalledPackages => {
                // Self-heal before reading: a reserved-`local`-owner install whose folder is gone
                // can never resolve again and would render as a phantom installed package (and
                // fail to load every session) — lockfiles written by app versions whose package
                // delete left install entries behind carry such strays. One with a folder is
                // migrated to the account's nickname form once a nickname exists.
                let nickname = self.cloud.snapshot.get().nickname_text();
                match smudgy_core::models::shared_packages::reconcile_local_installs(
                    &self.server_name,
                    nickname.as_deref(),
                ) {
                    Ok(changed) if !changed.is_empty() => {
                        log::info!("Reconciled local package installs: {}", changed.join(", "));
                    }
                    Ok(_) => {}
                    Err(e) => log::warn!("Failed to reconcile local installs: {e}"),
                }
                self.installed_packages =
                    smudgy_core::models::shared_packages::load_lock(&self.server_name)
                        .map(|lock| lock.packages)
                        .unwrap_or_else(|e| {
                            log::warn!("Failed to load lockfile: {e}");
                            Vec::new()
                        });
                self.rebuild_graph();
                let mut task = self.resolve_graph_deps();
                if let Some(sweep) = self.sweep_stale_account_installs() {
                    task = Task::batch([task, sweep]);
                }
                Update::with_task(task)
            }
            // -------- live (script-created) automations --------------------
            Message::AutomationEvent(event) => {
                match event {
                    AutomationEvent::Reset(summaries) => self.live.reset(&summaries),
                    AutomationEvent::Changed(deltas) => self.live.apply(&deltas),
                }
                Update::none()
            }
            Message::ToggleCreator(id) => {
                if !self.expanded_creators.remove(&id) {
                    self.expanded_creators.insert(id);
                }
                Update::none()
            }
            Message::ToggleCreatorShowAll(id) => {
                if !self.show_all_creators.remove(&id) {
                    self.show_all_creators.insert(id);
                }
                Update::none()
            }

            // -------- session-store inspector --------------------------------
            Message::OpenStoreInspector => {
                self.clear_selection();
                self.selection = Selection::StoreInspector;
                self.pane = Pane::StoreInspector;
                Update::none()
            }
            Message::CatalogueEvent(CatalogueEvent::Snapshot(snapshot)) => {
                self.catalogue = Some(snapshot);
                Update::none()
            }
            Message::ToggleStoreNode(key) => {
                if !self.store_toggled.remove(&key) {
                    self.store_toggled.insert(key);
                }
                Update::none()
            }

            // -------- navigation -------------------------------------------
            Message::ShowDashboard => {
                self.clear_selection();
                self.selection = Selection::Dashboard;
                self.pane = Pane::Dashboard;
                Update::none()
            }
            Message::SelectScript(key) => self.open_script(key),
            Message::SelectFolder(path) => self.open_folder(path),
            Message::SelectModule(subpath) => self.open_module(subpath),
            Message::SelectOwnedPackage(name) => self.open_owned_package(name),
            Message::SelectInstalledPackage(spec) => self.open_installed_package(spec),
            Message::SelectDependency { parent, spec } => self.open_dependency(parent, spec),
            Message::SelectCreatorAutomation {
                creator_id,
                kind,
                name,
            } => self.open_creator_automation(creator_id, kind, name),
            Message::ToggleFolderExpanded(path) => {
                if !self.collapsed_folders.remove(&path) {
                    self.collapsed_folders.insert(path);
                }
                Update::none()
            }

            // -------- sidebar ----------------------------------------------
            Message::ToggleNewMenu => {
                self.new_menu_open = !self.new_menu_open;
                Update::none()
            }
            Message::SearchChanged(q) => {
                self.search = q;
                Update::none()
            }
            Message::ClearSearch => {
                self.search.clear();
                Update::none()
            }
            Message::SelectChip(chip) => {
                self.chip = chip;
                Update::none()
            }

            // -------- create -----------------------------------------------
            Message::NewAlias => self.new_alias(),
            Message::NewTrigger => self.new_trigger(),
            Message::NewHotkey => self.new_hotkey(),
            Message::NewFolder => self.new_folder(),
            Message::NewModule => self.new_module(),
            Message::NewPackage => self.new_package(),

            // -------- editor fields ----------------------------------------
            Message::SetName(name) => {
                if let Pane::Editor(state) = &mut self.pane {
                    state.name = name;
                }
                Update::none()
            }
            Message::SetAliasPattern(pattern) => {
                if let Pane::Editor(state) = &mut self.pane
                    && let EditNode::Alias(alias) = &mut state.node
                {
                    alias.pattern = pattern;
                }
                Update::none()
            }
            Message::SetScriptFolder(folder) => self.set_script_folder(folder),
            Message::SetBehavior(language) => {
                if let Pane::Editor(state) = &mut self.pane {
                    match &mut state.node {
                        EditNode::Alias(a) => a.language = language,
                        EditNode::Hotkey(h) => h.language = language,
                        EditNode::Trigger { language: l, .. } => *l = language,
                    }
                }
                Update::none()
            }
            Message::AdjustPriority(delta) => {
                if let Pane::Editor(state) = &mut self.pane {
                    match &mut state.node {
                        EditNode::Alias(alias) => {
                            alias.priority = alias.priority.saturating_add(delta);
                        }
                        EditNode::Trigger { priority, .. } => {
                            *priority = priority.saturating_add(delta);
                        }
                        EditNode::Hotkey(_) => {}
                    }
                }
                Update::none()
            }
            Message::ToggleFallthrough => {
                if let Pane::Editor(state) = &mut self.pane {
                    match &mut state.node {
                        EditNode::Alias(alias) => alias.fallthrough = !alias.fallthrough,
                        EditNode::Trigger { fallthrough, .. } => {
                            *fallthrough = !*fallthrough;
                        }
                        EditNode::Hotkey(_) => {}
                    }
                }
                Update::none()
            }
            Message::ScriptEditorAction(action) => {
                self.editor_content.perform(action);
                Update::none()
            }
            Message::SetTestInput(value) => {
                self.test_input = value;
                Update::none()
            }
            Message::ToggleEnabled => self.toggle_open_enabled(),
            Message::MarkHotkeyState(keys) => {
                self.hotkey_state = keys;
                Update::none()
            }
            Message::AddPattern => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                {
                    rows.push((PatternKind::Match, String::new()));
                }
                Update::none()
            }
            Message::RemovePattern(i) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && i < rows.len()
                {
                    rows.remove(i);
                }
                Update::none()
            }
            Message::SetPatternKind(i, kind) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    row.0 = kind;
                }
                Update::none()
            }
            Message::SetPatternText(i, text) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    row.1 = text;
                }
                Update::none()
            }

            // -------- save bar ---------------------------------------------
            Message::Save => self.save_open(),
            Message::Discard => {
                self.dirty = false;
                self.pending_nav = None;
                self.clear_selection();
                self.selection = Selection::Dashboard;
                self.pane = Pane::Dashboard;
                Update::none()
            }
            Message::Delete => self.delete_open(),
            Message::ConfirmDiscardNav => {
                // Discard both kinds of pending edits — the script-editor buffer and the manifest
                // draft (re-seeded from disk when the owned package is next opened).
                self.dirty = false;
                self.manifest_dirty = false;
                match self.pending_nav.take() {
                    Some(msg) => Update::with_task(Task::done(*msg)),
                    None => Update::none(),
                }
            }
            Message::CancelDiscardNav => {
                self.pending_nav = None;
                Update::none()
            }

            // -------- folder -----------------------------------------------
            Message::SetFolderPath(value) => {
                if let Pane::Folder(state) = &mut self.pane {
                    state.path = value;
                }
                Update::none()
            }
            Message::SaveFolder => self.save_folder(),
            Message::RequestDeleteFolder => {
                self.confirm_folder_delete = true;
                Update::none()
            }
            Message::CancelDeleteFolder => {
                self.confirm_folder_delete = false;
                Update::none()
            }
            Message::ConfirmDeleteFolder(delete_scripts) => self.delete_folder(delete_scripts),

            // -------- module -----------------------------------------------
            Message::SaveModule => self.save_module(),
            Message::SetNewModuleName(value) => {
                if let Pane::Module(state) = &mut self.pane {
                    state.name = value;
                }
                Update::none()
            }
            Message::CreateModule => self.create_module(),

            // -------- owned package ----------------------------------------
            Message::SelectOwnedFile(subpath) => self.select_owned_file(subpath),
            Message::SaveOwnedFile => self.save_owned_file(),
            Message::EditManifest(edit) => self.apply_manifest_edit(edit),
            Message::SelectManifestTab(tab) => {
                self.manifest_tab = tab;
                Update::none()
            }
            Message::ManifestBeginEdit => self.begin_manifest_edit(),
            Message::SaveManifest => self.save_manifest(),
            Message::RevertManifest => self.revert_manifest(),
            Message::PublishOwned => self.publish_owned(),
            Message::PublishFinished(result) => {
                self.authoring_busy = false;
                match result {
                    Ok(summary) => {
                        let mut feedback = format!("Published v{}", summary.version);
                        if summary.typings_generated > 0 {
                            feedback.push_str(&format!(
                                " \u{b7} {} typings",
                                summary.typings_generated
                            ));
                        }
                        // Surface tsc warnings to the author — typings are best-effort, so a
                        // warning here never means the publish failed.
                        if !summary.typings_warnings.is_empty() {
                            feedback.push_str(&format!(
                                " \u{b7} \u{26a0} typings: {}",
                                summary.typings_warnings.join("; ")
                            ));
                        }
                        // Show exactly what each dependency froze to — a publish pins the whole tree,
                        // so a stale range silently locking an old version is otherwise invisible.
                        if !summary.locked_dependencies.is_empty() {
                            let locked: Vec<String> = summary
                                .locked_dependencies
                                .iter()
                                .map(|(spec, ver)| {
                                    format!("{}@{ver}", spec.trim_start_matches("smudgy://"))
                                })
                                .collect();
                            feedback.push_str(&format!(" \u{b7} deps: {}", locked.join(", ")));
                        }
                        // A range that excludes a newer published version (the 0.0.x caret footgun):
                        // non-fatal, but the author almost certainly wanted the newer one.
                        if !summary.dependency_warnings.is_empty() {
                            feedback.push_str(&format!(
                                " \u{b7} \u{26a0} {}",
                                summary.dependency_warnings.join("; ")
                            ));
                        }
                        // Interop-declaration warnings (duplicate/aliased handle exports, a
                        // handle the previous version published that this one drops): a handle
                        // name is the identity consumers import, so these deserve eyes even
                        // though the publish succeeded.
                        if !summary.interop_warnings.is_empty() {
                            feedback.push_str(&format!(
                                " \u{b7} \u{26a0} interop: {}",
                                summary.interop_warnings.join("; ")
                            ));
                        }
                        self.authoring_feedback = Some(feedback);
                        Update::with_task(
                            self.show_toast(format!("Published v{}", summary.version)),
                        )
                    }
                    Err(e) => {
                        self.authoring_feedback = Some(format!("Publish failed: {e}"));
                        Update::none()
                    }
                }
            }
            Message::RequestDeleteOwned => {
                self.confirm_delete_local = true;
                Update::none()
            }
            Message::CancelDeleteOwned => {
                self.confirm_delete_local = false;
                Update::none()
            }
            Message::DeleteOwned => self.delete_owned(),
            Message::SetNewPackageName(value) => {
                if let Pane::NewPackage { name, .. } = &mut self.pane {
                    *name = value;
                }
                Update::none()
            }
            Message::CreatePackage => self.create_package(),
            Message::SetVisibility(public) => self.set_visibility(public),
            Message::VisibilityUpdated(result) => self.visibility_updated(result),
            Message::YankVersion { version, yanked } => self.yank_version(version, yanked),
            Message::DeleteVersion(version) => self.delete_version(version),
            Message::VersionsUpdated(result) => self.versions_updated(result),
            Message::ShareWithFriend(grantee) => self.share_with_friend(grantee),
            Message::GrantsUpdated(result) => self.grants_updated(result),
            Message::OwnedShareLoaded(result) => self.owned_share_loaded(result),

            // -------- installed package ------------------------------------
            Message::InstalledDetailLoaded(seq, result) => {
                self.installed_detail_loaded(seq, result)
            }
            Message::InstalledResolvedForGraph(spec, result) => {
                self.installed_resolved_for_graph(&spec, result)
            }
            Message::SetInstalledUpdateMode(mode) => self.set_installed_update_mode(mode),
            Message::TogglePackageEnabled(spec) => self.toggle_package_enabled(spec),
            Message::SetActiveMember {
                target_spec,
                siblings,
            } => self.set_active_member(target_spec, siblings),
            Message::ToggleLocalEnabled(name) => self.toggle_local_enabled(name),
            Message::SelectInstalledFile(subpath) => self.select_installed_file(subpath),
            Message::SelectInstalledFileTab(tab) => {
                self.installed_file_tab = tab;
                // Entering the Source tab with a file already selected: make sure its source is
                // loading/loaded (idempotent — no-ops when nothing is selected or it's cached).
                match tab {
                    InstalledFileTab::Source => self.ensure_selected_source(),
                    InstalledFileTab::Readme => Update::none(),
                }
            }
            Message::InstalledSourceLoaded { hash, result } => {
                self.installed_source_loaded(hash, result)
            }
            Message::RequestUninstall => self.request_uninstall(),
            Message::UninstallPlanComputed { breaks, orphans } => {
                // Only adopt the result if the user is still in the uninstall confirmation (it
                // wasn't cancelled while the resolve was in flight).
                if self.confirm_uninstall {
                    self.uninstall_breaks = breaks;
                    self.uninstall_orphans = orphans;
                }
                Update::none()
            }
            Message::UninstallKeepOrphans => {
                // Keep the offered orphans; the forced breaks still go.
                self.uninstall_orphans.clear();
                Update::none()
            }
            Message::CancelUninstall => {
                self.confirm_uninstall = false;
                self.uninstall_orphans.clear();
                self.uninstall_breaks.clear();
                Update::none()
            }
            Message::ConfirmUninstall => self.uninstall_installed(),
            Message::ForkPackage => self.fork_installed(),
            Message::ForkFinished(result) => self.fork_finished(result),
            Message::StaleAccountInstallsChecked(outcome) => {
                self.stale_account_installs_checked(outcome)
            }
            Message::RevealPackageFolder => self.reveal_package_folder(),
            Message::StartRenameOwned => self.start_rename_owned(),
            Message::RenameOwnedChanged(value) => {
                self.rename_buffer = Some(value);
                Update::none()
            }
            Message::CommitRenameOwned => self.commit_rename_owned(),
            Message::CancelRenameOwned => {
                self.rename_buffer = None;
                Update::none()
            }
            Message::RequestTrust => self.request_trust(),
            Message::CancelTrust => self.cancel_trust(),
            Message::SetTrusted(trusted) => self.set_trusted(trusted),
            Message::EditOwnedCapabilities => self.edit_owned_capabilities(),
            Message::SetLocalUnsandboxed(unsandboxed) => self.set_local_unsandboxed(unsandboxed),
            Message::GrantUpdate => self.grant_update(),
            Message::DismissUpdate => self.dismiss_update(),
            Message::RateInstalledPackage(stars) => self.rate_installed_package(stars),
            Message::InstalledRatingUpdated(result) => self.installed_rating_updated(result),

            // -------- discover ---------------------------------------------
            Message::OpenDiscover => self.open_discover(),
            Message::LoadFeaturedDiscover => self.load_featured_discover(),
            Message::FeaturedDiscoverLoaded(result) => {
                if let Ok(results) = result {
                    self.featured_packages = results;
                }
                Update::none()
            }
            Message::DiscoverQueryChanged(q) => {
                self.discover_query = q;
                Update::none()
            }
            Message::DiscoverSearch => self.discover_search(),
            Message::DiscoverScopeChanged(scope) => {
                // Scope is a radio; changing it re-runs the search immediately (no separate Search press).
                self.discover_scope = scope;
                self.discover_search()
            }
            Message::DiscoverResultsLoaded(result) => self.discover_results_loaded(result),
            Message::DiscoverSelect { package_id, owner } => {
                self.discover_select(package_id, owner)
            }
            Message::DiscoverInstallResult { owner, name } => {
                self.discover_install_result(owner, name)
            }
            Message::DiscoverDetailLoaded(result) => self.discover_detail_loaded(result),
            Message::DiscoverCommentsLoaded(result) => self.discover_comments_loaded(result),
            Message::DiscoverBack => self.discover_back(),
            Message::RatePackage(stars) => self.rate_package(stars),
            Message::RatingUpdated(result) => self.rating_updated(result),
            Message::CommentInputChanged(value) => {
                self.discover_comment_input = value;
                Update::none()
            }
            Message::AddComment => self.add_comment(),
            Message::CommentAdded(result) => self.comment_added(result),
            Message::OpenReadmeLink(uri) => {
                let _ = open::that(uri.as_str());
                Update::none()
            }
            Message::DiscoverInstall => self.discover_install(),
            Message::InstallResolved(seq, result) => self.install_resolved(seq, result),
            Message::ConsentGrant { enable } => self.consent_grant(enable),
            Message::ConsentCancel => self.consent_cancel(),
            Message::ParamValueEdit(target, key, edit) => self.param_value_edit(target, key, edit),
            Message::ParamPromptSubmit => self.param_prompt_submit(),
            Message::ParamPromptCancel => self.param_prompt_cancel(),
            Message::ParamConfigSave => self.param_config_save(),
            Message::ParamConfigClearSecret(key) => self.param_config_clear_secret(key),

            // -------- private & shared -------------------------------------
            Message::OpenShared => self.open_shared(),
            Message::SharedLoaded(result) => self.shared_loaded(result),
            Message::MyCloudLoaded(result) => self.my_cloud_loaded(result),
            Message::InstallShared { owner, name } => self.begin_install(owner, name),

            // -------- top action bar ---------------------------------------
            Message::Reload => {
                // Pick up a Settings change to the advanced-features gate without reopening.
                self.advanced_features =
                    smudgy_core::models::settings::load_settings().advanced_scripting_features;
                let toast = self.show_toast(format!("Reloaded scripts for {}.", self.server_name));
                Update::new(
                    Task::batch([
                        Task::done(self.load_scripts_message()),
                        Task::done(Message::LoadFolders),
                        Task::done(Message::LoadModules),
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                        Task::done(Message::LoadFeaturedDiscover),
                        toast,
                    ]),
                    Some(Event::ScriptsChanged {
                        server_name: self.server_name.clone(),
                    }),
                )
            }
            Message::Inspect => {
                match smudgy_core::session::registry::get_inspector_address(self.session_id) {
                    Some(addr) => {
                        crate::windows::smudgy_window::spawn_inspector(addr);
                        Update::none()
                    }
                    // The inspector port is opened at session-connect time, so a session
                    // that connected before advanced features were turned on has none yet.
                    // Surface it (a log line is invisible in a windowed build) and point at
                    // the fix: reconnect. The button itself is already gated on advanced
                    // features being on, so we don't repeat that here.
                    None => {
                        log::warn!(
                            "No script inspector for session {}: it is created at connect \
                             time; reconnect this session to start it.",
                            self.server_name
                        );
                        Update::with_task(self.show_toast(
                            "No inspector yet — it starts when the session connects. \
                             Reconnect this session, then click Inspect again.",
                        ))
                    }
                }
            }

            // -------- palette ----------------------------------------------
            Message::OpenPalette => {
                self.palette_open = true;
                self.palette_query.clear();
                self.palette_cursor = 0;
                self.new_menu_open = false;
                Update::with_task(self.focus_palette())
            }
            Message::ClosePalette => {
                self.palette_open = false;
                Update::none()
            }
            Message::PaletteInput(value) => {
                self.palette_query = value;
                self.palette_cursor = 0;
                Update::none()
            }
            Message::PaletteMove(delta) => self.palette_move(delta),
            Message::PaletteRun => self.palette_run_active(),
            Message::PaletteRunItem(index) => {
                self.palette_cursor = index;
                self.palette_run_active()
            }

            // -------- toast ------------------------------------------------
            Message::DismissToast(toast_id) => {
                if toast_id == self.toast_gen {
                    self.toast = None;
                }
                Update::none()
            }
        }
    }

    // ---- guards ------------------------------------------------------------

    fn is_edit_message(message: &Message) -> bool {
        match message {
            Message::ScriptEditorAction(action) => matches!(action, text_editor::Action::Edit(_)),
            Message::SetName(_)
            | Message::SetAliasPattern(_)
            | Message::SetBehavior(_)
            | Message::AdjustPriority(_)
            | Message::ToggleFallthrough
            | Message::AddPattern
            | Message::RemovePattern(_)
            | Message::SetPatternKind(_, _)
            | Message::SetPatternText(_, _)
            | Message::MarkHotkeyState(_) => true,
            _ => false,
        }
    }

    fn is_guarded_navigation(message: &Message) -> bool {
        matches!(
            message,
            Message::SelectScript(_)
                | Message::SelectFolder(_)
                | Message::SelectModule(_)
                | Message::SelectOwnedPackage(_)
                | Message::SelectInstalledPackage(_)
                | Message::ShowDashboard
                | Message::OpenDiscover
                | Message::OpenShared
                | Message::OpenStoreInspector
                | Message::NewAlias
                | Message::NewTrigger
                | Message::NewHotkey
                | Message::NewFolder
                | Message::NewModule
                | Message::NewPackage
        )
    }

    /// Resets per-pane selection scaffolding before opening a new pane.
    pub(super) fn clear_selection(&mut self) {
        self.new_menu_open = false;
        self.confirm_folder_delete = false;
        self.confirm_delete_local = false;
        self.confirm_uninstall = false;
        self.confirm_trust = false;
        // Drop any open manifest draft + its unsaved/editing flags — leaving the owned-package pane
        // abandons the edit (re-seeded fresh from disk when an owned package is next opened). Also
        // keeps the unsaved-changes guard from later firing for a package that's no longer open.
        self.manifest_draft = None;
        self.manifest_dirty = false;
        self.manifest_editing = false;
        // Drop the inline param-value editor; the next package pane re-seeds it from its own params.
        self.param_config = None;
        // Abandon any in-flight install confirmation / update re-prompt on navigation — neither
        // has written anything yet (the consent window writes only on Grant). Bumping the
        // generation also discards a still-pending resolve so it can't pop a stale window later.
        self.consent_prompt = None;
        self.update_delta = None;
        self.install_seq.bump();
        // Drop any not-yet-shown required-params prompts queued after a multi-package install; their
        // packages are already installed (just left unconfigured), so navigating away is safe.
        self.param_prompt_queue.clear();
        // Opening any pane abandons the manage pane's in-flight detail load too — invalidate it so a
        // late result can't repaint or record consent against the package that was open before.
        self.detail_seq.bump();
    }

    /// The cloud package client (constructed per use).
    pub(super) fn package_client(&self) -> smudgy_cloud::package_api::PackageApiClient {
        smudgy_cloud::package_api::PackageApiClient::new(
            self.cloud.base_url.as_str(),
            self.cloud.credentials.clone(),
        )
    }

    pub(super) fn signed_in(&self) -> bool {
        self.cloud.snapshot.get().signed_in
    }
}

// ---- top-level view --------------------------------------------------------

use iced::widget::{column, container, row, scrollable, stack};
use iced::{Length, Padding};

impl AutomationsWindow {
    pub fn view(&self) -> Elem<'_> {
        let main = column![
            self.view_topbar(),
            self.view_nav_banner(),
            container(scrollable(self.view_pane()).height(Length::Fill))
                .width(Length::Fill)
                .height(Length::Fill),
        ]
        .spacing(0)
        .width(Length::Fill)
        .height(Length::Fill);

        let base = container(
            row![self.view_sidebar(), main]
                .spacing(0)
                .height(Length::Fill),
        )
        .padding(Padding::ZERO)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|theme: &crate::theme::Theme| container::Style {
            background: Some(common::top_gradient(
                theme.styles.general.top_highlight,
                theme.styles.general.background,
            )),
            ..Default::default()
        });

        let mut layers: Vec<Elem<'_>> = vec![base.into()];

        if self.palette_open {
            layers.push(self.view_palette());
        }
        if let Some(message) = &self.toast {
            layers.push(common::toast(message));
        }

        stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The sticky unsaved-changes banner, shown while a navigation is deferred.
    fn view_nav_banner(&self) -> Elem<'_> {
        use iced::alignment::Vertical;
        use iced::widget::{button, text};
        if self.pending_nav.is_none() {
            return iced::widget::space::vertical()
                .height(Length::Fixed(0.0))
                .into();
        }
        container(
            row![
                text("\u{25CF}").size(10.0).style(common::danger),
                text("You have unsaved changes.").size(13.0),
                iced::widget::space::horizontal(),
                button(text("Discard").size(13.0))
                    .style(crate::theme::builtins::button::secondary)
                    .on_press(Message::ConfirmDiscardNav),
                button(text("Keep editing").size(13.0))
                    .style(crate::theme::builtins::button::primary)
                    .on_press(Message::CancelDiscardNav),
            ]
            .spacing(10.0)
            .align_y(Vertical::Center),
        )
        .width(Length::Fill)
        .padding(Padding {
            top: 8.0,
            bottom: 8.0,
            left: 18.0,
            right: 18.0,
        })
        .style(common::banner_style)
        .into()
    }

    /// Dispatches to the active content pane.
    fn view_pane(&self) -> Elem<'_> {
        match &self.pane {
            Pane::Dashboard => self.view_dashboard(),
            Pane::Error(errors) => self.view_error(errors),
            Pane::Editor(state) => self.view_editor(state),
            Pane::Folder(state) => self.view_folder_editor(state),
            Pane::Module(state) => self.view_module(state),
            Pane::OwnedPackage => self.view_owned_package(),
            Pane::NewPackage { name, error } => self.view_new_package(name, error.as_deref()),
            Pane::InstalledPackage => self.view_installed_package(),
            Pane::CreatorAutomation {
                creator_id,
                kind,
                name,
            } => self.view_creator_automation(creator_id, *kind, name),
            Pane::Discover => self.view_discover(),
            Pane::Shared => self.view_shared(),
            Pane::StoreInspector => self.view_store_inspector(),
        }
    }

    fn view_error(&self, errors: &[String]) -> Elem<'_> {
        use iced::widget::text;
        let mut col = column![].spacing(8).padding(28);
        for err in errors {
            col = col.push(text(err.clone()).size(13.0).style(common::danger));
        }
        col.width(Length::Fill).into()
    }
}
