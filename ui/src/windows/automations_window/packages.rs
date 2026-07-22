//! Package panes and the client-side dependency model:
//! installed packages, owned packages, Discover, and Private & Shared (the caller's own
//! cloud packages plus packages friends have shared).

use std::collections::{HashMap, HashSet};

use iced::Task;
use iced::alignment::Vertical;
use iced::widget::{
    Column, button, column, container, markdown, pick_list, radio, rich_text, row, scrollable,
    span, text, text_input,
};
use iced::{Color, Font, Length, Padding};

use smudgy_cloud::cloud_api::FriendView;
use smudgy_cloud::package_api::{
    CommentView, PackageApiClient, PackageDetail, PackageGrantView, PackageSearchResult,
    ResolvedPackageWire, SearchCategory, VersionListItem,
};
use smudgy_cloud::{CloudError, Uuid};
use smudgy_core::models::local_packages::{self, LocalModule, LocalPackage};
use smudgy_core::models::naming;
use smudgy_core::models::shared_packages::{
    self, ImportPolicy, LockedPackage, PackageManifest, PackageParameter, PackagePermissions,
    ParamKind, SmudgyCapabilities, UpdateMode,
};

use crate::assets::fonts;
use crate::components::cloud_errors::display_error;
use crate::theme::builtins::button as button_style;
use crate::update::Update;

use smudgy_core::session::runtime::{AutomationBody, AutomationKind};

use super::common;
use super::editors::pane_scroll;
use super::manifest::{ManifestDraft, ManifestTab};
use super::model::{
    CreatorAutomations, DepEdge, NodeStatus, package_display_name, parse_specifier, specifier_for,
};
use super::param_values::{self, ParamTarget, ParamValueEdit, ParamValueState, ScalarEdit};
use super::{AutomationsWindow, DiscoverScope, Elem, Event, Message, Pane, Selection};

/// The install-time prompt for a package's required params that aren't yet set.
#[derive(Debug, Clone)]
pub struct ParamPrompt {
    pub specifier: String,
    pub name: String,
    pub version: String,
    pub params: Vec<PackageParameter>,
    /// In-progress value state per key (a checkbox bool, a dropdown selection, a list of rows…),
    /// seeded empty. See [`param_values`].
    pub values: HashMap<String, ParamValueState>,
    /// Whether the consent step chose to enable (run) the package — carried so finishing after the
    /// params are filled honors the same choice.
    pub enable: bool,
    pub error: Option<String>,
}

/// The in-pane editor for an *already-present* package's configured param values, shown inline in
/// both the installed-package pane and the owned (local) package pane. Distinct from [`ParamPrompt`]
/// (the install-time gate that only collects the *missing required* params and finishes an install):
/// this exposes **every** declared param, pre-filled with the current values, and only ever persists
/// configuration — it never installs the package or changes its enabled state.
#[derive(Debug, Clone)]
pub struct ParamConfig {
    /// The package whose params these are (`smudgy://owner/name`). For an installed package this is
    /// the lock entry's specifier; for a local package its own-handle specifier (`local_own_spec`).
    /// Param storage is keyed by it, matching what the runtime's `smudgy:params` op reads.
    pub specifier: String,
    /// Every param the package declares, in manifest order.
    pub params: Vec<PackageParameter>,
    /// Value state per key (see [`param_values`]). A non-secret is seeded from its current stored
    /// value (the manifest `default` shows only as a placeholder/initial control state, never
    /// persisted unless edited). A secret is always seeded empty — an existing secret is never read
    /// back into the UI; an empty box on save keeps it.
    pub values: HashMap<String, ParamValueState>,
    /// The secret keys that currently have a stored value (drives the "set" hint and the
    /// leave-blank-to-keep semantics). Non-secret keys never appear here.
    pub secret_stored: HashSet<String>,
    /// Non-secret keys the user has actually edited this session. An untouched optional value is not
    /// written on Save, so a manifest `default` is never materialized into storage just by opening
    /// the pane (a checkbox/dropdown otherwise always projects a concrete value). Required params are
    /// always written regardless.
    pub touched: HashSet<String>,
    pub error: Option<String>,
    /// Set after a successful save so the section can confirm it; cleared on the next edit.
    pub saved: bool,
}

impl ParamConfig {
    /// Builds the editor for `specifier`'s `params`, seeding each non-secret value from the on-disk
    /// param store and recording which secrets are already set. Reads the param files once, at
    /// pane-open time (never from `view`).
    fn seed(server_name: &str, specifier: String, params: Vec<PackageParameter>) -> Self {
        let mut values = HashMap::new();
        let mut secret_stored = HashSet::new();
        for param in &params {
            if is_secret_string(param) {
                if shared_packages::load_secret_param(server_name, &specifier, &param.key).is_some()
                {
                    secret_stored.insert(param.key.clone());
                }
                values.insert(param.key.clone(), ParamValueState::Text(String::new()));
            } else {
                let stored = shared_packages::get_param_value(server_name, &specifier, &param.key);
                values.insert(
                    param.key.clone(),
                    param_values::seed(param, stored.as_ref()),
                );
            }
        }
        Self {
            specifier,
            params,
            values,
            secret_stored,
            touched: HashSet::new(),
            error: None,
            saved: false,
        }
    }
}

/// Whether a param is a secret rendered as a write-only secure box. Secrets are stored as keyring
/// strings, so only a `String` param can be one — a (hand-authored) secret of any other kind falls
/// back to its real value control rather than a misleading secret box. The manifest editor already
/// gates the `secret` flag to `String`, so this only matters for a malformed manifest.
fn is_secret_string(param: &PackageParameter) -> bool {
    param.secret && param.kind == ParamKind::String
}

/// The trimmed text a secret param's box holds (its [`ParamValueState::Text`]), or empty when unset
/// or seeded as a non-text state (never the case for a secret).
fn secret_text(state: Option<&ParamValueState>) -> String {
    match state {
        Some(ParamValueState::Text(text)) => text.trim().to_string(),
        _ => String::new(),
    }
}

/// One persisted parameter value, computed during a validate-then-write save so a validation
/// failure leaves the on-disk values untouched.
enum Persist {
    /// A secret value, written to the OS keyring.
    Secret(String),
    /// A non-secret JSON value, written to `smudgy.params.json`.
    Value(serde_json::Value),
    /// A non-secret value to clear (the box was emptied), so the package reads null and may apply
    /// its own default.
    Clear,
}

impl Persist {
    /// Apply this write for `key` under `specifier` on `server_name`, surfacing any failure as a
    /// display string for the inline error.
    fn write(&self, server_name: &str, specifier: &str, key: &str) -> Result<(), String> {
        let result = match self {
            Persist::Secret(value) => {
                shared_packages::save_secret_param(server_name, specifier, key, value)
            }
            Persist::Value(value) => {
                shared_packages::save_param_value(server_name, specifier, key, value.clone())
            }
            Persist::Clear => shared_packages::clear_param_value(server_name, specifier, key),
        };
        result.map_err(|e| e.to_string())
    }
}

/// A secure text-input field row for a secret parameter, emitting a scalar text edit on `target`.
/// Secrets are write-only (never read back into the box), so this is rendered here rather than by
/// [`param_values::view`]. `clear` appends a Clear button when present (the config editor only).
fn secret_field_row<'a>(
    param: &'a PackageParameter,
    state: Option<&'a ParamValueState>,
    target: ParamTarget,
    placeholder: &str,
    clear: Option<Message>,
) -> Elem<'a> {
    let mut label = param.label.as_deref().unwrap_or(&param.key).to_string();
    if param.required {
        label.push_str(" *");
    }
    let value = match state {
        Some(ParamValueState::Text(text)) => text.as_str(),
        _ => "",
    };
    let key = param.key.clone();
    let input = text_input(placeholder, value)
        .secure(true)
        .on_input(move |v| {
            Message::ParamValueEdit(
                target,
                key.clone(),
                ParamValueEdit::Scalar(ScalarEdit::Text(v)),
            )
        });
    let mut field = row![
        container(text(label).size(13.0)).width(Length::Fixed(140.0)),
        input,
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);
    if let Some(msg) = clear {
        field = field.push(
            button(text("Clear").size(11.0))
                .style(button_style::secondary)
                .on_press(msg),
        );
    }
    field.into()
}

/// How "Edit a copy" settled the fork's enabled state, so [`AutomationsWindow::fork_finished`]
/// can phrase its toast truthfully. A fork **mirrors the source's enabled state**.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkActivation {
    /// The source was enabled and the fork occupies a DISTINCT specifier slot: the fork was
    /// enabled and the original source install removed from the lockfile (the name handoff — the
    /// local fork supersedes it, so it isn't left as a stale second identity for the same leaf name).
    TookOver,
    /// The source was enabled and the fork shares the source's specifier (a self-fork keeping
    /// the leaf name): the single install was enabled — nothing separate was disabled.
    Mirrored,
    /// The fork was left disabled (the source was disabled): an inspect-only local copy until the
    /// author enables it.
    Inactive,
}

/// Decide how a fork's enabled state settles. `activate` is whether the source was enabled;
/// `fork_is_self` is whether the fork shares the source's specifier (a self-fork keeping the
/// leaf name). Pure so the "mirror the source's enabled state, but don't re-disable a shared
/// slot" rule is unit-tested without a live lockfile.
fn fork_activation(activate: bool, fork_is_self: bool) -> ForkActivation {
    match (activate, fork_is_self) {
        (false, _) => ForkActivation::Inactive,
        (true, true) => ForkActivation::Mirrored,
        (true, false) => ForkActivation::TookOver,
    }
}

/// Outcome of an async cloud check over account-owned installs — `delete_owned`'s post-delete
/// check of the deleted package's own entry, or the installed-list sweep over folder-less
/// entries. Drives [`AutomationsWindow::stale_account_installs_checked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StaleInstallCheck {
    /// Nothing is published under the checked name(s): the stale entries were removed from the
    /// lockfile, so the installed list must refresh.
    Pruned,
    /// A published copy exists for a parked (temporarily disabled) entry: it was re-enabled so
    /// the published package takes over from the deleted working copy.
    Restored,
    /// The check couldn't decide (cloud unreachable) or nothing needed doing.
    Unchanged,
}

/// A monotonic generation token for an in-flight install resolve (the stale-result guard). A
/// newtype so it can't be confused with any other counter, and only [`InstallSeq::next`] advances
/// it — callers can't fabricate an arbitrary value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InstallSeq(u64);

impl InstallSeq {
    /// Advances to a fresh generation. Called on `begin_install` and on any action that abandons a
    /// pending install (navigation, Back, another install), invalidating an earlier captured token.
    pub fn bump(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

/// A monotonic generation token for an in-flight **installed-package detail** load (the
/// stale-result guard, mirroring [`InstallSeq`]). The manage pane resolves the open package
/// asynchronously (latest version, closure union, version list); a late result must be discarded if
/// the user has since opened a different package, navigated away, uninstalled, or re-resolved (e.g.
/// changed update mode). Without it, a superseded load could repaint the pane — or worse, fire the
/// silent shrink-branch `record_consent` against a package that is no longer open. Only
/// [`DetailSeq::bump`] advances it, so callers can't fabricate a value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DetailSeq(u64);

impl DetailSeq {
    /// Advances to a fresh generation. Called when a detail load is started or abandoned (opening a
    /// package via `clear_selection`, re-resolving on update-mode change, or uninstalling).
    pub fn bump(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

/// The manage-pane detail-load payload ([`Message::InstalledDetailLoaded`]): the freshly
/// resolved wire package, its pinnable (non-deleted) version list, the closure permission
/// union, the closure `min_smudgy_version` floor, and best-effort cloud rating metadata.
pub type InstalledDetail = (
    ResolvedPackageWire,
    Vec<String>,
    PackagePermissions,
    shared_packages::SmudgyVersionFloor,
    Option<PackageDetail>,
);

/// The outcome of resolving a package for install: the identity plus the things the
/// install-consent flow needs — the **closure** permission union (what the sandboxed isolate
/// will be granted, `PACKAGE-ISOLATES-CONSENT-TRUST.md`), the root manifest's declared params
/// (so a Grant can chain into the required-params prompt without re-resolving), and the
/// transitively-walked `requires`-closure (the required roots co-installed with this package,
/// any cycle warnings, and a peer-conflict refusal when one applies). See
/// `script/REQUIRED-PACKAGES.md`.
#[derive(Debug, Clone)]
pub struct InstallResolution {
    pub specifier: String,
    pub owner: String,
    pub name: String,
    pub version: String,
    /// The whole closure permission union — recorded verbatim as `consented_permissions`
    /// on Grant. Computed by walking the dependency closure (mirrors the engine's `solve_closure`).
    pub permissions: PackagePermissions,
    pub params: Vec<PackageParameter>,
    /// The `requires`-closure walked transitively from this root: each required top-level root,
    /// whether it is already installed/satisfied, its own permission closure, and its missing
    /// required params. Empty when the package requires nothing.
    pub required_roots: Vec<RequiredRoot>,
    /// Cycle warnings from the `requires` walk (a back-edge in the requires graph). Advisory — a
    /// cycle warns, never blocks (`script/REQUIRED-PACKAGES.md`).
    pub cycle_warnings: Vec<String>,
    /// A peer-conflict refusal: when set, the install is **blocked** because no single version of a
    /// required library satisfies every current requirer's range. Carries the explanation.
    pub conflict: Option<String>,
    /// A version-floor refusal: when set, the install is **blocked** because the package's
    /// dependency closure (or a required root's) declares a `min_smudgy_version` above this
    /// smudgy — the engine would refuse it at every load. Carries the
    /// [`SmudgyVersionFloor::refusal`](shared_packages::SmudgyVersionFloor::refusal) reason.
    pub needs_smudgy: Option<String>,
}

/// One required root surfaced by the `requires`-closure walk — a `smudgy://owner/name` that must
/// be installed and running on its own (consumed over the event bus + its types, never imported).
/// Distinct from a `dependencies` edge: it becomes its own top-level lockfile root.
#[derive(Debug, Clone)]
pub struct RequiredRoot {
    pub specifier: String,
    pub name: String,
    /// The version this root would resolve to (the version satisfying the requirers' ranges, or the
    /// already-installed version when it is reused as-is).
    pub version: String,
    /// The required root's own closure permission union (so the consent prompt can show what it
    /// will be able to do), recorded as its `consented_permissions` on Grant.
    pub permissions: PackagePermissions,
    /// The root's declared params (so a Grant can chain the required-params prompt for it too).
    pub params: Vec<PackageParameter>,
    /// Whether a satisfying root is **already installed** — then it is reused as-is (never
    /// downgraded, never re-consented) and only surfaces as an informational line, not an install.
    pub already_satisfied: bool,
    /// Whether installing this root **upgrades** an existing (unsatisfying) install to a version
    /// that meets every requirer's range — surfaced as an upgrade line in the consent prompt.
    pub is_upgrade: bool,
}

/// The always-shown install confirmation (`PACKAGE-ISOLATES-CONSENT-TRUST.md`): an
/// all-or-nothing grant of the closure permission union, enumerating both what the package
/// *will* and *will NOT* be able to do. Shown before any lock entry is written; Cancel
/// writes nothing.
#[derive(Debug, Clone)]
pub struct ConsentPrompt {
    pub specifier: String,
    pub owner: String,
    pub name: String,
    pub version: String,
    /// The closure union the user grants on confirm (recorded as `consented_permissions`).
    pub permissions: PackagePermissions,
    /// The root manifest's params — carried so a Grant can chain straight into the
    /// required-params prompt without a second resolve.
    pub params: Vec<PackageParameter>,
    /// The transitively-walked `requires`-closure: the required top-level roots co-installed with
    /// this package (`script/REQUIRED-PACKAGES.md`). A single grant covers the whole set; on Grant
    /// each not-already-satisfied root is installed via `install_required_package` and consented.
    pub required_roots: Vec<RequiredRoot>,
    /// Cycle warnings from the requires walk — shown advisory; a cycle never blocks the install.
    pub cycle_warnings: Vec<String>,
    /// A peer-conflict refusal: when set, Install is disabled and this message explains why (no
    /// single version of a required library satisfies every requirer's range).
    pub conflict: Option<String>,
    /// A version-floor refusal: when set, Install is disabled and this message explains which
    /// package requires a newer smudgy than this one.
    pub needs_smudgy: Option<String>,
    pub error: Option<String>,
}

/// The update re-prompt (`PACKAGE-ISOLATES-CONSENT-TRUST.md`): a freshly-resolved version
/// of an installed package whose closure union **adds** asks beyond the consented baseline. Only
/// the added lines are shown; until granted, the engine keeps enforcing the old consented union
/// (the new asks are withheld). Surfaced in the manage pane.
#[derive(Debug, Clone)]
pub struct UpdateDelta {
    pub specifier: String,
    pub name: String,
    /// The newest (held-back) version — the one that demands more than was granted.
    pub version: String,
    /// The version actually loaded/running (the highest that fits the grant), from the lockfile's
    /// last-resolved record. `None` if it hasn't loaded yet.
    pub current_version: Option<String>,
    /// The per-field additions over the consented baseline (`PackagePermissions::added_since`).
    pub added: PackagePermissions,
    /// The full new closure union — recorded as the new `consented_permissions` on Grant.
    pub new_union: PackagePermissions,
    /// Why the resolved version can't run on this smudgy (its closure's `min_smudgy_version`
    /// floor refusal), when the version floor — rather than permissions — holds the update
    /// back. The card then explains the floor and offers no grant (granting wouldn't help;
    /// only updating smudgy or pinning an older version would).
    pub needs_smudgy: Option<String>,
}

/// Largest module body the installed-package source browser will fetch and render as text. Real
/// source files are far below this; a blob above it is shown as a "too large to preview"
/// placeholder (size only) rather than pulled into a text widget. The server also caps a single
/// blob at 10 MiB, so this is the UI-side, not the only, bound. Enforced twice: as a pre-fetch
/// gate on the wire `byte_size` (skip the download) and again on the actual fetched length (so a
/// missing/under-reported `byte_size` can't sneak a huge body through).
const SOURCE_PREVIEW_CAP_BYTES: u64 = 1024 * 1024;

/// The two tabs of the installed-package "README & source" area. README is the rendered,
/// publisher-supplied markdown shown full-width; Source is the file-list + on-demand source
/// browser. Defaults to README so the user reviews the description before enabling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InstalledFileTab {
    #[default]
    Readme,
    Source,
}

/// One installed-package module's render state in the source browser. The body is fetched on
/// demand (and integrity-checked against its `content_hash`) the first time the file is selected,
/// then cached by hash. A binary blob or an oversized file is never decoded into the text view —
/// this is the audit-safety guard for the pane users open to inspect a freshly-installed package.
#[derive(Debug, Clone)]
pub enum FilePreview {
    /// Fetch in flight.
    Loading,
    /// Valid UTF-8 source, ready to display. `bidi` flags the presence of Unicode
    /// bidirectional/invisible control characters (the "Trojan Source" class, CVE-2021-42574) so
    /// the view can warn that the rendered order may not match what actually executes.
    Text { source: String, bidi: bool },
    /// Detected as binary (contains a NUL byte or isn't valid UTF-8) — shown as a placeholder.
    Binary { size: u64 },
    /// Above [`SOURCE_PREVIEW_CAP_BYTES`] — shown as a placeholder; not rendered as text.
    TooLarge { size: u64 },
    /// The fetch or its integrity check failed.
    Error(String),
}

/// True if `s` contains a Unicode bidirectional or invisible control character that can make
/// rendered source read differently from what the engine executes ("Trojan Source"). Covers the
/// bidi embeddings/overrides/isolates and marks plus a few zero-width/invisible code points — none
/// of which legitimately appear in source outside of string/comment content, so flagging them in an
/// audit pane is the safe default. We *warn*, not strip: legitimate right-to-left source exists, so
/// the auditor is told to look closely rather than having their text silently rewritten.
fn has_deceptive_unicode(s: &str) -> bool {
    s.chars().any(|c| {
        matches!(c,
            '\u{202A}'..='\u{202E}'   // LRE RLE PDF LRO RLO (embeddings + overrides)
            | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI (isolates)
            | '\u{200E}' | '\u{200F}' | '\u{061C}' // LRM RLM ALM (bidi marks)
            | '\u{200B}'..='\u{200D}' // ZWSP ZWNJ ZWJ
            | '\u{2060}' | '\u{FEFF}' | '\u{00AD}' // word joiner, ZWNBSP/BOM, soft hyphen
        )
    })
}

/// Classify fetched module bytes for the source browser. The declared media type is
/// publisher-controlled, so it is *not* trusted to decide text-vs-binary here — the bytes do:
/// content above the cap is "too large", content with a NUL byte or invalid UTF-8 is "binary",
/// and everything else is decoded source. This means a binary blob is never rendered as mojibake,
/// and a genuine source file a malicious author mislabeled as an image is still shown to the
/// auditor.
fn classify_source(bytes: Vec<u8>) -> FilePreview {
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > SOURCE_PREVIEW_CAP_BYTES {
        return FilePreview::TooLarge { size };
    }
    if bytes.contains(&0) {
        return FilePreview::Binary { size };
    }
    match String::from_utf8(bytes) {
        Ok(source) => {
            let bidi = has_deceptive_unicode(&source);
            FilePreview::Text { source, bidi }
        }
        Err(_) => FilePreview::Binary { size },
    }
}

/// Human-readable byte size for source-browser placeholders ("1.4 KB", "2.0 MB"). Integer math
/// only (no float casts), so it stays clippy-pedantic clean and can't overflow for any `u64`.
fn human_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    let (unit, div) = if bytes >= MB {
        ("MB", MB)
    } else if bytes >= KB {
        ("KB", KB)
    } else {
        return format!("{bytes} B");
    };
    // `bytes % div < div`, so `(bytes % div) * 10 < div * 10 <= 10 MiB * 10` — no overflow.
    let tenth = (bytes % div) * 10 / div;
    format!("{}.{} {}", bytes / div, tenth, unit)
}

/// Resolves a package and folds the **whole dependency-closure** permission union, mirroring the
/// engine's `SmudgyPackageProvider::solve_closure`: every distinct `(owner, name, version)` in the
/// closure contributes its `manifest.permissions` (`PACKAGE-ISOLATES-ENFORCEMENT.md`). The
/// sandboxed isolate is granted exactly this union (recorded as `consented_permissions`), so
/// the consent window must show — and consent must record — the closure union, not just the root
/// manifest. Best-effort, like the engine: a dependency that fails to resolve is skipped rather
/// than aborting (its perms simply don't fold in). Dedups by `(owner, name, version)` so diamonds
/// and cycles terminate.
async fn resolve_install_closure(
    client: &PackageApiClient,
    owner: &str,
    name: &str,
    pinned: Option<&str>,
    installed: &[LockedPackage],
) -> Result<InstallResolution, CloudError> {
    let root = client.resolve_package(owner, name, pinned).await?;
    let (permissions, floor) = closure_permission_union(client, &root).await;
    let params = serde_json::from_value::<PackageManifest>(root.manifest.clone())
        .map(|manifest| manifest.params)
        .unwrap_or_default();
    let specifier = specifier_for(&root.owner_nickname, &root.name);
    // A closure floored above this smudgy blocks the install up front — the engine would
    // refuse it at every load. The requires walk is skipped; nothing co-installs anyway.
    if let Some(reason) = floor.refusal(&shared_packages::running_smudgy_release()) {
        return Ok(InstallResolution {
            specifier,
            owner: root.owner_nickname,
            name: root.name,
            version: root.version,
            permissions,
            params,
            required_roots: Vec::new(),
            cycle_warnings: Vec::new(),
            conflict: None,
            needs_smudgy: Some(reason),
        });
    }
    // Walk this root's `requires`-closure transitively — the required top-level roots co-installed
    // alongside it, any cycle warnings, and a peer-conflict or version-floor refusal if one applies.
    let closure = resolve_required_closure(client, &root, installed).await;
    Ok(InstallResolution {
        specifier,
        owner: root.owner_nickname,
        name: root.name,
        version: root.version,
        permissions,
        params,
        required_roots: closure.roots,
        cycle_warnings: closure.cycle_warnings,
        conflict: closure.conflict,
        needs_smudgy: closure.needs_smudgy,
    })
}

/// The accumulated result of the `requires`-closure walk.
struct RequiredClosure {
    roots: Vec<RequiredRoot>,
    cycle_warnings: Vec<String>,
    conflict: Option<String>,
    /// A required root's closure declares a `min_smudgy_version` above this smudgy — the
    /// whole install is refused (the grant is all-or-nothing and the root couldn't load).
    needs_smudgy: Option<String>,
}

/// Why [`plan_required_root`] refused the whole install — each variant lands in its own
/// [`RequiredClosure`] field so the consent card shows the matching banner.
enum RequiredRefusal {
    /// No single version of the required library satisfies every requirer's range.
    Conflict(String),
    /// The required root's closure is floored above this smudgy.
    NeedsSmudgy(String),
}

/// Walks `root`'s `requires` **transitively** (`script/REQUIRED-PACKAGES.md`): for each
/// `smudgy://owner/name[@range]` in a manifest's `requires`, resolve it, read ITS `requires`, and
/// recurse — de-duping required roots by package key and turning a back-edge (a cycle) into a
/// warning line rather than aborting. For each required root it gathers every requirer's declared
/// range (including the new root's and the still-installed packages' manifests) and applies the
/// peer-conflict policy: a satisfied existing install is reused as-is; an unsatisfied one is
/// upgraded to a single version meeting every range when one exists; if no version satisfies all,
/// the whole install is **refused** (`conflict` set, `script/REQUIRED-PACKAGES.md`).
///
/// Best-effort like the rest of the install path: a required root that fails to resolve is skipped
/// (it can't be co-installed if the registry won't return it) rather than aborting the user's
/// install of the root they actually asked for.
async fn resolve_required_closure(
    client: &PackageApiClient,
    root: &ResolvedPackageWire,
    installed: &[LockedPackage],
) -> RequiredClosure {
    let root_key = (root.owner_nickname.clone(), root.name.clone());
    // Requirer ranges gathered per required (owner, name): every range any package in the closure
    // (or already installed) declares for it, so a single satisfying version can be sought.
    let mut requirer_ranges: HashMap<(String, String), Vec<RequirerRange>> = HashMap::new();
    // Seed with the ranges every *already-installed* package declares for its required roots, so an
    // existing requirer's pin is honored when picking/keeping a shared version (one root per
    // library). Resolving each installed package's manifest is best-effort.
    for pkg in installed {
        let Some((pkg_owner, pkg_name)) = parse_specifier(&pkg.specifier) else {
            continue;
        };
        let pinned = pkg.pinned_version().map(str::to_string);
        let Ok(wire) = client
            .resolve_package(&pkg_owner, &pkg_name, pinned.as_deref())
            .await
        else {
            continue;
        };
        let Ok(manifest) = serde_json::from_value::<PackageManifest>(wire.manifest.clone()) else {
            continue;
        };
        for req in manifest.smudgy_requires() {
            requirer_ranges
                .entry((req.key.owner.clone(), req.key.name.clone()))
                .or_default()
                .push(RequirerRange {
                    requirer: package_display_name(&pkg.specifier).to_string(),
                    range: req.range.clone(),
                });
        }
        // `req` above is a `smudgy_script::PackageDependency`; consumed inline so the type is
        // never named here.
    }

    let mut closure = RequiredClosure {
        roots: Vec::new(),
        cycle_warnings: Vec::new(),
        conflict: None,
        needs_smudgy: None,
    };
    // Required roots already turned into a `RequiredRoot`, keyed by (owner, name), so a diamond in
    // the requires graph yields one entry.
    let mut emitted: HashSet<(String, String)> = HashSet::new();
    // Every package whose `requires` have been walked (the cycle seen-set), seeded with the root.
    let mut walked: HashSet<(String, String)> = HashSet::new();
    walked.insert(root_key.clone());

    // The requires edges still to walk: (the requiring package's display name, the required edge).
    let mut frontier: Vec<(String, RequiresEdge)> = manifest_requires(root)
        .into_iter()
        .map(|edge| (root.name.clone(), edge))
        .collect();

    while let Some((requirer, edge)) = frontier.pop() {
        let key = (edge.owner.clone(), edge.name.clone());
        // Record this edge's range for the conflict computation.
        requirer_ranges
            .entry(key.clone())
            .or_default()
            .push(RequirerRange {
                requirer: requirer.clone(),
                range: edge.range.clone(),
            });
        // A back-edge to something already walked (incl. the root) is a cycle — warn, don't block.
        if walked.contains(&key) {
            let line = format!(
                "{} requires {} \u{2014} a requires cycle (already in the set; not re-walked)",
                requirer, edge.name
            );
            if !closure.cycle_warnings.contains(&line) {
                closure.cycle_warnings.push(line);
            }
            continue;
        }
        if !emitted.insert(key.clone()) {
            // Already turned into a RequiredRoot via another edge (a diamond) — its requires were
            // walked then; only its range (recorded above) still matters.
            continue;
        }
        walked.insert(key.clone());

        let ranges = requirer_ranges.get(&key).cloned().unwrap_or_default();
        match plan_required_root(client, &edge.owner, &edge.name, &ranges, installed).await {
            Ok(Some((wire, plan_root))) => {
                // Recurse into the required root's own requires.
                for sub in manifest_requires(&wire) {
                    frontier.push((wire.name.clone(), sub));
                }
                closure.roots.push(plan_root);
            }
            Ok(None) => {
                // Resolve failed — skip it (best-effort); the user's chosen root still installs.
            }
            Err(refusal) => {
                // A refusal blocks the whole install; the first one found stops the walk
                // with a clear message routed to its matching consent-card banner.
                match refusal {
                    RequiredRefusal::Conflict(message) => closure.conflict = Some(message),
                    RequiredRefusal::NeedsSmudgy(message) => closure.needs_smudgy = Some(message),
                }
                return closure;
            }
        }
    }
    closure
}

/// One `requires` edge flattened to plain strings — the required library's owner/name and its
/// declared range — so the closure walk never names a `smudgy_script` type in `ui`.
struct RequiresEdge {
    owner: String,
    name: String,
    range: Option<String>,
}

/// One requirer's declared range for a required library — the requirer's display name (for the
/// refusal message) and the raw range (`None`/empty = bare, satisfied by any version).
#[derive(Debug, Clone)]
struct RequirerRange {
    requirer: String,
    range: Option<String>,
}

/// The `requires` of an already-resolved package, flattened to plain-string [`RequiresEdge`]s
/// (empty if it declares none or the manifest doesn't parse).
fn manifest_requires(wire: &ResolvedPackageWire) -> Vec<RequiresEdge> {
    serde_json::from_value::<PackageManifest>(wire.manifest.clone())
        .map(|manifest| {
            manifest
                .smudgy_requires()
                .into_iter()
                .map(|dep| RequiresEdge {
                    owner: dep.key.owner,
                    name: dep.key.name,
                    range: dep.range,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Applies the peer-conflict policy to one required library and produces its install plan, or a
/// [`RequiredRefusal`] when the whole install must be blocked: a peer conflict (no single
/// version satisfies every requirer's range) or a version floor (the root's closure requires a
/// newer smudgy than this one, so co-installing it would install something that can't load).
///
/// - If a root for `key` is **already installed** and its resolved version satisfies every range,
///   reuse it as-is (`already_satisfied`, never downgraded).
/// - Otherwise find the highest published version satisfying **every** range; install (or upgrade
///   an existing install) to it.
/// - If no version satisfies all ranges, refuse with `X needs name ^2 but Y needs name ^1`.
///
/// Returns `Ok(None)` when the library can't be resolved at all (best-effort skip). Range matching
/// uses `semver::VersionReq`, the same mechanism as the resolution engine (`package_solver.rs`).
async fn plan_required_root(
    client: &PackageApiClient,
    owner: &str,
    name: &str,
    ranges: &[RequirerRange],
    installed: &[LockedPackage],
) -> Result<Option<(ResolvedPackageWire, RequiredRoot)>, RequiredRefusal> {
    let specifier = specifier_for(owner, name);
    let entry = installed.iter().find(|p| p.specifier == specifier);
    // The installed version, if any: the recorded resolution, or — for a root installed earlier in
    // THIS window session (resolution is recorded only on session load, so the lockfile entry has
    // no `last_resolved_version` yet) — its current resolved version. Without this fallback a
    // just-installed root reads as not-installed and gets falsely re-offered for co-install.
    let existing = if let Some(p) = entry {
        if let Some(version) = p.last_resolved_version.clone() {
            Some(version)
        } else {
            let pinned = p.pinned_version().map(str::to_string);
            client
                .resolve_package(owner, name, pinned.as_deref())
                .await
                .ok()
                .map(|w| w.version)
        }
    } else {
        None
    };

    // An already-installed root whose version satisfies every range is reused untouched.
    if let Some(version) = &existing
        && let Ok(parsed) = semver::Version::parse(version)
        && ranges
            .iter()
            .all(|r| range_admits(r.range.as_deref(), &parsed))
    {
        let Ok(wire) = client.resolve_package(owner, name, Some(version)).await else {
            return Ok(None);
        };
        let root = required_root_from(&wire, client, true, false).await?;
        return Ok(Some((wire, root)));
    }

    // Otherwise seek a single published version satisfying every requirer's range.
    let Ok(latest) = client.resolve_package(owner, name, None).await else {
        return Ok(None);
    };
    let Ok(versions) = client.list_versions(latest.package_id).await else {
        return Ok(None);
    };
    let Some(target) = highest_version_satisfying_all(&versions, ranges) else {
        return Err(RequiredRefusal::Conflict(conflict_message(name, ranges)));
    };
    let Ok(wire) = client.resolve_package(owner, name, Some(&target)).await else {
        return Ok(None);
    };
    // Installed (the lockfile has it) but its version doesn't satisfy every range → an upgrade, even
    // if `existing` couldn't be resolved to a concrete version above.
    let is_upgrade = entry.is_some();
    let root = required_root_from(&wire, client, false, is_upgrade).await?;
    Ok(Some((wire, root)))
}

/// Build a [`RequiredRoot`] from a resolved required package, folding its own closure permission
/// union and reading its declared params. Refuses when that closure's `min_smudgy_version`
/// floor is above this smudgy — a required root that can't load here blocks the whole install
/// (the grant is all-or-nothing), including the reuse of an already-installed root, which the
/// engine is refusing to load for the same reason.
async fn required_root_from(
    wire: &ResolvedPackageWire,
    client: &PackageApiClient,
    already_satisfied: bool,
    is_upgrade: bool,
) -> Result<RequiredRoot, RequiredRefusal> {
    let (permissions, floor) = closure_permission_union(client, wire).await;
    if let Some(reason) = floor.refusal(&shared_packages::running_smudgy_release()) {
        return Err(RequiredRefusal::NeedsSmudgy(reason));
    }
    let params = serde_json::from_value::<PackageManifest>(wire.manifest.clone())
        .map(|manifest| manifest.params)
        .unwrap_or_default();
    Ok(RequiredRoot {
        specifier: specifier_for(&wire.owner_nickname, &wire.name),
        name: wire.name.clone(),
        version: wire.version.clone(),
        permissions,
        params,
        already_satisfied,
        is_upgrade,
    })
}

/// Whether `range` (`None`/empty = bare = any version) admits `version`, via `semver::VersionReq`
/// — the same matcher the resolution engine uses. A malformed range admits nothing (it can't be
/// satisfied), so it surfaces as a conflict rather than silently passing.
fn range_admits(range: Option<&str>, version: &semver::Version) -> bool {
    match range {
        None => true,
        Some(raw) if raw.trim().is_empty() => true,
        Some(raw) => semver::VersionReq::parse(raw).is_ok_and(|req| req.matches(version)),
    }
}

/// The highest non-yanked, non-deleted published version satisfying **every** range in `ranges`
/// (bare ranges admit anything), or `None` when no single version satisfies all. The multi-range
/// generalization of the cloud crate's `highest_satisfying_version` (which intersects a single
/// range): a version is a candidate only if it satisfies all of them, matched via
/// `semver::VersionReq` like the resolution engine.
fn highest_version_satisfying_all(
    versions: &[VersionListItem],
    ranges: &[RequirerRange],
) -> Option<String> {
    let mut best: Option<semver::Version> = None;
    for item in versions {
        if item.yanked || item.deleted {
            continue;
        }
        let Ok(parsed) = semver::Version::parse(&item.version) else {
            continue;
        };
        if ranges
            .iter()
            .all(|r| range_admits(r.range.as_deref(), &parsed))
            && best.as_ref().is_none_or(|b| parsed > *b)
        {
            best = Some(parsed);
        }
    }
    best.map(|v| v.to_string())
}

/// Resolves each installed package's manifest and builds the `requires_of` map
/// `SharedPackageLock::orphaned_by_removal` consumes: specifier → the specifiers it `requires`.
/// Best-effort — an installed package that fails to resolve contributes no edges (it just can't
/// keep anything alive), so an orphan sweep is conservative rather than wrong. Only `requires`
/// edges count toward orphan retention; `dependencies` are imported into the importer's isolate and
/// don't create a top-level root (`script/REQUIRED-PACKAGES.md`).
async fn resolve_requires_of(
    client: &PackageApiClient,
    installed: &[LockedPackage],
) -> HashMap<String, Vec<String>> {
    let mut requires_of: HashMap<String, Vec<String>> = HashMap::new();
    for pkg in installed {
        let Some((owner, name)) = parse_specifier(&pkg.specifier) else {
            continue;
        };
        let pinned = pkg.pinned_version().map(str::to_string);
        let Ok(wire) = client
            .resolve_package(&owner, &name, pinned.as_deref())
            .await
        else {
            continue;
        };
        let requires: Vec<String> = manifest_requires(&wire)
            .into_iter()
            .map(|edge| specifier_for(&edge.owner, &edge.name))
            .collect();
        if !requires.is_empty() {
            requires_of.insert(pkg.specifier.clone(), requires);
        }
    }
    requires_of
}

/// The peer-conflict refusal message: `autoloot needs arctic-prompt ^2 but mapper needs ^1`. Names
/// the two requirers whose ranges can't both be met (the first pair of distinct constrained ranges).
fn conflict_message(name: &str, ranges: &[RequirerRange]) -> String {
    let constrained: Vec<&RequirerRange> = ranges
        .iter()
        .filter(|r| r.range.as_deref().is_some_and(|s| !s.trim().is_empty()))
        .collect();
    if let [first, .., last] = constrained.as_slice() {
        format!(
            "{} needs {name} {} but {} needs {name} {}",
            first.requirer,
            first.range.as_deref().unwrap_or("*"),
            last.requirer,
            last.range.as_deref().unwrap_or("*"),
        )
    } else if let Some(only) = constrained.first() {
        format!(
            "no published version of {name} satisfies {} (required by {})",
            only.range.as_deref().unwrap_or("*"),
            only.requirer,
        )
    } else {
        format!("no published version of {name} satisfies every requirer")
    }
}

/// Folds the whole dependency-closure permission union and `min_smudgy_version` floor starting
/// from an already-resolved `root`, mirroring the engine's `solve_closure` /
/// `closure_union_for`: every distinct `(owner, name, version)` contributes its
/// `manifest.permissions` and its declared floor. Best-effort (a dep that fails to resolve is
/// skipped) and dedups by `(owner, name, version)` so diamonds and cycles terminate. Each dep
/// is resolved at its locked `resolved_version`.
async fn closure_permission_union(
    client: &PackageApiClient,
    root: &ResolvedPackageWire,
) -> (PackagePermissions, shared_packages::SmudgyVersionFloor) {
    let mut union = PackagePermissions::default();
    let mut floor = shared_packages::SmudgyVersionFloor::default();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    // (owner, name, resolved_version) of closure nodes still to fold; the root is folded inline.
    let mut stack: Vec<(String, String, String)> = Vec::new();

    let fold = |wire: &ResolvedPackageWire,
                union: &mut PackagePermissions,
                floor: &mut shared_packages::SmudgyVersionFloor| {
        if let Ok(manifest) = serde_json::from_value::<PackageManifest>(wire.manifest.clone()) {
            union.merge(&manifest.permissions);
            floor.fold(&wire.name, manifest.min_smudgy_version.as_deref());
        }
    };

    seen.insert((
        root.owner_nickname.clone(),
        root.name.clone(),
        root.version.clone(),
    ));
    fold(root, &mut union, &mut floor);
    for dep in &root.dependencies {
        stack.push((
            dep.owner_nickname.clone(),
            dep.name.clone(),
            dep.resolved_version.clone(),
        ));
    }
    while let Some((dep_owner, dep_name, dep_version)) = stack.pop() {
        if !seen.insert((dep_owner.clone(), dep_name.clone(), dep_version.clone())) {
            continue;
        }
        // Resolve the dep at its locked version; a failure is non-fatal (engine parity).
        let Ok(wire) = client
            .resolve_package(&dep_owner, &dep_name, Some(&dep_version))
            .await
        else {
            continue;
        };
        fold(&wire, &mut union, &mut floor);
        for dep in &wire.dependencies {
            stack.push((
                dep.owner_nickname.clone(),
                dep.name.clone(),
                dep.resolved_version.clone(),
            ));
        }
    }
    (union, floor)
}

/// How dangerous one granted permission is — the tier that drives the consent/pane styling, so
/// the display conveys *risk*, not just a flat list (the deno-style framing: some grants are
/// scoped capabilities, some are the whole computer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PermissionRisk {
    /// A scoped grant that does what the line says and nothing more.
    Normal,
    /// Elevated exposure (reading files outside the package's own data folder, downloading
    /// arbitrary web code to run) — flagged amber, but still contained by the sandbox.
    Caution,
    /// Sandbox-escape-equivalent: subprocesses (`run`), native code (`ffi`), or writes outside
    /// `$DATA`. A subprocess or native library runs with the user's full privileges, and an
    /// outside write can rewrite config/scripts/other packages — whatever the line says, the
    /// honest summary is "effectively full access".
    Critical,
}

/// A single "can do" / "cannot do" line in the consent enumeration.
struct PermissionLine {
    /// The capability label (e.g. `"connect to"`, `"read"`), or the categorical denial.
    head: String,
    /// The specific target (host/path/var/program), when the line lists one.
    detail: Option<String>,
    /// How this line should be framed (colors + the full-access banner roll-up).
    risk: PermissionRisk,
}

/// The `import` "can do" line for the consent enumeration — one line whose wording follows the
/// tri-state [`ImportPolicy`]. `None` shows nothing (the "cannot" list covers it).
fn import_can_line(policy: ImportPolicy) -> Option<&'static str> {
    match policy {
        ImportPolicy::None => None,
        ImportPolicy::Registries => Some("download & run code from public registries (npm, jsr)"),
        ImportPolicy::Any => Some("download & run code from anywhere on the web"),
    }
}

/// The "this package will be able to" lines for a granted union: one per host/path/var/program.
/// Empty when the package asks for nothing — callers phrase the no-access case in context (see
/// `sandbox_summary`). Lines carry a [`PermissionRisk`] so the rows and the callers' full-access
/// banner ([`union_risk`]) agree on what's scoped and what's effectively unlimited.
fn permission_can_lines(perms: &PackagePermissions) -> Vec<PermissionLine> {
    let mut lines = Vec::new();
    // Hosts dedup case-insensitively (DNS is case-insensitive) so the list shows no near-dupes,
    // keeping each host's first-seen spelling.
    let mut seen_hosts: HashSet<String> = HashSet::new();
    for host in &perms.net {
        if seen_hosts.insert(host.trim().to_lowercase()) {
            lines.push(PermissionLine {
                head: "connect to".to_string(),
                detail: Some(host.clone()),
                risk: PermissionRisk::Normal,
            });
        }
    }
    // `import` is a separate axis from `net`: it downloads third-party code to RUN (sandboxed, but
    // not visible in the package source you reviewed), rather than opening a data connection.
    // Registry code is at least published/auditable; "anywhere on the web" is not — amber.
    if let Some(head) = import_can_line(perms.import) {
        lines.push(PermissionLine {
            head: head.to_string(),
            detail: None,
            risk: if perms.import == ImportPolicy::Any {
                PermissionRisk::Caution
            } else {
                PermissionRisk::Normal
            },
        });
    }
    // Only advertise read/write/ffi paths the engine will actually grant: a `$DATA/..` entry is
    // dropped by the enforcement guardrail (it would escape the data dir), so it isn't a real
    // capability. A path OUTSIDE the package's own data folder changes the line's meaning: a read
    // reaches the user's files (privacy), a write can rewrite config/scripts/other packages — the
    // unbox — so it is flagged, not listed as if it were a scoped grant.
    for path in perms.read.iter().filter(|p| path_grant_enforced(p)) {
        let scoped = data_scoped(path);
        lines.push(PermissionLine {
            head: if scoped {
                "read"
            } else {
                "read (outside its data folder)"
            }
            .to_string(),
            detail: Some(pretty_path(path)),
            risk: if scoped {
                PermissionRisk::Normal
            } else {
                PermissionRisk::Caution
            },
        });
    }
    for path in perms.write.iter().filter(|p| path_grant_enforced(p)) {
        let scoped = data_scoped(path);
        lines.push(PermissionLine {
            head: if scoped {
                "write"
            } else {
                "write (outside its data folder)"
            }
            .to_string(),
            detail: Some(pretty_path(path)),
            risk: if scoped {
                PermissionRisk::Normal
            } else {
                PermissionRisk::Critical
            },
        });
    }
    for var in &perms.env {
        lines.push(PermissionLine {
            head: "read environment variable".to_string(),
            detail: Some(var.clone()),
            risk: PermissionRisk::Normal,
        });
    }
    // Subprocesses and native libraries run OUTSIDE the sandbox with your full privileges —
    // always critical, however narrow the listed target looks.
    for program in &perms.run {
        lines.push(PermissionLine {
            head: "run the program".to_string(),
            detail: Some(program.clone()),
            risk: PermissionRisk::Critical,
        });
    }
    for path in perms.ffi.iter().filter(|p| path_grant_enforced(p)) {
        lines.push(PermissionLine {
            head: "load the native library".to_string(),
            detail: Some(pretty_path(path)),
            risk: PermissionRisk::Critical,
        });
    }
    // System details roll into one line (fingerprinting-grade info, not a capability).
    if !perms.sys.is_empty() {
        lines.push(PermissionLine {
            head: "read system details".to_string(),
            detail: Some(perms.sys.join(", ")),
            risk: PermissionRisk::Normal,
        });
    }
    // The granted smudgy op-capabilities (one row each, no target list).
    lines.extend(smudgy_can_lines(&perms.smudgy));
    lines
}

/// A smudgy op-capability "can do" line with no target list (the head text is the whole label).
fn cap_line(head: &str) -> PermissionLine {
    PermissionLine {
        head: head.to_string(),
        detail: None,
        risk: PermissionRisk::Normal,
    }
}

/// Whether a `read`/`write`/`ffi` entry stays inside the package's OWN data folder (the `$DATA`
/// placeholder). Callers filter `..`-escapes with [`path_grant_enforced`] first, so a `$DATA/…`
/// entry seen here really is contained; a `$DATA`-lookalike (`$DATABASE`) or an absolute path is
/// not. Outside-`$DATA` grants are what change a file permission from "its own storage" to "your
/// computer" — the risk cliff the consent framing keys on. `pub(super)` so the manifest editor
/// warns the author on the same predicate installers will be warned on.
pub(super) fn data_scoped(entry: &str) -> bool {
    let Some(rest) = entry.trim().strip_prefix("$DATA") else {
        return false;
    };
    matches!(rest.chars().next(), None | Some('/' | '\\'))
}

/// The highest [`PermissionRisk`] across a union's lines — what decides whether a pane shows the
/// full-access banner over the enumeration.
fn union_risk(perms: &PackagePermissions) -> PermissionRisk {
    permission_can_lines(perms)
        .iter()
        .map(|line| line.risk)
        .max()
        .unwrap_or(PermissionRisk::Normal)
}

/// The specific sandbox-escape grants in a union, phrased for the full-access banner ("it can
/// {a}, {b}"). Empty iff the union has no [`PermissionRisk::Critical`] line.
fn escape_reasons(perms: &PackagePermissions) -> Vec<&'static str> {
    let mut reasons = Vec::new();
    if !perms.run.is_empty() {
        reasons.push("run other programs");
    }
    if perms.ffi.iter().any(|p| path_grant_enforced(p)) {
        reasons.push("load native code");
    }
    if perms
        .write
        .iter()
        .any(|p| path_grant_enforced(p) && !data_scoped(p))
    {
        reasons.push("write files outside its own data folder");
    }
    reasons
}

/// The "effectively full access" banner shown over a permission enumeration whose union contains a
/// sandbox-escape grant ([`escape_reasons`]). One honest paragraph instead of letting a
/// scoped-looking line (`run git`) read like a scoped grant: programs it runs, native code it
/// loads, and files it writes outside its data folder are NOT sandboxed. `None` when the union has
/// no critical grant.
fn full_access_banner<'a>(perms: &PackagePermissions) -> Option<Elem<'a>> {
    let reasons = escape_reasons(perms);
    if reasons.is_empty() {
        return None;
    }
    Some(
        container(
            column![
                row![
                    text("\u{26A0}").size(14.0).style(common::danger),
                    text("Effectively full access").size(14.0).style(common::danger),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
                text(format!(
                    "Because this package can {}, which are outside of its sandbox in smudgy, it \
                    will be able to affect your computer in ways the sandbox in smudgy cannot offer protection from. \
                    Be certain that you trust it before enabling it.",
                    join_reasons(&reasons)
                ))
                .size(12.0),
            ]
            .spacing(6.0),
        )
        .padding(12.0)
        .width(Length::Fill)
        .style(common::banner_style)
        .into(),
    )
}

/// Join escape reasons into prose: `a`, `a and b`, `a, b, and c`.
fn join_reasons(reasons: &[&str]) -> String {
    match reasons {
        [one] => (*one).to_string(),
        [a, b] => format!("{a} and {b}"),
        [head @ .., last] => format!("{}, and {last}", head.join(", ")),
        [] => String::new(),
    }
}

/// The smudgy op-capability "can do" rows for the consent window
/// (`PACKAGE-ISOLATES-OP-CAPABILITIES.md`), in a stable grouped order. `send`/`send-direct` are
/// one combined, nuanced row whose wording depends on which (neither/either/both) is granted
/// ([`send_can_line`]); `change_display` describes what it can do (hide/restyle/inject/replace)
/// plainly; `reach-others` is a normal row, not flagged high-risk.
fn smudgy_can_lines(caps: &SmudgyCapabilities) -> Vec<PermissionLine> {
    let mut out = Vec::new();
    if caps.create_aliases {
        out.push(cap_line("Create aliases"));
    }
    if caps.create_triggers {
        out.push(cap_line("Create triggers"));
    }
    if let Some(line) = send_can_line(caps) {
        out.push(cap_line(&line));
    }
    if caps.echo {
        out.push(cap_line("Echo text to the screen"));
    }
    if caps.reach_others {
        out.push(cap_line("Interact with other open sessions"));
    }
    if caps.change_display {
        out.push(cap_line(
            "Hide, restyle, inject, or replace game text; as well as see the current line",
        ));
    }
    if caps.mapper_read {
        out.push(cap_line("Read your maps"));
    }
    if caps.mapper_write {
        out.push(cap_line("Change your maps"));
    }
    if caps.widgets {
        out.push(cap_line("Create and change on-screen widgets"));
    }
    if caps.panes {
        out.push(cap_line(
            "Create session output panes and route game lines into them",
        ));
    }
    if caps.gmcp_send {
        out.push(cap_line(
            "Send GMCP messages to the game and manage GMCP modules",
        ));
    }
    if caps.input {
        out.push(cap_line(
            "See and rewrite what you type in the command input, including \
             submitting commands and switching it into password mode",
        ));
    }
    out
}

/// The combined `send` / `send-direct` "can do" line: the wording changes with which of the two is
/// granted — both, send-only (through your aliases, which it can re-trigger), or direct-only
/// (bypassing them). `None` when neither is granted (the "cannot send" row covers that case).
fn send_can_line(caps: &SmudgyCapabilities) -> Option<String> {
    let line = match (caps.send, caps.send_direct) {
        (true, true) => {
            "Send commands to the game both as if you typed them (through your aliases, \
             which they can re-trigger) and directly (bypassing your aliases)"
        }
        (true, false) => {
            "Send commands to the game as if you typed them, possibly triggering aliases"
        }
        (false, true) => "Send commands straight to the game, bypassing your aliases",
        (false, false) => return None,
    };
    Some(line.to_string())
}

/// The combined `send` / `send-direct` "cannot do" line — shown only when NEITHER is granted (if
/// either is, [`send_can_line`] already conveys the scope).
fn send_cannot_line(caps: &SmudgyCapabilities) -> Option<&'static str> {
    match (caps.send, caps.send_direct) {
        (false, false) => Some("send commands to the game"),
        _ => None,
    }
}

/// The "cannot do" lines for the un-granted smudgy capabilities — what a sandboxed package
/// can NOT do, reinforcing the sandbox guarantee; a zero-capability package surfaces all of them.
fn smudgy_cannot_lines(caps: &SmudgyCapabilities) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !caps.create_aliases {
        out.push("create aliases".to_string());
    }
    if !caps.create_triggers {
        out.push("create triggers that act on game output".to_string());
    }
    if let Some(line) = send_cannot_line(caps) {
        out.push(line.to_string());
    }
    if !caps.echo {
        out.push("echo text to the screen".to_string());
    }
    if !caps.reach_others {
        out.push("reach your other sessions".to_string());
    }
    if !caps.change_display {
        out.push("interact with the game's terminal (hide, style, inject, or replace text; or see the current line)".to_string());
    }
    if !caps.mapper_read {
        out.push("read your maps".to_string());
    }
    if !caps.mapper_write {
        out.push("change your maps".to_string());
    }
    if !caps.widgets {
        out.push("create on-screen widgets".to_string());
    }
    if !caps.panes {
        out.push("create or access other panes".to_string());
    }
    if !caps.interop_write {
        out.push("broadcast events or publish shared state".to_string());
    }
    if !caps.interop_read {
        out.push("listen for events or read shared state".to_string());
    }
    if !caps.gmcp_send {
        out.push("send GMCP messages to the game".to_string());
    }
    if !caps.input {
        out.push("see or change what you type in the command input".to_string());
    }
    out
}

/// One-line summary of a fully-sandboxed package with no granted access — the calm "nothing to
/// worry about" register, shown wherever the consented union is empty.
fn sandbox_summary() -> &'static str {
    "Runs fully sandboxed — no access to your files, network, or system."
}

/// The sandbox guarantees that still HOLD for this union — the closing rows of the "cannot"
/// list. These used to be unconditional ("never grantable"), but `run`/`ffi` and outside-`$DATA`
/// file grants are now declarable, so each guarantee is computed from the union rather than
/// promised falsely:
///
/// - "native code / other programs" holds only while `run` and `ffi` are empty;
/// - "your other packages' data" holds only while, additionally, every enforced file grant stays
///   inside the package's own `$DATA` (an absolute-path read/write could reach another package's
///   storage — and a subprocess could reach anything). Note this line is about *running state*,
///   not code: a sandboxed package may still `import "smudgy://…"` its own declared dependencies —
///   those load into its own isolate.
fn sandbox_guarantee_lines(perms: &PackagePermissions) -> Vec<&'static str> {
    let mut lines = Vec::new();
    let no_native = perms.run.is_empty() && !perms.ffi.iter().any(|p| path_grant_enforced(p));
    if no_native {
        lines.push("load native code / run other programs");
    }
    let fs_contained = perms
        .read
        .iter()
        .chain(&perms.write)
        .filter(|p| path_grant_enforced(p))
        .all(|p| data_scoped(p));
    if no_native && fs_contained {
        lines.push("read or change the data of your other packages or scripts");
    }
    lines
}

/// The "this package will NOT be able to" lines for a sandboxed package: the categorical
/// denial for each empty deno capability, plus the still-true guarantee rows
/// ([`sandbox_guarantee_lines`]) that make the sandbox legible.
fn permission_cannot_lines(perms: &PackagePermissions) -> Vec<String> {
    let mut lines = Vec::new();
    // The net assurance must not over-promise. A package granted `import` (but not `net`) can still
    // reach the network to DOWNLOAD CODE from its listed sources, so a flat "no internet at all"
    // would be false. When that's the case, scope the assurance to what genuinely stays denied —
    // opening or accepting network connections — and name the code-download carve-out.
    if perms.net.is_empty() {
        if perms.import.is_none() {
            lines.push("connect to the internet / any server".to_string());
        } else {
            lines.push(
                "connect to or receive connections over the network (it can still download code \
                 to run, as noted above)"
                    .to_string(),
            );
        }
    }
    // `import` and `net` are independent: granting `net` never grants the ability to pull in new
    // code, so this assurance holds even for a net-enabled package.
    if perms.import.is_none() {
        lines.push("download or run code from npm, jsr, or the web".to_string());
    }
    // "cannot read/write" when no grant SURVIVES enforcement — so a package whose only path grant is
    // a dropped `$DATA/..` reads as "cannot", consistent with the (filtered) can-list above.
    if !perms.read.iter().any(|p| path_grant_enforced(p)) {
        lines.push("read your files".to_string());
    }
    if !perms.write.iter().any(|p| path_grant_enforced(p)) {
        lines.push("modify your files".to_string());
    }
    if perms.env.is_empty() {
        lines.push("read environment variables".to_string());
    }
    if perms.sys.is_empty() {
        lines.push("read details about your computer (hostname, OS, …)".to_string());
    }
    // The un-granted smudgy op-capabilities (send/echo/automations/display/mapper/widgets).
    lines.extend(smudgy_cannot_lines(&perms.smudgy));
    lines.extend(
        sandbox_guarantee_lines(perms)
            .iter()
            .map(|s| (*s).to_string()),
    );
    lines
}

/// Prettifies a permission path for display: the `$DATA` placeholder (host-expanded before
/// enforcement) reads as `<data>` rather than a raw env-style token.
fn pretty_path(path: &str) -> String {
    path.replace("$DATA", "<data>")
}

/// Whether a `read`/`write` path grant survives the enforcement guardrail
/// (`PACKAGE-ISOLATES-ENFORCEMENT.md`, mirroring `script_engine::expand_data_placeholder`): a
/// `$DATA/<sub>` (or `$DATA\<sub>`) whose subpath contains a `..` component is **dropped** by the
/// engine (it would let the manifest escape the data dir), so the consent window must not advertise
/// it as a capability. A bare `$DATA`, a `$DATA`-lookalike (`$DATABASE`), or a non-placeholder
/// absolute path is the author's own explicit grant and is kept.
fn path_grant_enforced(entry: &str) -> bool {
    let Some(rest) = entry.strip_prefix("$DATA") else {
        return true;
    };
    let sub = match rest.chars().next() {
        None => return true, // bare `$DATA`
        Some('/' | '\\') => rest.trim_start_matches(['/', '\\']),
        Some(_) => return true, // `$DATABASE` etc. — not the placeholder
    };
    !sub.split(['/', '\\']).any(|component| component == "..")
}

/// Whether the owned package's manifest version can be published. Drives the Publish
/// button's enabled state and the explanation banner (the semver-fluent UX).
#[derive(Debug, Clone)]
enum PublishVerdict {
    /// Valid, unused semver — publishing is allowed.
    Ready,
    /// `manifest.version` isn't a publishable semver (unparseable or carries build
    /// metadata); carries the reason to show the author.
    Invalid(String),
    /// The number is already published (live, yanked, or deleted). Numbers are
    /// permanently reserved and can never be reused.
    AlreadyUsed,
}

/// Decide whether `version` (the manifest version) may be published, given the package's
/// already-published versions (which now includes yanked + hard-deleted numbers). The
/// server is the source of truth; this mirrors its rule so the UI can pre-empt the 409
/// and explain why Publish is disabled. Comparison is canonical-vs-canonical, matching
/// the server's reservation key.
fn publish_verdict(version: &str, published: &[VersionListItem]) -> PublishVerdict {
    let Ok(parsed) = semver::Version::parse(version) else {
        return PublishVerdict::Invalid(format!(
            "\u{201c}{version}\u{201d} is not a valid semver version (e.g. 1.2.3). Edit the version in the Manifest section above."
        ));
    };
    if !parsed.build.is_empty() {
        return PublishVerdict::Invalid(
            "Version must not include build metadata (drop the +\u{2026} suffix).".to_string(),
        );
    }
    let canonical = parsed.to_string();
    if published.iter().any(|v| v.version == canonical) {
        return PublishVerdict::AlreadyUsed;
    }
    PublishVerdict::Ready
}

// ============================================================================
// Dependency graph
// ============================================================================

impl AutomationsWindow {
    /// Rebuilds the direct/owned sets from the current lists, preserving the
    /// async-resolved `requires`/`resolved` maps and the user's enable intent.
    pub(super) fn rebuild_graph(&mut self) {
        self.graph.direct.clear();
        self.graph.owned.clear();
        for pkg in &self.installed_packages {
            self.graph.direct.insert(pkg.specifier.clone());
            // Seed the enable intent from the persisted lockfile flag (the engine's source of
            // truth), so a package installed "don't enable" — or toggled off — shows disabled and
            // is held out of execution until enabled.
            self.graph.intent.insert(pkg.specifier.clone(), pkg.enabled);
            if let Some(v) = &pkg.last_resolved_version {
                self.graph.resolved.insert(pkg.specifier.clone(), v.clone());
            }
        }
        // Owned (local) packages: pull their declared deps from the manifest.
        for name in self.local_packages.clone() {
            let spec = format!("local:{name}");
            self.graph.owned.insert(spec.clone());
            if let Ok(Some(pkg)) = local_packages::load_local_package(&self.server_name, &name) {
                let edges = pkg
                    .manifest
                    .dependencies
                    .iter()
                    .map(|d| DepEdge {
                        specifier: d.clone(),
                        range: String::new(),
                    })
                    .collect();
                self.graph.requires.insert(spec, edges);
            }
        }
    }

    /// Resolves each installed package once to populate its `requires` edges
    /// (best-effort; failures leave the tree flat). Resolution is public, so the dependency
    /// graph fills in for installed public packages whether or not an account is signed in.
    pub(super) fn resolve_graph_deps(&self) -> Task<Message> {
        let mut tasks = Vec::new();
        for pkg in &self.installed_packages {
            if self.graph.requires.contains_key(&pkg.specifier) {
                continue;
            }
            let Some((owner, name)) = parse_specifier(&pkg.specifier) else {
                continue;
            };
            let spec = pkg.specifier.clone();
            let pinned = pkg.pinned_version().map(str::to_string);
            let client = self.package_client();
            tasks.push(Task::perform(
                async move {
                    let resolved = client
                        .resolve_package(&owner, &name, pinned.as_deref())
                        .await?;
                    // Fold the newest resolvable version's closure union too, so the tree can flag
                    // an update that's blocked because it needs more permissions than were granted.
                    // (The version floor isn't surfaced in the graph; the manage pane covers it.)
                    let (union, _floor) = closure_permission_union(&client, &resolved).await;
                    Ok::<_, CloudError>((resolved, union))
                },
                move |result| Message::InstalledResolvedForGraph(spec.clone(), result),
            ));
        }
        Task::batch(tasks)
    }

    pub(super) fn installed_resolved_for_graph(
        &mut self,
        spec: &str,
        result: Result<(ResolvedPackageWire, PackagePermissions), CloudError>,
    ) -> Update<Message, Event> {
        if let Ok((resolved, union)) = result {
            self.graph
                .resolved
                .insert(spec.to_string(), resolved.version.clone());
            let edges = resolved
                .dependencies
                .iter()
                .map(|d| DepEdge {
                    specifier: specifier_for(&d.owner_nickname, &d.name),
                    range: d.range.clone(),
                })
                .collect();
            self.graph.requires.insert(spec.to_string(), edges);
            for dep in &resolved.dependencies {
                self.graph.resolved.insert(
                    specifier_for(&dep.owner_nickname, &dep.name),
                    dep.resolved_version.clone(),
                );
            }
            // Blocked-update detection: the newest version's closure union exceeds
            // the consented grant, so the engine holds the package back (or won't load it). Trusted
            // packages run allow-all — never blocked.
            let blocked = self
                .installed_packages
                .iter()
                .find(|p| p.specifier == spec)
                .is_some_and(|p| {
                    !p.trusted
                        && !union.is_within(&p.consented_permissions.clone().unwrap_or_default())
                });
            if blocked {
                self.blocked_updates.insert(spec.to_string());
            } else {
                self.blocked_updates.remove(spec);
            }
        }
        Update::none()
    }

    pub(super) fn toggle_package_enabled(&mut self, spec: String) -> Update<Message, Event> {
        if !self.graph.controllable(&spec) {
            return Update::none();
        }
        let before = self.effective_set();
        let cur = self.graph.intent.get(&spec).copied().unwrap_or(false);
        let new_enabled = !cur;
        // Persist the flag for installed (lockfile) packages so the engine honors it on reload;
        // owned/derived nodes keep the in-memory-only behavior. A persist failure aborts the toggle.
        let is_installed_pkg = self.installed_packages.iter().any(|p| p.specifier == spec);
        if is_installed_pkg
            && let Err(e) = shared_packages::set_enabled(&self.server_name, &spec, new_enabled)
        {
            self.manage_feedback = Some(format!("Failed to update enabled state: {e}"));
            return Update::none();
        }
        self.graph.intent.insert(spec.clone(), new_enabled);
        if let Some(pkg) = self
            .installed_packages
            .iter_mut()
            .find(|p| p.specifier == spec)
        {
            pkg.enabled = new_enabled;
        }
        let after = self.effective_set();
        let name = package_display_name(&spec).to_string();

        let toast = if cur {
            // Turning off: deps that turned off as a result.
            let dropped: Vec<String> = before
                .difference(&after)
                .filter(|s| **s != spec)
                .map(|s| package_display_name(s).to_string())
                .collect();
            if dropped.is_empty() {
                format!("Disabled {name}.")
            } else {
                format!(
                    "Disabled {name} + {} (no longer required).",
                    dropped.join(", ")
                )
            }
        } else {
            let added: Vec<String> = after
                .difference(&before)
                .filter(|s| **s != spec)
                .map(|s| package_display_name(s).to_string())
                .collect();
            if added.is_empty() {
                format!("Enabled {name}.")
            } else {
                format!("Enabled {name} + {} (dependencies).", added.join(", "))
            }
        };
        // Reload the live session so the engine re-partitions (loads/drops the package now) — only
        // for installed packages, whose enabled flag the engine reads. No reconnect needed.
        let event = is_installed_pkg.then(|| Event::ScriptsChanged {
            server_name: self.server_name.clone(),
        });
        Update::new(self.show_toast(toast), event)
    }

    /// Make `target_spec` the active member of a same-name group: enable it (installing its own
    /// specifier first if it's a local that isn't in the lockfile yet) and disable every sibling.
    /// This is the fork radio-handoff lifted to the navigator, so the user can switch which
    /// same-named package is live without re-forking. Reloads the live session.
    pub(super) fn set_active_member(
        &mut self,
        target_spec: String,
        siblings: Vec<String>,
    ) -> Update<Message, Event> {
        let in_lock = self
            .installed_packages
            .iter()
            .any(|p| p.specifier == target_spec);
        let result = if in_lock {
            shared_packages::set_enabled(&self.server_name, &target_spec, true)
        } else {
            shared_packages::install_package(
                &self.server_name,
                &target_spec,
                UpdateMode::Auto,
                true,
            )
        };
        if let Err(e) = result {
            return Update::with_task(self.show_toast(format!("Couldn't switch: {e}")));
        }
        for sib in &siblings {
            if sib != &target_spec {
                let _ = shared_packages::set_enabled(&self.server_name, sib, false);
            }
        }
        let toast = self.show_toast(format!(
            "Switched to {}.",
            package_display_name(&target_spec)
        ));
        Update::new(
            Task::batch([
                Task::done(Message::LoadInstalledPackages),
                Task::done(Message::LoadLocalPackages),
                toast,
            ]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    /// Enable/disable a lone (non-colliding) local package from the tree. A local "runs" iff there
    /// is an enabled lockfile install of its own `smudgy://<you>/<name>` specifier, so enabling
    /// installs+enables it and disabling clears the flag (the folder stays on disk).
    pub(super) fn toggle_local_enabled(&mut self, name: String) -> Update<Message, Event> {
        let own_spec = self.local_own_spec(&name);
        let active = self.graph.effectively_enabled(&own_spec);
        let in_lock = self
            .installed_packages
            .iter()
            .any(|p| p.specifier == own_spec);
        let result = if active {
            shared_packages::set_enabled(&self.server_name, &own_spec, false)
        } else if in_lock {
            shared_packages::set_enabled(&self.server_name, &own_spec, true)
        } else {
            shared_packages::install_package(&self.server_name, &own_spec, UpdateMode::Auto, true)
        };
        if let Err(e) = result {
            return Update::with_task(self.show_toast(format!("Couldn't update {name}: {e}")));
        }
        let toast = self.show_toast(format!(
            "{} {name}.",
            if active { "Disabled" } else { "Enabled" }
        ));
        Update::new(
            Task::batch([
                Task::done(Message::LoadInstalledPackages),
                Task::done(Message::LoadLocalPackages),
                toast,
            ]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    fn effective_set(&self) -> HashSet<String> {
        let mut set = HashSet::new();
        for spec in self.graph.direct.iter().chain(self.graph.owned.iter()) {
            if self.graph.effectively_enabled(spec) {
                set.insert(spec.clone());
            }
            for edge in self.graph.requires.get(spec).into_iter().flatten() {
                if self.graph.effectively_enabled(&edge.specifier) {
                    set.insert(edge.specifier.clone());
                }
            }
        }
        set
    }
}

// ============================================================================
// Installed package — update side
// ============================================================================

impl AutomationsWindow {
    pub(super) fn open_installed_package(&mut self, specifier: String) -> Update<Message, Event> {
        let selection = Selection::InstalledPackage(specifier.clone());
        self.open_installed_package_with_selection(specifier, selection)
    }

    /// Open an installed package reached via a nested dependency-reference row. Same pane as
    /// [`Self::open_installed_package`], but the selection stays keyed to the clicked reference
    /// so only that row highlights.
    pub(super) fn open_dependency(
        &mut self,
        parent: String,
        specifier: String,
    ) -> Update<Message, Event> {
        let selection = Selection::Dependency {
            parent,
            spec: specifier.clone(),
        };
        self.open_installed_package_with_selection(specifier, selection)
    }

    fn open_installed_package_with_selection(
        &mut self,
        specifier: String,
        selection: Selection,
    ) -> Update<Message, Event> {
        self.clear_selection();
        self.installed_detail = None;
        // Drop the prior package's rating so the meta row doesn't flash the previous package's
        // stars/installs during this package's async detail load; repopulated when the resolve lands.
        self.installed_rating = None;
        self.installed_versions.clear();
        self.installed_selected_file = None;
        // Default to the README tab so the user reviews the description before enabling.
        self.installed_file_tab = InstalledFileTab::Readme;
        // Bound the content-addressed source cache to the open package's files. Late fetches from a
        // prior package would only ever re-insert their own (hash-verified) bytes, so this is for
        // memory, not correctness.
        self.installed_source.clear();
        // Drop the prior package's README so the audit pane shows a clean "No README." placeholder
        // during this package's async detail load rather than flashing the previously-viewed
        // package's description (`local_readme` is shared with the owned pane and only repopulated
        // when the resolve below lands).
        self.local_readme = None;
        self.manage_feedback = None;
        self.selection = selection;
        let locked = self
            .installed_packages
            .iter()
            .find(|p| p.specifier == specifier)
            .cloned();
        // Open the pane even for a package that isn't a direct lockfile install (e.g. a transitive
        // dependency) so it can be inspected and forked ("Edit a copy"). The synthetic lock entry
        // is transient (never persisted); detail loads via resolve, gated to smudgy:// specifiers.
        let open =
            locked.unwrap_or_else(|| LockedPackage::new(specifier.clone(), UpdateMode::Auto));
        self.installed_open = Some(Box::new(open));
        self.pane = Pane::InstalledPackage;
        self.load_installed_detail(&specifier)
    }

    /// Open the read-only detail pane for a script-created (module/package) automation. The
    /// pattern/body are read live from `self.live` at view time, so this just records the key.
    pub(super) fn open_creator_automation(
        &mut self,
        creator_id: String,
        kind: AutomationKind,
        name: String,
    ) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::CreatorAutomation {
            creator_id: creator_id.clone(),
            kind,
            name: name.clone(),
        };
        self.pane = Pane::CreatorAutomation {
            creator_id,
            kind,
            name,
        };
        Update::none()
    }

    /// Resolves a creator-tree node id (`module:<subpath>` / `package:<spec>`) to its live
    /// automations, mirroring how the sidebar looked them up to render the node.
    fn creator_automations(&self, creator_id: &str) -> Option<&CreatorAutomations> {
        if let Some(subpath) = creator_id.strip_prefix("module:") {
            self.live.module(subpath)
        } else if let Some(spec) = creator_id.strip_prefix("package:") {
            let (owner, name) = parse_specifier(spec)?;
            self.live.package(&owner, &name)
        } else {
            None
        }
    }

    /// The message that navigates to the module/package that created an automation, for the
    /// detail pane's "open creator" affordance.
    fn creator_jump(creator_id: &str) -> Option<Message> {
        if let Some(subpath) = creator_id.strip_prefix("module:") {
            Some(Message::SelectModule(subpath.to_string()))
        } else {
            creator_id
                .strip_prefix("package:")
                .map(|spec| Message::SelectInstalledPackage(spec.to_string()))
        }
    }

    fn load_installed_detail(&mut self, specifier: &str) -> Update<Message, Event> {
        let Some((owner, name)) = parse_specifier(specifier) else {
            return Update::none();
        };
        let pinned = self
            .installed_packages
            .iter()
            .find(|p| p.specifier == specifier)
            .and_then(|p| p.pinned_version().map(str::to_string));
        let client = self.package_client();
        self.manage_busy = true;
        self.manage_feedback = None;
        // Tag this load with the current detail generation; a result that arrives after the user has
        // moved on (opened another package, navigated away, uninstalled, or re-resolved) carries a
        // stale token and is discarded in `installed_detail_loaded`.
        let seq = self.detail_seq;
        Update::with_task(Task::perform(
            async move {
                let resolved = client
                    .resolve_package(&owner, &name, pinned.as_deref())
                    .await?;
                // Fold the closure union too, so the manage pane can detect an update that adds
                // permission asks beyond the consented baseline (delta re-prompt), and the
                // version floor, so it can explain a version held back by `min_smudgy_version`.
                let (permissions, floor) = closure_permission_union(&client, &resolved).await;
                let versions = client.list_versions(resolved.package_id).await?;
                // Best-effort cloud metadata (rating average/count, install count) for the meta row.
                // Public read, so it works logged out; a failure just leaves the rating UI hidden
                // rather than failing the whole detail load.
                let rating = client.get_package(resolved.package_id).await.ok();
                // Exclude hard-deleted numbers: their content is gone (would resolve 404),
                // so they must never be offered as a pin target. (Yanked stays — it's still
                // resolvable by an exact pin.)
                Ok((
                    resolved,
                    versions
                        .into_iter()
                        .filter(|v| !v.deleted)
                        .map(|v| v.version)
                        .collect(),
                    permissions,
                    floor,
                    rating,
                ))
            },
            move |result| Message::InstalledDetailLoaded(seq, result),
        ))
    }

    /// Select a module file in the Source tab and start loading its source. This is for actual
    /// modules only; the rendered metadata README lives in its own tab (`installed_file_tab`), so a
    /// module that happens to be named `README.md` still routes here and has its real source fetched.
    pub(super) fn select_installed_file(&mut self, subpath: String) -> Update<Message, Event> {
        self.installed_selected_file = Some(subpath);
        self.ensure_selected_source()
    }

    /// Ensure the currently-selected Source-tab file's source is cached or in flight, returning the
    /// fetch task when one is started. Idempotent: a file already loaded/loading no-ops, and an empty
    /// selection (`None` — nothing picked in the Source tab) no-ops. Called when a file is selected,
    /// when the Source tab is opened, *and* again after a re-resolve swaps the module set — a new
    /// version changes content hashes, so the open file must re-fetch. A module is NOT special-cased
    /// by subpath here: a package may ship one literally named `README.md`, and the auditor must be
    /// able to read its actual source rather than the (separate, publisher-supplied) metadata README.
    pub(super) fn ensure_selected_source(&mut self) -> Update<Message, Event> {
        let Some(subpath) = self.installed_selected_file.clone() else {
            return Update::none();
        };
        let Some(module) = self
            .installed_detail
            .as_deref()
            .and_then(|detail| detail.modules.iter().find(|m| m.subpath == subpath))
        else {
            return Update::none();
        };
        let hash = module.content_hash.clone();
        // Content-addressed cache: a successful or in-flight entry → nothing to do (the view reads it
        // back by the selected file's hash, so a late fetch needs no staleness token). A cached
        // *error*, though, is retried: a one-off network blip must not permanently brick a file's
        // preview in the pane whose whole job is letting the user read the source before trusting it.
        if matches!(
            self.installed_source.get(&hash),
            Some(
                FilePreview::Loading
                    | FilePreview::Text { .. }
                    | FilePreview::Binary { .. }
                    | FilePreview::TooLarge { .. }
            )
        ) {
            return Update::none();
        }
        // Pre-fetch size gate: skip downloading a blob the view won't render anyway. The fetched
        // length is re-checked in `classify_source`, so an absent/under-reported `byte_size` here
        // can't smuggle an oversized body through.
        if u64::try_from(module.byte_size).is_ok_and(|n| n > SOURCE_PREVIEW_CAP_BYTES) {
            let size = u64::try_from(module.byte_size).unwrap_or(u64::MAX);
            self.installed_source
                .insert(hash, FilePreview::TooLarge { size });
            return Update::none();
        }
        let url = module.content_url.clone();
        let fetch_hash = hash.clone();
        let client = self.package_client();
        self.installed_source
            .insert(hash.clone(), FilePreview::Loading);
        Update::with_task(Task::perform(
            async move {
                // `fetch_module_bytes` verifies the body against `content_hash`, so a tampered or
                // corrupt blob fails here rather than being shown as trusted source.
                client
                    .fetch_module_bytes(&url, &fetch_hash)
                    .await
                    .map(classify_source)
            },
            move |result| Message::InstalledSourceLoaded {
                hash: hash.clone(),
                result,
            },
        ))
    }

    pub(super) fn installed_source_loaded(
        &mut self,
        hash: String,
        result: Result<FilePreview, CloudError>,
    ) -> Update<Message, Event> {
        let preview = result.unwrap_or_else(|e| FilePreview::Error(display_error(&e)));
        self.installed_source.insert(hash, preview);
        Update::none()
    }

    pub(super) fn installed_detail_loaded(
        &mut self,
        seq: DetailSeq,
        result: Result<InstalledDetail, CloudError>,
    ) -> Update<Message, Event> {
        // Discard a superseded load: the open package changed (another package opened, navigation,
        // uninstall) or was re-resolved while this was in flight. Returning before touching
        // `manage_busy` leaves the newer in-flight load's spinner intact, and — critically — keeps
        // the silent shrink-branch `record_consent` below from firing for a package that is no
        // longer open (it would otherwise rewrite consent for the wrong, superseded package).
        if seq != self.detail_seq {
            return Update::none();
        }
        self.manage_busy = false;
        match result {
            Ok((resolved, versions, permissions, floor, rating)) => {
                // Cloud rating/install metadata for the meta row (best-effort; `None` just hides it).
                self.installed_rating = rating.map(Box::new);
                // Always track the resolved version's README (the pane defaults to it so the user
                // reviews before enabling). Refreshing unconditionally — not only when the README is
                // the current selection — keeps it in sync across a re-resolve: otherwise a pin/update
                // change while a source file is selected would leave the README sub-tab showing the
                // previous version's text under the new version's header.
                self.local_readme = resolved.readme.as_deref().map(markdown::Content::parse);
                // Feed the dependency graph (and the blocked-update flag, via the closure union).
                if let Some(spec) = self.installed_open.as_ref().map(|p| p.specifier.clone()) {
                    self.installed_resolved_for_graph(
                        &spec,
                        Ok((resolved.clone(), permissions.clone())),
                    );
                }
                // Update re-prompt: compare this freshly-resolved version's closure union
                // against the consented baseline. A trusted package runs allow-all, so consent is
                // moot — neither path applies.
                self.update_delta = None;
                let open_info = self
                    .installed_open
                    .as_deref()
                    .filter(|open| !open.trusted)
                    .map(|open| {
                        (
                            open.specifier.clone(),
                            open.consented_permissions.clone().unwrap_or_default(),
                            open.last_resolved_version.clone(),
                        )
                    });
                // A resolved version whose closure floor is above this smudgy is refused or
                // held back by the engine no matter what is granted, so the version card
                // takes precedence over any permission delta — and, unlike the delta, it
                // applies to TRUSTED installs too (no grant is involved, only the floor).
                let floored = floor.refusal(&shared_packages::running_smudgy_release());
                if let Some(reason) = floored {
                    if let Some(open) = self.installed_open.as_deref() {
                        self.update_delta = Some(UpdateDelta {
                            specifier: open.specifier.clone(),
                            name: package_display_name(&open.specifier).to_string(),
                            version: resolved.version.clone(),
                            current_version: open.last_resolved_version.clone(),
                            added: PackagePermissions::default(),
                            new_union: permissions,
                            needs_smudgy: Some(reason),
                        });
                    }
                } else if let Some((spec, baseline, current_version)) = open_info {
                    let added = permissions.added_since(&baseline);
                    if added.is_empty() {
                        // No new asks. If the union actually SHRANK (a previously-consented entry
                        // is gone), silently adopt the smaller union (auto-accept) so the
                        // consented baseline tracks the manifest and never over-grants; the engine
                        // keeps enforcing the consented union, so this only ever narrows access.
                        let removed = baseline.added_since(&permissions);
                        if !removed.is_empty()
                            && shared_packages::record_consent(
                                &self.server_name,
                                &spec,
                                &permissions,
                            )
                            .is_ok()
                        {
                            if let Some(pkg) = self
                                .installed_packages
                                .iter_mut()
                                .find(|p| p.specifier == spec)
                            {
                                pkg.consented_permissions = Some(permissions.clone());
                            }
                            if let Some(open) = &mut self.installed_open {
                                open.consented_permissions = Some(permissions.clone());
                            }
                        }
                    } else {
                        // New asks beyond the consented set — surface the delta. Until the user
                        // accepts, the engine keeps enforcing the OLD consented union, withholding
                        // the new asks.
                        self.update_delta = Some(UpdateDelta {
                            specifier: spec.clone(),
                            name: package_display_name(&spec).to_string(),
                            version: resolved.version.clone(),
                            current_version,
                            added,
                            new_union: permissions,
                            needs_smudgy: None,
                        });
                    }
                }
                // Seed the inline "Settings" editor from the resolved version's declared params
                // (re-seeded on every resolve, so a version that adds/removes params stays in step).
                // A dependency-reference view configures nothing of its own, so it's left unseeded.
                let params = serde_json::from_value::<PackageManifest>(resolved.manifest.clone())
                    .map(|manifest| manifest.params)
                    .unwrap_or_default();
                self.installed_detail = Some(Box::new(resolved));
                self.installed_versions = versions;
                if matches!(self.selection, Selection::Dependency { .. }) {
                    self.param_config = None;
                } else if let Some(spec) = self.installed_open.as_ref().map(|p| p.specifier.clone())
                {
                    self.seed_param_config(spec, params);
                }
                // A re-resolve (e.g. a version-pin change) can swap the module set out from under a
                // still-selected file, changing its content hash. Re-fetch the open file so the
                // source pane tracks the version now shown instead of stalling on "Fetching…".
                self.ensure_selected_source()
            }
            Err(e) => {
                self.manage_feedback = Some(display_error(&e));
                Update::none()
            }
        }
    }

    /// Sets the caller's 1–5 star rating for the open installed cloud package. The package id comes
    /// from the resolved detail (always present while the manage pane is open); an account is
    /// required server-side, so the view gates the star control on `signed_in()`.
    pub(super) fn rate_installed_package(&self, stars: i16) -> Update<Message, Event> {
        let Some(detail) = self.installed_detail.as_deref() else {
            return Update::none();
        };
        let package_id = detail.package_id;
        let client = self.package_client();
        Update::with_task(Task::perform(
            async move { client.rate_package(package_id, stars).await },
            Message::InstalledRatingUpdated,
        ))
    }

    pub(super) fn installed_rating_updated(
        &mut self,
        result: Result<PackageDetail, CloudError>,
    ) -> Update<Message, Event> {
        match result {
            // The server returns the fresh rating average/count, so the meta row updates in place.
            Ok(detail) => self.installed_rating = Some(Box::new(detail)),
            Err(e) => self.manage_feedback = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn set_installed_update_mode(&mut self, mode: UpdateMode) -> Update<Message, Event> {
        let Some(specifier) = self.installed_open.as_ref().map(|p| p.specifier.clone()) else {
            return Update::none();
        };
        if let Err(e) =
            shared_packages::set_update_mode(&self.server_name, &specifier, mode.clone())
        {
            self.manage_feedback = Some(format!("Failed to set update mode: {e}"));
            return Update::none();
        }
        if let Some(pkg) = self
            .installed_packages
            .iter_mut()
            .find(|p| p.specifier == specifier)
        {
            pkg.mode = mode.clone();
        }
        if let Some(open) = &mut self.installed_open {
            open.mode = mode;
        }
        // Re-resolving supersedes any still-in-flight detail load for this package: bump the
        // generation so the prior load's late result is discarded and only this re-resolve applies.
        self.detail_seq.bump();
        let reload = self.load_installed_detail(&specifier);
        Update::new(
            reload.task,
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    /// Begin the uninstall flow: open the confirmation and, in the background, compute the apt-style
    /// orphan set (the auto-installed required roots nothing else would need once this package is
    /// removed). The confirmation shows immediately; the orphan list fills in when the resolve lands
    /// (`UninstallOrphansComputed`). Resolving the installed packages' `requires` is what lets
    /// `SharedPackageLock::orphaned_by_removal` run (`script/REQUIRED-PACKAGES.md`).
    pub(super) fn request_uninstall(&mut self) -> Update<Message, Event> {
        self.confirm_uninstall = true;
        self.uninstall_orphans.clear();
        self.uninstall_breaks.clear();
        let Some(specifier) = self.installed_open.as_ref().map(|p| p.specifier.clone()) else {
            return Update::none();
        };
        let client = self.package_client();
        let installed = self.installed_packages.clone();
        Update::with_task(Task::perform(
            async move {
                let requires_of = resolve_requires_of(&client, &installed).await;
                let lock = shared_packages::SharedPackageLock {
                    packages: installed,
                };
                let plan = lock.plan_removal(&specifier, &requires_of);
                (plan.breaks, plan.orphans)
            },
            |(breaks, orphans)| Message::UninstallPlanComputed { breaks, orphans },
        ))
    }

    pub(super) fn uninstall_installed(&mut self) -> Update<Message, Event> {
        let Some(specifier) = self.installed_open.as_ref().map(|p| p.specifier.clone()) else {
            return Update::none();
        };
        // Computed before the lockfile entry is dropped: if an enabled package still requires this
        // one, removing the standalone install leaves it resolved as a dependency, so the toast
        // says "removed the standalone install" rather than "uninstalled" (which would read as a
        // no-op since the package is still there).
        let survives = !self.graph.enabled_dependents(&specifier).is_empty();
        if let Err(e) = shared_packages::uninstall_package(&self.server_name, &specifier) {
            self.manage_feedback = Some(format!("Uninstall failed: {e}"));
            return Update::none();
        }
        // Also remove the dependents that `require` this package (forced — they'd break without it)
        // and the orphaned auto-installed roots the user didn't keep (apt-style; never silent). A
        // failure on one is surfaced but doesn't abort — the chosen package is already gone.
        let breaks = std::mem::take(&mut self.uninstall_breaks);
        let orphans = std::mem::take(&mut self.uninstall_orphans);
        let mut also_removed: Vec<String> = Vec::new();
        for spec in breaks.iter().chain(orphans.iter()) {
            match shared_packages::uninstall_package(&self.server_name, spec) {
                Ok(()) => also_removed.push(package_display_name(spec).to_string()),
                Err(e) => {
                    self.manage_feedback = Some(format!(
                        "Removed {}, but failed to remove {}: {e}",
                        package_display_name(&specifier),
                        package_display_name(spec)
                    ));
                }
            }
        }
        self.confirm_uninstall = false;
        // The package is gone: discard any in-flight detail load for it so its late result can't
        // repaint the (now-closed) pane or record consent for the removed package.
        self.detail_seq.bump();
        self.installed_open = None;
        self.installed_detail = None;
        self.installed_rating = None;
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        let name = package_display_name(&specifier);
        let toast = self.show_toast(if survives {
            format!("Removed the standalone install of {name}; it stays installed as a dependency.")
        } else if also_removed.is_empty() {
            format!("Uninstalled {name}.")
        } else {
            format!("Uninstalled {name} + {}.", also_removed.join(", "))
        });
        Update::new(
            Task::batch([Task::done(Message::LoadInstalledPackages), toast]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    pub(super) fn fork_installed(&mut self) -> Update<Message, Event> {
        let Some(source_specifier) = self.installed_open.as_ref().map(|p| p.specifier.clone())
        else {
            self.manage_feedback = Some("No package selected.".to_string());
            return Update::none();
        };
        let Some(resolved) = self.installed_detail.clone() else {
            self.manage_feedback = Some("Package detail not loaded yet.".to_string());
            return Update::none();
        };

        // Keep the source's leaf name; de-dup only if a local package of that name already exists.
        let source_leaf = package_display_name(&source_specifier).to_string();
        let new_name = self.derive_fork_name(&source_leaf);

        // The fork mirrors the source's enabled state: an enabled source yields an active fork,
        // a disabled one stays an inspect-only copy. The fork runs under its own-handle specifier
        // (`local_own_spec`): the account nickname when signed in, else the reserved `local`
        // owner — the same identity the enable toggle uses, so activation works signed out too.
        let source_enabled = shared_packages::load_lock(&self.server_name)
            .ok()
            .and_then(|lock| lock.find(&source_specifier).map(|p| p.enabled))
            .unwrap_or(true);
        let fork_specifier = self.local_own_spec(&new_name);
        let activate = source_enabled;

        let client = self.package_client();
        let server = self.server_name.clone();
        self.manage_busy = true;
        self.manage_feedback = Some(format!(
            "Copying to a local package \u{201c}{new_name}\u{201d}\u{2026}"
        ));
        Update::with_task(Task::perform(
            async move {
                let mut modules = Vec::new();
                for module in &resolved.modules {
                    // Fetch as raw bytes so a fork copies binary modules faithfully too.
                    let body = client
                        .fetch_module_bytes(&module.content_url, &module.content_hash)
                        .await
                        .map_err(|e| e.to_string())?;
                    modules.push(LocalModule {
                        subpath: module.subpath.clone(),
                        content: body,
                    });
                }
                let manifest: PackageManifest = serde_json::from_value(resolved.manifest.clone())
                    .map_err(|e| format!("parse manifest: {e}"))?;
                local_packages::fork_to_local(&server, &new_name, &manifest, &modules)
                    .map_err(|e| e.to_string())?;
                // Carry the source's README into the fork (resolve includes it), so a forked
                // package isn't left without one.
                if let Some(readme) = resolved.readme.as_deref() {
                    local_packages::write_local_file(&server, &new_name, "README.md", readme)
                        .map_err(|e| e.to_string())?;
                }
                // Mirror the source's enabled state. A self-fork keeping the leaf name shares the
                // source's specifier slot (now resolving to the local folder), so installing it
                // enabled IS the mirror — the old unconditional "disable the source" step would
                // re-disable that single entry, the bug that left a self-fork disabled.
                let fork_is_self = fork_specifier == source_specifier;
                let activation = fork_activation(activate, fork_is_self);
                if !matches!(activation, ForkActivation::Inactive) {
                    shared_packages::install_package(
                        &server,
                        &fork_specifier,
                        UpdateMode::Auto,
                        true,
                    )
                    .map_err(|e| e.to_string())?;
                    if matches!(activation, ForkActivation::TookOver) {
                        // Distinct slot: the local fork supersedes the original install, so remove
                        // the original from the lockfile entirely. (Merely disabling it left a stale
                        // entry that resurfaced — at its persisted older version — when the local
                        // copy was later deleted, and lingered as a second resolvable identity for
                        // the same leaf name.)
                        shared_packages::uninstall_package(&server, &source_specifier)
                            .map_err(|e| e.to_string())?;
                    }
                }
                Ok((new_name, activation))
            },
            Message::ForkFinished,
        ))
    }

    /// Keeps the source's leaf name, de-duping against existing local packages only on a
    /// collision (`boo`, then `boo-2`, `boo-3`, …), case-folded like the filesystem.
    fn derive_fork_name(&self, source_leaf: &str) -> String {
        let taken = |candidate: &str| {
            self.local_packages
                .iter()
                .any(|n| n.eq_ignore_ascii_case(candidate))
        };
        if !taken(source_leaf) {
            return source_leaf.to_string();
        }
        let mut i = 2;
        loop {
            let candidate = format!("{source_leaf}-{i}");
            if !taken(&candidate) {
                return candidate;
            }
            i += 1;
        }
    }

    pub(super) fn fork_finished(
        &mut self,
        result: Result<(String, ForkActivation), String>,
    ) -> Update<Message, Event> {
        self.manage_busy = false;
        match result {
            Ok((name, activation)) => {
                let (feedback, toast) = match activation {
                    ForkActivation::TookOver => (
                        format!(
                            "Editing a copy named \u{201c}{name}\u{201d} — it's now active and the original installed copy was removed."
                        ),
                        format!("Now editing {name} (installed copy removed)."),
                    ),
                    ForkActivation::Mirrored => (
                        format!(
                            "Editing a copy named \u{201c}{name}\u{201d} — your local copy is now active."
                        ),
                        format!("Now editing {name}."),
                    ),
                    ForkActivation::Inactive => (
                        format!(
                            "Editing a copy named \u{201c}{name}\u{201d} — left disabled so you can read it; enable it to run."
                        ),
                        format!("Created local copy {name} (disabled)."),
                    ),
                };
                self.manage_feedback = Some(feedback);
                let toast = self.show_toast(toast);
                let tasks = vec![
                    Task::done(Message::LoadLocalPackages),
                    Task::done(Message::LoadInstalledPackages),
                    Task::done(Message::SelectOwnedPackage(name)),
                    toast,
                ];
                // An active fork (took over or mirrored) changed the enabled set — reload the
                // running session. An inactive fork only wrote a folder, so no reload is needed.
                if matches!(activation, ForkActivation::Inactive) {
                    Update::with_task(Task::batch(tasks))
                } else {
                    Update::new(
                        Task::batch(tasks),
                        Some(Event::ScriptsChanged {
                            server_name: self.server_name.clone(),
                        }),
                    )
                }
            }
            Err(e) => {
                self.manage_feedback = Some(format!("Couldn't edit a copy: {e}"));
                Update::none()
            }
        }
    }

    /// Opens the selected local package's folder in the OS file manager (Explorer/Finder/…), so
    /// the author can drag files in, open it in an external editor, or use git. Toasts on failure
    /// rather than silently doing nothing.
    pub(super) fn reveal_package_folder(&mut self) -> Update<Message, Event> {
        let name = match self.local_package.as_deref() {
            Some(package) => package.name.clone(),
            None => return Update::none(),
        };
        let dir = match local_packages::packages_dir(&self.server_name) {
            Ok(dir) => dir.join(&name),
            Err(e) => {
                return Update::with_task(
                    self.show_toast(format!("Couldn't locate the folder: {e}")),
                );
            }
        };
        if !dir.exists() {
            return Update::with_task(
                self.show_toast("That package folder doesn't exist yet.".to_string()),
            );
        }
        if let Err(e) = open::that(&dir) {
            return Update::with_task(self.show_toast(format!("Couldn't open the folder: {e}")));
        }
        Update::none()
    }

    pub(super) fn start_rename_owned(&mut self) -> Update<Message, Event> {
        if let Some(package) = self.local_package.as_deref() {
            self.rename_buffer = Some(package.name.clone());
            self.authoring_feedback = None;
        }
        Update::none()
    }

    /// Commits the inline rename: rename the folder (+ its fork sidecar), then migrate any lockfile
    /// install of its `smudgy://<you>/<name>` specifier so an active local package keeps resolving
    /// under its new name. Renaming a fork off the source's name is also what unblocks publishing.
    pub(super) fn commit_rename_owned(&mut self) -> Update<Message, Event> {
        let Some(new_name) = self.rename_buffer.as_ref().map(|s| s.trim().to_string()) else {
            return Update::none();
        };
        let Some(old_name) = self.local_package.as_deref().map(|p| p.name.clone()) else {
            self.rename_buffer = None;
            return Update::none();
        };
        if new_name == old_name {
            self.rename_buffer = None;
            return Update::none();
        }
        if let Err(message) = naming::validate_package_name(&new_name) {
            self.authoring_feedback = Some(message);
            return Update::none();
        }
        if let Err(e) =
            local_packages::rename_local_package(&self.server_name, &old_name, &new_name)
        {
            self.authoring_feedback = Some(format!("Rename failed: {e}"));
            return Update::none();
        }

        // Migrate the lockfile install if this local package was active under its own specifier (a
        // fork that took over, or any enabled local package), so the engine keeps resolving it.
        // The entry's owner segment depends on the sign-in state it was created under — the
        // account nickname, or the reserved `local` placeholder when signed out — so both forms
        // are migrated.
        let mut session_changed = false;
        let mut owners = vec![local_packages::LOCAL_OWNER.to_string()];
        if let Some(nick) = self.cloud.snapshot.get().nickname_text() {
            owners.push(nick);
        }
        for owner in owners {
            let old_spec = specifier_for(&owner, &old_name);
            let new_spec = specifier_for(&owner, &new_name);
            let migrate = shared_packages::load_lock(&self.server_name)
                .ok()
                .and_then(|lock| lock.find(&old_spec).map(|p| (p.mode.clone(), p.enabled)));
            if let Some((mode, enabled)) = migrate {
                let _ =
                    shared_packages::install_package(&self.server_name, &new_spec, mode, enabled);
                let _ = shared_packages::uninstall_package(&self.server_name, &old_spec);
                session_changed = true;
            }
        }

        self.rename_buffer = None;
        self.authoring_feedback = None;
        let toast = self.show_toast(format!("Renamed to {new_name}."));
        let tasks = Task::batch([
            Task::done(Message::LoadLocalPackages),
            Task::done(Message::LoadInstalledPackages),
            Task::done(Message::SelectOwnedPackage(new_name)),
            toast,
        ]);
        if session_changed {
            Update::new(
                tasks,
                Some(Event::ScriptsChanged {
                    server_name: self.server_name.clone(),
                }),
            )
        } else {
            Update::with_task(tasks)
        }
    }

    // ---- trust toggle ------------------------------------------------------

    pub(super) fn request_trust(&mut self) -> Update<Message, Event> {
        self.confirm_trust = true;
        Update::none()
    }

    pub(super) fn cancel_trust(&mut self) -> Update<Message, Event> {
        self.confirm_trust = false;
        Update::none()
    }

    /// Flips the package's `trusted` flag. Trusting promotes it onto the allow-all main
    /// isolate (heavy — confirmed in the UI first); untrusting returns it to its sandbox + last
    /// consented union. Either way it takes effect on the next session reload — there is no live
    /// isolate migration — so the toast says so rather than implying an instant change.
    pub(super) fn set_trusted(&mut self, trusted: bool) -> Update<Message, Event> {
        let Some(specifier) = self.installed_open.as_ref().map(|p| p.specifier.clone()) else {
            return Update::none();
        };
        self.confirm_trust = false;
        if let Err(e) = shared_packages::set_trusted(&self.server_name, &specifier, trusted) {
            self.manage_feedback = Some(format!("Failed to update trust: {e}"));
            return Update::none();
        }
        // Mirror the flip into the in-memory copies so the pane reflects it immediately.
        if let Some(pkg) = self
            .installed_packages
            .iter_mut()
            .find(|p| p.specifier == specifier)
        {
            pkg.trusted = trusted;
        }
        if let Some(open) = &mut self.installed_open {
            open.trusted = trusted;
        }
        // A trusted package runs allow-all, so any pending update delta is moot.
        if trusted {
            self.update_delta = None;
        }
        let name = package_display_name(&specifier).to_string();
        let toast = self.show_toast(if trusted {
            format!("Unsandboxed {name}")
        } else {
            format!("Sandboxed {name}")
        });
        Update::new(
            toast,
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    // ---- owned (local) package sandbox -------------------------------------

    /// Jump straight into the manifest editor's Capabilities tab from the owned-package pane. For a
    /// local package the manifest IS the grant table, so this is the "grant capabilities" affordance.
    pub(super) fn edit_owned_capabilities(&mut self) -> Update<Message, Event> {
        let update = self.begin_manifest_edit();
        self.manifest_tab = ManifestTab::Capabilities;
        update
    }

    /// Toggle "develop unsandboxed" for the open local package — the author-only escape hatch that
    /// runs it allow-all on the main isolate (the `trusted` flag), for capabilities a sandbox can
    /// never grant (`ffi`/`run`). Enabling installs + enables the package's own specifier and trusts
    /// it; disabling returns it to its manifest-scoped sandbox. Reloads the live session.
    pub(super) fn set_local_unsandboxed(&mut self, unsandboxed: bool) -> Update<Message, Event> {
        self.confirm_trust = false;
        let Some(name) = self.local_package.as_ref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        let own_spec = self.local_own_spec(&name);
        let in_lock = self
            .installed_packages
            .iter()
            .any(|p| p.specifier == own_spec);
        let result = if unsandboxed {
            // Ensure it's installed + enabled, then trust it (allow-all on the main isolate).
            if in_lock {
                shared_packages::set_enabled(&self.server_name, &own_spec, true)
            } else {
                shared_packages::install_package(
                    &self.server_name,
                    &own_spec,
                    UpdateMode::Auto,
                    true,
                )
            }
            .and_then(|()| shared_packages::set_trusted(&self.server_name, &own_spec, true))
        } else if in_lock {
            shared_packages::set_trusted(&self.server_name, &own_spec, false)
        } else {
            Ok(())
        };
        if let Err(e) = result {
            return Update::with_task(self.show_toast(format!("Couldn't update {name}: {e}")));
        }
        let toast = self.show_toast(if unsandboxed {
            format!("{name} now runs unsandboxed (full access).")
        } else {
            format!("{name} returned to its manifest sandbox.")
        });
        Update::new(
            Task::batch([
                Task::done(Message::LoadInstalledPackages),
                Task::done(Message::LoadLocalPackages),
                toast,
            ]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    // ---- update re-prompt --------------------------------------------------

    /// Grant & update: adopt the new closure union as the consented baseline. The package is
    /// already installed, so `record_consent` updates the existing lock entry. Takes effect on the
    /// next reload (the engine reads the consented union at session start).
    pub(super) fn grant_update(&mut self) -> Update<Message, Event> {
        let Some(delta) = self.update_delta.take() else {
            return Update::none();
        };
        // A version-floor hold-back has no grant (its card offers only dismissal): consenting
        // wouldn't load the held-back version, so don't rewrite the baseline.
        if delta.needs_smudgy.is_some() {
            self.update_delta = Some(delta);
            return Update::none();
        }
        if let Err(e) =
            shared_packages::record_consent(&self.server_name, &delta.specifier, &delta.new_union)
        {
            self.manage_feedback = Some(format!("Failed to record consent: {e}"));
            // Re-show the delta so the user can retry.
            self.update_delta = Some(delta);
            return Update::none();
        }
        if let Some(pkg) = self
            .installed_packages
            .iter_mut()
            .find(|p| p.specifier == delta.specifier)
        {
            pkg.consented_permissions = Some(delta.new_union.clone());
        }
        if let Some(open) = &mut self.installed_open {
            open.consented_permissions = Some(delta.new_union);
        }
        let toast = self.show_toast(format!("Updated permissions for {}", delta.name));
        Update::new(
            toast,
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    pub(super) fn dismiss_update(&mut self) -> Update<Message, Event> {
        // "Keep current perms": write nothing. The engine keeps enforcing the old consented union,
        // so the new asks stay withheld — this only hides the prompt.
        self.update_delta = None;
        Update::none()
    }
}

// ============================================================================
// Owned (local) package — update side
// ============================================================================

impl AutomationsWindow {
    pub(super) fn new_package(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.selection = Selection::None;
        self.pane = Pane::NewPackage {
            name: String::new(),
            error: None,
        };
        Update::none()
    }

    pub(super) fn open_owned_package(&mut self, name: String) -> Update<Message, Event> {
        self.clear_selection();
        self.owned_selected_file = None;
        self.authoring_feedback = None;
        self.share_package_id = None;
        self.share_friends.clear();
        self.share_grants.clear();
        self.share_versions.clear();
        self.share_feedback = None;
        self.selection = Selection::OwnedPackage(name.clone());
        match local_packages::load_local_package(&self.server_name, &name) {
            Ok(Some(package)) => {
                self.local_readme = package.readme.as_deref().map(markdown::Content::parse);
                self.manifest_draft = Some(ManifestDraft::from_manifest(&package.manifest));
                self.manifest_dirty = false;
                self.manifest_tab = ManifestTab::default();
                // Seed the inline "Settings" editor from the saved manifest's params, keyed by the
                // local package's own-handle specifier (what the runtime resolves it under).
                let spec = self.local_own_spec(&name);
                let params = package.manifest.params.clone();
                self.local_package = Some(Box::new(package));
                self.pane = Pane::OwnedPackage;
                self.seed_param_config(spec, params);
            }
            Ok(None) => {
                self.pane = Pane::Error(std::sync::Arc::new(vec![format!(
                    "Package '{name}' not found"
                )]));
                return Update::none();
            }
            Err(e) => {
                self.pane = Pane::Error(std::sync::Arc::new(vec![format!(
                    "Failed to load package '{name}': {e}"
                )]));
                return Update::none();
            }
        }
        // Load the cloud share state (if published + signed in).
        if !self.signed_in() {
            return Update::none();
        }
        self.share_busy = true;
        let pkg_client = self.package_client();
        let cloud_client = self.cloud.client.clone();
        Update::with_task(Task::perform(
            async move {
                let mine = pkg_client.list_my_packages().await?;
                let detail = mine
                    .into_iter()
                    .find(|p| p.package.name == name)
                    .ok_or(CloudError::NotFoundOrNoAccess)?;
                let id = detail.package.id;
                let is_public = detail.package.is_public;
                let grants = pkg_client.list_grants(id).await?;
                let friends = cloud_client.friends().await?;
                let versions = pkg_client.list_versions(id).await?;
                Ok((id, is_public, friends, grants, versions))
            },
            Message::OwnedShareLoaded,
        ))
    }

    #[allow(clippy::type_complexity)]
    pub(super) fn owned_share_loaded(
        &mut self,
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
    ) -> Update<Message, Event> {
        self.share_busy = false;
        match result {
            Ok((id, is_public, friends, grants, versions)) => {
                self.share_package_id = Some(id);
                self.share_is_public = is_public;
                self.share_friends = friends;
                self.share_grants = grants;
                self.share_versions = versions;
            }
            // A not-yet-published package simply has no cloud state.
            Err(CloudError::NotFoundOrNoAccess) => {}
            Err(e) => self.share_feedback = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn select_owned_file(&mut self, subpath: String) -> Update<Message, Event> {
        let Some(name) = self.local_package.as_ref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        if subpath == "README.md" {
            self.owned_selected_file = None;
            return Update::none();
        }
        match local_packages::read_local_file(&self.server_name, &name, &subpath) {
            Ok(content) => {
                self.editor_content = iced::widget::text_editor::Content::with_text(&content);
                self.dirty = false;
                self.owned_selected_file = Some(subpath);
            }
            Err(e) => self.authoring_feedback = Some(format!("Failed to read {subpath}: {e}")),
        }
        Update::none()
    }

    pub(super) fn save_owned_file(&mut self) -> Update<Message, Event> {
        let (name, subpath) = match (
            self.local_package.as_ref().map(|p| p.name.clone()),
            self.owned_selected_file.clone(),
        ) {
            (Some(name), Some(subpath)) => (name, subpath),
            _ => return Update::none(),
        };
        if let Err(e) = local_packages::write_local_file(
            &self.server_name,
            &name,
            &subpath,
            &self.editor_content.text(),
        ) {
            self.authoring_feedback = Some(format!("Save failed: {e}"));
            return Update::none();
        }
        self.dirty = false;
        if let Ok(Some(package)) = local_packages::load_local_package(&self.server_name, &name) {
            self.local_readme = package.readme.as_deref().map(markdown::Content::parse);
            self.local_package = Some(Box::new(package));
        }
        Update::with_task(self.show_toast(format!("Saved {subpath}.")))
    }

    pub(super) fn publish_owned(&mut self) -> Update<Message, Event> {
        let Some(name) = self.local_package.as_ref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        self.authoring_busy = true;
        // Shown live while the (possibly slow) tsc declaration pass + upload run; the
        // outcome — including any non-fatal tsc warnings — lands in `PublishFinished`.
        self.authoring_feedback = Some(format!("Generating declarations & publishing {name}…"));
        let client = self.package_client();
        let server = self.server_name.clone();
        Update::with_task(Task::perform(
            async move {
                local_packages::publish_local_package(&client, &server, &name)
                    .await
                    .map_err(|e| e.to_string())
            },
            Message::PublishFinished,
        ))
    }

    pub(super) fn delete_owned(&mut self) -> Update<Message, Event> {
        let Some(name) = self.local_package.as_ref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        if let Err(e) = local_packages::delete_local_package(&self.server_name, &name) {
            self.authoring_feedback = Some(format!("Delete failed: {e}"));
            return Update::none();
        }
        // The folder was the package: purge the lockfile installs that ran it, or they linger as
        // phantom "installed" entries that fail to load every session (rename migrates its entry
        // for the same reason). The reserved `local`-owner specifier can only ever resolve to the
        // deleted folder, so it goes unconditionally. The account specifier can outlive the
        // folder — deleting a local working copy un-shadows a published copy of the same name —
        // so it is settled asynchronously below, once the cloud says whether one is published.
        if let Err(e) = shared_packages::uninstall_package(
            &self.server_name,
            &specifier_for(local_packages::LOCAL_OWNER, &name),
        ) {
            log::warn!("Failed to remove the deleted package's install entry: {e}");
        }
        let account_check = self.cloud.snapshot.get().nickname_text().and_then(|nick| {
            let spec = specifier_for(&nick, &name);
            let was_enabled = shared_packages::load_lock(&self.server_name)
                .ok()
                .and_then(|lock| lock.find(&spec).map(|p| p.enabled));
            was_enabled.map(|was_enabled| {
                // Park the entry disabled across the cloud round-trip: the session reload this
                // delete triggers must not try to load it — the folder is gone, so it would
                // emit a spurious "no version could be found" notice for a deliberate delete.
                if was_enabled
                    && let Err(e) = shared_packages::set_enabled(&self.server_name, &spec, false)
                {
                    log::warn!("Failed to park the deleted package's account install: {e}");
                }
                let client = self.package_client();
                let server = self.server_name.clone();
                let name = name.clone();
                Task::perform(
                    async move {
                        match client.resolve_package(&nick, &name, None).await {
                            // A published copy exists: deleting the working copy un-shadows it
                            // (the npm-unlink direction of the local-override design), so the
                            // parked entry is restored and the published package takes over.
                            Ok(_) => {
                                if was_enabled
                                    && shared_packages::set_enabled(&server, &spec, true).is_ok()
                                {
                                    StaleInstallCheck::Restored
                                } else {
                                    StaleInstallCheck::Unchanged
                                }
                            }
                            // Nothing is published under the name, so the entry can never
                            // resolve again — drop it, unless the author re-created a
                            // same-named local package while this round-trip was in flight.
                            Err(CloudError::NotFoundOrNoAccess) => {
                                let recreated = local_packages::packages_dir(&server)
                                    .is_ok_and(|dir| dir.join(&name).exists());
                                if !recreated
                                    && shared_packages::uninstall_package(&server, &spec).is_ok()
                                {
                                    StaleInstallCheck::Pruned
                                } else {
                                    StaleInstallCheck::Unchanged
                                }
                            }
                            // Unreachable/offline: can't tell. The entry stays parked
                            // (disabled) — the installed-list sweep prunes it later if nothing
                            // is published, and the user can re-enable it any time.
                            Err(_) => StaleInstallCheck::Unchanged,
                        }
                    },
                    Message::StaleAccountInstallsChecked,
                )
            })
        });
        self.confirm_delete_local = false;
        self.local_package = None;
        // Drop the manifest draft too (delete doesn't route through clear_selection), so a dirty
        // draft for the now-deleted package can't trip the unsaved-changes guard on the next nav.
        self.manifest_draft = None;
        self.manifest_dirty = false;
        self.manifest_editing = false;
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        let toast = self.show_toast(format!("Deleted package {name}."));
        // Reload same-server sessions like an uninstall does: the deleted package stops running
        // and the engine rebuild prunes its now-orphaned `.isolates/<slug>` scratch dir.
        // Re-read the installed list + re-resolve the graph too: deleting a local package can change
        // which installed rows are shadowed by a local override, so the installed pane must refresh
        // rather than keep showing a now-stale view until the next manual Reload.
        let mut tasks = vec![
            Task::done(Message::LoadLocalPackages),
            Task::done(Message::LoadInstalledPackages),
            toast,
        ];
        if let Some(check) = account_check {
            tasks.push(check);
        }
        Update::new(
            Task::batch(tasks),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    /// Applies the outcome of an async account-install check ([`delete_owned`](Self::delete_owned)
    /// or the installed-list sweep). Pruned entries only need the installed list refreshed — they
    /// never loaded anything, so no session reload. A restored entry re-enabled a published copy
    /// that should now take over, which is a change to the enabled set, so sessions reload.
    pub(super) fn stale_account_installs_checked(
        &mut self,
        outcome: StaleInstallCheck,
    ) -> Update<Message, Event> {
        match outcome {
            StaleInstallCheck::Pruned => {
                Update::with_task(Task::done(Message::LoadInstalledPackages))
            }
            StaleInstallCheck::Restored => Update::new(
                Task::done(Message::LoadInstalledPackages),
                Some(Event::ScriptsChanged {
                    server_name: self.server_name.clone(),
                }),
            ),
            StaleInstallCheck::Unchanged => Update::none(),
        }
    }

    /// Background sweep behind `Message::LoadInstalledPackages`: collects the account's OWN
    /// installs (`smudgy://<nickname>/…`) that have no backing local folder and verifies each
    /// against the cloud, uninstalling the ones nothing is published under — they can never
    /// resolve again. Complements `reconcile_local_installs`, which settles the reserved
    /// `local`-owner entries synchronously: an account-owned entry needs the cloud's word before
    /// it can be called stale, e.g. one stranded by deleting its package while signed out or
    /// offline. `None` when signed out or when every own install has its folder (the common
    /// case — published packages by other authors are never checked).
    pub(super) fn sweep_stale_account_installs(&self) -> Option<Task<Message>> {
        let nick = self.cloud.snapshot.get().nickname_text()?;
        let prefix = format!("smudgy://{nick}/");
        let candidates: Vec<String> = self
            .installed_packages
            .iter()
            .filter_map(|p| p.specifier.strip_prefix(&prefix).map(str::to_string))
            .filter(|name| {
                !self
                    .local_packages
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(name))
            })
            .collect();
        if candidates.is_empty() {
            return None;
        }
        let client = self.package_client();
        let server = self.server_name.clone();
        Some(Task::perform(
            async move {
                let mut pruned = false;
                for name in candidates {
                    if matches!(
                        client.resolve_package(&nick, &name, None).await,
                        Err(CloudError::NotFoundOrNoAccess)
                    ) {
                        // Re-check the folder right before the write: the package may have
                        // been (re)created since the candidate list was drawn up.
                        let recreated = local_packages::packages_dir(&server)
                            .is_ok_and(|dir| dir.join(&name).exists());
                        if !recreated {
                            let spec = specifier_for(&nick, &name);
                            pruned |= shared_packages::uninstall_package(&server, &spec).is_ok();
                        }
                    }
                }
                if pruned {
                    StaleInstallCheck::Pruned
                } else {
                    StaleInstallCheck::Unchanged
                }
            },
            Message::StaleAccountInstallsChecked,
        ))
    }

    pub(super) fn create_package(&mut self) -> Update<Message, Event> {
        let name = match &self.pane {
            Pane::NewPackage { name, .. } => name.trim().to_string(),
            _ => return Update::none(),
        };
        if let Err(message) = naming::validate_package_name(&name) {
            if let Pane::NewPackage { error, .. } = &mut self.pane {
                *error = Some(message);
            }
            return Update::none();
        }
        if let Err(e) = local_packages::scaffold_local_package(&self.server_name, &name) {
            if let Pane::NewPackage { error, .. } = &mut self.pane {
                *error = Some(format!("Failed to create package: {e}"));
            }
            return Update::none();
        }
        Update::with_task(Task::batch([
            Task::done(Message::LoadLocalPackages),
            Task::done(Message::SelectOwnedPackage(name)),
        ]))
    }

    pub(super) fn set_visibility(&mut self, public: bool) -> Update<Message, Event> {
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        let client = self.package_client();
        self.share_busy = true;
        Update::with_task(Task::perform(
            async move {
                client
                    .patch_package(id, None, Some(public))
                    .await
                    .map(|view| view.is_public)
            },
            Message::VisibilityUpdated,
        ))
    }

    pub(super) fn visibility_updated(
        &mut self,
        result: Result<bool, CloudError>,
    ) -> Update<Message, Event> {
        self.share_busy = false;
        match result {
            Ok(is_public) => self.share_is_public = is_public,
            Err(e) => self.share_feedback = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn yank_version(&mut self, version: String, yanked: bool) -> Update<Message, Event> {
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        let client = self.package_client();
        self.share_busy = true;
        Update::with_task(Task::perform(
            async move { client.set_version_yanked(id, &version, yanked).await },
            Message::VersionsUpdated,
        ))
    }

    pub(super) fn delete_version(&mut self, version: String) -> Update<Message, Event> {
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        let client = self.package_client();
        self.share_busy = true;
        Update::with_task(Task::perform(
            async move {
                client.delete_version(id, &version).await?;
                client.list_versions(id).await
            },
            Message::VersionsUpdated,
        ))
    }

    pub(super) fn versions_updated(
        &mut self,
        result: Result<Vec<VersionListItem>, CloudError>,
    ) -> Update<Message, Event> {
        self.share_busy = false;
        match result {
            Ok(versions) => self.share_versions = versions,
            Err(e) => self.share_feedback = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn share_with_friend(&mut self, grantee: Uuid) -> Update<Message, Event> {
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        // Toggle: revoke if already granted to this friend, else share.
        if let Some(grant) = self
            .share_grants
            .iter()
            .find(|g| g.grantee_id == Some(grantee))
        {
            let grant_id = grant.id;
            return self.revoke_grant(grant_id);
        }
        let client = self.package_client();
        self.share_busy = true;
        Update::with_task(Task::perform(
            async move { client.share_with_friend(id, grantee).await },
            Message::GrantsUpdated,
        ))
    }

    pub(super) fn revoke_grant(&mut self, grant_id: Uuid) -> Update<Message, Event> {
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        let client = self.package_client();
        self.share_busy = true;
        Update::with_task(Task::perform(
            async move { client.revoke_grant(id, grant_id).await },
            Message::GrantsUpdated,
        ))
    }

    pub(super) fn grants_updated(
        &mut self,
        result: Result<Vec<PackageGrantView>, CloudError>,
    ) -> Update<Message, Event> {
        self.share_busy = false;
        match result {
            Ok(grants) => self.share_grants = grants,
            Err(e) => self.share_feedback = Some(display_error(&e)),
        }
        Update::none()
    }
}

// ============================================================================
// Discover + Shared — update side (ported)
// ============================================================================

impl AutomationsWindow {
    pub(super) fn open_discover(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.param_prompt = None;
        self.discover_error = None;
        // Land on the results list (not a stale open detail) and load it for the current query/scope
        // so the pane is never empty on open (an empty query is the "browse everything" default).
        self.discover_detail = None;
        self.discover_readme = None;
        self.discover_comments.clear();
        self.selection = Selection::Discover;
        self.pane = Pane::Discover;
        // Public discovery needs no account, so load the results list for everyone.
        self.discover_search()
    }

    /// Loads the dashboard "Discover" teaser: a default-scope ([`DiscoverScope::Relevant`]),
    /// empty-query search whose top results are shown on the dashboard. The search is public, so
    /// it loads with or without an account. A failure leaves the teaser empty (it's
    /// non-essential), so errors are swallowed.
    pub(super) fn load_featured_discover(&mut self) -> Update<Message, Event> {
        let client = self.package_client();
        let host = self.mud_host.clone();
        Update::with_task(Task::perform(
            async move {
                client
                    .search_packages(host.as_deref(), None, SearchCategory::Both)
                    .await
            },
            Message::FeaturedDiscoverLoaded,
        ))
    }

    pub(super) fn discover_search(&mut self) -> Update<Message, Event> {
        self.discover_busy = true;
        self.discover_error = None;
        let client = self.package_client();
        let query = self.discover_query.trim().to_string();
        // Translate the host-aware scope into the wire `(host, category)` pair. "All" drops the host
        // so the server's `host IS NULL` branch returns every public package (incl. other MUDs');
        // "Relevant"/"Host only" pass the host; "Universal" needs no host.
        let (host, category) = match self.discover_scope {
            DiscoverScope::Relevant => (self.mud_host.clone(), SearchCategory::Both),
            DiscoverScope::HostOnly => (self.mud_host.clone(), SearchCategory::MudSpecific),
            DiscoverScope::Universal => (None, SearchCategory::Universal),
            DiscoverScope::All => (None, SearchCategory::Both),
        };
        Update::with_task(Task::perform(
            async move {
                let query = if query.is_empty() { None } else { Some(query) };
                client
                    .search_packages(host.as_deref(), query.as_deref(), category)
                    .await
            },
            Message::DiscoverResultsLoaded,
        ))
    }

    pub(super) fn discover_results_loaded(
        &mut self,
        result: Result<Vec<PackageSearchResult>, CloudError>,
    ) -> Update<Message, Event> {
        self.discover_busy = false;
        match result {
            Ok(results) => self.discover_results = results,
            Err(e) => self.discover_error = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn discover_select(
        &mut self,
        package_id: Uuid,
        owner: String,
    ) -> Update<Message, Event> {
        // Reachable from the dashboard teaser too, so make sure we're on the Discover pane (the
        // detail renders there). Harmless when already on it.
        self.pane = Pane::Discover;
        self.selection = Selection::Discover;
        self.discover_busy = true;
        self.discover_error = None;
        self.discover_owner = Some(owner);
        self.discover_detail = None;
        self.discover_readme = None;
        self.discover_comments.clear();
        self.param_prompt = None;
        let detail_client = self.package_client();
        let comments_client = self.package_client();
        Update::with_task(Task::batch([
            Task::perform(
                async move { detail_client.get_package(package_id).await },
                Message::DiscoverDetailLoaded,
            ),
            Task::perform(
                async move { comments_client.list_comments(package_id).await },
                Message::DiscoverCommentsLoaded,
            ),
        ]))
    }

    pub(super) fn discover_detail_loaded(
        &mut self,
        result: Result<PackageDetail, CloudError>,
    ) -> Update<Message, Event> {
        self.discover_busy = false;
        match result {
            Ok(detail) => {
                self.discover_readme = detail.readme.as_deref().map(markdown::Content::parse);
                self.discover_detail = Some(Box::new(detail));
            }
            Err(e) => self.discover_error = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn discover_comments_loaded(
        &mut self,
        result: Result<Vec<CommentView>, CloudError>,
    ) -> Update<Message, Event> {
        if let Ok(comments) = result {
            self.discover_comments = comments;
        }
        Update::none()
    }

    pub(super) fn discover_back(&mut self) -> Update<Message, Event> {
        self.discover_detail = None;
        self.discover_readme = None;
        self.discover_comments.clear();
        self.param_prompt = None;
        self.param_prompt_queue.clear();
        self.consent_prompt = None;
        self.discover_error = None;
        // Back within the Discover pane abandons a pending install too (it doesn't go through
        // clear_selection), so invalidate any in-flight resolve.
        self.install_seq.bump();
        Update::none()
    }

    /// Dismisses the post-install required-params prompt for the current package. The package was
    /// already installed + consented at the Grant step, so this still advances (refreshes the
    /// installed list when the queue drains) — it just leaves this package unconfigured, so it won't
    /// load until the required params are set. When more required roots are queued, their prompts
    /// follow; the closing toast reports the chosen package.
    pub(super) fn param_prompt_cancel(&mut self) -> Update<Message, Event> {
        let Some((specifier, enable)) = self
            .param_prompt
            .as_ref()
            .map(|p| (p.specifier.clone(), p.enable))
        else {
            return Update::none();
        };
        self.param_prompt = None;
        self.advance_param_prompt_queue(&specifier, enable)
    }

    pub(super) fn rate_package(&self, stars: i16) -> Update<Message, Event> {
        let Some(detail) = self.discover_detail.as_ref() else {
            return Update::none();
        };
        let package_id = detail.package.id;
        let client = self.package_client();
        Update::with_task(Task::perform(
            async move { client.rate_package(package_id, stars).await },
            Message::RatingUpdated,
        ))
    }

    pub(super) fn rating_updated(
        &mut self,
        result: Result<PackageDetail, CloudError>,
    ) -> Update<Message, Event> {
        match result {
            Ok(detail) => {
                self.discover_readme = detail.readme.as_deref().map(markdown::Content::parse);
                self.discover_detail = Some(Box::new(detail));
            }
            Err(e) => self.discover_error = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn add_comment(&self) -> Update<Message, Event> {
        let Some(detail) = self.discover_detail.as_ref() else {
            return Update::none();
        };
        let body = self.discover_comment_input.trim().to_string();
        if body.is_empty() {
            return Update::none();
        }
        let package_id = detail.package.id;
        let client = self.package_client();
        Update::with_task(Task::perform(
            async move { client.add_comment(package_id, &body).await },
            Message::CommentAdded,
        ))
    }

    pub(super) fn comment_added(
        &mut self,
        result: Result<CommentView, CloudError>,
    ) -> Update<Message, Event> {
        match result {
            Ok(comment) => {
                self.discover_comment_input.clear();
                self.discover_comments.insert(0, comment);
            }
            Err(e) => self.discover_error = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn discover_install(&mut self) -> Update<Message, Event> {
        let Some(owner) = self.discover_owner.clone() else {
            return Update::none();
        };
        let Some(name) = self
            .discover_detail
            .as_ref()
            .map(|d| d.package.name.clone())
        else {
            return Update::none();
        };
        self.begin_install(owner, name)
    }

    /// Installs a search result directly from its card (the result-card "Install" button, also used
    /// by the dashboard teaser). Ensures we're on the Discover pane so the install consent window —
    /// rendered by `view_discover` — is visible; when arriving from elsewhere (e.g. the dashboard),
    /// it also kicks the empty-query search so the results list is populated behind the consent
    /// window once the install completes.
    pub(super) fn discover_install_result(
        &mut self,
        owner: String,
        name: String,
    ) -> Update<Message, Event> {
        let arriving = !matches!(self.pane, Pane::Discover);
        self.pane = Pane::Discover;
        self.selection = Selection::Discover;
        // Arriving from elsewhere (e.g. the dashboard) kicks the public empty-query search so the
        // results list is populated behind the consent window — no account required.
        let search = if arriving {
            self.discover_search().task
        } else {
            Task::none()
        };
        let install = self.begin_install(owner, name);
        Update::new(Task::batch([search, install.task]), install.event)
    }

    pub(super) fn begin_install(&mut self, owner: String, name: String) -> Update<Message, Event> {
        self.discover_busy = true;
        self.discover_error = None;
        // New install generation: a result tagged with this seq is honored only if nothing has
        // abandoned the install (navigation, Back, or another install) in the meantime.
        self.install_seq.bump();
        let seq = self.install_seq;
        let client = self.package_client();
        // Resolve the root, fold the whole dependency-closure permission union, AND walk the
        // `requires`-closure (required roots + cycle warnings + peer-conflict check) before showing
        // the consent window — the sandboxed isolate is granted exactly that union, and the user
        // grants the whole required set at once.
        let installed = self.installed_packages.clone();
        Update::with_task(Task::perform(
            async move { resolve_install_closure(&client, &owner, &name, None, &installed).await },
            move |result| Message::InstallResolved(seq, result),
        ))
    }

    pub(super) fn install_resolved(
        &mut self,
        seq: InstallSeq,
        result: Result<InstallResolution, CloudError>,
    ) -> Update<Message, Event> {
        // Discard a stale resolve: the user navigated away, hit Back, or started another install
        // while this one was in flight, so the consent window would be orphaned.
        if seq != self.install_seq {
            return Update::none();
        }
        self.discover_busy = false;
        match result {
            Ok(res) => {
                // The Install Confirmation window is ALWAYS shown before a lock entry is written,
                // even for a zero-permission package. Nothing is persisted yet.
                self.consent_prompt = Some(ConsentPrompt {
                    specifier: res.specifier,
                    owner: res.owner,
                    name: res.name,
                    version: res.version,
                    permissions: res.permissions,
                    params: res.params,
                    required_roots: res.required_roots,
                    cycle_warnings: res.cycle_warnings,
                    conflict: res.conflict,
                    needs_smudgy: res.needs_smudgy,
                    error: None,
                });
                Update::none()
            }
            Err(e) => {
                self.discover_error = Some(display_error(&e));
                Update::none()
            }
        }
    }

    /// Grant & install: write the lock entry for the chosen package (with the `enable` choice baked
    /// into the same write, so an "install, don't enable" never transiently persists as
    /// `enabled: true`), then co-install every not-already-satisfied **required root** as its own
    /// top-level root (apt's "automatically installed" mark, via `install_required_package`).
    /// Records the consented closure union + last-resolved version for each, then chains the
    /// required-params prompt across every package (the root and each required root) that still has
    /// unset required params. A single grant covers the whole set (`script/REQUIRED-PACKAGES.md`).
    /// Install precedes `record_consent` so the entry exists for it to update. Both install actions
    /// record the same consent; `enable` only decides whether each package is turned on (runs) now
    /// or left off for the user to review first. A peer conflict (`conflict`) blocks the install —
    /// the view disables the grant buttons, so this is only reached when there's no conflict.
    pub(super) fn consent_grant(&mut self, enable: bool) -> Update<Message, Event> {
        let Some(prompt) = self.consent_prompt.as_ref() else {
            return Update::none();
        };
        // A peer conflict or version-floor refusal is unresolvable from here — refuse rather
        // than install a broken set (the view disables the grant buttons for both).
        if prompt.conflict.is_some() || prompt.needs_smudgy.is_some() {
            return Update::none();
        }
        let specifier = prompt.specifier.clone();
        let name = prompt.name.clone();
        let version = prompt.version.clone();
        let permissions = prompt.permissions.clone();
        let params = prompt.params.clone();
        // Only the not-already-satisfied required roots are (co-)installed; an already-installed
        // satisfying root is reused as-is (never downgraded, never re-consented).
        let required: Vec<RequiredRoot> = prompt
            .required_roots
            .iter()
            .filter(|r| !r.already_satisfied)
            .cloned()
            .collect();

        // Install + consent the chosen package first (the user's explicit, user-owned root).
        if let Err(e) = shared_packages::install_package(
            &self.server_name,
            &specifier,
            UpdateMode::Auto,
            enable,
        ) {
            if let Some(prompt) = self.consent_prompt.as_mut() {
                prompt.error = Some(format!("Failed to install: {e}"));
            }
            return Update::none();
        }
        if let Err(e) = shared_packages::record_consent(&self.server_name, &specifier, &permissions)
        {
            // The lock entry is written; surface the consent-record failure rather than roll back
            // (a missing record just means the engine denies everything until consent is recorded).
            if let Some(prompt) = self.consent_prompt.as_mut() {
                prompt.error = Some(format!("Installed, but failed to record consent: {e}"));
            }
            return Update::none();
        }

        // Co-install each required root as its own top-level root (auto-installed mark on a NEW
        // entry; a user-owned entry that already exists keeps its mark — handled by
        // `install_required_package`). Mirrors the chosen package's install + consent recording;
        // the engine records each one's resolved version + integrity on the next session load (the
        // `ScriptsChanged` event below), as it does for the chosen package.
        for root in &required {
            if let Err(e) = shared_packages::install_required_package(
                &self.server_name,
                &root.specifier,
                UpdateMode::Auto,
                enable,
            ) {
                if let Some(prompt) = self.consent_prompt.as_mut() {
                    prompt.error = Some(format!("Failed to install required {}: {e}", root.name));
                }
                return Update::none();
            }
            let _ = shared_packages::record_consent(
                &self.server_name,
                &root.specifier,
                &root.permissions,
            );
        }
        self.consent_prompt = None;

        // Build the required-params prompt queue across the chosen package and every co-installed
        // required root, in install order, skipping any with no missing required params.
        let mut prompts: Vec<ParamPrompt> = Vec::new();
        if let Some(prompt) = self.build_param_prompt(&specifier, &name, &version, &params, enable)
        {
            prompts.push(prompt);
        }
        for root in &required {
            if let Some(prompt) = self.build_param_prompt(
                &root.specifier,
                &root.name,
                &root.version,
                &root.params,
                enable,
            ) {
                prompts.push(prompt);
            }
        }
        if prompts.is_empty() {
            return self.finalize_install(&specifier, enable);
        }
        // Show the first prompt; the rest wait their turn (each submit/cancel pops the next).
        self.param_prompt = Some(prompts.remove(0));
        self.param_prompt_queue = prompts;
        Update::none()
    }

    /// Build the install-time required-params prompt for `specifier`, or `None` when it has no
    /// unset required params (so it needs no configuration before loading). Shared by the chosen
    /// package and each co-installed required root.
    fn build_param_prompt(
        &self,
        specifier: &str,
        name: &str,
        version: &str,
        params: &[PackageParameter],
        enable: bool,
    ) -> Option<ParamPrompt> {
        let missing: Vec<PackageParameter> = params
            .iter()
            .filter(|param| {
                param.required
                    && !shared_packages::param_has_value(&self.server_name, specifier, param)
            })
            .cloned()
            .collect();
        if missing.is_empty() {
            return None;
        }
        let values = missing
            .iter()
            .map(|param| {
                // Secrets seed empty (never read back); other kinds seed from their declared
                // default into the matching control state.
                let state = if is_secret_string(param) {
                    ParamValueState::Text(String::new())
                } else {
                    param_values::seed(param, None)
                };
                (param.key.clone(), state)
            })
            .collect();
        Some(ParamPrompt {
            specifier: specifier.to_string(),
            name: name.to_string(),
            version: version.to_string(),
            params: missing,
            values,
            enable,
            error: None,
        })
    }

    /// Advance the install-time param-prompt queue: show the next pending prompt if any, else run
    /// the install tail. Called when a prompt is submitted or dismissed, so a multi-package required
    /// install configures each package in turn before finishing. `finalize_specifier`/`enable` drive
    /// the closing toast + reload once the queue drains.
    fn advance_param_prompt_queue(
        &mut self,
        finalize_specifier: &str,
        enable: bool,
    ) -> Update<Message, Event> {
        if self.param_prompt_queue.is_empty() {
            return self.finalize_install(finalize_specifier, enable);
        }
        self.param_prompt = Some(self.param_prompt_queue.remove(0));
        Update::none()
    }

    pub(super) fn consent_cancel(&mut self) -> Update<Message, Event> {
        // Cancel writes nothing.
        self.consent_prompt = None;
        Update::none()
    }

    /// The common install tail: the package is already installed + consented. `enable` decides
    /// whether it runs now (enabled + the session hot-reloads to pick it up) or is left off for the
    /// user to review first (no reload, so it doesn't execute this session). Either way the hub's
    /// installed list refreshes.
    fn finalize_install(&mut self, specifier: &str, enable: bool) -> Update<Message, Event> {
        self.param_prompt = None;
        // The whole required set has been configured (the queue drained) — clear it defensively.
        self.param_prompt_queue.clear();
        self.graph.intent.insert(specifier.to_string(), enable);
        let name = package_display_name(specifier);
        let toast = self.show_toast(if enable {
            format!("Installed and enabled {name}.")
        } else {
            format!("Installed {name} — review it, then enable it to run.")
        });
        // Only an enabled install reloads the live session; "install, don't enable" stays inert so
        // the user can review the code before it executes.
        let event = enable.then(|| Event::ScriptsChanged {
            server_name: self.server_name.clone(),
        });
        Update::new(
            Task::batch([Task::done(Message::LoadInstalledPackages), toast]),
            event,
        )
    }

    /// Apply one parameter-value edit, routed by `target` to the install-time prompt or the in-pane
    /// config editor. Folds the addressed change into the matching value state via
    /// [`param_values::apply`], looking up the param's spec for its kind/columns/options.
    pub(super) fn param_value_edit(
        &mut self,
        target: ParamTarget,
        key: String,
        edit: ParamValueEdit,
    ) -> Update<Message, Event> {
        match target {
            ParamTarget::Prompt => {
                let Some(prompt) = self.param_prompt.as_mut() else {
                    return Update::none();
                };
                let Some(spec) = prompt.params.iter().find(|p| p.key == key).cloned() else {
                    return Update::none();
                };
                if let Some(state) = prompt.values.get_mut(&key) {
                    param_values::apply(&spec, state, edit);
                }
                prompt.error = None;
            }
            ParamTarget::Config => {
                let Some(config) = self.param_config.as_mut() else {
                    return Update::none();
                };
                let Some(spec) = config.params.iter().find(|p| p.key == key).cloned() else {
                    return Update::none();
                };
                if let Some(state) = config.values.get_mut(&key) {
                    param_values::apply(&spec, state, edit);
                }
                config.touched.insert(key);
                config.error = None;
                config.saved = false;
            }
        }
        Update::none()
    }

    pub(super) fn param_prompt_submit(&mut self) -> Update<Message, Event> {
        let (specifier, params) = match self.param_prompt.as_ref() {
            Some(prompt) => (prompt.specifier.clone(), prompt.params.clone()),
            None => return Update::none(),
        };
        // Project + validate every value (all prompt params are required) before writing anything.
        let mut plan: Vec<(String, Persist)> = Vec::new();
        for param in &params {
            let state = self
                .param_prompt
                .as_ref()
                .and_then(|p| p.values.get(&param.key));
            if is_secret_string(param) {
                let text = secret_text(state);
                if text.is_empty() {
                    return self.fail_prompt(format!("'{}' is required.", param.key));
                }
                plan.push((param.key.clone(), Persist::Secret(text)));
            } else {
                match state.map_or(Ok(None), |s| param_values::to_json(param, s)) {
                    Ok(Some(value)) => plan.push((param.key.clone(), Persist::Value(value))),
                    Ok(None) => return self.fail_prompt(format!("'{}' is required.", param.key)),
                    Err(reason) => {
                        return self.fail_prompt(format!("'{}': {reason}", param.key));
                    }
                }
            }
        }
        for (key, persist) in &plan {
            if let Err(e) = persist.write(&self.server_name, &specifier, key) {
                return self.fail_prompt(format!("Failed to save '{key}': {e}"));
            }
        }
        // The package was already installed + consented at the Grant step; this only saves
        // configuration, then advances to the next queued required-root prompt (or finishes with
        // the enable choice made there once the queue drains).
        let enable = self.param_prompt.as_ref().is_some_and(|p| p.enable);
        self.param_prompt = None;
        self.advance_param_prompt_queue(&specifier, enable)
    }

    /// Set the install-time prompt's inline error and stay open.
    fn fail_prompt(&mut self, message: String) -> Update<Message, Event> {
        if let Some(prompt) = self.param_prompt.as_mut() {
            prompt.error = Some(message);
        }
        Update::none()
    }

    /// Set the in-pane config editor's inline error and clear the "saved" confirmation.
    fn fail_config(&mut self, message: String) -> Update<Message, Event> {
        if let Some(config) = self.param_config.as_mut() {
            config.error = Some(message);
            config.saved = false;
        }
        Update::none()
    }

    // ---- in-pane param-value editor (installed & owned panes) -------------

    /// (Re)seed the inline param-value editor for the open package. `None` when the package
    /// declares no params, so the section renders nothing. Called when a package pane opens (and
    /// when an owned package's manifest is saved, which can add/remove params).
    pub(super) fn seed_param_config(&mut self, specifier: String, params: Vec<PackageParameter>) {
        self.param_config =
            (!params.is_empty()).then(|| ParamConfig::seed(&self.server_name, specifier, params));
    }

    /// Persist every declared param's configured value: non-secrets to `smudgy.params.json`
    /// (cleared when emptied), secrets to the keyring (an empty box keeps the stored secret).
    /// Required params must resolve to a value; a value that fails to project for its kind (a number
    /// that won't parse, a dropdown value that isn't a choice) is reported and nothing is written. An
    /// enabled package hot-reloads so it picks the new config up; saving never changes the package's
    /// installed/enabled state.
    pub(super) fn param_config_save(&mut self) -> Update<Message, Event> {
        let (specifier, params) = match self.param_config.as_ref() {
            Some(config) => (config.specifier.clone(), config.params.clone()),
            None => return Update::none(),
        };
        let secret_stored = self
            .param_config
            .as_ref()
            .map(|c| c.secret_stored.clone())
            .unwrap_or_default();
        let touched = self
            .param_config
            .as_ref()
            .map(|c| c.touched.clone())
            .unwrap_or_default();

        // Validate + project everything before writing, so a mid-list failure leaves the on-disk
        // values untouched. A required secret counts as satisfied if one is already stored, even
        // with an empty box (the box only ever *replaces* a secret, never reveals it).
        let mut plan: Vec<(String, Persist)> = Vec::new();
        for param in &params {
            let state = self
                .param_config
                .as_ref()
                .and_then(|c| c.values.get(&param.key));
            if is_secret_string(param) {
                let text = secret_text(state);
                if param.required && text.is_empty() && !secret_stored.contains(&param.key) {
                    return self.fail_config(format!("'{}' is required.", param.key));
                }
                // A non-empty box replaces the secret; an empty box keeps whatever is stored.
                if !text.is_empty() {
                    plan.push((param.key.clone(), Persist::Secret(text)));
                }
            } else {
                let projected = match state.map_or(Ok(None), |s| param_values::to_json(param, s)) {
                    Ok(value) => value,
                    Err(reason) => return self.fail_config(format!("'{}': {reason}", param.key)),
                };
                if param.required && projected.is_none() {
                    return self.fail_config(format!("'{}' is required.", param.key));
                }
                // Don't materialize an untouched optional value: a manifest `default` stays a
                // default the script applies, not a stored value. (A bool/dropdown always projects a
                // concrete value, so without this an untouched checkbox would persist its default on
                // the first Save.) Required params are always written so the load-gate is satisfied.
                if !param.required && !touched.contains(&param.key) {
                    continue;
                }
                plan.push((
                    param.key.clone(),
                    projected.map_or(Persist::Clear, Persist::Value),
                ));
            }
        }

        for (key, persist) in &plan {
            if let Err(e) = persist.write(&self.server_name, &specifier, key) {
                return self.fail_config(format!("Failed to save '{key}': {e}"));
            }
        }

        // Reflect the writes in the editor: a secret just typed is now stored, and its (write-only)
        // box is cleared so plaintext doesn't linger; mark the section saved.
        if let Some(config) = self.param_config.as_mut() {
            for (key, persist) in &plan {
                if matches!(persist, Persist::Secret(_)) {
                    config.secret_stored.insert(key.clone());
                    config
                        .values
                        .insert(key.clone(), ParamValueState::Text(String::new()));
                }
            }
            // The current state is now the on-disk state; a follow-up Save without edits writes
            // nothing (and doesn't re-materialize untouched defaults).
            config.touched.clear();
            config.error = None;
            config.saved = true;
        }

        // A running (enabled) package should pick up the new config — hot-reload the live session,
        // the same signal an enabled install emits. A disabled package reads the new values when it
        // is next enabled, so there's nothing to reload.
        let event = self
            .graph
            .effectively_enabled(&specifier)
            .then(|| Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            });
        Update::new(self.show_toast("Saved settings."), event)
    }

    /// Remove a stored secret param entirely (the only way to *unset* a secret, since the box can
    /// only replace one). An enabled package hot-reloads so a script reading it sees the change.
    pub(super) fn param_config_clear_secret(&mut self, key: String) -> Update<Message, Event> {
        let Some(specifier) = self.param_config.as_ref().map(|c| c.specifier.clone()) else {
            return Update::none();
        };
        if let Err(e) = shared_packages::clear_secret_param(&self.server_name, &specifier, &key) {
            if let Some(config) = self.param_config.as_mut() {
                config.error = Some(format!("Failed to clear '{key}': {e}"));
                config.saved = false;
            }
            return Update::none();
        }
        if let Some(config) = self.param_config.as_mut() {
            config.secret_stored.remove(&key);
            config
                .values
                .insert(key.clone(), ParamValueState::Text(String::new()));
            config.error = None;
            config.saved = false;
        }
        let event = self
            .graph
            .effectively_enabled(&specifier)
            .then(|| Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            });
        Update::new(self.show_toast("Cleared secret."), event)
    }

    pub(super) fn open_shared(&mut self) -> Update<Message, Event> {
        self.clear_selection();
        self.shared_with_me = None;
        self.my_cloud_packages = None;
        self.param_prompt = None;
        self.discover_error = None;
        self.selection = Selection::Shared;
        self.pane = Pane::Shared;
        if !self.signed_in() {
            return Update::none();
        }
        // Load both halves of the pane in parallel: packages friends shared with the caller, and
        // the caller's own cloud packages (so an owner sees private packages that exist in no other
        // surface — e.g. one published from another machine).
        let shared_client = self.package_client();
        let mine_client = self.package_client();
        Update::with_task(Task::batch([
            Task::perform(
                async move { shared_client.list_shared_packages().await },
                Message::SharedLoaded,
            ),
            Task::perform(
                async move { mine_client.list_my_packages().await },
                Message::MyCloudLoaded,
            ),
        ]))
    }

    pub(super) fn shared_loaded(
        &mut self,
        result: Result<Vec<PackageDetail>, CloudError>,
    ) -> Update<Message, Event> {
        match result {
            Ok(list) => self.shared_with_me = Some(list),
            Err(e) => {
                self.shared_with_me = Some(Vec::new());
                self.discover_error = Some(display_error(&e));
            }
        }
        Update::none()
    }

    pub(super) fn my_cloud_loaded(
        &mut self,
        result: Result<Vec<PackageDetail>, CloudError>,
    ) -> Update<Message, Event> {
        match result {
            Ok(list) => self.my_cloud_packages = Some(list),
            Err(e) => {
                self.my_cloud_packages = Some(Vec::new());
                self.discover_error = Some(display_error(&e));
            }
        }
        Update::none()
    }
}

// ============================================================================
// Views
// ============================================================================

impl AutomationsWindow {
    pub(super) fn package_status(&self, specifier: &str) -> NodeStatus {
        if !self.graph.effectively_enabled(specifier) {
            NodeStatus::Disabled
        } else if self.blocked_updates.contains(specifier) {
            // Enabled and running (at a fitting version), but its newest version is held back for
            // lack of permissions — flag it so the user reviews + grants the update.
            NodeStatus::Warning
        } else {
            NodeStatus::Ok
        }
    }

    /// A local package's own published-style specifier `smudgy://<owner>/<name>` — the lockfile
    /// install of which is what actually loads the local folder (the provider's local-override).
    /// `<owner>` is the account nickname when signed in, else the reserved
    /// [`LOCAL_OWNER`](smudgy_core::models::local_packages::LOCAL_OWNER) placeholder, so local
    /// packages run signed out too (mirrors the provider's `local_owner`).
    pub(super) fn local_own_spec(&self, name: &str) -> String {
        let owner = self
            .cloud
            .snapshot
            .get()
            .nickname_text()
            .unwrap_or_else(|| smudgy_core::models::local_packages::LOCAL_OWNER.to_string());
        specifier_for(&owner, name)
    }

    /// The set of every local package's own specifier — the rows that represent a local package
    /// (so an active fork's INSTALLED row can be suppressed in favor of its LOCAL row).
    pub(super) fn local_own_specs(&self) -> std::collections::HashSet<String> {
        self.local_packages
            .iter()
            .map(|name| self.local_own_spec(name))
            .collect()
    }

    /// Whether a local package is actually loading (an enabled install of its own specifier).
    pub(super) fn local_active(&self, name: &str) -> bool {
        self.graph.effectively_enabled(&self.local_own_spec(name))
    }

    /// The truthful status of a local package: the status of its own-specifier install (Ok/Warning
    /// when loading, else Disabled).
    pub(super) fn local_status(&self, name: &str) -> NodeStatus {
        self.package_status(&self.local_own_spec(name))
    }

    fn signed_out_banner<'a>(&self, what: &str) -> Elem<'a> {
        container(
            text(format!(
                "Sign in from the main window's Settings → Account to {what}."
            ))
            .size(13.0),
        )
        .width(Length::Fill)
        .padding(Padding {
            top: 10.0,
            bottom: 10.0,
            left: 14.0,
            right: 14.0,
        })
        .style(common::banner_style)
        .into()
    }

    // ---- installed package pane -------------------------------------------

    pub(super) fn view_installed_package(&self) -> Elem<'_> {
        let Some(locked) = self.installed_open.as_deref() else {
            return pane_scroll(column![text("No package selected.").size(13.0)]);
        };
        let specifier = &locked.specifier;
        let name = package_display_name(specifier).to_string();
        let effective = self.graph.effectively_enabled(specifier);
        let controllable = self.graph.controllable(specifier);
        let dep_only = self.graph.is_dep_only(specifier);
        let enabled_dependents = self.graph.enabled_dependents(specifier);
        // A dependency-reference view shows a package whose resolved version is dictated by the
        // parent's manifest, not chosen here. The blocked-update callout (and its "Latest
        // (blocked)" metric) prompt to grant/keep a version the user can't actually pick in this
        // context, so suppress them — they belong to the package's own top-level pane.
        let viewing_as_dependency = matches!(self.selection, Selection::Dependency { .. });

        // Header enable control: locked if a dependent forces it on or dep-only. Hidden entirely
        // for a dependency-reference view — a dependency's on/off state follows its parent, so a
        // toggle here would be meaningless.
        let switch = (!viewing_as_dependency).then(|| {
            common::pill_switch(
                effective,
                !controllable,
                Some(Message::TogglePackageEnabled(specifier.clone())),
            )
        });
        let status = if effective {
            NodeStatus::Ok
        } else {
            NodeStatus::Disabled
        };

        let mut body =
            column![self.scene_header(Some(status), &name, Some(specifier.clone()), switch,)]
                .spacing(16.0);

        // Context banner.
        let banner_text = if dep_only {
            Some(
                "Installed automatically as a dependency — its on/off state follows the packages \
                 that need it, so it can't be toggled here."
                    .to_string(),
            )
        } else if !enabled_dependents.is_empty() {
            let who: Vec<String> = enabled_dependents
                .iter()
                .map(|s| package_display_name(s).to_string())
                .collect();
            Some(format!(
                "You installed this directly, and {} also depends on it. It stays enabled until \
                 nothing needs it.",
                who.join(", ")
            ))
        } else if controllable && !effective {
            Some(
                "Disabled until you allow it. Review the README and source below, then enable it \
                 when you trust it."
                    .to_string(),
            )
        } else {
            None
        };
        if let Some(banner) = banner_text {
            body = body.push(
                container(text(banner).size(13.0))
                    .width(Length::Fill)
                    .padding(12.0)
                    .style(common::banner_style),
            );
        }

        // Update re-prompt: a newly-resolved version wants more access than was consented. Not
        // shown for a dependency-reference view — its version follows the parent's manifest, so a
        // grant/keep choice here would be meaningless.
        if !viewing_as_dependency
            && let Some(delta) = &self.update_delta
            && delta.specifier == *specifier
        {
            body = body.push(self.view_update_delta(delta));
        }

        // Meta row. "Loaded" is the version the engine actually resolved (the lockfile's
        // last-resolved record) — which, for a held-back package, is the older fitting version,
        // NOT the latest the inspect pane probes. Show the held-back latest separately so the two
        // never look contradictory.
        let loaded = locked
            .last_resolved_version
            .clone()
            .or_else(|| self.graph.resolved.get(specifier).cloned());
        let blocked_latest = self
            .update_delta
            .as_ref()
            .filter(|_| !viewing_as_dependency)
            .filter(|delta| delta.specifier == *specifier)
            .map(|delta| delta.version.clone());
        let mut meta = row![].spacing(20.0).align_y(Vertical::Center);
        if let Some(detail) = self.installed_detail.as_deref() {
            meta = meta.push(metric("Author", &detail.owner_nickname));
        }
        if let Some(v) = &loaded {
            meta = meta.push(metric("Loaded", &format!("v{v}")));
        }
        if let Some(v) = &blocked_latest {
            meta = meta.push(metric("Latest (blocked)", &format!("v{v}")));
        }
        meta = meta.push(metric(
            "Update",
            match &locked.mode {
                UpdateMode::Auto => "Auto",
                UpdateMode::Pinned { .. } => "Pinned",
            },
        ));
        // Cloud rating + popularity (best-effort metadata; absent for a local/owned package or while
        // the detail is still loading).
        if let Some(rating) = self.installed_rating.as_deref() {
            let star_color = crate::prefs::current().palette.output;
            meta = meta.push(rating_metric(
                rating.avg_rating,
                rating.rating_count,
                star_color,
            ));
            meta = meta.push(metric("Installs", &rating.install_count.to_string()));
        }
        body = body.push(meta);
        // DEFERRED (`script/REQUIRED-PACKAGES.md` "Version contention surfacing"): a small note here
        // when a singleton-registering library is loaded at more than one version — a `requires`
        // root vs an `import`ed-for-helpers copy at a different version. Detecting it needs the
        // per-importer (referrer-aware) resolved versions of *every* installed package, which lives
        // in the engine's resolution (`package_solver.rs`/`package_provider.rs`), not in the UI: the
        // window only caches one resolved version per specifier (`graph.resolved`) and the open
        // package's own `dependencies`, neither of which can witness a second loaded version pulled
        // in by another package. Surfacing it from here would mean resolving every installed
        // package's closure and threading per-version provenance into the UI — out of proportion to
        // a best-effort note — so this is deferred rather than faked. Items 1–5 (the install/consent
        // closure, peer conflict, orphan prompt) are the substantive deliverables.

        // Rate — an account-only write, so the star control shows only when signed in and the
        // package's cloud metadata loaded (i.e. it's a real cloud package, not a local copy).
        if self.signed_in() && self.installed_rating.is_some() {
            body = body.push(star_rate_row(Message::RateInstalledPackage));
        }

        if let Some(feedback) = &self.manage_feedback {
            body = body.push(text(feedback.clone()).size(12.0).style(common::muted));
        }

        // Permissions view + trust toggle. A dependency has no sandbox or consent of its own — it
        // loads into its parent's isolate and runs with the parent's grants — so describing its
        // own manifest permissions here would be misleading. Point at the parent instead.
        body = if let Selection::Dependency { parent, .. } = &self.selection {
            body.push(self.view_dependency_permissions_section(parent))
        } else {
            body.push(self.view_permissions_section(locked))
        };

        // Required by.
        if !enabled_dependents.is_empty() || !self.graph.required_by(specifier).is_empty() {
            let mut req = Column::new()
                .spacing(4.0)
                .push(common::section_label("Required by"));
            for parent in self.graph.required_by(specifier) {
                let enabled = self.graph.effectively_enabled(&parent);
                req = req.push(self.dep_link_row(&parent, enabled, "needs", None));
            }
            body = body.push(req);
        }

        // Settings (configured param values). A dependency-reference view configures nothing of its
        // own — like permissions, its params belong to its own top-level pane — so it's suppressed
        // here; the section also renders nothing unless the package declares params.
        if !viewing_as_dependency && let Some(settings) = self.view_param_config_section(specifier)
        {
            body = body.push(settings);
        }

        // Update mode (controllable only).
        if controllable {
            let mut update_row = row![
                common::section_label("Update mode"),
                radio(
                    "Auto — track latest",
                    false,
                    Some(matches!(locked.mode, UpdateMode::Pinned { .. })),
                    |_| Message::SetInstalledUpdateMode(UpdateMode::Auto)
                ),
            ]
            .spacing(16.0)
            .align_y(Vertical::Center);
            if !self.installed_versions.is_empty() {
                let current = match &locked.mode {
                    UpdateMode::Pinned { version } => Some(version.clone()),
                    UpdateMode::Auto => None,
                };
                update_row = update_row.push(
                    pick_list(self.installed_versions.clone(), current, |v| {
                        Message::SetInstalledUpdateMode(UpdateMode::Pinned { version: v })
                    })
                    .placeholder("Pinned — pick a version…"),
                );
            }
            body = body.push(update_row);
        }

        // Dependencies.
        let deps = self
            .graph
            .requires
            .get(specifier)
            .cloned()
            .unwrap_or_default();
        if !deps.is_empty() {
            let mut dep_col = Column::new()
                .spacing(4.0)
                .push(common::section_label("Dependencies"));
            for edge in &deps {
                // This row exists because the open package (`specifier`) depends on
                // `edge.specifier`, so its dot follows the parent's context: it greys when the
                // parent is disabled, instead of staying lit on the dep's global enabled state
                // (which a separately-installed dep keeps on its own row).
                let enabled = self.graph.dep_edge_active(specifier, &edge.specifier);
                let resolved = self.graph.resolved.get(&edge.specifier).cloned();
                let range = if edge.range.is_empty() {
                    resolved
                        .clone()
                        .map(|v| format!("→ v{v}"))
                        .unwrap_or_default()
                } else {
                    format!(
                        "{} → v{}",
                        edge.range,
                        resolved.clone().unwrap_or_else(|| "?".to_string())
                    )
                };
                dep_col =
                    dep_col.push(self.dep_link_row(&edge.specifier, enabled, &range, Some(())));
            }
            if controllable
                && !effective
                && deps
                    .iter()
                    .any(|e| !self.graph.effectively_enabled(&e.specifier))
            {
                let names: Vec<String> = deps
                    .iter()
                    .map(|e| package_display_name(&e.specifier).to_string())
                    .collect();
                dep_col = dep_col.push(
                    text(format!(
                        "Enabling {name} will also enable {}.",
                        names.join(", ")
                    ))
                    .size(12.0)
                    .style(common::muted),
                );
            }
            body = body.push(dep_col);
        }

        // README & source — tabbed (README rendered full-width; Source is the file browser).
        body = body.push(self.installed_file_browser());

        // Actions. A dep-only package is removed automatically and has nothing to manage. A
        // dependency-reference view of a package that's *also* installed on its own defers
        // management to that package's own pane: uninstalling from here would drop only the
        // standalone install while the parent keeps the package resolved, so it reads as a no-op
        // (this mirrors how the dependency view already suppresses the toggle, params, and
        // permissions). Otherwise — the package's own pane — show the real actions. `controllable`
        // only gates the enable *toggle* (an enabled dependent forces it on); it must NOT gate
        // fork/uninstall here, or a package installed directly *and* pulled in as a dependency
        // would lose "Edit a copy" on its own pane just because something else also needs it.
        if dep_only {
            body = body.push(
                container(
                    text(
                        "This dependency is removed automatically once no installed package \
                         requires it.",
                    )
                    .size(12.0)
                    .style(common::muted),
                )
                .padding(10.0)
                .style(common::banner_style),
            );
        } else if viewing_as_dependency {
            body = body.push(self.dependency_also_installed_note(specifier));
        } else {
            body = body.push(self.installed_actions(&name, &enabled_dependents));
        }

        pane_scroll(body)
    }

    fn dep_link_row<'a>(
        &self,
        specifier: &str,
        enabled: bool,
        prefix: &str,
        is_dep: Option<()>,
    ) -> Elem<'a> {
        let name = package_display_name(specifier).to_string();
        // A dependency has no enable state of its own — it loads because the package that requires
        // it is enabled — so its rows read "active/inactive". The user-controllable "enabled/
        // disabled" is reserved for the "Required by" parent rows (`is_dep` is `None`), which are
        // top-level packages the user actually toggles.
        let state = match (is_dep.is_some(), enabled) {
            (true, true) => "active",
            (true, false) => "inactive",
            (false, true) => "enabled",
            (false, false) => "disabled",
        };
        button(
            row![
                common::status_dot(if enabled {
                    NodeStatus::Ok
                } else {
                    NodeStatus::Disabled
                }),
                text(name).size(13.0),
                text(prefix.to_string()).size(12.0).style(common::muted),
                iced::widget::space::horizontal(),
                text(state).size(11.0).style(common::faint),
                text("\u{203A}").size(14.0).style(common::muted),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        )
        .style(button_style::list_item)
        .on_press(Message::SelectInstalledPackage(specifier.to_string()))
        .width(Length::Fill)
        .into()
    }

    /// The tabbed "README & source" area: a README tab (rendered markdown, full width, no interior
    /// scroll — it flows in the pane's own scroll) and a Source tab (file list + on-demand source
    /// browser). No README pseudo-file: a module literally named `README.md` appears in the Source
    /// tab and has its real bytes fetched like any other file.
    fn installed_file_browser(&self) -> Elem<'_> {
        let tab = self.installed_file_tab;
        let tabs = row![
            installed_file_tab_button(tab, InstalledFileTab::Readme, "README"),
            installed_file_tab_button(tab, InstalledFileTab::Source, "Source"),
        ]
        .spacing(4.0)
        .align_y(Vertical::Center);

        let content: Elem = match tab {
            InstalledFileTab::Readme => self.installed_readme_view(),
            InstalledFileTab::Source => self.installed_source_browser(),
        };

        column![tabs, content].spacing(12.0).into()
    }

    /// The README tab: the resolved version's markdown rendered full-width with no interior scroll,
    /// so the whole document flows in the pane's own scrollbar.
    fn installed_readme_view(&self) -> Elem<'_> {
        if let Some(readme) = &self.local_readme {
            let settings = markdown::Settings::with_text_size(
                13.0,
                markdown::Style::from_palette(iced::theme::Palette::DARK),
            );
            container(markdown::view(readme.items(), settings).map(Message::OpenReadmeLink))
                .width(Length::Fill)
                .into()
        } else {
            container(text("No README.").size(13.0).style(common::muted))
                .padding(10.0)
                .into()
        }
    }

    /// The Source tab: the module file list (left) and the selected file's on-demand source (right).
    /// The right pane keeps its own fixed-height scroll so a long file scrolls independently of the
    /// README. README is intentionally absent — it lives in its own tab.
    fn installed_source_browser(&self) -> Elem<'_> {
        let detail = self.installed_detail.as_deref();
        let mut files = Column::new().spacing(2.0);
        if let Some(detail) = detail {
            for module in &detail.modules {
                let selected =
                    self.installed_selected_file.as_deref() == Some(module.subpath.as_str());
                files = files.push(file_row(
                    &module.subpath,
                    selected,
                    Message::SelectInstalledFile(module.subpath.clone()),
                ));
            }
        }

        let right: Elem = match detail {
            None => container(text("Loading…").size(13.0).style(common::muted))
                .padding(10.0)
                .into(),
            Some(detail) if detail.modules.is_empty() => container(
                text("This package ships no source files.")
                    .size(13.0)
                    .style(common::muted),
            )
            .padding(10.0)
            .into(),
            Some(detail) => match self.installed_selected_file.as_deref() {
                Some(subpath) => self.installed_source_view(detail, subpath),
                None => container(
                    text("Select a file to view its source.")
                        .size(13.0)
                        .style(common::muted),
                )
                .padding(10.0)
                .into(),
            },
        };

        row![
            container(scrollable(files)).width(Length::Fixed(220.0)),
            container(right)
                .width(Length::Fill)
                .height(Length::Fixed(320.0))
                .style(common::code_surface_style),
        ]
        .spacing(12.0)
        .into()
    }

    /// Render the right-hand pane of the installed-package source browser for the selected
    /// (non-README) file: its fetched source, or a placeholder for the loading / binary / oversized
    /// / error states. The body is read from the content-addressed cache keyed by the module's
    /// `content_hash`; the fetch is kicked off in [`Self::ensure_selected_source`].
    fn installed_source_view<'a>(
        &'a self,
        detail: &'a ResolvedPackageWire,
        subpath: &str,
    ) -> Elem<'a> {
        let placeholder = |message: String| -> Elem<'a> {
            container(text(message).size(13.0).style(common::muted))
                .padding(10.0)
                .into()
        };
        let Some(module) = detail.modules.iter().find(|m| m.subpath == subpath) else {
            return placeholder("This file is no longer part of the package.".to_string());
        };
        match self.installed_source.get(&module.content_hash) {
            None | Some(FilePreview::Loading) => placeholder("Fetching source\u{2026}".to_string()),
            Some(FilePreview::Text { source, bidi }) => {
                let code = scrollable(
                    container(text(source.as_str()).size(12.0).font(fonts::GEIST_MONO_VF))
                        .padding(10.0)
                        .width(Length::Fill),
                )
                .height(Length::Fill);
                // Trojan-Source warning: if the body carries bidi/invisible control characters, the
                // rendered order can differ from what the engine runs, so caution the auditor rather
                // than trusting their eyes. Pinned above the (scrolling) source so it stays visible.
                if *bidi {
                    column![
                        container(
                            text(
                                "Heads up: this file contains bidirectional or invisible control \
                                 characters, so the text shown may not match what actually runs."
                            )
                            .size(11.0)
                            .style(common::muted),
                        )
                        .padding(8.0)
                        .width(Length::Fill)
                        .style(common::banner_style),
                        code,
                    ]
                    .height(Length::Fixed(320.0))
                    .into()
                } else {
                    code.height(Length::Fixed(320.0)).into()
                }
            }
            Some(FilePreview::Binary { size }) => placeholder(format!(
                "Binary file ({}) \u{2014} not shown.",
                human_size(*size)
            )),
            Some(FilePreview::TooLarge { size }) => placeholder(format!(
                "File is {} \u{2014} too large to preview (limit {}).",
                human_size(*size),
                human_size(SOURCE_PREVIEW_CAP_BYTES)
            )),
            Some(FilePreview::Error(error)) => placeholder(format!(
                "Couldn't load source: {error}\nRe-select the file to try again."
            )),
        }
    }

    /// Shown in a dependency-reference view of a package that is *also* installed on its own:
    /// management belongs to that standalone entry, not here. Uninstalling from this view would
    /// drop only the standalone install while the parent keeps the package resolved — a no-op to
    /// the eye — so we point at the package's own pane instead of offering the action.
    ///
    /// In this view the specifier is always directly installed: dependency edges are cloud
    /// specifiers, so a dep that isn't `dep_only` is in `direct` (an owned `local:` package can't
    /// be a dependency), and `SelectInstalledPackage` opens its own top-level pane.
    fn dependency_also_installed_note(&self, specifier: &str) -> Elem<'_> {
        column![
            container(
                text(
                    "This package is also installed on its own. Manage or uninstall it from its \
                     own entry in the sidebar.",
                )
                .size(12.0)
                .style(common::muted),
            )
            .padding(10.0)
            .style(common::banner_style),
            row![
                iced::widget::space::horizontal(),
                button(text("Open its own pane").size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::SelectInstalledPackage(specifier.to_string())),
            ]
            .align_y(Vertical::Center),
        ]
        .spacing(8.0)
        .into()
    }

    /// The package's own-pane actions. `kept_by` is the set of enabled packages that require this
    /// one: when it's non-empty, "uninstalling" only removes the standalone install — the package
    /// stays resolved as their dependency — so the uninstall action says exactly that rather than
    /// implying full removal.
    fn installed_actions(&self, name: &str, kept_by: &[String]) -> Elem<'_> {
        let mut col = Column::new()
            .spacing(10.0)
            .push(common::section_label("Actions"));

        // Edit a copy (local fork). The button sits on its own row so the explainer text can't
        // squeeze it into a sliver.
        col = col.push(
            column![
                text("Edit a copy").size(13.0),
                text("Make an editable local copy of this package.")
                    .size(11.0)
                    .style(common::muted),
                row![
                    iced::widget::space::horizontal(),
                    button(text("Edit a copy").size(12.0))
                        .style(button_style::secondary)
                        .on_press_maybe(
                            (!self.manage_busy && self.installed_detail.is_some())
                                .then_some(Message::ForkPackage)
                        ),
                ]
                .align_y(Vertical::Center),
            ]
            .spacing(6.0),
        );

        // Uninstall (base-state → inline confirm). When an enabled package still requires this
        // one, removing the standalone install leaves the package resolved as that dependent's
        // dependency, so the label + confirm describe a standalone removal rather than a full
        // uninstall. The "Required by …" section above already names who keeps it.
        let survives = !kept_by.is_empty();
        let mut uninstall = Column::new().spacing(6.0);
        if survives {
            let kept_names = kept_by
                .iter()
                .map(|s| package_display_name(s).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            uninstall = uninstall
                .push(text("Remove standalone install").size(13.0))
                .push(
                    text(format!(
                        "Removes the on-its-own copy. {name} stays installed as a dependency of \
                     {kept_names}."
                    ))
                    .size(11.0)
                    .style(common::muted),
                );
        }
        if self.confirm_uninstall {
            let breaks = &self.uninstall_breaks;
            let orphans = &self.uninstall_orphans;
            // Forced: packages that `require` this one would break without it, so they're removed too
            // (`script/REQUIRED-PACKAGES.md`). Not a choice — keeping them would leave them depending
            // on a missing package.
            if !breaks.is_empty() {
                let names = breaks
                    .iter()
                    .map(|s| package_display_name(s).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                uninstall = uninstall.push(
                    container(
                        text(format!(
                            "{names} {} {name} and will be removed too.",
                            if breaks.len() == 1 {
                                "requires"
                            } else {
                                "require"
                            },
                        ))
                        .size(12.0)
                        .style(common::warning),
                    )
                    .padding(8.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                );
            }
            // apt-style orphan prompt: auto-installed required roots nothing else would need once
            // this (and any forced removals) are gone — offered, never silent.
            if !orphans.is_empty() {
                let names = orphans
                    .iter()
                    .map(|s| package_display_name(s).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                uninstall = uninstall.push(
                    container(
                        text(format!(
                            "Also remove {names}? Nothing else requires {} afterward.",
                            if orphans.len() == 1 { "it" } else { "them" },
                        ))
                        .size(12.0)
                        .style(common::muted),
                    )
                    .padding(8.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                );
            }
            let confirm_label = if !breaks.is_empty() {
                "Remove all".to_string()
            } else if survives {
                "Remove".to_string()
            } else if orphans.is_empty() {
                "Uninstall".to_string()
            } else {
                "Remove all".to_string()
            };
            let mut buttons = row![
                text(if !breaks.is_empty() {
                    "Remove these together?"
                } else if survives {
                    "Remove the standalone install?"
                } else if orphans.is_empty() {
                    "Uninstall (clears params + secrets)?"
                } else {
                    "Remove these together?"
                })
                .size(12.0),
                iced::widget::space::horizontal(),
                button(text("Cancel").size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::CancelUninstall),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center);
            // "Keep them" applies only to the offered orphans; the forced breaks always go.
            if !orphans.is_empty() && !survives {
                buttons = buttons.push(
                    button(text("Keep them").size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::UninstallKeepOrphans),
                );
            }
            buttons = buttons.push(
                button(text(confirm_label).size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::ConfirmUninstall),
            );
            uninstall = uninstall.push(buttons);
        } else {
            uninstall = uninstall.push(
                row![
                    iced::widget::space::horizontal(),
                    button(
                        text(if survives {
                            "Remove standalone install…".to_string()
                        } else {
                            format!("Uninstall {name}…")
                        })
                        .size(12.0)
                    )
                    .style(button_style::secondary)
                    .on_press(Message::RequestUninstall),
                ]
                .align_y(Vertical::Center),
            );
        }
        col = col.push(uninstall);
        col.into()
    }

    // ---- creator automation (read-only) -----------------------------------

    /// The read-only detail of a script-created automation: its pattern and body, plus a jump
    /// to the module/package that created it. These are runtime-generated and managed by their
    /// creator, so nothing here is editable.
    pub(super) fn view_creator_automation(
        &self,
        creator_id: &str,
        kind: AutomationKind,
        name: &str,
    ) -> Elem<'_> {
        let kind_label = match kind {
            AutomationKind::Alias => "alias",
            AutomationKind::Trigger => "trigger",
            AutomationKind::Hotkey => "hotkey",
        };
        let entry = self
            .creator_automations(creator_id)
            .and_then(|creator| match kind {
                AutomationKind::Alias => creator.aliases.get(name),
                AutomationKind::Trigger => creator.triggers.get(name),
                AutomationKind::Hotkey => None,
            });
        let Some(entry) = entry else {
            return pane_scroll(column![
                text(format!("{name} is no longer available."))
                    .size(13.0)
                    .style(common::muted)
            ]);
        };

        let status = if entry.enabled {
            NodeStatus::Ok
        } else {
            NodeStatus::Disabled
        };
        let creator_label = creator_id
            .strip_prefix("module:")
            .map(|subpath| format!("module {subpath}"))
            .or_else(|| {
                creator_id
                    .strip_prefix("package:")
                    .map(|spec| format!("package {}", package_display_name(spec)))
            })
            .unwrap_or_else(|| creator_id.to_string());

        let mut body = column![self.scene_header(
            Some(status),
            name,
            Some(format!(
                "Read-only {kind_label} · created by {creator_label}"
            )),
            Some(common::badge(if entry.enabled {
                "Enabled"
            } else {
                "Disabled"
            })),
        )]
        .spacing(16.0);

        body = body.push(
            text(
                "Created and managed by its package or module — it can't be edited or toggled \
                 here.",
            )
            .size(13.0)
            .style(common::muted),
        );

        if let Some(jump) = Self::creator_jump(creator_id) {
            body = body.push(
                button(text(format!("Open {creator_label}")).size(12.0))
                    .style(button_style::secondary)
                    .on_press(jump),
            );
        }

        body = body.push(
            column![
                common::section_label("Pattern"),
                code_block(if entry.pattern.is_empty() {
                    "(none)"
                } else {
                    &entry.pattern
                }),
            ]
            .spacing(6.0),
        );

        let (body_label, body_text): (&str, String) = match &entry.body {
            AutomationBody::Command(cmd) => ("Sends", cmd.to_string()),
            AutomationBody::Script(Some(src)) => ("Script", src.to_string()),
            AutomationBody::Script(None) => (
                "Script",
                "(JavaScript handler — source not available)".to_string(),
            ),
            AutomationBody::Noop => ("Does", "(nothing)".to_string()),
        };
        body = body
            .push(column![common::section_label(body_label), code_block(&body_text)].spacing(6.0));

        pane_scroll(body)
    }

    // ---- owned package pane -----------------------------------------------

    pub(super) fn view_owned_package(&self) -> Elem<'_> {
        let Some(package) = self.local_package.as_deref() else {
            return pane_scroll(column![text("No package selected.").size(13.0)]);
        };
        let manifest = &package.manifest;
        let visibility = if self.share_is_public {
            "Public"
        } else {
            "Private"
        };
        // Display the *draft* manifest the form is editing (falling back to the on-disk one), so the
        // header/meta/publish-verdict never contradict the editor below while there are unsaved edits.
        let draft = self.manifest_draft.as_ref();
        let disp_version = draft.map_or_else(
            || manifest.version.clone(),
            |d| d.version.trim().to_string(),
        );
        let disp_description = draft.map_or_else(
            || manifest.description.clone(),
            |d| d.description.trim().to_string(),
        );
        let disp_dep_count = draft.map_or(manifest.dependencies.len(), |d| {
            d.dependencies
                .iter()
                .filter(|s| !s.trim().is_empty())
                .count()
        });
        let verdict = publish_verdict(&disp_version, &self.share_versions);

        let mut body = column![self.scene_header(
            None,
            &package.name,
            Some(format!("You own this package · v{disp_version}")),
            Some(common::badge(visibility)),
        )]
        .spacing(16.0);

        // The package description (authored in the manifest) — what Discover shows publicly.
        if !disp_description.is_empty() {
            body = body.push(text(disp_description).size(13.0).style(common::muted));
        }

        if let Some(feedback) = &self.authoring_feedback {
            body = body.push(text(feedback.clone()).size(12.0).style(common::muted));
        }

        // Rename affordance — the folder name is the package's identity (the manifest has no name),
        // and renaming is how a fork is "claimed" so it can be published.
        if let Some(buffer) = &self.rename_buffer {
            body = body.push(
                row![
                    text_input("new name", buffer)
                        .on_input(Message::RenameOwnedChanged)
                        .on_submit(Message::CommitRenameOwned)
                        .width(Length::Fixed(220.0)),
                    button(text("Save name").size(12.0))
                        .style(button_style::primary)
                        .on_press(Message::CommitRenameOwned),
                    button(text("Cancel").size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::CancelRenameOwned),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            );
        } else {
            body = body.push(
                button(text("Rename").size(12.0))
                    .style(button_style::subtle)
                    .on_press(Message::StartRenameOwned),
            );
        }

        // Enabled toggle (mirrors cloud packages' pane switch): a local "runs" when an enabled
        // install of its own specifier loads it; enabling installs it as a base package so it runs
        // even if it's currently only pulled in as a dependency.
        body = body.push(
            row![
                text("Enabled").size(13.0),
                iced::widget::space::horizontal(),
                common::pill_switch(
                    self.local_active(&package.name),
                    false,
                    Some(Message::ToggleLocalEnabled(package.name.clone())),
                ),
            ]
            .align_y(Vertical::Center),
        );

        // Meta.
        let mut meta = row![].spacing(20.0).align_y(Vertical::Center);
        meta = meta.push(metric("Latest", &format!("v{disp_version}")));
        let live_count = self.share_versions.iter().filter(|v| !v.deleted).count();
        meta = meta.push(metric("Versions", &live_count.to_string()));
        if disp_dep_count > 0 {
            meta = meta.push(metric("Dependencies", &disp_dep_count.to_string()));
        }
        body = body.push(meta);

        // Sandbox status: a local package runs sandboxed against its own manifest permissions (the
        // manifest is the grant table). States the runtime reality the QA pass found missing, and
        // links into the manifest editor as the capability-grant mechanism.
        body = body.push(self.view_owned_sandbox_section(package));

        // Rich manifest editor (the smudgy.package.json file itself is hidden from the source
        // browser below).
        body = body.push(self.view_manifest_section());

        // Settings (configured param values) — the manifest above declares the params; this sets
        // the values the package reads when run locally. Keyed by the local package's own-handle
        // specifier, the same one the runtime resolves it under. Renders nothing without params.
        if let Some(settings) = self.view_param_config_section(&self.local_own_spec(&package.name))
        {
            body = body.push(settings);
        }

        // Source browser (editable).
        body = body.push(self.owned_file_browser(package));

        // Publish. Publish reads the on-disk manifest, so it's disabled while the manifest editor has
        // unsaved edits (you'd otherwise ship the pre-edit manifest). Otherwise it's gated on a
        // semver-fluent verdict: disabled while busy, when the version isn't valid publishable semver,
        // or when the number is already used (live/yanked/deleted) — numbers are permanently reserved.
        // Package names are owner-scoped on the server, so a fork always publishes under your own
        // handle and can never clobber another author's package — no client-side rename gate needed.
        let can_publish = !self.authoring_busy
            && !self.manifest_dirty
            && matches!(verdict, PublishVerdict::Ready);
        body = body.push(
            row![
                iced::widget::space::horizontal(),
                button(
                    row![
                        text(crate::assets::bootstrap_icons::CLOUD_UPLOAD)
                            .font(fonts::BOOTSTRAP_ICONS)
                            .size(13.0),
                        text("Publish").size(13.0),
                    ]
                    .spacing(6.0)
                    .align_y(Vertical::Center)
                )
                .style(button_style::primary)
                .on_press_maybe(can_publish.then_some(Message::PublishOwned)),
            ]
            .align_y(Vertical::Center),
        );
        // Explain why Publish is disabled (when it is). Unsaved manifest edits take precedence —
        // publishing them requires saving them first.
        if self.manifest_dirty {
            body = body.push(
                text("Save your manifest changes before publishing.")
                    .size(12.0)
                    .style(common::warning),
            );
        } else {
            match &verdict {
                PublishVerdict::Invalid(reason) => {
                    body = body.push(text(reason.clone()).size(12.0).style(common::danger));
                }
                PublishVerdict::AlreadyUsed => {
                    body = body.push(
                        text(format!(
                            "v{disp_version} is already published. Version numbers can’t be reused."
                        ))
                        .size(12.0)
                        .style(common::warning),
                    );
                }
                PublishVerdict::Ready => {}
            }
        }

        // Published versions.
        let mut versions = Column::new()
            .spacing(4.0)
            .push(common::section_label("Published versions"));
        if self.share_versions.is_empty() {
            versions = versions.push(
                text("No published versions yet.")
                    .size(12.0)
                    .style(common::muted),
            );
        }
        // "latest" is the highest live (non-yanked, non-deleted) version. The list now
        // also carries hard-deleted numbers (reserved forever) which render greyed.
        let latest_idx = self
            .share_versions
            .iter()
            .position(|v| !v.yanked && !v.deleted);
        for (i, v) in self.share_versions.iter().enumerate() {
            // A hard-deleted number: content is gone, but the number stays reserved. Show
            // it greyed so the author sees it's spent; no actions.
            if v.deleted {
                versions = versions.push(
                    row![
                        text(format!("v{}", v.version))
                            .size(13.0)
                            .style(common::faint),
                        text("deleted").size(11.0).style(common::faint),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
                continue;
            }
            let mut left = row![text(format!("v{}", v.version)).size(13.0)]
                .spacing(8.0)
                .align_y(Vertical::Center);
            if Some(i) == latest_idx {
                left = left.push(common::badge("latest"));
            }
            if v.yanked {
                left = left.push(text("yanked").size(11.0).style(common::faint));
            }
            let mut actions = row![
                left,
                iced::widget::space::horizontal(),
                button(text(if v.yanked { "Unyank" } else { "Yank" }).size(11.0))
                    .style(button_style::secondary)
                    .on_press(Message::YankVersion {
                        version: v.version.clone(),
                        yanked: !v.yanked,
                    }),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center);
            // Delete is the heavy, deliberate step — only offered once a version is yanked.
            if v.yanked {
                actions = actions.push(
                    button(text("Delete").size(11.0).style(common::danger))
                        .style(button_style::secondary)
                        .on_press(Message::DeleteVersion(v.version.clone())),
                );
            }
            versions = versions.push(actions);
        }
        versions = versions.push(
            text(
                "Yank prevents new installs without forcefully removing it from existing installs.",
            )
            .size(11.0)
            .style(common::faint),
        );
        body = body.push(versions);

        // Sharing.
        body = body.push(self.owned_sharing_section());

        // Delete package.
        if self.confirm_delete_local {
            body = body.push(
                row![
                    text("Delete this package and all its files?").size(12.0),
                    iced::widget::space::horizontal(),
                    button(text("Cancel").size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::CancelDeleteOwned),
                    button(text("Delete").size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::DeleteOwned),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            );
        } else {
            body = body.push(
                row![
                    iced::widget::space::horizontal(),
                    button(text("Delete package…").size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::RequestDeleteOwned),
                ]
                .align_y(Vertical::Center),
            );
        }

        pane_scroll(body)
    }

    fn owned_file_browser<'a>(&'a self, package: &'a LocalPackage) -> Elem<'a> {
        // A platform-aware "reveal the package folder in the OS file manager" affordance, so the
        // author can drag files in, open the folder in an external editor, or use git.
        let reveal_label = if cfg!(target_os = "windows") {
            "Show in Explorer"
        } else if cfg!(target_os = "macos") {
            "Show in Finder"
        } else {
            "Open Folder"
        };
        let source_header = row![
            common::section_label("Source"),
            iced::widget::space::horizontal(),
            button(text(reveal_label).size(11.0))
                .style(button_style::subtle)
                .on_press(Message::RevealPackageFolder),
        ]
        .align_y(Vertical::Center);
        let mut files = Column::new().spacing(2.0).push(source_header);
        let readme_selected = self.owned_selected_file.is_none();
        if package.readme.is_some() {
            files = files.push(file_row(
                "README.md",
                readme_selected,
                Message::SelectOwnedFile("README.md".to_string()),
            ));
        }
        // `smudgy.package.json` is intentionally not listed: the manifest is edited through the
        // rich manifest editor (`view_manifest_section`) instead of a raw text editor.
        for module in &package.modules {
            let selected = self.owned_selected_file.as_deref() == Some(module.subpath.as_str());
            files = files.push(file_row(
                &module.subpath,
                selected,
                Message::SelectOwnedFile(module.subpath.clone()),
            ));
        }

        let right: Elem<'a> = if self.owned_selected_file.is_none() {
            if let Some(readme) = &self.local_readme {
                let settings = markdown::Settings::with_text_size(
                    13.0,
                    markdown::Style::from_palette(iced::theme::Palette::DARK),
                );
                scrollable(
                    container(
                        markdown::view(readme.items(), settings).map(Message::OpenReadmeLink),
                    )
                    .padding(10.0),
                )
                .height(Length::Fixed(340.0))
                .into()
            } else {
                container(
                    text("Select a file to edit.")
                        .size(13.0)
                        .style(common::muted),
                )
                .padding(10.0)
                .into()
            }
        } else {
            let subpath = self.owned_selected_file.clone().unwrap_or_default();
            let token = std::path::Path::new(&subpath)
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("ts")
                .to_string();
            let editor = iced::widget::text_editor(&self.editor_content)
                .highlight_with::<iced::highlighter::Highlighter>(
                    iced::highlighter::Settings {
                        theme: iced::highlighter::Theme::SolarizedDark,
                        token,
                    },
                    |h: &iced::highlighter::Highlight, _| h.to_format(),
                )
                .font(fonts::GEIST_MONO_VF)
                .on_action(Message::ScriptEditorAction)
                .height(Length::Fixed(300.0));
            column![
                editor,
                row![
                    iced::widget::space::horizontal(),
                    button(text("Save").size(12.0))
                        .style(button_style::primary)
                        .on_press(Message::SaveOwnedFile),
                ]
                .padding(Padding {
                    top: 6.0,
                    bottom: 0.0,
                    left: 0.0,
                    right: 0.0,
                })
                .align_y(Vertical::Center),
            ]
            .spacing(0.0)
            .into()
        };

        row![
            container(scrollable(files)).width(Length::Fixed(220.0)),
            container(right)
                .width(Length::Fill)
                .style(common::code_surface_style)
                .padding(6.0),
        ]
        .spacing(12.0)
        .into()
    }

    fn owned_sharing_section(&self) -> Elem<'_> {
        let mut col = Column::new()
            .spacing(10.0)
            .push(common::section_label("Sharing"));
        if self.share_package_id.is_none() {
            return col
                .push(
                    text("Publish this package first, then share it from here.")
                        .size(12.0)
                        .style(common::muted),
                )
                .into();
        }
        // Visibility card.
        col = col.push(
            container(
                row![
                    column![
                        text(if self.share_is_public {
                            "Public"
                        } else {
                            "Private"
                        })
                        .size(13.0),
                        text(if self.share_is_public {
                            "Anyone can discover and install it."
                        } else {
                            "Only friends you share it with can install it."
                        })
                        .size(11.0)
                        .style(common::muted),
                    ]
                    .spacing(2.0),
                    iced::widget::space::horizontal(),
                    button(
                        text(if self.share_is_public {
                            "Make private"
                        } else {
                            "Make public"
                        })
                        .size(12.0)
                    )
                    .style(button_style::secondary)
                    .on_press(Message::SetVisibility(!self.share_is_public)),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            )
            .padding(12.0)
            .width(Length::Fill)
            .style(common::banner_style),
        );

        // Friends list (private only).
        if !self.share_is_public {
            let mut friends = Column::new().spacing(4.0);
            if self.share_friends.is_empty() {
                friends = friends.push(text("No friends found.").size(12.0).style(common::muted));
            }
            for friend in &self.share_friends {
                let handle = friend
                    .nickname
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string());
                let shared = self
                    .share_grants
                    .iter()
                    .any(|g| g.grantee_id == Some(friend.user_id) || g.all_friends);
                friends = friends.push(
                    row![
                        text(crate::assets::bootstrap_icons::PEOPLE)
                            .font(fonts::BOOTSTRAP_ICONS)
                            .size(13.0)
                            .style(common::muted),
                        text(handle).size(13.0),
                        iced::widget::space::horizontal(),
                        button(text(if shared { "\u{2713} Shared" } else { "Share" }).size(12.0))
                            .style(button_style::secondary)
                            .on_press(Message::ShareWithFriend(friend.user_id)),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
            }
            col = col.push(friends);
        }
        col.into()
    }

    pub(super) fn view_new_package(&self, name: &str, error: Option<&str>) -> Elem<'_> {
        let mut body = column![self.scene_header(
            None,
            "New package",
            Some("Author a shareable smudgy:// package".to_string()),
            None,
        )]
        .spacing(16.0);
        if let Some(error) = error {
            body = body.push(text(error.to_string()).size(13.0).style(common::danger));
        }
        body = body.push(
            row![
                container(text("Name").size(13.0).style(common::muted)).width(Length::Fixed(92.0)),
                text_input("e.g. mySpellTriggers", name).on_input(Message::SetNewPackageName),
            ]
            .spacing(12.0)
            .align_y(Vertical::Center),
        );
        body = body.push(
            text(
                "A package is sort-of a small program. It can contain aliases, triggers, hotkeys, and scripts that run in the background. \
                 It can also include modules and assets that other packages can use.",
            )
            .size(12.0)
            .style(common::muted),
        );
        body = body.push(
            row![
                iced::widget::space::horizontal(),
                button(text("Discard").size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::Discard),
                button(text("Create package").size(13.0))
                    .style(button_style::primary)
                    .on_press(Message::CreatePackage),
            ]
            .spacing(12.0)
            .align_y(Vertical::Center),
        );
        pane_scroll(body)
    }

    // ---- Discover ----------------------------------------------------------

    pub(super) fn view_discover(&self) -> Elem<'_> {
        let mut body = column![self.scene_header(
            None,
            "Discover packages",
            Some("Browse and install public packages".to_string()),
            None,
        )]
        .spacing(16.0);

        // The Install Confirmation window takes over the pane while pending.
        if let Some(prompt) = &self.consent_prompt {
            return pane_scroll(body.push(self.view_consent_prompt(prompt)));
        }

        body = body.push(
            row![
                text_input("Search packages…", &self.discover_query)
                    .on_input(Message::DiscoverQueryChanged)
                    .on_submit(Message::DiscoverSearch),
                button(text("Search").size(13.0))
                    .style(button_style::primary)
                    .on_press(Message::DiscoverSearch),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
        // Host-aware scope radios. "For <host> only" is shown only when this profile has a MUD host;
        // changing any radio re-runs the search (handled in `update`).
        let mut scope = row![
            text("Scope").size(13.0).style(common::muted),
            radio(
                "Relevant",
                DiscoverScope::Relevant,
                Some(self.discover_scope),
                Message::DiscoverScopeChanged
            ),
        ]
        .spacing(16.0)
        .align_y(Vertical::Center);
        if let Some(host) = &self.mud_host {
            scope = scope.push(radio(
                format!("For {host} only"),
                DiscoverScope::HostOnly,
                Some(self.discover_scope),
                Message::DiscoverScopeChanged,
            ));
        }
        scope = scope
            .push(radio(
                "Universal packages only",
                DiscoverScope::Universal,
                Some(self.discover_scope),
                Message::DiscoverScopeChanged,
            ))
            .push(radio(
                "All packages",
                DiscoverScope::All,
                Some(self.discover_scope),
                Message::DiscoverScopeChanged,
            ));
        body = body.push(scope);

        if self.discover_busy {
            body = body.push(text("Working…").size(13.0).style(common::muted));
        }
        if let Some(error) = &self.discover_error {
            body = body.push(text(error.clone()).size(13.0).style(common::danger));
        }
        if let Some(prompt) = &self.param_prompt {
            body = body.push(self.view_param_prompt(prompt));
        }

        if let Some(detail) = self.discover_detail.as_deref() {
            body = body.push(self.view_discover_detail(detail));
        } else {
            for result in &self.discover_results {
                body = body.push(self.discover_result_card(result));
            }
            if self.discover_results.is_empty() && !self.discover_busy {
                body = body.push(
                    text("No results yet — enter a search and press Search.")
                        .size(13.0)
                        .style(common::muted),
                );
            }
        }
        pane_scroll(body)
    }

    pub(super) fn discover_result_card(&self, result: &PackageSearchResult) -> Elem<'_> {
        let installed = super::model::is_installed(
            &self.installed_packages,
            &result.owner_nickname,
            &result.name,
        );
        let action: Elem = if installed {
            button(text("Manage").size(12.0))
                .style(button_style::secondary)
                .on_press(Message::SelectInstalledPackage(specifier_for(
                    &result.owner_nickname,
                    &result.name,
                )))
                .into()
        } else {
            // "View" opens the package's detail page (README, comments, rating); "Install" begins
            // the install straight away (resolve → consent), the same flow as the detail page's
            // own Install button — so the user can install without a detour through the detail.
            row![
                button(text("View").size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::DiscoverSelect {
                        package_id: result.package_id,
                        owner: result.owner_nickname.clone(),
                    }),
                button(text("Install").size(12.0))
                    .style(button_style::primary)
                    .on_press(Message::DiscoverInstallResult {
                        owner: result.owner_nickname.clone(),
                        name: result.name.clone(),
                    }),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center)
            .into()
        };
        // Meta line as a single text run: the prefix and rating average/count inherit the faint base
        // color, while the ★ span is tinted the "out" color.
        let star_color = crate::prefs::current().palette.output;
        let mut meta_spans: Vec<iced::widget::text::Span<'_, ()>> = vec![span(format!(
            "{} · v{} · {} installs · ",
            result.owner_nickname,
            result.latest_version.as_deref().unwrap_or("—"),
            result.install_count,
        ))];
        meta_spans.extend(rating_spans(
            result.avg_rating,
            result.rating_count,
            star_color,
        ));
        let meta_line: Elem = rich_text(meta_spans).size(11.0).style(common::faint).into();
        container(
            row![
                column![
                    row![
                        text(result.name.clone()).size(15.0),
                        if installed {
                            common::badge("Installed")
                        } else {
                            iced::widget::space::horizontal()
                                .width(Length::Shrink)
                                .into()
                        },
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                    text(result.description.clone())
                        .size(12.0)
                        .style(common::muted),
                    meta_line,
                ]
                .spacing(3.0),
                iced::widget::space::horizontal(),
                action,
            ]
            .spacing(10.0)
            .align_y(Vertical::Center),
        )
        .padding(12.0)
        .width(Length::Fill)
        .style(common::card_style)
        .into()
    }

    fn view_discover_detail(&self, detail: &PackageDetail) -> Elem<'_> {
        let pkg = &detail.package;
        let owner = pkg
            .owner_nickname
            .clone()
            .unwrap_or_else(|| "you".to_string());
        let installed = super::model::is_installed(&self.installed_packages, &owner, &pkg.name);
        let action: Elem = if installed {
            button(text("Installed").size(12.0))
                .style(button_style::secondary)
                .into()
        } else {
            button(text("Install").size(12.0))
                .style(button_style::primary)
                .on_press(Message::DiscoverInstall)
                .into()
        };
        // The meta line is a single text run: the owner/version/installs prefix and the rating
        // average/count inherit the muted base color, while the ★ span is tinted the "out" color.
        let star_color = crate::prefs::current().palette.output;
        let mut meta_spans: Vec<iced::widget::text::Span<'_, ()>> = vec![span(format!(
            "{owner} · v{} · {} installs · ",
            detail.latest_version.as_deref().unwrap_or("—"),
            detail.install_count,
        ))];
        meta_spans.extend(rating_spans(
            detail.avg_rating,
            detail.rating_count,
            star_color,
        ));
        let meta_line: Elem = rich_text(meta_spans).size(12.0).style(common::muted).into();
        let mut col = column![
            row![
                button(text("\u{2039} Back").size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::DiscoverBack),
                iced::widget::space::horizontal(),
                action,
            ]
            .align_y(Vertical::Center),
            text(pkg.name.clone()).size(20.0),
            meta_line,
        ]
        .spacing(8.0);
        if !pkg.description.is_empty() {
            col = col.push(text(pkg.description.clone()).size(13.0));
        }
        if let Some(readme) = &self.discover_readme {
            let settings = markdown::Settings::with_text_size(
                13.0,
                markdown::Style::from_palette(iced::theme::Palette::DARK),
            );
            col = col.push(
                container(markdown::view(readme.items(), settings).map(Message::OpenReadmeLink))
                    .padding(10.0)
                    .style(common::code_surface_style),
            );
        }

        // Rate — an account-only write, so the star control shows only when signed in.
        if self.signed_in() {
            col = col.push(star_rate_row(Message::RatePackage));
        }

        // Comments. Existing comments read for everyone; posting a new one needs an account.
        col = col.push(common::section_label("Comments"));
        if self.signed_in() {
            col = col.push(
                row![
                    text_input("Add a comment…", &self.discover_comment_input)
                        .on_input(Message::CommentInputChanged)
                        .on_submit(Message::AddComment),
                    button(text("Post").size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::AddComment),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            );
        }
        if self.discover_comments.is_empty() {
            col = col.push(text("No comments yet.").size(12.0).style(common::muted));
        }
        for comment in &self.discover_comments {
            let who = comment
                .user_nickname
                .clone()
                .unwrap_or_else(|| "someone".to_string());
            col = col.push(
                column![
                    text(who).size(12.0).style(common::accent),
                    text(comment.body.clone()).size(13.0),
                ]
                .spacing(2.0),
            );
        }

        col.into()
    }

    fn view_param_prompt<'a>(&self, prompt: &'a ParamPrompt) -> Elem<'a> {
        let mut form = Column::new()
            .spacing(8.0)
            .push(text(format!("Configure {} v{}", prompt.name, prompt.version)).size(14.0))
            .push(
                text("Required settings (the package won't load until these are filled):")
                    .size(12.0)
                    .style(common::muted),
            );
        for param in &prompt.params {
            let state = prompt.values.get(&param.key);
            let field = if is_secret_string(param) {
                secret_field_row(param, state, ParamTarget::Prompt, "secret value", None)
            } else if let Some(state) = state {
                param_values::view(param, state, ParamTarget::Prompt)
            } else {
                continue;
            };
            form = form.push(field);
        }
        if let Some(error) = &prompt.error {
            form = form.push(text(error.clone()).size(12.0).style(common::danger));
        }
        form = form.push(
            row![
                iced::widget::space::horizontal(),
                button(text("Cancel").size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::ParamPromptCancel),
                button(text("Save").size(12.0))
                    .style(button_style::primary)
                    .on_press(Message::ParamPromptSubmit),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
        container(form)
            .padding(12.0)
            .style(common::banner_style)
            .into()
    }

    /// The inline "Settings" section shown in the installed and owned package panes: an editable
    /// field per declared param, pre-filled with the current value, persisting via
    /// [`Self::param_config_save`]. Renders nothing unless a [`ParamConfig`] is seeded for
    /// `specifier` (i.e. the open package declares params) — the caller guards on a matching
    /// specifier so a stale config from a previous pane can't leak in.
    fn view_param_config_section<'a>(&'a self, specifier: &str) -> Option<Elem<'a>> {
        let config = self
            .param_config
            .as_ref()
            .filter(|c| c.specifier == specifier)?;

        let mut form = Column::new().spacing(10.0).push(
            text("Values this package reads at runtime. Required ones must be set for it to load.")
                .size(12.0)
                .style(common::muted),
        );

        for param in &config.params {
            let state = config.values.get(&param.key);
            let field = if is_secret_string(param) {
                let stored = config.secret_stored.contains(&param.key);
                let placeholder = if stored {
                    "set — leave blank to keep"
                } else {
                    "secret value"
                };
                // A stored secret can only be replaced through the box (never revealed), so offer an
                // explicit Clear — the one way to unset it.
                let clear = stored.then(|| Message::ParamConfigClearSecret(param.key.clone()));
                secret_field_row(param, state, ParamTarget::Config, placeholder, clear)
            } else if let Some(state) = state {
                param_values::view(param, state, ParamTarget::Config)
            } else {
                continue;
            };
            form = form.push(field);
        }

        if let Some(error) = &config.error {
            form = form.push(text(error.clone()).size(12.0).style(common::danger));
        } else if config.saved {
            form = form.push(text("Saved.").size(12.0).style(common::accent));
        }

        form = form.push(
            row![
                iced::widget::space::horizontal(),
                button(text("Save settings").size(12.0))
                    .style(button_style::primary)
                    .on_press(Message::ParamConfigSave),
            ]
            .align_y(Vertical::Center),
        );

        Some(
            column![
                common::section_label("Settings"),
                container(form)
                    .padding(16.0)
                    .width(Length::Fill)
                    .style(common::card_style),
            ]
            .spacing(8.0)
            .into(),
        )
    }

    /// The always-shown Install Confirmation window: an all-or-nothing grant of the closure
    /// permission union, enumerating both what the package *will* and *will NOT* be able to do.
    fn view_consent_prompt<'a>(&self, prompt: &'a ConsentPrompt) -> Elem<'a> {
        let mut form = Column::new()
            .spacing(12.0)
            .push(text(format!("Install {} v{}", prompt.name, prompt.version)).size(16.0))
            .push(
                text(format!("Publisher: {}", prompt.owner))
                    .size(12.0)
                    .style(common::muted),
            );

        let can = permission_can_lines(&prompt.permissions);
        if can.is_empty() {
            // Zero-permission package: a calm one-liner, then the smudgy op-capabilities it can't use
            // (reinforcing the sandbox guarantee) and the guarantee rows (all of which hold for an
            // empty union). The deno per-category denials are omitted — `sandbox_summary` already
            // states "no files/network/system".
            form = form.push(text(sandbox_summary()).size(13.0));
            let mut cannot = Column::new()
                .spacing(4.0)
                .push(text("It also can't:").size(13.0));
            for line in smudgy_cannot_lines(&prompt.permissions.smudgy) {
                cannot = cannot.push(consent_cannot_row(&line));
            }
            for line in sandbox_guarantee_lines(&prompt.permissions) {
                cannot = cannot.push(consent_cannot_row(line));
            }
            form = form.push(cannot);
        } else {
            // A sandbox-escape grant changes what this window IS: not a scoped-permission review
            // but a trust decision. Say so before the enumeration.
            if let Some(banner) = full_access_banner(&prompt.permissions) {
                form = form.push(banner);
            }
            let mut can_col = Column::new()
                .spacing(4.0)
                .push(text("This package will be able to:").size(13.0));
            for line in &can {
                can_col = can_col.push(consent_can_row(line));
            }
            form = form.push(can_col);

            // What it will NOT be able to do (the sandbox guarantee made legible).
            let mut cannot = Column::new()
                .spacing(4.0)
                .push(text("It will NOT be able to:").size(13.0));
            for line in permission_cannot_lines(&prompt.permissions) {
                cannot = cannot.push(consent_cannot_row(&line));
            }
            form = form.push(cannot);
        }

        // "This also installs" — the required roots co-installed with this package. Each lists its
        // own permission closure; already-satisfied roots show as a reuse note, not a fresh install.
        if let Some(section) = self.view_required_roots_section(&prompt.required_roots) {
            form = form.push(section);
        }

        // Cycle warnings (advisory — a requires cycle never blocks the install).
        if !prompt.cycle_warnings.is_empty() {
            let mut warnings = Column::new()
                .spacing(4.0)
                .push(text("Note:").size(13.0).style(common::muted));
            for line in &prompt.cycle_warnings {
                warnings = warnings.push(
                    row![
                        text("\u{26A0}").size(12.0).style(common::muted),
                        text(line.clone()).size(12.0).style(common::muted),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
            }
            form = form.push(warnings);
        }

        // A peer conflict refuses the install: explain it and disable the install buttons.
        let conflict = prompt.conflict.as_deref();
        if let Some(message) = conflict {
            form = form.push(
                container(
                    column![
                        row![
                            text("\u{26A0}").size(14.0).style(common::danger),
                            text("Can't install \u{2014} required-package version conflict")
                                .size(14.0),
                        ]
                        .spacing(8.0)
                        .align_y(Vertical::Center),
                        text(message.to_string()).size(12.0),
                    ]
                    .spacing(6.0),
                )
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style),
            );
        }

        // A version floor above this smudgy also refuses the install: the engine would refuse
        // the package at every load, so installing it would only install something broken.
        let needs_smudgy = prompt.needs_smudgy.as_deref();
        if let Some(message) = needs_smudgy {
            form = form.push(
                container(
                    column![
                        row![
                            text("\u{26A0}").size(14.0).style(common::danger),
                            text("Can't install \u{2014} needs a newer smudgy").size(14.0),
                        ]
                        .spacing(8.0)
                        .align_y(Vertical::Center),
                        text(format!("{message}.")).size(12.0),
                    ]
                    .spacing(6.0),
                )
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style),
            );
        }

        if let Some(error) = &prompt.error {
            form = form.push(text(error.clone()).size(12.0).style(common::danger));
        }
        // Both install actions grant the shown permissions (and co-install the required set); they
        // differ only in whether the packages are enabled (run) now or left off for review. A peer
        // conflict or version-floor refusal disables both install buttons — only Cancel remains.
        let can_install = conflict.is_none() && needs_smudgy.is_none();
        form = form.push(
            row![
                iced::widget::space::horizontal(),
                button(text("Cancel").size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::ConsentCancel),
                button(text("Install, don't enable").size(12.0))
                    .style(button_style::secondary)
                    .on_press_maybe(can_install.then_some(Message::ConsentGrant { enable: false })),
                button(text("Install & enable").size(12.0))
                    .style(button_style::primary)
                    .on_press_maybe(can_install.then_some(Message::ConsentGrant { enable: true })),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
        container(form)
            .padding(16.0)
            .width(Length::Fill)
            .style(common::card_style)
            .into()
    }

    /// The "This also installs" section of the consent prompt: the required top-level roots
    /// co-installed alongside the chosen package (`script/REQUIRED-PACKAGES.md`). A not-yet-satisfied
    /// root lists its name/version, whether it's an upgrade of an existing install, and its own
    /// permission closure; an already-satisfied root is shown as a brief "already installed" reuse
    /// note. `None` when nothing is required, so the section is omitted entirely.
    fn view_required_roots_section<'a>(&self, roots: &'a [RequiredRoot]) -> Option<Elem<'a>> {
        if roots.is_empty() {
            return None;
        }
        let mut col = Column::new()
            .spacing(8.0)
            .push(text("This also installs:").size(13.0));
        for root in roots {
            if root.already_satisfied {
                col = col.push(
                    row![
                        text("\u{2022}").size(13.0).style(common::muted),
                        text(format!(
                            "{} v{} \u{2014} already installed",
                            root.name, root.version
                        ))
                        .size(12.0)
                        .style(common::muted),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
                continue;
            }
            let heading = if root.is_upgrade {
                format!("{} \u{2192} v{} (upgrade)", root.name, root.version)
            } else {
                format!("{} v{}", root.name, root.version)
            };
            let mut entry = Column::new()
                .spacing(4.0)
                .push(text(heading).size(13.0).style(common::accent));
            let can = permission_can_lines(&root.permissions);
            if can.is_empty() {
                entry = entry.push(text(sandbox_summary()).size(12.0).style(common::muted));
            } else {
                // A co-installed root with a sandbox-escape grant gets its own compact call-out —
                // the main banner above only covers the package the user actually picked.
                if union_risk(&root.permissions) == PermissionRisk::Critical {
                    entry = entry.push(
                        row![
                            text("\u{26A0}").size(12.0).style(common::danger),
                            text(format!(
                                "Effectively full access \u{2014} it can {}.",
                                join_reasons(&escape_reasons(&root.permissions))
                            ))
                            .size(12.0)
                            .style(common::danger),
                        ]
                        .spacing(8.0)
                        .align_y(Vertical::Center),
                    );
                }
                for line in &can {
                    entry = entry.push(consent_can_row(line));
                }
            }
            col = col.push(
                container(entry)
                    .padding(10.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
            );
        }
        Some(col.into())
    }

    /// The owned-package sandbox status. A local package runs sandboxed against its OWN manifest
    /// permissions — the manifest IS the grant table, so the author edits it (here, via "Edit
    /// capabilities") and reloads to test the exact sandbox an installer gets. Reuses the installed
    /// pane's `permission_can_lines`/`consent_can_row`/`sandbox_summary` so both panes describe the
    /// sandbox identically. Also offers the advanced "develop unsandboxed" (trust) escape hatch —
    /// full, unenumerated access while iterating (scoped `run`/`ffi` grants are declarable in the
    /// manifest, so the hatch is a convenience, no longer the only route to native power).
    fn view_owned_sandbox_section(&self, package: &LocalPackage) -> Elem<'_> {
        let own_spec = self.local_own_spec(&package.name);
        let unsandboxed = self
            .installed_packages
            .iter()
            .find(|p| p.specifier == own_spec)
            .is_some_and(|p| p.trusted);

        let mut col = Column::new()
            .spacing(8.0)
            .push(common::section_label("Sandbox"));

        if unsandboxed {
            col = col.push(
                container(
                    column![
                        row![
                            text("\u{26A0}").size(14.0).style(common::danger),
                            text("Developing unsandboxed \u{2014} full access").size(14.0),
                        ]
                        .spacing(8.0)
                        .align_y(Vertical::Center),
                        text(
                            "Runs on your main isolate with full access to your computer, as if you \
                             wrote it. Return it to the manifest sandbox to test what installers get."
                        )
                        .size(12.0)
                        .style(common::muted),
                    ]
                    .spacing(6.0),
                )
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style),
            );
            // Re-sandboxing is the safe direction — always offered, even with advanced features off.
            col = col.push(
                row![
                    iced::widget::space::horizontal(),
                    button(text("Use manifest sandbox").size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::SetLocalUnsandboxed(false)),
                ]
                .align_y(Vertical::Center),
            );
            return col.into();
        }

        // Sandboxed against the live manifest: show what it currently grants (reusing the consent
        // can-lines), and point at the manifest editor as the grant mechanism. The full-access
        // banner shows here too — the author sees exactly the framing installers will get.
        let can = permission_can_lines(&package.manifest.permissions);
        let mut card =
            column![text("Runs sandboxed against this package\u{2019}s manifest").size(14.0)]
                .spacing(6.0);
        if can.is_empty() {
            card = card.push(text(sandbox_summary()).size(12.0).style(common::muted));
        } else {
            if let Some(banner) = full_access_banner(&package.manifest.permissions) {
                card = card.push(banner);
            }
            card = card.push(text("It can:").size(12.0).style(common::muted));
            let mut lines = Column::new().spacing(4.0);
            for line in &can {
                lines = lines.push(consent_can_row(line));
            }
            card = card.push(lines);
        }
        card = card.push(
            row![
                button(text("Edit capabilities").size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::EditOwnedCapabilities),
            ]
            .align_y(Vertical::Center),
        );
        col = col.push(
            container(card)
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style),
        );

        // Advanced escape hatch: develop with full access (trust), for ffi/run etc. a sandbox can't
        // grant. Gated on advanced features + a heavy two-step confirm (reusing the trust confirm
        // state; only one package pane shows at a time).
        if self.advanced_features {
            if self.confirm_trust {
                col = col.push(
                    container(
                        column![
                            row![
                                text("\u{26A0}").size(14.0).style(common::danger),
                                text("Develop this package unsandboxed?").size(14.0),
                            ]
                            .spacing(8.0)
                            .align_y(Vertical::Center),
                            text(
                                "It will run with FULL access to your computer, on your main isolate, \
                                 sharing state with your own scripts — ignoring its manifest's \
                                 permissions. Use this only while developing, when maintaining the \
                                 manifest's permission list gets in the way. To ship native power, \
                                 declare scoped run/ffi permissions in the manifest instead."
                            )
                            .size(12.0),
                            row![
                                iced::widget::space::horizontal(),
                                button(text("Cancel").size(12.0))
                                    .style(button_style::secondary)
                                    .on_press(Message::CancelTrust),
                                button(text("Develop unsandboxed").size(12.0))
                                    .style(button_style::primary)
                                    .on_press(Message::SetLocalUnsandboxed(true)),
                            ]
                            .spacing(8.0)
                            .align_y(Vertical::Center),
                        ]
                        .spacing(10.0),
                    )
                    .padding(12.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                );
            } else {
                col = col.push(
                    row![
                        column![
                            text("Develop unsandboxed (advanced)").size(13.0),
                            text(
                                "Run it on your main isolate with full access (and inspector \
                                 debugging), ignoring the manifest sandbox."
                            )
                            .size(11.0)
                            .style(common::muted),
                        ]
                        .spacing(2.0),
                        iced::widget::space::horizontal(),
                        button(text("Develop unsandboxed\u{2026}").size(12.0))
                            .style(button_style::secondary)
                            .on_press(Message::RequestTrust),
                    ]
                    .align_y(Vertical::Center),
                );
            }
        }
        col.into()
    }

    /// The manage-pane permission view: the consented closure union read-only (all-or-nothing,
    /// so no per-permission revoke), or "full access (trusted)" — plus the trust toggle.
    /// The "Permissions" card for a dependency-reference view. A dependency isn't its own
    /// sandboxed package: it loads into its parent's isolate and runs with the parent's grants, so
    /// it has no separate consent of its own. Describing its manifest permissions here (as the
    /// installed pane does) would imply a sandbox and a grant/keep choice that don't exist in this
    /// context — so explain the parent relationship in plain terms and send the user there instead.
    fn view_dependency_permissions_section(&self, parent: &str) -> Elem<'_> {
        let parent_name = package_display_name(parent).to_string();
        let card = column![
            text(format!("Runs inside {parent_name}")).size(14.0),
            text(format!(
                "This is a building block of {parent_name}. It runs in {parent_name}\u{2019}s \
                 sandbox and can only do what {parent_name} is allowed to do \u{2014} it has no \
                 permissions of its own. To review or change its access, open {parent_name}."
            ))
            .size(12.0)
            .style(common::muted),
        ]
        .spacing(6.0);
        column![
            common::section_label("Permissions"),
            container(card)
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style),
        ]
        .spacing(8.0)
        .into()
    }

    fn view_permissions_section(&self, locked: &LockedPackage) -> Elem<'_> {
        let mut col = Column::new()
            .spacing(8.0)
            .push(common::section_label("Permissions"));

        if locked.trusted {
            col = col.push(
                container(
                    column![
                        row![
                            text("\u{26A0}").size(14.0).style(common::danger),
                            text("Full access \u{2014} sandbox removed").size(14.0),
                        ]
                        .spacing(8.0)
                        .align_y(Vertical::Center),
                        text(
                            "Runs on your main isolate with full access to your computer, as if you \
                             wrote it — sharing state with your own scripts."
                        )
                        .size(12.0)
                        .style(common::muted),
                    ]
                    .spacing(6.0),
                )
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style),
            );
            // Restoring the sandbox is the safe direction — always offered, even with advanced
            // features off (so a package can't get stuck unsandboxed if the gate is later disabled).
            col = col.push(
                row![
                    iced::widget::space::horizontal(),
                    button(text("Restore sandbox").size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::SetTrusted(false)),
                ]
                .align_y(Vertical::Center),
            );
            return col.into();
        }

        // Sandboxed: mirror the trusted card — a heading plus a breakdown of the consented access
        // (read-only; the union is whatever was granted at install). A consented sandbox-escape
        // grant keeps its banner here too: "Runs in sandbox" must not read as containment the
        // grant no longer provides.
        let consented = locked.consented_permissions.clone().unwrap_or_default();
        let can = permission_can_lines(&consented);
        let heading = if union_risk(&consented) == PermissionRisk::Critical {
            "Runs in sandbox \u{2014} with grants that can escape it"
        } else {
            "Runs in sandbox"
        };
        let mut card = column![text(heading).size(14.0)].spacing(6.0);
        if can.is_empty() {
            card = card.push(text(sandbox_summary()).size(12.0).style(common::muted));
        } else {
            if let Some(banner) = full_access_banner(&consented) {
                card = card.push(banner);
            }
            card = card.push(text("It can only:").size(12.0).style(common::muted));
            let mut lines = Column::new().spacing(4.0);
            for line in &can {
                lines = lines.push(consent_can_row(line));
            }
            card = card.push(lines);
        }
        if locked.consented_permissions.is_none() {
            card = card.push(
                text(
                    "Not yet consented — denied all access until you confirm its permissions \
                     (reinstall from Discover).",
                )
                .size(11.0)
                .style(common::faint),
            );
        }
        col = col.push(
            container(card)
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style),
        );

        // "Remove sandbox" is an advanced, footgun-prone action (run the package with full
        // authority), so the affordance only appears when advanced scripting features are unlocked
        // in Settings. The heavy two-step confirm applies.
        if self.advanced_features {
            if self.confirm_trust {
                col = col.push(
                    container(
                        column![
                            row![
                                text("\u{26A0}").size(14.0).style(common::danger),
                                text("Remove this package's sandbox?").size(14.0),
                            ]
                            .spacing(8.0)
                            .align_y(Vertical::Center),
                            text(
                                "Without its sandbox, this package runs with FULL access to your \
                                 computer, on your main isolate, sharing state with your own \
                                 scripts. It can do anything you can. Only remove the sandbox for \
                                 packages you would have written yourself."
                            )
                            .size(12.0),
                            row![
                                iced::widget::space::horizontal(),
                                button(text("Cancel").size(12.0))
                                    .style(button_style::secondary)
                                    .on_press(Message::CancelTrust),
                                button(text("Remove sandbox").size(12.0))
                                    .style(button_style::primary)
                                    .on_press(Message::SetTrusted(true)),
                            ]
                            .spacing(8.0)
                            .align_y(Vertical::Center),
                        ]
                        .spacing(10.0),
                    )
                    .padding(12.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                );
            } else {
                col = col.push(
                    row![
                        column![
                            text("Remove sandbox (advanced)").size(13.0),
                            text(
                                "Run it on your main isolate with full access to your computer. This allows the package to be debugged with the inspector."
                            )
                            .size(11.0)
                            .style(common::muted),
                        ]
                        .spacing(2.0),
                        iced::widget::space::horizontal(),
                        button(text("Remove sandbox\u{2026}").size(12.0))
                            .style(button_style::secondary)
                            .on_press(Message::RequestTrust),
                    ]
                    .spacing(10.0)
                    .align_y(Vertical::Center),
                );
            }
        }
        col.into()
    }

    /// The update re-prompt card: the new version's *added* asks beyond the consented
    /// baseline. "Grant & update" adopts the new union; "Keep current perms" leaves the old union
    /// enforced (the new asks stay withheld).
    fn view_update_delta<'a>(&self, delta: &'a UpdateDelta) -> Elem<'a> {
        // A version-floor hold-back is informational: no grant can load the held-back version
        // (only updating smudgy, or pinning an older version, would), so the card explains the
        // floor and offers only dismissal.
        if let Some(reason) = &delta.needs_smudgy {
            let col = Column::new()
                .spacing(8.0)
                .push(
                    row![
                        common::status_dot(NodeStatus::Warning),
                        text(format!(
                            "Update held back — {} needs a newer smudgy",
                            delta.name
                        ))
                        .size(14.0),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                )
                .push(
                    // No "you're running vX" claim: the lockfile's last-resolved version can
                    // be stale (a floored pin, or a smudgy downgrade since it last loaded),
                    // so the card states only what is certainly true. The reason carries its
                    // own remedy.
                    text(format!(
                        "v{} is held back \u{2014} {reason}.",
                        delta.version
                    ))
                    .size(12.0)
                    .style(common::muted),
                )
                .push(
                    row![
                        iced::widget::space::horizontal(),
                        button(text("OK").size(12.0))
                            .style(button_style::secondary)
                            .on_press(Message::DismissUpdate),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
            return container(col)
                .padding(14.0)
                .width(Length::Fill)
                .style(common::card_style)
                .into();
        }
        let mut col = Column::new()
            .spacing(8.0)
            .push(
                row![
                    common::status_dot(NodeStatus::Warning),
                    text(format!(
                        "Update blocked — {} needs more permissions",
                        delta.name
                    ))
                    .size(14.0),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            )
            .push(
                text(match &delta.current_version {
                    Some(current) => format!(
                        "You're running v{current} (it fits your grant). v{} is held back — to \
                         update, it additionally needs to:",
                        delta.version
                    ),
                    None => format!(
                        "v{} is held back — to load it, it additionally needs to:",
                        delta.version
                    ),
                })
                .size(12.0)
                .style(common::muted),
            );
        // An update whose ADDED asks include a sandbox escape is a bigger decision than "more
        // hosts" — the banner makes granting it a deliberate trust call, not a reflex.
        if let Some(banner) = full_access_banner(&delta.added) {
            col = col.push(banner);
        }
        let mut lines = Column::new().spacing(4.0);
        for line in permission_can_lines(&delta.added) {
            lines = lines.push(consent_can_row(&line));
        }
        col = col.push(lines);
        col = col.push(
            row![
                iced::widget::space::horizontal(),
                button(text("Keep current version").size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::DismissUpdate),
                button(text("Grant & update").size(12.0))
                    .style(button_style::primary)
                    .on_press(Message::GrantUpdate),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
        container(col)
            .padding(14.0)
            .width(Length::Fill)
            .style(common::card_style)
            .into()
    }

    // ---- Shared-with-me ----------------------------------------------------

    pub(super) fn view_shared(&self) -> Elem<'_> {
        let mut body =
            column![
                self.scene_header(
                    None,
                    "Private & Shared",
                    Some(
                        "Private packages you own and packages friends have shared with you."
                            .to_string()
                    ),
                    None,
                )
            ]
            .spacing(16.0);

        if !self.signed_in() {
            return pane_scroll(body.push(
                self.signed_out_banner("see the packages you own and ones friends have shared"),
            ));
        }
        if let Some(error) = &self.discover_error {
            body = body.push(text(error.clone()).size(13.0).style(common::danger));
        }
        if let Some(prompt) = &self.consent_prompt {
            return pane_scroll(body.push(self.view_consent_prompt(prompt)));
        }
        if let Some(prompt) = &self.param_prompt {
            return pane_scroll(body.push(self.view_param_prompt(prompt)));
        }

        // ---- Your packages (owned in the cloud) ----
        body = body.push(common::section_label("Your packages"));
        // Your own nickname is the owner handle for installing/resolving these — the server omits
        // owner_nickname on /packages/mine (it's you), so it isn't carried on the rows.
        let my_nick = self
            .cloud
            .snapshot
            .get()
            .nickname_text()
            .unwrap_or_default();
        match &self.my_cloud_packages {
            None => {
                body = body.push(text("Loading…").size(13.0).style(common::muted));
            }
            Some(list) => {
                for detail in list {
                    // A package that's also an authored copy on THIS machine lives in the
                    // sidebar's "Local" section (its own authoring pane). We still list it here —
                    // so an owner can find a package they published as Private — but badge it
                    // "Local" rather than offer to install a cloud copy over your own source.
                    let is_local = self
                        .local_packages
                        .iter()
                        .any(|n| n.eq_ignore_ascii_case(&detail.package.name));
                    let installed = super::model::is_installed(
                        &self.installed_packages,
                        &my_nick,
                        &detail.package.name,
                    );
                    let action: Elem = if is_local {
                        common::badge("Local")
                    } else if installed {
                        common::badge("Installed")
                    } else {
                        button(text("Install").size(12.0))
                            .style(button_style::primary)
                            .on_press(Message::InstallShared {
                                owner: my_nick.clone(),
                                name: detail.package.name.clone(),
                            })
                            .into()
                    };
                    let mut title_row = row![text(detail.package.name.clone()).size(15.0)]
                        .spacing(8.0)
                        .align_y(Vertical::Center);
                    if !detail.package.is_public {
                        title_row = title_row.push(common::badge("Private"));
                    }
                    body = body.push(
                        container(
                            row![
                                column![
                                    title_row,
                                    text(detail.package.description.clone())
                                        .size(12.0)
                                        .style(common::muted),
                                    text(format!(
                                        "v{}",
                                        detail.latest_version.as_deref().unwrap_or("—")
                                    ))
                                    .size(11.0)
                                    .style(common::faint),
                                ]
                                .spacing(3.0),
                                iced::widget::space::horizontal(),
                                action,
                            ]
                            .spacing(10.0)
                            .align_y(Vertical::Center),
                        )
                        .padding(12.0)
                        .width(Length::Fill)
                        .style(common::card_style),
                    );
                }
                if list.is_empty() {
                    body = body.push(
                        text("You don't own any cloud packages yet.")
                            .size(13.0)
                            .style(common::muted),
                    );
                }
            }
        }

        // ---- Shared with you (by friends) ----
        body = body.push(common::section_label("Shared with you"));
        match &self.shared_with_me {
            None => {
                body = body.push(text("Loading…").size(13.0).style(common::muted));
            }
            Some(list) if list.is_empty() => {
                body = body.push(
                    text("No packages have been shared with you yet.")
                        .size(13.0)
                        .style(common::muted),
                );
            }
            Some(list) => {
                for detail in list {
                    let owner = detail.package.owner_nickname.clone().unwrap_or_default();
                    let installed = super::model::is_installed(
                        &self.installed_packages,
                        &owner,
                        &detail.package.name,
                    );
                    let action: Elem = if installed {
                        common::badge("Installed")
                    } else {
                        button(text("Install").size(12.0))
                            .style(button_style::primary)
                            .on_press(Message::InstallShared {
                                owner: owner.clone(),
                                name: detail.package.name.clone(),
                            })
                            .into()
                    };
                    body = body.push(
                        container(
                            row![
                                column![
                                    text(detail.package.name.clone()).size(15.0),
                                    text(detail.package.description.clone())
                                        .size(12.0)
                                        .style(common::muted),
                                    text(format!(
                                        "{owner} · v{}",
                                        detail.latest_version.as_deref().unwrap_or("—")
                                    ))
                                    .size(11.0)
                                    .style(common::faint),
                                ]
                                .spacing(3.0),
                                iced::widget::space::horizontal(),
                                action,
                            ]
                            .spacing(10.0)
                            .align_y(Vertical::Center),
                        )
                        .padding(12.0)
                        .width(Length::Fill)
                        .style(common::card_style),
                    );
                }
            }
        }
        pane_scroll(body)
    }
}

// ---- view helpers ----------------------------------------------------------

/// The rating spans for a [`rich_text`] run: a ★ glyph tinted `star_color`, then the average and
/// count — or a single `unrated` span. Shared by the installed-pane [`rating_metric`] and the
/// Discover detail header so the ★ tinting and wording stay identical. The spans carry no links, so
/// the `Link` type is `()` (pinned by the return type).
fn rating_spans<'a>(
    avg_rating: Option<f64>,
    rating_count: i64,
    star_color: Color,
) -> Vec<iced::widget::text::Span<'a, ()>> {
    match avg_rating {
        Some(r) => vec![
            span("\u{2605}").color(star_color),
            span(format!(" {r:.1} ({rating_count})")),
        ],
        None => vec![span("unrated")],
    }
}

/// A 1–5 star rating control emitting `make_msg(stars)` on press. Rating is an account-only write,
/// so callers gate this on `signed_in()`. Shared by the Discover detail and the installed-package
/// pane. The star glyphs take the terminal palette's "out" (output) color, matching outgoing text.
fn star_rate_row<'a>(make_msg: fn(i16) -> Message) -> Elem<'a> {
    let star_color = crate::prefs::current().palette.output;
    let mut rate = row![text("Rate").size(12.0).style(common::muted)]
        .spacing(6.0)
        .align_y(Vertical::Center);
    for stars in 1..=5_i16 {
        rate = rate.push(
            button(text("\u{2605}").size(13.0).style(move |_| text::Style {
                color: Some(star_color),
            }))
            .style(button_style::subtle)
            .on_press(make_msg(stars))
            .padding(2),
        );
    }
    rate.into()
}

/// A [`metric`]-styled cell for the cloud rating: the ★ glyph is its own span tinted with the "out"
/// (output) palette color while the average/count keep the default metric color. Using `rich_text`
/// spans keeps it a single text run (one baseline, wraps as a unit) rather than separate widgets.
/// Falls back to a plain `unrated` value when the package has no ratings yet.
fn rating_metric<'a>(avg_rating: Option<f64>, rating_count: i64, star_color: Color) -> Elem<'a> {
    let value_font = Font {
        weight: iced::font::Weight::Light,
        ..fonts::GEIST_VF
    };
    let value: Elem = rich_text(rating_spans(avg_rating, rating_count, star_color))
        .size(20.0)
        .font(value_font)
        .into();
    column![value, text("RATING").size(10.0).style(common::faint)]
        .spacing(2.0)
        .into()
}

fn metric<'a>(label: &str, value: &str) -> Elem<'a> {
    column![
        text(value.to_string()).size(20.0).font(Font {
            weight: iced::font::Weight::Light,
            ..fonts::GEIST_VF
        }),
        text(label.to_uppercase()).size(10.0).style(common::faint),
    ]
    .spacing(2.0)
    .into()
}

/// A monospaced read-only code panel (pattern source / automation body) on the code surface.
fn code_block<'a>(content: &str) -> Elem<'a> {
    container(text(content.to_string()).size(12.0).font(Font::MONOSPACE))
        .width(Length::Fill)
        .padding(10.0)
        .style(common::code_surface_style)
        .into()
}

/// One "can do" row in the consent enumeration: a bullet, the capability label, and (when the line
/// names one) the specific host/path/var in monospace.
fn consent_can_row<'a>(line: &PermissionLine) -> Elem<'a> {
    // The row's framing follows its risk tier: a plain scoped grant keeps the quiet accent
    // bullet; a caution line goes amber; a sandbox-escape line goes red and says so inline, so
    // the tier survives even when a caller shows rows without the full-access banner.
    let (bullet, bullet_style, head_style): (&str, _, fn(&crate::theme::Theme) -> text::Style) =
        match line.risk {
            PermissionRisk::Normal => (
                "\u{2022}",
                common::accent as fn(&crate::theme::Theme) -> text::Style,
                common::regular,
            ),
            PermissionRisk::Caution => ("\u{26A0}", common::warning, common::warning),
            PermissionRisk::Critical => ("\u{26A0}", common::danger, common::danger),
        };
    let mut r = row![
        text(bullet).size(13.0).style(bullet_style),
        text(line.head.clone()).size(13.0).style(head_style),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center);
    if let Some(detail) = &line.detail {
        r = r.push(
            text(detail.clone())
                .size(12.0)
                .font(fonts::GEIST_MONO_VF)
                .style(common::muted),
        );
    }
    r.into()
}

/// One "cannot do" row: an ✕ and the categorical denial, muted.
fn consent_cannot_row<'a>(line: &str) -> Elem<'a> {
    row![
        text("\u{2715}").size(11.0).style(common::faint),
        text(line.to_string()).size(13.0).style(common::muted),
    ]
    .spacing(8.0)
    .align_y(Vertical::Center)
    .into()
}

fn file_row<'a>(label: &str, selected: bool, msg: Message) -> Elem<'a> {
    button(
        row![
            text(crate::assets::bootstrap_icons::FONTS)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(12.0)
                .style(common::muted),
            text(label.to_string())
                .size(12.0)
                .font(fonts::GEIST_MONO_VF),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center),
    )
    .style(if selected {
        button_style::list_item_selected
    } else {
        button_style::list_item
    })
    .on_press(msg)
    .width(Length::Fill)
    .into()
}

/// One tab button for the installed-package "README & source" area. Active uses the selected
/// list-item fill; inactive is quiet — mirroring the manifest editor's tab strip.
fn installed_file_tab_button<'a>(
    active: InstalledFileTab,
    tab: InstalledFileTab,
    label: &str,
) -> Elem<'a> {
    button(text(label.to_string()).size(13.0))
        .style(if active == tab {
            button_style::list_item_selected
        } else {
            button_style::list_item
        })
        .on_press(Message::SelectInstalledFileTab(tab))
        .padding(Padding {
            top: 6.0,
            bottom: 6.0,
            left: 12.0,
            right: 12.0,
        })
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fork_mirrors_source_enabled_state() {
        // Source disabled: the fork is an inspect-only copy.
        assert_eq!(fork_activation(false, false), ForkActivation::Inactive);
        assert_eq!(fork_activation(false, true), ForkActivation::Inactive);
        // Source enabled, distinct slot (foreign fork or renamed self-fork): hand the name over.
        assert_eq!(fork_activation(true, false), ForkActivation::TookOver);
        // Source enabled, self-fork keeping the leaf name: share the slot, just enable it. This is
        // the regression guard — the old path disabled the shared entry and left it disabled.
        assert_eq!(fork_activation(true, true), ForkActivation::Mirrored);
    }

    /// The source-browser classifier is an audit-safety guard, so pin its decisions: plain UTF-8 is
    /// text, NUL or invalid UTF-8 is binary (never decoded into the view), and the size cap is
    /// enforced on the *actual* bytes regardless of any declared size.
    #[test]
    fn classify_source_text_vs_binary_vs_oversize() {
        assert!(matches!(
            classify_source(b"export const x = 1;\n".to_vec()),
            FilePreview::Text { bidi: false, .. }
        ));
        // A NUL byte → binary, even though the rest is valid UTF-8.
        assert!(matches!(
            classify_source(b"valid text\0then nul".to_vec()),
            FilePreview::Binary { .. }
        ));
        // Invalid UTF-8 (lone continuation byte) → binary, never lossy-decoded.
        assert!(matches!(
            classify_source(vec![0xff, 0xfe, 0x41]),
            FilePreview::Binary { .. }
        ));
        // Over the cap (by actual length) → too large, even with no NUL/invalid bytes.
        let over = usize::try_from(SOURCE_PREVIEW_CAP_BYTES + 1).unwrap();
        match classify_source(vec![b'a'; over]) {
            FilePreview::TooLarge { size } => assert_eq!(size, SOURCE_PREVIEW_CAP_BYTES + 1),
            other => panic!("expected TooLarge, got {other:?}"),
        }
    }

    /// Bidi/invisible control characters (Trojan Source) must trip the warning flag so the view can
    /// caution the auditor; ordinary international text (incl. RTL letters) must not.
    #[test]
    fn classify_source_flags_deceptive_unicode() {
        // RLO override mid-line — the canonical Trojan-Source vector.
        assert!(matches!(
            classify_source("let a = \u{202E}evil\u{202C};".as_bytes().to_vec()),
            FilePreview::Text { bidi: true, .. }
        ));
        // Zero-width space hidden in an identifier.
        assert!(has_deceptive_unicode("ad\u{200B}min"));
        // Plain Arabic (RTL letters, no control chars) is legitimate source/text — no warning.
        assert!(!has_deceptive_unicode(
            "\u{0645}\u{0631}\u{062D}\u{0628}\u{0627} = 1"
        ));
        assert!(!has_deceptive_unicode("const greeting = \"hello\";"));
    }

    /// The placeholder size formatter: integer-only, one decimal, correct unit boundaries.
    #[test]
    fn human_size_formats() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1024), "1.0 KB");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(1024 * 1024), "1.0 MB");
        assert_eq!(human_size(1024 * 1024 * 3 / 2), "1.5 MB");
    }
}
