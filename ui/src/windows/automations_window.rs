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

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use iced::event::{Event as IcedEvent, Status};
use iced::keyboard::{self, key::Named};
use iced::widget::{markdown, operation, text_editor};
use iced::{Subscription, Task, window};
use smudgy_cloud::cloud_api::FriendView;
use smudgy_cloud::package_api::{
    CommentView, PackageDetail, PackageGrantView, PackageSearchResult, ResolvedPackageWire,
    VersionListItem,
};
use smudgy_cloud::{CloudError, Uuid};
use smudgy_core::models::local_packages::{LocalPackage, PublicationWarning, PublishSummary};
use smudgy_core::models::modules::{ModuleFile, ModuleSettings};
use smudgy_core::models::packages::PackageTree;
use smudgy_core::models::profile_activation::ProfileActivation;
use smudgy_core::models::server;
use smudgy_core::models::shared_packages::{
    LockedPackage, PackagePermissions, ParameterScope, SharedPackageLock, UpdateMode,
};
use smudgy_core::models::{ScriptLang, aliases, automation_transaction, hotkeys};
use smudgy_core::session::SessionId;
use smudgy_core::session::runtime::catalogue::{CatalogueEvent, CatalogueSnapshot};
use smudgy_core::session::runtime::{AutomationEvent, AutomationKind};

use crate::cloud_account::{
    CloudHandles, PackageOperationCompletion, PackageOperationId, PackageOperationPermit,
};
use crate::keymap::MaybePhysicalKey;
use crate::theme::Element as ThemedElement;
use crate::update::Update;

pub(crate) mod common;
// Host-owned writable-code controller and `iced-code-editor` adapter. The current
// read-only previews intentionally keep their existing text widgets.
#[allow(dead_code)]
mod code_editor;
mod dashboard;
mod editors;
mod highlight;
mod keyboard_control;
mod manifest;
pub(crate) mod model;
mod package_tabs;
mod packages;
mod palette;
mod param_values;
mod sidebar;
mod store_inspector;
mod topbar;

use manifest::{ManifestDraft, ManifestEdit, ManifestTab};
use model::{LiveAutomations, PackageGraph, PatternKind, Script, ScriptKey};
pub(crate) use packages::StaleInstallCheck;
use packages::{
    AccountReadFence, ConsentPrompt, DetailSeq, DiscoverSearchSeq, DiscoverSeq, FilePreview,
    GraphSeq, InstallResolution, InstallSeq, PackageChangeFinalize, ParamConfig, ParamPrompt,
    PreparedConsentCache, PublicationStatus, PublishOutput, ShareSeq, UpdateDelta,
};

/// Returns the traversal direction for an unconsumed, unmodified Tab press.
/// `true` means backwards (Shift+Tab). Shortcut-modified Tabs remain
/// available to the window manager/application.
fn tab_traversal(modifiers: keyboard::Modifiers, status: Status) -> Option<bool> {
    (status == Status::Ignored && !modifiers.control() && !modifiers.alt() && !modifiers.logo())
        .then_some(modifiers.shift())
}

fn code_completion_shortcut(
    key: &keyboard::Key,
    modifiers: keyboard::Modifiers,
    status: Status,
) -> bool {
    status == Status::Ignored
        && modifiers.control()
        && matches!(key, keyboard::Key::Named(keyboard::key::Named::Space))
}

fn matcher_truecolor_range(
    color: smudgy_core::models::matchers::MatcherColor,
) -> Option<smudgy_core::models::matchers::MatcherHsvRange> {
    use smudgy_core::models::matchers::{MatcherColor, MatcherHsvRange};
    let MatcherColor::Truecolor { r, g, b, range } = color else {
        return None;
    };
    let point = smudgy_core::models::matchers::MatcherHsv::from_rgb(r, g, b);
    let range = range
        .unwrap_or_else(|| MatcherHsvRange::from_to(point, point))
        .rgb_canonicalized();
    let (from, to) = range.directed_endpoints();
    Some(MatcherHsvRange::from_to(from, to))
}

fn matcher_hsv_to_picker(
    hsv: smudgy_core::models::matchers::MatcherHsv,
) -> crate::components::color_picker::Hsv {
    crate::components::color_picker::Hsv {
        hue: f32::from(hsv.hue % 360),
        saturation: f32::from(hsv.saturation) / 255.0,
        value: f32::from(hsv.value) / 255.0,
    }
}

fn picker_hsv_to_matcher(
    hsv: crate::components::color_picker::Hsv,
) -> smudgy_core::models::matchers::MatcherHsv {
    let hsv = hsv.normalized();
    let quantized = smudgy_core::models::matchers::MatcherHsv {
        hue: (hsv.hue.round() as u16) % 360,
        saturation: (hsv.saturation * 255.0).round() as u8,
        value: (hsv.value * 255.0).round() as u8,
    };
    // Store the HSV value that the displayed 8-bit RGB swatch represents.
    // Without this canonical value, HSV-to-RGB-to-HSV quantization can make a
    // single-color or narrow range reject its selected color. Keep the
    // specified hue as a range boundary for an achromatic endpoint. Matching
    // does not compare hue when the input color is achromatic.
    quantized.rgb_canonicalized()
}

fn matcher_truecolor_from_range(
    range: smudgy_core::models::matchers::MatcherHsvRange,
) -> smudgy_core::models::matchers::MatcherColor {
    let from = range.first.rgb_canonicalized();
    let to = range.second.rgb_canonicalized();
    let range = smudgy_core::models::matchers::MatcherHsvRange::from_to(from, to);
    let (r, g, b) = range.first.to_rgb();
    smudgy_core::models::matchers::MatcherColor::Truecolor {
        r,
        g,
        b,
        range: Some(range),
    }
}

fn matcher_hsv_hex(hsv: smudgy_core::models::matchers::MatcherHsv) -> String {
    let (r, g, b) = hsv.to_rgb();
    format!("#{r:02x}{g:02x}{b:02x}")
}

/// Convenience alias for this window's themed elements.
pub(crate) type Elem<'a> = ThemedElement<'a, Message>;

/// Events bubbled up to the daemon when persisted runtime inputs change.
#[derive(Debug, Clone)]
pub enum Event {
    /// Aliases, triggers, hotkeys, or their legacy folder enablement changed. Live sessions
    /// reconcile only the changed user-owned registrations.
    UserAutomationsChanged { server_name: String },
    /// Modules or script packages changed and still require the existing full engine reload.
    ScriptsChanged { server_name: String },
    /// Replace this singleton window's disk/session context after the user has accepted any
    /// unsaved-changes guard. The daemon owns reconstruction so every pending task and subscription
    /// from the old context can be fenced out together.
    SwitchContext {
        server_name: String,
        session_id: SessionId,
        profile_name: String,
    },
    /// The user chose Keep editing for this exact daemon-requested context switch.
    ContextSwitchCancelled,
    /// Close the native singleton after the window has accepted the unsaved-changes guard.
    CloseRequested,
    /// The user kept editing after an application-exit request was routed through this window.
    /// The daemon uses this to release the main window that it kept alive for the confirmation.
    CloseCancelled,
}

/// A daemon-requested navigation that a save or discard released without the user choosing
/// either banner action. The daemon retains state for both targets while the guard is open, so
/// each is answered with its cancel event; see [`AutomationsWindow::release_pending_navigation`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleasedNavigation {
    /// [`Message::RequestClose`] was pending.
    Close,
    /// [`Message::SwitchContext`] was pending.
    ContextSwitch,
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

/// The body of a script editor — the per-kind editable fields. Writable JS/TS
/// bodies live in [`AutomationsWindow::code_editor`].
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
        /// The unified, ordered matcher row list (role + syntax per row).
        rows: Vec<model::TriggerRow>,
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
    pub activation: ProfileActivation,
    pub error: Option<String>,
}

/// View vs. create, for the module pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModuleMode {
    View,
    Create,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ModuleTab {
    #[default]
    Settings,
    Source,
}

/// State for the module pane (a local, non-shareable helper file).
#[derive(Debug, Clone)]
pub struct ModuleState {
    pub mode: ModuleMode,
    pub subpath: String,
    pub path: Option<PathBuf>,
    pub name: String,
    pub tab: ModuleTab,
    pub activation: ProfileActivation,
    /// Create-mode activation follows the path-based default only until the user changes it.
    pub activation_touched: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InstalledPackageTab {
    #[default]
    About,
    Settings,
    Source,
    Permissions,
}

/// The installed package's README belongs to the exact resolved detail generation. Keeping
/// loading and failure distinct from a successful response with no README prevents About from
/// describing incomplete data as an authoritative absence.
pub(super) enum InstalledReadmeState {
    Loading,
    Loaded(Option<markdown::Content>),
    Failed(String),
}

impl InstalledReadmeState {
    #[must_use]
    pub(super) fn is_loaded(&self) -> bool {
        matches!(self, Self::Loaded(_))
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LocalPackageTab {
    #[default]
    About,
    Settings,
    Source,
    Permissions,
    Manifest,
    Sharing,
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
    /// Ask the singleton window to move to another live session. This is normal guarded navigation:
    /// clean windows switch immediately, while dirty windows reuse the existing Keep editing /
    /// Discard banner before emitting [`Event::SwitchContext`].
    SwitchContext {
        server_name: String,
        session_id: SessionId,
        profile_name: String,
    },
    // ---- loading -----------------------------------------------------------
    ScriptsLoaded {
        scripts: BTreeMap<String, Script>,
        load: Option<automation_transaction::AutomationStateSnapshot>,
        errors: Arc<Vec<String>>,
    },
    LoadFolders,
    LoadModules,
    LoadLocalPackages,
    LoadInstalledPackages,
    /// The app-global signed-in identity changed. Invalidate authenticated pane data before any
    /// result launched under the previous identity can repaint it.
    AccountChanged,

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
    /// An edit in the alias Regex field's one-line editor.
    AliasRegexAction(text_editor::Action),
    // alias matcher draft
    SetAliasKind(model::AliasKind),
    SetArgName(usize, String),
    SetArgKind(usize, smudgy_core::models::matchers::ArgKind),
    AddArg,
    RemoveArg(usize),
    SetCmdMode(smudgy_core::models::matchers::CmdMode),
    SetParseMode(smudgy_core::models::matchers::ParseMode),
    /// Open/close the Parsing picker's floating list.
    OpenParsingPicker,
    CloseParsingPicker,
    /// Move the Parsing picker's keyboard cursor by a delta.
    MoveParsingCursor(i32),
    /// An edit in the alias Simple-pattern field's one-line editor.
    AliasPatternAction(text_editor::Action),
    ToggleAnchorStart,
    ToggleAnchorEnd,
    TogglePrompt,
    RevealOrder,
    HideOrder,
    /// Insert a capture reference at the caret in the action body.
    InsertReference(String),
    /// Move the open script to a folder (`None` = top level). Also dispatched by
    /// the palette's "Move to…" group for the selected script.
    SetScriptFolder(Option<String>),
    SetBehavior(ScriptLang),
    AdjustPriority(i32),
    ToggleFallthrough,
    /// Flip the open alias's "sent text may match itself" opt-in.
    ToggleAllowSelfMatch,
    /// An event emitted by the active writable JS/TS editor.
    CodeEditorAction(code_editor::BoundEditorMessage),
    /// Applies a language-service completion from the visible candidate list.
    ApplyCodeCompletion(code_editor::CompletionSelection),
    /// Synchronizes keyboard reveal state with an exact completion popup's scroll viewport.
    CodeCompletionViewportChanged(code_editor::CompletionViewportTarget),
    /// Requests completions at the active code-editor caret (Ctrl+Space or button).
    TriggerCodeCompletion,
    /// A pointer button went down somewhere in this window.
    ///
    /// The canvas editor keeps its own keyboard focus and never notices
    /// clicks landing elsewhere, so the window releases that focus itself
    /// unless the pointer is over the editor region.
    PointerPressed(window::Id),
    /// The pointer entered the writable code editor region (editor, overlays, and chrome).
    CodeEditorPointerEntered,
    /// The pointer left the writable code editor region.
    CodeEditorPointerExited,
    /// Closes host-owned completion and hover overlays without changing source.
    DismissCodeOverlays,
    /// Keeps one exact hover card alive while the pointer is over its rich content.
    CodeHoverOverlayEntered(code_editor::HoverOverlayTarget),
    /// Starts the grace period for dismissing one exact hover card.
    CodeHoverOverlayExited(code_editor::HoverOverlayTarget),
    /// An intentionally inert link from user-authored hover documentation.
    CodeHoverLinkPressed(code_editor::HoverOverlayTarget, markdown::Uri),
    /// An intentionally inert link from signature or parameter documentation.
    CodeSignatureLinkPressed(code_editor::SignatureOverlayTarget, markdown::Uri),
    /// Opens the first current-project target from an accepted definition response.
    NavigateCodeDefinition(code_editor::DefinitionNavigation),
    /// An edit in a plaintext hotkey body (JS/TS hotkeys use `CodeEditorAction`).
    HotkeyTextAction(text_editor::Action),
    /// An edit in the send-text action draft.
    SendTextAction(text_editor::Action),
    /// Expand/collapse the Try-it accordion (collapsed by default).
    ToggleTryIt,
    SetTestInput(String),
    ToggleEnabled,
    MarkHotkeyState(Vec<MaybePhysicalKey>),
    // trigger patterns
    AddPattern,
    /// Add an exception row (Pattern syntax by default).
    AddExceptionRow,
    /// Add a raw row (always Regex syntax).
    AddRawRow,
    /// A click on one of the trigger pane's matcher cards: creates the first
    /// matcher row at the zero-matcher state, or re-shapes the single existing
    /// matcher at the selector state (README §4).
    SetTriggerCard(model::TriggerCard),
    RemovePattern(usize),
    /// Move a row up/down within its role group (the phase order is fixed).
    MoveRowUp(usize),
    MoveRowDown(usize),
    /// An edit in a trigger row's one-line source editor.
    RowSourceAction(usize, text_editor::Action),
    SetRowSyntax(usize, smudgy_core::models::matchers::MatcherSyntax),
    ToggleRowAnchorStart(usize),
    ToggleRowAnchorEnd(usize),
    /// Adds or removes the color filter for a normal or exception matcher row.
    ToggleRowColor(usize, bool),
    SelectRowColorChannel(usize, smudgy_core::models::matchers::MatcherColorChannel),
    SelectRowColorKind(usize, model::MatcherColorKind),
    SetRowAnsiColor(usize, u8),
    SetRowXtermColor(usize, u8),
    SetRowColorRange(
        usize,
        model::ColorRangeEndpoint,
        crate::components::color_picker::Message,
    ),
    SetRowColorRangeHex(usize, model::ColorRangeEndpoint, String),
    SetRowExactTruecolorHex(usize, String),
    SetRowExactTruecolorRgb(usize, model::TruecolorComponent, String),
    ToggleRowColorAttribute(
        usize,
        smudgy_core::models::matchers::MatcherTextAttribute,
        bool,
    ),

    // ---- save bar ----------------------------------------------------------
    Save,
    Discard,
    Delete,
    /// Confirm only the pending navigation revision that rendered this action.
    ConfirmDiscardNavRevision(u64),
    /// Cancel only the pending navigation revision that rendered this action.
    CancelDiscardNavRevision(u64),
    RequestClose,
    /// A save or discard released a daemon-requested navigation. Delivered through a task so
    /// the releasing update can still carry its own event.
    NavigationReleased(ReleasedNavigation),

    // ---- folder ------------------------------------------------------------
    SetFolderPath(String),
    EnableEverywhere,
    DisableEverywhere,
    ToggleActivationProfile(String),
    SaveFolder,
    RequestDeleteFolder,
    CancelDeleteFolder,
    ConfirmDeleteFolder(bool),

    // ---- module ------------------------------------------------------------
    SaveModule,
    SetNewModuleName(String),
    CreateModule,
    SelectModuleTab(ModuleTab),

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
    PublishFinished {
        server_name: String,
        name: String,
        operation_id: PackageOperationId,
        completion: PackageOperationCompletion,
        credential_generation: u64,
        publisher_id: Uuid,
        result: Result<PublishSummary, String>,
    },
    RequestDeleteOwned,
    CancelDeleteOwned,
    DeleteOwned,
    SetNewPackageName(String),
    CreatePackage,
    // owned sharing / versions
    SetVisibility(bool),
    VisibilityUpdated {
        server_name: String,
        name: String,
        seq: ShareSeq,
        package_id: Uuid,
        operation_id: PackageOperationId,
        completion: PackageOperationCompletion,
        credential_generation: u64,
        result: Result<bool, CloudError>,
    },
    YankVersion {
        version: String,
        yanked: bool,
    },
    DeleteVersion(String),
    VersionsUpdated {
        server_name: String,
        name: String,
        seq: ShareSeq,
        package_id: Uuid,
        operation_id: PackageOperationId,
        completion: PackageOperationCompletion,
        credential_generation: u64,
        result: Result<Vec<VersionListItem>, CloudError>,
    },
    ShareWithFriend(Uuid),
    GrantsUpdated {
        server_name: String,
        name: String,
        seq: ShareSeq,
        package_id: Uuid,
        operation_id: PackageOperationId,
        completion: PackageOperationCompletion,
        credential_generation: u64,
        result: Result<Vec<PackageGrantView>, CloudError>,
    },
    /// A mutation launched by an older singleton instance completed. Refresh only when that same
    /// package is open; never navigate the replacement window on its behalf.
    RefreshOwnedShareIfOpen {
        server_name: String,
        name: String,
    },
    #[allow(clippy::type_complexity)]
    OwnedShareLoaded {
        account_epoch: u64,
        account_fence: AccountReadFence,
        seq: ShareSeq,
        name: String,
        result: Result<
            (
                Uuid,
                bool,
                Vec<FriendView>,
                Vec<PackageGrantView>,
                Vec<VersionListItem>,
            ),
            CloudError,
        >,
    },

    // ---- installed package -------------------------------------------------
    /// The [`DetailSeq`] is the manage-pane detail generation captured when the load started; a
    /// stale result (the open package changed, navigation, uninstall, or a re-resolve) is discarded.
    InstalledDetailLoaded(
        DetailSeq,
        AccountReadFence,
        Box<Result<packages::InstalledDetail, CloudError>>,
    ),
    InstalledLatestCompared(
        DetailSeq,
        AccountReadFence,
        Result<packages::InstalledLatestComparison, CloudError>,
    ),
    InstalledVersionChangeResolved(
        DetailSeq,
        AccountReadFence,
        UpdateMode,
        Result<InstallResolution, CloudError>,
    ),
    LocalManifestRequirementsResolved {
        seq: InstallSeq,
        account_fence: AccountReadFence,
        completion: PackageOperationCompletion,
        result: Result<ConsentPrompt, String>,
    },
    InstalledResolvedForGraph(
        GraphSeq,
        AccountReadFence,
        String,
        Option<String>,
        Result<(ResolvedPackageWire, PackagePermissions), CloudError>,
    ),
    SetInstalledUpdateMode(UpdateMode),
    /// Select a source file in the open installed package.
    SelectInstalledFile(String),
    SelectInstalledPackageTab(InstalledPackageTab),
    SelectLocalPackageTab(LocalPackageTab),
    SetParameterScope(ParameterScope),
    ConfirmGlobalParameterSource,
    CancelGlobalParameterSource,
    SelectParameterProfile(String),
    /// Open the dialog that copies the current profile's settings to another profile.
    OpenCopySettings,
    SelectCopySettingsDestination(String),
    CancelCopySettings,
    ConfirmCopySettings,
    /// A source-browser module body finished fetching for the open installed package, keyed by its
    /// `content_hash`. Content-addressed, so a late result just fills the cache and is matched to
    /// the selected file by hash — no staleness token needed.
    InstalledSourceLoaded {
        hash: String,
        account_fence: AccountReadFence,
        result: Result<FilePreview, CloudError>,
    },
    RequestUninstall,
    /// "Keep them": keep the offered orphans (clears only the orphan set; forced breaks still go).
    UninstallKeepOrphans,
    CancelUninstall,
    ConfirmUninstall,
    StartForkPackage,
    SetForkName(String),
    CancelForkPackage,
    ForkPackage,
    ForkFinished {
        source_specifier: String,
        destination_name: String,
        operation_id: PackageOperationId,
        completion: PackageOperationCompletion,
        origin: Selection,
        origin_revision: u64,
        result: Result<String, String>,
    },
    /// An async installed-list sweep of account-owned legacy rows finished: stale entries were
    /// pruned, or no still-current row needed a change.
    StaleAccountInstallsChecked {
        outcome: StaleInstallCheck,
    },
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
    InstalledRatingUpdated {
        detail_seq: DetailSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<PackageDetail, CloudError>,
    },

    // ---- discover ----------------------------------------------------------
    OpenDiscover,
    /// Loads the dashboard "Discover" teaser (a default-scope empty-query search).
    LoadFeaturedDiscover,
    FeaturedDiscoverLoaded(Result<Vec<PackageSearchResult>, CloudError>),
    DiscoverQueryChanged(String),
    DiscoverSearch,
    DiscoverScopeChanged(DiscoverScope),
    DiscoverResultsLoaded(
        DiscoverSearchSeq,
        Result<Vec<PackageSearchResult>, CloudError>,
    ),
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
    DiscoverDetailLoaded {
        seq: DiscoverSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<PackageDetail, CloudError>,
    },
    DiscoverCommentsLoaded {
        seq: DiscoverSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<Vec<CommentView>, CloudError>,
    },
    DiscoverBack,
    RatePackage(i16),
    RatingUpdated {
        seq: DiscoverSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<PackageDetail, CloudError>,
    },
    CommentInputChanged(String),
    AddComment,
    CommentAdded {
        seq: DiscoverSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<CommentView, CloudError>,
    },
    OpenReadmeLink(markdown::Uri),
    DiscoverInstall,
    /// The [`InstallSeq`] is the install generation captured at `begin_install`; a stale result
    /// (the user navigated away / clicked Back / started another install) is discarded.
    InstallResolved(
        InstallSeq,
        AccountReadFence,
        Result<InstallResolution, CloudError>,
    ),
    // install-time consent confirmation; `enable` = "Install & enable" vs "Install, don't
    // enable" (both record the same consent — they differ only in turning the package on now).
    ConsentGrant {
        enable: bool,
    },
    /// Hash-verified cache preparation completed for the consent prompt at `seq`. The lockfile is
    /// still unchanged; a stale result is discarded.
    ConsentCachePrepared {
        seq: InstallSeq,
        account_fence: AccountReadFence,
        enable: bool,
        result: Result<PreparedConsentCache, String>,
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
    SharedLoaded {
        account_epoch: u64,
        account_fence: AccountReadFence,
        result: Result<Vec<PackageDetail>, CloudError>,
    },
    /// The caller's own cloud packages (`GET /packages/mine`), shown alongside the
    /// shared-with-me list in the "Private & Shared" pane — including private ones with
    /// no local copy on this machine, which appear in no other surface.
    MyCloudLoaded {
        account_epoch: u64,
        account_fence: AccountReadFence,
        result: Result<Vec<PackageDetail>, CloudError>,
    },
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

    // ---- keyboard focus traversal -----------------------------------------
    /// Focus one feature-local composite color control after a pointer press.
    FocusColorControl(iced::widget::Id),
    FocusNext(window::Id),
    FocusPrevious(window::Id),

    // ---- toast -------------------------------------------------------------
    DismissToast(u64),

    /// Drain ready embedded language-service events without blocking the UI.
    PollLanguageService,

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

/// The app-wide singleton Automations window, bound to one disk/session context at a time.
pub struct AutomationsWindow {
    window_id: window::Id,
    pub(super) server_name: String,
    pub(super) cloud: CloudHandles,
    pub(super) session_id: SessionId,
    pub(super) profile_name: String,
    /// Cleared by the daemon before this bound session leaves the UI store. Omitting the runtime
    /// subscriptions on the next subscription rebuild cancels a pre-registration polling loop as
    /// well as any live broadcast receivers; the remaining window continues as an offline editor.
    session_binding_live: bool,
    pub(super) profile_names: Vec<String>,
    /// False when the profile directory could not be read as one complete inventory. Per-profile
    /// edits stay disabled in that state so a partial list cannot erase hidden profile keys.
    pub(super) profile_inventory_complete: bool,
    pub(super) parameter_profile: String,
    /// Window-local fence for authenticated reads, including nickname changes that retain the
    /// same session credential.
    pub(super) account_epoch: u64,
    /// Profile→Global needs an explicit source when stored profile values differ.
    pub(super) confirm_global_parameter_source: bool,
    /// The open copy-settings dialog, if any. Cleared with the rest of the parameter editor.
    pub(super) copy_settings_prompt: Option<packages::CopySettingsPrompt>,
    pub(super) mud_host: Option<String>,
    /// Whether advanced scripting features are unlocked (settings `advanced_scripting_features`):
    /// the "Remove sandbox" package action and the script inspector. Read at construction and
    /// refreshed on Reload — toggling it in Settings takes effect on the next reload/reopen.
    pub(super) advanced_features: bool,

    // ---- script tree -------------------------------------------------------
    pub(super) scripts: BTreeMap<String, Script>,
    pub(super) packages: PackageTree,
    /// Complete on-disk snapshot from which `scripts` and `packages` were built. Every editor save
    /// compares this baseline before committing so trusted script mutations cannot be overwritten
    /// by a stale open window.
    pub(super) automation_snapshot: Option<automation_transaction::AutomationStateSnapshot>,
    /// Present when `packages.json` could not be read as authoritative state. Keep the last good
    /// tree for display, but do not permit activation writes against a fabricated default.
    pub(super) folder_state_error: Option<String>,
    pub(super) modules: Vec<ModuleFile>,
    pub(super) module_settings: ModuleSettings,
    /// Present when either the module inventory or its settings file is unreadable. Runtime loads
    /// modules fail closed in this state, so the editor must not present editable/default-on state.
    pub(super) module_state_error: Option<String>,
    pub(super) local_packages: Vec<String>,
    pub(super) installed_packages: Vec<LockedPackage>,
    /// The local package inventory or its governing-row reconciliation is unavailable. Preserve
    /// the last good list for display, but block every mutation whose target depends on shadowing.
    pub(super) local_package_state_error: Option<String>,
    /// The installed-package lock is unavailable. Preserve the last good rows for display, but do
    /// not infer absent installs or permit settings/activation mutations.
    pub(super) installed_package_state_error: Option<String>,

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
    /// Advanced whenever a pane replacement clears selection-owned state. Async recovery may
    /// navigate only when the revision captured at launch still matches this value.
    pub(super) selection_revision: u64,
    pub(super) collapsed_folders: HashSet<String>,
    pub(super) pane: Pane,

    // ---- sidebar -----------------------------------------------------------
    pub(super) search: String,
    pub(super) chip: Chip,
    pub(super) new_menu_open: bool,

    // ---- shared editor buffers --------------------------------------------
    /// The active writable JS/TS/module/package editor. This field precedes
    /// `language_service`, so its Drop queues CloseDocument before host shutdown.
    code_editor: Option<code_editor::ActiveCodeEditor>,
    /// Whether the pointer is over the code editor region; see `Message::PointerPressed`.
    pointer_over_code_editor: bool,
    /// Lazily spawned and retained for this Automations-window lifetime.
    pub(super) language_service:
        Option<smudgy_script::language_service_worker::LanguageServiceHost>,
    /// Saved-source graph currently installed beneath the active editor overlay.
    language_project_context: Option<code_editor::LanguageProjectContext>,
    /// Context selected by the newest editor binding, which may still be awaiting refresh.
    language_project_target_context: Option<code_editor::LanguageProjectContext>,
    /// Exact in-flight graph refresh. Only its acknowledgement commits the installed context.
    pending_language_project_refresh: Option<code_editor::PendingLanguageProjectRefresh>,
    /// Stable identities for saved module/package sources during this window lifetime.
    language_source_ids:
        HashMap<code_editor::LanguageSourceKey, smudgy_script::language_service::DocumentId>,
    /// Local editor-mount fence for delayed upstream tasks such as clipboard reads.
    code_editor_mount_generation: u64,
    next_language_graph_generation: u64,
    pub(super) next_language_request_id: u64,
    pub(super) next_code_disk_revision: u64,
    /// Saved module text captured when the current module editor was bound. A save compares the
    /// disk file with this baseline before replacing it, so an external edit is never discarded.
    pub(super) module_source_baseline: Option<String>,
    /// Legacy plaintext body used only while a hotkey's behavior is Send Text.
    pub(super) hotkey_text_content: text_editor::Content,
    /// The send-text action draft, held separately from the script draft so
    /// switching action tabs never destroys work. Save writes whichever tab
    /// is active.
    pub(super) send_text_content: text_editor::Content,
    /// Whether the send-text draft has been edited (or came from disk). An
    /// unpinned draft is regenerated from the live matcher on every edit.
    pub(super) action_text_pinned: bool,
    /// As [`Self::action_text_pinned`], for the script draft.
    pub(super) action_script_pinned: bool,
    /// The language the Run JavaScript tab writes: `JS`, or `TS` when the
    /// automation opened as TypeScript (a TS alias stays TS on save).
    pub(super) action_script_lang: ScriptLang,
    /// The alias Simple-pattern field's buffer; `alias_draft.pattern_source`
    /// mirrors it after every edit (the draft stays the compile input).
    pub(super) alias_pattern_content: text_editor::Content,
    /// The alias Regex field's buffer, mirrored into `alias_draft.regex_source`.
    pub(super) alias_regex_content: text_editor::Content,
    /// One buffer per trigger matcher row, kept index-aligned with the open
    /// trigger's `rows` through every add/remove/reorder.
    pub(super) trigger_row_contents: Vec<text_editor::Content>,
    pub(super) hotkey_state: Vec<MaybePhysicalKey>,
    /// The alias editor's matcher draft (kind + every kind's buffers), seeded
    /// on open/create like `hotkey_state` and consumed at save.
    pub(super) alias_draft: model::AliasMatcherDraft,
    /// Whether the "When it runs" module is disclosed by the user's click.
    /// Non-default values force it open regardless (and it cannot re-hide
    /// while they hold); reset when an editor opens.
    pub(super) order_revealed: bool,
    /// Whether the Try-it accordion is expanded; collapsed when an editor opens.
    pub(super) try_it_open: bool,
    /// Whether the Parsing picker's floating list is open.
    pub(super) parsing_open: bool,
    /// The Parsing picker's keyboard cursor (an index into
    /// `ParseModeChoice::ALL`).
    pub(super) parsing_cursor: usize,
    pub(super) test_input: String,
    pub(super) dirty: bool,
    pub(super) pending_nav: Option<Box<Message>>,
    /// Advanced whenever a guarded navigation replaces the pending target. Rendered confirmation
    /// actions carry this value so a click from an older frame cannot affect a newer request.
    pending_nav_revision: u64,
    pub(super) confirm_folder_delete: bool,

    // ---- package dependency graph ------------------------------------------
    pub(super) graph: PackageGraph,
    /// Installed-package specifiers whose newest resolvable version's closure permission union
    /// exceeds the consented grant — the engine holds them at an older fitting version (or won't
    /// load them), so the tree flags them orange and the manage pane shows "update blocked"
    /// (`PACKAGE-ISOLATES-CONSENT-TRUST.md`). Populated by the background graph resolve.
    pub(super) blocked_updates: HashSet<String>,
    /// Generation fence for the background installed-package graph resolve batch.
    pub(super) graph_seq: GraphSeq,

    // ---- owned (local) package state --------------------------------------
    pub(super) local_package: Option<Box<LocalPackage>>,
    pub(super) local_readme: Option<markdown::Content>,
    pub(super) owned_selected_file: Option<String>,
    /// Saved text captured when the selected local-package file was opened.
    pub(super) owned_source_baseline: Option<String>,
    /// Inline rename buffer for the open local package (the folder name is its identity). `Some`
    /// while the rename field is showing; `None` otherwise.
    pub(super) rename_buffer: Option<String>,
    /// Exact local package identity that owns `rename_buffer`. The pair is cleared only by an
    /// explicit cancel/discard or a successful/no-op rename commit.
    pub(super) rename_source_name: Option<String>,
    /// The editable manifest form for the open owned package (the rich editor for its
    /// `smudgy.package.json`). Seeded on open + after a Save; `None` off-pane.
    pub(super) manifest_draft: Option<ManifestDraft>,
    /// Exact on-disk manifest text from the start of the current structured edit.
    pub(super) manifest_source_baseline: Option<String>,
    /// Whether the manifest draft has unsaved edits (independent of the script-editor `dirty`
    /// flag, which guards a different pane).
    pub(super) manifest_dirty: bool,
    /// Whether the manifest section is in the structured editor (vs the default read-only summary).
    pub(super) manifest_editing: bool,
    /// Local package reservation retained while a requirements-changing manifest waits for its
    /// consent/cache steps. Dropping the pane releases it without allowing a stale task to commit.
    pub(super) manifest_operation: Option<PackageOperationPermit>,
    /// Which manifest-editor tab is showing (view-only; reset to `Settings` when a package opens).
    pub(super) manifest_tab: ManifestTab,
    pub(super) authoring_busy: bool,
    /// Exact publish operation represented by `authoring_busy`. Manifest resolution also uses the
    /// visual latch but has its own generation fence and therefore leaves this as `None`.
    pub(super) authoring_operation: Option<PackageOperationId>,
    pub(super) authoring_feedback: Option<String>,
    /// The latest publish command/output, scoped to the package that produced it. Kept separate
    /// from general authoring feedback so it can render beside Publish in a bounded console.
    publish_output: Option<PublishOutput>,
    pub(super) confirm_delete_local: bool,
    pub(super) share_package_id: Option<Uuid>,
    /// Durable/cloud-backed publication knowledge used to gate rename independently of whether
    /// Sharing details are loaded or the user is signed in.
    publication_status: PublicationStatus,
    pub(super) share_is_public: bool,
    pub(super) share_friends: Vec<FriendView>,
    pub(super) share_grants: Vec<PackageGrantView>,
    pub(super) share_versions: Vec<VersionListItem>,
    pub(super) share_busy: bool,
    /// Exact sharing mutation represented by `share_busy`. Share-state loads use `share_seq` only.
    pub(super) share_operation: Option<PackageOperationId>,
    pub(super) share_seq: ShareSeq,
    pub(super) share_feedback: Option<String>,

    // ---- installed package state ------------------------------------------
    pub(super) installed_open: Option<Box<LockedPackage>>,
    pub(super) installed_detail: Option<Box<ResolvedPackageWire>>,
    pub(super) installed_readme: InstalledReadmeState,
    /// The cloud package metadata (rating, install count) for the open installed package, fetched
    /// best-effort alongside the detail resolve. `None` for a local/owned package, while loading, or
    /// when the fetch failed — gating the rating UI on `Some` keeps it to real cloud packages.
    /// Replaced by the fresh `PackageDetail` the server returns when the user rates.
    pub(super) installed_rating: Option<Box<PackageDetail>>,
    pub(super) installed_versions: Vec<String>,
    pub(super) installed_selected_file: Option<String>,
    pub(super) installed_package_tab: InstalledPackageTab,
    pub(super) local_package_tab: LocalPackageTab,
    /// On-demand source for the installed-package source browser, keyed by module `content_hash`
    /// (content-addressed, so identical blobs share an entry and a late fetch is self-validating).
    /// Populated lazily when a file is selected; cleared when a different installed package opens.
    pub(super) installed_source: HashMap<String, FilePreview>,
    pub(super) manage_busy: bool,
    pub(super) manage_feedback: Option<String>,
    /// Inline destination-name buffer for "Edit a copy". The user can keep the package's leaf
    /// name to create a local override, or choose a new name for an independent local package.
    pub(super) fork_name: Option<String>,
    /// Exact installed source that owns `fork_name`, so a retained form can never be rendered or
    /// submitted as if it belonged to another package.
    pub(super) fork_source_specifier: Option<String>,
    /// Exact app-global destination reservation owned by the active Edit-a-copy task.
    pub(super) fork_operation: Option<PackageOperationId>,
    pub(super) confirm_uninstall: bool,
    /// Exact lock snapshot used to render the open uninstall confirmation. Confirm uses it as an
    /// optimistic concurrency token, so a package update or relationship change forces a retry.
    pub(super) uninstall_expected_lock: Option<SharedPackageLock>,
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
    /// The root operation to finish after any required-parameter prompt queue drains.
    package_change_finalize: Option<PackageChangeFinalize>,

    // ---- discover state ----------------------------------------------------
    pub(super) discover_query: String,
    pub(super) discover_scope: DiscoverScope,
    pub(super) discover_results: Vec<PackageSearchResult>,
    /// The dashboard "Discover" teaser: the top results of a default ([`DiscoverScope::Relevant`])
    /// empty-query search, loaded on window init. Kept separate from `discover_results` so it stays
    /// stable regardless of how the user later searches/filters inside the Discover pane.
    pub(super) featured_packages: Vec<PackageSearchResult>,
    pub(super) discover_owner: Option<String>,
    pub(super) discover_requested_package: Option<Uuid>,
    pub(super) discover_detail: Option<Box<PackageDetail>>,
    pub(super) discover_readme: Option<markdown::Content>,
    pub(super) discover_comments: Vec<CommentView>,
    pub(super) discover_comment_input: String,
    pub(super) discover_busy: bool,
    pub(super) discover_error: Option<String>,
    pub(super) discover_seq: DiscoverSeq,
    pub(super) discover_search_seq: DiscoverSearchSeq,
    /// The always-shown install confirmation; shown before any lock entry is written.
    pub(super) consent_prompt: Option<ConsentPrompt>,
    /// The accepted package set is being hash-verified and made cache-complete. Until this clears,
    /// another Grant is disabled; Cancel invalidates the result without touching the lockfile.
    pub(super) consent_busy: bool,
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
    /// Per-profile required-parameter completeness for `param_config` when it is profile-scoped;
    /// `None` otherwise. Maintained by `sync_profile_param_status` after every update so the
    /// Settings tab never reads parameter storage while rendering.
    pub(super) profile_param_status: Option<model::ProfileParamStatus>,

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
    /// Profile directory names for the server, sorted by name. Every profile control in this
    /// window labels a profile by this name; the profile caption is a Connect-dialog detail.
    fn load_profile_choices(server_name: &str) -> Result<Vec<String>, String> {
        let mut names = smudgy_core::models::profile::list_profiles_strict(server_name)
            .map_err(|error| error.to_string())?
            .into_iter()
            .map(|profile| profile.name)
            .collect::<Vec<_>>();
        names.sort();
        Ok(names)
    }

    #[cfg(test)]
    pub fn new(
        window_id: window::Id,
        server_name: String,
        cloud: CloudHandles,
        session_id: SessionId,
    ) -> Self {
        let profile_names = Self::load_profile_choices(&server_name).unwrap_or_default();
        let profile_name = profile_names.first().cloned().unwrap_or_default();
        Self::new_for_profile(window_id, server_name, cloud, session_id, profile_name)
    }

    pub fn new_for_profile(
        window_id: window::Id,
        server_name: String,
        cloud: CloudHandles,
        session_id: SessionId,
        profile_name: String,
    ) -> Self {
        let mud_host = server::load_server(&server_name)
            .ok()
            .map(|server| server.config.host);
        let advanced_features =
            smudgy_core::models::settings::load_settings().advanced_scripting_features;
        let profile_choices = Self::load_profile_choices(&server_name);
        let mut profile_inventory_complete = profile_choices.is_ok();
        let mut profile_names = profile_choices.unwrap_or_default();
        if profile_names.is_empty() && !profile_name.is_empty() {
            profile_names.push(profile_name.clone());
            profile_inventory_complete = false;
        }
        Self {
            window_id,
            server_name,
            cloud,
            session_id,
            profile_name: profile_name.clone(),
            session_binding_live: true,
            parameter_profile: profile_name,
            account_epoch: 0,
            confirm_global_parameter_source: false,
            copy_settings_prompt: None,
            profile_names,
            profile_inventory_complete,
            mud_host,
            advanced_features,
            scripts: BTreeMap::new(),
            packages: PackageTree::new(),
            automation_snapshot: None,
            folder_state_error: None,
            modules: Vec::new(),
            module_settings: ModuleSettings::default(),
            module_state_error: None,
            local_packages: Vec::new(),
            installed_packages: Vec::new(),
            local_package_state_error: None,
            installed_package_state_error: None,
            live: LiveAutomations::default(),
            expanded_creators: HashSet::new(),
            show_all_creators: HashSet::new(),
            catalogue: None,
            store_toggled: HashSet::new(),
            selection: Selection::Dashboard,
            selection_revision: 0,
            collapsed_folders: HashSet::new(),
            pane: Pane::Dashboard,
            search: String::new(),
            chip: Chip::All,
            new_menu_open: false,
            code_editor: None,
            pointer_over_code_editor: false,
            language_service: None,
            language_project_context: None,
            language_project_target_context: None,
            pending_language_project_refresh: None,
            language_source_ids: HashMap::new(),
            code_editor_mount_generation: 0,
            next_language_graph_generation: 2,
            next_language_request_id: 1,
            next_code_disk_revision: 1,
            module_source_baseline: None,
            hotkey_text_content: text_editor::Content::new(),
            send_text_content: text_editor::Content::new(),
            action_text_pinned: false,
            action_script_pinned: false,
            action_script_lang: ScriptLang::JS,
            alias_pattern_content: text_editor::Content::new(),
            alias_regex_content: text_editor::Content::new(),
            trigger_row_contents: Vec::new(),
            hotkey_state: Vec::new(),
            alias_draft: model::AliasMatcherDraft::default(),
            order_revealed: false,
            try_it_open: false,
            parsing_open: false,
            parsing_cursor: 0,
            test_input: String::new(),
            dirty: false,
            pending_nav: None,
            pending_nav_revision: 0,
            confirm_folder_delete: false,
            graph: PackageGraph::default(),
            blocked_updates: HashSet::new(),
            graph_seq: GraphSeq::default(),
            local_package: None,
            local_readme: None,
            owned_selected_file: None,
            owned_source_baseline: None,
            rename_buffer: None,
            rename_source_name: None,
            manifest_draft: None,
            manifest_source_baseline: None,
            manifest_dirty: false,
            manifest_editing: false,
            manifest_operation: None,
            manifest_tab: ManifestTab::default(),
            authoring_busy: false,
            authoring_operation: None,
            authoring_feedback: None,
            publish_output: None,
            confirm_delete_local: false,
            share_package_id: None,
            publication_status: PublicationStatus::Unknown,
            share_is_public: false,
            share_friends: Vec::new(),
            share_grants: Vec::new(),
            share_versions: Vec::new(),
            share_busy: false,
            share_operation: None,
            share_seq: ShareSeq::default(),
            share_feedback: None,
            installed_open: None,
            installed_detail: None,
            installed_readme: InstalledReadmeState::Loaded(None),
            installed_rating: None,
            installed_versions: Vec::new(),
            installed_selected_file: None,
            installed_package_tab: InstalledPackageTab::default(),
            local_package_tab: LocalPackageTab::default(),
            installed_source: HashMap::new(),
            manage_busy: false,
            manage_feedback: None,
            fork_name: None,
            fork_source_specifier: None,
            fork_operation: None,
            confirm_uninstall: false,
            uninstall_expected_lock: None,
            uninstall_orphans: Vec::new(),
            uninstall_breaks: Vec::new(),
            confirm_trust: false,
            update_delta: None,
            package_change_finalize: None,
            discover_query: String::new(),
            discover_scope: DiscoverScope::default(),
            discover_results: Vec::new(),
            featured_packages: Vec::new(),
            discover_owner: None,
            discover_requested_package: None,
            discover_detail: None,
            discover_readme: None,
            discover_comments: Vec::new(),
            discover_comment_input: String::new(),
            discover_busy: false,
            discover_error: None,
            discover_seq: DiscoverSeq::default(),
            discover_search_seq: DiscoverSearchSeq::default(),
            consent_prompt: None,
            consent_busy: false,
            install_seq: InstallSeq::default(),
            detail_seq: DetailSeq::default(),
            param_prompt: None,
            param_prompt_queue: Vec::new(),
            param_config: None,
            profile_param_status: None,
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

    /// The live session whose runtime streams this window follows. The window remains useful as an
    /// offline disk editor if that session later closes.
    #[must_use]
    pub fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Stop subscriptions that are scoped to `session_id` when the daemon removes that session.
    /// A different session closing has no effect on this window's exact binding.
    pub fn retire_session_binding(&mut self, session_id: SessionId) {
        if self.session_id == session_id {
            self.session_binding_live = false;
        }
    }

    /// Whether a test navigation would abandon a draft.
    #[cfg(test)]
    #[must_use]
    fn has_unsaved_changes(&self) -> bool {
        self.has_unsaved_draft()
    }

    #[must_use]
    fn has_content_draft(&self) -> bool {
        self.dirty
            || self.manifest_dirty
            || self
                .param_config
                .as_ref()
                .is_some_and(|config| !config.touched.is_empty())
    }

    #[must_use]
    fn has_unsaved_draft(&self) -> bool {
        self.has_content_draft() || self.rename_buffer.is_some() || self.fork_name.is_some()
    }

    /// A repeated request for this window's current session supersedes a previously queued switch.
    pub fn cancel_pending_context_switch(&mut self) {
        if self.pending_context_switch() {
            self.pending_nav = None;
            self.pending_nav_revision = self.pending_nav_revision.wrapping_add(1);
        }
    }

    #[must_use]
    fn pending_context_switch(&self) -> bool {
        self.pending_nav
            .as_deref()
            .is_some_and(|message| matches!(message, Message::SwitchContext { .. }))
    }

    #[must_use]
    fn pending_close(&self) -> bool {
        self.pending_nav
            .as_deref()
            .is_some_and(|message| matches!(message, Message::RequestClose))
    }

    /// Ctrl/⌘+P opens the palette; arrows/enter/escape drive it while open.
    /// Navigation keys only act on events no focused widget captured, so they
    /// don't fight text inputs elsewhere.
    pub fn subscription(&self) -> Subscription<Message> {
        let keyboard = iced::event::listen_with(|event, status, event_window| {
            let (key, modifiers) = match event {
                IcedEvent::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
                    (key, modifiers)
                }
                IcedEvent::Mouse(iced::mouse::Event::ButtonPressed(_)) => {
                    return Some(Message::PointerPressed(event_window));
                }
                _ => return None,
            };
            match (key.as_ref(), status) {
                _ if code_completion_shortcut(&key, modifiers, status) => {
                    Some(Message::TriggerCodeCompletion)
                }
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
                (keyboard::Key::Named(Named::Tab), status) => {
                    tab_traversal(modifiers, status).map(|backwards| {
                        if backwards {
                            Message::FocusPrevious(event_window)
                        } else {
                            Message::FocusNext(event_window)
                        }
                    })
                }
                _ => None,
            }
        });
        let mut subscriptions = vec![keyboard];
        if self.session_binding_live {
            // Stream this session's script-created automation updates, keyed by session id so
            // iced keeps a single broadcast subscription (one runtime receiver) across renders.
            subscriptions.push(
                Subscription::run_with(self.session_id, |session_id| {
                    automation_stream(*session_id)
                })
                .map(Message::AutomationEvent),
            );
            // The catalogue broadcast is subscribed only while the store pane is showing: the
            // runtime builds snapshots only while receivers exist, so a closed pane costs it
            // nothing, and re-opening gets a fresh snapshot (the new-subscriber resync).
            if matches!(self.pane, Pane::StoreInspector) {
                subscriptions.push(
                    Subscription::run_with(self.session_id, |session_id| {
                        catalogue_stream(*session_id)
                    })
                    .map(Message::CatalogueEvent),
                );
            }
        }
        if self.language_service.is_some() {
            subscriptions.push(
                iced::time::every(Duration::from_millis(50)).map(|_| Message::PollLanguageService),
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
        // Message tracing for GUI debugging: run with
        // `SMUDGY_LOG=smudgy_ui::windows::automations_window=trace` to watch
        // every message this window handles.
        log::trace!("{message:?}");
        // Drafts guard all navigation that can unmount them.
        let navigation_needs_guard =
            Self::is_guarded_navigation(&message) && self.has_unsaved_draft();
        if navigation_needs_guard {
            // Once the app has asked this singleton to switch session context (or close), ordinary
            // sidebar clicks must not replace that request. Otherwise cancelling the new ordinary
            // navigation cannot clear the app-level switch target, and a later switch message can
            // unexpectedly replace the window after the user chose to keep editing.
            let pending_is_terminal = self.pending_nav.as_deref().is_some_and(|pending| {
                matches!(
                    pending,
                    Message::SwitchContext { .. } | Message::RequestClose
                )
            });
            let incoming_is_terminal = matches!(
                &message,
                Message::SwitchContext { .. } | Message::RequestClose
            );
            if pending_is_terminal && !incoming_is_terminal {
                return Update::none();
            }
            self.pending_nav_revision = self.pending_nav_revision.wrapping_add(1);
            self.pending_nav = Some(Box::new(message));
            return Update::none();
        }
        if Self::is_edit_message(&message) {
            self.dirty = true;
        }
        let refresh_generated = Self::affects_captures(&message);
        let values_written = Self::writes_parameter_values(&message);
        let mut update = match message {
            Message::RequestClose => Update::with_event(Event::CloseRequested),
            Message::NavigationReleased(released) => Update::with_event(match released {
                ReleasedNavigation::Close => Event::CloseCancelled,
                ReleasedNavigation::ContextSwitch => Event::ContextSwitchCancelled,
            }),
            Message::SwitchContext {
                server_name,
                session_id,
                profile_name,
            } => Update::with_event(Event::SwitchContext {
                server_name,
                session_id,
                profile_name,
            }),
            // -------- loading ----------------------------------------------
            Message::AccountChanged => self.account_changed(),
            Message::ScriptsLoaded {
                scripts,
                load,
                errors,
            } => {
                if let Some(snapshot) = load {
                    self.packages = snapshot.packages.clone();
                    self.automation_snapshot = Some(snapshot);
                    self.folder_state_error = None;
                    self.scripts = scripts;
                    self.merge_folders();
                    if self.language_project_context_matches(
                        &code_editor::LanguageProjectContext::Inline,
                    ) {
                        self.language_project_target_context =
                            Some(code_editor::LanguageProjectContext::Inline);
                        self.refresh_language_project();
                    }
                } else {
                    let error = errors.join("\n");
                    self.folder_state_error = Some(error);
                }
                if errors.is_empty() {
                    Update::none()
                } else {
                    self.pane = Pane::Error(errors);
                    Update::none()
                }
            }
            Message::LoadFolders => {
                // The failure is already logged and reflected in `profile_inventory_complete`;
                // the tree renders the stale inventory with its warning until the next reload.
                let _ = self.refresh_profile_inventory();
                if !self.profile_names.contains(&self.parameter_profile) {
                    self.parameter_profile.clone_from(&self.profile_name);
                }
                self.merge_folders();
                Update::none()
            }
            Message::LoadModules => {
                match smudgy_core::models::modules::load_module_state(&self.server_name) {
                    Ok((modules, settings)) => {
                        self.modules = modules;
                        self.module_settings = settings;
                        self.module_state_error = None;
                    }
                    Err(error) => {
                        log::warn!(
                            "Failed to load module state for {}: {error}",
                            self.server_name
                        );
                        self.module_state_error = Some(error.to_string());
                    }
                }
                Update::with_task(self.reconcile_module_language_project_reload())
            }
            Message::LoadLocalPackages => self.reload_local_package_state(),
            Message::LoadInstalledPackages => self.reload_installed_package_state(),
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

            // -------- keyboard focus traversal ----------------------------
            Message::FocusColorControl(id) => Update::with_task(operation::focus(id)),
            Message::FocusNext(event_window) if event_window == self.window_id => {
                self.release_code_editor_focus();
                Update::with_task(operation::focus_next())
            }
            Message::FocusPrevious(event_window) if event_window == self.window_id => {
                self.release_code_editor_focus();
                Update::with_task(operation::focus_previous())
            }
            Message::FocusNext(_) | Message::FocusPrevious(_) => Update::none(),
            Message::PointerPressed(event_window) => {
                if event_window == self.window_id && !self.pointer_over_code_editor {
                    self.release_code_editor_focus();
                }
                Update::none()
            }
            Message::CodeEditorPointerEntered => {
                self.pointer_over_code_editor = true;
                Update::none()
            }
            Message::CodeEditorPointerExited => {
                self.pointer_over_code_editor = false;
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
                    // The name IS the command: editing it drops any stored
                    // command-word override a legacy save carried, so the
                    // command follows the name from here on.
                    if matches!(state.node, EditNode::Alias(_)) {
                        self.alias_draft.command_override = None;
                    }
                }
                Update::none()
            }
            Message::AliasRegexAction(action) => {
                // The Regex kind's source buffer; `pattern` on the definition is
                // written from the draft at save time.
                editors::perform_single_line(&mut self.alias_regex_content, action);
                self.alias_draft.regex_source =
                    editors::single_line_text(&self.alias_regex_content);
                Update::none()
            }
            Message::SetAliasKind(kind) => {
                self.alias_draft.kind = kind;
                Update::none()
            }
            Message::SetArgName(i, name) => {
                if let Some(arg) = self.alias_draft.args.get_mut(i) {
                    arg.name = name;
                }
                Update::none()
            }
            Message::SetArgKind(i, kind) => {
                if let Some(arg) = self.alias_draft.args.get_mut(i) {
                    arg.kind = kind;
                    self.alias_draft.normalize_args();
                }
                Update::none()
            }
            Message::AddArg => {
                self.alias_draft
                    .args
                    .push(smudgy_core::models::matchers::ArgSpec {
                        name: format!("arg{}", self.alias_draft.args.len() + 1),
                        kind: smudgy_core::models::matchers::ArgKind::Required,
                    });
                self.alias_draft.normalize_args();
                Update::none()
            }
            Message::RemoveArg(i) => {
                if i < self.alias_draft.args.len() {
                    self.alias_draft.args.remove(i);
                    self.alias_draft.normalize_args();
                }
                Update::none()
            }
            Message::SetCmdMode(mode) => {
                self.alias_draft.cmd_mode = mode;
                self.alias_draft.normalize_args();
                Update::none()
            }
            Message::SetParseMode(parse) => {
                self.alias_draft.parse = parse;
                self.parsing_open = false;
                Update::none()
            }
            Message::OpenParsingPicker => {
                self.parsing_open = true;
                // The cursor starts on the current choice.
                self.parsing_cursor = model::ParseModeChoice::ALL
                    .iter()
                    .position(|choice| choice.0 == self.alias_draft.parse)
                    .unwrap_or(0);
                Update::none()
            }
            Message::CloseParsingPicker => {
                self.parsing_open = false;
                Update::none()
            }
            Message::MoveParsingCursor(delta) => {
                let len = model::ParseModeChoice::ALL.len() as i32;
                let cursor = self.parsing_cursor as i32 + delta;
                self.parsing_cursor = cursor.rem_euclid(len) as usize;
                Update::none()
            }
            Message::AliasPatternAction(action) => {
                editors::perform_single_line(&mut self.alias_pattern_content, action);
                self.alias_draft.pattern_source =
                    editors::single_line_text(&self.alias_pattern_content);
                Update::none()
            }
            Message::ToggleAnchorStart => {
                self.alias_draft.anchor_start = !self.alias_draft.anchor_start;
                Update::none()
            }
            Message::ToggleAnchorEnd => {
                self.alias_draft.anchor_end = !self.alias_draft.anchor_end;
                Update::none()
            }
            Message::TogglePrompt => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { prompt, .. },
                    ..
                }) = &mut self.pane
                {
                    *prompt = !*prompt;
                }
                Update::none()
            }
            Message::RevealOrder => {
                self.order_revealed = true;
                Update::none()
            }
            Message::HideOrder => {
                self.order_revealed = false;
                Update::none()
            }
            Message::InsertReference(reference) => {
                // The badge inserts into whichever action tab is active.
                if self.open_action_language() == Some(ScriptLang::Plaintext) {
                    self.action_text_pinned = true;
                    self.send_text_content.perform(text_editor::Action::Edit(
                        text_editor::Edit::Paste(Arc::new(reference)),
                    ));
                    Update::none()
                } else {
                    self.action_script_pinned = true;
                    let Some(message) = self
                        .bind_code_editor_message(code_editor::IcedEditorMessage::Paste(reference))
                    else {
                        return Update::none();
                    };
                    let (task, _) = self.update_code_editor(&message);
                    Update::with_task(task)
                }
            }
            Message::SetScriptFolder(folder) => self.set_script_folder(folder),
            Message::SetBehavior(language) => {
                let previous = match &self.pane {
                    Pane::Editor(EditorState { node, .. }) => match node {
                        EditNode::Alias(alias) => {
                            Some((alias.language, code_editor::CodeDocument::Alias))
                        }
                        EditNode::Hotkey(hotkey) => {
                            Some((hotkey.language, code_editor::CodeDocument::Hotkey))
                        }
                        EditNode::Trigger { language, .. } => {
                            Some((*language, code_editor::CodeDocument::Trigger))
                        }
                    },
                    _ => None,
                };
                if let Pane::Editor(state) = &mut self.pane {
                    match &mut state.node {
                        EditNode::Alias(a) => a.language = language,
                        EditNode::Hotkey(h) => h.language = language,
                        EditNode::Trigger { language: l, .. } => *l = language,
                    }
                }
                let Some((previous, kind)) = previous else {
                    return Update::none();
                };
                if kind == code_editor::CodeDocument::Hotkey && language != ScriptLang::Plaintext {
                    self.action_script_lang = language;
                }
                if previous == language {
                    Update::none()
                } else if kind == code_editor::CodeDocument::Hotkey {
                    if previous == ScriptLang::Plaintext {
                        let text = self.hotkey_text_content.text();
                        Update::with_task(self.bind_code_editor(
                            &text,
                            code_editor::script_language(language),
                            kind,
                        ))
                    } else if language == ScriptLang::Plaintext {
                        // A hotkey has one body regardless of execution language.
                        // Transfer the authoritative code buffer back before the
                        // plaintext editor becomes visible.
                        let text = self.code_editor_text();
                        self.hotkey_text_content = text_editor::Content::with_text(&text);
                        self.clear_code_editor();
                        Update::none()
                    } else {
                        let text = self.code_editor_text();
                        Update::with_task(self.bind_code_editor(
                            &text,
                            code_editor::script_language(language),
                            kind,
                        ))
                    }
                } else if language != ScriptLang::Plaintext {
                    let text = self.code_editor_text();
                    self.action_script_lang = language;
                    Update::with_task(self.bind_code_editor(
                        &text,
                        code_editor::script_language(language),
                        kind,
                    ))
                } else {
                    Update::none()
                }
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
            Message::ToggleAllowSelfMatch => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Alias(alias),
                    ..
                }) = &mut self.pane
                {
                    alias.allow_self_match = !alias.allow_self_match;
                }
                Update::none()
            }
            Message::CodeEditorAction(action)
                if matches!(
                    action.message,
                    code_editor::IcedEditorMessage::WriteRequested
                ) =>
            {
                if !self.code_editor_message_is_current(&action) {
                    return Update::none();
                }
                match &self.pane {
                    Pane::Editor(_) => self.save_open(),
                    Pane::Module(_) => self.save_module(),
                    Pane::OwnedPackage => self.save_owned_file(),
                    _ => Update::none(),
                }
            }
            Message::CodeEditorAction(action) => {
                let (task, changed) = self.update_code_editor(&action);
                if changed {
                    self.dirty = true;
                    if matches!(self.pane, Pane::Editor(_)) {
                        self.action_script_pinned = true;
                    }
                }
                Update::with_task(task)
            }
            Message::ApplyCodeCompletion(selection) => {
                let (task, changed) = self.apply_code_completion(selection);
                if changed {
                    self.dirty = true;
                    if matches!(self.pane, Pane::Editor(_)) {
                        self.action_script_pinned = true;
                    }
                }
                Update::with_task(task)
            }
            Message::CodeCompletionViewportChanged(target) => {
                self.code_completion_viewport_changed(target);
                Update::none()
            }
            Message::TriggerCodeCompletion => {
                let Some(message) = self.bind_code_editor_message(
                    code_editor::IcedCodeEditorSurface::explicit_completion_message(),
                ) else {
                    return Update::none();
                };
                let (task, _) = self.update_code_editor(&message);
                Update::with_task(task)
            }
            Message::DismissCodeOverlays => {
                if let Some(editor) = &mut self.code_editor {
                    editor.dismiss_pointer_overlays();
                }
                Update::none()
            }
            Message::CodeHoverOverlayEntered(target) => {
                self.code_hover_overlay_entered(target);
                Update::none()
            }
            Message::CodeHoverOverlayExited(target) => {
                self.code_hover_overlay_exited(target);
                Update::none()
            }
            Message::CodeHoverLinkPressed(target, _uri) => {
                // Hover documentation is rich but inert: scripts cannot turn JSDoc
                // into navigation or external-open side effects.
                self.code_hover_overlay_entered(target);
                Update::none()
            }
            Message::CodeSignatureLinkPressed(target, _uri) => {
                // Signature documentation uses the same rich, inert link policy
                // as hover documentation.
                self.code_signature_link_pressed(target);
                Update::none()
            }
            Message::NavigateCodeDefinition(navigation) => {
                self.navigate_code_definition(navigation)
            }
            Message::HotkeyTextAction(action) => {
                self.hotkey_text_content.perform(action);
                Update::none()
            }
            Message::SendTextAction(action) => {
                if action.is_edit() {
                    self.action_text_pinned = true;
                }
                self.send_text_content.perform(action);
                Update::none()
            }
            Message::ToggleTryIt => {
                self.try_it_open = !self.try_it_open;
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
                    // "Another" means another of what you have: the new row
                    // copies the last Match row's syntax, defaulting to the
                    // Simple pattern for the first one.
                    let syntax = rows
                        .iter()
                        .rev()
                        .find(|row| row.role == PatternKind::Match)
                        .map_or(
                            smudgy_core::models::matchers::MatcherSyntax::Pattern,
                            |row| row.syntax,
                        );
                    rows.push(model::TriggerRow {
                        syntax,
                        ..model::TriggerRow::new(PatternKind::Match)
                    });
                    self.trigger_row_contents.push(text_editor::Content::new());
                }
                Update::none()
            }
            Message::AddExceptionRow => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                {
                    rows.push(model::TriggerRow {
                        syntax: smudgy_core::models::matchers::MatcherSyntax::Pattern,
                        ..model::TriggerRow::new(PatternKind::Anti)
                    });
                    self.trigger_row_contents.push(text_editor::Content::new());
                }
                Update::none()
            }
            Message::AddRawRow => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                {
                    rows.push(model::TriggerRow::new(PatternKind::Raw));
                    self.trigger_row_contents.push(text_editor::Content::new());
                }
                Update::none()
            }
            Message::SetTriggerCard(card) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                {
                    let (syntax, role) = card.shape();
                    let matcher_indexes: Vec<usize> = rows
                        .iter()
                        .enumerate()
                        .filter(|(_, row)| row.role != PatternKind::Anti)
                        .map(|(i, _)| i)
                        .collect();
                    match matcher_indexes[..] {
                        [] => {
                            rows.push(model::TriggerRow {
                                syntax,
                                ..model::TriggerRow::new(role)
                            });
                            self.trigger_row_contents.push(text_editor::Content::new());
                        }
                        [index] => {
                            if let Some(row) = rows.get_mut(index) {
                                row.syntax = syntax;
                                row.role = role;
                                if role == PatternKind::Raw {
                                    row.color = None;
                                }
                            }
                        }
                        // The cards are not shown at two or more matchers.
                        _ => {}
                    }
                }
                Update::none()
            }
            Message::MoveRowUp(i) => {
                self.move_trigger_row(i, false);
                Update::none()
            }
            Message::MoveRowDown(i) => {
                self.move_trigger_row(i, true);
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
                    if i < self.trigger_row_contents.len() {
                        self.trigger_row_contents.remove(i);
                    }
                }
                Update::none()
            }
            Message::RowSourceAction(i, action) => {
                if let Some(content) = self.trigger_row_contents.get_mut(i) {
                    editors::perform_single_line(content, action);
                    let source = editors::single_line_text(content);
                    if let Pane::Editor(EditorState {
                        node: EditNode::Trigger { rows, .. },
                        ..
                    }) = &mut self.pane
                        && let Some(row) = rows.get_mut(i)
                    {
                        row.source = source;
                    }
                }
                Update::none()
            }
            Message::SetRowSyntax(i, syntax) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    row.syntax = syntax;
                    // Raw implies Regex: choosing Pattern demotes the role.
                    if syntax == smudgy_core::models::matchers::MatcherSyntax::Pattern
                        && row.role == PatternKind::Raw
                    {
                        row.role = PatternKind::Match;
                    }
                }
                Update::none()
            }
            Message::ToggleRowAnchorStart(i) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    row.anchor_start = !row.anchor_start;
                }
                Update::none()
            }
            Message::ToggleRowAnchorEnd(i) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    row.anchor_end = !row.anchor_end;
                }
                Update::none()
            }
            Message::ToggleRowColor(i, enabled) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                    && row.role != PatternKind::Raw
                {
                    row.set_color_enabled(enabled);
                }
                Update::none()
            }
            Message::SelectRowColorChannel(i, channel) => {
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    row.color_channel = channel;
                }
                Update::none()
            }
            Message::SelectRowColorKind(i, kind) => {
                use smudgy_core::models::matchers::{MatcherColor, MatcherColorChannel};
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    let channel = row.color_channel;
                    let draft = row.color_draft(channel);
                    let [r, g, b] = draft.exact_truecolor.last_valid;
                    let range = draft.color_range_last_valid;
                    if let Some(filter) = row.color.as_mut() {
                        let slot = match channel {
                            MatcherColorChannel::Foreground => &mut filter.foreground,
                            MatcherColorChannel::Background => &mut filter.background,
                        };
                        if model::MatcherColorKind::of(*slot) != kind {
                            *slot = match kind {
                                model::MatcherColorKind::Any => None,
                                model::MatcherColorKind::Ansi => {
                                    Some(MatcherColor::Ansi { index: 7 })
                                }
                                model::MatcherColorKind::Xterm => {
                                    Some(MatcherColor::Xterm { index: 7 })
                                }
                                model::MatcherColorKind::Truecolor => {
                                    Some(MatcherColor::Truecolor {
                                        r,
                                        g,
                                        b,
                                        range: None,
                                    })
                                }
                                model::MatcherColorKind::ColorRange => {
                                    Some(matcher_truecolor_from_range(range))
                                }
                            };
                        }
                    }
                }
                Update::none()
            }
            Message::SetRowAnsiColor(i, index) => {
                self.set_row_matcher_color(
                    i,
                    smudgy_core::models::matchers::MatcherColor::Ansi {
                        index: index.min(15),
                    },
                );
                Update::none()
            }
            Message::SetRowXtermColor(i, index) => {
                self.set_row_matcher_color(
                    i,
                    smudgy_core::models::matchers::MatcherColor::Xterm { index },
                );
                Update::none()
            }
            Message::SetRowColorRange(i, endpoint, message) => {
                let mut range = self
                    .trigger_row(i)
                    .map(|row| row.color_draft(row.color_channel).color_range_last_valid)
                    .unwrap_or_else(|| {
                        let point =
                            smudgy_core::models::matchers::MatcherHsv::from_rgb(255, 255, 255);
                        smudgy_core::models::matchers::MatcherHsvRange::from_to(point, point)
                    });
                let (mut from, mut to) = range.directed_endpoints();
                let initial = match endpoint {
                    model::ColorRangeEndpoint::First => from,
                    model::ColorRangeEndpoint::Second => to,
                };
                let mut picker = crate::components::color_picker::ColorPicker::from_hsv(
                    matcher_hsv_to_picker(initial),
                );
                let _ = picker.update(message);
                let hsv = picker_hsv_to_matcher(picker.hsv());
                match endpoint {
                    model::ColorRangeEndpoint::First => from = hsv,
                    model::ColorRangeEndpoint::Second => to = hsv,
                }
                range = smudgy_core::models::matchers::MatcherHsvRange::from_to(from, to);
                let color = matcher_truecolor_from_range(range);
                range = matcher_truecolor_range(color).unwrap_or(range);
                self.set_row_matcher_color(i, color);
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    let channel = row.color_channel;
                    let draft = row.color_draft_mut(channel);
                    draft.color_range_last_valid = range;
                    let (from, to) = range.directed_endpoints();
                    draft.color_range_hex[endpoint.index()] = matcher_hsv_hex(match endpoint {
                        model::ColorRangeEndpoint::First => from,
                        model::ColorRangeEndpoint::Second => to,
                    });
                }
                Update::none()
            }
            Message::SetRowColorRangeHex(i, endpoint, value) => {
                let parsed = model::parse_matcher_hex(&value);
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    let channel = row.color_channel;
                    row.color_draft_mut(channel).color_range_hex[endpoint.index()] = value;
                }
                if let Some((r, g, b)) = parsed {
                    let mut range = self
                        .trigger_row(i)
                        .map(|row| row.color_draft(row.color_channel).color_range_last_valid)
                        .unwrap_or_else(|| {
                            let point =
                                smudgy_core::models::matchers::MatcherHsv::from_rgb(255, 255, 255);
                            smudgy_core::models::matchers::MatcherHsvRange::from_to(point, point)
                        });
                    let (mut from, mut to) = range.directed_endpoints();
                    let old_hue = match endpoint {
                        model::ColorRangeEndpoint::First => from.hue,
                        model::ColorRangeEndpoint::Second => to.hue,
                    };
                    let mut hsv = smudgy_core::models::matchers::MatcherHsv::from_rgb(r, g, b);
                    if hsv.saturation == 0 {
                        hsv.hue = old_hue;
                    }
                    match endpoint {
                        model::ColorRangeEndpoint::First => from = hsv,
                        model::ColorRangeEndpoint::Second => to = hsv,
                    }
                    range = smudgy_core::models::matchers::MatcherHsvRange::from_to(from, to);
                    let color = matcher_truecolor_from_range(range);
                    range = matcher_truecolor_range(color).unwrap_or(range);
                    self.set_row_matcher_color(i, color);
                    if let Pane::Editor(EditorState {
                        node: EditNode::Trigger { rows, .. },
                        ..
                    }) = &mut self.pane
                        && let Some(row) = rows.get_mut(i)
                    {
                        let channel = row.color_channel;
                        row.color_draft_mut(channel).color_range_last_valid = range;
                    }
                }
                Update::none()
            }
            Message::SetRowExactTruecolorHex(i, value) => {
                let parsed = model::parse_matcher_hex(&value);
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    let channel = row.color_channel;
                    let draft = &mut row.color_draft_mut(channel).exact_truecolor;
                    draft.hex = value;
                    if let Some((r, g, b)) = parsed {
                        draft.rgb = [r.to_string(), g.to_string(), b.to_string()];
                        draft.last_valid = [r, g, b];
                    }
                }
                if let Some((r, g, b)) = parsed {
                    self.set_row_matcher_color(
                        i,
                        smudgy_core::models::matchers::MatcherColor::Truecolor {
                            r,
                            g,
                            b,
                            range: None,
                        },
                    );
                }
                Update::none()
            }
            Message::SetRowExactTruecolorRgb(i, component, value) => {
                let parsed = if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(row) = rows.get_mut(i)
                {
                    let channel = row.color_channel;
                    let draft = &mut row.color_draft_mut(channel).exact_truecolor;
                    draft.rgb[component.index()] = value;
                    let [red, green, blue] = &draft.rgb;
                    let parsed = red
                        .parse::<u8>()
                        .ok()
                        .zip(green.parse::<u8>().ok())
                        .zip(blue.parse::<u8>().ok())
                        .map(|((r, g), b)| (r, g, b));
                    if let Some((r, g, b)) = parsed {
                        draft.hex = format!("#{r:02x}{g:02x}{b:02x}");
                        draft.last_valid = [r, g, b];
                    }
                    parsed
                } else {
                    None
                };
                if let Some((r, g, b)) = parsed {
                    self.set_row_matcher_color(
                        i,
                        smudgy_core::models::matchers::MatcherColor::Truecolor {
                            r,
                            g,
                            b,
                            range: None,
                        },
                    );
                }
                Update::none()
            }
            Message::ToggleRowColorAttribute(i, attribute, selected) => {
                use smudgy_core::models::matchers::MatcherTextAttribute;
                if let Pane::Editor(EditorState {
                    node: EditNode::Trigger { rows, .. },
                    ..
                }) = &mut self.pane
                    && let Some(filter) = rows.get_mut(i).and_then(|row| row.color.as_mut())
                {
                    filter.attributes.retain(|current| *current != attribute);
                    if selected {
                        let incompatible = match attribute {
                            MatcherTextAttribute::Bold => Some(MatcherTextAttribute::Faint),
                            MatcherTextAttribute::Faint => Some(MatcherTextAttribute::Bold),
                            MatcherTextAttribute::Underline => {
                                Some(MatcherTextAttribute::DoubleUnderline)
                            }
                            MatcherTextAttribute::DoubleUnderline => {
                                Some(MatcherTextAttribute::Underline)
                            }
                            MatcherTextAttribute::SlowBlink => {
                                Some(MatcherTextAttribute::FastBlink)
                            }
                            MatcherTextAttribute::FastBlink => {
                                Some(MatcherTextAttribute::SlowBlink)
                            }
                            _ => None,
                        };
                        if let Some(incompatible) = incompatible {
                            filter.attributes.retain(|current| *current != incompatible);
                        }
                        filter.attributes.push(attribute);
                    }
                }
                Update::none()
            }

            // -------- save bar ---------------------------------------------
            Message::Save => self.save_open(),
            Message::Discard => {
                self.dirty = false;
                let released = self.release_pending_navigation();
                self.clear_selection();
                self.selection = Selection::Dashboard;
                self.pane = Pane::Dashboard;
                Update::with_task(released)
            }
            Message::Delete => self.delete_open(),
            Message::ConfirmDiscardNavRevision(revision) => {
                self.confirm_pending_navigation(revision)
            }
            Message::CancelDiscardNavRevision(revision) => self.cancel_pending_navigation(revision),

            // -------- folder -----------------------------------------------
            Message::SetFolderPath(value) => {
                if let Pane::Folder(state) = &mut self.pane {
                    state.path = value;
                    // The error described the previous path; it also locks the activation
                    // controls, so a fresh attempt must not stay blocked by it.
                    state.error = None;
                }
                Update::none()
            }
            Message::EnableEverywhere => self.set_open_activation(ProfileActivation::All),
            Message::DisableEverywhere => self.set_open_activation(ProfileActivation::None),
            Message::ToggleActivationProfile(profile_name) => {
                if self.profile_inventory_complete {
                    self.toggle_open_activation_profile(profile_name)
                } else {
                    Update::none()
                }
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
                let is_module = if let Pane::Module(state) = &mut self.pane {
                    state.name.clone_from(&value);
                    if !state.activation_touched {
                        state.activation = smudgy_core::models::modules::default_activation(&value);
                    }
                    true
                } else {
                    false
                };
                if !is_module {
                    return Update::none();
                }
                let language = code_editor::path_language(&value);
                if self
                    .code_editor
                    .as_ref()
                    .is_some_and(|editor| editor.document().language == language)
                {
                    Update::none()
                } else {
                    let text = self.code_editor_text();
                    Update::with_task(self.bind_code_editor(
                        &text,
                        language,
                        code_editor::CodeDocument::StandaloneModule,
                    ))
                }
            }
            Message::CreateModule => self.create_module(),
            Message::SelectModuleTab(tab) => {
                if let Pane::Module(state) = &mut self.pane {
                    state.tab = tab;
                }
                Update::none()
            }

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
            Message::PublishFinished {
                server_name,
                name,
                operation_id,
                completion,
                credential_generation,
                publisher_id,
                result,
            } => {
                completion.release();
                if self.server_name != server_name {
                    return Update::none();
                }
                if self.authoring_operation != Some(operation_id) {
                    // The durable workflow belongs to an older pane operation. It can still have
                    // changed the local publication binding before its result was lost or delayed.
                    // Reconcile disk/runtime state, but never clear or repaint the newer command.
                    return Update::new(
                        Task::batch([
                            Task::done(Message::LoadLocalPackages),
                            Task::done(Message::LoadInstalledPackages),
                            Task::done(Message::RefreshOwnedShareIfOpen {
                                server_name: server_name.clone(),
                                name,
                            }),
                        ]),
                        Some(Event::ScriptsChanged { server_name }),
                    );
                }
                self.authoring_operation = None;
                self.authoring_busy = false;
                let same_account = self.cloud.credentials.generation() == credential_generation
                    && self
                        .cloud
                        .snapshot
                        .get()
                        .profile
                        .as_ref()
                        .is_some_and(|profile| profile.id == publisher_id);
                let is_open_package = matches!(
                    &self.selection,
                    Selection::OwnedPackage(open) if open == &name
                );
                let update: Update<Message, Event> = match result {
                    Ok(summary) => {
                        if is_open_package {
                            self.publication_status = PublicationStatus::Bound(summary.package_id);
                            if same_account {
                                self.share_package_id = Some(summary.package_id);
                                self.share_is_public = summary.is_public;
                                if !self
                                    .share_versions
                                    .iter()
                                    .any(|version| version.version == summary.version)
                                {
                                    self.share_versions.insert(
                                        0,
                                        VersionListItem {
                                            version: summary.version.clone(),
                                            yanked: false,
                                            deleted: false,
                                            published_at: summary.published_at,
                                        },
                                    );
                                }
                            } else {
                                // The frozen workflow completed for the account that launched it.
                                // Keep the durable rename lock, but do not expose that account's
                                // sharing controls under the newly active credential.
                                self.share_package_id = None;
                                self.share_is_public = false;
                                self.share_versions.clear();
                                self.share_grants.clear();
                            }
                        }
                        let mut feedback = format!(
                            "smudgy> publish {name}\n{}",
                            crate::i18n::t!(
                                "automation-published",
                                "version" => &summary.version
                            )
                        );
                        if summary.typings_generated > 0 {
                            let typings_generated =
                                i64::try_from(summary.typings_generated).unwrap_or(i64::MAX);
                            feedback.push('\n');
                            feedback.push_str(&crate::i18n::t!(
                                "automation-published-typings",
                                "count" => typings_generated
                            ));
                        }
                        // Surface tsc warnings to the author — typings are best-effort, so a
                        // warning here never means the publish failed.
                        if !summary.typings_warnings.is_empty() {
                            feedback.push('\n');
                            feedback.push_str(&crate::i18n::t!(
                                "automation-typings-warning",
                                "warnings" => summary.typings_warnings.join("\n")
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
                            feedback.push('\n');
                            feedback.push_str(&crate::i18n::t!(
                                "automation-locked-dependencies",
                                "dependencies" => locked.join(", ")
                            ));
                        }
                        // A range that excludes a newer published version (the 0.0.x caret footgun):
                        // non-fatal, but the author almost certainly wanted the newer one.
                        if !summary.dependency_warnings.is_empty() {
                            feedback.push_str(&format!(
                                "\n\u{26a0} {}",
                                summary.dependency_warnings.join("\n\u{26a0} ")
                            ));
                        }
                        // Interop-declaration warnings (duplicate/aliased handle exports, a
                        // handle the previous version published that this one drops): a handle
                        // name is the identity consumers import, so these deserve eyes even
                        // though the publish succeeded.
                        if !summary.interop_warnings.is_empty() {
                            feedback.push('\n');
                            feedback.push_str(&crate::i18n::t!(
                                "automation-interop-warning",
                                "warnings" => summary.interop_warnings.join("\n")
                            ));
                        }
                        // The cloud publish is already complete. A local sidecar warning is
                        // therefore informational and must never invite a retry of the immutable
                        // version upload.
                        if !summary.publication_warnings.is_empty() {
                            let warnings = summary
                                .publication_warnings
                                .iter()
                                .map(|warning| match warning {
                                    PublicationWarning::VersionPresentAfterLostResponse {
                                        name,
                                        version,
                                    } => crate::i18n::t!(
                                        "automation-publication-response-lost-warning",
                                        "name" => name,
                                        "version" => version
                                    ),
                                    PublicationWarning::InconsistentResponseRecovered {
                                        name,
                                        version,
                                    } => crate::i18n::t!(
                                        "automation-publication-inconsistent-response-warning",
                                        "name" => name,
                                        "version" => version
                                    ),
                                    PublicationWarning::ExistingVersionRecovered {
                                        name,
                                        version,
                                    } => crate::i18n::t!(
                                        "automation-publication-existing-version-warning",
                                        "name" => name,
                                        "version" => version
                                    ),
                                    PublicationWarning::MissingLocalBinding { name, version } => {
                                        crate::i18n::t!(
                                            "automation-publication-link-missing-warning",
                                            "name" => name,
                                            "version" => version
                                        )
                                    }
                                    PublicationWarning::LocalBindingUnverified {
                                        name,
                                        version,
                                        error,
                                    } => crate::i18n::t!(
                                        "automation-publication-link-unverified-warning",
                                        "name" => name,
                                        "version" => version,
                                        "error" => error
                                    ),
                                    PublicationWarning::LocalSnapshotChanged { name, version } => {
                                        crate::i18n::t!(
                                            "automation-publication-local-changed-warning",
                                            "name" => name,
                                            "version" => version
                                        )
                                    }
                                    PublicationWarning::DescriptionUpdateFailed {
                                        name,
                                        version,
                                        error,
                                    } => crate::i18n::t!(
                                        "automation-publication-description-warning",
                                        "name" => name,
                                        "version" => version,
                                        "error" => error
                                    ),
                                })
                                .collect::<Vec<_>>();
                            feedback.push('\n');
                            feedback.push_str(&crate::i18n::t!(
                                "automation-publication-record-warning",
                                "warnings" => warnings.join("\n")
                            ));
                        }
                        self.publish_output = Some(PublishOutput {
                            package: name.clone(),
                            text: feedback,
                        });
                        // AccountChanged may have attempted this refresh while the publish still
                        // owned the package gate. Once the completion releases it, reload with the
                        // currently active account even when that is not the publishing account.
                        let refresh = if is_open_package && self.signed_in() {
                            self.load_owned_share(name)
                        } else {
                            Task::none()
                        };
                        Update::with_task(Task::batch([
                            refresh,
                            self.show_toast(crate::i18n::t!(
                                "automation-published",
                                "version" => &summary.version
                            )),
                        ]))
                    }
                    Err(e) => {
                        self.publish_output = Some(PublishOutput {
                            package: name.clone(),
                            text: format!(
                                "smudgy> publish {name}\n{}",
                                crate::i18n::t!(
                                    "automation-publish-failed",
                                    "error" => e.to_string()
                                )
                            ),
                        });
                        // Namespace creation/finalize can commit before a response is lost. Always
                        // reconcile server truth after an error so Rename stays locked if the cloud
                        // accepted any irreversible part of the publish.
                        if is_open_package
                            && let Ok(Some(binding)) =
                                smudgy_core::models::local_packages::load_publication_binding(
                                    &server_name,
                                    &name,
                                )
                        {
                            self.publication_status = PublicationStatus::Bound(binding.package_id);
                        }
                        if is_open_package && self.signed_in() {
                            Update::with_task(self.load_owned_share(name))
                        } else {
                            Update::none()
                        }
                    }
                };
                // Publish can durably claim a namespace or save its binding before a later step
                // fails. Refresh both lists and every same-server runtime after all outcomes.
                Update::new(
                    Task::batch([
                        update.task,
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]),
                    Some(Event::ScriptsChanged { server_name }),
                )
            }
            Message::RequestDeleteOwned => {
                if !self.authoring_busy && !self.share_busy {
                    let package_busy = self.local_package.as_deref().is_some_and(|package| {
                        self.cloud
                            .package_operations
                            .is_busy(&self.server_name, &package.name)
                    });
                    if package_busy {
                        self.authoring_feedback =
                            Some(crate::i18n::t!("package-operation-in-progress"));
                    } else {
                        self.confirm_delete_local = true;
                    }
                }
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
            Message::VisibilityUpdated {
                server_name,
                name,
                seq,
                package_id,
                operation_id,
                completion,
                credential_generation,
                result,
            } => {
                completion.release();
                self.visibility_updated(
                    &server_name,
                    &name,
                    seq,
                    package_id,
                    operation_id,
                    credential_generation,
                    result,
                )
            }
            Message::YankVersion { version, yanked } => self.yank_version(version, yanked),
            Message::DeleteVersion(version) => self.delete_version(version),
            Message::VersionsUpdated {
                server_name,
                name,
                seq,
                package_id,
                operation_id,
                completion,
                credential_generation,
                result,
            } => {
                completion.release();
                self.versions_updated(
                    &server_name,
                    &name,
                    seq,
                    package_id,
                    operation_id,
                    credential_generation,
                    result,
                )
            }
            Message::ShareWithFriend(grantee) => self.share_with_friend(grantee),
            Message::GrantsUpdated {
                server_name,
                name,
                seq,
                package_id,
                operation_id,
                completion,
                credential_generation,
                result,
            } => {
                completion.release();
                self.grants_updated(
                    &server_name,
                    &name,
                    seq,
                    package_id,
                    operation_id,
                    credential_generation,
                    result,
                )
            }
            Message::RefreshOwnedShareIfOpen { server_name, name } => {
                self.refresh_owned_share_if_open(&server_name, &name)
            }
            Message::OwnedShareLoaded {
                account_epoch,
                account_fence,
                seq,
                name,
                result,
            } => {
                if account_epoch == self.account_epoch
                    && self.account_read_is_current(account_fence)
                {
                    self.owned_share_loaded(seq, &name, result)
                } else {
                    Update::none()
                }
            }

            // -------- installed package ------------------------------------
            Message::InstalledDetailLoaded(seq, account_fence, result) => {
                self.installed_detail_loaded(seq, account_fence, *result)
            }
            Message::InstalledLatestCompared(seq, account_fence, result) => {
                self.installed_latest_compared(seq, account_fence, result)
            }
            Message::InstalledVersionChangeResolved(seq, account_fence, mode, result) => {
                self.installed_version_change_resolved(seq, account_fence, mode, result)
            }
            Message::LocalManifestRequirementsResolved {
                seq,
                account_fence,
                completion,
                result,
            } => self.local_manifest_requirements_resolved(seq, account_fence, completion, result),
            Message::InstalledResolvedForGraph(seq, account_fence, spec, staged, result) => self
                .installed_resolved_for_graph(seq, account_fence, &spec, staged.as_deref(), result),
            Message::SetInstalledUpdateMode(mode) => self.set_installed_update_mode(mode),
            Message::SelectInstalledFile(subpath) => self.select_installed_file(subpath),
            Message::SelectInstalledPackageTab(tab) => {
                self.installed_package_tab = tab;
                if tab == InstalledPackageTab::Source {
                    self.ensure_selected_source()
                } else {
                    Update::none()
                }
            }
            Message::SelectLocalPackageTab(tab) => {
                self.local_package_tab = tab;
                Update::none()
            }
            Message::SetParameterScope(scope) => self.set_open_parameter_scope(scope),
            Message::ConfirmGlobalParameterSource => self.confirm_global_parameter_source(),
            Message::CancelGlobalParameterSource => {
                self.confirm_global_parameter_source = false;
                self.copy_settings_prompt = None;
                Update::none()
            }
            Message::OpenCopySettings => self.open_copy_settings(),
            Message::SelectCopySettingsDestination(profile_name) => {
                self.select_copy_settings_destination(profile_name)
            }
            Message::CancelCopySettings => self.cancel_copy_settings(),
            Message::ConfirmCopySettings => self.confirm_copy_settings(),
            Message::SelectParameterProfile(profile_name) => {
                self.select_parameter_profile(profile_name)
            }
            Message::InstalledSourceLoaded {
                hash,
                account_fence,
                result,
            } => self.installed_source_loaded(hash, account_fence, result),
            Message::RequestUninstall => self.request_uninstall(),
            Message::UninstallKeepOrphans => {
                // Keep the offered orphans; the forced breaks still go.
                self.uninstall_orphans.clear();
                Update::none()
            }
            Message::CancelUninstall => {
                self.confirm_uninstall = false;
                self.uninstall_expected_lock = None;
                self.uninstall_orphans.clear();
                self.uninstall_breaks.clear();
                Update::none()
            }
            Message::ConfirmUninstall => self.uninstall_installed(),
            Message::StartForkPackage => self.start_fork_package(),
            Message::SetForkName(name) => {
                if !self.manage_busy && self.fork_draft_is_for_open_package() {
                    self.fork_name = Some(name);
                    self.manage_feedback = None;
                }
                Update::none()
            }
            Message::CancelForkPackage => {
                if !self.manage_busy {
                    self.clear_fork_draft();
                    self.manage_feedback = None;
                }
                Update::none()
            }
            Message::ForkPackage => self.fork_installed(),
            Message::ForkFinished {
                source_specifier,
                destination_name,
                operation_id,
                completion,
                origin,
                origin_revision,
                result,
            } => {
                completion.release();
                self.fork_finished(
                    &source_specifier,
                    &destination_name,
                    operation_id,
                    &origin,
                    origin_revision,
                    result,
                )
            }
            Message::StaleAccountInstallsChecked { outcome } => {
                self.stale_account_installs_checked(outcome)
            }
            Message::RevealPackageFolder => self.reveal_package_folder(),
            Message::StartRenameOwned => self.start_rename_owned(),
            Message::RenameOwnedChanged(value) => {
                if self.rename_draft_is_for_open_package() {
                    self.rename_buffer = Some(value);
                }
                Update::none()
            }
            Message::CommitRenameOwned => self.commit_rename_owned(),
            Message::CancelRenameOwned => {
                self.clear_rename_draft();
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
            Message::InstalledRatingUpdated {
                detail_seq,
                package_id,
                account_fence,
                result,
            } => self.installed_rating_updated(detail_seq, package_id, account_fence, result),

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
            Message::DiscoverResultsLoaded(seq, result) => {
                self.discover_results_loaded(seq, result)
            }
            Message::DiscoverSelect { package_id, owner } => {
                self.discover_select(package_id, owner)
            }
            Message::DiscoverInstallResult { owner, name } => {
                self.discover_install_result(owner, name)
            }
            Message::DiscoverDetailLoaded {
                seq,
                package_id,
                account_fence,
                result,
            } => self.discover_detail_loaded(seq, package_id, account_fence, result),
            Message::DiscoverCommentsLoaded {
                seq,
                package_id,
                account_fence,
                result,
            } => self.discover_comments_loaded(seq, package_id, account_fence, result),
            Message::DiscoverBack => self.discover_back(),
            Message::RatePackage(stars) => self.rate_package(stars),
            Message::RatingUpdated {
                seq,
                package_id,
                account_fence,
                result,
            } => self.rating_updated(seq, package_id, account_fence, result),
            Message::CommentInputChanged(value) => {
                self.discover_comment_input = value;
                Update::none()
            }
            Message::AddComment => self.add_comment(),
            Message::CommentAdded {
                seq,
                package_id,
                account_fence,
                result,
            } => self.comment_added(seq, package_id, account_fence, result),
            Message::OpenReadmeLink(uri) => {
                let _ = open::that(uri.as_str());
                Update::none()
            }
            Message::DiscoverInstall => self.discover_install(),
            Message::InstallResolved(seq, account_fence, result) => {
                self.install_resolved(seq, account_fence, result)
            }
            Message::ConsentGrant { enable } => self.consent_grant(enable),
            Message::ConsentCachePrepared {
                seq,
                account_fence,
                enable,
                result,
            } => self.consent_cache_prepared(seq, account_fence, enable, result),
            Message::ConsentCancel => self.consent_cancel(),
            Message::ParamValueEdit(target, key, edit) => self.param_value_edit(target, key, edit),
            Message::ParamPromptSubmit => self.param_prompt_submit(),
            Message::ParamPromptCancel => self.param_prompt_cancel(),
            Message::ParamConfigSave => self.param_config_save(),
            Message::ParamConfigClearSecret(key) => self.param_config_clear_secret(key),

            // -------- private & shared -------------------------------------
            Message::OpenShared => self.open_shared(),
            Message::SharedLoaded {
                account_epoch,
                account_fence,
                result,
            } => self.shared_loaded(account_epoch, account_fence, result),
            Message::MyCloudLoaded {
                account_epoch,
                account_fence,
                result,
            } => self.my_cloud_loaded(account_epoch, account_fence, result),
            Message::InstallShared { owner, name } => self.begin_install(owner, name),

            // -------- top action bar ---------------------------------------
            Message::Reload => {
                // Pick up a Settings change to the advanced-features gate without reopening.
                self.advanced_features =
                    smudgy_core::models::settings::load_settings().advanced_scripting_features;
                let toast = self.show_toast(crate::i18n::t!(
                    "automation-reloaded",
                    "server" => &self.server_name
                ));
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
                        Update::with_task(
                            self.show_toast(crate::i18n::t!("automation-inspector-unavailable")),
                        )
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
                // Unconsumed Escape routes here even with no palette open, so
                // it also provides conventional, source-preserving dismissal
                // for all host-owned intelligence overlays. Signature help is
                // suppressed until `(` or `,` starts a new call lifecycle, so
                // the following passive caret notification cannot reopen it.
                if let Some(editor) = &mut self.code_editor {
                    editor.dismiss_overlays();
                }
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
            Message::PollLanguageService => {
                let (task, changed) = self.poll_language_service();
                if changed {
                    self.dirty = true;
                    if matches!(self.pane, Pane::Editor(_)) {
                        self.action_script_pinned = true;
                    }
                }
                Update::with_task(task)
            }
        };
        // An unpinned action draft follows the live matcher: any edit that can
        // change what is captured regenerates the example bodies.
        if refresh_generated {
            update.task = Task::batch([update.task, self.refresh_generated_actions()]);
        }
        // The Settings tab reads parameter completeness from a cache; keep it aligned with the
        // parameter editor this update may have opened, re-seeded, or written through.
        self.sync_profile_param_status(values_written);
        update
    }

    // ---- guards ------------------------------------------------------------

    fn is_edit_message(message: &Message) -> bool {
        match message {
            Message::HotkeyTextAction(action)
            | Message::SendTextAction(action)
            | Message::AliasPatternAction(action)
            | Message::AliasRegexAction(action)
            | Message::RowSourceAction(_, action) => {
                matches!(action, text_editor::Action::Edit(_))
            }
            Message::SetName(_)
            | Message::SetFolderPath(_)
            | Message::SetNewModuleName(_)
            | Message::SetAliasKind(_)
            | Message::SetArgName(_, _)
            | Message::SetArgKind(_, _)
            | Message::AddArg
            | Message::RemoveArg(_)
            | Message::SetCmdMode(_)
            | Message::SetParseMode(_)
            | Message::ToggleAnchorStart
            | Message::ToggleAnchorEnd
            | Message::TogglePrompt
            | Message::SetBehavior(_)
            | Message::AdjustPriority(_)
            | Message::ToggleFallthrough
            | Message::ToggleAllowSelfMatch
            | Message::AddPattern
            | Message::AddExceptionRow
            | Message::AddRawRow
            | Message::SetTriggerCard(_)
            | Message::RemovePattern(_)
            | Message::MoveRowUp(_)
            | Message::MoveRowDown(_)
            | Message::SetRowSyntax(_, _)
            | Message::ToggleRowAnchorStart(_)
            | Message::ToggleRowAnchorEnd(_)
            | Message::ToggleRowColor(_, _)
            | Message::SelectRowColorKind(_, _)
            | Message::SetRowAnsiColor(_, _)
            | Message::SetRowXtermColor(_, _)
            | Message::SetRowColorRange(_, _, _)
            | Message::SetRowColorRangeHex(_, _, _)
            | Message::SetRowExactTruecolorHex(_, _)
            | Message::SetRowExactTruecolorRgb(_, _, _)
            | Message::ToggleRowColorAttribute(_, _, _)
            | Message::InsertReference(_)
            | Message::MarkHotkeyState(_) => true,
            _ => false,
        }
    }

    /// Whether a message can change what the open matcher captures — the
    /// signal to regenerate any unpinned action draft.
    fn affects_captures(message: &Message) -> bool {
        match message {
            Message::AliasPatternAction(action)
            | Message::AliasRegexAction(action)
            | Message::RowSourceAction(_, action) => {
                matches!(action, text_editor::Action::Edit(_))
            }
            Message::SetAliasKind(_)
            | Message::SetArgName(_, _)
            | Message::SetArgKind(_, _)
            | Message::AddArg
            | Message::RemoveArg(_)
            | Message::SetCmdMode(_)
            | Message::AddPattern
            | Message::AddExceptionRow
            | Message::AddRawRow
            | Message::SetTriggerCard(_)
            | Message::RemovePattern(_)
            | Message::MoveRowUp(_)
            | Message::MoveRowDown(_)
            | Message::SetRowSyntax(_, _) => true,
            _ => false,
        }
    }

    /// The action language of the open alias/trigger editor, if one is open.
    fn open_action_language(&self) -> Option<ScriptLang> {
        match &self.pane {
            Pane::Editor(EditorState { node, .. }) => match node {
                EditNode::Alias(alias) => Some(alias.language),
                EditNode::Trigger { language, .. } => Some(*language),
                EditNode::Hotkey(_) => None,
            },
            _ => None,
        }
    }

    fn is_guarded_navigation(message: &Message) -> bool {
        matches!(
            message,
            Message::SwitchContext { .. }
                | Message::SelectScript(_)
                | Message::SelectFolder(_)
                | Message::SelectModule(_)
                | Message::SelectOwnedPackage(_)
                | Message::SelectOwnedFile(_)
                | Message::NavigateCodeDefinition(_)
                | Message::SelectInstalledPackage(_)
                | Message::SelectDependency { .. }
                | Message::SetParameterScope(_)
                | Message::ConfirmGlobalParameterSource
                | Message::SelectParameterProfile(_)
                | Message::RequestClose
                | Message::SelectCreatorAutomation { .. }
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

    /// Commits the user's Discard choice before navigation consumes the current pane.
    /// Re-seeding the manifest matters for same-package definition jumps, which replace only
    /// the source editor and otherwise retain the structured manifest form in memory.
    fn accept_discarded_navigation(&mut self) {
        self.dirty = false;
        if self.manifest_dirty {
            let _ = self.revert_manifest();
        }
        if let Some(config) = self.param_config.as_mut() {
            config.touched.clear();
        }
        // These forms are drafts too. Ordinary navigation reaches this point only after the user
        // selected Discard; terminal navigation retains the whole window until the daemon accepts
        // the close/switch, so it deliberately does not call this helper.
        self.clear_rename_draft();
        self.clear_fork_draft();
    }

    fn confirm_pending_navigation(&mut self, revision: u64) -> Update<Message, Event> {
        if revision != self.pending_nav_revision {
            return Update::none();
        }
        let Some(message) = self.pending_nav.take() else {
            return Update::none();
        };
        self.pending_nav_revision = self.pending_nav_revision.wrapping_add(1);
        match *message {
            // Terminal navigation is accepted or rejected by the daemon in this same update turn.
            // Do not clear drafts first: an obsolete target must leave the old
            // window fully protected. An accepted switch/close drops this entire state.
            Message::SwitchContext {
                server_name,
                session_id,
                profile_name,
            } => Update::with_event(Event::SwitchContext {
                server_name,
                session_id,
                profile_name,
            }),
            Message::RequestClose => Update::with_event(Event::CloseRequested),
            // Definition results are state-fenced and can become stale while this confirmation is
            // open. Execute synchronously and clear dirty state only if the jump leaves the origin.
            Message::NavigateCodeDefinition(navigation) => {
                let (update, left_origin) = self.navigate_code_definition_checked(navigation);
                if left_origin {
                    self.accept_discarded_navigation();
                }
                update
            }
            message => {
                // Complete ordinary navigation in this turn too. Deferring it would let a newer
                // click run first and then allow this older confirmed destination to overwrite it.
                // Re-seed the manifest immediately so Discard cannot leave a dormant edited draft.
                self.accept_discarded_navigation();
                self.update(message)
            }
        }
    }

    fn cancel_pending_navigation(&mut self, revision: u64) -> Update<Message, Event> {
        if revision != self.pending_nav_revision {
            return Update::none();
        }
        let cancelled_navigation = self
            .pending_nav
            .as_deref()
            .and_then(|message| match message {
                Message::SwitchContext { .. } => Some(Event::ContextSwitchCancelled),
                Message::RequestClose => Some(Event::CloseCancelled),
                _ => None,
            });
        self.pending_nav = None;
        self.pending_nav_revision = self.pending_nav_revision.wrapping_add(1);
        Update::new(Task::none(), cancelled_navigation)
    }

    /// Drops the deferred navigation once a save or discard has resolved the draft that guarded
    /// it. Every path that forgets a pending navigation outside the banner's own actions goes
    /// through here.
    ///
    /// A terminal target (an application close or a session switch) is answered with its cancel
    /// event, exactly as Keep editing does: the daemon retains the last main window and the queued
    /// switch until it hears one answer, and a silent drop would leave the application unable to
    /// exit or switch sessions. The window stays open on the saved item; the banner offers no
    /// save-and-continue path, so a save never completes the close or switch by itself. The event
    /// is delivered through [`Message::NavigationReleased`] because the releasing update already
    /// carries the save's own event. Ordinary tree navigation is simply forgotten.
    pub(super) fn release_pending_navigation(&mut self) -> Task<Message> {
        let released = self
            .pending_nav
            .as_deref()
            .and_then(|message| match message {
                Message::SwitchContext { .. } => Some(ReleasedNavigation::ContextSwitch),
                Message::RequestClose => Some(ReleasedNavigation::Close),
                _ => None,
            });
        if self.pending_nav.take().is_some() {
            self.pending_nav_revision = self.pending_nav_revision.wrapping_add(1);
        }
        released.map_or_else(Task::none, |released| {
            Task::done(Message::NavigationReleased(released))
        })
    }

    /// Swaps a trigger row with its neighbor **within its role group** — the
    /// phase order (exceptions, raw, matches) is fixed, so reordering never
    /// crosses roles. The row buffers move with the rows.
    fn move_trigger_row(&mut self, i: usize, down: bool) {
        if let Pane::Editor(EditorState {
            node: EditNode::Trigger { rows, .. },
            ..
        }) = &mut self.pane
            && i < rows.len()
        {
            let role = rows[i].role;
            let neighbor = if down {
                rows[i + 1..]
                    .iter()
                    .position(|row| row.role == role)
                    .map(|offset| i + 1 + offset)
            } else {
                rows[..i].iter().rposition(|row| row.role == role)
            };
            if let Some(j) = neighbor {
                rows.swap(i, j);
                if i < self.trigger_row_contents.len() && j < self.trigger_row_contents.len() {
                    self.trigger_row_contents.swap(i, j);
                }
            }
        }
    }

    fn trigger_row(&self, index: usize) -> Option<&model::TriggerRow> {
        let Pane::Editor(EditorState {
            node: EditNode::Trigger { rows, .. },
            ..
        }) = &self.pane
        else {
            return None;
        };
        rows.get(index)
    }

    fn set_row_matcher_color(
        &mut self,
        index: usize,
        color: smudgy_core::models::matchers::MatcherColor,
    ) {
        use smudgy_core::models::matchers::MatcherColorChannel;
        let Pane::Editor(EditorState {
            node: EditNode::Trigger { rows, .. },
            ..
        }) = &mut self.pane
        else {
            return;
        };
        let Some(row) = rows.get_mut(index) else {
            return;
        };
        let Some(filter) = row.color.as_mut() else {
            return;
        };
        match row.color_channel {
            MatcherColorChannel::Foreground => filter.foreground = Some(color),
            MatcherColorChannel::Background => filter.background = Some(color),
        }
    }

    /// Resets per-pane selection scaffolding before opening a new pane.
    pub(super) fn clear_selection(&mut self) {
        self.selection_revision = self.selection_revision.wrapping_add(1);
        self.clear_code_editor();
        self.new_menu_open = false;
        self.confirm_folder_delete = false;
        self.confirm_delete_local = false;
        self.confirm_uninstall = false;
        self.uninstall_expected_lock = None;
        self.confirm_trust = false;
        self.clear_rename_draft();
        self.clear_fork_draft();
        self.fork_operation = None;
        // Detail/fork tasks are fenced below. The latch belongs to the pane being abandoned; a
        // stale completion must not be required to make a later pane usable.
        self.manage_busy = false;
        self.installed_readme = InstalledReadmeState::Loaded(None);
        // Drop any open manifest draft + its unsaved/editing flags — leaving the owned-package pane
        // abandons the edit (re-seeded fresh from disk when an owned package is next opened). Also
        // keeps the unsaved-changes guard from later firing for a package that's no longer open.
        self.manifest_draft = None;
        self.module_source_baseline = None;
        self.owned_source_baseline = None;
        self.manifest_source_baseline = None;
        self.manifest_dirty = false;
        self.manifest_editing = false;
        self.manifest_operation = None;
        self.authoring_operation = None;
        self.authoring_busy = false;
        self.authoring_feedback = None;
        self.manage_feedback = None;
        self.share_feedback = None;
        // Drop the inline param-value editor; the next package pane re-seeds it from its own params.
        self.param_config = None;
        self.confirm_global_parameter_source = false;
        // Abandon any in-flight install confirmation / update re-prompt on navigation — neither
        // has written anything yet (the consent window writes only on Grant). Bumping the
        // generation also discards a still-pending resolve so it can't pop a stale window later.
        self.consent_prompt = None;
        self.consent_busy = false;
        self.update_delta = None;
        self.package_change_finalize = None;
        self.install_seq.bump();
        // Drop required-parameter prompts after a committed package change. Missing values keep
        // that package fail-closed, and the inventory refresh was already queued at commit time.
        self.param_prompt = None;
        self.param_prompt_queue.clear();
        // Opening any pane abandons the manage pane's in-flight detail load too — invalidate it so a
        // late result can't repaint or record consent against the package that was open before.
        self.detail_seq.bump();
        // Discover detail, comments, ratings, and comments can carry viewer-specific state. A late
        // completion from the pane being abandoned must not repaint a later visit to the same item.
        self.discover_seq.bump();
        self.discover_requested_package = None;
        self.share_seq.bump();
        self.share_busy = false;
        self.share_operation = None;
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
            self.view_state_error_banner(),
            // The pane learns its viewport height here, outside the scrollable, so an editor
            // pane can grow into the room the viewport has before it has to scroll.
            container(iced::widget::responsive(move |size| {
                scrollable(self.view_pane(size.height))
                    .height(Length::Fill)
                    .into()
            }))
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
        if let Some(prompt) = &self.copy_settings_prompt {
            layers.push(self.view_copy_settings_modal(prompt));
        }
        if let Some(message) = &self.toast {
            layers.push(common::toast(message));
        }

        stack(layers)
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Persistent fail-closed notice for inventories that could not be read completely. Keep it
    /// outside the selected pane: an empty sidebar must never look like a valid empty setup.
    fn view_state_error_banner(&self) -> Elem<'_> {
        use iced::alignment::Vertical;
        use iced::widget::{button, text};

        let errors = [
            self.folder_state_error.as_deref(),
            self.module_state_error.as_deref(),
            self.local_package_state_error.as_deref(),
            self.installed_package_state_error.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<BTreeSet<_>>();
        if errors.is_empty() {
            return iced::widget::space::vertical()
                .height(Length::Fixed(0.0))
                .into();
        }

        let mut details = column![
            text(crate::i18n::t!("automations-state-unavailable"))
                .size(13.0)
                .style(common::warning)
        ]
        .spacing(3.0);
        for error in errors {
            details = details.push(text(error).size(11.0).style(common::muted));
        }
        container(
            row![
                details,
                iced::widget::space::horizontal(),
                button(text(crate::i18n::t!("automations-reload")).size(13.0))
                    .style(crate::theme::builtins::button::secondary)
                    .on_press(Message::Reload),
            ]
            .spacing(12.0)
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

    /// The sticky draft banner shown while navigation is deferred.
    fn view_nav_banner(&self) -> Elem<'_> {
        use iced::alignment::Vertical;
        use iced::widget::{button, text};
        if self.pending_nav.is_none() {
            return iced::widget::space::vertical()
                .height(Length::Fixed(0.0))
                .into();
        }
        let message = crate::i18n::t!("automation-nav-unsaved");
        let continue_label = if self.pending_context_switch() {
            crate::i18n::t!("automation-discard-and-switch")
        } else if self.pending_close() {
            crate::i18n::t!("automation-discard-and-close")
        } else {
            crate::i18n::t!("editor-discard")
        };
        let stay_label = crate::i18n::t!("automation-keep-editing");
        container(
            row![
                text("\u{25CF}").size(10.0).style(common::danger),
                text(message).size(13.0),
                iced::widget::space::horizontal(),
                button(text(continue_label).size(13.0))
                    .style(crate::theme::builtins::button::secondary)
                    .on_press(Message::ConfirmDiscardNavRevision(
                        self.pending_nav_revision,
                    )),
                button(text(stay_label).size(13.0))
                    .style(crate::theme::builtins::button::primary)
                    .on_press(Message::CancelDiscardNavRevision(self.pending_nav_revision,)),
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
    /// `viewport_height` is the height of the scroll viewport the pane renders into.
    fn view_pane(&self, viewport_height: f32) -> Elem<'_> {
        match &self.pane {
            Pane::Dashboard => self.view_dashboard(),
            Pane::Error(errors) => self.view_error(errors),
            Pane::Editor(state) => self.view_editor(state, viewport_height),
            Pane::Folder(state) => self.view_folder_editor(state),
            Pane::Module(state) => self.view_module(state, viewport_height),
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

#[cfg(test)]
mod tab_traversal_tests {
    use super::*;

    fn window_with_foreground(
        color: smudgy_core::models::matchers::MatcherColor,
    ) -> AutomationsWindow {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "truecolor-editor-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.pane = Pane::Editor(EditorState {
            mode: EditorMode::Create,
            original_name: None,
            name: "color".to_string(),
            node: EditNode::Trigger {
                enabled: true,
                language: ScriptLang::Plaintext,
                prompt: false,
                priority: 0,
                fallthrough: false,
                package: None,
                rows: vec![model::TriggerRow {
                    color: Some(smudgy_core::models::matchers::MatcherColorMatch {
                        foreground: Some(color),
                        ..Default::default()
                    }),
                    ..model::TriggerRow::new(PatternKind::Match)
                }],
            },
            error: None,
        });
        window
    }

    fn first_row(window: &AutomationsWindow) -> &model::TriggerRow {
        let Pane::Editor(EditorState {
            node: EditNode::Trigger { rows, .. },
            ..
        }) = &window.pane
        else {
            panic!("test window must contain a trigger editor");
        };
        &rows[0]
    }

    fn foreground(window: &AutomationsWindow) -> smudgy_core::models::matchers::MatcherColor {
        first_row(window)
            .color
            .as_ref()
            .and_then(|filter| filter.foreground)
            .expect("test row must have a foreground filter")
    }

    fn background(window: &AutomationsWindow) -> smudgy_core::models::matchers::MatcherColor {
        first_row(window)
            .color
            .as_ref()
            .and_then(|filter| filter.background)
            .expect("test row must have a background filter")
    }

    #[test]
    fn retiring_a_session_cancels_only_its_exact_runtime_subscriptions() {
        let session_id = SessionId::from(71);
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "subscription-liveness-test".to_string(),
            crate::cloud_account::test_handles(),
            session_id,
        );
        assert!(window.session_binding_live);

        window.retire_session_binding(SessionId::from(72));
        assert!(window.session_binding_live);

        window.retire_session_binding(session_id);
        assert!(!window.session_binding_live);
    }

    #[test]
    fn publish_completion_updates_the_open_pane_immediately() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "publish-completion-test".to_string(),
            crate::cloud_account::test_handles_signed_in("publisher"),
            SessionId::from(1),
        );
        window.selection = Selection::OwnedPackage("demo".to_string());
        window.authoring_busy = true;
        let operation = window
            .cloud
            .package_operations
            .try_acquire("publish-completion-test", "demo")
            .unwrap();
        let operation_id = operation.id();
        window.authoring_operation = Some(operation_id);
        let completion = operation.into_completion();
        let package_id = Uuid::new_v4();
        let published_at = "2026-08-10T00:00:00Z".parse().unwrap();
        let publisher_id = window.cloud.snapshot.get().profile.as_ref().unwrap().id;

        let _ = window.update(Message::PublishFinished {
            server_name: "publish-completion-test".to_string(),
            name: "demo".to_string(),
            operation_id,
            completion,
            credential_generation: window.cloud.credentials.generation(),
            publisher_id,
            result: Ok(PublishSummary {
                package_id,
                is_public: true,
                version: "1.2.3".to_string(),
                published_at,
                typings_generated: 1,
                typings_warnings: Vec::new(),
                locked_dependencies: Vec::new(),
                dependency_warnings: Vec::new(),
                interop_warnings: Vec::new(),
                publication_warnings: Vec::new(),
            }),
        });

        assert!(!window.authoring_busy);
        assert_eq!(window.share_package_id, Some(package_id));
        assert!(window.share_is_public);
        assert_eq!(window.share_versions.len(), 1);
        assert_eq!(window.share_versions[0].version, "1.2.3");
        assert_eq!(window.share_versions[0].published_at, published_at);
        let output = window.publish_output.as_ref().unwrap();
        assert_eq!(output.package, "demo");
        assert!(output.text.starts_with("smudgy> publish demo\n"));
    }

    #[test]
    fn publish_completion_does_not_expose_another_accounts_sharing_state() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "publish-account-fence-test".to_string(),
            crate::cloud_account::test_handles_signed_in("same-visible-name"),
            SessionId::from(1),
        );
        window.selection = Selection::OwnedPackage("demo".to_string());
        window.authoring_busy = true;
        window.share_package_id = Some(Uuid::new_v4());
        window.share_is_public = true;
        let operation = window
            .cloud
            .package_operations
            .try_acquire("publish-account-fence-test", "demo")
            .unwrap();
        let operation_id = operation.id();
        window.authoring_operation = Some(operation_id);
        let package_id = Uuid::new_v4();

        let _ = window.update(Message::PublishFinished {
            server_name: "publish-account-fence-test".to_string(),
            name: "demo".to_string(),
            operation_id,
            completion: operation.into_completion(),
            credential_generation: window.cloud.credentials.generation(),
            publisher_id: Uuid::new_v4(),
            result: Ok(PublishSummary {
                package_id,
                is_public: true,
                version: "1.0.0".to_string(),
                published_at: "2026-08-10T00:00:00Z".parse().unwrap(),
                typings_generated: 0,
                typings_warnings: Vec::new(),
                locked_dependencies: Vec::new(),
                dependency_warnings: Vec::new(),
                interop_warnings: Vec::new(),
                publication_warnings: Vec::new(),
            }),
        });

        assert_eq!(
            window.publication_status,
            PublicationStatus::Bound(package_id)
        );
        assert_eq!(window.share_package_id, None);
        assert!(!window.share_is_public);
        assert!(window.share_versions.is_empty());
        assert!(window.share_grants.is_empty());
        assert!(
            window.share_busy,
            "the completion must start a refresh for the newly active account"
        );
    }

    #[test]
    fn old_package_completions_do_not_release_newer_ui_latches() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "operation-fence-test".to_string(),
            crate::cloud_account::test_handles_signed_in("publisher"),
            SessionId::from(1),
        );
        let old_publish = window
            .cloud
            .package_operations
            .try_acquire("operation-fence-test", "demo")
            .unwrap();
        let old_publish_id = old_publish.id();
        let old_publish_completion = old_publish.into_completion();
        let current_publish = window
            .cloud
            .package_operations
            .try_acquire("operation-fence-test", "other")
            .unwrap();
        window.authoring_operation = Some(current_publish.id());
        window.authoring_busy = true;
        let publisher_id = window.cloud.snapshot.get().profile.as_ref().unwrap().id;

        let _ = window.update(Message::PublishFinished {
            server_name: "operation-fence-test".to_string(),
            name: "demo".to_string(),
            operation_id: old_publish_id,
            completion: old_publish_completion,
            credential_generation: window.cloud.credentials.generation(),
            publisher_id,
            result: Err("old result".to_string()),
        });
        assert!(window.authoring_busy);
        assert_eq!(window.authoring_operation, Some(current_publish.id()));
        drop(current_publish);

        let old_fork = window
            .cloud
            .package_operations
            .try_acquire("operation-fence-test", "copy")
            .unwrap();
        let old_fork_id = old_fork.id();
        let old_fork_completion = old_fork.into_completion();
        let current_fork = window
            .cloud
            .package_operations
            .try_acquire("operation-fence-test", "other-copy")
            .unwrap();
        window.fork_operation = Some(current_fork.id());
        window.fork_name = Some("current-copy".to_string());
        window.fork_source_specifier = Some("smudgy://publisher/current".to_string());
        window.manage_busy = true;

        old_fork_completion.release();
        let _ = window.fork_finished(
            "smudgy://publisher/source",
            "copy",
            old_fork_id,
            &Selection::Dashboard,
            0,
            Err("old result".to_string()),
        );
        assert!(window.manage_busy);
        assert_eq!(window.fork_operation, Some(current_fork.id()));
        assert_eq!(window.fork_name.as_deref(), Some("current-copy"));
        assert_eq!(
            window.fork_source_specifier.as_deref(),
            Some("smudgy://publisher/current")
        );
    }

    #[test]
    fn ui_audit_package_action_forms_are_source_bound_navigation_drafts() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "package-form-draft-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        let source = "smudgy://publisher/alpha";
        window.installed_open = Some(Box::new(LockedPackage::new(source, UpdateMode::Auto)));
        window.selection = Selection::InstalledPackage(source.to_string());
        window.pane = Pane::InstalledPackage;
        window.fork_source_specifier = Some(source.to_string());
        window.fork_name = Some("alpha-copy".to_string());

        assert_eq!(window.open_fork_name(), Some("alpha-copy"));
        assert!(window.has_unsaved_changes());
        let guarded = window.update(Message::ShowDashboard);
        assert!(guarded.event.is_none());
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::ShowDashboard)
        ));
        assert_eq!(window.open_fork_name(), Some("alpha-copy"));

        let revision = window.pending_nav_revision;
        let _ = window.update(Message::ConfirmDiscardNavRevision(revision));
        assert!(window.fork_name.is_none());
        assert!(window.fork_source_specifier.is_none());
        assert_eq!(window.selection, Selection::Dashboard);

        let manifest = serde_json::from_value(serde_json::json!({"version": "0.1.0"}))
            .expect("minimal package manifest");
        window.local_package = Some(Box::new(LocalPackage {
            name: "alpha".to_string(),
            manifest,
            readme: None,
            modules: Vec::new(),
        }));
        window.rename_source_name = Some("alpha".to_string());
        window.rename_buffer = Some("alpha-renamed".to_string());
        assert_eq!(window.open_rename_buffer(), Some("alpha-renamed"));

        window.local_package.as_mut().unwrap().name = "beta".to_string();
        assert_eq!(window.open_rename_buffer(), None);
        let _ = window.update(Message::RenameOwnedChanged("must-not-leak".to_string()));
        assert_eq!(window.rename_buffer.as_deref(), Some("alpha-renamed"));
        let _ = window.update(Message::CancelRenameOwned);
        assert!(window.rename_buffer.is_none());
        assert!(window.rename_source_name.is_none());
    }

    #[test]
    fn ui_audit_installed_readme_state_distinguishes_empty_loading_and_failure() {
        assert!(InstalledReadmeState::Loaded(None).is_loaded());
        assert!(!InstalledReadmeState::Loading.is_loaded());
        assert!(!InstalledReadmeState::Failed("offline".to_string()).is_loaded());

        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "installed-readme-state-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.installed_readme = InstalledReadmeState::Loading;
        window.manage_busy = true;
        let _ = window.installed_detail_loaded(
            window.detail_seq,
            window.account_read_fence(),
            Err(CloudError::NetworkError("offline".to_string())),
        );
        assert!(matches!(
            window.installed_readme,
            InstalledReadmeState::Failed(_)
        ));
        assert!(!window.installed_detail_ready_for_copy());
    }

    #[test]
    fn opening_legacy_folder_uses_existing_case_insensitive_activation_row() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "folder-case-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        smudgy_core::models::packages::insert_folder(&mut window.packages, "Combat");

        let _ = window.open_folder("combat".to_string());

        assert_eq!(
            smudgy_core::models::packages::collect_folder_paths(&window.packages),
            vec!["Combat".to_string()]
        );
        assert_eq!(window.selection, Selection::Folder("combat".to_string()));
        let Pane::Folder(folder) = &window.pane else {
            panic!("folder pane should be open");
        };
        assert_eq!(folder.path, "combat");
        assert_eq!(folder.original_path.as_deref(), Some("combat"));
    }

    #[test]
    fn plain_and_shift_tab_choose_forward_and_backward_traversal() {
        assert_eq!(
            tab_traversal(keyboard::Modifiers::empty(), Status::Ignored),
            Some(false)
        );
        assert_eq!(
            tab_traversal(keyboard::Modifiers::SHIFT, Status::Ignored),
            Some(true)
        );
    }

    #[test]
    fn captured_or_shortcut_modified_tabs_do_not_traverse() {
        assert_eq!(
            tab_traversal(keyboard::Modifiers::empty(), Status::Captured),
            None
        );
        for modifier in [
            keyboard::Modifiers::CTRL,
            keyboard::Modifiers::ALT,
            keyboard::Modifiers::LOGO,
        ] {
            assert_eq!(tab_traversal(modifier, Status::Ignored), None);
        }
    }

    #[test]
    fn control_space_requests_completion_only_when_unconsumed() {
        let space = keyboard::Key::Named(keyboard::key::Named::Space);
        assert!(code_completion_shortcut(
            &space,
            keyboard::Modifiers::CTRL,
            Status::Ignored
        ));
        assert!(!code_completion_shortcut(
            &space,
            keyboard::Modifiers::CTRL,
            Status::Captured
        ));
        assert!(!code_completion_shortcut(
            &space,
            keyboard::Modifiers::empty(),
            Status::Ignored
        ));
    }

    #[test]
    fn escape_route_closes_transient_ui_without_editing_code() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "escape-code-overlay-test".to_owned(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        let _ = window.bind_code_editor(
            "const value = 1;",
            smudgy_script::language_service::Language::TypeScript,
            code_editor::CodeDocument::StandaloneModule,
        );
        window.palette_open = true;

        let _ = window.update(Message::ClosePalette);

        assert!(!window.palette_open);
        assert_eq!(window.code_editor_text(), "const value = 1;");
        assert!(!window.dirty);
    }

    #[test]
    fn hotkey_preserves_its_single_body_across_language_changes() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "hotkey-code-transition-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.action_script_lang = ScriptLang::TS;
        let _ = window.new_hotkey();
        assert_eq!(window.action_script_lang, ScriptLang::JS);
        window.hotkey_text_content = text_editor::Content::with_text("say hello");

        let _ = window.update(Message::SetBehavior(ScriptLang::JS));

        assert_eq!(window.action_script_lang, ScriptLang::JS);
        assert!(window.code_editor.is_some());
        assert!(window.language_service.is_some());
        assert_eq!(window.code_editor_text(), "say hello");
        let message = window
            .bind_code_editor_message(code_editor::IcedEditorMessage::CtrlEnd)
            .expect("code editor is bound");
        let _ = window.update(Message::CodeEditorAction(message));
        let message = window
            .bind_code_editor_message(code_editor::IcedEditorMessage::Paste("();".to_owned()))
            .expect("code editor is bound");
        let _ = window.update(Message::CodeEditorAction(message));
        let _ = window.update(Message::SetBehavior(ScriptLang::Plaintext));

        assert_eq!(window.action_script_lang, ScriptLang::JS);
        assert_eq!(window.hotkey_text_content.text(), "say hello();");
        assert!(window.code_editor.is_none());
        assert!(matches!(
            &window.pane,
            Pane::Editor(EditorState {
                node: EditNode::Hotkey(hotkeys::HotkeyDefinition {
                    language: ScriptLang::Plaintext,
                    ..
                }),
                ..
            })
        ));

        let _ = window.update(Message::SetBehavior(ScriptLang::TS));
        assert_eq!(window.action_script_lang, ScriptLang::TS);
        assert_eq!(window.code_editor_text(), "say hello();");
    }

    #[test]
    fn opening_a_hotkey_restores_its_script_dialect_for_tab_round_trips() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "hotkey-script-dialect-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.scripts.insert(
            "typed hotkey".to_string(),
            Script::Hotkey(hotkeys::HotkeyDefinition {
                key: "F1".to_string(),
                modifiers: Vec::new(),
                script: Some("const value: number = 1;".to_string()),
                package: None,
                language: ScriptLang::TS,
                enabled: true,
            }),
        );
        window.action_script_lang = ScriptLang::JS;

        let _ = window.open_script(ScriptKey {
            folder_name: None,
            script_name: "typed hotkey".to_string(),
        });
        assert_eq!(window.action_script_lang, ScriptLang::TS);

        let _ = window.update(Message::SetBehavior(ScriptLang::Plaintext));
        assert_eq!(window.action_script_lang, ScriptLang::TS);
        let _ = window.update(Message::SetBehavior(window.action_script_lang));
        assert!(matches!(
            &window.pane,
            Pane::Editor(EditorState {
                node: EditNode::Hotkey(hotkeys::HotkeyDefinition {
                    language: ScriptLang::TS,
                    ..
                }),
                ..
            })
        ));
    }

    #[test]
    fn pointer_presses_outside_the_editor_region_release_its_focus() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "editor-focus-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        let _ = window.new_hotkey();
        let _ = window.update(Message::SetBehavior(ScriptLang::JS));
        let losses = |window: &AutomationsWindow| {
            window
                .code_editor
                .as_ref()
                .expect("code editor is bound")
                .focus_losses()
        };
        assert_eq!(losses(&window), 0);

        // A press while the pointer is over the editor region (including its
        // overlays and chrome) keeps focus: that press is the editor's own.
        let _ = window.update(Message::CodeEditorPointerEntered);
        let _ = window.update(Message::PointerPressed(window.window_id));
        assert_eq!(losses(&window), 0);

        // A press anywhere else in this window hands focus to that widget.
        let _ = window.update(Message::CodeEditorPointerExited);
        let _ = window.update(Message::PointerPressed(window.window_id));
        assert_eq!(losses(&window), 1);

        // Other windows' presses are not this editor's concern.
        let _ = window.update(Message::PointerPressed(window::Id::unique()));
        assert_eq!(losses(&window), 1);

        // Keyboard traversal moves focus to another widget the same way.
        let _ = window.update(Message::FocusNext(window.window_id));
        assert_eq!(losses(&window), 2);
    }

    #[test]
    fn non_script_writable_files_do_not_start_the_language_service() {
        for language in [
            smudgy_script::language_service::Language::PlainText,
            smudgy_script::language_service::Language::Json,
        ] {
            let mut window = AutomationsWindow::new(
                window::Id::unique(),
                "non-script-editor-test".to_string(),
                crate::cloud_account::test_handles(),
                SessionId::from(1),
            );

            let _ = window.bind_code_editor(
                "notes only",
                language,
                code_editor::CodeDocument::OwnedPackage,
            );

            assert!(window.code_editor.is_some());
            assert!(window.language_service.is_none());
            assert_eq!(window.code_editor_text(), "notes only");
        }
    }

    #[test]
    fn new_module_rebinds_language_from_its_name_without_losing_text() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "new-module-language-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        let _ = window.new_module();
        let original = window.code_editor_text();

        let _ = window.update(Message::SetNewModuleName("helpers.js".to_owned()));
        assert_eq!(
            window.code_editor.as_ref().unwrap().document().language,
            smudgy_script::language_service::Language::JavaScript
        );
        assert_eq!(window.code_editor_text(), original);

        let _ = window.update(Message::SetNewModuleName("data.json".to_owned()));
        let editor = window.code_editor.as_ref().unwrap();
        assert_eq!(
            editor.document().language,
            smudgy_script::language_service::Language::Json
        );
        assert!(!editor.has_language_service());
        assert_eq!(window.code_editor_text(), original);
        assert!(window.dirty);
    }

    #[test]
    fn new_module_name_changes_do_not_replace_explicit_activation() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "new-module-activation-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        let _ = window.new_module();
        let _ = window.update(Message::SetNewModuleName("nested/worker.ts".to_owned()));
        assert!(matches!(
            &window.pane,
            Pane::Module(state) if state.activation == ProfileActivation::None
        ));

        // This equals the nested-path default, but it is still an explicit user choice.
        let _ = window.update(Message::DisableEverywhere);
        let _ = window.update(Message::SetNewModuleName("worker.ts".to_owned()));
        assert!(matches!(
            &window.pane,
            Pane::Module(state)
                if state.activation == ProfileActivation::None && state.activation_touched
        ));

        let _ = window.new_module();
        let _ = window.update(Message::EnableEverywhere);
        let _ = window.update(Message::SetNewModuleName("nested/worker.ts".to_owned()));
        assert!(matches!(
            &window.pane,
            Pane::Module(state)
                if state.activation == ProfileActivation::All && state.activation_touched
        ));
    }

    #[test]
    fn stale_async_editor_message_cannot_mutate_a_remounted_stable_document() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "stale-editor-message-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.pane = Pane::Module(ModuleState {
            mode: ModuleMode::View,
            subpath: "same.ts".to_owned(),
            path: Some(std::path::PathBuf::from("same.ts")),
            name: String::new(),
            tab: ModuleTab::Source,
            activation: ProfileActivation::All,
            activation_touched: false,
            error: None,
        });
        let _ = window.bind_code_editor(
            "first",
            smudgy_script::language_service::Language::TypeScript,
            code_editor::CodeDocument::StandaloneModule,
        );
        let first_document = window
            .code_editor
            .as_ref()
            .unwrap()
            .document()
            .document
            .key
            .document_id;
        let stale = window
            .bind_code_editor_message(code_editor::IcedEditorMessage::Paste(" stale".to_owned()))
            .unwrap();
        let _ = window.bind_code_editor(
            "second",
            smudgy_script::language_service::Language::TypeScript,
            code_editor::CodeDocument::StandaloneModule,
        );
        assert_eq!(
            window
                .code_editor
                .as_ref()
                .unwrap()
                .document()
                .document
                .key
                .document_id,
            first_document,
            "saved paths deliberately reuse their stable routing identity"
        );
        assert_ne!(
            stale.mount_generation, window.code_editor_mount_generation,
            "each surface binding needs a distinct asynchronous-task fence"
        );

        let _ = window.update(Message::CodeEditorAction(stale));

        assert_eq!(window.code_editor_text(), "second");
        assert!(!window.dirty);
    }

    #[test]
    fn module_reload_rebinds_clean_text_and_preserves_a_dirty_overlay() {
        let directory = tempfile::tempdir().expect("temporary module directory");
        let path = directory.path().join("same.ts");
        std::fs::write(&path, "export const value = 1;\n").expect("seed module");
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "module-reload-overlay-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.modules = vec![smudgy_core::models::modules::ModuleFile {
            subpath: "same.ts".to_owned(),
            path: path.clone(),
        }];
        window.selection = Selection::Module("same.ts".to_owned());
        window.pane = Pane::Module(ModuleState {
            mode: ModuleMode::View,
            subpath: "same.ts".to_owned(),
            path: Some(path.clone()),
            name: String::new(),
            tab: ModuleTab::Source,
            activation: ProfileActivation::All,
            activation_touched: false,
            error: None,
        });
        let _ = window.bind_code_editor(
            "export const value = 1;\n",
            smudgy_script::language_service::Language::TypeScript,
            code_editor::CodeDocument::StandaloneModule,
        );
        let stable_id = window
            .code_editor
            .as_ref()
            .unwrap()
            .document()
            .document
            .key
            .document_id;

        std::fs::write(&path, "export const value = 2;\n").expect("update clean module");
        let _ = window.reconcile_module_language_project_reload();
        assert_eq!(window.code_editor_text(), "export const value = 2;\n");
        assert_eq!(
            window
                .code_editor
                .as_ref()
                .unwrap()
                .document()
                .document
                .key
                .document_id,
            stable_id
        );

        let end = window
            .bind_code_editor_message(code_editor::IcedEditorMessage::CtrlEnd)
            .unwrap();
        let _ = window.update(Message::CodeEditorAction(end));
        let paste = window
            .bind_code_editor_message(code_editor::IcedEditorMessage::Paste(
                "// unsaved\n".to_owned(),
            ))
            .unwrap();
        let _ = window.update(Message::CodeEditorAction(paste));
        let dirty_text = window.code_editor_text();
        assert!(window.dirty);

        std::fs::write(&path, "export const value = 3;\n").expect("update disk beneath overlay");
        let _ = window.reconcile_module_language_project_reload();
        assert_eq!(window.code_editor_text(), dirty_text);
        assert!(window.dirty);
    }

    #[test]
    fn every_sidebar_document_route_is_guarded_while_source_is_dirty() {
        let messages = [
            Message::SelectDependency {
                parent: "parent".to_owned(),
                spec: "smudgy://owner/package".to_owned(),
            },
            Message::SelectCreatorAutomation {
                creator_id: "creator".to_owned(),
                kind: AutomationKind::Alias,
                name: "generated".to_owned(),
            },
        ];
        for message in messages {
            let mut window = AutomationsWindow::new(
                window::Id::unique(),
                "guarded-sidebar-route-test".to_string(),
                crate::cloud_account::test_handles(),
                SessionId::from(1),
            );
            window.dirty = true;

            let _ = window.update(message);

            assert!(window.pending_nav.is_some());
            assert!(window.dirty);
        }
    }

    #[test]
    fn selecting_another_owned_file_is_guarded_while_source_is_dirty() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "owned-file-navigation-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.selection = Selection::OwnedPackage("demo".to_owned());
        window.pane = Pane::OwnedPackage;
        window.owned_selected_file = Some("first.ts".to_owned());
        window.dirty = true;

        let _ = window.update(Message::SelectOwnedFile("second.ts".to_owned()));

        assert_eq!(window.owned_selected_file.as_deref(), Some("first.ts"));
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::SelectOwnedFile(path)) if path == "second.ts"
        ));
    }

    #[test]
    fn color_focus_and_channel_navigation_do_not_mark_the_editor_dirty() {
        use smudgy_core::models::matchers::{MatcherColor, MatcherColorChannel};

        let mut window = window_with_foreground(MatcherColor::Ansi { index: 7 });
        assert!(!window.dirty);

        let _ = window.update(Message::FocusColorControl(iced::widget::Id::from(
            "automation-trigger-color-row-0-channel".to_string(),
        )));
        assert!(!window.dirty);

        let _ = window.update(Message::SelectRowColorChannel(
            0,
            MatcherColorChannel::Background,
        ));
        assert!(!window.dirty);

        let _ = window.update(Message::SetRowAnsiColor(0, 3));
        assert!(window.dirty);
    }

    #[test]
    fn exact_truecolor_inputs_synchronize_only_after_valid_edits() {
        use model::{MatcherColorKind, TruecolorComponent};
        use smudgy_core::models::matchers::MatcherColor;

        let mut window = window_with_foreground(MatcherColor::Ansi { index: 7 });
        assert_eq!(model::parse_matcher_hex("aéabc"), None);
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Truecolor));
        assert_eq!(
            foreground(&window),
            MatcherColor::Truecolor {
                r: 255,
                g: 255,
                b: 255,
                range: None,
            }
        );

        let _ = window.update(Message::SetRowExactTruecolorHex(0, "#0a80ff".to_string()));
        assert_eq!(
            first_row(&window)
                .color_draft(smudgy_core::models::matchers::MatcherColorChannel::Foreground)
                .exact_truecolor
                .rgb,
            ["10", "128", "255"]
        );
        assert_eq!(
            foreground(&window),
            MatcherColor::Truecolor {
                r: 10,
                g: 128,
                b: 255,
                range: None,
            }
        );

        let valid_color = foreground(&window);
        let _ = window.update(Message::SetRowExactTruecolorHex(0, "#0a80f".to_string()));
        assert_eq!(
            first_row(&window)
                .color_draft(smudgy_core::models::matchers::MatcherColorChannel::Foreground)
                .exact_truecolor
                .hex,
            "#0a80f"
        );
        assert_eq!(foreground(&window), valid_color);

        let _ = window.update(Message::SetRowExactTruecolorRgb(
            0,
            TruecolorComponent::Green,
            "300".to_string(),
        ));
        let _ = window.update(Message::SetRowExactTruecolorRgb(
            0,
            TruecolorComponent::Red,
            "17".to_string(),
        ));
        assert_eq!(foreground(&window), valid_color);
        assert_eq!(
            first_row(&window)
                .color_draft(smudgy_core::models::matchers::MatcherColorChannel::Foreground)
                .exact_truecolor
                .rgb,
            ["17", "300", "255"]
        );

        let _ = window.update(Message::SetRowExactTruecolorRgb(
            0,
            TruecolorComponent::Green,
            "42".to_string(),
        ));
        assert_eq!(
            first_row(&window)
                .color_draft(smudgy_core::models::matchers::MatcherColorChannel::Foreground)
                .exact_truecolor
                .hex,
            "#112aff"
        );
        assert_eq!(
            foreground(&window),
            MatcherColor::Truecolor {
                r: 17,
                g: 42,
                b: 255,
                range: None,
            }
        );
    }

    #[test]
    fn channel_switches_preserve_partial_color_drafts() {
        use model::{ColorRangeEndpoint, MatcherColorKind};
        use smudgy_core::models::matchers::{MatcherColor, MatcherColorChannel};

        let mut window = window_with_foreground(MatcherColor::Ansi { index: 7 });
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Truecolor));
        let _ = window.update(Message::SetRowExactTruecolorHex(0, "#12".to_string()));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Ansi));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Truecolor));

        let _ = window.update(Message::SelectRowColorChannel(
            0,
            MatcherColorChannel::Background,
        ));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::First,
            "#34".to_string(),
        ));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Ansi));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));

        let _ = window.update(Message::SelectRowColorChannel(
            0,
            MatcherColorChannel::Foreground,
        ));
        assert_eq!(
            first_row(&window)
                .color_draft(MatcherColorChannel::Foreground)
                .exact_truecolor
                .hex,
            "#12"
        );
        let _ = window.update(Message::SelectRowColorChannel(
            0,
            MatcherColorChannel::Background,
        ));
        assert_eq!(
            first_row(&window)
                .color_draft(MatcherColorChannel::Background)
                .color_range_hex[0],
            "#34"
        );
    }

    #[test]
    fn color_toggle_message_restores_foreground_background_and_attributes() {
        use smudgy_core::models::matchers::{
            MatcherColor, MatcherColorMatch, MatcherTextAttribute,
        };

        let mut window = window_with_foreground(MatcherColor::Ansi { index: 7 });
        let filter = MatcherColorMatch {
            foreground: Some(MatcherColor::Ansi { index: 2 }),
            background: Some(MatcherColor::Xterm { index: 196 }),
            attributes: vec![MatcherTextAttribute::Bold, MatcherTextAttribute::Italic],
        };
        let Pane::Editor(EditorState {
            node: EditNode::Trigger { rows, .. },
            ..
        }) = &mut window.pane
        else {
            panic!("test window must contain a trigger editor");
        };
        rows[0].color = Some(filter.clone());

        let _ = window.update(Message::ToggleRowColor(0, false));
        assert!(first_row(&window).color.is_none());

        let _ = window.update(Message::ToggleRowColor(0, true));
        assert_eq!(first_row(&window).color.as_ref(), Some(&filter));
    }

    #[test]
    fn color_kind_tabs_restore_each_channel_dormant_values() {
        use model::{ColorRangeEndpoint, MatcherColorKind};
        use smudgy_core::models::matchers::{MatcherColor, MatcherColorChannel, MatcherHsv};

        let hex = |hsv: MatcherHsv| {
            let (r, g, b) = hsv.to_rgb();
            format!("#{r:02x}{g:02x}{b:02x}")
        };
        let vivid = |hue| MatcherHsv {
            hue,
            saturation: 255,
            value: 255,
        };
        let mut window = window_with_foreground(MatcherColor::Ansi { index: 7 });

        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Truecolor));
        let _ = window.update(Message::SetRowExactTruecolorHex(0, "#0a80ff".to_string()));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::First,
            hex(vivid(350)),
        ));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::Second,
            hex(vivid(10)),
        ));
        let foreground_range = matcher_truecolor_range(foreground(&window)).unwrap();
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Ansi));

        let _ = window.update(Message::SelectRowColorChannel(
            0,
            MatcherColorChannel::Background,
        ));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Truecolor));
        let _ = window.update(Message::SetRowExactTruecolorHex(0, "#112233".to_string()));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::First,
            hex(vivid(120)),
        ));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::Second,
            hex(vivid(240)),
        ));
        let background_range = matcher_truecolor_range(background(&window)).unwrap();
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Ansi));

        let _ = window.update(Message::SelectRowColorChannel(
            0,
            MatcherColorChannel::Foreground,
        ));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Truecolor));
        assert_eq!(
            foreground(&window),
            MatcherColor::Truecolor {
                r: 10,
                g: 128,
                b: 255,
                range: None,
            }
        );
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));
        assert_eq!(
            matcher_truecolor_range(foreground(&window)),
            Some(foreground_range)
        );

        let _ = window.update(Message::SelectRowColorChannel(
            0,
            MatcherColorChannel::Background,
        ));
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::Truecolor));
        assert_eq!(
            background(&window),
            MatcherColor::Truecolor {
                r: 17,
                g: 34,
                b: 51,
                range: None,
            }
        );
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));
        assert_eq!(
            matcher_truecolor_range(background(&window)),
            Some(background_range)
        );
    }

    #[test]
    fn invalid_color_text_blocks_save_and_remains_dirty() {
        use model::{ColorRangeEndpoint, MatcherColorKind};
        use smudgy_core::models::matchers::MatcherColor;

        let mut exact = window_with_foreground(MatcherColor::Ansi { index: 7 });
        let _ = exact.update(Message::SelectRowColorKind(0, MatcherColorKind::Truecolor));
        let _ = exact.update(Message::SetRowExactTruecolorHex(0, "#123".to_string()));
        let _ = exact.update(Message::Save);
        assert!(exact.dirty);
        let Pane::Editor(state) = &exact.pane else {
            panic!("save must keep the editor open");
        };
        assert!(state.error.is_some());
        assert_eq!(
            first_row(&exact)
                .color_draft(smudgy_core::models::matchers::MatcherColorChannel::Foreground,)
                .exact_truecolor
                .hex,
            "#123"
        );

        let mut range = window_with_foreground(MatcherColor::Ansi { index: 7 });
        let _ = range.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));
        let _ = range.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::Second,
            "#abcd".to_string(),
        ));
        let _ = range.update(Message::Save);
        assert!(range.dirty);
        let Pane::Editor(state) = &range.pane else {
            panic!("save must keep the editor open");
        };
        assert!(state.error.is_some());
        assert_eq!(
            first_row(&range)
                .color_draft(smudgy_core::models::matchers::MatcherColorChannel::Foreground,)
                .color_range_hex[1],
            "#abcd"
        );
    }

    #[test]
    fn color_range_derives_the_directed_hue_interval() {
        use model::{ColorRangeEndpoint, MatcherColorKind};
        use smudgy_core::models::matchers::{MatcherColor, MatcherHsv};

        let mut window = window_with_foreground(MatcherColor::Ansi { index: 7 });
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));
        let hex = |hsv: MatcherHsv| {
            let (r, g, b) = hsv.to_rgb();
            format!("#{r:02x}{g:02x}{b:02x}")
        };
        let vivid = |hue| MatcherHsv {
            hue,
            saturation: 255,
            value: 255,
        };

        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::First,
            hex(vivid(350)),
        ));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::Second,
            hex(vivid(10)),
        ));
        let narrow = matcher_truecolor_range(foreground(&window)).unwrap();
        assert_eq!(narrow.directed_endpoints(), (vivid(350), vivid(10)));
        assert!(narrow.wrap_hue);

        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::First,
            hex(vivid(10)),
        ));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::Second,
            hex(vivid(350)),
        ));
        let broad = matcher_truecolor_range(foreground(&window)).unwrap();
        assert_eq!(broad.directed_endpoints(), (vivid(10), vivid(350)));
        assert!(!broad.wrap_hue);
    }

    #[test]
    fn achromatic_range_hex_preserves_each_endpoint_hue() {
        use model::{ColorRangeEndpoint, MatcherColorKind};
        use smudgy_core::models::matchers::{MatcherColor, MatcherHsv};

        let mut window = window_with_foreground(MatcherColor::Ansi { index: 7 });
        let _ = window.update(Message::SelectRowColorKind(0, MatcherColorKind::ColorRange));
        let hex = |hsv: MatcherHsv| {
            let (r, g, b) = hsv.to_rgb();
            format!("#{r:02x}{g:02x}{b:02x}")
        };
        let vivid = |hue| MatcherHsv {
            hue,
            saturation: 255,
            value: 255,
        };
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::First,
            hex(vivid(350)),
        ));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::Second,
            hex(vivid(10)),
        ));

        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::First,
            "#808080".to_string(),
        ));
        let _ = window.update(Message::SetRowColorRangeHex(
            0,
            ColorRangeEndpoint::Second,
            "#ffffff".to_string(),
        ));

        let range = matcher_truecolor_range(foreground(&window)).unwrap();
        let (from, to) = range.directed_endpoints();
        assert_eq!(
            from,
            MatcherHsv {
                hue: 350,
                saturation: 0,
                value: 128,
            }
        );
        assert_eq!(
            to,
            MatcherHsv {
                hue: 10,
                saturation: 0,
                value: 255,
            }
        );
        assert!(range.wrap_hue);
        assert_eq!(
            first_row(&window)
                .color_draft(smudgy_core::models::matchers::MatcherColorChannel::Foreground)
                .color_range_last_valid,
            range
        );
    }

    fn context_switch_message() -> Message {
        Message::SwitchContext {
            server_name: "other-server".to_string(),
            session_id: SessionId::from(42),
            profile_name: "other-profile".to_string(),
        }
    }

    fn confirm_pending(window: &mut AutomationsWindow) -> Update<Message, Event> {
        window.update(Message::ConfirmDiscardNavRevision(
            window.pending_nav_revision,
        ))
    }

    fn cancel_pending(window: &mut AutomationsWindow) -> Update<Message, Event> {
        window.update(Message::CancelDiscardNavRevision(
            window.pending_nav_revision,
        ))
    }

    #[test]
    fn clean_context_switch_preserves_the_exact_requested_binding() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "source-server".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );

        let update = window.update(context_switch_message());

        assert!(matches!(
            update.event,
            Some(Event::SwitchContext {
                server_name,
                session_id,
                profile_name,
            }) if server_name == "other-server"
                && session_id == SessionId::from(42)
                && profile_name == "other-profile"
        ));
        assert!(window.pending_nav.is_none());
    }

    #[test]
    fn copy_settings_dialog_needs_profile_scope_and_a_different_destination() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "copy-settings-server".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.profile_names = vec!["alt".to_string(), "main".to_string()];
        window.profile_inventory_complete = true;
        window.parameter_profile = "main".to_string();
        let seed = |scope: ParameterScope| ParamConfig {
            specifier: "smudgy://owner/package".to_string(),
            expected_package: Some(LockedPackage::new(
                "smudgy://owner/package",
                UpdateMode::Auto,
            )),
            parameter_scope: scope,
            profile_name: "main".to_string(),
            available: true,
            params: Vec::new(),
            values: HashMap::new(),
            secret_stored: HashSet::new(),
            touched: HashSet::new(),
            error: None,
            saved: false,
        };

        window.param_config = Some(seed(ParameterScope::Global));
        let _ = window.update(Message::OpenCopySettings);
        assert!(
            window.copy_settings_prompt.is_none(),
            "same-settings-everywhere has nothing to copy between profiles"
        );

        window.param_config = Some(seed(ParameterScope::Profile));
        let _ = window.update(Message::OpenCopySettings);
        let prompt = window.copy_settings_prompt.clone().expect("dialog opens");
        assert_eq!(prompt.source, "main");
        assert_eq!(prompt.destination, None);

        let _ = window.update(Message::SelectCopySettingsDestination("main".to_string()));
        assert_eq!(
            window.copy_settings_prompt.as_ref().unwrap().destination,
            None,
            "the source profile is not a destination"
        );
        let _ = window.update(Message::SelectCopySettingsDestination("alt".to_string()));
        assert_eq!(
            window.copy_settings_prompt.as_ref().unwrap().destination,
            Some("alt".to_string())
        );

        let _ = window.update(Message::CancelCopySettings);
        assert!(window.copy_settings_prompt.is_none());
    }

    #[test]
    fn context_switch_uses_the_existing_guard_for_parameter_drafts() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "source-server".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.param_config = Some(ParamConfig {
            specifier: "smudgy://owner/package".to_string(),
            expected_package: Some(LockedPackage::new(
                "smudgy://owner/package",
                UpdateMode::Auto,
            )),
            parameter_scope: ParameterScope::Global,
            profile_name: "main".to_string(),
            available: true,
            params: Vec::new(),
            values: HashMap::new(),
            secret_stored: HashSet::new(),
            touched: HashSet::from(["draft".to_string()]),
            error: None,
            saved: false,
        });

        let update = window.update(context_switch_message());

        assert!(update.event.is_none());
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::SwitchContext { session_id, .. })
                if *session_id == SessionId::from(42)
        ));

        let confirmed = confirm_pending(&mut window);
        assert!(matches!(confirmed.event, Some(Event::SwitchContext { .. })));
        assert!(window.pending_nav.is_none());
        assert!(window.has_unsaved_changes());
    }

    #[test]
    fn ordinary_navigation_cannot_replace_a_pending_context_switch() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "source-server".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.dirty = true;

        let _ = window.update(context_switch_message());
        let _ = window.update(Message::ShowDashboard);

        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::SwitchContext { session_id, .. })
                if *session_id == SessionId::from(42)
        ));
        let cancelled = cancel_pending(&mut window);
        assert!(matches!(
            cancelled.event,
            Some(Event::ContextSwitchCancelled)
        ));
    }

    #[test]
    fn stale_confirmation_actions_cannot_affect_a_newer_terminal_target() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "source-server".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.dirty = true;
        let _ = window.update(context_switch_message());
        let stale_revision = window.pending_nav_revision;

        let _ = window.update(Message::SwitchContext {
            server_name: "newest-server".to_string(),
            session_id: SessionId::from(99),
            profile_name: "newest-profile".to_string(),
        });
        assert_ne!(window.pending_nav_revision, stale_revision);

        let stale_confirm = window.update(Message::ConfirmDiscardNavRevision(stale_revision));
        assert!(stale_confirm.event.is_none());
        assert!(window.dirty);
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::SwitchContext { session_id, .. })
                if *session_id == SessionId::from(99)
        ));

        let stale_cancel = window.update(Message::CancelDiscardNavRevision(stale_revision));
        assert!(stale_cancel.event.is_none());
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::SwitchContext { session_id, .. })
                if *session_id == SessionId::from(99)
        ));

        let current_revision = window.pending_nav_revision;
        let current = window.update(Message::ConfirmDiscardNavRevision(current_revision));
        assert!(matches!(
            current.event,
            Some(Event::SwitchContext { session_id, .. })
                if session_id == SessionId::from(99)
        ));
        assert!(window.dirty, "daemon acceptance still owns draft disposal");
    }

    #[test]
    fn close_request_uses_the_existing_dirty_navigation_guard() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "close-guard-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.dirty = true;

        let guarded = window.update(Message::RequestClose);
        assert!(guarded.event.is_none());
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::RequestClose)
        ));

        let accepted = confirm_pending(&mut window);
        assert!(matches!(accepted.event, Some(Event::CloseRequested)));
        assert!(window.has_unsaved_changes());
    }

    #[test]
    fn authenticated_read_fence_expires_as_soon_as_the_credential_changes() {
        let window = AutomationsWindow::new(
            window::Id::unique(),
            "account-read-fence-test".to_string(),
            crate::cloud_account::test_handles_signed_in("first"),
            SessionId::from(1),
        );
        let fence = window.account_read_fence();
        assert!(window.account_read_is_current(fence));

        window.cloud.credentials.set(None);
        assert!(!window.account_read_is_current(fence));
    }

    #[test]
    fn authenticated_read_fence_expires_on_an_account_snapshot_epoch_change() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "account-read-epoch-test".to_string(),
            crate::cloud_account::test_handles_signed_in("first"),
            SessionId::from(1),
        );
        let fence = window.account_read_fence();
        window.account_epoch = window.account_epoch.wrapping_add(1);
        assert!(!window.account_read_is_current(fence));
    }

    #[test]
    fn discover_drops_out_of_order_search_results() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "discover-search-fence-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.selection = Selection::Discover;
        window.discover_busy = true;
        let superseded = window.discover_search_seq;
        window.discover_search_seq.bump();

        let _ = window.discover_results_loaded(superseded, Ok(Vec::new()));

        assert!(window.discover_busy, "the current search must remain busy");
    }

    #[test]
    fn discover_drops_viewer_results_after_an_account_change() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "discover-account-fence-test".to_string(),
            crate::cloud_account::test_handles_signed_in("first"),
            SessionId::from(1),
        );
        let package_id = Uuid::new_v4();
        window.selection = Selection::Discover;
        window.discover_requested_package = Some(package_id);
        let seq = window.discover_seq;
        let fence = window.account_read_fence();
        window.cloud.credentials.set(None);

        let _ = window.discover_detail_loaded(
            seq,
            package_id,
            fence,
            Err(CloudError::Unauthorized("old account".to_string())),
        );

        assert!(window.discover_error.is_none());
        assert!(window.discover_detail.is_none());
    }

    #[test]
    fn installed_detail_drops_results_from_the_previous_account() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "installed-account-fence-test".to_string(),
            crate::cloud_account::test_handles_signed_in("first"),
            SessionId::from(1),
        );
        let seq = window.detail_seq;
        let fence = window.account_read_fence();
        window.manage_busy = true;
        window.cloud.credentials.set(None);

        let _ = window.installed_detail_loaded(
            seq,
            fence,
            Err(CloudError::Unauthorized("old account".to_string())),
        );

        assert!(window.manage_busy, "a newer load owns the busy state");
        assert!(window.manage_feedback.is_none());
    }

    #[test]
    fn parameter_drafts_guard_profile_and_scope_changes() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "source-server".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.param_config = Some(ParamConfig {
            specifier: "smudgy://owner/package".to_string(),
            expected_package: Some(LockedPackage::new(
                "smudgy://owner/package",
                UpdateMode::Auto,
            )),
            parameter_scope: ParameterScope::Global,
            profile_name: "main".to_string(),
            available: true,
            params: Vec::new(),
            values: HashMap::new(),
            secret_stored: HashSet::new(),
            touched: HashSet::from(["draft".to_string()]),
            error: None,
            saved: false,
        });

        let profile_update = window.update(Message::SelectParameterProfile("alt".to_string()));
        assert!(profile_update.event.is_none());
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::SelectParameterProfile(profile)) if profile == "alt"
        ));

        let _ = cancel_pending(&mut window);
        let scope_update = window.update(Message::SetParameterScope(ParameterScope::Profile));
        assert!(scope_update.event.is_none());
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::SetParameterScope(ParameterScope::Profile))
        ));
    }

    #[test]
    fn repeated_current_session_request_can_cancel_a_queued_context_switch() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "source-server".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.dirty = true;
        let _ = window.update(context_switch_message());
        assert!(window.pending_nav.is_some());

        window.cancel_pending_context_switch();

        assert!(window.pending_nav.is_none());
        assert!(window.has_unsaved_changes());
    }

    #[test]
    fn keep_editing_notifies_the_daemon_that_the_switch_was_cancelled() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "source-server".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.manifest_dirty = true;
        let _ = window.update(context_switch_message());

        let update = cancel_pending(&mut window);

        assert!(matches!(update.event, Some(Event::ContextSwitchCancelled)));
        assert!(window.pending_nav.is_none());
        assert!(window.has_unsaved_changes());
    }

    /// The messages a task yields without waiting on anything: `Task::done` values and batches
    /// of them. Timed work (toasts) never appears here, so callers pass tasks that contain none.
    fn task_messages(task: Task<Message>) -> Vec<Message> {
        use iced::futures::StreamExt;
        let Some(stream) = iced_runtime::task::into_stream(task) else {
            return Vec::new();
        };
        tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("test runtime")
            .block_on(
                stream
                    .filter_map(|action| async move {
                        match action {
                            iced_runtime::Action::Output(message) => Some(message),
                            _ => None,
                        }
                    })
                    .collect::<Vec<_>>(),
            )
    }

    #[test]
    fn discard_with_a_pending_terminal_navigation_answers_the_daemon_with_its_cancel_event() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "release-pending-close-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.dirty = true;
        let guarded = window.update(Message::RequestClose);
        assert!(guarded.event.is_none());
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::RequestClose)
        ));

        let discarded = window.update(Message::Discard);
        assert!(discarded.event.is_none());
        assert!(window.pending_nav.is_none());
        let mut released = task_messages(discarded.task);
        assert!(matches!(
            released.as_slice(),
            [Message::NavigationReleased(ReleasedNavigation::Close)]
        ));
        // Delivering the released notice is what lets the daemon drop
        // `main_window_close_after_automations` and un-close the last main window.
        let answered = window.update(released.remove(0));
        assert!(matches!(answered.event, Some(Event::CloseCancelled)));
        assert!(window.pending_nav.is_none());

        window.dirty = true;
        let _ = window.update(context_switch_message());
        let discarded = window.update(Message::Discard);
        let mut released = task_messages(discarded.task);
        assert!(matches!(
            released.as_slice(),
            [Message::NavigationReleased(
                ReleasedNavigation::ContextSwitch
            )]
        ));
        let answered = window.update(released.remove(0));
        assert!(matches!(
            answered.event,
            Some(Event::ContextSwitchCancelled)
        ));

        // Ordinary tree navigation owes the daemon nothing.
        window.dirty = true;
        let _ = window.update(Message::ShowDashboard);
        assert!(window.pending_nav.is_some());
        let discarded = window.update(Message::Discard);
        assert!(task_messages(discarded.task).is_empty());
        assert!(window.pending_nav.is_none());
    }

    #[test]
    fn a_save_that_leaves_nothing_dirty_releases_a_pending_close() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "release-pending-close-save-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.pane = Pane::Module(ModuleState {
            mode: ModuleMode::View,
            subpath: "same.ts".to_owned(),
            path: Some(PathBuf::from("same.ts")),
            name: String::new(),
            tab: ModuleTab::Source,
            activation: ProfileActivation::All,
            activation_touched: false,
            error: None,
        });
        window.dirty = true;
        let _ = window.update(Message::RequestClose);
        assert!(matches!(
            window.pending_nav.as_deref(),
            Some(Message::RequestClose)
        ));

        let saved = window.update(Message::SaveModule);

        assert!(!window.dirty);
        assert!(window.pending_nav.is_none());
        assert!(matches!(
            task_messages(saved.task).as_slice(),
            [Message::NavigationReleased(ReleasedNavigation::Close)]
        ));
    }

    #[test]
    fn folder_activation_controls_share_one_availability_predicate_with_the_writes() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "folder-activation-predicate-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.profile_names = vec!["alpha".to_string(), "beta".to_string()];
        window.profile_inventory_complete = true;
        window.pane = Pane::Folder(FolderState {
            mode: EditorMode::Create,
            original_path: None,
            path: "combat".to_string(),
            activation: ProfileActivation::All,
            error: Some("the last write failed".to_string()),
        });

        assert_eq!(
            window.open_activation_storage_error().as_deref(),
            Some(crate::i18n::t!("activation-folder-error-blocked").as_str())
        );
        assert!(!window.open_activation_storage_available());
        let _ = window.update(Message::DisableEverywhere);
        let _ = window.update(Message::ToggleActivationProfile("alpha".to_string()));
        let Pane::Folder(state) = &window.pane else {
            panic!("folder pane");
        };
        assert_eq!(state.activation, ProfileActivation::All);
        assert!(!window.dirty);

        // A new path attempt clears the stale error, and with it the block.
        let _ = window.update(Message::SetFolderPath("combat-2".to_string()));
        assert!(window.open_activation_storage_error().is_none());
        assert!(window.open_activation_storage_available());
        let _ = window.update(Message::DisableEverywhere);
        let Pane::Folder(state) = &window.pane else {
            panic!("folder pane");
        };
        assert_eq!(state.activation, ProfileActivation::None);
    }

    #[test]
    fn settings_profile_completeness_is_cached_by_the_model_not_read_by_the_view() {
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            "param-status-cache-test".to_string(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        window.profile_names = vec!["main".to_string(), "alt".to_string()];
        let token: smudgy_core::models::shared_packages::PackageParameter =
            serde_json::from_value(serde_json::json!({"key": "token", "required": true}))
                .expect("required parameter");
        let config = ParamConfig {
            specifier: "smudgy://owner/package".to_string(),
            expected_package: Some(LockedPackage::new(
                "smudgy://owner/package",
                UpdateMode::Auto,
            )),
            parameter_scope: ParameterScope::Profile,
            profile_name: "main".to_string(),
            available: true,
            params: vec![token],
            values: HashMap::new(),
            secret_stored: HashSet::new(),
            touched: HashSet::new(),
            error: None,
            saved: false,
        };
        window.param_config = Some(config.clone());
        assert!(window.profile_param_status.is_none());

        // Any update aligns the cache with the open editor: the server has no stored values, so
        // every profile still lacks the required key.
        let _ = window.update(Message::CancelGlobalParameterSource);
        let status = window
            .profile_param_status
            .as_ref()
            .expect("profile-scoped editor is cached");
        assert_eq!(status.specifier, "smudgy://owner/package");
        assert_eq!(status.missing_for("main"), Some(&["token".to_string()][..]));
        assert_eq!(status.missing_for("alt"), Some(&["token".to_string()][..]));
        assert_eq!(status.missing_for("other"), None);

        // A changed inventory is part of the cache identity.
        window.profile_names.push("third".to_string());
        let _ = window.update(Message::CancelGlobalParameterSource);
        let status = window.profile_param_status.as_ref().expect("recomputed");
        assert_eq!(status.missing.len(), 3);
        assert_eq!(
            status.missing_for("third"),
            Some(&["token".to_string()][..])
        );

        // Global scope and a closed editor carry no per-profile status.
        window.param_config = Some(ParamConfig {
            parameter_scope: ParameterScope::Global,
            ..config
        });
        let _ = window.update(Message::CancelGlobalParameterSource);
        assert!(window.profile_param_status.is_none());
        window.param_config = None;
        let _ = window.update(Message::CancelGlobalParameterSource);
        assert!(window.profile_param_status.is_none());
    }

    /// A process-wide temporary smudgy home for tests that write server state. Another test
    /// module may already own the override; whichever temporary root won is the one every model
    /// read resolves to, so the returned path is always the live home.
    fn use_temp_smudgy_home() -> PathBuf {
        static TEST_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        TEST_HOME
            .get_or_init(|| {
                let path = std::env::temp_dir().join(format!(
                    "smudgy-automations-window-test-home-{}",
                    std::process::id()
                ));
                std::fs::create_dir_all(&path).expect("create automations test home");
                smudgy_core::set_smudgy_home(path);
                smudgy_core::get_smudgy_home().expect("resolve the test home")
            })
            .clone()
    }

    fn create_test_server(server_name: &str, profiles: &[&str]) {
        server::create_server(
            server_name,
            server::ServerConfig::new("mud.example.com".to_string(), 4000),
        )
        .expect("create test server");
        for profile in profiles {
            create_test_profile(server_name, profile);
        }
    }

    fn create_test_profile(server_name: &str, profile: &str) {
        smudgy_core::models::profile::create_profile(
            server_name,
            profile,
            smudgy_core::models::profile::ProfileConfig {
                caption: profile.to_string(),
                send_on_connect: String::new(),
            },
        )
        .expect("create test profile");
    }

    fn selected(profiles: &[&str]) -> ProfileActivation {
        ProfileActivation::Selected {
            profiles: profiles.iter().map(|name| (*name).to_string()).collect(),
        }
    }

    #[test]
    fn activation_toggles_canonicalize_against_a_fresh_profile_inventory() {
        let home = use_temp_smudgy_home();
        let server_name = format!("activation-inventory-test-{}", std::process::id());
        create_test_server(&server_name, &["alpha", "beta", "gamma"]);
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            server_name.clone(),
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        assert_eq!(window.profile_names, ["alpha", "beta", "gamma"]);
        window.pane = Pane::Folder(FolderState {
            mode: EditorMode::Create,
            original_path: None,
            path: "combat".to_string(),
            activation: selected(&["alpha", "beta"]),
            error: None,
        });

        // With the open-time inventory, enabling the last visible profile would canonicalize to
        // `All` and silently enable the profile created meanwhile.
        create_test_profile(&server_name, "delta");
        let _ = window.update(Message::ToggleActivationProfile("gamma".to_string()));

        let Pane::Folder(state) = &window.pane else {
            panic!("folder pane");
        };
        assert_eq!(state.activation, selected(&["alpha", "beta", "gamma"]));
        assert_eq!(window.profile_names, ["alpha", "beta", "delta", "gamma"]);
        assert!(window.profile_inventory_complete);
        assert!(window.dirty);

        // An inventory that cannot be read completely refuses the write rather than guessing.
        std::fs::write(
            home.join(&server_name)
                .join("profiles")
                .join("delta")
                .join("profile.json"),
            "{ not json",
        )
        .expect("corrupt a profile");
        let _ = window.update(Message::ToggleActivationProfile("delta".to_string()));

        let Pane::Folder(state) = &window.pane else {
            panic!("folder pane");
        };
        assert_eq!(state.activation, selected(&["alpha", "beta", "gamma"]));
        assert_eq!(
            state.error.as_deref(),
            Some(crate::i18n::t!("activation-profile-inventory-error").as_str())
        );
        assert!(!window.profile_inventory_complete);
    }

    #[test]
    fn folder_create_and_rename_leave_the_editor_clean() {
        let _home = use_temp_smudgy_home();
        let server_name = format!("folder-create-clean-test-{}", std::process::id());
        create_test_server(&server_name, &["alpha"]);
        let mut window = AutomationsWindow::new(
            window::Id::unique(),
            server_name,
            crate::cloud_account::test_handles(),
            SessionId::from(1),
        );
        let loaded = window.load_scripts_message();
        let _ = window.update(loaded);
        assert!(window.automation_snapshot.is_some());

        let _ = window.update(Message::NewFolder);
        let _ = window.update(Message::SetFolderPath("combat".to_string()));
        assert!(window.dirty);
        let _ = window.update(Message::SaveFolder);
        assert!(!window.dirty, "a created folder is not an unsaved draft");
        assert!(matches!(
            &window.pane,
            Pane::Folder(FolderState {
                mode: EditorMode::Edit,
                original_path: Some(path),
                error: None,
                ..
            }) if path == "combat"
        ));
        let _ = window.update(Message::ShowDashboard);
        assert!(window.pending_nav.is_none());
        assert!(matches!(window.pane, Pane::Dashboard));

        let _ = window.update(Message::SelectFolder("combat".to_string()));
        let _ = window.update(Message::SetFolderPath("combat-renamed".to_string()));
        assert!(window.dirty);
        let _ = window.update(Message::SaveFolder);
        assert!(!window.dirty, "a renamed folder is not an unsaved draft");
        assert_eq!(
            window.selection,
            Selection::Folder("combat-renamed".to_string())
        );

        // Retyping the current path is not a change either.
        let _ = window.update(Message::SetFolderPath("combat-renamed".to_string()));
        assert!(window.dirty);
        let _ = window.update(Message::SaveFolder);
        assert!(!window.dirty);
    }
}
