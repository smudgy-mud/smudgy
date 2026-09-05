//! Package panes and the client-side dependency model:
//! installed packages, owned packages, Discover, and Private & Shared (the caller's own
//! cloud packages plus packages friends have shared).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use iced::Task;
use iced::alignment::Vertical;
use iced::widget::{
    Column, button, column, container, markdown, radio, rich_text, row, scrollable, span, text,
    text_input,
};
use iced::{Background, Border, Color, Font, Length};

use smudgy_cloud::cloud_api::{CloudApiClient, FriendView};
use smudgy_cloud::package_api::{
    CommentView, PackageApiClient, PackageDetail, PackageGrantView, PackageSearchResult,
    ResolvedPackageWire, SearchCategory, VersionListItem,
};
use smudgy_cloud::{CloudError, DependencyKind, Uuid};
use smudgy_core::models::local_packages::{self, LocalModule};
use smudgy_core::models::naming;
use smudgy_core::models::package_updates::PackageVersionRef;
use smudgy_core::models::profile_activation::ProfileActivation;
use smudgy_core::models::shared_packages::{
    self, Cas, LockedPackage, PackageManifest, PackageParamCommit, PackageParamMutation,
    PackageParameter, PackagePermissions, ParamKind, ParamValueScope, ParameterScope,
    SharedPackageLock, SmudgyCapabilities, UpdateMode,
};

use crate::assets::fonts;
use crate::cloud_account::{
    PackageOperationCompletion, PackageOperationId, PackageOperationPermit,
};
use crate::components::cloud_errors::display_error;
use crate::theme::builtins::button as button_style;
use crate::update::Update;

use crate::components::permissions::{
    PermissionRisk, consent_can_row, data_scoped, escape_reasons, full_access_banner, join_reasons,
    path_grant_enforced, permission_can_lines, union_risk,
};

use smudgy_core::session::runtime::package_cache::PackageCache;
use smudgy_core::session::runtime::{AutomationBody, AutomationKind};
use smudgy_core::session::styled_line::{InvisiblePolicy, deceptive_invisible};

use super::code_editor;
use super::common;
use super::editors::pane_scroll;
use super::manifest::{ManifestDraft, ManifestTab};
use super::model::{
    CreatorAutomations, DepEdge, NodeStatus, package_display_name, parse_specifier, specifier_for,
};
use super::param_values::{self, ParamTarget, ParamValueEdit, ParamValueState, ScalarEdit};
use super::{
    AutomationsWindow, DiscoverScope, Elem, Event, InstalledPackageTab, InstalledReadmeState,
    LocalPackageTab, Message, Pane, Selection,
};

/// Terminal-like output from the latest publish attempt, keyed to its package so navigating to a
/// different owned package never shows the wrong command or diagnostics.
#[derive(Debug, Clone)]
pub(super) struct PublishOutput {
    pub(super) package: String,
    pub(super) text: String,
}

/// What the local folder can prove about its cloud publication history. Rename must fail closed:
/// a transient cloud failure or a damaged durable binding must never make a published name look
/// editable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum PublicationStatus {
    /// No durable binding exists and cloud history has not been checked in this session.
    Unknown,
    /// A signed-in cloud-history check is in progress.
    Checking,
    /// Cloud history was checked and this local folder has not published a namespace.
    Unpublished,
    /// The local folder is durably (or newly, in this process) bound to this namespace.
    Bound(Uuid),
    /// The durable publication record is unreadable or contradicts cloud state.
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileChoice {
    pub(super) key: String,
    pub(super) label: String,
}

/// The copy-settings dialog: which package's per-profile values to copy, from which profile,
/// and the destination the user has picked so far.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopySettingsPrompt {
    pub specifier: String,
    pub source: String,
    pub destination: Option<String>,
}

impl std::fmt::Display for ProfileChoice {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.label)
    }
}

/// The install-time prompt for a package's required params that aren't yet set.
#[derive(Debug, Clone)]
pub struct ParamPrompt {
    /// Exact governing row captured when this prompt was prepared. Submission compares this full
    /// row under the package-state transaction and writes the values only when it still matches.
    pub expected_package: LockedPackage,
    pub name: String,
    pub version: String,
    pub params: Vec<PackageParameter>,
    /// In-progress value state per key (a checkbox bool, a dropdown selection, a list of rows…),
    /// seeded empty. See [`param_values`].
    pub values: HashMap<String, ParamValueState>,
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
    /// Exact governing row that authorized the values read into this editor. `None` means the
    /// authority was unavailable and all mutations must remain disabled.
    pub expected_package: Option<LockedPackage>,
    pub parameter_scope: ParameterScope,
    pub profile_name: String,
    /// False when the authoritative lock, value file, or secret store could not be read. The
    /// section remains visible with an error, but never presents writable controls seeded from
    /// fabricated defaults.
    pub available: bool,
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
    fn seed(
        server_name: &str,
        profile_name: &str,
        expected_package: LockedPackage,
        params: Vec<PackageParameter>,
    ) -> Result<Self, String> {
        let specifier = expected_package.specifier.clone();
        let parameter_scope = expected_package.parameter_scope;
        let scope = match parameter_scope {
            ParameterScope::Global => ParamValueScope::Global,
            ParameterScope::Profile => ParamValueScope::Profile(profile_name),
        };
        let mut values = HashMap::new();
        let mut secret_stored = HashSet::new();
        for param in &params {
            if is_secret_string(param) {
                if shared_packages::load_secret_param_scoped_checked(
                    server_name,
                    scope,
                    &specifier,
                    &param.key,
                )
                .map_err(|error| error.to_string())?
                .is_some()
                {
                    secret_stored.insert(param.key.clone());
                }
                values.insert(param.key.clone(), ParamValueState::Text(String::new()));
            } else {
                let stored = shared_packages::get_param_value_scoped_checked(
                    server_name,
                    scope,
                    &specifier,
                    &param.key,
                )
                .map_err(|error| error.to_string())?;
                values.insert(
                    param.key.clone(),
                    param_values::seed(param, stored.as_ref()),
                );
            }
        }
        Ok(Self {
            specifier,
            expected_package: Some(expected_package),
            parameter_scope,
            profile_name: profile_name.to_string(),
            available: true,
            params,
            values,
            secret_stored,
            touched: HashSet::new(),
            error: None,
            saved: false,
        })
    }

    fn unavailable(
        specifier: String,
        parameter_scope: ParameterScope,
        profile_name: &str,
        params: Vec<PackageParameter>,
        error: String,
    ) -> Self {
        Self {
            specifier,
            expected_package: None,
            parameter_scope,
            profile_name: profile_name.to_string(),
            available: false,
            params,
            values: HashMap::new(),
            secret_stored: HashSet::new(),
            touched: HashSet::new(),
            error: Some(error),
            saved: false,
        }
    }
}

/// Whether a param is a secret rendered as a write-only secure box. Secrets are stored as keyring
/// strings, so only a `String` param can be one — a (hand-authored) secret of any other kind falls
/// back to its real value control rather than a misleading secret box. The manifest editor already
/// gates the `secret` flag to `String`, so this only matters for a malformed manifest.
pub(super) fn is_secret_string(param: &PackageParameter) -> bool {
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
    fn mutation(&self, key: &str) -> PackageParamMutation {
        match self {
            Self::Secret(value) => PackageParamMutation::SetSecret {
                key: key.to_string(),
                value: value.clone(),
            },
            Self::Value(value) => PackageParamMutation::SetValue {
                key: key.to_string(),
                value: value.clone(),
            },
            Self::Clear => PackageParamMutation::ClearValue {
                key: key.to_string(),
            },
        }
    }
}

/// A secure text-input field row for a secret parameter, emitting a scalar text edit on `target`.
/// Secrets are write-only (never read back into the box), so this is rendered here rather than by
/// [`param_values::view`]. `clear` appends a Clear button when present (the config editor only).
pub(super) fn secret_field_row<'a>(
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
            button(text(crate::i18n::t!("action-clear")).size(11.0))
                .style(button_style::secondary)
                .on_press(msg),
        );
    }
    field.into()
}

/// How "Edit a copy" settled the local copy's runtime state, so
/// [`AutomationsWindow::fork_finished`] can describe the result precisely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForkActivation {
    /// The copy keeps the leaf name and therefore becomes the canonical implementation of the
    /// already-installed package. Its remote lock row remains available as the delete fallback.
    OverrideActive,
    /// As [`Self::OverrideActive`], but the installed fallback was disabled everywhere, so no
    /// running session changes when the local implementation becomes canonical.
    OverrideInactive,
    /// The copy uses a different leaf name. With no package of that name already installed, its
    /// newly materialized local row starts disabled.
    Independent,
}

/// Outcome of an async cloud check over account-owned installs — `delete_owned`'s post-delete
/// check of the deleted package's own entry, or the installed-list sweep over folder-less
/// entries. Drives [`AutomationsWindow::stale_account_installs_checked`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleInstallCheck {
    /// Nothing is published under the checked name(s): the stale entries were removed from the
    /// lockfile, so the installed list must refresh.
    Pruned(Vec<String>),
    /// The check couldn't decide (cloud unreachable) or nothing needed doing.
    Unchanged,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpdateComparisonApply {
    Current,
    ConsentChanged,
    Stale,
}

/// Generation fence for owned-package cloud state. Opening a package, refreshing after publish,
/// or starting a sharing mutation advances it; late results from an older load or mutation cannot
/// repaint the same package after a newer operation has begun.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ShareSeq(u64);

impl ShareSeq {
    pub fn bump(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

/// Exact account identity attached to authenticated read results. The window-local epoch handles
/// refreshes; this fence also closes the daemon-turn gap between a credential/account swap and the
/// queued `AccountChanged` refresh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountReadFence {
    account_epoch: u64,
    credential_generation: u64,
    user_id: Option<Uuid>,
    signed_in: bool,
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

/// Generation token for one complete installed-package graph resolve. Reloading the lockfile
/// advances it, so a response from an older package/version snapshot cannot rewrite the current
/// graph or its persisted `required_by` closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GraphSeq(u64);

impl GraphSeq {
    pub fn bump(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

/// Generation for the selected Discover package and its viewer-specific reads/mutations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiscoverSeq(u64);

impl DiscoverSeq {
    pub fn bump(&mut self) {
        self.0 = self.0.wrapping_add(1);
    }
}

/// Generation for public Discover result searches, so a slower old query cannot replace a newer
/// query or scope selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DiscoverSearchSeq(u64);

impl DiscoverSearchSeq {
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

/// Latest-registry comparison kept separate from [`InstalledDetail`], whose resolved package is
/// always the staged/running version used by About and Source.
#[derive(Debug, Clone)]
pub struct InstalledLatestComparison {
    pub version: String,
    pub permissions: PackagePermissions,
    pub floor: shared_packages::SmudgyVersionFloor,
    /// The offered manifest adds or changes independent `requires` roots. Such a version is held
    /// until the full install planner checks ranges, consent, and materialization.
    pub requirements_changed: bool,
}

/// Whether a consent prompt creates a new install or changes an existing root's version policy.
#[derive(Debug, Clone)]
pub enum ConsentOperation {
    Install,
    Update {
        mode: UpdateMode,
        activation: ProfileActivation,
    },
    LocalManifest {
        name: String,
        manifest: Box<PackageManifest>,
        json: String,
        expected_manifest: String,
        activation: ProfileActivation,
    },
}

/// Final action after all newly-required parameter prompts have been handled.
#[derive(Debug, Clone)]
pub(super) enum PackageChangeKind {
    Install,
    Update,
    Manifest,
}

#[derive(Debug, Clone)]
pub(super) struct PackageChangeFinalize {
    pub(super) specifier: String,
    pub(super) activation: ProfileActivation,
    pub(super) kind: PackageChangeKind,
    /// Replaces the success toast when the change committed but a follow-up step (such as
    /// reading the required-parameter state) failed.
    pub(super) warning: Option<String>,
}

/// The outcome of resolving a package for install: the identity plus the things the
/// install-consent flow needs — the **closure** permission union (what the sandboxed isolate
/// will be granted, `PACKAGE-ISOLATES-CONSENT-TRUST.md`), the root manifest's declared params
/// (so a Grant can chain into the required-params prompt without re-resolving), and the
/// transitively-walked `requires`-closure (the required roots co-installed with this package and a
/// peer-conflict refusal when one applies). See `script/REQUIRED-PACKAGES.md`.
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
    /// Exact published import-closure coordinates that must be prefetched before this accepted
    /// root can be committed. The root itself is implicit in `specifier` + `version`.
    pub closure: Vec<PackageVersionRef>,
    /// The `requires`-closure walked transitively from this root: each required top-level root,
    /// whether it is already installed/satisfied, its own permission closure, and its missing
    /// required params. Empty when the package requires nothing.
    pub required_roots: Vec<RequiredRoot>,
    /// A peer-conflict refusal: when set, the install is **blocked** because no single version of a
    /// required library satisfies every current requirer's range. Carries the explanation.
    pub conflict: Option<String>,
    /// A version-floor refusal: when set, the install is **blocked** because the package's
    /// dependency closure (or a required root's) declares a `min_smudgy_version` above this
    /// smudgy — the engine would refuse it at every load. Carries the
    /// [`SmudgyVersionFloor::refusal`](shared_packages::SmudgyVersionFloor::refusal) reason.
    pub needs_smudgy: Option<String>,
    /// A required root or its manifest could not be resolved. `requires` is mandatory, so this is
    /// a blocking error rather than a best-effort omission.
    pub required_unavailable: Option<String>,
    /// Exact durable package state used to build this resolution. Consent is valid only while
    /// both snapshots still match; otherwise local shadows, activation, and dependency edges may
    /// no longer be the ones the user reviewed.
    pub expected_lock: SharedPackageLock,
    pub expected_local_manifests: HashMap<String, PackageManifest>,
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
    /// Exact published import-closure coordinates to prefetch if this required root is newly
    /// installed or upgraded. The required root itself is implicit.
    pub closure: Vec<PackageVersionRef>,
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
    /// Account and credential that resolved every cloud-backed part of this prompt. A prompt is
    /// invalid as soon as the singleton receives an account change.
    pub account_fence: AccountReadFence,
    pub specifier: String,
    pub owner: String,
    pub name: String,
    pub version: String,
    /// The closure union the user grants on confirm (recorded as `consented_permissions`).
    pub permissions: PackagePermissions,
    /// The root manifest's params — carried so a Grant can chain straight into the
    /// required-params prompt without a second resolve.
    pub params: Vec<PackageParameter>,
    /// Exact published import-closure coordinates for the chosen remote root. Empty for a local
    /// manifest operation, whose live files are not package-cache content.
    pub closure: Vec<PackageVersionRef>,
    /// The transitively-walked `requires`-closure: the required top-level roots co-installed with
    /// this package (`script/REQUIRED-PACKAGES.md`). A single grant covers the whole set; on Grant
    /// each not-already-satisfied root is installed via `install_required_package` and consented.
    pub required_roots: Vec<RequiredRoot>,
    /// A peer-conflict refusal: when set, Install is disabled and this message explains why (no
    /// single version of a required library satisfies every requirer's range).
    pub conflict: Option<String>,
    /// A version-floor refusal: when set, Install is disabled and this message explains which
    /// package requires a newer smudgy than this one.
    pub needs_smudgy: Option<String>,
    /// A mandatory required package could not be resolved or read.
    pub required_unavailable: Option<String>,
    pub expected_lock: SharedPackageLock,
    pub expected_local_manifests: HashMap<String, PackageManifest>,
    pub operation: ConsentOperation,
    pub error: Option<String>,
}

/// Proof that every published module in a consented change is present in the package cache, so
/// the committed lock rows can load without touching the network.
#[derive(Debug, Clone, Copy, Default)]
pub struct PreparedConsentCache;

#[derive(Debug, Clone)]
struct ConsentCacheTarget {
    specifier: String,
    version: String,
    closure: Vec<PackageVersionRef>,
}

async fn prepare_consent_cache(
    client: PackageApiClient,
    root: Option<ConsentCacheTarget>,
    required: Vec<ConsentCacheTarget>,
) -> Result<PreparedConsentCache, String> {
    if root.is_none() && required.is_empty() {
        return Ok(PreparedConsentCache);
    }
    let cache =
        PackageCache::new().map_err(|error| format!("package cache unavailable: {error}"))?;
    for target in root.iter().chain(required.iter()) {
        prefetch_closure(&client, &cache, target).await?;
    }
    Ok(PreparedConsentCache)
}

async fn prefetch_closure(
    client: &PackageApiClient,
    cache: &PackageCache,
    target: &ConsentCacheTarget,
) -> Result<(), String> {
    let (owner, name) = parse_specifier(&target.specifier)
        .ok_or_else(|| format!("not a package specifier: {}", target.specifier))?;
    let mut nodes = Vec::with_capacity(target.closure.len() + 1);
    nodes.push(PackageVersionRef {
        owner,
        name,
        version: target.version.clone(),
    });
    nodes.extend(target.closure.iter().cloned());

    let mut fetched = HashSet::new();
    for node in nodes {
        if !fetched.insert((
            node.owner.to_ascii_lowercase(),
            node.name.to_ascii_lowercase(),
            node.version.clone(),
        )) {
            continue;
        }
        crate::package_update_checker::prefetch_version(
            client,
            cache,
            &node.owner,
            &node.name,
            &node.version,
        )
        .await?;
    }
    Ok(())
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
    /// Why the resolved version can't run on this smudgy (its closure's `min_smudgy_version`
    /// floor refusal), when the version floor — rather than permissions — holds the update
    /// back. The card then explains the floor and offers no grant (granting wouldn't help;
    /// only updating smudgy or pinning an older version would).
    pub needs_smudgy: Option<String>,
    /// The offered version changes the set or ranges of independently-running required roots.
    pub requirements_changed: bool,
}

/// The update mode a granted delta card resolves under. Granting keeps the package's current
/// policy: a pinned install re-pins to the exact version the card was built from (the staged
/// version whose closure grew), while an Auto install keeps following the latest. Granting must
/// never silently drop a pin.
pub(super) fn update_grant_mode(open: &LockedPackage, delta: &UpdateDelta) -> UpdateMode {
    match &open.mode {
        UpdateMode::Pinned { .. } => UpdateMode::Pinned {
            version: delta.version.clone(),
        },
        UpdateMode::Auto => UpdateMode::Auto,
    }
}

/// Largest module body the installed-package source browser will fetch and render as text. Real
/// source files are far below this; a blob above it is shown as a "too large to preview"
/// placeholder (size only) rather than pulled into a text widget. The server also caps a single
/// blob at 10 MiB, so this is the UI-side, not the only, bound. Enforced twice: as a pre-fetch
/// gate on the wire `byte_size` (skip the download) and again on the actual fetched length (so a
/// missing/under-reported `byte_size` can't sneak a huge body through).
pub(super) const SOURCE_PREVIEW_CAP_BYTES: u64 = 1024 * 1024;

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
    Text {
        source: String,
        bidi: bool,
        /// The executable source contained NUL bytes. They are rendered visibly as `␀` and the
        /// pane warns that the display is an escaped audit view.
        nul: bool,
    },
    /// Detected as binary (isn't valid UTF-8) — shown as a placeholder.
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
    s.chars()
        .any(|c| deceptive_invisible(c, InvisiblePolicy::ActionTarget))
}

/// Classify fetched module bytes for the source browser. The declared media type is
/// publisher-controlled, so it is *not* trusted to decide text-vs-binary here — the bytes do:
/// content above the cap is "too large" and invalid UTF-8 is "binary". Valid UTF-8 containing NUL
/// remains auditable because runtime accepts it: each NUL is rendered visibly as `␀` with a warning
/// instead of hiding potentially executable source behind a binary placeholder.
pub(super) fn classify_source(bytes: Vec<u8>) -> FilePreview {
    let size = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if size > SOURCE_PREVIEW_CAP_BYTES {
        return FilePreview::TooLarge { size };
    }
    match String::from_utf8(bytes) {
        Ok(mut source) => {
            let bidi = has_deceptive_unicode(&source);
            let nul = source.contains('\0');
            if nul {
                source = source.replace('\0', "␀");
            }
            FilePreview::Text { source, bidi, nul }
        }
        Err(_) => FilePreview::Binary { size },
    }
}

/// Human-readable byte size for source-browser placeholders ("1.4 KB", "2.0 MB"). Integer math
/// only (no float casts), so it stays clippy-pedantic clean and can't overflow for any `u64`.
pub(super) fn human_size(bytes: u64) -> String {
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

/// Reads every local package manifest as one complete shadow-resolution snapshot. A listed folder
/// with an unreadable or missing manifest is uncertainty, not permission to resolve its remote
/// namesake, so callers must stop rather than silently omit it.
fn load_local_manifest_snapshot(
    server_name: &str,
) -> Result<HashMap<String, PackageManifest>, String> {
    let names =
        local_packages::list_local_packages(server_name).map_err(|error| error.to_string())?;
    load_local_manifest_snapshot_for_names(server_name, &names)
}

fn load_local_manifest_snapshot_for_names(
    server_name: &str,
    names: &[String],
) -> Result<HashMap<String, PackageManifest>, String> {
    let mut manifests = HashMap::with_capacity(names.len());
    for name in names {
        let package = local_packages::load_local_package(server_name, name)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| crate::i18n::t!("package-local-not-found", "name" => name))?;
        let specifier = specifier_for(local_packages::LOCAL_OWNER, &package.name);
        if manifests.insert(specifier, package.manifest).is_some() {
            return Err(crate::i18n::t!(
                "package-local-state-unavailable",
                "error" => crate::i18n::t!("package-copy-name-exists", "name" => name)
            ));
        }
    }
    Ok(manifests)
}

/// Resolves a package and folds the **whole dependency-closure** permission union, mirroring the
/// engine's `SmudgyPackageProvider::solve_closure`: every distinct `(owner, name, version)` in the
/// closure contributes its `manifest.permissions` (`PACKAGE-ISOLATES-ENFORCEMENT.md`). The
/// sandboxed isolate is granted exactly this union (recorded as `consented_permissions`), so
/// the consent window must show — and consent must record — the closure union, not just the root
/// manifest. Every dependency must resolve: skipping one could hide its transitive code or
/// permission requests from both the consent prompt and the cache-authority ledger. Dedups by
/// `(owner, name, version)` so diamonds and cycles terminate.
async fn resolve_install_closure(
    client: &PackageApiClient,
    owner: &str,
    name: &str,
    pinned: Option<&str>,
    installed: &[LockedPackage],
    local_manifests: &HashMap<String, PackageManifest>,
) -> Result<InstallResolution, CloudError> {
    let expected_lock = SharedPackageLock {
        packages: installed.to_vec(),
    };
    let expected_local_manifests = local_manifests.clone();
    let root = client.resolve_package(owner, name, pinned).await?;
    let ResolvedImportClosure {
        permissions,
        floor,
        closure: import_closure,
    } = closure_permission_union(client, &root).await?;
    let params = resolved_manifest_checked(&root)?.params;
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
            closure: import_closure,
            required_roots: Vec::new(),
            conflict: None,
            needs_smudgy: Some(reason),
            required_unavailable: None,
            expected_lock,
            expected_local_manifests,
        });
    }
    // Walk this root's `requires`-closure transitively — the required top-level roots co-installed
    // alongside it and any peer-conflict or version-floor refusal.
    let required_closure =
        resolve_required_closure(client, &root, installed, local_manifests).await;
    Ok(InstallResolution {
        specifier,
        owner: root.owner_nickname,
        name: root.name,
        version: root.version,
        permissions,
        params,
        closure: import_closure,
        required_roots: required_closure.roots,
        conflict: required_closure.conflict,
        needs_smudgy: required_closure.needs_smudgy,
        required_unavailable: required_closure.unavailable,
        expected_lock,
        expected_local_manifests,
    })
}

/// The accumulated result of the `requires`-closure walk.
struct RequiredClosure {
    roots: Vec<RequiredRoot>,
    conflict: Option<String>,
    /// A required root's closure declares a `min_smudgy_version` above this smudgy — the
    /// whole install is refused (the grant is all-or-nothing and the root couldn't load).
    needs_smudgy: Option<String>,
    /// A mandatory root could not be resolved/read, so installation must stop.
    unavailable: Option<String>,
}

/// Why [`plan_required_root`] refused the whole install — each variant lands in its own
/// [`RequiredClosure`] field so the consent card shows the matching banner.
enum RequiredRefusal {
    /// No single version of the required library satisfies every requirer's range.
    Conflict(String),
    /// The required root's closure is floored above this smudgy.
    NeedsSmudgy(String),
    /// Registry/cache access or manifest parsing failed for a mandatory required root.
    Unavailable(String),
}

/// Walks `root`'s `requires` **transitively** (`script/REQUIRED-PACKAGES.md`): for each
/// `smudgy://owner/name[@range]` in a manifest's `requires`, resolve it, read ITS `requires`, and
/// recurse — de-duping required roots by package key so back-edges terminate without aborting. For
/// each required root it gathers every requirer's declared range (including the new root's and the
/// still-installed packages' manifests) and applies the peer-conflict policy: a satisfied existing
/// install is reused as-is; an unsatisfied one is upgraded to a single version meeting every range
/// when one exists; if no version satisfies all, the whole install is **refused** (`conflict` set,
/// `script/REQUIRED-PACKAGES.md`).
///
/// Every required root is mandatory. Resolution or manifest failures stop the install and are
/// surfaced through [`RequiredClosure::unavailable`].
async fn resolve_required_closure(
    client: &PackageApiClient,
    root: &ResolvedPackageWire,
    installed: &[LockedPackage],
    local_manifests: &HashMap<String, PackageManifest>,
) -> RequiredClosure {
    let root_edges = match manifest_requires_checked(root) {
        Ok(edges) => canonical_required_edges(edges, local_manifests),
        Err(message) => {
            return RequiredClosure {
                roots: Vec::new(),
                conflict: None,
                needs_smudgy: None,
                unavailable: Some(message),
            };
        }
    };
    resolve_required_closure_from_edges(
        client,
        &root.owner_nickname,
        &root.name,
        &root.version,
        root_edges,
        installed,
        local_manifests,
    )
    .await
}

/// Fixed-point `requires` planner shared by remote installation/version changes and local manifest
/// saves. The root itself can be remote or the reserved local state identity; only its identity,
/// version, and already-parsed outgoing edges participate in peer-range planning.
async fn resolve_required_closure_from_edges(
    client: &PackageApiClient,
    root_owner: &str,
    root_name: &str,
    root_version: &str,
    root_edges: Vec<RequiresEdge>,
    installed: &[LockedPackage],
    local_manifests: &HashMap<String, PackageManifest>,
) -> RequiredClosure {
    let root_key = canonical_required_key(root_owner, root_name, local_manifests);
    let mut closure = RequiredClosure {
        roots: Vec::new(),
        conflict: None,
        needs_smudgy: None,
        unavailable: None,
    };

    // Constraints from packages that already exist are fixed for this planning run. A local folder
    // owns its leaf, so its dormant remote fallback contributes neither a manifest nor a second
    // identity. This is also where impossible multiple-remote-author leaf state fails closed.
    let local_leaves = local_manifests
        .keys()
        .map(|specifier| package_display_name(specifier).to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut fixed_ranges: BTreeMap<RequiredKey, Vec<RequirerRange>> = BTreeMap::new();
    let mut remote_owner_by_leaf: BTreeMap<String, String> = BTreeMap::new();
    if !local_leaves.contains(&root_name.to_ascii_lowercase()) {
        remote_owner_by_leaf.insert(
            root_name.to_ascii_lowercase(),
            root_owner.to_ascii_lowercase(),
        );
    }
    let mut local_manifests_seen = HashSet::new();
    for package in installed {
        let Some((owner, name)) = parse_specifier(&package.specifier) else {
            continue;
        };
        let leaf = name.to_ascii_lowercase();
        let is_local_state =
            owner.eq_ignore_ascii_case(local_packages::LOCAL_OWNER) && local_leaves.contains(&leaf);
        if local_leaves.contains(&leaf) && !is_local_state {
            continue;
        }
        if !is_local_state {
            if let Some(existing) = remote_owner_by_leaf.insert(leaf.clone(), owner.clone())
                && !existing.eq_ignore_ascii_case(&owner)
            {
                closure.unavailable = Some(crate::i18n::t!(
                    "package-remote-leaf-conflict",
                    "name" => &name
                ));
                return closure;
            }
        }
        let manifest = if let Some(manifest) = local_manifests.get(&package.specifier) {
            local_manifests_seen.insert(package.specifier.clone());
            manifest.clone()
        } else {
            let staged = package.staged_version().map(str::to_string);
            let wire = match client
                .resolve_package(&owner, &name, staged.as_deref())
                .await
            {
                Ok(wire) => wire,
                Err(error) => {
                    closure.unavailable = Some(crate::i18n::t!(
                        "package-requirer-unavailable",
                        "name" => package_display_name(&package.specifier),
                        "error" => error.to_string()
                    ));
                    return closure;
                }
            };
            match serde_json::from_value::<PackageManifest>(wire.manifest) {
                Ok(manifest) => manifest,
                Err(error) => {
                    closure.unavailable = Some(crate::i18n::t!(
                        "package-required-manifest-invalid",
                        "name" => package_display_name(&package.specifier),
                        "error" => error.to_string()
                    ));
                    return closure;
                }
            }
        };
        add_required_ranges(
            &mut fixed_ranges,
            package_display_name(&package.specifier),
            canonical_required_edges(manifest_requires_from_manifest(&manifest), local_manifests),
        );
    }
    // A newly-created local folder may not have reached lock materialization yet. It still governs
    // the leaf and its own `requires` constraints must participate.
    for (specifier, manifest) in local_manifests {
        if local_manifests_seen.contains(specifier) {
            continue;
        }
        add_required_ranges(
            &mut fixed_ranges,
            package_display_name(specifier),
            canonical_required_edges(manifest_requires_from_manifest(manifest), local_manifests),
        );
    }

    let mut planned: BTreeMap<RequiredKey, PlannedRequired> = BTreeMap::new();
    const MAX_FIXED_POINT_PASSES: usize = 64;
    for _ in 0..MAX_FIXED_POINT_PASSES {
        let reachable = reachable_required_keys(&root_edges, &planned);
        let mut ranges = fixed_ranges.clone();
        add_required_ranges(&mut ranges, root_name, root_edges.clone());
        for (key, plan) in &planned {
            if reachable.contains(key) {
                add_required_ranges(&mut ranges, &plan.root.name, plan.edges.clone());
            }
        }

        for (owner, name) in ranges.keys() {
            if owner.eq_ignore_ascii_case(local_packages::LOCAL_OWNER) {
                continue;
            }
            if let Some(existing) = remote_owner_by_leaf.insert(name.clone(), owner.clone())
                && !existing.eq_ignore_ascii_case(owner)
            {
                closure.unavailable = Some(crate::i18n::t!(
                    "package-remote-leaf-conflict",
                    "name" => name
                ));
                return closure;
            }
        }

        // A cycle/back-edge can constrain the selected root itself. It cannot be silently skipped
        // merely because the root was the initial seen node.
        if let Some(root_ranges) = ranges.get(&root_key) {
            let root_version = match semver::Version::parse(root_version) {
                Ok(version) => version,
                Err(error) => {
                    closure.unavailable = Some(crate::i18n::t!(
                        "package-required-root-version-invalid",
                        "name" => root_name,
                        "error" => error.to_string()
                    ));
                    return closure;
                }
            };
            if !root_ranges
                .iter()
                .all(|range| range_admits(range.range.as_deref(), &root_version))
            {
                closure.conflict = Some(conflict_message(root_name, root_ranges));
                return closure;
            }
        }

        let mut changed = false;
        for key in &reachable {
            if key == &root_key {
                continue;
            }
            let key_ranges = ranges.get(key).cloned().unwrap_or_default();
            let next = if let Some(manifest) = local_manifest_for_key(key, local_manifests) {
                match planned_local_required(key, manifest, local_manifests) {
                    Ok(plan) => plan,
                    Err(refusal) => {
                        apply_required_refusal(&mut closure, refusal);
                        return closure;
                    }
                }
            } else {
                match plan_required_root(client, &key.0, &key.1, &key_ranges, installed).await {
                    Ok((wire, root)) => {
                        let edges = match manifest_requires_checked(&wire) {
                            Ok(edges) => canonical_required_edges(edges, local_manifests),
                            Err(message) => {
                                closure.unavailable = Some(message);
                                return closure;
                            }
                        };
                        PlannedRequired { root, edges }
                    }
                    Err(refusal) => {
                        apply_required_refusal(&mut closure, refusal);
                        return closure;
                    }
                }
            };
            let differs = planned.get(key).is_none_or(|current| {
                current.root.version != next.root.version || current.edges != next.edges
            });
            if differs {
                planned.insert(key.clone(), next);
                changed = true;
            }
        }
        let before = planned.len();
        planned.retain(|key, _| reachable.contains(key));
        changed |= planned.len() != before;
        if !changed {
            closure.roots = reachable
                .into_iter()
                .filter(|key| key != &root_key)
                .filter_map(|key| planned.remove(&key).map(|plan| plan.root))
                .collect();
            return closure;
        }
    }

    closure.unavailable = Some(crate::i18n::t!("package-required-graph-unstable"));
    closure
}

/// One `requires` edge flattened to plain strings — the required library's owner/name and its
/// declared range — so the closure walk never names a `smudgy_script` type in `ui`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RequiresEdge {
    owner: String,
    name: String,
    range: Option<String>,
}

/// One requirer's declared range for a required library — the requirer's display name (for the
/// refusal message) and the raw range (`None`/empty = bare, satisfied by any version).
#[derive(Debug, Clone, PartialEq, Eq)]
struct RequirerRange {
    requirer: String,
    range: Option<String>,
}

#[derive(Debug, Clone)]
struct PlannedRequired {
    root: RequiredRoot,
    edges: Vec<RequiresEdge>,
}

type RequiredKey = (String, String);

fn normalized_required_key(owner: &str, name: &str) -> RequiredKey {
    (owner.to_ascii_lowercase(), name.to_ascii_lowercase())
}

fn local_required_key(
    name: &str,
    local_manifests: &HashMap<String, PackageManifest>,
) -> Option<RequiredKey> {
    local_manifests.keys().find_map(|specifier| {
        let (owner, local_name) = parse_specifier(specifier)?;
        naming::names_conflict(&local_name, name)
            .then(|| normalized_required_key(&owner, &local_name))
    })
}

fn canonical_required_key(
    owner: &str,
    name: &str,
    local_manifests: &HashMap<String, PackageManifest>,
) -> RequiredKey {
    local_required_key(name, local_manifests)
        .unwrap_or_else(|| normalized_required_key(owner, name))
}

fn canonical_required_edges(
    edges: Vec<RequiresEdge>,
    local_manifests: &HashMap<String, PackageManifest>,
) -> Vec<RequiresEdge> {
    let mut edges = edges
        .into_iter()
        .map(|mut edge| {
            let (owner, name) = canonical_required_key(&edge.owner, &edge.name, local_manifests);
            edge.owner = owner;
            edge.name = name;
            edge
        })
        .collect::<Vec<_>>();
    edges.sort_by(|left, right| {
        (&left.owner, &left.name, &left.range).cmp(&(&right.owner, &right.name, &right.range))
    });
    edges
}

fn manifest_requires_from_manifest(manifest: &PackageManifest) -> Vec<RequiresEdge> {
    manifest
        .smudgy_requires()
        .into_iter()
        .map(|dependency| RequiresEdge {
            owner: dependency.key.owner,
            name: dependency.key.name,
            range: dependency.range,
        })
        .collect()
}

fn local_manifest_for_key<'a>(
    key: &RequiredKey,
    local_manifests: &'a HashMap<String, PackageManifest>,
) -> Option<&'a PackageManifest> {
    local_manifests.iter().find_map(|(specifier, manifest)| {
        let (owner, name) = parse_specifier(specifier)?;
        (normalized_required_key(&owner, &name) == *key).then_some(manifest)
    })
}

fn add_required_ranges(
    ranges: &mut BTreeMap<RequiredKey, Vec<RequirerRange>>,
    requirer: &str,
    edges: Vec<RequiresEdge>,
) {
    for edge in edges {
        let entry = ranges
            .entry(normalized_required_key(&edge.owner, &edge.name))
            .or_default();
        let range = RequirerRange {
            requirer: requirer.to_string(),
            range: edge.range,
        };
        if !entry.contains(&range) {
            entry.push(range);
        }
    }
}

fn reachable_required_keys(
    root_edges: &[RequiresEdge],
    planned: &BTreeMap<RequiredKey, PlannedRequired>,
) -> BTreeSet<RequiredKey> {
    let mut frontier = root_edges
        .iter()
        .map(|edge| normalized_required_key(&edge.owner, &edge.name))
        .collect::<Vec<_>>();
    let mut reachable = BTreeSet::new();
    while let Some(key) = frontier.pop() {
        if !reachable.insert(key.clone()) {
            continue;
        }
        if let Some(plan) = planned.get(&key) {
            frontier.extend(
                plan.edges
                    .iter()
                    .map(|edge| normalized_required_key(&edge.owner, &edge.name)),
            );
        }
    }
    reachable
}

fn planned_local_required(
    key: &RequiredKey,
    manifest: &PackageManifest,
    local_manifests: &HashMap<String, PackageManifest>,
) -> Result<PlannedRequired, RequiredRefusal> {
    let mut floor = shared_packages::SmudgyVersionFloor::default();
    floor.fold(&key.1, manifest.min_smudgy_version.as_deref());
    if let Some(reason) = floor.refusal(&shared_packages::running_smudgy_release()) {
        return Err(RequiredRefusal::NeedsSmudgy(reason));
    }
    Ok(PlannedRequired {
        root: RequiredRoot {
            specifier: specifier_for(&key.0, &key.1),
            name: key.1.clone(),
            version: manifest.version.clone(),
            permissions: manifest.permissions.clone(),
            params: manifest.params.clone(),
            closure: Vec::new(),
            already_satisfied: true,
            is_upgrade: false,
        },
        edges: canonical_required_edges(manifest_requires_from_manifest(manifest), local_manifests),
    })
}

fn apply_required_refusal(closure: &mut RequiredClosure, refusal: RequiredRefusal) {
    match refusal {
        RequiredRefusal::Conflict(message) => closure.conflict = Some(message),
        RequiredRefusal::NeedsSmudgy(message) => closure.needs_smudgy = Some(message),
        RequiredRefusal::Unavailable(message) => closure.unavailable = Some(message),
    }
}

/// Canonical comparison key for the independent roots declared by one resolved version. Resolved
/// versions are intentionally excluded: the declaration (target + accepted range) is the contract;
/// a registry re-resolution within the same range does not itself change what the package requires.
fn resolved_requires_signature(wire: &ResolvedPackageWire) -> Vec<(String, String, String)> {
    let mut signature = wire
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind == DependencyKind::Requires)
        .map(|dependency| {
            let range = dependency.range.trim();
            let range = if range.is_empty() {
                String::new()
            } else {
                semver::VersionReq::parse(range)
                    .map_or_else(|_| format!("invalid:{range}"), |range| range.to_string())
            };
            (
                dependency.owner_nickname.to_ascii_lowercase(),
                dependency.name.to_ascii_lowercase(),
                range,
            )
        })
        .collect::<Vec<_>>();
    signature.sort_unstable();
    signature.dedup();
    signature
}

/// Strict variant used by installation. A malformed manifest cannot be treated as "requires
/// nothing" because that would let a package install without mandatory runtime roots.
fn manifest_requires_checked(wire: &ResolvedPackageWire) -> Result<Vec<RequiresEdge>, String> {
    serde_json::from_value::<PackageManifest>(wire.manifest.clone())
        .map(|manifest| {
            manifest
                .smudgy_requires()
                .into_iter()
                .map(|dependency| RequiresEdge {
                    owner: dependency.key.owner,
                    name: dependency.key.name,
                    range: dependency.range,
                })
                .collect()
        })
        .map_err(|error| {
            crate::i18n::t!(
                "package-required-manifest-invalid",
                "name" => &wire.name,
                "error" => error.to_string()
            )
        })
}

fn resolved_manifest_checked(wire: &ResolvedPackageWire) -> Result<PackageManifest, CloudError> {
    serde_json::from_value(wire.manifest.clone()).map_err(|error| {
        CloudError::SerializationError(crate::i18n::t!(
            "package-required-manifest-invalid",
            "name" => &wire.name,
            "error" => error.to_string()
        ))
    })
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
/// A resolution/network/manifest failure is an [`RequiredRefusal::Unavailable`]: `requires` is a
/// mandatory runtime relationship and can never be silently omitted. Range matching uses
/// `semver::VersionReq`, the same mechanism as the resolution engine (`package_solver.rs`).
async fn plan_required_root(
    client: &PackageApiClient,
    owner: &str,
    name: &str,
    ranges: &[RequirerRange],
    installed: &[LockedPackage],
) -> Result<(ResolvedPackageWire, RequiredRoot), RequiredRefusal> {
    let specifier = specifier_for(owner, name);
    let entry = installed.iter().find(|p| p.specifier == specifier);
    // The installed version, if any: an explicit pin wins over the previous resolution. An Auto
    // entry reuses its staged immutable version; only a never-resolved entry needs discovery.
    let existing = if let Some(p) = entry {
        if let Some(version) = p.staged_version() {
            Some(version.to_string())
        } else {
            Some(
                client
                    .resolve_package(owner, name, None)
                    .await
                    .map_err(|error| required_unavailable(name, &error))?
                    .version,
            )
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
        let wire = client
            .resolve_package(owner, name, Some(version))
            .await
            .map_err(|error| required_unavailable(name, &error))?;
        let root = required_root_from(&wire, client, true, false).await?;
        return Ok((wire, root));
    }

    // Otherwise seek a single published version satisfying every requirer's range.
    let latest = client
        .resolve_package(owner, name, None)
        .await
        .map_err(|error| required_unavailable(name, &error))?;
    let versions = client
        .list_versions(latest.package_id)
        .await
        .map_err(|error| required_unavailable(name, &error))?;
    let Some(target) = highest_version_satisfying_all(&versions, ranges) else {
        return Err(RequiredRefusal::Conflict(conflict_message(name, ranges)));
    };
    let wire = client
        .resolve_package(owner, name, Some(&target))
        .await
        .map_err(|error| required_unavailable(name, &error))?;
    // Installed (the lockfile has it) but its version doesn't satisfy every range → an upgrade, even
    // if `existing` couldn't be resolved to a concrete version above.
    let is_upgrade = entry.is_some();
    let root = required_root_from(&wire, client, false, is_upgrade).await?;
    Ok((wire, root))
}

fn required_unavailable(name: &str, error: &CloudError) -> RequiredRefusal {
    RequiredRefusal::Unavailable(crate::i18n::t!(
        "package-required-unavailable",
        "name" => name,
        "error" => error.to_string()
    ))
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
    let ResolvedImportClosure {
        permissions,
        floor,
        closure,
    } = closure_permission_union(client, wire)
        .await
        .map_err(|error| required_unavailable(&wire.name, &error))?;
    if let Some(reason) = floor.refusal(&shared_packages::running_smudgy_release()) {
        return Err(RequiredRefusal::NeedsSmudgy(reason));
    }
    let params = resolved_manifest_checked(wire)
        .map_err(|error| required_unavailable(&wire.name, &error))?
        .params;
    Ok(RequiredRoot {
        specifier: specifier_for(&wire.owner_nickname, &wire.name),
        name: wire.name.clone(),
        version: wire.version.clone(),
        permissions,
        params,
        closure,
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

/// The peer-conflict refusal message: `autoloot needs arctic-prompt ^2 but mapper needs ^1`. Names
/// the two requirers whose ranges can't both be met (the first pair of distinct constrained ranges).
fn conflict_message(name: &str, ranges: &[RequirerRange]) -> String {
    let constrained: Vec<&RequirerRange> = ranges
        .iter()
        .filter(|r| r.range.as_deref().is_some_and(|s| !s.trim().is_empty()))
        .collect();
    if let [first, .., last] = constrained.as_slice() {
        crate::i18n::t!(
            "package-conflict-two-requirers",
            "first" => &first.requirer,
            "name" => name,
            "first_range" => first.range.as_deref().unwrap_or("*"),
            "last" => &last.requirer,
            "last_range" => last.range.as_deref().unwrap_or("*"),
        )
    } else if let Some(only) = constrained.first() {
        crate::i18n::t!(
            "package-conflict-one-requirer",
            "name" => name,
            "range" => only.range.as_deref().unwrap_or("*"),
            "requirer" => &only.requirer,
        )
    } else {
        crate::i18n::t!("package-conflict-all-requirers", "name" => name)
    }
}

/// Folds the whole dependency-closure permission union and `min_smudgy_version` floor starting
/// from an already-resolved `root`, mirroring the engine's `solve_closure` /
/// `closure_union_for`: every distinct `(owner, name, version)` contributes its
/// `manifest.permissions` and its declared floor. A missing or malformed node fails the walk so an
/// install can never grant or cache only a prefix of the executable closure. Dedups by `(owner,
/// name, version)` so diamonds and cycles terminate. Each dep is resolved at its locked
/// `resolved_version`.
struct ResolvedImportClosure {
    permissions: PackagePermissions,
    floor: shared_packages::SmudgyVersionFloor,
    closure: Vec<PackageVersionRef>,
}

async fn closure_permission_union(
    client: &PackageApiClient,
    root: &ResolvedPackageWire,
) -> Result<ResolvedImportClosure, CloudError> {
    let mut union = PackagePermissions::default();
    let mut floor = shared_packages::SmudgyVersionFloor::default();
    let mut seen: HashSet<(String, String, String)> = HashSet::new();
    let mut closure = Vec::new();
    // (owner, name, resolved_version) of closure nodes still to fold; the root is folded inline.
    let mut stack: Vec<(String, String, String)> = Vec::new();

    let fold = |wire: &ResolvedPackageWire,
                union: &mut PackagePermissions,
                floor: &mut shared_packages::SmudgyVersionFloor|
     -> Result<(), CloudError> {
        let manifest = resolved_manifest_checked(wire)?;
        union.merge(&manifest.permissions);
        floor.fold(&wire.name, manifest.min_smudgy_version.as_deref());
        Ok(())
    };

    seen.insert((
        root.owner_nickname.clone(),
        root.name.clone(),
        root.version.clone(),
    ));
    fold(root, &mut union, &mut floor)?;
    for dep in root
        .dependencies
        .iter()
        .filter(|dependency| dependency.kind == DependencyKind::Dependency)
    {
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
        closure.push(PackageVersionRef {
            owner: dep_owner.clone(),
            name: dep_name.clone(),
            version: dep_version.clone(),
        });
        let wire = client
            .resolve_package(&dep_owner, &dep_name, Some(&dep_version))
            .await?;
        fold(&wire, &mut union, &mut floor)?;
        for dep in wire
            .dependencies
            .iter()
            .filter(|dependency| dependency.kind == DependencyKind::Dependency)
        {
            stack.push((
                dep.owner_nickname.clone(),
                dep.name.clone(),
                dep.resolved_version.clone(),
            ));
        }
    }
    Ok(ResolvedImportClosure {
        permissions: union,
        floor,
        closure,
    })
}

/// The combined `send` / `send-direct` "cannot do" line — shown only when NEITHER is granted (if
/// either is, [`send_can_line`] already conveys the scope).
fn send_cannot_line(caps: &SmudgyCapabilities) -> Option<&'static str> {
    match (caps.send, caps.send_direct) {
        (false, false) => Some(crate::i18n::ts!("permission-cannot-send")),
        _ => None,
    }
}

/// The "cannot do" lines for the un-granted smudgy capabilities — what a sandboxed package
/// can NOT do, reinforcing the sandbox guarantee; a zero-capability package surfaces all of them.
fn smudgy_cannot_lines(caps: &SmudgyCapabilities) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if !caps.create_aliases {
        out.push(crate::i18n::t!("permission-cannot-aliases"));
    }
    if !caps.create_triggers {
        out.push(crate::i18n::t!("permission-cannot-triggers"));
    }
    if let Some(line) = send_cannot_line(caps) {
        out.push(line.to_string());
    }
    if !caps.echo {
        out.push(crate::i18n::t!("permission-cannot-echo"));
    }
    if !caps.reach_others {
        out.push(crate::i18n::t!("permission-cannot-sessions"));
    }
    if !caps.change_display {
        out.push(crate::i18n::t!("permission-cannot-display"));
    }
    if !caps.mapper_read {
        out.push(crate::i18n::t!("permission-cannot-map-read"));
    }
    if !caps.mapper_write {
        out.push(crate::i18n::t!("permission-cannot-map-write"));
    }
    if !caps.widgets {
        out.push(crate::i18n::t!("permission-cannot-widgets"));
    }
    if !caps.panes {
        out.push(crate::i18n::t!("permission-cannot-panes"));
    }
    if !caps.interop_write {
        out.push(crate::i18n::t!("permission-cannot-interop-write"));
    }
    if !caps.interop_read {
        out.push(crate::i18n::t!("permission-cannot-interop-read"));
    }
    if !caps.interop_broadcast {
        out.push(crate::i18n::t!("permission-cannot-interop-broadcast"));
    }
    if !caps.workers {
        out.push(crate::i18n::t!("permission-cannot-workers"));
    }
    if !caps.gmcp_send {
        out.push(crate::i18n::t!("permission-cannot-gmcp"));
    }
    if !caps.input {
        out.push("see or change what you type in the command input".to_string());
    }
    out
}

/// One-line summary of a fully-sandboxed package with no granted access — the calm "nothing to
/// worry about" register, shown wherever the consented union is empty.
pub(super) fn sandbox_summary() -> &'static str {
    crate::i18n::ts!("permission-sandbox-summary")
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
            lines.push(crate::i18n::t!("permission-cannot-network"));
        } else {
            lines.push(crate::i18n::t!("permission-cannot-network-except-import"));
        }
    }
    // `import` and `net` are independent: granting `net` never grants the ability to pull in new
    // code, so this assurance holds even for a net-enabled package.
    if perms.import.is_none() {
        lines.push(crate::i18n::t!("permission-cannot-import"));
    }
    // "cannot read/write" when no grant SURVIVES enforcement — so a package whose only path grant is
    // a dropped `$DATA/..` reads as "cannot", consistent with the (filtered) can-list above.
    if !perms.read.iter().any(|p| path_grant_enforced(p)) {
        lines.push(crate::i18n::t!("permission-cannot-read-files"));
    }
    if !perms.write.iter().any(|p| path_grant_enforced(p)) {
        lines.push(crate::i18n::t!("permission-cannot-write-files"));
    }
    if perms.env.is_empty() {
        lines.push(crate::i18n::t!("permission-cannot-read-env"));
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

/// Whether the owned package's manifest version can be published. Drives the Publish
/// button's enabled state and the explanation banner (the semver-fluent UX).
#[derive(Debug, Clone)]
pub(super) enum PublishVerdict {
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
pub(super) fn publish_verdict(version: &str, published: &[VersionListItem]) -> PublishVerdict {
    let Ok(parsed) = semver::Version::parse(version) else {
        return PublishVerdict::Invalid(crate::i18n::t!(
            "package-version-invalid-semver",
            "version" => version
        ));
    };
    if !parsed.build.is_empty() {
        return PublishVerdict::Invalid(crate::i18n::t!("package-version-build-metadata"));
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
    pub(super) fn account_read_fence(&self) -> AccountReadFence {
        let snapshot = self.cloud.snapshot.get();
        AccountReadFence {
            account_epoch: self.account_epoch,
            credential_generation: self.cloud.credentials.generation(),
            user_id: snapshot.profile.as_ref().map(|profile| profile.id),
            signed_in: snapshot.signed_in,
        }
    }

    /// Captures one credential generation and returns a source that cannot switch principals
    /// between awaits. The accompanying fence also includes the account snapshot epoch.
    fn frozen_cloud_credentials(&self) -> (AccountReadFence, smudgy_cloud::CredentialSource) {
        let (credential_generation, credentials) = self.cloud.credentials.freeze();
        let snapshot = self.cloud.snapshot.get();
        let fence = AccountReadFence {
            account_epoch: self.account_epoch,
            credential_generation,
            user_id: snapshot.profile.as_ref().map(|profile| profile.id),
            signed_in: snapshot.signed_in,
        };
        (fence, credentials)
    }

    fn frozen_package_client(&self) -> (AccountReadFence, PackageApiClient) {
        let (fence, credentials) = self.frozen_cloud_credentials();
        (
            fence,
            PackageApiClient::new(self.cloud.base_url.as_str(), credentials),
        )
    }

    pub(super) fn account_read_is_current(&self, fence: AccountReadFence) -> bool {
        self.account_read_fence() == fence
    }

    pub(super) fn set_open_package_activation(
        &mut self,
        activation: ProfileActivation,
    ) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        if !self.profile_inventory_complete
            && matches!(&activation, ProfileActivation::Selected { .. })
        {
            self.manage_feedback = Some(crate::i18n::t!("activation-profile-inventory-error"));
            return Update::none();
        }
        let requested_specifier = match &self.pane {
            Pane::InstalledPackage => self
                .installed_open
                .as_deref()
                .map(|package| package.specifier.clone()),
            Pane::OwnedPackage => self
                .local_package
                .as_ref()
                .map(|package| self.local_own_spec(&package.name)),
            _ => None,
        };
        let Some(requested_specifier) = requested_specifier else {
            return Update::none();
        };
        let installed_pane = matches!(self.pane, Pane::InstalledPackage);
        let specifier = self.governing_specifier(&requested_specifier);

        let expected_package = self
            .installed_packages
            .iter()
            .find(|package| package.specifier == specifier)
            .cloned();
        let inserting_governing_row =
            matches!(self.pane, Pane::OwnedPackage) && expected_package.is_none();
        // A local same-leaf package is canonical. Its fallback remote row is deliberately left
        // unchanged so deleting the local folder restores the user's previous remote activation.
        // If reconciliation has not materialized the local governing row yet, create that row and
        // its requested activation in one write.
        let outcome = if inserting_governing_row {
            shared_packages::install_package_with_activation_if_unchanged(
                &self.server_name,
                &specifier,
                UpdateMode::Auto,
                activation,
            )
        } else {
            let Some(expected_package) = expected_package.as_ref() else {
                self.manage_feedback = Some(crate::i18n::t!("package-settings-state-changed"));
                return Update::with_task(Task::batch([
                    Task::done(Message::LoadLocalPackages),
                    Task::done(Message::LoadInstalledPackages),
                ]));
            };
            shared_packages::set_governing_activation_if_unchanged(
                &self.server_name,
                &requested_specifier,
                expected_package,
                activation,
            )
        };
        match outcome {
            Ok(Cas::Applied) => {}
            Ok(Cas::StateChanged) => {
                self.refresh_local_shadow_after_authoritative_mutation();
                if let Err(message) = self.reload_package_lock_snapshot() {
                    if matches!(self.pane, Pane::OwnedPackage) {
                        self.authoring_feedback = Some(message);
                    } else {
                        self.manage_feedback = Some(message);
                    }
                } else {
                    let message = crate::i18n::t!("package-settings-state-changed");
                    if matches!(self.pane, Pane::OwnedPackage) {
                        self.authoring_feedback = Some(message);
                    } else {
                        self.manage_feedback = Some(message);
                    }
                }
                return Update::with_task(Task::batch([
                    Task::done(Message::LoadLocalPackages),
                    Task::done(Message::LoadInstalledPackages),
                ]));
            }
            Err(error) => {
                if installed_pane {
                    self.refresh_local_shadow_after_authoritative_mutation();
                }
                let message = error.to_string();
                if matches!(self.pane, Pane::OwnedPackage) {
                    self.authoring_feedback = Some(message);
                } else {
                    self.manage_feedback = Some(message);
                }
                if inserting_governing_row {
                    return Update::new(
                        Task::done(Message::LoadInstalledPackages),
                        Some(Event::ScriptsChanged {
                            server_name: self.server_name.clone(),
                        }),
                    );
                }
                return Update::none();
            }
        }
        if installed_pane {
            self.refresh_local_shadow_after_authoritative_mutation();
        }

        if let Err(message) = self.reload_package_lock_snapshot() {
            if matches!(self.pane, Pane::OwnedPackage) {
                self.authoring_feedback = Some(message);
            } else {
                self.manage_feedback = Some(message);
            }
        }
        Update::with_event(Event::ScriptsChanged {
            server_name: self.server_name.clone(),
        })
    }

    pub(super) fn set_open_parameter_scope(
        &mut self,
        target: ParameterScope,
    ) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            return self.fail_config(error);
        }
        if !self.profile_inventory_complete {
            return self.fail_config(crate::i18n::t!("activation-profile-inventory-error"));
        }
        let Some(config) = self.param_config.as_ref() else {
            return Update::none();
        };
        if !config.available {
            return Update::none();
        }
        if config.parameter_scope == target {
            if target == ParameterScope::Profile {
                self.confirm_global_parameter_source = false;
            }
            return Update::none();
        }
        if target == ParameterScope::Global {
            match self.profile_param_values_are_equal(&config.specifier, &config.params) {
                Ok(true) => {}
                Ok(false) => {
                    self.confirm_global_parameter_source = true;
                    return Update::none();
                }
                Err(error) => {
                    if let Some(config) = self.param_config.as_mut() {
                        config.available = false;
                        config.error = Some(error);
                        config.saved = false;
                    }
                    return Update::none();
                }
            }
        }
        self.confirm_global_parameter_source = false;
        self.commit_open_parameter_scope(target)
    }

    pub(super) fn confirm_global_parameter_source(&mut self) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            self.confirm_global_parameter_source = false;
            return self.fail_config(error);
        }
        if !self.profile_inventory_complete {
            self.confirm_global_parameter_source = false;
            return self.fail_config(crate::i18n::t!("activation-profile-inventory-error"));
        }
        if !self.confirm_global_parameter_source {
            return Update::none();
        }
        self.confirm_global_parameter_source = false;
        self.commit_open_parameter_scope(ParameterScope::Global)
    }

    /// Open the copy-settings dialog for the parameter editor's current profile. Only a package in
    /// per-profile scope with an editable, current configuration can copy.
    pub(super) fn open_copy_settings(&mut self) -> Update<Message, Event> {
        let Some(config) = self.param_config.as_ref() else {
            return Update::none();
        };
        if !self.param_config_edit_available(config)
            || config.parameter_scope != ParameterScope::Profile
            || self.profile_names.len() < 2
        {
            return Update::none();
        }
        self.copy_settings_prompt = Some(CopySettingsPrompt {
            specifier: config.specifier.clone(),
            source: config.profile_name.clone(),
            destination: None,
        });
        Update::none()
    }

    pub(super) fn select_copy_settings_destination(
        &mut self,
        profile_name: String,
    ) -> Update<Message, Event> {
        if let Some(prompt) = self.copy_settings_prompt.as_mut()
            && profile_name != prompt.source
            && self.profile_names.contains(&profile_name)
        {
            prompt.destination = Some(profile_name);
        }
        Update::none()
    }

    pub(super) fn cancel_copy_settings(&mut self) -> Update<Message, Event> {
        self.copy_settings_prompt = None;
        Update::none()
    }

    /// Copy every declared value and secret of the open package from the dialog's source profile
    /// to its destination, replacing the destination's values, and reload a session running the
    /// destination profile.
    pub(super) fn confirm_copy_settings(&mut self) -> Update<Message, Event> {
        let Some(prompt) = self.copy_settings_prompt.take() else {
            return Update::none();
        };
        let Some(destination) = prompt.destination else {
            return Update::none();
        };
        let Some((expected, params)) = self
            .param_config
            .as_ref()
            .filter(|config| {
                config.specifier == prompt.specifier
                    && config.profile_name == prompt.source
                    && self.param_config_edit_available(config)
            })
            .and_then(|config| {
                config
                    .expected_package
                    .clone()
                    .map(|expected| (expected, config.params.clone()))
            })
        else {
            return Update::none();
        };
        match shared_packages::copy_profile_param_values_if_unchanged(
            &self.server_name,
            &expected,
            &params,
            &prompt.source,
            &destination,
        ) {
            Ok(PackageParamCommit::Applied) => {}
            Ok(PackageParamCommit::StateChanged) => {
                return self.fail_config_parameter_state_changed();
            }
            Err(error) => {
                return self.fail_config(crate::i18n::t!(
                    "package-settings-copy-failed",
                    "error" => error.to_string()
                ));
            }
        }
        let event = self
            .configuration_change_affects_running(&prompt.specifier, Some(&destination))
            .then(|| Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            });
        Update::new(
            self.show_toast(crate::i18n::t!("package-settings-copied", "profile" => &destination)),
            event,
        )
    }

    /// The copy-settings dialog over the whole window: a backdrop that cancels, and a card with
    /// the destination picker and Cancel/Copy actions. Copy is offered only once a destination is
    /// chosen.
    pub(super) fn view_copy_settings_modal<'a>(
        &'a self,
        prompt: &'a CopySettingsPrompt,
    ) -> Elem<'a> {
        let backdrop = iced::widget::mouse_area(
            container(iced::widget::space::vertical())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|theme: &crate::theme::Theme| container::Style {
                    background: Some(Background::Color(theme.styles.general.overlay_background)),
                    ..Default::default()
                }),
        )
        .on_press(Message::CancelCopySettings);

        let choices = self
            .profile_names
            .iter()
            .filter(|profile| **profile != prompt.source)
            .map(|profile| ProfileChoice {
                key: profile.clone(),
                label: profile.clone(),
            })
            .collect::<Vec<_>>();
        let selected = choices
            .iter()
            .find(|choice| Some(&choice.key) == prompt.destination.as_ref())
            .cloned();
        let card = container(
            column![
                text(crate::i18n::t!("package-copy-settings-title")).size(14.0),
                text(crate::i18n::t!(
                    "package-copy-settings-help",
                    "profile" => &prompt.source
                ))
                .size(12.0)
                .style(common::muted),
                row![
                    text(crate::i18n::t!("package-copy-settings-destination"))
                        .size(12.0)
                        .style(common::muted),
                    iced::widget::pick_list(choices, selected, |choice| {
                        Message::SelectCopySettingsDestination(choice.key)
                    })
                    .placeholder(crate::i18n::ts!("package-copy-settings-choose")),
                ]
                .spacing(10.0)
                .align_y(Vertical::Center),
                row![
                    iced::widget::space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::CancelCopySettings),
                    button(text(crate::i18n::t!("action-copy")).size(12.0))
                        .style(button_style::primary)
                        .on_press_maybe(
                            prompt
                                .destination
                                .is_some()
                                .then_some(Message::ConfirmCopySettings)
                        ),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            ]
            .spacing(10.0),
        )
        .padding(16.0)
        .width(Length::Fixed(420.0))
        .style(common::card_style);
        let centered = container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .align_y(Vertical::Center);
        iced::widget::stack![backdrop, centered]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Compare the complete stored profile values without displaying secret contents. A global
    /// source can be selected automatically only when every profile is identical.
    fn profile_param_values_are_equal(
        &self,
        specifier: &str,
        params: &[PackageParameter],
    ) -> Result<bool, String> {
        if !self.profile_inventory_complete {
            return Ok(false);
        }
        let profiles = if self.profile_names.is_empty() {
            vec![self.profile_name.as_str()]
        } else {
            self.profile_names.iter().map(String::as_str).collect()
        };
        let mut first = None;
        for profile in profiles {
            let mut signature = Vec::with_capacity(params.len());
            for param in params {
                let entry = if is_secret_string(param) {
                    (
                        param.key.clone(),
                        None,
                        shared_packages::load_secret_param_scoped_checked(
                            &self.server_name,
                            ParamValueScope::Profile(profile),
                            specifier,
                            &param.key,
                        )
                        .map_err(|error| {
                            crate::i18n::t!(
                                "package-settings-read-unavailable",
                                "error" => error.to_string()
                            )
                        })?,
                    )
                } else {
                    (
                        param.key.clone(),
                        shared_packages::get_param_value_scoped_checked(
                            &self.server_name,
                            ParamValueScope::Profile(profile),
                            specifier,
                            &param.key,
                        )
                        .map_err(|error| {
                            crate::i18n::t!(
                                "package-settings-read-unavailable",
                                "error" => error.to_string()
                            )
                        })?,
                        None,
                    )
                };
                signature.push(entry);
            }
            if let Some(first) = &first {
                if first != &signature {
                    return Ok(false);
                }
            } else {
                first = Some(signature);
            }
        }
        Ok(true)
    }

    fn commit_open_parameter_scope(&mut self, target: ParameterScope) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            return self.fail_config(error);
        }
        if !self.profile_inventory_complete {
            return self.fail_config(crate::i18n::t!("activation-profile-inventory-error"));
        }
        let Some(config) = self.param_config.as_ref() else {
            return Update::none();
        };
        if !config.available {
            return Update::none();
        }
        let specifier = config.specifier.clone();
        let params = config.params.clone();
        let Some(expected_package) = config.expected_package.clone() else {
            return self.fail_config(crate::i18n::t!("package-settings-read-unavailable-generic"));
        };
        let mut known_profiles: BTreeSet<String> = self.profile_names.iter().cloned().collect();
        if known_profiles.is_empty() {
            known_profiles.insert(self.profile_name.clone());
        }
        let source_profile =
            matches!(target, ParameterScope::Global).then_some(self.parameter_profile.as_str());
        match shared_packages::migrate_parameter_scope_if_unchanged(
            &self.server_name,
            &expected_package,
            target,
            source_profile,
            &known_profiles,
            &params,
        ) {
            Ok(PackageParamCommit::Applied) => {}
            Ok(PackageParamCommit::StateChanged) => {
                return self.fail_config_parameter_state_changed();
            }
            Err(error) => {
                return self.fail_config(crate::i18n::t!(
                    "package-settings-save-failed",
                    "error" => error.to_string()
                ));
            }
        }

        if let Err(message) = self.reload_package_lock_snapshot() {
            if let Some(config) = self.param_config.as_mut() {
                config.available = false;
                config.error = Some(message);
                config.saved = false;
            }
            return Update::with_event(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            });
        }
        self.seed_param_config(specifier.clone(), params);
        let event = self
            .configuration_change_affects_running(&specifier, None)
            .then(|| Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            });
        let task = self.show_toast(crate::i18n::t!("package-parameter-scope-updated"));
        Update::new(task, event)
    }

    pub(super) fn select_parameter_profile(
        &mut self,
        profile_name: String,
    ) -> Update<Message, Event> {
        if !self.package_state_available()
            || !self.profile_inventory_complete
            || !self.profile_names.contains(&profile_name)
        {
            return Update::none();
        }
        let Some(config) = self.param_config.as_ref() else {
            return Update::none();
        };
        if !config.available {
            return Update::none();
        }
        let specifier = config.specifier.clone();
        let params = config.params.clone();
        self.parameter_profile = profile_name;
        self.seed_param_config(specifier, params);
        Update::none()
    }

    /// Whether parameter state for `specifier` can affect running code. `profile` limits the test
    /// to one profile; `None` covers a global/scope change and therefore checks every profile.
    /// Both imported dependencies and separately-running `requires` roots count: changing either
    /// can alter an enabled parent's behavior.
    fn configuration_change_affects_running(&self, specifier: &str, profile: Option<&str>) -> bool {
        // A global value or scope change can affect any same-server session. If profile discovery
        // is incomplete, conservatively reload them all rather than leave an undiscovered profile
        // running stale parameter state.
        if profile.is_none() && !self.profile_inventory_complete {
            return true;
        }
        let mut profiles = match profile {
            Some(profile) => vec![profile.to_string()],
            None => self.profile_names.clone(),
        };
        if profiles.is_empty() {
            profiles.push(self.profile_name.clone());
        }
        profiles.into_iter().any(|profile| {
            let mut visited = HashSet::new();
            self.package_used_in_profile(specifier, &profile, &mut visited)
        })
    }

    /// Whether replacing all providers of `leaf` with a new local folder changes any running
    /// root/import path. This covers pure transitive dependencies that have no lock row of their
    /// own, as well as a differently named copy that happens to shadow another live leaf.
    fn leaf_change_affects_running(&self, leaf: &str) -> bool {
        let mut candidates = self
            .installed_packages
            .iter()
            .map(|package| package.specifier.clone())
            .chain(self.graph.requires.keys().cloned())
            .chain(
                self.graph
                    .requires
                    .values()
                    .flatten()
                    .map(|edge| edge.specifier.clone()),
            )
            .filter(|specifier| naming::names_conflict(package_display_name(specifier), leaf))
            .collect::<HashSet<_>>();
        if let Some(open) = self.installed_open.as_deref()
            && naming::names_conflict(package_display_name(&open.specifier), leaf)
        {
            candidates.insert(open.specifier.clone());
        }
        candidates
            .iter()
            .any(|specifier| self.configuration_change_affects_running(specifier, None))
    }

    fn package_used_in_profile(
        &self,
        specifier: &str,
        profile: &str,
        visited: &mut HashSet<String>,
    ) -> bool {
        let canonical = if let Some(name) = specifier.strip_prefix("local:") {
            self.local_own_spec(name)
        } else {
            self.governing_specifier(specifier)
        };
        if !visited.insert(canonical.clone()) {
            return false;
        }
        let lock = SharedPackageLock {
            packages: self.installed_packages.clone(),
        };
        if lock.is_effectively_enabled_for(&canonical, profile) {
            return true;
        }
        self.graph.requires.iter().any(|(parent, edges)| {
            edges.iter().any(|edge| {
                if edge.kind != DependencyKind::Dependency
                    || self.governing_specifier(&edge.specifier) != canonical
                {
                    return false;
                }
                let mut branch = visited.clone();
                self.package_used_in_profile(parent, profile, &mut branch)
            })
        })
    }

    /// A local folder is authoritative for its leaf name across every owner. Remote lock rows with
    /// that leaf are dormant fallbacks and must never contribute graph metadata while the folder
    /// exists.
    fn has_local_override_for(&self, specifier: &str) -> bool {
        let leaf = package_display_name(specifier);
        self.local_packages
            .iter()
            .any(|name| naming::names_conflict(name, leaf))
    }

    /// Rebuilds the direct/owned sets from the current lists, preserving the
    /// async-resolved `requires`/`resolved` maps and the user's enable intent.
    pub(super) fn rebuild_graph(&mut self) {
        self.graph.direct.clear();
        self.graph.owned.clear();
        for pkg in &self.installed_packages {
            let dormant_fallback = self.has_local_override_for(&pkg.specifier)
                && parse_specifier(&pkg.specifier).is_some_and(|(owner, _)| {
                    !owner.eq_ignore_ascii_case(local_packages::LOCAL_OWNER)
                });
            if dormant_fallback {
                continue;
            }
            if pkg.has_direct_activation() {
                self.graph.direct.insert(pkg.specifier.clone());
            }
            for parent in &pkg.required_by {
                // A published parent's relationship set belongs to its dormant fallback. The
                // local replacement gets its own explicit `smudgy://local/...` links from its
                // manifest, so the two closures cannot accidentally merge.
                if self.governing_specifier(parent) != *parent {
                    continue;
                }
                let edges = self.graph.requires.entry(parent.clone()).or_default();
                if !edges.iter().any(|edge| edge.specifier == pkg.specifier) {
                    edges.push(DepEdge {
                        specifier: pkg.specifier.clone(),
                        range: "*".to_string(),
                        kind: DependencyKind::Requires,
                    });
                }
            }
            // Seed the enable intent from the persisted lockfile flag (the engine's source of
            // truth), so a package installed "don't enable" — or toggled off — shows disabled and
            // is held out of execution until enabled.
            self.graph.intent.insert(
                pkg.specifier.clone(),
                pkg.is_enabled_for(&self.profile_name),
            );
            if let Some(v) = &pkg.last_resolved_version {
                self.graph.resolved.insert(pkg.specifier.clone(), v.clone());
            }
        }
        // Local packages: attach their manifest edges to the same canonical lock-row identity that
        // carries activation. They are author-owned, but no longer unconditionally enabled.
        for name in self.local_packages.clone() {
            let spec = self.local_own_spec(&name);
            if let Ok(Some(pkg)) = local_packages::load_local_package(&self.server_name, &name) {
                let edges = pkg
                    .manifest
                    .smudgy_dependencies()
                    .into_iter()
                    .map(|dependency| {
                        let requested = specifier_for(&dependency.key.owner, &dependency.key.name);
                        DepEdge {
                            specifier: self.governing_specifier(&requested),
                            range: dependency.range.unwrap_or_default(),
                            kind: DependencyKind::Dependency,
                        }
                    })
                    .chain(
                        pkg.manifest
                            .smudgy_requires()
                            .into_iter()
                            .map(|dependency| {
                                let requested =
                                    specifier_for(&dependency.key.owner, &dependency.key.name);
                                DepEdge {
                                    specifier: self.governing_specifier(&requested),
                                    range: dependency.range.unwrap_or_default(),
                                    kind: DependencyKind::Requires,
                                }
                            }),
                    )
                    .collect();
                self.graph.requires.insert(spec, edges);
            }
        }
    }

    /// Resolves each installed package once to populate its `requires` edges
    /// (best-effort; failures leave the tree flat). A frozen credential is still required because
    /// installed private packages may be visible to only one account.
    pub(super) fn resolve_graph_deps(&self) -> Task<Message> {
        let mut tasks = Vec::new();
        let seq = self.graph_seq;
        for pkg in &self.installed_packages {
            if self.has_local_override_for(&pkg.specifier) {
                continue;
            }
            let Some((owner, name)) = parse_specifier(&pkg.specifier) else {
                continue;
            };
            let spec = pkg.specifier.clone();
            let staged = pkg.staged_version().map(str::to_string);
            let expected_staged = staged.clone();
            let (account_fence, client) = self.frozen_package_client();
            tasks.push(Task::perform(
                async move {
                    let resolved = client
                        .resolve_package(&owner, &name, staged.as_deref())
                        .await?;
                    // Fold the newest resolvable version's closure union too, so the tree can flag
                    // an update that's blocked because it needs more permissions than were granted.
                    // (The version floor isn't surfaced in the graph; the manage pane covers it.)
                    let closure = closure_permission_union(&client, &resolved).await?;
                    Ok::<_, CloudError>((resolved, closure.permissions))
                },
                move |result| {
                    Message::InstalledResolvedForGraph(
                        seq,
                        account_fence,
                        spec.clone(),
                        expected_staged.clone(),
                        result,
                    )
                },
            ));
        }
        Task::batch(tasks)
    }

    pub(super) fn installed_resolved_for_graph(
        &mut self,
        seq: GraphSeq,
        account_fence: AccountReadFence,
        spec: &str,
        expected_staged: Option<&str>,
        result: Result<(ResolvedPackageWire, PackagePermissions), CloudError>,
    ) -> Update<Message, Event> {
        if seq != self.graph_seq
            || !self.account_read_is_current(account_fence)
            || self.has_local_override_for(spec)
            || self
                .installed_packages
                .iter()
                .find(|package| package.specifier == spec)
                .map(|package| package.staged_version())
                != Some(expected_staged)
        {
            return Update::none();
        }
        if let Ok((resolved, union)) = result {
            let required_specifiers = resolved
                .dependencies
                .iter()
                .filter(|dependency| dependency.kind == DependencyKind::Requires)
                .map(|dependency| specifier_for(&dependency.owner_nickname, &dependency.name))
                .collect::<Vec<_>>();
            let edges = resolved
                .dependencies
                .iter()
                .map(|d| {
                    let requested = specifier_for(&d.owner_nickname, &d.name);
                    DepEdge {
                        specifier: self.governing_specifier(&requested),
                        range: d.range.clone(),
                        kind: d.kind,
                    }
                })
                .collect();
            let installed_requirements = required_specifiers
                .iter()
                .flat_map(|dependency| self.required_state_specifiers(dependency))
                .collect::<Vec<_>>();
            let links_changed = match shared_packages::set_required_closure_if_staged_unchanged(
                &self.server_name,
                spec,
                expected_staged,
                &installed_requirements,
            ) {
                Ok(shared_packages::RequiredClosureCommit::Changed) => true,
                Ok(shared_packages::RequiredClosureCommit::Unchanged) => false,
                Ok(shared_packages::RequiredClosureCommit::Stale) => {
                    self.graph_seq.bump();
                    return Update::with_task(Task::done(Message::LoadInstalledPackages));
                }
                Err(error) => {
                    log::warn!("Failed to persist package dependency links for {spec}: {error:#}");
                    return Update::none();
                }
            };
            self.graph
                .resolved
                .insert(spec.to_string(), resolved.version.clone());
            self.graph.requires.insert(spec.to_string(), edges);
            for dep in &resolved.dependencies {
                let requested = specifier_for(&dep.owner_nickname, &dep.name);
                self.graph.resolved.insert(
                    self.governing_specifier(&requested),
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
            if links_changed {
                return Update::with_event(Event::ScriptsChanged {
                    server_name: self.server_name.clone(),
                });
            }
        } else {
            self.blocked_updates.remove(spec);
        }
        Update::none()
    }
}

// ============================================================================
// Installed package — update side
// ============================================================================

impl AutomationsWindow {
    pub(super) fn open_installed_package(&mut self, specifier: String) -> Update<Message, Event> {
        if let Some(local_name) = self.local_override_name(&specifier).map(str::to_string) {
            return self.open_owned_package(local_name);
        }
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
        if let Some(local_name) = self.local_override_name(&specifier).map(str::to_string) {
            return self.open_owned_package(local_name);
        }
        let selection = Selection::Dependency {
            parent,
            spec: specifier.clone(),
        };
        self.open_installed_package_with_selection(specifier, selection)
    }

    pub(super) fn selected_dependency_kind(&self) -> Option<DependencyKind> {
        let Selection::Dependency { parent, spec } = &self.selection else {
            return None;
        };
        self.graph
            .requires
            .get(parent)
            .and_then(|edges| edges.iter().find(|edge| edge.specifier == *spec))
            .map(|edge| edge.kind)
    }

    fn open_installed_package_with_selection(
        &mut self,
        specifier: String,
        selection: Selection,
    ) -> Update<Message, Event> {
        self.clear_selection();
        self.installed_detail = None;
        self.installed_readme = InstalledReadmeState::Loading;
        // Drop the prior package's rating so the meta row doesn't flash the previous package's
        // stars/installs during this package's async detail load; repopulated when the resolve lands.
        self.installed_rating = None;
        self.installed_versions.clear();
        self.installed_selected_file = None;
        self.installed_package_tab = InstalledPackageTab::About;
        self.parameter_profile.clone_from(&self.profile_name);
        // Bound the content-addressed source cache to the open package's files. Late fetches from a
        // prior package would only ever re-insert their own (hash-verified) bytes, so this is for
        // memory, not correctness.
        self.installed_source.clear();
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
            self.installed_readme =
                InstalledReadmeState::Failed(crate::i18n::t!("package-detail-not-loaded"));
            return Update::none();
        };
        // The primary detail and Source panes audit the version the runtime is actually staged to
        // load. Auto mode may have a newer registry version, but showing that code here would make
        // a blocked update look like the currently running package. Update comparison is handled
        // separately by the graph/update pipeline.
        let staged = self
            .installed_packages
            .iter()
            .find(|p| p.specifier == specifier)
            .and_then(|p| p.staged_version().map(str::to_string))
            .or_else(|| self.graph.resolved.get(specifier).cloned());
        let compare_latest = self
            .installed_packages
            .iter()
            .find(|package| package.specifier == specifier)
            .is_some_and(|package| matches!(package.mode, UpdateMode::Auto));
        let (account_fence, client) = self.frozen_package_client();
        let latest_client = client.clone();
        self.manage_busy = true;
        self.manage_feedback = None;
        self.installed_readme = InstalledReadmeState::Loading;
        self.update_delta = None;
        // Tag this load with the current detail generation; a result that arrives after the user has
        // moved on (opened another package, navigated away, uninstalled, or re-resolved) carries a
        // stale token and is discarded in `installed_detail_loaded`.
        let seq = self.detail_seq;
        let staged_for_detail = staged.clone();
        let latest_owner = owner.clone();
        let latest_name = name.clone();
        let staged_for_latest = staged.clone();
        let detail_task = Task::perform(
            async move {
                let resolved = client
                    .resolve_package(&owner, &name, staged_for_detail.as_deref())
                    .await?;
                // Fold the closure union too, so the manage pane can detect an update that adds
                // permission asks beyond the consented baseline (delta re-prompt), and the
                // version floor, so it can explain a version held back by `min_smudgy_version`.
                let closure = closure_permission_union(&client, &resolved).await?;
                let versions = client.list_versions(resolved.package_id).await?;
                // Best-effort cloud metadata (rating average/count, install count) for the meta row.
                // Public read, so it works logged out; a failure just leaves the rating UI hidden
                // rather than failing the whole detail load.
                let rating = client.get_package(resolved.package_id).await.ok();
                // A yanked version is valid only when this install is already pinned to it.
                // Do not offer unrelated yanked versions as new pin targets. Hard-deleted
                // versions have no content and are never valid targets.
                Ok((
                    resolved,
                    versions
                        .into_iter()
                        .filter(|version| {
                            !version.deleted
                                && (!version.yanked
                                    || staged_for_detail.as_deref()
                                        == Some(version.version.as_str()))
                        })
                        .map(|v| v.version)
                        .collect(),
                    closure.permissions,
                    closure.floor,
                    rating,
                ))
            },
            move |result| Message::InstalledDetailLoaded(seq, account_fence, Box::new(result)),
        );
        if !compare_latest {
            return Update::with_task(detail_task);
        }

        // Auto mode needs a second, independently fenced probe. It drives only the update/floor
        // card; it never replaces the staged detail, README, dependency graph, parameters, or
        // Source modules above.
        let latest_task = Task::perform(
            async move {
                let resolved = latest_client
                    .resolve_package(&latest_owner, &latest_name, None)
                    .await?;
                let latest_requirements = resolved_requires_signature(&resolved);
                let requirements_changed = if latest_requirements.is_empty()
                    || staged_for_latest.as_deref() == Some(resolved.version.as_str())
                {
                    false
                } else if let Some(staged) = staged_for_latest.as_deref() {
                    latest_client
                        .resolve_package(&latest_owner, &latest_name, Some(staged))
                        .await
                        .map(|current| resolved_requires_signature(&current) != latest_requirements)
                        // A non-empty offered requirement set without a readable baseline must be
                        // reviewed. Failing open here would let a new independent root bypass the
                        // planner merely because the old metadata was temporarily unavailable.
                        .unwrap_or(true)
                } else {
                    true
                };
                let closure = closure_permission_union(&latest_client, &resolved).await?;
                Ok::<_, CloudError>(InstalledLatestComparison {
                    version: resolved.version,
                    permissions: closure.permissions,
                    floor: closure.floor,
                    requirements_changed,
                })
            },
            move |result| Message::InstalledLatestCompared(seq, account_fence, result),
        );
        Update::with_task(Task::batch([detail_task, latest_task]))
    }

    /// Select a module file in the Source tab and start loading its source. This is for actual
    /// modules only; the rendered metadata README lives in About, so a
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
        let (account_fence, client) = self.frozen_package_client();
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
                account_fence,
                result,
            },
        ))
    }

    pub(super) fn installed_source_loaded(
        &mut self,
        hash: String,
        account_fence: AccountReadFence,
        result: Result<FilePreview, CloudError>,
    ) -> Update<Message, Event> {
        if !self.account_read_is_current(account_fence) {
            return Update::none();
        }
        let preview = result.unwrap_or_else(|e| FilePreview::Error(display_error(&e)));
        self.installed_source.insert(hash, preview);
        Update::none()
    }

    pub(super) fn installed_detail_loaded(
        &mut self,
        seq: DetailSeq,
        account_fence: AccountReadFence,
        result: Result<InstalledDetail, CloudError>,
    ) -> Update<Message, Event> {
        // Discard a superseded load: the open package changed (another package opened, navigation,
        // uninstall) or was re-resolved while this was in flight. Returning before touching
        // `manage_busy` leaves the newer in-flight load's spinner intact, and — critically — keeps
        // the silent shrink-branch `record_consent` below from firing for a package that is no
        // longer open (it would otherwise rewrite consent for the wrong, superseded package).
        if seq != self.detail_seq || !self.account_read_is_current(account_fence) {
            return Update::none();
        }
        self.manage_busy = false;
        self.consent_busy = false;
        match result {
            Ok((resolved, versions, permissions, floor, rating)) => {
                let mut side_tasks = Vec::new();
                let mut scripts_changed = false;
                // Cloud rating/install metadata for the meta row (best-effort; `None` just hides it).
                self.installed_rating = rating.map(Box::new);
                // Always track the resolved version's README (the pane defaults to it so the user
                // reviews before enabling). Refreshing unconditionally — not only when the README is
                // the current selection — keeps it in sync across a re-resolve: otherwise a pin/update
                // change while a source file is selected would leave the README sub-tab showing the
                // previous version's text under the new version's header.
                self.installed_readme = InstalledReadmeState::Loaded(
                    resolved.readme.as_deref().map(markdown::Content::parse),
                );
                // Feed the dependency graph (and the blocked-update flag, via the closure union).
                if let Some((spec, staged)) = self.installed_open.as_ref().map(|package| {
                    (
                        package.specifier.clone(),
                        package.staged_version().map(str::to_string),
                    )
                }) {
                    let graph_update = self.installed_resolved_for_graph(
                        self.graph_seq,
                        account_fence,
                        &spec,
                        staged.as_deref(),
                        Ok((resolved.clone(), permissions.clone())),
                    );
                    scripts_changed |= graph_update.event.is_some();
                    side_tasks.push(graph_update.task);
                }
                // Pinned installs compare this exact staged version. Auto installs use the
                // separate latest probe so a late staged-detail response cannot overwrite the
                // newest update/floor card.
                if self
                    .installed_open
                    .as_deref()
                    .is_some_and(|open| matches!(open.mode, UpdateMode::Pinned { .. }))
                {
                    match self.apply_update_comparison(
                        resolved.version.clone(),
                        permissions,
                        floor,
                        false,
                    ) {
                        UpdateComparisonApply::Current => {}
                        UpdateComparisonApply::ConsentChanged => scripts_changed = true,
                        UpdateComparisonApply::Stale => {
                            let refresh = self.refresh_stale_installed_detail();
                            side_tasks.push(refresh.task);
                            return Update::new(
                                Task::batch(side_tasks),
                                refresh.event.or_else(|| {
                                    scripts_changed.then_some(Event::ScriptsChanged {
                                        server_name: self.server_name.clone(),
                                    })
                                }),
                            );
                        }
                    }
                }
                // Seed the inline "Settings" editor from the resolved version's declared params
                // (re-seeded on every resolve, so a version that adds/removes params stays in step).
                // An imported dependency runs in its parent's isolate and has no independent
                // settings. A `requires` target is a separate root and keeps its own parameters.
                let params = serde_json::from_value::<PackageManifest>(resolved.manifest.clone())
                    .map(|manifest| manifest.params)
                    .unwrap_or_default();
                self.installed_detail = Some(Box::new(resolved));
                self.installed_versions = versions;
                if self.selected_dependency_kind() == Some(DependencyKind::Dependency) {
                    self.param_config = None;
                } else if let Some(spec) = self.installed_open.as_ref().map(|p| p.specifier.clone())
                {
                    self.seed_param_config(spec, params);
                }
                // A re-resolve (e.g. a version-pin change) can swap the module set out from under a
                // still-selected file, changing its content hash. Re-fetch the open file so the
                // source pane tracks the version now shown instead of stalling on "Fetching…".
                let source_update = self.ensure_selected_source();
                side_tasks.push(source_update.task);
                Update::new(
                    Task::batch(side_tasks),
                    if scripts_changed {
                        Some(Event::ScriptsChanged {
                            server_name: self.server_name.clone(),
                        })
                    } else {
                        source_update.event
                    },
                )
            }
            Err(e) => {
                let error = display_error(&e);
                self.installed_readme = InstalledReadmeState::Failed(error.clone());
                self.manage_feedback = Some(error);
                Update::none()
            }
        }
    }

    pub(super) fn installed_latest_compared(
        &mut self,
        seq: DetailSeq,
        account_fence: AccountReadFence,
        result: Result<InstalledLatestComparison, CloudError>,
    ) -> Update<Message, Event> {
        if seq != self.detail_seq || !self.account_read_is_current(account_fence) {
            return Update::none();
        }
        if let Ok(comparison) = result {
            match self.apply_update_comparison(
                comparison.version,
                comparison.permissions,
                comparison.floor,
                comparison.requirements_changed,
            ) {
                UpdateComparisonApply::Stale => return self.refresh_stale_installed_detail(),
                UpdateComparisonApply::ConsentChanged => {
                    return Update::with_event(Event::ScriptsChanged {
                        server_name: self.server_name.clone(),
                    });
                }
                UpdateComparisonApply::Current => {}
            }
        }
        Update::none()
    }

    fn refresh_stale_installed_detail(&mut self) -> Update<Message, Event> {
        let Some(specifier) = self
            .installed_open
            .as_deref()
            .map(|package| package.specifier.clone())
        else {
            return Update::none();
        };
        let lock = match shared_packages::load_lock(&self.server_name) {
            Ok(lock) => lock,
            Err(error) => {
                self.manage_feedback = Some(error.to_string());
                return Update::none();
            }
        };
        self.installed_packages = lock.packages;
        self.rebuild_graph();
        let Some(current) = self
            .installed_packages
            .iter()
            .find(|package| package.specifier == specifier)
            .cloned()
        else {
            self.clear_selection();
            self.selection = Selection::Dashboard;
            self.pane = Pane::Dashboard;
            return Update::none();
        };
        self.installed_open = Some(Box::new(current));
        self.detail_seq.bump();
        self.load_installed_detail(&specifier)
    }

    /// Compare one candidate version with the open package's consent and client-version floor.
    /// This mutates only the update card/consent baseline; the candidate never becomes pane Source
    /// truth until the normal staged-version pipeline advances the lock.
    fn apply_update_comparison(
        &mut self,
        version: String,
        permissions: PackagePermissions,
        floor: shared_packages::SmudgyVersionFloor,
        requirements_changed: bool,
    ) -> UpdateComparisonApply {
        let Some(open) = self.installed_open.as_deref() else {
            return UpdateComparisonApply::Current;
        };
        let expected = open.clone();
        let specifier = open.specifier.clone();
        let current_version = open.last_resolved_version.clone();

        // A version floor applies to trusted packages too: no permission grant can make an
        // incompatible build run.
        if let Some(reason) = floor.refusal(&shared_packages::running_smudgy_release()) {
            self.update_delta = Some(UpdateDelta {
                name: package_display_name(&specifier).to_string(),
                specifier,
                version,
                current_version,
                added: PackagePermissions::default(),
                needs_smudgy: Some(reason),
                requirements_changed,
            });
            return UpdateComparisonApply::Current;
        }
        if requirements_changed {
            let baseline = open.consented_permissions.clone().unwrap_or_default();
            self.update_delta = Some(UpdateDelta {
                name: package_display_name(&specifier).to_string(),
                specifier,
                version,
                current_version,
                added: permissions.added_since(&baseline),
                needs_smudgy: None,
                requirements_changed: true,
            });
            return UpdateComparisonApply::Current;
        }
        if open.trusted {
            self.update_delta = None;
            return UpdateComparisonApply::Current;
        }

        let baseline = open.consented_permissions.clone().unwrap_or_default();
        let added = permissions.added_since(&baseline);
        if !added.is_empty() {
            self.update_delta = Some(UpdateDelta {
                name: package_display_name(&specifier).to_string(),
                specifier,
                version,
                current_version,
                added,
                needs_smudgy: None,
                requirements_changed: false,
            });
            return UpdateComparisonApply::Current;
        }

        self.update_delta = None;
        // Consent may narrow automatically, but never grows without an explicit grant.
        let removed = baseline.added_since(&permissions);
        if !removed.is_empty() {
            match shared_packages::record_consent_if_unchanged(
                &self.server_name,
                &expected,
                &permissions,
            ) {
                Ok(true) => {
                    if let Some(package) = self
                        .installed_packages
                        .iter_mut()
                        .find(|package| package.specifier == specifier)
                    {
                        package.consented_permissions = Some(permissions.clone());
                    }
                    if let Some(open) = &mut self.installed_open {
                        open.consented_permissions = Some(permissions);
                    }
                    return UpdateComparisonApply::ConsentChanged;
                }
                Ok(false) => return UpdateComparisonApply::Stale,
                Err(error) => self.manage_feedback = Some(error.to_string()),
            }
        }
        UpdateComparisonApply::Current
    }

    /// Sets the caller's 1–5 star rating for the open installed cloud package. The package id comes
    /// from the resolved detail (always present while the manage pane is open); an account is
    /// required server-side, so the view gates the star control on `signed_in()`.
    pub(super) fn rate_installed_package(&self, stars: i16) -> Update<Message, Event> {
        let Some(detail) = self.installed_detail.as_deref() else {
            return Update::none();
        };
        let package_id = detail.package_id;
        let detail_seq = self.detail_seq;
        let (account_fence, client) = self.frozen_package_client();
        Update::with_task(Task::perform(
            async move { client.rate_package(package_id, stars).await },
            move |result| Message::InstalledRatingUpdated {
                detail_seq,
                package_id,
                account_fence,
                result,
            },
        ))
    }

    pub(super) fn installed_rating_updated(
        &mut self,
        detail_seq: DetailSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<PackageDetail, CloudError>,
    ) -> Update<Message, Event> {
        if detail_seq != self.detail_seq
            || !self.account_read_is_current(account_fence)
            || self
                .installed_detail
                .as_deref()
                .map(|detail| detail.package_id)
                != Some(package_id)
        {
            return Update::none();
        }
        match result {
            // The server returns the fresh rating average/count, so the meta row updates in place.
            Ok(detail) => self.installed_rating = Some(Box::new(detail)),
            Err(e) => self.manage_feedback = Some(display_error(&e)),
        }
        Update::none()
    }

    pub(super) fn set_installed_update_mode(&mut self, mode: UpdateMode) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        let Some(open) = self.installed_open.as_deref() else {
            return Update::none();
        };
        if open.mode == mode {
            return Update::none();
        }
        let expected = open.clone();

        // Pinning the version already staged changes policy only; no code, permissions, or required
        // roots move, so it does not need another consent round trip.
        if let UpdateMode::Pinned { version } = &mode
            && open.staged_version() == Some(version.as_str())
        {
            return self.persist_installed_update_mode(&expected, mode);
        }

        // Every actual version movement goes through the same closure/requirements planner as an
        // install. A version picker must not bypass peer constraints or materialization merely
        // because the user selected an exact pin.
        self.begin_installed_version_change(mode)
    }

    fn persist_installed_update_mode(
        &mut self,
        expected: &LockedPackage,
        mode: UpdateMode,
    ) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        match shared_packages::set_update_mode_if_unchanged(
            &self.server_name,
            expected,
            mode.clone(),
        ) {
            Ok(Cas::Applied) => {}
            Ok(Cas::StateChanged) => {
                if let Err(message) = self.reload_package_lock_snapshot() {
                    self.manage_feedback = Some(message);
                    return Update::none();
                }
                self.refresh_local_shadow_after_authoritative_mutation();
                if self.installed_open.is_none() {
                    return Update::with_task(Task::batch([
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]));
                }
                self.manage_feedback = Some(crate::i18n::t!("package-install-plan-changed"));
                return self.begin_installed_version_change(mode);
            }
            Err(e) => {
                self.refresh_local_shadow_after_authoritative_mutation();
                self.manage_feedback = Some(crate::i18n::t!(
                    "package-update-mode-failed",
                    "error" => e.to_string()
                ));
                return Update::none();
            }
        }
        if let Err(message) = self.reload_package_lock_snapshot() {
            self.manage_feedback = Some(message);
            return Update::none();
        }
        // Only the policy changed, but the pane's update comparison depends on it (Auto probes
        // the latest version; a pin compares the exact staged one), so re-open the row from the
        // committed lock rather than leaving a card that was built for the previous mode.
        self.refresh_stale_installed_detail()
    }

    fn begin_installed_version_change(&mut self, mode: UpdateMode) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        let Some(open) = self.installed_open.as_deref().cloned() else {
            return Update::none();
        };
        let specifier = open.specifier.clone();
        if self.governing_specifier(&specifier) != specifier {
            self.manage_feedback = Some(crate::i18n::t!("package-install-plan-changed"));
            return Update::none();
        }
        let Some((owner, name)) = parse_specifier(&specifier) else {
            return Update::none();
        };
        let (expected_lock, local_manifests) = match self.load_consent_resolution_state() {
            Ok(state) => state,
            Err(error) => {
                self.manage_feedback = Some(error);
                return Update::none();
            }
        };
        if expected_lock.find(&specifier) != Some(&open) {
            self.manage_feedback = Some(crate::i18n::t!("package-install-plan-changed"));
            return Update::with_task(Task::done(Message::LoadInstalledPackages));
        }
        if local_manifests.keys().any(|local_specifier| {
            naming::names_conflict(package_display_name(local_specifier), &name)
        }) {
            self.manage_feedback = Some(crate::i18n::t!("package-install-plan-changed"));
            return Update::with_task(Task::batch([
                Task::done(Message::LoadLocalPackages),
                Task::done(Message::LoadInstalledPackages),
            ]));
        }
        let pinned = match &mode {
            UpdateMode::Auto => None,
            UpdateMode::Pinned { version } => Some(version.clone()),
        };
        let installed = expected_lock.packages;
        let (account_fence, client) = self.frozen_package_client();
        self.manage_busy = true;
        self.manage_feedback = None;
        self.detail_seq.bump();
        let seq = self.detail_seq;
        Update::with_task(Task::perform(
            async move {
                resolve_install_closure(
                    &client,
                    &owner,
                    &name,
                    pinned.as_deref(),
                    &installed,
                    &local_manifests,
                )
                .await
            },
            move |result| {
                Message::InstalledVersionChangeResolved(seq, account_fence, mode.clone(), result)
            },
        ))
    }

    pub(super) fn installed_version_change_resolved(
        &mut self,
        seq: DetailSeq,
        account_fence: AccountReadFence,
        mode: UpdateMode,
        result: Result<InstallResolution, CloudError>,
    ) -> Update<Message, Event> {
        if seq != self.detail_seq || !self.account_read_is_current(account_fence) {
            return Update::none();
        }
        self.manage_busy = false;
        match result {
            Ok(resolution)
                if self
                    .installed_open
                    .as_deref()
                    .is_some_and(|open| open.specifier == resolution.specifier) =>
            {
                if let Err(error) = self.validate_consent_snapshot(
                    &resolution.expected_lock,
                    &resolution.expected_local_manifests,
                ) {
                    self.manage_feedback = Some(error);
                    return Update::with_task(Task::batch([
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]));
                }
                let activation = self
                    .installed_open
                    .as_deref()
                    .map(LockedPackage::activation)
                    .unwrap_or(ProfileActivation::None);
                self.update_delta = None;
                self.install_seq.bump();
                self.consent_prompt = Some(ConsentPrompt {
                    account_fence,
                    specifier: resolution.specifier,
                    owner: resolution.owner,
                    name: resolution.name,
                    version: resolution.version,
                    permissions: resolution.permissions,
                    params: resolution.params,
                    closure: resolution.closure,
                    required_roots: resolution.required_roots,
                    conflict: resolution.conflict,
                    needs_smudgy: resolution.needs_smudgy,
                    required_unavailable: resolution.required_unavailable,
                    expected_lock: resolution.expected_lock,
                    expected_local_manifests: resolution.expected_local_manifests,
                    operation: ConsentOperation::Update { mode, activation },
                    error: None,
                });
            }
            Ok(_) => {}
            Err(error) => {
                // Beginning the change fenced the pane's detail load. If that load never landed,
                // reload it so the pane is not left loading forever; the failure stays visible
                // (the reload clears feedback, so it is set afterwards).
                let reload = if self.installed_detail.is_none() {
                    self.refresh_stale_installed_detail()
                } else {
                    Update::none()
                };
                self.manage_feedback = Some(display_error(&error));
                return reload;
            }
        }
        Update::none()
    }

    /// Begin the uninstall flow from one authoritative lock snapshot. Durable flattened
    /// `required_by` links provide the apt-style impact immediately; the same snapshot becomes the
    /// confirmation's optimistic concurrency token.
    pub(super) fn request_uninstall(&mut self) -> Update<Message, Event> {
        self.confirm_uninstall = false;
        self.uninstall_expected_lock = None;
        self.uninstall_orphans.clear();
        self.uninstall_breaks.clear();
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        let Some(specifier) = self.installed_open.as_ref().map(|p| p.specifier.clone()) else {
            return Update::none();
        };
        let lock = match shared_packages::load_lock(&self.server_name) {
            Ok(lock) => lock,
            Err(error) => {
                self.manage_feedback = Some(crate::i18n::t!(
                    "package-uninstall-failed",
                    "error" => error.to_string()
                ));
                return Update::none();
            }
        };
        let Some(target) = lock.find(&specifier) else {
            self.manage_feedback = Some(crate::i18n::t!("package-install-plan-changed"));
            return Update::with_task(Task::done(Message::LoadInstalledPackages));
        };
        if !target.has_direct_activation() {
            self.manage_feedback = Some(crate::i18n::t!("package-required-managed"));
            return Update::none();
        }
        if target.required_by.is_empty() {
            let plan = lock.plan_removal_from_links(&specifier);
            self.uninstall_breaks = plan.breaks;
            self.uninstall_orphans = plan.orphans;
        }
        self.uninstall_expected_lock = Some(lock);
        self.confirm_uninstall = true;
        Update::none()
    }

    pub(super) fn uninstall_installed(&mut self) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        let Some(specifier) = self.installed_open.as_ref().map(|p| p.specifier.clone()) else {
            return Update::none();
        };
        let Some(expected) = self.uninstall_expected_lock.as_ref() else {
            self.confirm_uninstall = false;
            self.manage_feedback = Some(crate::i18n::t!("package-install-plan-changed"));
            return Update::none();
        };
        let remove_orphans = !self.uninstall_orphans.is_empty();
        let outcome = match shared_packages::commit_uninstall_if_unchanged(
            &self.server_name,
            expected,
            &specifier,
            remove_orphans,
        ) {
            Ok(shared_packages::UninstallCommit::Stale) => {
                self.confirm_uninstall = false;
                self.uninstall_expected_lock = None;
                self.uninstall_breaks.clear();
                self.uninstall_orphans.clear();
                self.manage_feedback = Some(crate::i18n::t!("package-install-plan-changed"));
                self.graph_seq.bump();
                return Update::with_task(Task::done(Message::LoadInstalledPackages));
            }
            Ok(outcome) => outcome,
            Err(error) => {
                self.manage_feedback = Some(crate::i18n::t!(
                    "package-uninstall-failed",
                    "error" => error.to_string()
                ));
                return Update::none();
            }
        };
        let (survives, also_removed) = match outcome {
            shared_packages::UninstallCommit::DirectInstallRemoved => (true, Vec::new()),
            shared_packages::UninstallCommit::PackagesRemoved(removed) => (
                false,
                removed
                    .into_iter()
                    .filter(|removed| removed != &specifier)
                    .map(|removed| package_display_name(&removed).to_string())
                    .collect(),
            ),
            shared_packages::UninstallCommit::Stale => unreachable!("handled above"),
        };
        self.confirm_uninstall = false;
        self.uninstall_expected_lock = None;
        self.uninstall_breaks.clear();
        self.uninstall_orphans.clear();
        self.graph_seq.bump();
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
            crate::i18n::t!("package-removed-standalone-toast", "name" => name)
        } else if also_removed.is_empty() {
            crate::i18n::t!("package-uninstalled-toast", "name" => name)
        } else {
            crate::i18n::t!(
                "package-uninstalled-with",
                "name" => name,
                "dependencies" => also_removed.join(", ")
            )
        });
        Update::new(
            Task::batch([Task::done(Message::LoadInstalledPackages), toast]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    pub(super) fn start_fork_package(&mut self) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        let Some(source) = self.installed_open.as_deref() else {
            self.manage_feedback = Some(crate::i18n::t!("package-no-selection"));
            return Update::none();
        };
        self.fork_source_specifier = Some(source.specifier.clone());
        self.fork_name = Some(package_display_name(&source.specifier).to_string());
        self.manage_feedback = None;
        Update::none()
    }

    pub(super) fn fork_draft_is_for_open_package(&self) -> bool {
        self.installed_open.as_deref().is_some_and(|package| {
            self.fork_source_specifier.as_deref() == Some(package.specifier.as_str())
                && self.fork_name.is_some()
        })
    }

    pub(super) fn open_fork_name(&self) -> Option<&str> {
        self.fork_draft_is_for_open_package()
            .then(|| self.fork_name.as_deref())
            .flatten()
    }

    pub(super) fn clear_fork_draft(&mut self) {
        self.fork_name = None;
        self.fork_source_specifier = None;
    }

    pub(super) fn installed_detail_ready_for_copy(&self) -> bool {
        self.installed_detail.is_some() && self.installed_readme.is_loaded()
    }

    pub(super) fn fork_installed(&mut self) -> Update<Message, Event> {
        if self.manage_busy {
            return Update::none();
        }
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        let Some(source_specifier) = self
            .installed_open
            .as_deref()
            .map(|package| package.specifier.clone())
        else {
            self.manage_feedback = Some(crate::i18n::t!("package-no-selection"));
            return Update::none();
        };
        let Some(resolved) = self.installed_detail.clone() else {
            self.manage_feedback = Some(crate::i18n::t!("package-detail-not-loaded"));
            return Update::none();
        };
        let Some(new_name) = self.open_fork_name().map(str::trim).map(str::to_string) else {
            return Update::none();
        };
        if let Err(message) = naming::validate_package_name(&new_name) {
            self.manage_feedback = Some(message);
            return Update::none();
        }
        if self
            .local_packages
            .iter()
            .any(|name| name.eq_ignore_ascii_case(&new_name))
        {
            self.manage_feedback = Some(crate::i18n::t!(
                "package-copy-name-exists",
                "name" => &new_name
            ));
            return Update::none();
        }
        let manifest: PackageManifest = match serde_json::from_value(resolved.manifest.clone()) {
            Ok(manifest) => manifest,
            Err(error) => {
                self.manage_feedback = Some(crate::i18n::t!(
                    "package-parse-manifest-failed",
                    "error" => error.to_string()
                ));
                return Update::none();
            }
        };
        // A local copy is a new independent root. If its manifest declares `requires`, resolve the
        // complete closure from an exact package-state snapshot now. Creation accepts only roots
        // that are already installed; anything that needs a new grant stays in the normal install
        // flow instead of being silently introduced by "Edit a copy".
        let requirement_context = if manifest.smudgy_requires().is_empty() {
            None
        } else {
            let (expected_lock, mut local_manifests) = match self.load_consent_resolution_state() {
                Ok(state) => state,
                Err(error) => {
                    self.manage_feedback = Some(error);
                    return Update::none();
                }
            };
            let local_specifier = specifier_for(local_packages::LOCAL_OWNER, &new_name);
            local_manifests.insert(local_specifier, manifest.clone());
            let root_edges = canonical_required_edges(
                manifest_requires_from_manifest(&manifest),
                &local_manifests,
            );
            Some((expected_lock, local_manifests, root_edges))
        };
        let Some(operation) = self
            .cloud
            .package_operations
            .try_acquire(&self.server_name, &new_name)
        else {
            self.manage_feedback = Some(crate::i18n::t!("package-operation-in-progress"));
            return Update::none();
        };
        let operation_id = operation.id();

        // A local leaf is the canonical implementation of that leaf. If a published install with
        // the requested destination name exists, its row stays as the fallback while the new local
        // row inherits its activation/update mode. With no matching row, the copy starts disabled.
        let canonical_owner = self
            .cloud
            .snapshot
            .get()
            .nickname_text()
            .unwrap_or_else(|| smudgy_core::models::local_packages::LOCAL_OWNER.to_string());

        let (_, client) = self.frozen_package_client();
        let server = self.server_name.clone();
        let result_source = source_specifier;
        let result_destination = new_name.clone();
        let result_origin = self.selection.clone();
        let result_origin_revision = self.selection_revision;
        self.manage_busy = true;
        self.fork_operation = Some(operation_id);
        self.manage_feedback = Some(crate::i18n::t!("package-copying-local", "name" => &new_name));
        Update::with_task(Task::perform(
            async move {
                let result = async {
                    let requirement_plan = if let Some((
                        expected_lock,
                        local_manifests,
                        root_edges,
                    )) = requirement_context
                    {
                        let closure = resolve_required_closure_from_edges(
                            &client,
                            local_packages::LOCAL_OWNER,
                            &new_name,
                            &manifest.version,
                            root_edges,
                            &expected_lock.packages,
                            &local_manifests,
                        )
                        .await;
                        if let Some(error) = closure
                            .conflict
                            .or(closure.needs_smudgy)
                            .or(closure.unavailable)
                        {
                            return Err(error);
                        }
                        if closure.roots.iter().any(|root| !root.already_satisfied) {
                            return Err(crate::i18n::t!("package-copy-requirements-not-ready"));
                        }
                        let required_specifiers = closure
                            .roots
                            .into_iter()
                            .map(|root| root.specifier)
                            .collect::<Vec<_>>();
                        Some((expected_lock, required_specifiers))
                    } else {
                        None
                    };
                    // The content-addressed cache is truth for bodies it holds (they were
                    // hash-verified when written), so a fork serves cache hits without
                    // touching the network and fetches only the misses.
                    let cache = PackageCache::new().ok();
                    let mut modules = Vec::new();
                    for module in &resolved.modules {
                        // Raw bytes either way, so a fork copies binary modules faithfully too.
                        let body = match cached_fork_body(cache.as_ref(), &module.content_hash) {
                            Some(body) => body,
                            None => client
                                .fetch_module_bytes(&module.content_url, &module.content_hash)
                                .await
                                .map_err(|e| e.to_string())?,
                        };
                        modules.push(LocalModule {
                            subpath: module.subpath.clone(),
                            content: body,
                        });
                    }
                    match requirement_plan {
                        Some((expected_lock, required_specifiers)) => {
                            // The requirement plan was reviewed against a lock snapshot; if it
                            // changed first, nothing was written and the copy must be re-planned.
                            match local_packages::fork_to_local_with_readme_and_existing_requirements_if_unchanged(
                                &server,
                                &new_name,
                                &manifest,
                                &modules,
                                resolved.readme.as_deref(),
                                &canonical_owner,
                                &expected_lock,
                                &required_specifiers,
                            )
                            .map_err(|e| e.to_string())?
                            {
                                Cas::Applied => {}
                                Cas::StateChanged => {
                                    return Err(crate::i18n::t!("package-install-plan-changed"));
                                }
                            }
                        }
                        None => local_packages::fork_to_local_with_readme_and_state(
                            &server,
                            &new_name,
                            &manifest,
                            &modules,
                            resolved.readme.as_deref(),
                            &canonical_owner,
                        )
                        .map_err(|e| e.to_string())?,
                    }
                    Ok::<_, String>(new_name)
                }
                .await;
                (operation.into_completion(), result)
            },
            move |(completion, result)| Message::ForkFinished {
                source_specifier: result_source.clone(),
                destination_name: result_destination.clone(),
                operation_id,
                completion,
                origin: result_origin.clone(),
                origin_revision: result_origin_revision,
                result,
            },
        ))
    }

    /// Describes a completed fork from the current package graph. The cloud download can take
    /// long enough for activation to change, so callers must classify from current state rather
    /// than retaining the state that existed when the fork started.
    fn current_fork_activation_feedback(&mut self, name: &str) -> (String, String) {
        if let Ok(lock) = shared_packages::load_lock(&self.server_name) {
            self.installed_packages = lock.packages;
            self.rebuild_graph();
        }
        let local_specifier = specifier_for(local_packages::LOCAL_OWNER, name);
        let destination_referenced = self
            .installed_packages
            .iter()
            .filter(|package| package.specifier != local_specifier)
            .map(|package| package.specifier.as_str())
            .chain(
                self.graph
                    .requires
                    .keys()
                    .filter(|specifier| specifier.as_str() != local_specifier)
                    .map(String::as_str),
            )
            .chain(
                self.graph
                    .requires
                    .values()
                    .flatten()
                    .map(|edge| edge.specifier.as_str()),
            )
            .any(|specifier| naming::names_conflict(package_display_name(specifier), name));
        let activation = if self.leaf_change_affects_running(name) {
            ForkActivation::OverrideActive
        } else if destination_referenced {
            ForkActivation::OverrideInactive
        } else {
            ForkActivation::Independent
        };
        match activation {
            ForkActivation::OverrideActive => (
                crate::i18n::t!("package-fork-took-over", "name" => name),
                crate::i18n::t!("package-fork-took-over-toast", "name" => name),
            ),
            ForkActivation::OverrideInactive => (
                crate::i18n::t!("package-fork-mirrored", "name" => name),
                crate::i18n::t!("package-fork-mirrored-toast", "name" => name),
            ),
            ForkActivation::Independent => (
                crate::i18n::t!("package-fork-inactive", "name" => name),
                crate::i18n::t!("package-fork-inactive-toast", "name" => name),
            ),
        }
    }

    pub(super) fn fork_finished(
        &mut self,
        source_specifier: &str,
        destination_name: &str,
        operation_id: PackageOperationId,
        origin: &Selection,
        origin_revision: u64,
        result: Result<String, String>,
    ) -> Update<Message, Event> {
        let owns_operation = self.fork_operation == Some(operation_id);
        let present_result = owns_operation
            && self.pending_nav.is_none()
            && !self.has_content_draft()
            && self.rename_buffer.is_none()
            && self.selection == *origin
            && self.selection_revision == origin_revision
            && self
                .installed_open
                .as_deref()
                .is_some_and(|package| package.specifier == source_specifier)
            && self.fork_source_specifier.as_deref() == Some(source_specifier)
            && self
                .fork_name
                .as_deref()
                .is_some_and(|name| name.trim() == destination_name);
        if owns_operation {
            self.fork_operation = None;
            self.manage_busy = false;
        }
        match result {
            Ok(name) => {
                let (feedback, toast) = self.current_fork_activation_feedback(&name);
                if owns_operation {
                    self.clear_fork_draft();
                }
                let mut tasks = vec![
                    Task::done(Message::LoadLocalPackages),
                    Task::done(Message::LoadInstalledPackages),
                ];
                if present_result {
                    let opened = self.open_owned_package(name);
                    self.authoring_feedback = Some(feedback);
                    tasks.extend([opened.task, self.show_toast(toast)]);
                }
                // Reload conservatively after every committed fork. An apparently disabled copy
                // can still replace an imported same-leaf package in another active root.
                Update::new(
                    Task::batch(tasks),
                    Some(Event::ScriptsChanged {
                        server_name: self.server_name.clone(),
                    }),
                )
            }
            Err(e) => {
                if present_result {
                    self.manage_feedback = Some(crate::i18n::t!(
                        "package-fork-failed",
                        "error" => e.to_string()
                    ));
                }
                // An error can occur after the final directory becomes visible but before its
                // state can be classified. Reconcile both views and runtime conservatively.
                Update::new(
                    Task::batch([
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]),
                    Some(Event::ScriptsChanged {
                        server_name: self.server_name.clone(),
                    }),
                )
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
                return Update::with_task(self.show_toast(crate::i18n::t!(
                    "package-folder-locate-failed",
                    "error" => e.to_string()
                )));
            }
        };
        if !dir.exists() {
            return Update::with_task(self.show_toast(crate::i18n::t!("package-folder-missing")));
        }
        if let Err(e) = open::that(&dir) {
            return Update::with_task(self.show_toast(crate::i18n::t!(
                "package-folder-open-failed",
                "error" => e.to_string()
            )));
        }
        Update::none()
    }

    pub(super) fn start_rename_owned(&mut self) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
        if !matches!(&self.publication_status, PublicationStatus::Unpublished) {
            self.authoring_feedback =
                Some(crate::i18n::t!("package-rename-publication-not-confirmed"));
            return Update::none();
        }
        if let Some(package) = self.local_package.as_deref() {
            if self
                .cloud
                .package_operations
                .is_busy(&self.server_name, &package.name)
            {
                self.authoring_feedback = Some(crate::i18n::t!("package-operation-in-progress"));
                return Update::none();
            }
            self.rename_source_name = Some(package.name.clone());
            self.rename_buffer = Some(package.name.clone());
            self.authoring_feedback = None;
        }
        Update::none()
    }

    pub(super) fn rename_draft_is_for_open_package(&self) -> bool {
        self.local_package.as_deref().is_some_and(|package| {
            self.rename_source_name.as_deref() == Some(package.name.as_str())
                && self.rename_buffer.is_some()
        })
    }

    pub(super) fn open_rename_buffer(&self) -> Option<&str> {
        self.rename_draft_is_for_open_package()
            .then(|| self.rename_buffer.as_deref())
            .flatten()
    }

    pub(super) fn clear_rename_draft(&mut self) {
        self.rename_buffer = None;
        self.rename_source_name = None;
    }

    /// Commits the inline rename: rename the folder (+ its fork sidecar), then migrate any lockfile
    /// install of its `smudgy://<you>/<name>` specifier so an active local package keeps resolving
    /// under its new name. Renaming a fork off the source's name is also what unblocks publishing.
    pub(super) fn commit_rename_owned(&mut self) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
        if let Some(error) = self.package_state_error() {
            self.authoring_feedback = Some(error);
            return Update::none();
        }
        if !matches!(&self.publication_status, PublicationStatus::Unpublished) {
            self.authoring_feedback =
                Some(crate::i18n::t!("package-rename-publication-not-confirmed"));
            return Update::none();
        }
        if self.dirty || self.manifest_dirty {
            self.authoring_feedback = Some(crate::i18n::t!("package-save-before-rename"));
            return Update::none();
        }
        let Some(old_name) = self.local_package.as_deref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        if self.rename_source_name.as_deref() != Some(old_name.as_str()) {
            return Update::none();
        }
        let Some(new_name) = self.rename_buffer.as_ref().map(|s| s.trim().to_string()) else {
            return Update::none();
        };
        if new_name == old_name {
            self.clear_rename_draft();
            return Update::none();
        }
        if let Err(message) = naming::validate_package_name(&new_name) {
            self.authoring_feedback = Some(message);
            return Update::none();
        }
        let Some(_operation) = self.reserve_package_operation(&old_name, false) else {
            return Update::none();
        };
        let session_changed =
            match local_packages::rename_local_package(&self.server_name, &old_name, &new_name) {
                Ok(changed) => changed,
                Err(e) => {
                    self.authoring_feedback = Some(crate::i18n::t!(
                        "package-rename-failed",
                        "error" => e.to_string()
                    ));
                    return Update::new(
                        Task::batch([
                            Task::done(Message::LoadLocalPackages),
                            Task::done(Message::LoadInstalledPackages),
                        ]),
                        Some(Event::ScriptsChanged {
                            server_name: self.server_name.clone(),
                        }),
                    );
                }
            };

        self.clear_rename_draft();
        self.authoring_feedback = None;
        let toast = self.show_toast(crate::i18n::t!(
            "package-renamed",
            "name" => &new_name
        ));
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
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
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
        if let Some(error) = self.package_state_error() {
            self.manage_feedback = Some(error);
            return Update::none();
        }
        let Some(expected_package) = self.installed_open.as_deref().cloned() else {
            return Update::none();
        };
        let specifier = expected_package.specifier.clone();
        self.confirm_trust = false;
        match shared_packages::set_governing_trusted_if_unchanged(
            &self.server_name,
            &specifier,
            &expected_package,
            trusted,
        ) {
            Ok(Cas::Applied) => {}
            Ok(Cas::StateChanged) => {
                self.refresh_local_shadow_after_authoritative_mutation();
                if let Err(message) = self.reload_package_lock_snapshot() {
                    self.manage_feedback = Some(message);
                } else {
                    self.manage_feedback = Some(crate::i18n::t!("package-settings-state-changed"));
                }
                return Update::with_task(Task::batch([
                    Task::done(Message::LoadLocalPackages),
                    Task::done(Message::LoadInstalledPackages),
                ]));
            }
            Err(e) => {
                self.refresh_local_shadow_after_authoritative_mutation();
                self.manage_feedback = Some(crate::i18n::t!(
                    "package-trust-update-failed",
                    "error" => e.to_string()
                ));
                return Update::none();
            }
        }
        self.refresh_local_shadow_after_authoritative_mutation();
        if let Err(message) = self.reload_package_lock_snapshot() {
            self.manage_feedback = Some(message);
        }
        // A trusted package runs allow-all, so any pending update delta is moot.
        if trusted {
            self.update_delta = None;
        }
        let name = package_display_name(&specifier).to_string();
        let toast = self.show_toast(if trusted {
            crate::i18n::t!("package-unsandboxed-toast", "name" => &name)
        } else {
            crate::i18n::t!("package-sandboxed-toast", "name" => &name)
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
        self.local_package_tab = LocalPackageTab::Manifest;
        self.manifest_tab = ManifestTab::Capabilities;
        update
    }

    /// Toggle "develop unsandboxed" for the open local package — the author-only escape hatch that
    /// runs it allow-all on the main isolate (the `trusted` flag), for capabilities a sandbox can
    /// never grant (`ffi`/`run`). Enabling installs + enables the package's own specifier and trusts
    /// it; disabling returns it to its manifest-scoped sandbox. Reloads the live session.
    pub(super) fn set_local_unsandboxed(&mut self, unsandboxed: bool) -> Update<Message, Event> {
        self.confirm_trust = false;
        if let Some(error) = self.package_state_error() {
            self.authoring_feedback = Some(error);
            return Update::none();
        }
        let Some(name) = self.local_package.as_ref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        let own_spec = self.local_own_spec(&name);
        let mut expected_package = self
            .installed_packages
            .iter()
            .find(|package| package.specifier == own_spec)
            .cloned();
        let in_lock = expected_package.is_some();
        if unsandboxed && !in_lock {
            // Materialize the disabled governing row before changing trust.
            match shared_packages::install_package_with_activation(
                &self.server_name,
                &own_spec,
                UpdateMode::Auto,
                ProfileActivation::None,
            ) {
                Ok(()) => {}
                Err(e) => {
                    return Update::new(
                        Task::batch([
                            Task::done(Message::LoadInstalledPackages),
                            Task::done(Message::LoadLocalPackages),
                            self.show_toast(crate::i18n::t!(
                                "package-update-failed",
                                "name" => &name,
                                "error" => e.to_string()
                            )),
                        ]),
                        Some(Event::ScriptsChanged {
                            server_name: self.server_name.clone(),
                        }),
                    );
                }
            }
            if let Err(message) = self.reload_package_lock_snapshot() {
                self.authoring_feedback = Some(message);
                return Update::with_task(Task::batch([
                    Task::done(Message::LoadInstalledPackages),
                    Task::done(Message::LoadLocalPackages),
                ]));
            }
            expected_package = self
                .installed_packages
                .iter()
                .find(|package| package.specifier == own_spec)
                .cloned();
        }
        if in_lock || unsandboxed {
            let Some(expected_package) = expected_package.as_ref() else {
                self.authoring_feedback = Some(crate::i18n::t!("package-settings-state-changed"));
                return Update::with_task(Task::batch([
                    Task::done(Message::LoadInstalledPackages),
                    Task::done(Message::LoadLocalPackages),
                ]));
            };
            match shared_packages::set_governing_trusted_if_unchanged(
                &self.server_name,
                &own_spec,
                expected_package,
                unsandboxed,
            ) {
                Ok(Cas::Applied) => {}
                Ok(Cas::StateChanged) => {
                    if let Err(message) = self.reload_package_lock_snapshot() {
                        self.authoring_feedback = Some(message);
                    } else {
                        self.authoring_feedback =
                            Some(crate::i18n::t!("package-settings-state-changed"));
                    }
                    return Update::with_task(Task::batch([
                        Task::done(Message::LoadInstalledPackages),
                        Task::done(Message::LoadLocalPackages),
                    ]));
                }
                Err(e) => {
                    return Update::with_task(self.show_toast(crate::i18n::t!(
                        "package-update-failed",
                        "name" => &name,
                        "error" => e.to_string()
                    )));
                }
            }
        }
        let toast = self.show_toast(if unsandboxed {
            crate::i18n::t!("package-local-unsandboxed-toast", "name" => &name)
        } else {
            crate::i18n::t!("package-local-sandboxed-toast", "name" => &name)
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

    /// Review an offered update through the complete install planner. Permission additions and
    /// changed `requires` roots share this path so neither can bypass range checks, required-root
    /// consent, exact version staging, or atomic lockfile commit.
    pub(super) fn grant_update(&mut self) -> Update<Message, Event> {
        let Some(delta) = self.update_delta.as_ref() else {
            return Update::none();
        };
        // A version-floor hold-back has no grant (its card offers only dismissal): consenting
        // wouldn't load the held-back version, so don't rewrite the baseline.
        if delta.needs_smudgy.is_some() {
            return Update::none();
        }
        let Some(open) = self.installed_open.as_deref() else {
            return Update::none();
        };
        if open.specifier != delta.specifier {
            return Update::none();
        }
        let mode = update_grant_mode(open, delta);
        self.begin_installed_version_change(mode)
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
    /// Resolve a local manifest's new independent-root set before writing the file. Removing every
    /// requirement is a synchronous empty-plan commit; additions/range changes open the same
    /// all-or-nothing consent card used by installs and version changes.
    pub(super) fn begin_local_manifest_requirements_save(
        &mut self,
        name: String,
        manifest: PackageManifest,
        json: String,
        expected_manifest: String,
        operation: PackageOperationPermit,
    ) -> Update<Message, Event> {
        if !self
            .local_package
            .as_deref()
            .is_some_and(|package| package.name == name)
        {
            if let Some(draft) = self.manifest_draft.as_mut() {
                draft.error = Some(crate::i18n::t!("manifest-changed-outside"));
            }
            return Update::none();
        }
        match local_packages::read_local_file(&self.server_name, &name, "smudgy.package.json") {
            Ok(current) if current == expected_manifest => {}
            Ok(_) => {
                if let Some(draft) = self.manifest_draft.as_mut() {
                    draft.error = Some(crate::i18n::t!("manifest-changed-outside"));
                }
                return Update::none();
            }
            Err(error) => {
                if let Some(draft) = self.manifest_draft.as_mut() {
                    draft.error = Some(crate::i18n::t!(
                        "manifest-save-failed",
                        "error" => error.to_string()
                    ));
                }
                return Update::none();
            }
        }
        let root_specifier = self.local_own_spec(&name);
        let materialized = local_packages::materialize_governing_local_lock_rows(
            &self.server_name,
            std::slice::from_ref(&name),
            local_packages::LOCAL_OWNER,
        );
        if let Err(error) = materialized {
            if let Some(draft) = self.manifest_draft.as_mut() {
                draft.error = Some(crate::i18n::t!(
                    "manifest-save-failed",
                    "error" => error.to_string()
                ));
            }
            return Update::none();
        }
        let (expected_lock, expected_local_manifests) = match self.load_consent_resolution_state() {
            Ok(state) => state,
            Err(error) => {
                if let Some(draft) = self.manifest_draft.as_mut() {
                    draft.error = Some(error);
                }
                return Update::none();
            }
        };
        let Some(activation) = expected_lock
            .find(&root_specifier)
            .map(LockedPackage::activation)
        else {
            if let Some(draft) = self.manifest_draft.as_mut() {
                draft.error = Some(crate::i18n::t!("package-install-plan-changed"));
            }
            return Update::with_task(Task::done(Message::LoadInstalledPackages));
        };

        if manifest.smudgy_requires().is_empty() {
            let result = shared_packages::commit_local_manifest_with_requirements_if_unchanged(
                &self.server_name,
                &name,
                &root_specifier,
                &expected_manifest,
                &json,
                &expected_lock,
                &[],
            );
            match result {
                Ok(shared_packages::LocalManifestCommit::Applied) => {}
                Ok(shared_packages::LocalManifestCommit::Stale) => {
                    if let Some(draft) = self.manifest_draft.as_mut() {
                        draft.error = Some(crate::i18n::t!("manifest-changed-outside"));
                    }
                    return Update::none();
                }
                Ok(shared_packages::LocalManifestCommit::StateChanged) => {
                    if let Some(draft) = self.manifest_draft.as_mut() {
                        draft.error = Some(crate::i18n::t!("package-install-plan-changed"));
                    }
                    return Update::with_task(Task::done(Message::LoadInstalledPackages));
                }
                Err(error) => {
                    if let Some(draft) = self.manifest_draft.as_mut() {
                        draft.error = Some(crate::i18n::t!(
                            "manifest-save-failed",
                            "error" => error.to_string()
                        ));
                    }
                    return Update::none();
                }
            }
            self.apply_saved_manifest(&name, manifest);
            self.package_change_finalize = Some(PackageChangeFinalize {
                specifier: root_specifier,
                activation,
                kind: PackageChangeKind::Manifest,
                warning: None,
            });
            return self.finalize_package_change();
        }

        let mut local_manifests = expected_local_manifests.clone();
        local_manifests.insert(root_specifier.clone(), manifest.clone());
        let root_edges =
            canonical_required_edges(manifest_requires_from_manifest(&manifest), &local_manifests);
        let installed = self.installed_packages.clone();
        let (account_fence, client) = self.frozen_package_client();
        let version = manifest.version.clone();
        let permissions = manifest.permissions.clone();
        self.install_seq.bump();
        let seq = self.install_seq;
        let operation_id = operation.id();
        self.authoring_busy = true;
        self.authoring_operation = Some(operation_id);
        Update::with_task(Task::perform(
            async move {
                let closure = resolve_required_closure_from_edges(
                    &client,
                    local_packages::LOCAL_OWNER,
                    &name,
                    &version,
                    root_edges,
                    &installed,
                    &local_manifests,
                )
                .await;
                let result = Ok::<_, String>(ConsentPrompt {
                    account_fence,
                    specifier: root_specifier,
                    owner: crate::i18n::t!("package-local-publisher"),
                    name: name.clone(),
                    version,
                    permissions,
                    params: Vec::new(),
                    closure: Vec::new(),
                    required_roots: closure.roots,
                    conflict: closure.conflict,
                    needs_smudgy: closure.needs_smudgy,
                    required_unavailable: closure.unavailable,
                    expected_lock,
                    expected_local_manifests,
                    operation: ConsentOperation::LocalManifest {
                        name,
                        manifest: Box::new(manifest),
                        json,
                        expected_manifest,
                        activation,
                    },
                    error: None,
                });
                (operation.into_completion(), result)
            },
            move |(completion, result)| Message::LocalManifestRequirementsResolved {
                seq,
                account_fence,
                completion,
                result,
            },
        ))
    }

    pub(super) fn local_manifest_requirements_resolved(
        &mut self,
        seq: InstallSeq,
        account_fence: AccountReadFence,
        completion: PackageOperationCompletion,
        result: Result<ConsentPrompt, String>,
    ) -> Update<Message, Event> {
        let operation_id = completion.id();
        let Some(operation) = completion.take_permit() else {
            return Update::none();
        };
        if seq != self.install_seq
            || !self.account_read_is_current(account_fence)
            || self.authoring_operation != Some(operation_id)
        {
            return Update::none();
        }
        self.authoring_operation = None;
        self.authoring_busy = false;
        self.consent_busy = false;
        match result {
            Ok(prompt) => {
                if let Err(error) = self.validate_consent_snapshot(
                    &prompt.expected_lock,
                    &prompt.expected_local_manifests,
                ) {
                    if let Some(draft) = self.manifest_draft.as_mut() {
                        draft.error = Some(error);
                    }
                    return Update::with_task(Task::batch([
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]));
                }
                let (name, expected_json) = match &prompt.operation {
                    ConsentOperation::LocalManifest { name, json, .. } => (name, json),
                    _ => return Update::none(),
                };
                let still_open = self
                    .local_package
                    .as_deref()
                    .is_some_and(|package| package.name == *name);
                let current_json = self
                    .manifest_draft
                    .as_ref()
                    .and_then(|draft| draft.to_manifest().ok())
                    .and_then(|manifest| serde_json::to_string_pretty(&manifest).ok())
                    .map(|json| format!("{json}\n"));
                if still_open && current_json.as_deref() == Some(expected_json.as_str()) {
                    self.manifest_operation = Some(operation);
                    self.consent_prompt = Some(prompt);
                }
            }
            Err(error) => {
                if let Some(draft) = self.manifest_draft.as_mut() {
                    draft.error = Some(error);
                }
            }
        }
        Update::none()
    }

    pub(super) fn new_package(&mut self) -> Update<Message, Event> {
        if let Some(error) = self.package_state_error() {
            return Update::with_task(self.show_toast(error));
        }
        self.clear_selection();
        self.selection = Selection::None;
        self.pane = Pane::NewPackage {
            name: String::new(),
            error: None,
        };
        self.local_package_tab = LocalPackageTab::Manifest;
        Update::none()
    }

    pub(super) fn open_owned_package(&mut self, name: String) -> Update<Message, Event> {
        self.clear_selection();
        self.owned_selected_file = None;
        self.owned_source_baseline = None;
        self.authoring_feedback = None;
        self.share_package_id = None;
        self.publication_status = PublicationStatus::Unknown;
        self.share_is_public = false;
        self.share_friends.clear();
        self.share_grants.clear();
        self.share_versions.clear();
        self.share_busy = false;
        self.share_feedback = None;
        self.local_package_tab = LocalPackageTab::About;
        self.parameter_profile.clone_from(&self.profile_name);
        self.selection = Selection::OwnedPackage(name.clone());
        match local_packages::load_local_package(&self.server_name, &name) {
            Ok(Some(package)) => {
                self.manifest_source_baseline = local_packages::read_local_file(
                    &self.server_name,
                    &name,
                    "smudgy.package.json",
                )
                .ok();
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

                match local_packages::load_publication_binding(&self.server_name, &name) {
                    Ok(Some(binding)) => {
                        self.share_package_id = Some(binding.package_id);
                        self.publication_status = PublicationStatus::Bound(binding.package_id);
                    }
                    Ok(None) => {
                        self.publication_status = if self.signed_in() {
                            PublicationStatus::Checking
                        } else {
                            PublicationStatus::Unknown
                        };
                    }
                    Err(error) => {
                        let message = crate::i18n::t!(
                            "package-publication-record-invalid",
                            "error" => error.to_string()
                        );
                        self.publication_status = PublicationStatus::Invalid(message.clone());
                        self.authoring_feedback = Some(message);
                    }
                }
            }
            Ok(None) => {
                self.pane = Pane::Error(std::sync::Arc::new(vec![crate::i18n::t!(
                    "package-local-not-found",
                    "name" => &name
                )]));
                return Update::none();
            }
            Err(e) => {
                self.pane = Pane::Error(std::sync::Arc::new(vec![crate::i18n::t!(
                    "package-load-failed",
                    "name" => &name,
                    "error" => e.to_string()
                )]));
                return Update::none();
            }
        }
        if self.language_project_context_matches(
            &code_editor::LanguageProjectContext::OwnedPackage(name.clone()),
        ) {
            self.language_project_target_context = Some(
                code_editor::LanguageProjectContext::OwnedPackage(name.clone()),
            );
            self.refresh_language_project();
        }
        // Load the cloud share state (if published + signed in).
        if !self.signed_in() || matches!(&self.publication_status, PublicationStatus::Invalid(_)) {
            return Update::none();
        }
        Update::with_task(self.load_owned_share(name))
    }

    /// Reload all cloud-backed state shown in an owned package pane. Publishing uses the same load
    /// as opening the pane so the new version, first-publish sharing controls, and visibility all
    /// repaint from server truth as soon as the upload completes.
    pub(super) fn load_owned_share(&mut self, name: String) -> Task<Message> {
        self.share_seq.bump();
        let seq = self.share_seq;
        let account_epoch = self.account_epoch;
        let (account_fence, frozen_credentials) = self.frozen_cloud_credentials();
        self.share_busy = true;
        let pkg_client =
            PackageApiClient::new(self.cloud.base_url.as_str(), frozen_credentials.clone());
        let cloud_client = CloudApiClient::new(self.cloud.base_url.as_str(), frozen_credentials);
        let result_name = name.clone();
        Task::perform(
            async move {
                let mine = pkg_client.list_my_packages().await?;
                let detail = mine
                    .into_iter()
                    .find(|p| naming::names_conflict(&p.package.name, &name))
                    .ok_or(CloudError::NotFoundOrNoAccess)?;
                let id = detail.package.id;
                let is_public = detail.package.is_public;
                let grants = pkg_client.list_grants(id).await?;
                let friends = cloud_client.friends().await?;
                let versions = pkg_client.list_versions(id).await?;
                Ok((id, is_public, friends, grants, versions))
            },
            move |result| Message::OwnedShareLoaded {
                account_epoch,
                account_fence,
                seq,
                name: result_name.clone(),
                result,
            },
        )
    }

    #[allow(clippy::type_complexity)]
    pub(super) fn owned_share_loaded(
        &mut self,
        seq: ShareSeq,
        name: &str,
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
        if seq != self.share_seq
            || !matches!(&self.selection, Selection::OwnedPackage(open) if open == name)
        {
            return Update::none();
        }
        self.share_busy = false;
        match result {
            Ok((id, is_public, friends, grants, versions)) => {
                if let PublicationStatus::Bound(bound_id) = &self.publication_status
                    && *bound_id != id
                {
                    let message = crate::i18n::t!(
                        "package-publication-record-conflict",
                        "local" => bound_id.to_string(),
                        "cloud" => id.to_string()
                    );
                    self.publication_status = PublicationStatus::Invalid(message.clone());
                    self.share_feedback = Some(message);
                    return Update::none();
                }
                // Backfill folders published before durable publication bindings existed. A
                // failed write cannot undo a successful cloud lookup; keep the name locked in
                // this process and explain that the local record still needs repair.
                if !matches!(&self.publication_status, PublicationStatus::Bound(_))
                    && let Err(error) =
                        local_packages::save_publication_binding(&self.server_name, name, id, name)
                {
                    self.share_feedback = Some(crate::i18n::t!(
                        "package-publication-record-save-failed",
                        "error" => error.to_string()
                    ));
                }
                self.share_package_id = Some(id);
                self.publication_status = PublicationStatus::Bound(id);
                self.share_is_public = is_public;
                self.share_friends = friends;
                self.share_grants = grants;
                self.share_versions = versions;
            }
            // A not-yet-published package simply has no cloud state.
            Err(CloudError::NotFoundOrNoAccess) => {
                if matches!(
                    self.publication_status,
                    PublicationStatus::Checking | PublicationStatus::Unknown
                ) {
                    self.share_package_id = None;
                    self.publication_status = PublicationStatus::Unpublished;
                    self.share_is_public = false;
                    self.share_friends.clear();
                    self.share_grants.clear();
                    self.share_versions.clear();
                } else if matches!(&self.publication_status, PublicationStatus::Bound(_)) {
                    self.share_feedback =
                        Some(crate::i18n::t!("package-published-state-unavailable"));
                }
            }
            Err(e) => {
                // Preserve Unknown/Bound/Invalid. A transient error must not unlock rename.
                self.share_feedback = Some(display_error(&e));
            }
        }
        Update::none()
    }

    /// Reconcile an old singleton's completed cloud mutation without navigating or replacing an
    /// unrelated package pane. Publication bindings are local authority for rename safety, so read
    /// that record even when the user changed accounts while the old task was running.
    pub(super) fn refresh_owned_share_if_open(
        &mut self,
        server_name: &str,
        name: &str,
    ) -> Update<Message, Event> {
        if self.server_name != server_name
            || !matches!(&self.selection, Selection::OwnedPackage(open) if open == name)
            || self.cloud.package_operations.is_busy(server_name, name)
        {
            return Update::none();
        }
        match local_packages::load_publication_binding(server_name, name) {
            Ok(Some(binding)) => {
                self.publication_status = PublicationStatus::Bound(binding.package_id);
                self.share_package_id = Some(binding.package_id);
            }
            Ok(None) => {}
            Err(error) => {
                let message = crate::i18n::t!(
                    "package-publication-record-invalid",
                    "error" => error.to_string()
                );
                self.publication_status = PublicationStatus::Invalid(message.clone());
                self.share_feedback = Some(message);
                return Update::none();
            }
        }
        if self.signed_in() {
            Update::with_task(self.load_owned_share(name.to_string()))
        } else {
            Update::none()
        }
    }

    pub(super) fn select_owned_file(&mut self, subpath: String) -> Update<Message, Event> {
        let Some(name) = self.local_package.as_ref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        if let Some(module) = self.local_package.as_deref().and_then(|package| {
            package
                .modules
                .iter()
                .find(|module| module.subpath == subpath)
        }) {
            let size = u64::try_from(module.content.len()).unwrap_or(u64::MAX);
            if size > SOURCE_PREVIEW_CAP_BYTES
                || module.content.contains(&0)
                || std::str::from_utf8(&module.content).is_err()
            {
                self.dirty = false;
                self.owned_selected_file = Some(subpath);
                self.owned_source_baseline = None;
                self.authoring_feedback = None;
                self.clear_code_editor();
                return Update::none();
            }
        }
        match local_packages::read_local_file(&self.server_name, &name, &subpath) {
            Ok(content) => {
                self.dirty = false;
                self.authoring_feedback = None;
                self.owned_selected_file = Some(subpath.clone());
                self.owned_source_baseline = Some(content.clone());
                return Update::with_task(self.bind_code_editor(
                    &content,
                    code_editor::path_language(&subpath),
                    code_editor::CodeDocument::OwnedPackage,
                ));
            }
            Err(e) => {
                self.authoring_feedback = Some(crate::i18n::t!(
                    "package-file-read-failed",
                    "path" => &subpath,
                    "error" => e.to_string()
                ));
            }
        }
        Update::none()
    }

    pub(super) fn save_owned_file(&mut self) -> Update<Message, Event> {
        if self.authoring_busy {
            return Update::none();
        }
        let (name, subpath) = match (
            self.local_package.as_ref().map(|p| p.name.clone()),
            self.owned_selected_file.clone(),
        ) {
            (Some(name), Some(subpath)) => (name, subpath),
            _ => return Update::none(),
        };
        if !self.code_editor_is_modified() {
            self.dirty = false;
            return Update::with_task(self.release_pending_navigation());
        }
        let content = self.code_editor_text();
        let Some(expected) = self.owned_source_baseline.clone() else {
            self.authoring_feedback = Some(crate::i18n::t!(
                "package-file-changed-outside",
                "path" => &subpath
            ));
            return Update::none();
        };
        let Some(_operation) = self.reserve_package_operation(&name, false) else {
            return Update::none();
        };
        match local_packages::write_local_file_if_unchanged(
            &self.server_name,
            &name,
            &subpath,
            &expected,
            &content,
        ) {
            Ok(local_packages::LocalFileWriteOutcome::Saved) => {}
            Ok(local_packages::LocalFileWriteOutcome::Conflict) => {
                self.authoring_feedback = Some(crate::i18n::t!(
                    "package-file-changed-outside",
                    "path" => &subpath
                ));
                return Update::none();
            }
            Err(e) => {
                self.authoring_feedback = Some(crate::i18n::t!(
                    "package-file-save-failed",
                    "error" => e.to_string()
                ));
                return Update::none();
            }
        }
        self.owned_source_baseline = Some(content.clone());
        self.dirty = false;
        let released = self.release_pending_navigation();
        self.mark_code_editor_saved();

        // The selected-file write is authoritative even if reloading an unrelated
        // package file fails. Patch the retained graph base first so closing this overlay can
        // never reveal stale source to importers, then replace it with a complete reload when
        // available.
        if let Some(package) = self
            .local_package
            .as_deref_mut()
            .filter(|package| package.name == name)
        {
            if subpath == "README.md" {
                package.readme = Some(content.clone());
            } else if let Some(module) = package
                .modules
                .iter_mut()
                .find(|module| module.subpath == subpath)
            {
                module.content = content.as_bytes().to_vec();
            } else {
                package.modules.push(local_packages::LocalModule {
                    subpath: subpath.clone(),
                    content: content.as_bytes().to_vec(),
                });
            }
        }
        match local_packages::load_local_package(&self.server_name, &name) {
            Ok(Some(package)) => {
                self.local_readme = package.readme.as_deref().map(markdown::Content::parse);
                self.local_package = Some(Box::new(package));
            }
            Ok(None) => {
                log::warn!("owned package {name} disappeared immediately after saving {subpath}");
            }
            Err(error) => {
                log::warn!(
                    "owned package file {name}/{subpath} changed on disk, but package reload failed: {error}"
                );
                self.authoring_feedback = Some(crate::i18n::t!(
                    "package-file-read-failed",
                    "path" => format!("{name}/{subpath}"),
                    "error" => error.to_string()
                ));
            }
        }
        self.refresh_language_project();
        let toast = self.show_toast(crate::i18n::t!(
            "package-file-saved",
            "path" => &subpath
        ));
        Update::new(
            Task::batch([toast, released]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    /// Reserve one local package across synchronous disk work or an entire asynchronous cloud
    /// workflow. The gate belongs to `CloudAccount`, so replacing the singleton window cannot
    /// orphan its protection while an old task is still running.
    pub(super) fn reserve_package_operation(
        &mut self,
        name: &str,
        sharing_feedback: bool,
    ) -> Option<PackageOperationPermit> {
        let operation = self
            .cloud
            .package_operations
            .try_acquire(&self.server_name, name);
        if operation.is_none() {
            let message = crate::i18n::t!("package-operation-in-progress");
            if sharing_feedback {
                self.share_feedback = Some(message);
            } else {
                self.authoring_feedback = Some(message);
            }
        }
        operation
    }

    fn begin_sharing_operation(
        &mut self,
        name: &str,
    ) -> Option<(PackageOperationPermit, PackageApiClient, u64)> {
        let operation = self.reserve_package_operation(name, true)?;
        let credential_generation = self.cloud.credentials.generation();
        let Some(credential) = self.cloud.credentials.get() else {
            self.share_feedback = Some(crate::i18n::t!("package-publish-sign-in"));
            return None;
        };
        if self.cloud.credentials.generation() != credential_generation {
            self.share_feedback = Some(crate::i18n::t!("package-account-changed"));
            return None;
        }
        let client = PackageApiClient::new(
            self.cloud.base_url.as_str(),
            smudgy_cloud::CredentialSource::new(Some(credential)),
        );
        Some((operation, client, credential_generation))
    }

    /// Validate a sharing mutation completion without allowing an older message to release or
    /// repaint a newer operation. All mutation results, including errors, can follow a committed
    /// server write, so a stale result schedules a fresh load when this package is still open.
    fn accept_sharing_completion(
        &mut self,
        server_name: &str,
        name: &str,
        seq: ShareSeq,
        package_id: Uuid,
        operation_id: PackageOperationId,
        credential_generation: u64,
    ) -> Result<(), Update<Message, Event>> {
        if self.server_name != server_name {
            return Err(Update::none());
        }
        let owns_operation = self.share_operation == Some(operation_id);
        let exact_view =
            owns_operation && seq == self.share_seq && self.share_package_id == Some(package_id);
        if owns_operation {
            self.share_operation = None;
        }
        if !exact_view {
            let same_package_open = matches!(
                &self.selection,
                Selection::OwnedPackage(open) if open == name
            );
            let newer_mutation_active = self.cloud.package_operations.is_busy(server_name, name);
            return Err(
                if same_package_open && self.signed_in() && !newer_mutation_active {
                    Update::with_task(self.load_owned_share(name.to_string()))
                } else {
                    Update::none()
                },
            );
        }

        self.share_busy = false;
        if self.cloud.credentials.generation() != credential_generation {
            self.share_package_id = None;
            self.share_is_public = false;
            self.share_versions.clear();
            self.share_grants.clear();
            self.share_feedback = Some(crate::i18n::t!("package-account-changed"));
            return Err(
                if self.signed_in() && !self.cloud.package_operations.is_busy(server_name, name) {
                    Update::with_task(self.load_owned_share(name.to_string()))
                } else {
                    Update::none()
                },
            );
        }
        Ok(())
    }

    pub(super) fn publish_owned(&mut self) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
        if self.dirty || self.manifest_dirty {
            self.authoring_feedback = Some(crate::i18n::t!("package-save-before-publish"));
            return Update::none();
        }
        let Some(name) = self.local_package.as_ref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        let account = self.cloud.snapshot.get();
        let Some(publisher) = account
            .profile
            .as_ref()
            .filter(|_| account.signed_in)
            .cloned()
        else {
            self.authoring_feedback = Some(crate::i18n::t!("package-publish-sign-in"));
            return Update::none();
        };
        if publisher.nickname.is_none() {
            self.authoring_feedback = Some(crate::i18n::t!("package-publish-sign-in"));
            return Update::none();
        }
        let credential_generation = self.cloud.credentials.generation();
        let Some(credential) = self.cloud.credentials.get() else {
            self.authoring_feedback = Some(crate::i18n::t!("package-publish-sign-in"));
            return Update::none();
        };
        if self.cloud.credentials.generation() != credential_generation {
            self.authoring_feedback = Some(crate::i18n::t!("package-account-changed"));
            return Update::none();
        }
        let Some(operation) = self.reserve_package_operation(&name, false) else {
            return Update::none();
        };
        let operation_id = operation.id();
        self.authoring_busy = true;
        self.authoring_operation = Some(operation_id);
        self.authoring_feedback = None;
        // Shown live beside Publish while the (possibly slow) tsc declaration pass + upload run;
        // the outcome — including any non-fatal tsc warnings — lands in `PublishFinished`.
        self.publish_output = Some(PublishOutput {
            package: name.clone(),
            text: format!(
                "smudgy> publish {name}\n{}",
                crate::i18n::t!("package-publishing-progress", "name" => &name)
            ),
        });
        // Namespace claim, binding, metadata, and version upload are one irreversible workflow.
        // Give it a detached credential so a login/logout cannot switch authors between awaits.
        let client = PackageApiClient::new(
            self.cloud.base_url.as_str(),
            smudgy_cloud::CredentialSource::new(Some(credential)),
        );
        let server = self.server_name.clone();
        let result_server = server.clone();
        let result_name = name.clone();
        let result_publisher_id = publisher.id;
        Update::with_task(Task::perform(
            async move {
                let result =
                    local_packages::publish_local_package(&client, &server, &name, &publisher)
                        .await
                        .map_err(|e| e.to_string());
                (operation.into_completion(), result)
            },
            move |(completion, result)| Message::PublishFinished {
                server_name: result_server.clone(),
                name: result_name.clone(),
                operation_id,
                completion,
                credential_generation,
                publisher_id: result_publisher_id,
                result,
            },
        ))
    }

    pub(super) fn delete_owned(&mut self) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
        if let Some(error) = self.package_state_error() {
            self.authoring_feedback = Some(error);
            return Update::none();
        }
        let Some(name) = self.local_package.as_ref().map(|p| p.name.clone()) else {
            return Update::none();
        };
        let Some(_operation) = self.reserve_package_operation(&name, false) else {
            return Update::none();
        };
        let deleted = match local_packages::delete_local_package(&self.server_name, &name) {
            Ok(summary) => summary,
            Err(e) => {
                self.authoring_feedback = Some(crate::i18n::t!(
                    "package-delete-failed",
                    "error" => e.to_string()
                ));
                return Update::new(
                    Task::batch([
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]),
                    Some(Event::ScriptsChanged {
                        server_name: self.server_name.clone(),
                    }),
                );
            }
        };
        self.confirm_delete_local = false;
        self.local_package = None;
        // Drop the manifest draft too (delete doesn't route through clear_selection), so a dirty
        // draft for the now-deleted package can't trip the unsaved-changes guard on the next nav.
        self.manifest_draft = None;
        self.owned_source_baseline = None;
        self.manifest_source_baseline = None;
        self.manifest_dirty = false;
        self.manifest_editing = false;
        self.clear_code_editor();
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        let toast = if deleted.warnings.is_empty() {
            self.show_toast(crate::i18n::t!(
                "package-deleted-toast",
                "name" => &name
            ))
        } else {
            self.show_toast(crate::i18n::t!(
                "package-deleted-with-warning-toast",
                "name" => &name,
                "error" => deleted.warnings.join(" ")
            ))
        };
        // Reload same-server sessions like an uninstall does: the deleted package stops running
        // and the engine rebuild prunes its now-orphaned `.isolates/<slug>` scratch dir.
        // Re-read the installed list + re-resolve the graph too: deleting a local package can change
        // which installed rows are shadowed by a local override, so the installed pane must refresh
        // rather than keep showing a now-stale view until the next manual Reload.
        let tasks = vec![
            Task::done(Message::LoadLocalPackages),
            Task::done(Message::LoadInstalledPackages),
            toast,
        ];
        Update::new(
            Task::batch(tasks),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    /// Applies the outcome of an async stale account-install sweep. A row can still be running
    /// from its verified cache when the registry no longer exposes it, so a committed prune must
    /// rebuild every live session for this server as well as refresh the installed list.
    pub(super) fn stale_account_installs_checked(
        &mut self,
        outcome: StaleInstallCheck,
    ) -> Update<Message, Event> {
        match outcome {
            StaleInstallCheck::Pruned(removed) => {
                if self.installed_open.as_deref().is_some_and(|package| {
                    removed
                        .iter()
                        .any(|specifier| specifier == &package.specifier)
                }) {
                    self.clear_selection();
                    self.installed_open = None;
                    self.installed_detail = None;
                    self.installed_rating = None;
                    self.selection = Selection::Dashboard;
                    self.pane = Pane::Dashboard;
                }
                Update::new(
                    Task::done(Message::LoadInstalledPackages),
                    Some(Event::ScriptsChanged {
                        server_name: self.server_name.clone(),
                    }),
                )
            }
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
        let account_snapshot = self.cloud.snapshot.get();
        if !account_snapshot.signed_in {
            return None;
        }
        let nick = account_snapshot.nickname_text()?;
        let prefix = format!("smudgy://{nick}/");
        let candidates: Vec<LockedPackage> = self
            .installed_packages
            .iter()
            .filter(|package| package.specifier.starts_with(&prefix))
            .filter(|package| {
                let name = package_display_name(&package.specifier);
                !self
                    .local_packages
                    .iter()
                    .any(|n| n.eq_ignore_ascii_case(name))
            })
            .cloned()
            .collect();
        if candidates.is_empty() {
            return None;
        }
        // Freeze the credential used by this destructive verifier. The app's normal package
        // client is hot-swappable; a request that silently crosses a logout/account switch could
        // turn a private package into NotFound and delete a valid install.
        let credentials = self.cloud.credentials.clone();
        let credential_generation = credentials.generation();
        let credential = credentials.get();
        if credentials.generation() != credential_generation {
            return None;
        }
        let client = PackageApiClient::new(
            self.cloud.base_url.as_str(),
            smudgy_cloud::CredentialSource::new(credential),
        );
        let account = self.cloud.snapshot.clone();
        let server = self.server_name.clone();
        Some(Task::perform(
            async move {
                let mut pruned = Vec::new();
                for candidate in candidates {
                    let name = package_display_name(&candidate.specifier).to_string();
                    if matches!(
                        client.resolve_package(&nick, &name, None).await,
                        Err(CloudError::NotFoundOrNoAccess)
                    ) {
                        // Re-check the folder right before the write: the package may have
                        // been (re)created since the candidate list was drawn up.
                        if credentials.generation() != credential_generation
                            || account.get().nickname_text().as_deref() != Some(nick.as_str())
                        {
                            break;
                        }
                        // Discovery is strict: an unreadable directory is uncertainty, not proof
                        // that the local shadow is absent.
                        let Ok(local_names) = local_packages::list_local_packages(&server) else {
                            continue;
                        };
                        let recreated = local_names
                            .iter()
                            .any(|local_name| local_name.eq_ignore_ascii_case(&name));
                        if !recreated
                            && shared_packages::uninstall_package_if_unchanged(&server, &candidate)
                                .unwrap_or(false)
                        {
                            pruned.push(candidate.specifier);
                        }
                    }
                }
                if pruned.is_empty() {
                    StaleInstallCheck::Unchanged
                } else {
                    StaleInstallCheck::Pruned(pruned)
                }
            },
            move |outcome| Message::StaleAccountInstallsChecked { outcome },
        ))
    }

    pub(super) fn create_package(&mut self) -> Update<Message, Event> {
        if let Some(state_error) = self.package_state_error() {
            if let Pane::NewPackage { error, .. } = &mut self.pane {
                *error = Some(state_error);
            }
            return Update::none();
        }
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
        let canonical_owner = self
            .cloud
            .snapshot
            .get()
            .nickname_text()
            .unwrap_or_else(|| local_packages::LOCAL_OWNER.to_string());
        match local_packages::scaffold_local_package_with_state(
            &self.server_name,
            &name,
            &canonical_owner,
        ) {
            Ok(()) => {}
            Err(e) => {
                if let Pane::NewPackage { error, .. } = &mut self.pane {
                    *error = Some(crate::i18n::t!(
                        "package-create-failed",
                        "error" => e.to_string()
                    ));
                }
                // A failure can follow a visible folder. Refresh both package views and runtime
                // instead of leaving a possible local shadow invisible.
                return Update::new(
                    Task::batch([
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]),
                    Some(Event::ScriptsChanged {
                        server_name: self.server_name.clone(),
                    }),
                );
            }
        }
        let opened = self.open_owned_package(name);
        self.local_package_tab = LocalPackageTab::Manifest;
        Update::new(
            Task::batch([
                Task::done(Message::LoadLocalPackages),
                Task::done(Message::LoadInstalledPackages),
                opened.task,
            ]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
    }

    pub(super) fn set_visibility(&mut self, public: bool) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        let Some(name) = self
            .local_package
            .as_deref()
            .map(|package| package.name.clone())
        else {
            return Update::none();
        };
        let Some((operation, client, credential_generation)) = self.begin_sharing_operation(&name)
        else {
            return Update::none();
        };
        let operation_id = operation.id();
        self.share_seq.bump();
        let seq = self.share_seq;
        self.share_busy = true;
        self.share_operation = Some(operation_id);
        let server_name = self.server_name.clone();
        Update::with_task(Task::perform(
            async move {
                let result = client
                    .patch_package(id, None, Some(public))
                    .await
                    .map(|view| view.is_public);
                (operation.into_completion(), result)
            },
            move |(completion, result)| Message::VisibilityUpdated {
                server_name: server_name.clone(),
                name: name.clone(),
                seq,
                package_id: id,
                operation_id,
                completion,
                credential_generation,
                result,
            },
        ))
    }

    pub(super) fn visibility_updated(
        &mut self,
        server_name: &str,
        name: &str,
        seq: ShareSeq,
        package_id: Uuid,
        operation_id: PackageOperationId,
        credential_generation: u64,
        result: Result<bool, CloudError>,
    ) -> Update<Message, Event> {
        if let Err(update) = self.accept_sharing_completion(
            server_name,
            name,
            seq,
            package_id,
            operation_id,
            credential_generation,
        ) {
            return update;
        }
        match result {
            Ok(is_public) => self.share_is_public = is_public,
            Err(e) => {
                self.share_feedback = Some(display_error(&e));
                return Update::with_task(self.load_owned_share(name.to_string()));
            }
        }
        Update::none()
    }

    pub(super) fn yank_version(&mut self, version: String, yanked: bool) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        let Some(name) = self
            .local_package
            .as_deref()
            .map(|package| package.name.clone())
        else {
            return Update::none();
        };
        let Some((operation, client, credential_generation)) = self.begin_sharing_operation(&name)
        else {
            return Update::none();
        };
        let operation_id = operation.id();
        self.share_seq.bump();
        let seq = self.share_seq;
        self.share_busy = true;
        self.share_operation = Some(operation_id);
        let server_name = self.server_name.clone();
        Update::with_task(Task::perform(
            async move {
                let result = client.set_version_yanked(id, &version, yanked).await;
                (operation.into_completion(), result)
            },
            move |(completion, result)| Message::VersionsUpdated {
                server_name: server_name.clone(),
                name: name.clone(),
                seq,
                package_id: id,
                operation_id,
                completion,
                credential_generation,
                result,
            },
        ))
    }

    pub(super) fn delete_version(&mut self, version: String) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        let Some(name) = self
            .local_package
            .as_deref()
            .map(|package| package.name.clone())
        else {
            return Update::none();
        };
        let Some((operation, client, credential_generation)) = self.begin_sharing_operation(&name)
        else {
            return Update::none();
        };
        let operation_id = operation.id();
        self.share_seq.bump();
        let seq = self.share_seq;
        self.share_busy = true;
        self.share_operation = Some(operation_id);
        let server_name = self.server_name.clone();
        Update::with_task(Task::perform(
            async move {
                let result = async {
                    client.delete_version(id, &version).await?;
                    client.list_versions(id).await
                }
                .await;
                (operation.into_completion(), result)
            },
            move |(completion, result)| Message::VersionsUpdated {
                server_name: server_name.clone(),
                name: name.clone(),
                seq,
                package_id: id,
                operation_id,
                completion,
                credential_generation,
                result,
            },
        ))
    }

    pub(super) fn versions_updated(
        &mut self,
        server_name: &str,
        name: &str,
        seq: ShareSeq,
        package_id: Uuid,
        operation_id: PackageOperationId,
        credential_generation: u64,
        result: Result<Vec<VersionListItem>, CloudError>,
    ) -> Update<Message, Event> {
        if let Err(update) = self.accept_sharing_completion(
            server_name,
            name,
            seq,
            package_id,
            operation_id,
            credential_generation,
        ) {
            return update;
        }
        match result {
            Ok(versions) => self.share_versions = versions,
            Err(e) => {
                self.share_feedback = Some(display_error(&e));
                return Update::with_task(self.load_owned_share(name.to_string()));
            }
        }
        Update::none()
    }

    pub(super) fn share_with_friend(&mut self, grantee: Uuid) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
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
        let Some(name) = self
            .local_package
            .as_deref()
            .map(|package| package.name.clone())
        else {
            return Update::none();
        };
        let Some((operation, client, credential_generation)) = self.begin_sharing_operation(&name)
        else {
            return Update::none();
        };
        let operation_id = operation.id();
        self.share_seq.bump();
        let seq = self.share_seq;
        self.share_busy = true;
        self.share_operation = Some(operation_id);
        let server_name = self.server_name.clone();
        Update::with_task(Task::perform(
            async move {
                let result = client.share_with_friend(id, grantee).await;
                (operation.into_completion(), result)
            },
            move |(completion, result)| Message::GrantsUpdated {
                server_name: server_name.clone(),
                name: name.clone(),
                seq,
                package_id: id,
                operation_id,
                completion,
                credential_generation,
                result,
            },
        ))
    }

    pub(super) fn revoke_grant(&mut self, grant_id: Uuid) -> Update<Message, Event> {
        if self.authoring_busy || self.share_busy {
            return Update::none();
        }
        let Some(id) = self.share_package_id else {
            return Update::none();
        };
        let Some(name) = self
            .local_package
            .as_deref()
            .map(|package| package.name.clone())
        else {
            return Update::none();
        };
        let Some((operation, client, credential_generation)) = self.begin_sharing_operation(&name)
        else {
            return Update::none();
        };
        let operation_id = operation.id();
        self.share_seq.bump();
        let seq = self.share_seq;
        self.share_busy = true;
        self.share_operation = Some(operation_id);
        let server_name = self.server_name.clone();
        Update::with_task(Task::perform(
            async move {
                let result = client.revoke_grant(id, grant_id).await;
                (operation.into_completion(), result)
            },
            move |(completion, result)| Message::GrantsUpdated {
                server_name: server_name.clone(),
                name: name.clone(),
                seq,
                package_id: id,
                operation_id,
                completion,
                credential_generation,
                result,
            },
        ))
    }

    pub(super) fn grants_updated(
        &mut self,
        server_name: &str,
        name: &str,
        seq: ShareSeq,
        package_id: Uuid,
        operation_id: PackageOperationId,
        credential_generation: u64,
        result: Result<Vec<PackageGrantView>, CloudError>,
    ) -> Update<Message, Event> {
        if let Err(update) = self.accept_sharing_completion(
            server_name,
            name,
            seq,
            package_id,
            operation_id,
            credential_generation,
        ) {
            return update;
        }
        match result {
            Ok(grants) => self.share_grants = grants,
            Err(e) => {
                self.share_feedback = Some(display_error(&e));
                return Update::with_task(self.load_owned_share(name.to_string()));
            }
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
        self.discover_owner = None;
        self.discover_requested_package = None;
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
        self.discover_search_seq.bump();
        let seq = self.discover_search_seq;
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
            move |result| Message::DiscoverResultsLoaded(seq, result),
        ))
    }

    pub(super) fn discover_results_loaded(
        &mut self,
        seq: DiscoverSearchSeq,
        result: Result<Vec<PackageSearchResult>, CloudError>,
    ) -> Update<Message, Event> {
        if seq != self.discover_search_seq || self.selection != Selection::Discover {
            return Update::none();
        }
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
        self.discover_seq.bump();
        let seq = self.discover_seq;
        self.discover_requested_package = Some(package_id);
        self.discover_busy = true;
        self.discover_error = None;
        self.discover_owner = Some(owner);
        self.discover_detail = None;
        self.discover_readme = None;
        self.discover_comments.clear();
        self.param_prompt = None;
        let (account_fence, frozen_credentials) = self.frozen_cloud_credentials();
        let detail_client =
            PackageApiClient::new(self.cloud.base_url.as_str(), frozen_credentials.clone());
        let comments_client =
            PackageApiClient::new(self.cloud.base_url.as_str(), frozen_credentials);
        Update::with_task(Task::batch([
            Task::perform(
                async move { detail_client.get_package(package_id).await },
                move |result| Message::DiscoverDetailLoaded {
                    seq,
                    package_id,
                    account_fence,
                    result,
                },
            ),
            Task::perform(
                async move { comments_client.list_comments(package_id).await },
                move |result| Message::DiscoverCommentsLoaded {
                    seq,
                    package_id,
                    account_fence,
                    result,
                },
            ),
        ]))
    }

    pub(super) fn discover_detail_loaded(
        &mut self,
        seq: DiscoverSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<PackageDetail, CloudError>,
    ) -> Update<Message, Event> {
        if seq != self.discover_seq
            || self.discover_requested_package != Some(package_id)
            || !self.account_read_is_current(account_fence)
            || self.selection != Selection::Discover
        {
            return Update::none();
        }
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
        seq: DiscoverSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<Vec<CommentView>, CloudError>,
    ) -> Update<Message, Event> {
        if seq != self.discover_seq
            || self.discover_requested_package != Some(package_id)
            || !self.account_read_is_current(account_fence)
            || self.selection != Selection::Discover
        {
            return Update::none();
        }
        if let Ok(comments) = result {
            self.discover_comments = comments;
        }
        Update::none()
    }

    pub(super) fn discover_back(&mut self) -> Update<Message, Event> {
        self.discover_seq.bump();
        self.discover_requested_package = None;
        self.discover_detail = None;
        self.discover_readme = None;
        self.discover_comments.clear();
        self.param_prompt = None;
        self.param_prompt_queue.clear();
        self.consent_prompt = None;
        self.consent_busy = false;
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
        if self.param_prompt.is_none() {
            return Update::none();
        }
        self.param_prompt = None;
        self.advance_param_prompt_queue()
    }

    pub(super) fn rate_package(&self, stars: i16) -> Update<Message, Event> {
        let Some(detail) = self.discover_detail.as_ref() else {
            return Update::none();
        };
        let package_id = detail.package.id;
        let seq = self.discover_seq;
        let (account_fence, client) = self.frozen_package_client();
        Update::with_task(Task::perform(
            async move { client.rate_package(package_id, stars).await },
            move |result| Message::RatingUpdated {
                seq,
                package_id,
                account_fence,
                result,
            },
        ))
    }

    pub(super) fn rating_updated(
        &mut self,
        seq: DiscoverSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<PackageDetail, CloudError>,
    ) -> Update<Message, Event> {
        if seq != self.discover_seq
            || self.discover_requested_package != Some(package_id)
            || !self.account_read_is_current(account_fence)
            || self.selection != Selection::Discover
            || self
                .discover_detail
                .as_deref()
                .map(|detail| detail.package.id)
                != Some(package_id)
        {
            return Update::none();
        }
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
        let seq = self.discover_seq;
        let (account_fence, client) = self.frozen_package_client();
        Update::with_task(Task::perform(
            async move { client.add_comment(package_id, &body).await },
            move |result| Message::CommentAdded {
                seq,
                package_id,
                account_fence,
                result,
            },
        ))
    }

    pub(super) fn comment_added(
        &mut self,
        seq: DiscoverSeq,
        package_id: Uuid,
        account_fence: AccountReadFence,
        result: Result<CommentView, CloudError>,
    ) -> Update<Message, Event> {
        if seq != self.discover_seq
            || self.discover_requested_package != Some(package_id)
            || !self.account_read_is_current(account_fence)
            || self.selection != Selection::Discover
        {
            return Update::none();
        }
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
        if let Some(error) = self.package_state_error() {
            self.discover_busy = false;
            self.discover_error = Some(error);
            return Update::none();
        }
        let (expected_lock, local_manifests) = match self.load_consent_resolution_state() {
            Ok(state) => state,
            Err(error) => {
                self.discover_busy = false;
                self.discover_error = Some(error);
                return Update::none();
            }
        };
        if let Some(local_name) = local_manifests.keys().find_map(|specifier| {
            let local_name = package_display_name(specifier);
            naming::names_conflict(local_name, &name).then(|| local_name.to_string())
        }) {
            // A local leaf is canonical for every author. Installing a hidden remote fallback here
            // would offer a choice the resolver no longer supports, so open the implementation that
            // will actually run instead.
            self.install_seq.bump();
            self.discover_busy = false;
            return self.open_owned_package(local_name);
        }
        self.discover_busy = true;
        self.discover_error = None;
        // New install generation: a result tagged with this seq is honored only if nothing has
        // abandoned the install (navigation, Back, or another install) in the meantime.
        self.install_seq.bump();
        let seq = self.install_seq;
        let (account_fence, client) = self.frozen_package_client();
        // Resolve the root, fold the whole dependency-closure permission union, AND walk the
        // `requires`-closure (required roots + peer-conflict check) before showing
        // the consent window — the sandboxed isolate is granted exactly that union, and the user
        // grants the whole required set at once.
        let installed = expected_lock.packages;
        Update::with_task(Task::perform(
            async move {
                resolve_install_closure(&client, &owner, &name, None, &installed, &local_manifests)
                    .await
            },
            move |result| Message::InstallResolved(seq, account_fence, result),
        ))
    }

    pub(super) fn install_resolved(
        &mut self,
        seq: InstallSeq,
        account_fence: AccountReadFence,
        result: Result<InstallResolution, CloudError>,
    ) -> Update<Message, Event> {
        // Discard a stale resolve: the user navigated away, hit Back, or started another install
        // while this one was in flight, so the consent window would be orphaned.
        if seq != self.install_seq || !self.account_read_is_current(account_fence) {
            return Update::none();
        }
        self.discover_busy = false;
        self.consent_busy = false;
        match result {
            Ok(res) => {
                if let Err(error) = self
                    .validate_consent_snapshot(&res.expected_lock, &res.expected_local_manifests)
                {
                    self.discover_error = Some(error);
                    return Update::with_task(Task::batch([
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]));
                }
                // The Install Confirmation window is ALWAYS shown before a lock entry is written,
                // even for a zero-permission package. Nothing is persisted yet.
                self.consent_prompt = Some(ConsentPrompt {
                    account_fence,
                    specifier: res.specifier,
                    owner: res.owner,
                    name: res.name,
                    version: res.version,
                    permissions: res.permissions,
                    params: res.params,
                    closure: res.closure,
                    required_roots: res.required_roots,
                    conflict: res.conflict,
                    needs_smudgy: res.needs_smudgy,
                    required_unavailable: res.required_unavailable,
                    expected_lock: res.expected_lock,
                    expected_local_manifests: res.expected_local_manifests,
                    operation: ConsentOperation::Install,
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

    /// Starts an accepted package change by hash-verifying and caching every exact published code
    /// resolution first. No lock state changes until [`Self::consent_cache_prepared`] receives a
    /// complete cache-authority set for the still-current prompt.
    pub(super) fn consent_grant(&mut self, enable: bool) -> Update<Message, Event> {
        let Some((expected_lock, expected_local_manifests, prompt_account_fence)) =
            self.consent_prompt.as_ref().map(|prompt| {
                (
                    prompt.expected_lock.clone(),
                    prompt.expected_local_manifests.clone(),
                    prompt.account_fence,
                )
            })
        else {
            return Update::none();
        };
        if !self.account_read_is_current(prompt_account_fence) {
            self.consent_prompt = None;
            self.consent_busy = false;
            self.discover_error = Some(crate::i18n::t!("package-account-review-changed"));
            return Update::none();
        }
        if let Err(error) =
            self.validate_consent_snapshot(&expected_lock, &expected_local_manifests)
        {
            if let Some(prompt) = self.consent_prompt.as_mut() {
                prompt.error = Some(error);
            }
            return Update::with_task(Task::batch([
                Task::done(Message::LoadLocalPackages),
                Task::done(Message::LoadInstalledPackages),
            ]));
        }
        let Some(prompt) = self.consent_prompt.as_ref() else {
            return Update::none();
        };
        if self.consent_busy {
            return Update::none();
        }
        // A peer conflict or version-floor refusal is unresolvable from here — refuse rather
        // than install a broken set (the view disables the grant buttons for both).
        if prompt.conflict.is_some()
            || prompt.needs_smudgy.is_some()
            || prompt.required_unavailable.is_some()
        {
            return Update::none();
        }
        let root =
            (!matches!(prompt.operation, ConsentOperation::LocalManifest { .. })).then(|| {
                ConsentCacheTarget {
                    specifier: prompt.specifier.clone(),
                    version: prompt.version.clone(),
                    closure: prompt.closure.clone(),
                }
            });
        let required = prompt
            .required_roots
            .iter()
            .filter(|required| !required.already_satisfied)
            .filter(|required| {
                parse_specifier(&required.specifier).is_some_and(|(owner, _)| {
                    !owner.eq_ignore_ascii_case(local_packages::LOCAL_OWNER)
                })
            })
            .map(|required| ConsentCacheTarget {
                specifier: required.specifier.clone(),
                version: required.version.clone(),
                closure: required.closure.clone(),
            })
            .collect::<Vec<_>>();
        self.consent_busy = true;
        if let Some(prompt) = self.consent_prompt.as_mut() {
            prompt.error = None;
        }
        let seq = self.install_seq;
        let (account_fence, client) = self.frozen_package_client();
        if account_fence != prompt_account_fence {
            self.consent_busy = false;
            self.consent_prompt = None;
            self.discover_error = Some(crate::i18n::t!("package-account-review-changed"));
            return Update::none();
        }
        Update::with_task(Task::perform(
            prepare_consent_cache(client, root, required),
            move |result| Message::ConsentCachePrepared {
                seq,
                account_fence,
                enable,
                result,
            },
        ))
    }

    pub(super) fn consent_cache_prepared(
        &mut self,
        seq: InstallSeq,
        account_fence: AccountReadFence,
        enable: bool,
        result: Result<PreparedConsentCache, String>,
    ) -> Update<Message, Event> {
        if seq != self.install_seq {
            return Update::none();
        }
        if !self.account_read_is_current(account_fence)
            || self
                .consent_prompt
                .as_ref()
                .is_none_or(|prompt| prompt.account_fence != account_fence)
        {
            self.consent_busy = false;
            self.consent_prompt = None;
            return Update::none();
        }
        self.consent_busy = false;
        match result {
            Ok(cache) => self.commit_consent_grant(enable, cache),
            Err(error) => {
                if let Some(prompt) = self.consent_prompt.as_mut() {
                    prompt.error = Some(crate::i18n::t!(
                        "package-install-failed",
                        "error" => error
                    ));
                }
                Update::none()
            }
        }
    }

    /// Commits the still-current, cache-complete consent plan in one lockfile transaction, then
    /// chains required-parameter prompts across the changed roots.
    fn commit_consent_grant(
        &mut self,
        enable: bool,
        cache: PreparedConsentCache,
    ) -> Update<Message, Event> {
        let Some((expected_lock, expected_local_manifests, account_fence)) =
            self.consent_prompt.as_ref().map(|prompt| {
                (
                    prompt.expected_lock.clone(),
                    prompt.expected_local_manifests.clone(),
                    prompt.account_fence,
                )
            })
        else {
            return Update::none();
        };
        if !self.account_read_is_current(account_fence) {
            self.consent_prompt = None;
            return Update::none();
        }
        if let Err(error) =
            self.validate_consent_snapshot(&expected_lock, &expected_local_manifests)
        {
            if let Some(prompt) = self.consent_prompt.as_mut() {
                prompt.error = Some(error);
            }
            return Update::with_task(Task::batch([
                Task::done(Message::LoadLocalPackages),
                Task::done(Message::LoadInstalledPackages),
            ]));
        }
        let Some(prompt) = self.consent_prompt.as_ref() else {
            return Update::none();
        };
        if prompt.conflict.is_some()
            || prompt.needs_smudgy.is_some()
            || prompt.required_unavailable.is_some()
        {
            return Update::none();
        }
        let specifier = prompt.specifier.clone();
        let name = prompt.name.clone();
        let version = prompt.version.clone();
        let permissions = prompt.permissions.clone();
        let params = prompt.params.clone();
        let operation = prompt.operation.clone();
        if matches!(&operation, ConsentOperation::LocalManifest { .. })
            && self.manifest_operation.is_none()
        {
            if let Some(prompt) = self.consent_prompt.as_mut() {
                prompt.error = Some(crate::i18n::t!("manifest-operation-expired"));
            }
            return Update::none();
        }
        if let ConsentOperation::LocalManifest {
            name,
            expected_manifest,
            ..
        } = &operation
        {
            if !self
                .local_package
                .as_deref()
                .is_some_and(|package| package.name == *name)
            {
                if let Some(prompt) = self.consent_prompt.as_mut() {
                    prompt.error = Some(crate::i18n::t!("manifest-changed-outside"));
                }
                return Update::none();
            }
            match local_packages::read_local_file(&self.server_name, name, "smudgy.package.json") {
                Ok(current) if current == *expected_manifest => {}
                Ok(_) => {
                    if let Some(prompt) = self.consent_prompt.as_mut() {
                        prompt.error = Some(crate::i18n::t!("manifest-changed-outside"));
                    }
                    return Update::none();
                }
                Err(error) => {
                    if let Some(prompt) = self.consent_prompt.as_mut() {
                        prompt.error = Some(crate::i18n::t!(
                            "manifest-save-failed",
                            "error" => error.to_string()
                        ));
                    }
                    return Update::none();
                }
            }
        }
        let (activation, kind) = match &operation {
            ConsentOperation::Install => (
                ProfileActivation::from_legacy(enable),
                PackageChangeKind::Install,
            ),
            ConsentOperation::Update { activation, .. } => {
                (activation.clone(), PackageChangeKind::Update)
            }
            ConsentOperation::LocalManifest { activation, .. } => {
                (activation.clone(), PackageChangeKind::Manifest)
            }
        };
        // Only the not-already-satisfied required roots need parameter prompts after the atomic
        // install. Satisfying roots are linked but keep all of their independent state.
        let required: Vec<RequiredRoot> = prompt
            .required_roots
            .iter()
            .filter(|r| !r.already_satisfied)
            .cloned()
            .collect();
        let required_plan = prompt
            .required_roots
            .iter()
            .map(|root| shared_packages::RequiredPackageInstall {
                specifier: root.specifier.clone(),
                version: root.version.clone(),
                permissions: root.permissions.clone(),
                already_satisfied: root.already_satisfied,
            })
            .collect::<Vec<_>>();

        // A local package created while this consent window was open can take over a leaf. The
        // resolved remote plan is then stale and must be reviewed again; never grant remote
        // permissions or activation to newly-created mutable local code.
        let requirement_plan_changed = prompt.required_roots.iter().any(|root| {
            if self.governing_specifier(&root.specifier) != root.specifier {
                return true;
            }
            parse_specifier(&root.specifier).is_some_and(|(owner, name)| {
                owner.eq_ignore_ascii_case(local_packages::LOCAL_OWNER)
                    && local_packages::load_local_package(&self.server_name, &name)
                        .ok()
                        .flatten()
                        .is_none()
            })
        });
        if self.governing_specifier(&specifier) != specifier || requirement_plan_changed {
            if let Some(prompt) = self.consent_prompt.as_mut() {
                prompt.error = Some(crate::i18n::t!("package-install-plan-changed"));
            }
            return Update::none();
        }

        // Installed-root changes and a local manifest's derived relationship set each commit in
        // one lock transaction.
        let PreparedConsentCache = cache;
        let commit = match operation {
            ConsentOperation::Install => {
                shared_packages::install_package_with_requirements_if_unchanged(
                    &self.server_name,
                    &expected_lock,
                    &specifier,
                    &version,
                    &permissions,
                    UpdateMode::Auto,
                    activation.clone(),
                    &required_plan,
                )
                .map(|committed| committed.then_some(()))
            }
            ConsentOperation::Update { mode, .. } => {
                shared_packages::install_package_with_requirements_if_unchanged(
                    &self.server_name,
                    &expected_lock,
                    &specifier,
                    &version,
                    &permissions,
                    mode,
                    activation.clone(),
                    &required_plan,
                )
                .map(|committed| committed.then_some(()))
            }
            ConsentOperation::LocalManifest {
                name: local_name,
                manifest,
                json,
                expected_manifest,
                ..
            } => match shared_packages::commit_local_manifest_with_requirements_if_unchanged(
                &self.server_name,
                &local_name,
                &specifier,
                &expected_manifest,
                &json,
                &expected_lock,
                &required_plan,
            ) {
                Ok(shared_packages::LocalManifestCommit::Applied) => {
                    self.apply_saved_manifest(&local_name, *manifest);
                    Ok(Some(()))
                }
                Ok(shared_packages::LocalManifestCommit::Stale) => {
                    if let Some(prompt) = self.consent_prompt.as_mut() {
                        prompt.error = Some(crate::i18n::t!("manifest-changed-outside"));
                    }
                    return Update::none();
                }
                Ok(shared_packages::LocalManifestCommit::StateChanged) => Ok(None),
                Err(error) => Err(error),
            },
        };
        match commit {
            Ok(Some(())) => {}
            Ok(None) => {
                if let Some(prompt) = self.consent_prompt.as_mut() {
                    prompt.error = Some(crate::i18n::t!("package-install-plan-changed"));
                }
                return Update::with_task(Task::batch([
                    Task::done(Message::LoadLocalPackages),
                    Task::done(Message::LoadInstalledPackages),
                ]));
            }
            Err(e) => {
                if let Some(prompt) = self.consent_prompt.as_mut() {
                    prompt.error = Some(if matches!(kind, PackageChangeKind::Manifest) {
                        crate::i18n::t!("manifest-save-failed", "error" => e.to_string())
                    } else {
                        crate::i18n::t!("package-install-failed", "error" => e.to_string())
                    });
                }
                return Update::none();
            }
        }
        if matches!(&kind, PackageChangeKind::Manifest) {
            self.manifest_operation = None;
        }
        // The executable/relationship state is committed now. Reload immediately, before optional
        // parameter prompts, so navigation cannot discard the only reload. Missing required
        // parameters fail closed until the prompt is completed.
        self.graph_seq.bump();
        self.consent_prompt = None;
        self.package_change_finalize = Some(PackageChangeFinalize {
            specifier: specifier.clone(),
            activation,
            kind: kind.clone(),
            warning: None,
        });

        // Build the required-params prompt queue across the chosen package and every co-installed
        // required root, in install order, skipping any with no missing required params.
        let mut prompts: Vec<ParamPrompt> = Vec::new();
        if !matches!(kind, PackageChangeKind::Manifest) {
            match self.build_param_prompt(&specifier, &name, &version, &params) {
                Ok(Some(prompt)) => prompts.push(prompt),
                Ok(None) => {}
                Err(error) => {
                    if let Some(finalize) = self.package_change_finalize.as_mut() {
                        finalize.warning = Some(error);
                    }
                    return self.finalize_package_change();
                }
            }
        }
        for root in &required {
            match self.build_param_prompt(&root.specifier, &root.name, &root.version, &root.params)
            {
                Ok(Some(prompt)) => prompts.push(prompt),
                Ok(None) => {}
                Err(error) => {
                    if let Some(finalize) = self.package_change_finalize.as_mut() {
                        finalize.warning = Some(error);
                    }
                    return self.finalize_package_change();
                }
            }
        }
        if prompts.is_empty() {
            return self.finalize_package_change();
        }
        // Show the first prompt; the rest wait their turn (each submit/cancel pops the next).
        self.param_prompt = Some(prompts.remove(0));
        self.param_prompt_queue = prompts;
        Update::new(
            Task::done(Message::LoadInstalledPackages),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
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
    ) -> Result<Option<ParamPrompt>, String> {
        let lock = shared_packages::load_lock(&self.server_name).map_err(|error| {
            crate::i18n::t!(
                "package-settings-read-unavailable",
                "error" => error.to_string()
            )
        })?;
        let expected_package = lock
            .find(specifier)
            .cloned()
            .ok_or_else(|| crate::i18n::t!("package-install-plan-changed"))?;
        // A single modal cannot safely choose values for every profile. Profile-scoped packages
        // remain fail-closed until the explicit Settings checklist is completed for each active
        // profile.
        if expected_package.parameter_scope == ParameterScope::Profile {
            return Ok(None);
        }
        let missing: Vec<PackageParameter> = params
            .iter()
            .filter(|param| param.required)
            .map(|param| {
                shared_packages::param_has_value_scoped_checked(
                    &self.server_name,
                    ParamValueScope::Global,
                    specifier,
                    param,
                )
                .map(|present| (!present).then(|| param.clone()))
                .map_err(|error| {
                    crate::i18n::t!(
                        "package-settings-read-unavailable",
                        "error" => error.to_string()
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        if missing.is_empty() {
            return Ok(None);
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
        Ok(Some(ParamPrompt {
            expected_package,
            name: name.to_string(),
            version: version.to_string(),
            params: missing,
            values,
            error: None,
        }))
    }

    /// Advance the install-time param-prompt queue: show the next pending prompt if any, else run
    /// the install tail. Called when a prompt is submitted or dismissed, so a multi-package required
    /// install configures each package in turn before finishing. `finalize_specifier`/`enable` drive
    /// the closing toast + reload once the queue drains.
    fn advance_param_prompt_queue(&mut self) -> Update<Message, Event> {
        if self.param_prompt_queue.is_empty() {
            return self.finalize_package_change();
        }
        self.param_prompt = Some(self.param_prompt_queue.remove(0));
        Update::none()
    }

    pub(super) fn consent_cancel(&mut self) -> Update<Message, Event> {
        // Cancel writes nothing.
        let abandoned_version_change = self.consent_prompt_for_open_installed().is_some();
        self.consent_prompt = None;
        self.consent_busy = false;
        self.manifest_operation = None;
        self.install_seq.bump();
        if abandoned_version_change {
            // The version change fenced the pane's detail load when it began (so a late result
            // could not repaint the pane mid-review). Abandoning it owes the pane a fresh load:
            // the About/Source tabs and the update card go back to the still-current lock row.
            return self.refresh_stale_installed_detail();
        }
        Update::none()
    }

    /// The pending consent card that belongs to the open installed package: an update/pin review
    /// for exactly that specifier. An install card (Discover/Shared) or another package's card is
    /// never shown on this pane.
    pub(super) fn consent_prompt_for_open_installed(&self) -> Option<&ConsentPrompt> {
        let prompt = self.consent_prompt.as_ref()?;
        let open = self.installed_open.as_deref()?;
        (matches!(prompt.operation, ConsentOperation::Update { .. })
            && prompt.specifier == open.specifier)
            .then_some(prompt)
    }

    /// The pending consent card that belongs to the open owned (local) package: a manifest
    /// requirements review for exactly that package name.
    pub(super) fn consent_prompt_for_open_local(&self) -> Option<&ConsentPrompt> {
        let prompt = self.consent_prompt.as_ref()?;
        let open = self.local_package.as_deref()?;
        match &prompt.operation {
            ConsentOperation::LocalManifest { name, .. } if *name == open.name => Some(prompt),
            _ => None,
        }
    }

    /// The common package-change tail: every lock mutation already committed atomically. Refresh
    /// the pane and reload live sessions. Effective activation can come from a recursively active
    /// requiring parent even when this row's direct activation is `None`.
    fn finalize_package_change(&mut self) -> Update<Message, Event> {
        let Some(finalize) = self.package_change_finalize.take() else {
            return Update::none();
        };
        self.param_prompt = None;
        // The whole required set has been configured (the queue drained) — clear it defensively.
        self.param_prompt_queue.clear();
        let enabled_here = finalize.activation.is_enabled_for(&self.profile_name);
        self.graph
            .intent
            .insert(finalize.specifier.clone(), enabled_here);
        let name = package_display_name(&finalize.specifier);
        let message = finalize.warning.unwrap_or_else(|| match finalize.kind {
            PackageChangeKind::Manifest => crate::i18n::t!("manifest-saved"),
            PackageChangeKind::Update => {
                crate::i18n::t!("package-updated-toast", "name" => name)
            }
            PackageChangeKind::Install if enabled_here => {
                crate::i18n::t!("package-installed-enabled-toast", "name" => name)
            }
            PackageChangeKind::Install => {
                crate::i18n::t!("package-installed-review-toast", "name" => name)
            }
        });
        let toast = self.show_toast(message);
        let event = Some(Event::ScriptsChanged {
            server_name: self.server_name.clone(),
        });
        let mut tasks = vec![Task::done(Message::LoadInstalledPackages), toast];
        // A version change committed against the open installed package: the inventory reload
        // above never touches the open pane's detail/versions/README, and the change fenced the
        // previous detail load when it began. Re-open the row from the committed lock so
        // About/Source/Settings show the new version and mode.
        if matches!(self.pane, Pane::InstalledPackage)
            && self
                .installed_open
                .as_deref()
                .is_some_and(|open| open.specifier == finalize.specifier)
        {
            tasks.push(self.refresh_stale_installed_detail().task);
        }
        Update::new(Task::batch(tasks), event)
    }

    /// Refresh the authored-package inventory without fabricating an empty list on I/O failure.
    /// The previous good snapshot remains visible, while every shadow-sensitive mutation is
    /// blocked until both the inventory and governing lock rows are authoritative again.
    pub(super) fn reload_local_package_state(&mut self) -> Update<Message, Event> {
        let local_names = match local_packages::list_local_packages(&self.server_name) {
            Ok(names) => names,
            Err(error) => {
                log::warn!("Failed to list local packages: {error:#}");
                self.local_package_state_error = Some(crate::i18n::t!(
                    "package-local-state-unavailable",
                    "error" => error.to_string()
                ));
                return Update::with_task(self.reconcile_owned_package_language_project_reload());
            }
        };
        self.local_packages = local_names;
        if let Err(error) =
            load_local_manifest_snapshot_for_names(&self.server_name, &self.local_packages)
        {
            log::warn!("Failed to validate local packages: {error}");
            self.local_package_state_error = Some(crate::i18n::t!(
                "package-local-state-unavailable",
                "error" => error
            ));
            return Update::with_task(self.reconcile_owned_package_language_project_reload());
        }
        let canonical_owner = self
            .cloud
            .snapshot
            .get()
            .nickname_text()
            .unwrap_or_else(|| local_packages::LOCAL_OWNER.to_string());
        if let Err(error) = local_packages::materialize_governing_local_lock_rows(
            &self.server_name,
            &self.local_packages,
            &canonical_owner,
        ) {
            log::warn!("Failed to materialize local package settings: {error:#}");
            self.local_package_state_error = Some(crate::i18n::t!(
                "package-local-state-unavailable",
                "error" => error.to_string()
            ));
            return Update::with_task(self.reconcile_owned_package_language_project_reload());
        }
        let package_state_authoritative = match shared_packages::load_lock(&self.server_name) {
            Ok(lock) => {
                self.installed_packages = lock.packages;
                self.local_package_state_error = None;
                self.rebuild_graph();
                true
            }
            Err(error) => {
                log::warn!("Failed to load installed package state: {error:#}");
                self.installed_package_state_error = Some(crate::i18n::t!(
                    "package-installed-state-unavailable",
                    "error" => error.to_string()
                ));
                false
            }
        };
        if !package_state_authoritative {
            return Update::with_task(self.reconcile_owned_package_language_project_reload());
        }
        // Installed panes become non-authoritative when a local same-leaf package takes over. The
        // helper refuses to close over a draft or accepted pending navigation.
        self.close_newly_shadowed_installed_pane();
        Update::with_task(self.reconcile_owned_package_language_project_reload())
    }

    /// Reconcile and load the complete package authority. Every prerequisite is strict: a missing
    /// local inventory must never be interpreted as permission to activate its remote fallback,
    /// and an unreadable lock must never close a valid pane as though it were uninstalled.
    pub(super) fn reload_installed_package_state(&mut self) -> Update<Message, Event> {
        let local_names = match local_packages::list_local_packages(&self.server_name) {
            Ok(names) => names,
            Err(error) => {
                log::warn!("Failed to list local packages during reconciliation: {error:#}");
                let message = crate::i18n::t!(
                    "package-local-state-unavailable",
                    "error" => error.to_string()
                );
                self.local_package_state_error = Some(message.clone());
                self.installed_package_state_error = Some(message);
                return Update::none();
            }
        };
        self.local_packages = local_names;
        if let Err(error) =
            load_local_manifest_snapshot_for_names(&self.server_name, &self.local_packages)
        {
            log::warn!("Failed to validate local packages during reconciliation: {error}");
            let message = crate::i18n::t!(
                "package-local-state-unavailable",
                "error" => error
            );
            self.local_package_state_error = Some(message.clone());
            self.installed_package_state_error = Some(message);
            return Update::none();
        }
        let changed = match shared_packages::reconcile_local_installs(&self.server_name) {
            Ok(changed) => changed,
            Err(error) => {
                log::warn!("Failed to reconcile local installs: {error:#}");
                let message = crate::i18n::t!(
                    "package-local-reconcile-failed",
                    "error" => error.to_string()
                );
                self.local_package_state_error = Some(message.clone());
                self.installed_package_state_error = Some(message);
                return Update::none();
            }
        };
        if !changed.is_empty() {
            log::info!("Reconciled local package installs: {}", changed.join(", "));
        }
        let nickname = self.cloud.snapshot.get().nickname_text();
        let canonical_owner = nickname.unwrap_or_else(|| local_packages::LOCAL_OWNER.to_string());
        if let Err(error) = local_packages::materialize_governing_local_lock_rows(
            &self.server_name,
            &self.local_packages,
            &canonical_owner,
        ) {
            log::warn!("Failed to materialize local package settings: {error:#}");
            let message = crate::i18n::t!(
                "package-local-reconcile-failed",
                "error" => error.to_string()
            );
            self.local_package_state_error = Some(message.clone());
            self.installed_package_state_error = Some(message);
            return Update::none();
        }
        let lock = match shared_packages::load_lock(&self.server_name) {
            Ok(lock) => lock,
            Err(error) => {
                log::warn!("Failed to load lockfile: {error:#}");
                self.installed_package_state_error = Some(crate::i18n::t!(
                    "package-installed-state-unavailable",
                    "error" => error.to_string()
                ));
                return Update::none();
            }
        };
        self.local_package_state_error = None;
        self.installed_package_state_error = None;
        self.installed_packages = lock.packages;
        self.close_newly_shadowed_installed_pane();

        let open_was_removed = self.installed_open.as_deref().is_some_and(|open| {
            changed.iter().any(|specifier| specifier == &open.specifier)
                || !self
                    .installed_packages
                    .iter()
                    .any(|package| package.specifier == open.specifier)
        });
        let selected_was_removed = match &self.selection {
            Selection::InstalledPackage(specifier) => !self
                .installed_packages
                .iter()
                .any(|package| package.specifier == *specifier),
            _ => false,
        };
        if open_was_removed || selected_was_removed {
            self.clear_selection();
            self.installed_open = None;
            self.installed_detail = None;
            self.installed_rating = None;
            self.selection = Selection::Dashboard;
            self.pane = Pane::Dashboard;
        }
        self.graph.requires.clear();
        self.graph.resolved.clear();
        self.blocked_updates.clear();
        self.rebuild_graph();
        self.graph_seq.bump();
        let mut task = self.resolve_graph_deps();
        if let Some(sweep) = self.sweep_stale_account_installs() {
            task = Task::batch([task, sweep]);
        }
        Update::new(
            task,
            (!changed.is_empty()).then_some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
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
                let editable = self
                    .param_config
                    .as_ref()
                    .is_some_and(|config| self.param_config_edit_available(config));
                if !editable {
                    return Update::none();
                }
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
        let (expected_package, params) = match self.param_prompt.as_ref() {
            Some(prompt) => (prompt.expected_package.clone(), prompt.params.clone()),
            None => return Update::none(),
        };
        if expected_package.parameter_scope != ParameterScope::Global {
            return self.fail_prompt(crate::i18n::t!("package-param-prompt-scope-changed"));
        }
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
                    return self.fail_prompt(crate::i18n::t!(
                        "package-field-required",
                        "field" => &param.key
                    ));
                }
                plan.push((param.key.clone(), Persist::Secret(text)));
            } else {
                match state.map_or(Ok(None), |s| param_values::to_json(param, s)) {
                    Ok(Some(value)) => plan.push((param.key.clone(), Persist::Value(value))),
                    Ok(None) => {
                        return self.fail_prompt(crate::i18n::t!(
                            "package-field-required",
                            "field" => &param.key
                        ));
                    }
                    Err(reason) => {
                        return self.fail_prompt(crate::i18n::t!(
                            "package-field-invalid",
                            "field" => &param.key,
                            "reason" => reason
                        ));
                    }
                }
            }
        }
        let mutations = plan
            .iter()
            .map(|(key, persist)| persist.mutation(key))
            .collect::<Vec<_>>();
        match shared_packages::commit_package_params_scoped_if_unchanged(
            &self.server_name,
            ParamValueScope::Global,
            &expected_package,
            &mutations,
        ) {
            Ok(PackageParamCommit::Applied) => {}
            Ok(PackageParamCommit::StateChanged) => {
                self.package_change_finalize = None;
                self.param_prompt_queue.clear();
                let _ = self.fail_prompt(crate::i18n::t!("package-settings-state-changed"));
                return Update::new(
                    Task::batch([
                        Task::done(Message::LoadLocalPackages),
                        Task::done(Message::LoadInstalledPackages),
                    ]),
                    Some(Event::ScriptsChanged {
                        server_name: self.server_name.clone(),
                    }),
                );
            }
            Err(error) => {
                return self.fail_prompt(crate::i18n::t!(
                    "package-settings-save-failed",
                    "error" => error.to_string()
                ));
            }
        }
        // The package was already installed + consented at the Grant step; this only saves
        // configuration, then advances to the next queued required-root prompt (or finishes the
        // root operation captured in `package_change_finalize`).
        self.param_prompt = None;
        self.advance_param_prompt_queue()
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

    /// Replaces every cached lock row from one authoritative read while preserving the identity of
    /// the installed package pane. The governing row for a shadowed remote can differ from the
    /// remote row whose About/detail view is open; a settings refresh must not mix those models.
    fn reload_package_lock_snapshot(&mut self) -> Result<(), String> {
        let open_specifier = self
            .installed_open
            .as_deref()
            .map(|package| package.specifier.clone());
        let lock = shared_packages::load_lock(&self.server_name).map_err(|error| {
            let message = crate::i18n::t!(
                "package-installed-state-unavailable",
                "error" => error.to_string()
            );
            self.installed_package_state_error = Some(message.clone());
            message
        })?;
        self.installed_package_state_error = None;
        self.installed_packages = lock.packages;
        if let Some(specifier) = open_specifier {
            self.installed_open = self
                .installed_packages
                .iter()
                .find(|package| package.specifier == specifier)
                .cloned()
                .map(Box::new);
        }
        if let Some(config) = self.param_config.as_mut()
            && let Some(package) = self
                .installed_packages
                .iter()
                .find(|package| package.specifier == config.specifier)
        {
            config.parameter_scope = package.parameter_scope;
        }
        self.rebuild_graph();
        Ok(())
    }

    pub(super) fn package_state_error(&self) -> Option<String> {
        self.local_package_state_error
            .clone()
            .or_else(|| self.installed_package_state_error.clone())
    }

    pub(super) fn package_state_available(&self) -> bool {
        self.local_package_state_error.is_none() && self.installed_package_state_error.is_none()
    }

    fn load_consent_resolution_state(
        &mut self,
    ) -> Result<(SharedPackageLock, HashMap<String, PackageManifest>), String> {
        let local_manifests = load_local_manifest_snapshot(&self.server_name).map_err(|error| {
            let message = crate::i18n::t!(
                "package-local-state-unavailable",
                "error" => error
            );
            self.local_package_state_error = Some(message.clone());
            message
        })?;
        let lock = shared_packages::load_lock(&self.server_name).map_err(|error| {
            let message = crate::i18n::t!(
                "package-installed-state-unavailable",
                "error" => error.to_string()
            );
            self.installed_package_state_error = Some(message.clone());
            message
        })?;
        Ok((lock, local_manifests))
    }

    /// Revalidates the exact lock and local-manifest authority used to prepare a consent card.
    /// This runs both when Grant starts and after asynchronous cache preparation, so a prompt
    /// cannot approve a package plan whose activation, dependency graph, or local shadow changed
    /// while it was open.
    fn validate_consent_snapshot(
        &mut self,
        expected_lock: &SharedPackageLock,
        expected_local_manifests: &HashMap<String, PackageManifest>,
    ) -> Result<(), String> {
        let (current_lock, current_local_manifests) = self.load_consent_resolution_state()?;
        if &current_lock != expected_lock || &current_local_manifests != expected_local_manifests {
            return Err(crate::i18n::t!("package-install-plan-changed"));
        }
        Ok(())
    }

    fn param_config_edit_available(&self, config: &ParamConfig) -> bool {
        config.available
            && self.package_state_available()
            && (config.parameter_scope == ParameterScope::Global || self.profile_inventory_complete)
    }

    // ---- in-pane param-value editor (installed & owned panes) -------------

    /// (Re)seed the inline param-value editor for the open package. `None` when the package
    /// declares no params, so the section renders nothing. Called when a package pane opens (and
    /// when an owned package's manifest is saved, which can add/remove params).
    pub(super) fn seed_param_config(&mut self, specifier: String, params: Vec<PackageParameter>) {
        if params.is_empty() {
            self.param_config = None;
            return;
        }
        let expected_package = self
            .installed_packages
            .iter()
            .find(|package| package.specifier == specifier)
            .cloned();
        let unavailable = self.package_state_error().or_else(|| {
            expected_package.is_none().then(|| {
                crate::i18n::t!(
                    "package-installed-state-unavailable",
                    "error" => crate::i18n::t!("package-settings-row-missing")
                )
            })
        });
        let parameter_scope = expected_package
            .as_ref()
            .map_or_else(ParameterScope::default, |package| package.parameter_scope);
        if let Some(error) = unavailable {
            self.param_config = Some(ParamConfig::unavailable(
                specifier,
                parameter_scope,
                &self.parameter_profile,
                params,
                error,
            ));
            return;
        }
        let expected_package = expected_package.expect("unavailable row handled above");
        self.param_config = Some(
            ParamConfig::seed(
                &self.server_name,
                &self.parameter_profile,
                expected_package,
                params.clone(),
            )
            .unwrap_or_else(|error| {
                ParamConfig::unavailable(
                    specifier,
                    parameter_scope,
                    &self.parameter_profile,
                    params,
                    crate::i18n::t!(
                        "package-settings-read-unavailable",
                        "error" => error
                    ),
                )
            }),
        );
    }

    /// Persist every declared param's configured value: non-secrets to `smudgy.params.json`
    /// (cleared when emptied), secrets to the keyring (an empty box keeps the stored secret).
    /// Required params must resolve to a value; a value that fails to project for its kind (a number
    /// that won't parse, a dropdown value that isn't a choice) is reported and nothing is written. An
    /// enabled package hot-reloads so it picks the new config up; saving never changes the package's
    /// installed/enabled state.
    pub(super) fn param_config_save(&mut self) -> Update<Message, Event> {
        let editable = self
            .param_config
            .as_ref()
            .is_some_and(|config| self.param_config_edit_available(config));
        if !editable {
            return Update::none();
        }
        let (specifier, expected_package, params, parameter_scope, profile_name) =
            match self.param_config.as_ref() {
                Some(config) => (
                    config.specifier.clone(),
                    config.expected_package.clone(),
                    config.params.clone(),
                    config.parameter_scope,
                    config.profile_name.clone(),
                ),
                None => return Update::none(),
            };
        let Some(expected_package) = expected_package else {
            return Update::none();
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
                    return self.fail_config(crate::i18n::t!(
                        "package-field-required",
                        "field" => &param.key
                    ));
                }
                // A non-empty box replaces the secret; an empty box keeps whatever is stored.
                if !text.is_empty() {
                    plan.push((param.key.clone(), Persist::Secret(text)));
                }
            } else {
                let projected = match state.map_or(Ok(None), |s| param_values::to_json(param, s)) {
                    Ok(value) => value,
                    Err(reason) => {
                        return self.fail_config(crate::i18n::t!(
                            "package-field-invalid",
                            "field" => &param.key,
                            "reason" => reason
                        ));
                    }
                };
                if param.required && projected.is_none() {
                    return self.fail_config(crate::i18n::t!(
                        "package-field-required",
                        "field" => &param.key
                    ));
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

        let value_scope = match parameter_scope {
            ParameterScope::Global => ParamValueScope::Global,
            ParameterScope::Profile => ParamValueScope::Profile(&profile_name),
        };
        let mutations = plan
            .iter()
            .map(|(key, persist)| persist.mutation(key))
            .collect::<Vec<_>>();
        match shared_packages::commit_package_params_scoped_if_unchanged(
            &self.server_name,
            value_scope,
            &expected_package,
            &mutations,
        ) {
            Ok(PackageParamCommit::Applied) => {}
            Ok(PackageParamCommit::StateChanged) => {
                return self.fail_config_parameter_state_changed();
            }
            Err(error) => {
                return self.fail_config(crate::i18n::t!(
                    "package-settings-save-failed",
                    "error" => error.to_string()
                ));
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
        let affected_profile =
            matches!(parameter_scope, ParameterScope::Profile).then_some(profile_name.as_str());
        let event = self
            .configuration_change_affects_running(&specifier, affected_profile)
            .then(|| Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            });
        Update::new(
            self.show_toast(crate::i18n::t!("package-settings-saved")),
            event,
        )
    }

    /// Remove a stored secret param entirely (the only way to *unset* a secret, since the box can
    /// only replace one). An enabled package hot-reloads so a script reading it sees the change.
    pub(super) fn param_config_clear_secret(&mut self, key: String) -> Update<Message, Event> {
        let editable = self
            .param_config
            .as_ref()
            .is_some_and(|config| self.param_config_edit_available(config));
        if !editable {
            return Update::none();
        }
        let Some((specifier, expected_package, parameter_scope, profile_name)) =
            self.param_config.as_ref().and_then(|config| {
                Some((
                    config.specifier.clone(),
                    config.expected_package.clone()?,
                    config.parameter_scope,
                    config.profile_name.clone(),
                ))
            })
        else {
            return Update::none();
        };
        let scope = match parameter_scope {
            ParameterScope::Global => ParamValueScope::Global,
            ParameterScope::Profile => ParamValueScope::Profile(&profile_name),
        };
        match shared_packages::clear_secret_param_scoped_if_unchanged(
            &self.server_name,
            scope,
            &expected_package,
            &key,
        ) {
            Ok(PackageParamCommit::Applied) => {}
            Ok(PackageParamCommit::StateChanged) => {
                return self.fail_config_parameter_state_changed();
            }
            Err(e) => {
                if let Some(config) = self.param_config.as_mut() {
                    config.error = Some(crate::i18n::t!(
                        "package-clear-secret-failed",
                        "field" => &key,
                        "error" => e.to_string()
                    ));
                    config.saved = false;
                }
                return Update::none();
            }
        }
        if let Some(config) = self.param_config.as_mut() {
            config.secret_stored.remove(&key);
            config
                .values
                .insert(key.clone(), ParamValueState::Text(String::new()));
            config.error = None;
            config.saved = false;
        }
        let affected_profile = self.param_config.as_ref().and_then(|config| {
            matches!(config.parameter_scope, ParameterScope::Profile)
                .then_some(config.profile_name.as_str())
        });
        let event = self
            .configuration_change_affects_running(&specifier, affected_profile)
            .then(|| Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            });
        Update::new(
            self.show_toast(crate::i18n::t!("package-secret-cleared")),
            event,
        )
    }

    /// A parameter CAS mismatch means this editor no longer has authority to write. Keep its
    /// values/touched markers for the user's review, disable further mutations, and refresh both
    /// package inventories without silently rebasing the draft onto a different row.
    fn fail_config_parameter_state_changed(&mut self) -> Update<Message, Event> {
        if let Some(config) = self.param_config.as_mut() {
            config.available = false;
            config.error = Some(crate::i18n::t!("package-settings-state-changed"));
            config.saved = false;
        }
        Update::new(
            Task::batch([
                Task::done(Message::LoadLocalPackages),
                Task::done(Message::LoadInstalledPackages),
            ]),
            Some(Event::ScriptsChanged {
                server_name: self.server_name.clone(),
            }),
        )
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
        self.load_shared_cloud_lists()
    }

    fn load_shared_cloud_lists(&self) -> Update<Message, Event> {
        // Load both halves of the pane in parallel: packages friends shared with the caller, and
        // the caller's own cloud packages (so an owner sees private packages that exist in no other
        // surface — e.g. one published from another machine).
        let account_epoch = self.account_epoch;
        let (account_fence, frozen_credentials) = self.frozen_cloud_credentials();
        let shared_client =
            PackageApiClient::new(self.cloud.base_url.as_str(), frozen_credentials.clone());
        let mine_client = PackageApiClient::new(self.cloud.base_url.as_str(), frozen_credentials);
        Update::with_task(Task::batch([
            Task::perform(
                async move { shared_client.list_shared_packages().await },
                move |result| Message::SharedLoaded {
                    account_epoch,
                    account_fence,
                    result,
                },
            ),
            Task::perform(
                async move { mine_client.list_my_packages().await },
                move |result| Message::MyCloudLoaded {
                    account_epoch,
                    account_fence,
                    result,
                },
            ),
        ]))
    }

    pub(super) fn shared_loaded(
        &mut self,
        account_epoch: u64,
        account_fence: AccountReadFence,
        result: Result<Vec<PackageDetail>, CloudError>,
    ) -> Update<Message, Event> {
        if account_epoch != self.account_epoch
            || !self.account_read_is_current(account_fence)
            || self.selection != Selection::Shared
        {
            return Update::none();
        }
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
        account_epoch: u64,
        account_fence: AccountReadFence,
        result: Result<Vec<PackageDetail>, CloudError>,
    ) -> Update<Message, Event> {
        if account_epoch != self.account_epoch
            || !self.account_read_is_current(account_fence)
            || self.selection != Selection::Shared
        {
            return Update::none();
        }
        match result {
            Ok(list) => self.my_cloud_packages = Some(list),
            Err(e) => {
                self.my_cloud_packages = Some(Vec::new());
                self.discover_error = Some(display_error(&e));
            }
        }
        Update::none()
    }

    /// Clear every account-scoped package surface before refetching the currently visible one.
    /// This also fences already-issued authenticated reads through `account_epoch`.
    pub(super) fn account_changed(&mut self) -> Update<Message, Event> {
        self.account_epoch = self.account_epoch.wrapping_add(1);
        self.install_seq.bump();
        self.discover_seq.bump();
        self.detail_seq.bump();
        self.graph_seq.bump();
        self.share_seq.bump();
        self.consent_prompt = None;
        self.consent_busy = false;
        // The permit is owned across manifest-consent continuations. Account invalidation cancels
        // that continuation and must release the shared package gate as well.
        self.manifest_operation = None;
        self.update_delta = None;
        self.authoring_operation = None;
        self.authoring_busy = false;
        self.share_operation = None;
        self.share_busy = false;
        self.share_package_id = None;
        self.share_is_public = false;
        self.share_friends.clear();
        self.share_grants.clear();
        self.share_versions.clear();
        self.share_feedback = None;
        self.shared_with_me = None;
        self.my_cloud_packages = None;
        // These details can include caller-specific ratings/comments. Public search results remain
        // useful, but the selected personalized detail must be fetched again on demand.
        self.discover_requested_package = None;
        self.discover_owner = None;
        self.discover_busy = false;
        self.discover_detail = None;
        self.discover_readme = None;
        self.discover_comments.clear();
        // Installed manifests, source URLs/bodies, requirements, and rating metadata can all be
        // private to the prior account. Remove them before starting replacement reads.
        self.installed_detail = None;
        self.installed_versions.clear();
        self.installed_source.clear();
        self.installed_readme = InstalledReadmeState::Loaded(None);
        self.installed_rating = None;
        self.graph.requires.clear();
        self.graph.resolved.clear();
        self.blocked_updates.clear();
        self.rebuild_graph();

        let server_name = self.server_name.clone();
        let selected_update = match self.selection.clone() {
            Selection::OwnedPackage(name) => self.refresh_owned_share_if_open(&server_name, &name),
            Selection::InstalledPackage(specifier) => self.load_installed_detail(&specifier),
            Selection::Dependency { spec, .. } => self.load_installed_detail(&spec),
            Selection::Shared if self.signed_in() => self.load_shared_cloud_lists(),
            _ => Update::none(),
        };
        Update::new(
            Task::batch([self.resolve_graph_deps(), selected_update.task]),
            selected_update.event,
        )
    }
}

// ============================================================================
// Views
// ============================================================================

impl AutomationsWindow {
    pub(super) fn package_status(&self, specifier: &str) -> NodeStatus {
        if !self.package_state_available() {
            NodeStatus::Error
        } else if !self.graph.effectively_enabled(specifier) {
            NodeStatus::Disabled
        } else if self.blocked_updates.contains(specifier) {
            // Enabled and running (at a fitting version), but its newest version is held back for
            // lack of permissions — flag it so the user reviews + grants the update.
            NodeStatus::Warning
        } else {
            NodeStatus::Ok
        }
    }

    pub(super) fn profile_activation_summary(&self, activation: &ProfileActivation) -> String {
        if matches!(activation, ProfileActivation::All) {
            return crate::i18n::t!("activation-every-profile");
        }
        if matches!(activation, ProfileActivation::None) {
            return crate::i18n::t!("activation-no-profile");
        }
        let enabled = self
            .profile_names
            .iter()
            .filter(|profile| activation.is_enabled_for(profile))
            .count();
        crate::i18n::t!(
            "activation-profile-count",
            "enabled" => enabled,
            "total" => self.profile_names.len()
        )
    }

    /// A local package's distinct durable state specifier. Runtime/public identity can use the
    /// current nickname, but activation, trust, consent, and parameter state always remain under
    /// the reserved owner so a same-name published row can never lend authority to mutable code.
    pub(super) fn local_own_spec(&self, name: &str) -> String {
        specifier_for(smudgy_core::models::local_packages::LOCAL_OWNER, name)
    }

    /// The local package that canonically replaces `specifier`, matched by leaf name.
    pub(super) fn local_override_name(&self, specifier: &str) -> Option<&str> {
        let leaf = package_display_name(specifier);
        self.local_packages
            .iter()
            .find(|name| name.eq_ignore_ascii_case(leaf))
            .map(String::as_str)
    }

    /// Closes an installed fallback pane as soon as an authoritative local inventory says that a
    /// same-leaf local package now governs it. Keeping the remote pane interactive would let its
    /// trust or update controls mutate dormant state that runtime does not use.
    pub(super) fn close_newly_shadowed_installed_pane(&mut self) -> bool {
        if !matches!(self.pane, Pane::InstalledPackage)
            || self.has_unsaved_draft()
            || self.pending_nav.is_some()
        {
            return false;
        }
        let newly_shadowed = self.installed_open.as_deref().is_some_and(|package| {
            parse_specifier(&package.specifier).is_some_and(|(owner, _)| {
                !owner.eq_ignore_ascii_case(local_packages::LOCAL_OWNER)
                    && self.local_override_name(&package.specifier).is_some()
            })
        });
        if !newly_shadowed {
            return false;
        }
        self.clear_selection();
        self.installed_open = None;
        self.installed_detail = None;
        self.installed_rating = None;
        self.selection = Selection::Dashboard;
        self.pane = Pane::Dashboard;
        true
    }

    fn refresh_local_shadow_after_authoritative_mutation(&mut self) {
        match local_packages::list_local_packages(&self.server_name) {
            Ok(names) => {
                self.local_packages = names;
                self.local_package_state_error = None;
                self.rebuild_graph();
                self.close_newly_shadowed_installed_pane();
            }
            Err(error) => {
                self.local_package_state_error = Some(crate::i18n::t!(
                    "package-local-state-unavailable",
                    "error" => error.to_string()
                ));
            }
        }
    }

    /// The lock row whose activation and settings govern a package reference.
    pub(super) fn governing_specifier(&self, specifier: &str) -> String {
        self.local_override_name(specifier)
            .map_or_else(|| specifier.to_string(), |name| self.local_own_spec(name))
    }

    /// Lock rows that must carry one declared `requires` parent link. The published fallback keeps
    /// the link for restoration after a local override is deleted; the canonical local row carries
    /// it while the override is active so persisted/UI effective activation matches the runtime.
    fn required_state_specifiers(&self, requested: &str) -> Vec<String> {
        let mut targets = Vec::new();
        if self
            .installed_packages
            .iter()
            .any(|package| package.specifier == requested)
        {
            targets.push(requested.to_string());
        }
        let governing = self.governing_specifier(requested);
        if governing != requested
            && self
                .installed_packages
                .iter()
                .any(|package| package.specifier == governing)
        {
            targets.push(governing);
        }
        targets
    }

    /// The truthful status of a local package: the status of its own-specifier install (Ok/Warning
    /// when loading, else Disabled).
    pub(super) fn local_status(&self, name: &str) -> NodeStatus {
        self.package_status(&self.local_own_spec(name))
    }

    // ---- installed package pane -------------------------------------------

    /// The README tab: the resolved version's markdown rendered full-width with no interior scroll,
    /// so the whole document flows in the pane's own scrollbar.
    /// The Source tab: the module file list (left) and the selected file's on-demand source (right).
    /// The right pane keeps its own fixed-height scroll so a long file scrolls independently of the
    /// README. README is intentionally absent — it lives in its own tab.
    /// Shown in a dependency-reference view of a package that is *also* installed on its own:
    /// management belongs to that standalone entry, not here. Uninstalling from this view would
    /// drop only the standalone install while the parent keeps the package resolved — a no-op to
    /// the eye — so we point at the package's own pane instead of offering the action.
    ///
    /// In this view the specifier is always directly installed: dependency edges are cloud
    /// specifiers, so a dep that isn't `dep_only` is in `direct` (an owned `local:` package can't
    /// be a dependency), and `SelectInstalledPackage` opens its own top-level pane.
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
            AutomationKind::Alias => crate::i18n::ts!("package-kind-alias"),
            AutomationKind::Trigger => crate::i18n::ts!("package-kind-trigger"),
            AutomationKind::Hotkey => crate::i18n::ts!("package-kind-hotkey"),
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
                text(crate::i18n::t!("package-automation-unavailable", "name" => name))
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
            .map(|subpath| crate::i18n::t!("package-creator-module", "name" => subpath))
            .or_else(|| {
                creator_id.strip_prefix("package:").map(|spec| {
                    crate::i18n::t!(
                        "package-creator-package",
                        "name" => package_display_name(spec)
                    )
                })
            })
            .unwrap_or_else(|| creator_id.to_string());
        let status_label = if entry.enabled {
            crate::i18n::t!("state-enabled")
        } else {
            crate::i18n::t!("state-disabled")
        };

        let mut body = column![self.scene_header(
            Some(status),
            name,
            Some(crate::i18n::t!(
                "package-readonly-created-by",
                "kind" => kind_label,
                "creator" => &creator_label
            )),
            Some(common::badge(status_label)),
        )]
        .spacing(16.0);

        body = body.push(
            text(crate::i18n::t!("package-created-managed"))
                .size(13.0)
                .style(common::muted),
        );

        if let Some(jump) = Self::creator_jump(creator_id) {
            body = body.push(
                button(
                    text(crate::i18n::t!(
                        "package-open-creator",
                        "creator" => &creator_label
                    ))
                    .size(12.0),
                )
                .style(button_style::secondary)
                .on_press(jump),
            );
        }

        body = body.push(
            column![
                common::section_label(crate::i18n::ts!("package-pattern")),
                code_block(if entry.pattern.is_empty() {
                    crate::i18n::ts!("package-none-parenthetical")
                } else {
                    &entry.pattern
                }),
            ]
            .spacing(6.0),
        );

        let (body_label, body_text): (String, String) = match &entry.body {
            AutomationBody::Command(cmd) => {
                (crate::i18n::t!("package-body-sends"), cmd.to_string())
            }
            AutomationBody::Script(Some(src)) => {
                (crate::i18n::t!("package-body-script"), src.to_string())
            }
            AutomationBody::Script(None) => (
                crate::i18n::t!("package-body-script"),
                crate::i18n::t!("package-body-script-unavailable"),
            ),
            AutomationBody::Noop => (
                crate::i18n::t!("package-body-does"),
                crate::i18n::t!("package-body-nothing"),
            ),
        };
        body = body
            .push(column![common::section_label(&body_label), code_block(&body_text)].spacing(6.0));

        pane_scroll(body)
    }

    // ---- owned package pane -----------------------------------------------

    pub(super) fn view_new_package(&self, name: &str, error: Option<&str>) -> Elem<'_> {
        let mut body = column![self.scene_header(
            None,
            crate::i18n::ts!("package-new"),
            Some(crate::i18n::t!("package-new-subtitle")),
            None,
        )]
        .spacing(16.0);
        if let Some(error) = error {
            body = body.push(text(error.to_string()).size(13.0).style(common::danger));
        }
        body = body.push(
            row![
                container(
                    text(crate::i18n::t!("package-name"))
                        .size(13.0)
                        .style(common::muted)
                )
                .width(Length::Fixed(92.0)),
                text_input(crate::i18n::ts!("package-name-placeholder"), name)
                    .on_input(Message::SetNewPackageName),
            ]
            .spacing(12.0)
            .align_y(Vertical::Center),
        );
        body = body.push(
            text(crate::i18n::t!("package-new-help"))
                .size(12.0)
                .style(common::muted),
        );
        body = body.push(
            row![
                iced::widget::space::horizontal(),
                button(text(crate::i18n::t!("editor-discard")).size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::Discard),
                button(text(crate::i18n::t!("package-create")).size(13.0))
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
            crate::i18n::ts!("package-discover"),
            Some(crate::i18n::t!("package-discover-subtitle")),
            None,
        )]
        .spacing(16.0);

        // The Install Confirmation window takes over the pane while pending.
        if let Some(prompt) = &self.consent_prompt {
            return pane_scroll(body.push(self.view_consent_prompt(prompt)));
        }

        body = body.push(
            row![
                text_input(
                    crate::i18n::ts!("package-search-placeholder"),
                    &self.discover_query
                )
                .on_input(Message::DiscoverQueryChanged)
                .on_submit(Message::DiscoverSearch),
                button(text(crate::i18n::t!("package-search")).size(13.0))
                    .style(button_style::primary)
                    .on_press(Message::DiscoverSearch),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
        // Host-aware scope radios. "For <host> only" is shown only when this profile has a MUD host;
        // changing any radio re-runs the search (handled in `update`).
        let mut scope = row![
            text(crate::i18n::t!("package-scope"))
                .size(13.0)
                .style(common::muted),
            radio(
                crate::i18n::t!("package-scope-relevant"),
                DiscoverScope::Relevant,
                Some(self.discover_scope),
                Message::DiscoverScopeChanged
            ),
        ]
        .spacing(16.0)
        .align_y(Vertical::Center);
        if let Some(host) = &self.mud_host {
            scope = scope.push(radio(
                crate::i18n::t!("package-scope-host", "host" => host),
                DiscoverScope::HostOnly,
                Some(self.discover_scope),
                Message::DiscoverScopeChanged,
            ));
        }
        scope = scope
            .push(radio(
                crate::i18n::t!("package-scope-universal"),
                DiscoverScope::Universal,
                Some(self.discover_scope),
                Message::DiscoverScopeChanged,
            ))
            .push(radio(
                crate::i18n::t!("package-scope-all"),
                DiscoverScope::All,
                Some(self.discover_scope),
                Message::DiscoverScopeChanged,
            ));
        body = body.push(scope);

        if self.discover_busy {
            body = body.push(
                text(crate::i18n::t!("package-working"))
                    .size(13.0)
                    .style(common::muted),
            );
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
                    text(crate::i18n::t!("package-no-results"))
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
            button(text(crate::i18n::t!("package-manage")).size(12.0))
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
                button(text(crate::i18n::t!("package-view")).size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::DiscoverSelect {
                        package_id: result.package_id,
                        owner: result.owner_nickname.clone(),
                    }),
                button(text(crate::i18n::t!("package-install")).size(12.0))
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
        let mut meta_spans: Vec<iced::widget::text::Span<'_, ()>> = vec![span(crate::i18n::t!(
            "package-search-meta",
            "owner" => &result.owner_nickname,
            "version" => result.latest_version.as_deref().unwrap_or("—"),
            "count" => result.install_count
        ))];
        meta_spans.extend(rating_spans(
            result.avg_rating,
            result.rating_count,
            star_color,
        ));
        let meta_line: Elem = rich_text(meta_spans).size(11.0).style(common::faint).into();
        container(card_with_trailing_action(
            column![
                row![
                    text(result.name.clone()).size(15.0),
                    if installed {
                        common::badge(crate::i18n::t!("package-installed"))
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
            action,
        ))
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
            .unwrap_or_else(|| crate::i18n::t!("package-you"));
        let installed = super::model::is_installed(&self.installed_packages, &owner, &pkg.name);
        let action: Elem = if installed {
            button(text(crate::i18n::t!("package-installed")).size(12.0))
                .style(button_style::secondary)
                .into()
        } else {
            button(text(crate::i18n::t!("package-install")).size(12.0))
                .style(button_style::primary)
                .on_press(Message::DiscoverInstall)
                .into()
        };
        // The meta line is a single text run: the owner/version/installs prefix and the rating
        // average/count inherit the muted base color, while the ★ span is tinted the "out" color.
        let star_color = crate::prefs::current().palette.output;
        let mut meta_spans: Vec<iced::widget::text::Span<'_, ()>> = vec![span(crate::i18n::t!(
            "package-search-meta",
            "owner" => &owner,
            "version" => detail.latest_version.as_deref().unwrap_or("—"),
            "count" => detail.install_count,
        ))];
        meta_spans.extend(rating_spans(
            detail.avg_rating,
            detail.rating_count,
            star_color,
        ));
        let meta_line: Elem = rich_text(meta_spans).size(12.0).style(common::muted).into();
        let mut col = column![
            row![
                button(text(crate::i18n::t!("package-back")).size(12.0))
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
        col = col.push(common::section_label(crate::i18n::ts!("package-comments")));
        if self.signed_in() {
            col = col.push(
                row![
                    text_input(
                        crate::i18n::ts!("package-comment-placeholder"),
                        &self.discover_comment_input
                    )
                    .on_input(Message::CommentInputChanged)
                    .on_submit(Message::AddComment),
                    button(text(crate::i18n::t!("package-post")).size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::AddComment),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            );
        }
        if self.discover_comments.is_empty() {
            col = col.push(
                text(crate::i18n::t!("package-no-comments"))
                    .size(12.0)
                    .style(common::muted),
            );
        }
        for comment in &self.discover_comments {
            let who = comment
                .user_nickname
                .clone()
                .unwrap_or_else(|| crate::i18n::t!("package-someone"));
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
            .push(
                text(crate::i18n::t!(
                    "package-configure",
                    "name" => &prompt.name,
                    "version" => &prompt.version
                ))
                .size(14.0),
            )
            .push(
                text(crate::i18n::t!("package-required-settings-help"))
                    .size(12.0)
                    .style(common::muted),
            );
        for param in &prompt.params {
            let state = prompt.values.get(&param.key);
            let field = if is_secret_string(param) {
                secret_field_row(
                    param,
                    state,
                    ParamTarget::Prompt,
                    crate::i18n::ts!("package-secret-placeholder"),
                    None,
                )
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
        if self.consent_busy {
            form = form.push(
                text(crate::i18n::t!("package-preparing-cache"))
                    .size(12.0)
                    .style(common::faint),
            );
        }
        form = form.push(
            row![
                iced::widget::space::horizontal(),
                button(text(crate::i18n::t!("action-cancel")).size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::ParamPromptCancel),
                button(text(crate::i18n::t!("action-save")).size(12.0))
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
    /// The always-shown Install Confirmation window: an all-or-nothing grant of the closure
    /// permission union, enumerating both what the package *will* and *will NOT* be able to do.
    pub(super) fn view_consent_prompt<'a>(&self, prompt: &'a ConsentPrompt) -> Elem<'a> {
        let is_update = matches!(&prompt.operation, ConsentOperation::Update { .. });
        let is_manifest = matches!(&prompt.operation, ConsentOperation::LocalManifest { .. });
        let mut form = Column::new()
            .spacing(12.0)
            .push(
                text(if is_manifest {
                    crate::i18n::t!("package-manifest-review-title", "name" => &prompt.name)
                } else if is_update {
                    crate::i18n::t!(
                        "package-update-review-title",
                        "name" => &prompt.name,
                        "version" => &prompt.version
                    )
                } else {
                    crate::i18n::t!(
                        "package-install-title",
                        "name" => &prompt.name,
                        "version" => &prompt.version
                    )
                })
                .size(16.0),
            )
            .push(
                text(crate::i18n::t!("package-publisher", "publisher" => &prompt.owner))
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
                .push(text(crate::i18n::t!("package-cannot-also")).size(13.0));
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
                .push(text(crate::i18n::t!("package-will-be-able")).size(13.0));
            for line in &can {
                can_col = can_col.push(consent_can_row(line));
            }
            form = form.push(can_col);

            // What it will NOT be able to do (the sandbox guarantee made legible).
            let mut cannot = Column::new()
                .spacing(4.0)
                .push(text(crate::i18n::t!("package-will-not-be-able")).size(13.0));
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

        // A peer conflict refuses the install: explain it and disable the install buttons.
        let conflict = prompt.conflict.as_deref();
        if let Some(message) = conflict {
            form = form.push(
                container(
                    column![
                        row![
                            text("\u{26A0}").size(14.0).style(common::danger),
                            text(crate::i18n::t!("package-install-conflict")).size(14.0),
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
                            text(crate::i18n::t!("package-install-newer-smudgy")).size(14.0),
                        ]
                        .spacing(8.0)
                        .align_y(Vertical::Center),
                        text(crate::i18n::t!("package-message-period", "message" => message))
                            .size(12.0),
                    ]
                    .spacing(6.0),
                )
                .padding(12.0)
                .width(Length::Fill)
                .style(common::banner_style),
            );
        }

        // A mandatory required package could not be resolved/read. Do not reinterpret `requires`
        // as optional just because the registry or manifest is temporarily unavailable.
        let required_unavailable = prompt.required_unavailable.as_deref();
        if let Some(message) = required_unavailable {
            form = form.push(
                container(
                    column![
                        row![
                            text("\u{26A0}").size(14.0).style(common::danger),
                            text(crate::i18n::t!("package-install-required-unavailable"))
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

        if let Some(error) = &prompt.error {
            form = form.push(text(error.clone()).size(12.0).style(common::danger));
        }
        // Both install actions grant the shown permissions (and co-install the required set); they
        // differ only in whether the packages are enabled (run) now or left off for review. A peer
        // conflict or version-floor refusal disables both install buttons — only Cancel remains.
        let can_install = conflict.is_none()
            && needs_smudgy.is_none()
            && required_unavailable.is_none()
            && !self.consent_busy;
        let mut actions = row![
            iced::widget::space::horizontal(),
            button(text(crate::i18n::t!("action-cancel")).size(12.0))
                .style(button_style::secondary)
                .on_press(Message::ConsentCancel),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center);
        if is_manifest {
            actions = actions.push(
                button(text(crate::i18n::t!("manifest-save")).size(12.0))
                    .style(button_style::primary)
                    // `enable` is ignored for a manifest save; existing activation is kept.
                    .on_press_maybe(can_install.then_some(Message::ConsentGrant { enable: false })),
            );
        } else if is_update {
            actions = actions.push(
                button(text(crate::i18n::t!("package-apply-update")).size(12.0))
                    .style(button_style::primary)
                    // `enable` is ignored for updates; their complete existing activation is kept.
                    .on_press_maybe(can_install.then_some(Message::ConsentGrant { enable: false })),
            );
        } else {
            actions = actions
                .push(
                    button(text(crate::i18n::t!("package-install-disabled")).size(12.0))
                        .style(button_style::secondary)
                        .on_press_maybe(
                            can_install.then_some(Message::ConsentGrant { enable: false }),
                        ),
                )
                .push(
                    button(text(crate::i18n::t!("package-install-enabled")).size(12.0))
                        .style(button_style::primary)
                        .on_press_maybe(
                            can_install.then_some(Message::ConsentGrant { enable: true }),
                        ),
                );
        }
        form = form.push(actions);
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
            .push(text(crate::i18n::t!("package-also-installs")).size(13.0));
        for root in roots {
            if root.already_satisfied {
                col = col.push(
                    row![
                        text("\u{2022}").size(13.0).style(common::muted),
                        text(crate::i18n::t!(
                            "package-already-installed-version",
                            "name" => &root.name,
                            "version" => &root.version
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
                crate::i18n::t!(
                    "package-upgrade-version",
                    "name" => &root.name,
                    "version" => &root.version
                )
            } else {
                crate::i18n::t!(
                    "package-name-version",
                    "name" => &root.name,
                    "version" => &root.version
                )
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
                            text(crate::i18n::t!(
                                "package-effectively-full-access-reasons",
                                "reasons" => join_reasons(&escape_reasons(&root.permissions))
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
    // ---- Shared-with-me ----------------------------------------------------

    pub(super) fn view_shared(&self) -> Elem<'_> {
        let mut body = column![self.scene_header(
            None,
            crate::i18n::ts!("package-private-shared"),
            Some(crate::i18n::t!("package-private-shared-subtitle")),
            None,
        )]
        .spacing(16.0);

        if !self.signed_in() {
            return pane_scroll(body.push(self.signed_out_banner()));
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
        body = body.push(common::section_label(crate::i18n::ts!(
            "package-your-packages"
        )));
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
                body = body.push(
                    text(crate::i18n::t!("package-loading"))
                        .size(13.0)
                        .style(common::muted),
                );
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
                        common::badge(crate::i18n::t!("package-local"))
                    } else if installed {
                        common::badge(crate::i18n::t!("package-installed"))
                    } else {
                        button(text(crate::i18n::t!("package-install")).size(12.0))
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
                        title_row =
                            title_row.push(common::badge(crate::i18n::t!("package-private")));
                    }
                    body = body.push(
                        container(card_with_trailing_action(
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
                            action,
                        ))
                        .padding(12.0)
                        .width(Length::Fill)
                        .style(common::card_style),
                    );
                }
                if list.is_empty() {
                    body = body.push(
                        text(crate::i18n::t!("package-no-owned-cloud"))
                            .size(13.0)
                            .style(common::muted),
                    );
                }
            }
        }

        // ---- Shared with you (by friends) ----
        body = body.push(common::section_label(crate::i18n::ts!(
            "package-shared-with-you"
        )));
        match &self.shared_with_me {
            None => {
                body = body.push(
                    text(crate::i18n::t!("package-loading"))
                        .size(13.0)
                        .style(common::muted),
                );
            }
            Some(list) if list.is_empty() => {
                body = body.push(
                    text(crate::i18n::t!("package-no-shared"))
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
                        common::badge(crate::i18n::t!("package-installed"))
                    } else {
                        button(text(crate::i18n::t!("package-install")).size(12.0))
                            .style(button_style::primary)
                            .on_press(Message::InstallShared {
                                owner: owner.clone(),
                                name: detail.package.name.clone(),
                            })
                            .into()
                    };
                    body = body.push(
                        container(card_with_trailing_action(
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
                            action,
                        ))
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

/// A card row whose trailing controls keep their intrinsic width while the leading content wraps
/// into the space that remains. In iced's flex layout, a shrink-width text column is measured before
/// later siblings and can consume their room; making the content fluid causes the action rail to be
/// measured first instead.
fn card_with_trailing_action<'a, Message, Theme, Renderer>(
    content: impl Into<iced::Element<'a, Message, Theme, Renderer>>,
    action: impl Into<iced::Element<'a, Message, Theme, Renderer>>,
) -> iced::Element<'a, Message, Theme, Renderer>
where
    Message: 'a,
    Theme: iced::widget::container::Catalog + 'a,
    Renderer: iced::advanced::Renderer + 'a,
{
    row![
        container(content).width(Length::Fill),
        container(action).width(Length::Shrink),
    ]
    .spacing(10.0)
    .align_y(Vertical::Center)
    .into()
}

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
        None => vec![span(crate::i18n::t!("package-unrated"))],
    }
}

/// A 1–5 star rating control emitting `make_msg(stars)` on press. Rating is an account-only write,
/// so callers gate this on `signed_in()`. Shared by the Discover detail and the installed-package
/// pane. The star glyphs take the terminal palette's "out" (output) color, matching outgoing text.
pub(super) fn star_rate_row<'a>(make_msg: fn(i16) -> Message) -> Elem<'a> {
    let star_color = crate::prefs::current().palette.output;
    let mut rate = row![
        text(crate::i18n::t!("package-rate"))
            .size(12.0)
            .style(common::muted)
    ]
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
pub(super) fn rating_metric<'a>(
    avg_rating: Option<f64>,
    rating_count: i64,
    star_color: Color,
) -> Elem<'a> {
    let value_font = Font {
        weight: iced::font::Weight::Light,
        ..fonts::GEIST_VF
    };
    let value: Elem = rich_text(rating_spans(avg_rating, rating_count, star_color))
        .size(20.0)
        .font(value_font)
        .into();
    column![
        value,
        text(crate::i18n::t!("package-rating").to_uppercase())
            .size(10.0)
            .style(common::faint)
    ]
    .spacing(2.0)
    .into()
}

pub(super) fn metric<'a>(label: &str, value: &str) -> Elem<'a> {
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

/// A bounded publish console using the user's actual terminal font and palette. Diagnostics can be
/// arbitrarily long, so both axes scroll inside this surface instead of pushing the pane around.
pub(super) fn publish_output_panel<'a>(output: &str) -> Elem<'a> {
    let prefs = crate::prefs::current();
    let background = prefs.palette.background;
    let foreground = prefs.palette.foreground;
    let terminal_font = prefs.font;
    let scrollbar = || {
        scrollable::Scrollbar::new()
            .width(6)
            .scroller_width(6)
            .margin(2)
    };
    let output = container(
        text(output.to_string())
            .size(12.0)
            .font(terminal_font)
            .color(foreground)
            .wrapping(text::Wrapping::None),
    )
    .padding(10.0)
    .width(Length::Shrink);

    container(
        scrollable(output)
            .direction(scrollable::Direction::Both {
                vertical: scrollbar(),
                horizontal: scrollbar(),
            })
            .width(Length::Fill)
            .height(Length::Fill),
    )
    .width(Length::Fill)
    .height(Length::Fixed(132.0))
    .style(move |_theme: &crate::theme::Theme| container::Style {
        text_color: Some(foreground),
        background: Some(Background::Color(background)),
        border: Border {
            color: foreground.scale_alpha(0.24),
            width: 1.0,
            radius: 6.0.into(),
        },
        ..Default::default()
    })
    .into()
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

pub(super) fn file_row<'a>(label: &str, selected: bool, msg: Message) -> Elem<'a> {
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

pub(super) fn installed_package_tab_button<'a>(
    active: InstalledPackageTab,
    tab: InstalledPackageTab,
    label: &str,
) -> Elem<'a> {
    common::tab(
        label,
        active == tab,
        Message::SelectInstalledPackageTab(tab),
    )
}

pub(super) fn local_package_tab_button<'a>(
    active: LocalPackageTab,
    tab: LocalPackageTab,
    label: impl Into<String>,
) -> Elem<'a> {
    common::tab(label, active == tab, Message::SelectLocalPackageTab(tab))
}

/// The cached body for a fork-copied module, when the content-addressed store
/// already holds it. `None` (including when the cache itself is unavailable)
/// sends the fork path to the network for that module.
fn cached_fork_body(cache: Option<&PackageCache>, content_hash: &str) -> Option<Vec<u8>> {
    cache?.read_blob_bytes(content_hash)
}

#[cfg(test)]
mod tests {
    use iced::Size;
    use iced::advanced::layout;
    use iced::advanced::widget::tree::Tree;

    use super::*;

    #[test]
    fn card_action_keeps_its_width_when_content_is_long() {
        type TestElement = iced::Element<'static, (), iced::Theme, ()>;

        let content: TestElement = iced::widget::Space::new()
            .width(Length::Fixed(1_000.0))
            .height(10.0)
            .into();
        let action: TestElement = iced::widget::Space::new()
            .width(Length::Fixed(96.0))
            .height(10.0)
            .into();
        let mut row = card_with_trailing_action(content, action);
        let limits = layout::Limits::new(Size::ZERO, Size::new(500.0, 100.0));
        let mut tree = Tree::new(row.as_widget());
        let node = row.as_widget_mut().layout(&mut tree, &(), &limits);
        let children = node.children();

        assert_eq!(children.len(), 2);
        assert_eq!(children[1].bounds().width, 96.0);
        assert_eq!(children[1].bounds().x + children[1].bounds().width, 500.0);
        assert_eq!(children[0].bounds().width, 394.0);
    }

    #[test]
    fn owned_share_result_refreshes_only_the_open_package() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "publish-refresh-test".to_string(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        window.selection = Selection::OwnedPackage("demo".to_string());
        window.share_busy = true;
        let version = VersionListItem {
            version: "1.2.3".to_string(),
            yanked: false,
            deleted: false,
            published_at: "2026-08-10T00:00:00Z".parse().unwrap(),
        };

        let current = window.share_seq;
        let _ = window.owned_share_loaded(
            current,
            "demo",
            Ok((Uuid::new_v4(), true, vec![], vec![], vec![version.clone()])),
        );
        assert!(!window.share_busy);
        assert_eq!(window.share_versions, vec![version.clone()]);

        window.selection = Selection::OwnedPackage("other".to_string());
        window.share_busy = true;
        let _ = window.owned_share_loaded(
            current,
            "demo",
            Ok((Uuid::new_v4(), false, vec![], vec![], Vec::new())),
        );
        assert!(
            window.share_busy,
            "a stale result must not finish the current load"
        );
        assert_eq!(window.share_versions, vec![version]);

        window.selection = Selection::OwnedPackage("demo".to_string());
        window.share_seq.bump();
        window.share_busy = true;
        let _ = window.owned_share_loaded(
            current,
            "demo",
            Ok((Uuid::new_v4(), false, vec![], vec![], Vec::new())),
        );
        assert!(window.share_busy, "an older same-package load stays fenced");
        assert_eq!(window.share_versions.len(), 1);
    }

    #[test]
    fn classify_source_text_vs_binary_vs_oversize() {
        assert!(matches!(
            classify_source(b"export const x = 1;\n".to_vec()),
            FilePreview::Text { bidi: false, .. }
        ));
        let nul = classify_source(b"valid text\0then nul".to_vec());
        assert!(matches!(
            nul,
            FilePreview::Text {
                nul: true,
                ref source,
                ..
            } if source == "valid text␀then nul"
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
        assert!(has_deceptive_unicode("admin\u{3164}"));
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

    /// Edit-a-copy sources module bodies from the content-addressed cache and
    /// falls back to the network only for misses (or with no cache at all).
    #[test]
    fn fork_bodies_come_from_the_cache_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let cache = PackageCache::with_root(dir.path().join("cache").join("packages"));
        const HASH: &str = "2bf28849cade87113ec4170ef2bf9ffb1179fc9a276afa675c9de2424349560b";
        cache
            .write_blob_bytes(HASH, b"export const x = 1;")
            .unwrap();
        assert_eq!(
            cached_fork_body(Some(&cache), HASH).as_deref(),
            Some(b"export const x = 1;".as_slice())
        );
        // A miss — and an unavailable cache — both defer to the network fetch.
        assert!(cached_fork_body(Some(&cache), "feed02").is_none());
        assert!(cached_fork_body(None, HASH).is_none());
    }

    #[test]
    fn authoritative_local_shadow_closes_only_a_clean_remote_pane() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "package-shadow-pane-test".to_string(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        let remote = LockedPackage::new("smudgy://publisher/tools", UpdateMode::Auto);
        window.pane = Pane::InstalledPackage;
        window.selection = Selection::InstalledPackage(remote.specifier.clone());
        window.installed_open = Some(Box::new(remote.clone()));
        window.local_packages = vec!["tools".to_string()];

        assert!(window.close_newly_shadowed_installed_pane());
        assert!(matches!(window.pane, Pane::Dashboard));
        assert!(window.installed_open.is_none());

        window.pane = Pane::InstalledPackage;
        window.selection = Selection::InstalledPackage(remote.specifier.clone());
        window.installed_open = Some(Box::new(remote));
        window.dirty = true;
        assert!(!window.close_newly_shadowed_installed_pane());
        assert!(matches!(window.pane, Pane::InstalledPackage));
        assert!(window.installed_open.is_some());
    }

    #[test]
    fn account_change_releases_a_manifest_consent_package_gate() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "manifest-account-change-test".to_string(),
            crate::cloud_account::test_handles_signed_in("publisher"),
            smudgy_core::session::SessionId::from(1),
        );
        let permit = window
            .cloud
            .package_operations
            .try_acquire("manifest-account-change-test", "tools")
            .unwrap();
        window.manifest_operation = Some(permit);
        assert!(
            window
                .cloud
                .package_operations
                .is_busy("manifest-account-change-test", "tools")
        );

        let _ = window.account_changed();

        assert!(window.manifest_operation.is_none());
        assert!(
            !window
                .cloud
                .package_operations
                .is_busy("manifest-account-change-test", "tools")
        );
    }

    #[test]
    fn stale_sharing_completion_schedules_refresh_for_the_current_account() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "sharing-account-refresh-test".to_string(),
            crate::cloud_account::test_handles_signed_in("publisher"),
            smudgy_core::session::SessionId::from(1),
        );
        window.selection = Selection::OwnedPackage("tools".to_string());
        let current_generation = window.cloud.credentials.generation();
        let permit = window
            .cloud
            .package_operations
            .try_acquire("sharing-account-refresh-test", "finished-operation")
            .unwrap();
        let old_operation_id = permit.id();
        drop(permit);

        let Err(update) = window.accept_sharing_completion(
            "sharing-account-refresh-test",
            "tools",
            window.share_seq,
            Uuid::new_v4(),
            old_operation_id,
            current_generation.wrapping_sub(1),
        ) else {
            panic!("an operation not owned by this view must be stale");
        };

        assert_eq!(update.task.units(), 1);
        assert!(
            window.share_busy,
            "the replacement account refresh owns busy state"
        );
    }

    fn test_window(server_name: &str) -> AutomationsWindow {
        AutomationsWindow::new(
            iced::window::Id::unique(),
            server_name.to_string(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        )
    }

    fn resolution_for(
        specifier: &str,
        version: &str,
        permissions: PackagePermissions,
    ) -> InstallResolution {
        let (owner, name) = parse_specifier(specifier).unwrap();
        InstallResolution {
            specifier: specifier.to_string(),
            owner,
            name,
            version: version.to_string(),
            permissions,
            params: Vec::new(),
            closure: Vec::new(),
            required_roots: Vec::new(),
            conflict: None,
            needs_smudgy: None,
            required_unavailable: None,
            expected_lock: SharedPackageLock::default(),
            expected_local_manifests: HashMap::new(),
        }
    }

    fn manifest_prompt(name: &str, fence: AccountReadFence) -> ConsentPrompt {
        let manifest: PackageManifest = serde_json::from_str(r#"{"version":"0.1.0"}"#).unwrap();
        ConsentPrompt {
            account_fence: fence,
            specifier: format!("smudgy://{}/{name}", local_packages::LOCAL_OWNER),
            owner: local_packages::LOCAL_OWNER.to_string(),
            name: name.to_string(),
            version: "0.1.0".to_string(),
            permissions: PackagePermissions::default(),
            params: Vec::new(),
            closure: Vec::new(),
            required_roots: Vec::new(),
            conflict: None,
            needs_smudgy: None,
            required_unavailable: None,
            expected_lock: SharedPackageLock::default(),
            expected_local_manifests: HashMap::new(),
            operation: ConsentOperation::LocalManifest {
                name: name.to_string(),
                manifest: Box::new(manifest),
                json: String::new(),
                expected_manifest: String::new(),
                activation: ProfileActivation::All,
            },
            error: None,
        }
    }

    /// A resolved pin/update whose closure asks for more than the consented baseline leaves an
    /// update review card targeted at the open installed package, and the installed pane renders
    /// that card (the pane used to drop it on the floor).
    #[test]
    fn resolved_version_change_with_delta_prompts_for_the_open_package_and_renders() {
        let mut window = test_window("version-change-consent-test");
        let specifier = "smudgy://publisher/tools";
        let mut open = LockedPackage::new(specifier, UpdateMode::Auto);
        open.consented_permissions = Some(PackagePermissions::default());
        window.pane = Pane::InstalledPackage;
        window.selection = Selection::InstalledPackage(specifier.to_string());
        window.installed_open = Some(Box::new(open.clone()));
        window.installed_readme = InstalledReadmeState::Loaded(None);
        window.manage_busy = true;
        let mode = UpdateMode::Pinned {
            version: "2.0.0".to_string(),
        };
        let fence = window.account_read_fence();
        let seq = window.detail_seq;
        let permissions = PackagePermissions {
            net: vec!["api.example.org".to_string()],
            ..PackagePermissions::default()
        };

        let _ = window.installed_version_change_resolved(
            seq,
            fence,
            mode.clone(),
            Ok(resolution_for(specifier, "2.0.0", permissions.clone())),
        );

        assert!(!window.manage_busy);
        let prompt = window
            .consent_prompt_for_open_installed()
            .expect("the resolved version change must leave a review card for the open package");
        assert_eq!(prompt.specifier, specifier);
        assert_eq!(prompt.version, "2.0.0");
        assert_eq!(prompt.permissions, permissions);
        assert!(matches!(
            &prompt.operation,
            ConsentOperation::Update { mode: requested, .. } if *requested == mode
        ));
        // The installed pane renders the card in place of the tab body.
        drop(window.view_installed_package());

        // The card belongs to exactly one package: another open package never claims it.
        window.installed_open = Some(Box::new(LockedPackage::new(
            "smudgy://publisher/other",
            UpdateMode::Auto,
        )));
        assert!(window.consent_prompt.is_some());
        assert!(window.consent_prompt_for_open_installed().is_none());
        drop(window.view_installed_package());
        window.installed_open = Some(Box::new(open));

        // Cancel abandons the review and re-opens the package from the committed lock. This test
        // server has no lock row for it, so that reload closes the pane rather than leaving a
        // fenced, never-completing detail load behind.
        let _ = window.consent_cancel();
        assert!(window.consent_prompt.is_none());
        assert!(matches!(window.pane, Pane::Dashboard));
    }

    /// Granting a delta card keeps the package's update policy: a pinned install re-pins to the
    /// version the card was built from instead of silently switching to "follow latest".
    #[test]
    fn grant_update_on_a_pinned_package_requests_the_pinned_version() {
        let specifier = "smudgy://publisher/tools";
        let delta = UpdateDelta {
            specifier: specifier.to_string(),
            name: "tools".to_string(),
            version: "1.4.0".to_string(),
            current_version: Some("1.4.0".to_string()),
            added: PackagePermissions::default(),
            needs_smudgy: None,
            requirements_changed: true,
        };
        let pinned = LockedPackage::new(
            specifier,
            UpdateMode::Pinned {
                version: "1.4.0".to_string(),
            },
        );
        assert_eq!(
            update_grant_mode(&pinned, &delta),
            UpdateMode::Pinned {
                version: "1.4.0".to_string()
            }
        );
        let auto = LockedPackage::new(specifier, UpdateMode::Auto);
        assert_eq!(update_grant_mode(&auto, &delta), UpdateMode::Auto);
    }

    /// Cancelling a manifest requirements review releases the package-operation gate it held, so
    /// the manifest editor's Save is usable again.
    #[test]
    fn consent_cancel_releases_the_manifest_operation() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "consent-cancel-manifest-test".to_string(),
            crate::cloud_account::test_handles_signed_in("publisher"),
            smudgy_core::session::SessionId::from(1),
        );
        let permit = window
            .cloud
            .package_operations
            .try_acquire("consent-cancel-manifest-test", "tools")
            .unwrap();
        window.manifest_operation = Some(permit);
        window.consent_prompt = Some(manifest_prompt("tools", window.account_read_fence()));
        assert!(
            window
                .cloud
                .package_operations
                .is_busy("consent-cancel-manifest-test", "tools")
        );

        let _ = window.consent_cancel();

        assert!(window.consent_prompt.is_none());
        assert!(window.manifest_operation.is_none());
        assert!(
            !window
                .cloud
                .package_operations
                .is_busy("consent-cancel-manifest-test", "tools")
        );
    }

    #[test]
    fn ui_audit_parameter_state_change_preserves_draft_and_disables_stale_authority() {
        let mut window = AutomationsWindow::new(
            iced::window::Id::unique(),
            "parameter-cas-ui-test".to_string(),
            crate::cloud_account::test_handles(),
            smudgy_core::session::SessionId::from(1),
        );
        let expected = LockedPackage::new("smudgy://publisher/tools", UpdateMode::Auto);
        window.param_config = Some(ParamConfig {
            specifier: expected.specifier.clone(),
            expected_package: Some(expected.clone()),
            parameter_scope: ParameterScope::Global,
            profile_name: "main".to_string(),
            available: true,
            params: Vec::new(),
            values: HashMap::new(),
            secret_stored: HashSet::new(),
            touched: HashSet::from(["token".to_string()]),
            error: None,
            saved: false,
        });

        let update = window.fail_config_parameter_state_changed();

        let config = window.param_config.as_ref().unwrap();
        assert!(!config.available);
        assert_eq!(config.expected_package.as_ref(), Some(&expected));
        assert!(config.touched.contains("token"));
        assert!(config.error.is_some());
        assert_eq!(update.task.units(), 2);
        assert!(matches!(update.event, Some(Event::ScriptsChanged { .. })));
    }
}
