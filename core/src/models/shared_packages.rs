//! Per-server state for installed `smudgy://` shared packages.
//!
//! "Installing" a package associates its specifier with a **server**. Its activation can select
//! all, none, or specific profiles. Installation metadata stays server-wide. Parameter values
//! can be server-wide or profile-specific. The state lives below `<smudgy_home>/<server>/`:
//!
//! - the **lockfile** (`smudgy.lock.json`) — the install list plus, per package, the
//!   update mode (auto-latest by default, or pinned to a version), the last-resolved version +
//!   integrity hash (for offline reuse and reproducibility), activation, parameter scope, and
//!   `required_by` links;
//! - **non-secret parameter values** (`smudgy.params.json`, and the same file inside each
//!   profile directory for profile-scoped values);
//! - **secret parameter values** — declared *secret* parameters go to the OS keyring (with
//!   an obfuscated-file fallback), never to plain JSON, mirroring the cloud-session
//!   token in [`crate::models::auth`]. A per-server index of keyring slots makes them
//!   discoverable for cleanup, and a tombstone list denies reads of slots whose external
//!   deletion is still pending.
//!
//! Every function takes one reentrant per-server lock, so public functions can call each other
//! freely and the UI, background tasks, and session runtimes never interleave partial states.
//!
//! The package **manifest** types (`smudgy.package.json`) are owned by `smudgy_script`
//! and re-exported here so `core` has a single import surface.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::{fs, io};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::persistence::write_atomic;
use super::profile_activation::{ProfileActivation, resolve_activation};
use super::state_lock::{self, StateLockGuard};

pub use smudgy_script::{
    ImportPolicy, IpcEntry, IpcEntryIssue, PackageManifest, PackageParameter, PackagePermissions,
    ParamKind, ParamOption, SmudgyCapabilities, is_any_host_net_entry,
    is_local_transport_net_entry, is_windows_pipe_namespace_entry,
};

use crate::get_smudgy_home;
use crate::models::auth::{hex_decode, hex_encode, obfuscate};
use crate::models::local_packages::LOCAL_OWNER;

/// Lockfile name, relative to a server directory.
const LOCK_FILE: &str = "smudgy.lock.json";
/// Non-secret param-values file name, relative to a server or profile directory.
const PARAMS_FILE: &str = "smudgy.params.json";
/// Obfuscated secret-option fallback file (used only when no OS keyring is available).
const SECRETS_FILE: &str = ".package-secrets.json";
/// Keyring slots that may hold a package secret, so profile/server deletion can find them.
const SECRET_INDEX_FILE: &str = ".package-secret-index.json";
/// Keyring slots whose deletion failed; reads deny them until cleanup succeeds.
const SECRET_TOMBSTONES_FILE: &str = ".package-secret-tombstones.json";

/// One reentrant lock per server covers the lockfile, parameter files, secret metadata, and the
/// local package folders that govern part of that state.
pub(crate) fn guard(server_name: &str) -> StateLockGuard {
    state_lock::acquire(&format!("package-state:{server_name}"))
}

/// Whether a compare-and-set mutation ran or found changed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cas {
    /// Every precondition still matched and the mutation ran.
    Applied,
    /// A row, authority, or snapshot changed before the mutation acquired the lock. Nothing was
    /// written; callers reload and present current state.
    StateChanged,
}

/// Result of atomically accepting a local manifest and its required-package state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalManifestCommit {
    Applied,
    /// The manifest no longer matches the editor snapshot. Nothing was committed.
    Stale,
    /// Package state changed after the save plan was reviewed. Nothing was committed; callers
    /// must resolve and present the plan again.
    StateChanged,
}

/// How an installed package resolves on each session load.
///
/// The default is [`UpdateMode::Auto`]: re-resolve the latest published version each
/// load (with an offline fallback to the last-resolved version). A package can opt into
/// [`UpdateMode::Pinned`] for reproducibility. Integrity is verified on every fetch
/// regardless of mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum UpdateMode {
    /// Re-resolve the latest published version on each load.
    #[default]
    Auto,
    /// Always resolve this exact version.
    Pinned { version: String },
}

/// Which stored parameter values a package reads.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParameterScope {
    /// Read the existing server-wide value store.
    #[default]
    Global,
    /// Read values from the current profile's value store.
    Profile,
}

/// One installed package recorded in a server's lockfile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LockedPackage {
    /// Package-level user specifier, e.g. `smudgy://wbk/mapper`.
    pub specifier: String,
    #[serde(default)]
    pub mode: UpdateMode,
    /// The version most recently resolved — offline fallback + reproducibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_resolved_version: Option<String>,
    /// The content hash most recently verified for `last_resolved_version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub integrity: Option<String>,
    /// An update offer the user dismissed ("Later"): the offered version. Suppresses
    /// re-offering that update until a strictly newer version appears; cleared by
    /// [`stage_resolved_version`] once the staged version reaches (or passes) it, so a
    /// stale dismissal never outlives the offer it silenced. Absent for lockfiles
    /// predating the field and whenever nothing is dismissed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dismissed_update_version: Option<String>,
    /// Whether the user has **trusted** this package, promoting it (and its closure) onto
    /// the trusted main isolate — allow-all, shared instances — instead of its own
    /// sandboxed per-package isolate (`script/PACKAGE-ISOLATES.md`). This is a server-wide user
    /// decision, default `false`: installed packages are sandboxed until trusted. The engine reads
    /// this to partition the install set across isolates at session start.
    #[serde(default)]
    pub trusted: bool,
    /// The deno-native permission union the user consented to at install (or last update
    /// re-consent) — the **enforced** grant for this package's sandboxed isolate and the
    /// baseline an update's delta is computed against (see [`PackagePermissions::added_since`]
    /// and `script/PACKAGE-ISOLATES-CONSENT-TRUST.md`). Stored as the whole *closure* union
    /// captured at consent time, not a hash — the delta needs the old set to subtract from the
    /// new. `None` = never consented: enforcement treats that as the empty union, denying
    /// everything ("must consent"). Moot while `trusted`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consented_permissions: Option<PackagePermissions>,
    /// Legacy server-wide enabled mirror. New code reads [`activation`](Self::activation) instead.
    /// It is true only for `All`, so an older client fails closed for selected-profile state.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Profile-aware direct-root activation. Absent data uses `enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<ProfileActivation>,
    /// Whether parameter values are global or separate for each profile.
    #[serde(default)]
    pub parameter_scope: ParameterScope,
    /// Direct package roots whose `requires` closure includes this package. This is derived
    /// install metadata, not user activation. It lets session startup compute dependency-effective
    /// activation before package code is loaded.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub required_by: BTreeSet<String>,
    /// Whether this automatic row's complete parent lineage has been persisted. Older lockfiles
    /// recorded `installed_as_requirement` without `required_by`; until a current resolver writes
    /// lineage, their legacy activation remains authoritative so an upgrade does not silently stop
    /// working packages. New relationship transactions always set this bit.
    #[serde(default, skip_serializing_if = "is_false")]
    pub requirement_lineage_known: bool,
    /// Whether this package was installed **automatically because another package `requires` it**
    /// (vs. installed explicitly by the user) — apt's "automatically installed" mark. When the last
    /// package that required it is uninstalled, an auto-installed requirement becomes an *orphan*
    /// candidate and the user is prompted to remove it too (never removed silently). An explicit
    /// (re)install clears the flag — the user owns it. Defaults to `false` so pre-existing and
    /// user-installed entries are never treated as orphans. See `script/REQUIRED-PACKAGES.md`.
    #[serde(default)]
    pub installed_as_requirement: bool,
    /// Whether this package has successfully prepared at least one Web Audio
    /// context while sandboxed. This is observed behavior, not a declared
    /// permission: it only controls whether package-specific gain controls
    /// are worth showing. The versionless bit survives package updates and
    /// defaults to `false` for pre-audio lockfiles.
    #[serde(default, skip_serializing_if = "is_false")]
    pub audio_used: bool,
}

/// The serde default for [`LockedPackage::enabled`] — `true`, so a lock entry written before this
/// field existed (or by any path that doesn't set it) is treated as enabled and keeps running.
fn default_enabled() -> bool {
    true
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl LockedPackage {
    /// A freshly-installed package (auto-update, not yet resolved). Untrusted by default —
    /// it gets its own sandboxed isolate until the user trusts it.
    #[must_use]
    pub fn new(specifier: impl Into<String>, mode: UpdateMode) -> Self {
        Self {
            specifier: specifier.into(),
            mode,
            last_resolved_version: None,
            integrity: None,
            dismissed_update_version: None,
            trusted: false,
            consented_permissions: None,
            enabled: true,
            activation: None,
            parameter_scope: ParameterScope::Global,
            required_by: BTreeSet::new(),
            requirement_lineage_known: false,
            installed_as_requirement: false,
            audio_used: false,
        }
    }

    /// The version this package should resolve to, if pinned.
    #[must_use]
    pub fn pinned_version(&self) -> Option<&str> {
        match &self.mode {
            UpdateMode::Pinned { version } => Some(version),
            UpdateMode::Auto => None,
        }
    }

    /// The direct activation scope, including the legacy bool fallback.
    #[must_use]
    pub fn activation(&self) -> ProfileActivation {
        resolve_activation(self.activation.as_ref(), self.enabled)
    }

    /// Whether this direct package root is enabled for `profile_name`.
    #[must_use]
    pub fn is_enabled_for(&self, profile_name: &str) -> bool {
        self.activation().is_enabled_for(profile_name)
    }

    /// Replaces the complete activation and updates the legacy fail-closed mirror.
    pub fn set_activation(&mut self, activation: ProfileActivation) {
        self.enabled = activation.legacy_enabled();
        self.activation = Some(activation);
    }

    /// Whether this row still has independent activation intent.
    ///
    /// A legacy automatic row with no persisted lineage keeps its old behavior until a current
    /// relationship transaction identifies its parents. This is an upgrade bridge, not a second
    /// activation mode for newly installed requirements.
    #[must_use]
    pub fn has_direct_activation(&self) -> bool {
        !self.installed_as_requirement || !self.requirement_lineage_known
    }

    /// The concrete version this install is already staged to serve without consulting
    /// the network: the user's pin, else the last resolution a load recorded. `None` for
    /// a never-resolved [`UpdateMode::Auto`] install — the one case where version
    /// discovery genuinely needs the cloud.
    #[must_use]
    pub fn staged_version(&self) -> Option<&str> {
        self.pinned_version()
            .or(self.last_resolved_version.as_deref())
    }

    /// Stages `version` without changing the independent update policy: a pin moves to the
    /// accepted version; Auto advances its offline fallback. Any prior integrity described the
    /// old resolution and is cleared; a dismissed offer at or below `version` is cleared too.
    fn stage(&mut self, version: &str) {
        match &mut self.mode {
            UpdateMode::Auto => self.last_resolved_version = Some(version.to_string()),
            UpdateMode::Pinned {
                version: pinned_version,
            } => *pinned_version = version.to_string(),
        }
        self.integrity = None;
        if self
            .dismissed_update_version
            .as_deref()
            .is_some_and(|dismissed| !version_is_newer(dismissed, version))
        {
            self.dismissed_update_version = None;
        }
    }
}

fn is_local_owner(owner: &str) -> bool {
    owner.eq_ignore_ascii_case(LOCAL_OWNER)
}

fn local_state_specifier(name: &str) -> String {
    format!("smudgy://{LOCAL_OWNER}/{name}")
}

/// A server's lockfile: the installed package set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SharedPackageLock {
    pub packages: Vec<LockedPackage>,
}

impl SharedPackageLock {
    /// The installed package matching `specifier`, if any.
    #[must_use]
    pub fn find(&self, specifier: &str) -> Option<&LockedPackage> {
        self.packages.iter().find(|p| p.specifier == specifier)
    }

    fn find_mut(&mut self, specifier: &str) -> Option<&mut LockedPackage> {
        self.packages.iter_mut().find(|p| p.specifier == specifier)
    }

    /// Resolves a persistent package coordinate to the one row that governs its leaf.
    ///
    /// A local row shadows the published row for every author. More than one local row or more
    /// than one published row for a leaf is corrupt state, so it fails closed instead of choosing
    /// an arbitrary identity.
    fn governing_package(
        &self,
        specifier: &str,
        redirect_shadowed_leaf: bool,
    ) -> Option<&LockedPackage> {
        let requested = smudgy_script::SmudgySpecifier::parse(specifier).ok()?;
        let mut local = None;
        let mut remote = None;
        for package in &self.packages {
            let Ok(candidate) = smudgy_script::SmudgySpecifier::parse(&package.specifier) else {
                continue;
            };
            if !candidate.name.eq_ignore_ascii_case(&requested.name) {
                continue;
            }
            let slot = if is_local_owner(&candidate.owner) {
                &mut local
            } else {
                &mut remote
            };
            if slot.replace(package).is_some() {
                return None;
            }
        }
        match local {
            Some(package) if is_local_owner(&requested.owner) || redirect_shadowed_leaf => {
                Some(package)
            }
            Some(_) => None,
            None => remote.filter(|package| package.specifier.eq_ignore_ascii_case(specifier)),
        }
    }

    /// The specifier of the row that governs `specifier`'s leaf, if exactly one does.
    #[must_use]
    pub fn governing_specifier(&self, specifier: &str) -> Option<&str> {
        self.governing_package(specifier, true)
            .map(|package| package.specifier.as_str())
    }

    /// Whether a package is a direct active root or is required by one for this profile.
    #[must_use]
    pub fn is_effectively_enabled_for(&self, specifier: &str, profile_name: &str) -> bool {
        fn visit(
            lock: &SharedPackageLock,
            specifier: &str,
            profile_name: &str,
            visiting: &mut BTreeSet<String>,
            redirect_shadowed_leaf: bool,
        ) -> bool {
            let Some(package) = lock.governing_package(specifier, redirect_shadowed_leaf) else {
                return false;
            };
            if package.has_direct_activation() && package.is_enabled_for(profile_name) {
                return true;
            }
            if !visiting.insert(package.specifier.to_ascii_lowercase()) {
                return false;
            }
            let enabled = package
                .required_by
                .iter()
                .any(|parent| visit(lock, parent, profile_name, visiting, false));
            visiting.remove(&package.specifier.to_ascii_lowercase());
            enabled
        }

        visit(self, specifier, profile_name, &mut BTreeSet::new(), true)
    }

    /// Insert or replace an installed package by specifier.
    pub fn upsert(&mut self, package: LockedPackage) {
        if let Some(existing) = self.find_mut(&package.specifier) {
            *existing = package;
        } else {
            self.packages.push(package);
        }
    }

    /// Remove an installed package by specifier. Returns whether one was removed.
    pub fn remove(&mut self, specifier: &str) -> bool {
        let before = self.packages.len();
        self.packages.retain(|p| p.specifier != specifier);
        self.packages.len() != before
    }

    /// The auto-installed requirements that would become **orphans** if `removing` were
    /// uninstalled — i.e. nothing left would `require` them — so the uninstall flow can offer to
    /// remove them too (apt-style; never silent). `requires_of` maps each still-installed
    /// package's specifier to the specifiers it `requires`. The result is transitive.
    /// `removing` itself is never included. Order is deterministic (lockfile order).
    #[must_use]
    pub fn orphaned_by_removal(
        &self,
        removing: &str,
        requires_of: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        let mut seeds: std::collections::HashSet<&str> = std::collections::HashSet::new();
        seeds.insert(removing);
        self.orphans_after(&seeds, requires_of)
    }

    /// The installed packages that (transitively) `require` `removing` — reverse reachability over
    /// the `requires` graph. If `removing` is uninstalled, these are left requiring a package that is
    /// gone (broken), so the uninstall flow removes them alongside it. `removing` itself is never
    /// included; order is deterministic (lockfile order). See `script/REQUIRED-PACKAGES.md`.
    #[must_use]
    pub fn requirers_of_removal(
        &self,
        removing: &str,
        requires_of: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        let mut doomed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        doomed.insert(removing);
        loop {
            let next = self.packages.iter().find(|p| {
                !doomed.contains(p.specifier.as_str())
                    && requires_of
                        .get(&p.specifier)
                        .is_some_and(|reqs| reqs.iter().any(|r| doomed.contains(r.as_str())))
            });
            match next {
                Some(p) => {
                    doomed.insert(p.specifier.as_str());
                }
                None => break,
            }
        }
        self.packages
            .iter()
            .filter(|p| p.specifier != removing && doomed.contains(p.specifier.as_str()))
            .map(|p| p.specifier.clone())
            .collect()
    }

    /// Plan a removal of `removing`: the transitive dependents that would break and must be removed
    /// with it ([`RemovalPlan::breaks`]), plus the auto-installed requirements left unneeded once the
    /// whole set is gone ([`RemovalPlan::orphans`]).
    #[must_use]
    pub fn plan_removal(
        &self,
        removing: &str,
        requires_of: &HashMap<String, Vec<String>>,
    ) -> RemovalPlan {
        let breaks = self.requirers_of_removal(removing, requires_of);
        let mut seeds: std::collections::HashSet<&str> = std::collections::HashSet::new();
        seeds.insert(removing);
        for b in &breaks {
            seeds.insert(b.as_str());
        }
        let orphans = self.orphans_after(&seeds, requires_of);
        RemovalPlan { breaks, orphans }
    }

    /// Plans removal from the lockfile's durable flattened `required_by` links.
    #[must_use]
    pub fn plan_removal_from_links(&self, removing: &str) -> RemovalPlan {
        let mut requires_of: HashMap<String, Vec<String>> = HashMap::new();
        for package in &self.packages {
            for parent in &package.required_by {
                requires_of
                    .entry(parent.clone())
                    .or_default()
                    .push(package.specifier.clone());
            }
        }
        self.plan_removal(removing, &requires_of)
    }

    fn orphans_after(
        &self,
        seeds: &std::collections::HashSet<&str>,
        requires_of: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        let mut removed: std::collections::HashSet<&str> = seeds.clone();
        loop {
            let still_required: std::collections::HashSet<&str> = self
                .packages
                .iter()
                .filter(|p| !removed.contains(p.specifier.as_str()))
                .filter_map(|p| requires_of.get(&p.specifier))
                .flatten()
                .map(String::as_str)
                .collect();
            let next = self.packages.iter().find(|p| {
                p.installed_as_requirement
                    && !removed.contains(p.specifier.as_str())
                    && !still_required.contains(p.specifier.as_str())
            });
            match next {
                Some(p) => {
                    removed.insert(p.specifier.as_str());
                }
                None => break,
            }
        }
        self.packages
            .iter()
            .filter(|p| {
                !seeds.contains(p.specifier.as_str()) && removed.contains(p.specifier.as_str())
            })
            .map(|p| p.specifier.clone())
            .collect()
    }
}

/// The outcome of [`SharedPackageLock::plan_removal`] — what a single uninstall entails.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RemovalPlan {
    /// Installed packages that (transitively) `require` the target and would break if it were
    /// removed — removed alongside it.
    pub breaks: Vec<String>,
    /// Auto-installed requirements left unneeded once the target + `breaks` are gone (offered,
    /// not forced).
    pub orphans: Vec<String>,
}

/// Result of an optimistic uninstall commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UninstallCommit {
    /// The lock changed after the confirmation was prepared. The caller must show a fresh plan.
    Stale,
    /// The package remains because another installed root requires it; only direct-install intent
    /// was removed.
    DirectInstallRemoved,
    /// The target and the listed related packages were removed in one lockfile replacement.
    PackagesRemoved(Vec<String>),
}

/// Result of replacing a root's flattened requirement links.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequiredClosureCommit {
    Changed,
    Unchanged,
    Stale,
}

/// One independently-running package root pulled in by another package's `requires` list.
///
/// `already_satisfied` is part of the resolved install plan. Satisfied rows are verified and
/// linked but otherwise left untouched. New or upgraded rows are staged to `version` and receive
/// the permission union shown in the consent prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequiredPackageInstall {
    pub specifier: String,
    pub version: String,
    pub permissions: PackagePermissions,
    pub already_satisfied: bool,
}

/// `<smudgy_home>/<server>/` — where the server-wide package state lives.
fn server_dir(server_name: &str) -> Result<PathBuf> {
    Ok(get_smudgy_home()?.join(server_name))
}

/// A scoped view of package state while the server's package lock is held.
///
/// The lock is reentrant, so these methods simply delegate to the public API; the token exists
/// to make the lock's extent visible at call sites that also touch local package folders.
pub struct PackageStateTxn<'a> {
    server_name: &'a str,
}

impl PackageStateTxn<'_> {
    /// Loads the lockfile.
    ///
    /// # Errors
    /// Returns an error if the lockfile cannot be read or parsed.
    pub fn load_lock(&self) -> Result<SharedPackageLock> {
        load_lock(self.server_name)
    }

    pub(crate) fn mutate_lock<R>(
        &self,
        mutation: impl FnOnce(&mut SharedPackageLock) -> Result<(R, bool)>,
    ) -> Result<R> {
        mutate_lock(self.server_name, mutation)
    }

    pub(crate) fn remove_package_param_state(&self, specifier: &str) -> Result<()> {
        remove_package_param_state(self.server_name, specifier)
    }

    pub(crate) fn copy_package_param_state(&self, from: &str, to: &str) -> Result<()> {
        copy_package_param_state(self.server_name, from, to)
    }

    pub(crate) fn remove_lock_entry_if_unchanged(&self, expected: &LockedPackage) -> Result<bool> {
        mutate_lock(self.server_name, |lock| {
            remove_package_lock_entry_if_unchanged_in(lock, expected)
        })
    }

    pub(crate) fn rename_local_package_state(
        &self,
        old_name: &str,
        new_name: &str,
    ) -> Result<bool> {
        rename_local_package_state(self.server_name, old_name, new_name)
    }
}

/// Runs one operation that spans local package folders and persisted package state under the
/// server's package lock.
///
/// # Errors
/// Returns the operation's error.
pub fn with_local_package_transaction<R>(
    server_name: &str,
    operation: impl FnOnce(&PackageStateTxn<'_>) -> Result<R>,
) -> Result<R> {
    let _guard = guard(server_name);
    operation(&PackageStateTxn { server_name })
}

// ---------------------------------------------------------------------------
// Lockfile
// ---------------------------------------------------------------------------

/// Loads a server's package lockfile. A missing file yields an empty lock.
///
/// # Errors
/// Returns an error if the home dir can't be located, or the file exists but can't be
/// read or parsed.
pub fn load_lock(server_name: &str) -> Result<SharedPackageLock> {
    let _guard = guard(server_name);
    retry_secret_tombstones_once(server_name);
    load_lock_in(&server_dir(server_name)?)
}

fn retry_secret_tombstones_once(server_name: &str) {
    static ATTEMPTED: std::sync::OnceLock<Mutex<BTreeSet<String>>> = std::sync::OnceLock::new();
    let attempted = ATTEMPTED.get_or_init(|| Mutex::new(BTreeSet::new()));
    let should_retry = attempted
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(server_name.to_string());
    if should_retry && let Err(error) = retry_secret_tombstones(server_name) {
        warn!("Deferred package-secret cleanup is still pending: {error:#}");
    }
}

/// Saves a server's package lockfile, creating the server directory if needed.
///
/// # Errors
/// Returns an error if the lock can't be serialized or written.
pub fn save_lock(server_name: &str, lock: &SharedPackageLock) -> Result<()> {
    let _guard = guard(server_name);
    save_lock_in(&server_dir(server_name)?, lock)
}

/// Performs one serialized package-lock read/modify/write transaction.
///
/// The boolean returned by `mutation` says whether the resulting snapshot
/// needs to be written. This is crate-visible so the runtime resolver can
/// preserve its "do not resurrect an uninstalled package" rule inside the
/// same transaction as UI-originated metadata updates.
pub(crate) fn mutate_lock<R>(
    server_name: &str,
    mutation: impl FnOnce(&mut SharedPackageLock) -> Result<(R, bool)>,
) -> Result<R> {
    let _guard = guard(server_name);
    let dir = server_dir(server_name)?;
    let mut lock = load_lock_in(&dir)?;
    let (result, changed) = mutation(&mut lock)?;
    if changed {
        save_lock_in(&dir, &lock)?;
    }
    Ok(result)
}

fn load_lock_in(dir: &Path) -> Result<SharedPackageLock> {
    let path = dir.join(LOCK_FILE);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(SharedPackageLock::default()),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn save_lock_in(dir: &Path, lock: &SharedPackageLock) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create server dir {}", dir.display()))?;
    let path = dir.join(LOCK_FILE);
    let json = serde_json::to_string_pretty(lock).context("Failed to serialize package lock")?;
    write_atomic(&path, json.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// The specifier whose lock row governs `requested_specifier`'s leaf once local folders are
/// considered: a local folder with the same leaf name always wins.
fn authoritative_governing_specifier(
    server_name: &str,
    requested_specifier: &str,
) -> Result<String> {
    let requested =
        smudgy_script::SmudgySpecifier::parse(requested_specifier).map_err(|error| {
            anyhow::anyhow!("invalid package specifier {requested_specifier}: {error}")
        })?;
    let local_names =
        crate::models::local_packages::list_local_packages_in(&get_smudgy_home()?, server_name)?;
    Ok(local_names
        .into_iter()
        .find(|name| name.eq_ignore_ascii_case(&requested.name))
        .map_or_else(
            || requested_specifier.to_string(),
            |name| local_state_specifier(&name),
        ))
}

/// Whether `expected` is still exactly the row that governs its own leaf.
fn row_is_current(
    server_name: &str,
    lock: &SharedPackageLock,
    expected: &LockedPackage,
) -> Result<bool> {
    if authoritative_governing_specifier(server_name, &expected.specifier)? != expected.specifier {
        return Ok(false);
    }
    Ok(lock.find(&expected.specifier) == Some(expected)
        && lock
            .governing_package(&expected.specifier, true)
            .is_some_and(|governing| governing == expected))
}

// ---------------------------------------------------------------------------
// Install / uninstall
// ---------------------------------------------------------------------------

/// Installs a package for a server (auto-update unless `mode` says otherwise), replacing
/// any existing entry for the same specifier. `enabled` is written as part of the same lock
/// write so an "install, don't enable" never transiently persists as `enabled: true`.
///
/// # Errors
/// Returns an error if the lockfile can't be loaded or saved.
pub fn install_package(
    server_name: &str,
    specifier: &str,
    mode: UpdateMode,
    enabled: bool,
) -> Result<()> {
    mutate_lock(server_name, |lock| {
        ensure_new_package_rows_have_no_retired_parameter_state(
            server_name,
            lock,
            std::iter::once(specifier),
        )?;
        let mut package = lock
            .find(specifier)
            .cloned()
            .unwrap_or_else(|| LockedPackage::new(specifier, mode.clone()));
        package.mode = mode;
        package.set_activation(ProfileActivation::from_legacy(enabled));
        // An explicit install means the user owns this package: clear the auto-installed mark so a
        // later orphan sweep never offers to remove it.
        package.installed_as_requirement = false;
        lock.upsert(package);
        Ok(((), true))
    })
}

/// Inserts a missing direct package with one complete activation value.
///
/// This is used when a Settings action must create its governing row. It fails if any
/// case-equivalent row already exists so the caller reloads instead of overwriting newer settings.
///
/// # Errors
/// Returns an error if a row exists or the lockfile cannot be written.
pub fn install_package_with_activation(
    server_name: &str,
    specifier: &str,
    mode: UpdateMode,
    activation: ProfileActivation,
) -> Result<()> {
    match install_package_with_activation_if_unchanged(server_name, specifier, mode, activation)? {
        Cas::Applied => Ok(()),
        Cas::StateChanged => {
            anyhow::bail!("package {specifier} appeared after its settings were loaded")
        }
    }
}

/// Inserts a missing authoritative row only while no row exists for it and no same-leaf local
/// package shadows it.
///
/// # Errors
/// Returns an error if local folders or the lockfile cannot be read or written.
pub fn install_package_with_activation_if_unchanged(
    server_name: &str,
    specifier: &str,
    mode: UpdateMode,
    activation: ProfileActivation,
) -> Result<Cas> {
    let _guard = guard(server_name);
    if authoritative_governing_specifier(server_name, specifier)? != specifier {
        return Ok(Cas::StateChanged);
    }
    mutate_lock(server_name, |lock| {
        if lock
            .packages
            .iter()
            .any(|package| package.specifier.eq_ignore_ascii_case(specifier))
        {
            return Ok((Cas::StateChanged, false));
        }
        ensure_new_package_rows_have_no_retired_parameter_state(
            server_name,
            lock,
            std::iter::once(specifier),
        )?;
        let mut package = LockedPackage::new(specifier, mode);
        package.set_activation(activation);
        package.installed_as_requirement = false;
        lock.upsert(package);
        Ok((Cas::Applied, true))
    })
}

/// Installs one explicit package and applies its complete `requires` plan in one lockfile write,
/// only if the complete lockfile still matches the snapshot used to resolve the plan.
///
/// This is the commit point for the install-consent flow. The root, every newly installed or
/// upgraded required root, their consent baselines, their exact resolved versions, and the
/// flattened `required_by` links are written together.
///
/// Existing satisfying required roots retain their update mode, activation, trust, consent,
/// parameter scope, and install provenance. Existing unsatisfied roots retain those independent
/// settings too, except that their staged version is advanced to the version the user accepted.
/// Newly materialized required roots start in Auto mode with no direct activation and are marked
/// as automatically installed. Their effective activation follows active `required_by` roots.
///
/// Returns `false` when another writer changed package state and nothing was written; callers
/// must resolve and present the plan again.
///
/// # Errors
/// Returns an error for an invalid or contradictory plan, a missing row marked satisfied, or a
/// lockfile read/write failure.
#[allow(clippy::too_many_arguments)]
pub fn install_package_with_requirements_if_unchanged(
    server_name: &str,
    expected_lock: &SharedPackageLock,
    root_specifier: &str,
    root_version: &str,
    root_permissions: &PackagePermissions,
    root_mode: UpdateMode,
    root_activation: ProfileActivation,
    required: &[RequiredPackageInstall],
) -> Result<bool> {
    smudgy_script::SmudgySpecifier::parse(root_specifier)
        .map_err(|error| anyhow::anyhow!("invalid package specifier {root_specifier}: {error}"))?;
    let root_version = semver::Version::parse(root_version)?.to_string();
    validate_required_install_plan(root_specifier, required)?;

    mutate_lock(server_name, |lock| {
        if expected_lock != lock {
            return Ok((false, false));
        }
        validate_satisfied_required_rows(lock, required)?;
        ensure_new_package_rows_have_no_retired_parameter_state(
            server_name,
            lock,
            std::iter::once(root_specifier)
                .chain(required.iter().map(|item| item.specifier.as_str())),
        )?;

        let mut root = lock
            .find(root_specifier)
            .cloned()
            .unwrap_or_else(|| LockedPackage::new(root_specifier, UpdateMode::Auto));
        root.mode = root_mode.clone();
        root.set_activation(root_activation.clone());
        root.installed_as_requirement = false;
        root.consented_permissions = Some(root_permissions.clone());
        root.stage(&root_version);
        lock.upsert(root);

        apply_required_rows(lock, required);
        // Replace this root's flattened relationship set, including satisfied rows. Links on
        // dormant same-leaf fallbacks are intentionally retained so deleting a local override can
        // restore the published package without losing its install provenance.
        replace_required_links(lock, root_specifier, required)?;
        Ok((true, true))
    })
}

fn validate_required_install_plan(
    root_specifier: &str,
    required: &[RequiredPackageInstall],
) -> Result<()> {
    let mut seen = BTreeSet::new();
    for item in required {
        smudgy_script::SmudgySpecifier::parse(&item.specifier).map_err(|error| {
            anyhow::anyhow!(
                "invalid required package specifier {}: {error}",
                item.specifier
            )
        })?;
        semver::Version::parse(&item.version).with_context(|| {
            format!(
                "required package {} has invalid version {}",
                item.specifier, item.version
            )
        })?;
        let folded = item.specifier.to_ascii_lowercase();
        if folded == root_specifier.to_ascii_lowercase() {
            anyhow::bail!("package {root_specifier} cannot require itself");
        }
        if !seen.insert(folded) {
            anyhow::bail!(
                "required package {} is listed more than once",
                item.specifier
            );
        }
    }
    Ok(())
}

fn validate_satisfied_required_rows(
    lock: &SharedPackageLock,
    required: &[RequiredPackageInstall],
) -> Result<()> {
    for item in required.iter().filter(|item| item.already_satisfied) {
        let package = lock.find(&item.specifier).with_context(|| {
            format!(
                "required package {} was marked satisfied but is not installed",
                item.specifier
            )
        })?;
        let is_local_state = smudgy_script::SmudgySpecifier::parse(&item.specifier)
            .is_ok_and(|specifier| is_local_owner(&specifier.owner));
        if is_local_state {
            continue;
        }
        let staged = package.staged_version().with_context(|| {
            format!(
                "required package {} was marked satisfied but has no staged version",
                item.specifier
            )
        })?;
        if semver::Version::parse(staged).ok() != semver::Version::parse(&item.version).ok() {
            anyhow::bail!(
                "required package {} changed from planned version {} to {staged}",
                item.specifier,
                item.version
            );
        }
    }
    Ok(())
}

fn apply_required_rows(lock: &mut SharedPackageLock, required: &[RequiredPackageInstall]) {
    for item in required.iter().filter(|item| !item.already_satisfied) {
        let (mut package, is_new) = match lock.find(&item.specifier) {
            Some(existing) => (existing.clone(), false),
            None => (LockedPackage::new(&item.specifier, UpdateMode::Auto), true),
        };
        if is_new {
            package.set_activation(ProfileActivation::None);
            package.installed_as_requirement = true;
            package.requirement_lineage_known = true;
        }
        package.consented_permissions = Some(item.permissions.clone());
        package.stage(&item.version);
        lock.upsert(package);
    }
}

fn replace_required_links(
    lock: &mut SharedPackageLock,
    root_specifier: &str,
    required: &[RequiredPackageInstall],
) -> Result<()> {
    let required_specifiers = required
        .iter()
        .map(|item| item.specifier.as_str())
        .collect::<BTreeSet<_>>();
    for specifier in &required_specifiers {
        if lock.find(specifier).is_none() {
            anyhow::bail!("required package {specifier} is not installed");
        }
    }
    for package in &mut lock.packages {
        let had_link = package.required_by.remove(root_specifier);
        let needs_link = required_specifiers.contains(package.specifier.as_str());
        if needs_link {
            package.required_by.insert(root_specifier.to_string());
        }
        if had_link || needs_link {
            package.requirement_lineage_known = true;
        }
    }
    Ok(())
}

/// Replaces a root's flattened requirement links only while its staged version is unchanged.
///
/// This is the async graph-resolver commit point: an uninstall or version change that lands while
/// the cloud response is in flight makes the result stale instead of resurrecting old links.
///
/// # Errors
/// Returns an error for a missing required row or lockfile I/O failure.
pub fn set_required_closure_if_staged_unchanged(
    server_name: &str,
    root_specifier: &str,
    expected_staged: Option<&str>,
    required_specifiers: &[String],
) -> Result<RequiredClosureCommit> {
    let required = required_specifiers
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    mutate_lock(server_name, |lock| {
        let Some(root) = lock.find(root_specifier) else {
            return Ok((RequiredClosureCommit::Stale, false));
        };
        if root.staged_version() != expected_staged {
            return Ok((RequiredClosureCommit::Stale, false));
        }
        for specifier in &required {
            if lock.find(specifier).is_none() {
                anyhow::bail!("required package {specifier} is not installed");
            }
        }
        let mut changed = false;
        for package in &mut lock.packages {
            let had_link = package.required_by.remove(root_specifier);
            let needs_link = required.contains(package.specifier.as_str());
            if needs_link {
                package.required_by.insert(root_specifier.to_string());
            }
            let learned_lineage = (had_link || needs_link) && !package.requirement_lineage_known;
            if had_link || needs_link {
                package.requirement_lineage_known = true;
            }
            changed |= had_link != needs_link || learned_lineage;
        }
        Ok((
            if changed {
                RequiredClosureCommit::Changed
            } else {
                RequiredClosureCommit::Unchanged
            },
            changed,
        ))
    })
}

/// Removes a package from a server, together with its parameter state.
///
/// # Errors
/// Returns an error if the lockfile can't be loaded or saved, or parameter cleanup fails after
/// the row was removed (a later installation retries that cleanup).
pub fn uninstall_package(server_name: &str, specifier: &str) -> Result<()> {
    let _guard = guard(server_name);
    let removed = mutate_lock(server_name, |lock| {
        let removed = lock.remove(specifier);
        if removed {
            for package in &mut lock.packages {
                package.required_by.remove(specifier);
            }
        }
        Ok((removed, removed))
    })?;
    if removed || package_param_state_exists(server_name, specifier)? {
        remove_package_param_state(server_name, specifier).with_context(|| {
            format!(
                "package {specifier} was removed, but its settings still need cleanup; retry the uninstall or installation"
            )
        })?;
    }
    Ok(())
}

/// Applies the uninstall confirmation only if the complete package lock is unchanged.
///
/// The impact is recomputed from durable `required_by` links inside the serialized lock mutation.
/// A direct package that is still required is demoted to an automatic requirement instead of
/// being deleted. Otherwise the target, forced dependents, and (when requested) newly orphaned
/// automatic requirements are removed in one lockfile replacement.
///
/// # Errors
/// Returns an error for an absent/non-direct target or a lockfile/cleanup failure.
pub fn commit_uninstall_if_unchanged(
    server_name: &str,
    expected: &SharedPackageLock,
    specifier: &str,
    remove_orphans: bool,
) -> Result<UninstallCommit> {
    let _guard = guard(server_name);
    let (outcome, removed) = mutate_lock(server_name, |lock| {
        if lock != expected {
            return Ok(((UninstallCommit::Stale, Vec::new()), false));
        }
        let target = lock
            .find(specifier)
            .with_context(|| format!("package {specifier} is not installed"))?;
        if !target.has_direct_activation() {
            anyhow::bail!("package {specifier} has no direct install to remove");
        }
        if !target.required_by.is_empty() {
            let target = lock.find_mut(specifier).expect("target was found above");
            target.installed_as_requirement = true;
            target.requirement_lineage_known = true;
            return Ok(((UninstallCommit::DirectInstallRemoved, Vec::new()), true));
        }

        let plan = lock.plan_removal_from_links(specifier);
        let mut removed = vec![specifier.to_string()];
        removed.extend(plan.breaks);
        if remove_orphans {
            removed.extend(plan.orphans);
        }
        let removed_set = removed.iter().cloned().collect::<BTreeSet<_>>();
        lock.packages
            .retain(|package| !removed_set.contains(&package.specifier));
        for package in &mut lock.packages {
            package
                .required_by
                .retain(|parent| !removed_set.contains(parent));
        }
        Ok((
            (UninstallCommit::PackagesRemoved(removed.clone()), removed),
            true,
        ))
    })?;

    let mut cleanup_failures = Vec::new();
    for removed_specifier in removed {
        if let Err(error) = remove_package_param_state(server_name, &removed_specifier) {
            cleanup_failures.push(format!("{removed_specifier}: {error:#}"));
        }
    }
    if !cleanup_failures.is_empty() {
        anyhow::bail!(
            "packages were removed, but their settings still need cleanup; retry an installation or remove the package settings: {}",
            cleanup_failures.join("; ")
        );
    }
    Ok(outcome)
}

/// Removes a package only if its complete lock row still equals `expected`.
///
/// This is used by asynchronous stale-entry reconciliation. A user edit, reinstall, resolution,
/// or activation change made while the cloud check is in flight changes the row and makes this a
/// no-op instead of allowing an old response to erase newer state. Returns `true` only when the
/// row was removed.
///
/// # Errors
/// Returns an error if the lockfile cannot be read or written.
pub fn uninstall_package_if_unchanged(server_name: &str, expected: &LockedPackage) -> Result<bool> {
    let _guard = guard(server_name);
    let removed = mutate_lock(server_name, |lock| {
        remove_package_lock_entry_if_unchanged_in(lock, expected)
    })?;
    // Settings are cleaned up only for a row this call removed. A stale `expected` means the
    // package is still installed under newer state, and its values and secrets stay with it.
    if removed {
        remove_package_param_state(server_name, &expected.specifier).with_context(|| {
            format!(
                "package {} was removed, but its settings still need cleanup; retry the uninstall or installation",
                expected.specifier
            )
        })?;
    }
    Ok(removed)
}

fn remove_package_lock_entry_if_unchanged_in(
    lock: &mut SharedPackageLock,
    expected: &LockedPackage,
) -> Result<(bool, bool)> {
    let Some(index) = lock
        .packages
        .iter()
        .position(|package| package.specifier == expected.specifier)
    else {
        return Ok((false, false));
    };
    if &lock.packages[index] != expected {
        return Ok((false, false));
    }
    lock.packages.remove(index);
    for package in &mut lock.packages {
        package.required_by.remove(&expected.specifier);
    }
    Ok((true, true))
}

/// Reconciles installs under the reserved [`LOCAL_OWNER`] placeholder with reality: an entry
/// whose backing `<server>/packages/<name>/` folder no longer exists is removed, because it can
/// never resolve again. Account-owned installs are never touched. Returns the removed specifiers.
///
/// Only the lock row goes. The identity's parameter values and secrets stay on disk: a folder
/// that is merely absent (moved out, mid-sync, or an interrupted rename) is not a deletion the
/// user confirmed, and a package re-created under that name adopts them. Explicit delete and
/// uninstall are the paths that clean settings up.
///
/// # Errors
/// Returns an error if the lockfile can't be loaded or saved, or the packages directory can't
/// be read.
pub fn reconcile_local_installs(server_name: &str) -> Result<Vec<String>> {
    let _guard = guard(server_name);
    // Discovery propagates directory failures. Do not use `Path::exists`: it turns an access
    // error into `false`, which could erase activation, trust, and settings during a transient
    // filesystem failure.
    let local_names =
        crate::models::local_packages::list_local_packages_in(&get_smudgy_home()?, server_name)?;
    let candidates = load_lock(server_name)?
        .packages
        .into_iter()
        .filter_map(|package| {
            let parsed = smudgy_script::SmudgySpecifier::parse(&package.specifier).ok()?;
            is_local_owner(&parsed.owner).then_some((package, parsed.name))
        })
        .collect::<Vec<_>>();
    let mut changed = Vec::new();
    for (expected, name) in candidates {
        if !local_names
            .iter()
            .any(|local_name| local_name.eq_ignore_ascii_case(&name))
            && mutate_lock(server_name, |lock| {
                remove_package_lock_entry_if_unchanged_in(lock, &expected)
            })?
        {
            changed.push(expected.specifier);
        }
    }
    Ok(changed)
}

// ---------------------------------------------------------------------------
// Row settings
// ---------------------------------------------------------------------------

/// Mutates one row only while it still equals `expected` and still governs its leaf.
fn mutate_row_if_unchanged(
    server_name: &str,
    expected: &LockedPackage,
    mutation: impl FnOnce(&mut LockedPackage),
) -> Result<Cas> {
    let _guard = guard(server_name);
    mutate_lock(server_name, |lock| {
        if !row_is_current(server_name, lock, expected)? {
            return Ok((Cas::StateChanged, false));
        }
        let package = lock
            .find_mut(&expected.specifier)
            .context("the package row disappeared while its settings were being changed")?;
        mutation(package);
        let changed = *package != *expected;
        Ok((Cas::Applied, changed))
    })
}

/// Sets the update mode (auto vs pinned) only while the complete package row still equals
/// `expected`. A changed row is never rewritten; callers reload and run their update planner.
///
/// # Errors
/// Returns an error if the lockfile cannot be read or written.
pub fn set_update_mode_if_unchanged(
    server_name: &str,
    expected: &LockedPackage,
    mode: UpdateMode,
) -> Result<Cas> {
    mutate_row_if_unchanged(server_name, expected, |package| {
        let prior_staged = package.staged_version().map(str::to_string);
        package.mode = mode;
        if package.staged_version() != prior_staged.as_deref() {
            package.integrity = None;
        }
    })
}

/// Sets whether an already-installed package is **trusted** (promoted onto the allow-all main
/// isolate) — the server-wide decision behind the trust toggle. Takes effect on the next session
/// reload.
///
/// # Errors
/// Returns an error if the package isn't installed, or the lockfile can't be saved.
pub fn set_trusted(server_name: &str, specifier: &str, trusted: bool) -> Result<()> {
    mutate_lock(server_name, |lock| {
        let package = lock
            .find_mut(specifier)
            .with_context(|| format!("package {specifier} is not installed"))?;
        package.trusted = trusted;
        Ok(((), true))
    })
}

/// Sets trust on the row governing `requested_specifier` only while that row still equals the
/// pane snapshot. A same-leaf local package appearing or any intervening setting change returns
/// [`Cas::StateChanged`] without rewriting current state.
///
/// # Errors
/// Returns an error if local folders or the lockfile cannot be read or written.
pub fn set_governing_trusted_if_unchanged(
    server_name: &str,
    requested_specifier: &str,
    expected: &LockedPackage,
    trusted: bool,
) -> Result<Cas> {
    let _guard = guard(server_name);
    if authoritative_governing_specifier(server_name, requested_specifier)? != expected.specifier {
        return Ok(Cas::StateChanged);
    }
    mutate_row_if_unchanged(server_name, expected, |package| package.trusted = trusted)
}

/// Sets the complete direct activation for an installed package.
///
/// # Errors
/// Returns an error if the package isn't installed, or the lockfile can't be saved.
pub fn set_activation(
    server_name: &str,
    specifier: &str,
    activation: ProfileActivation,
) -> Result<()> {
    mutate_lock(server_name, |lock| {
        let package = lock
            .find_mut(specifier)
            .with_context(|| format!("package {specifier} is not installed"))?;
        package.set_activation(activation);
        Ok(((), true))
    })
}

/// Sets activation on the row governing `requested_specifier` only while that row still equals
/// the pane snapshot.
///
/// # Errors
/// Returns an error if local folders or the lockfile cannot be read or written.
pub fn set_governing_activation_if_unchanged(
    server_name: &str,
    requested_specifier: &str,
    expected: &LockedPackage,
    activation: ProfileActivation,
) -> Result<Cas> {
    let _guard = guard(server_name);
    if authoritative_governing_specifier(server_name, requested_specifier)? != expected.specifier {
        return Ok(Cas::StateChanged);
    }
    mutate_row_if_unchanged(server_name, expected, |package| {
        package.set_activation(activation);
    })
}

/// Removes one deleted profile from every installed package activation without interpreting any
/// other profile name.
///
/// # Errors
/// Returns an error if the lockfile cannot be loaded or saved.
pub fn remove_profile_activation(server_name: &str, profile_name: &str) -> Result<()> {
    mutate_lock(server_name, |lock| {
        let mut changed = false;
        for package in &mut lock.packages {
            let activation = package.activation();
            let updated = activation.clone().without_profile(profile_name);
            changed |= updated != activation;
            package.set_activation(updated);
        }
        Ok(((), changed))
    })
}

/// Records that one installed package successfully prepared a sandboxed Web
/// Audio context. Returns `true` only when the durable bit changed.
///
/// # Errors
/// Returns an error if the package is absent or the lockfile cannot be saved.
pub fn record_audio_use(server_name: &str, owner: &str, name: &str) -> Result<bool> {
    let specifier = format!("smudgy://{owner}/{name}");
    mutate_lock(server_name, |lock| {
        let package = lock
            .packages
            .iter_mut()
            .find(|package| package.specifier.eq_ignore_ascii_case(&specifier))
            .with_context(|| format!("package {specifier} is not installed"))?;
        if package.audio_used {
            return Ok((false, false));
        }
        package.audio_used = true;
        Ok((true, true))
    })
}

/// Records the deno-native permission union the user consented to for an already-installed
/// package — the all-or-nothing grant the install/update confirmation captures. The engine
/// enforces exactly this for the package's sandboxed isolate, and an update's delta is computed
/// against it.
///
/// # Errors
/// Returns an error if the package isn't installed, or the lockfile can't be saved.
pub fn record_consent(
    server_name: &str,
    specifier: &str,
    permissions: &PackagePermissions,
) -> Result<()> {
    mutate_lock(server_name, |lock| {
        let package = lock
            .find_mut(specifier)
            .with_context(|| format!("package {specifier} is not installed"))?;
        package.consented_permissions = Some(permissions.clone());
        Ok(((), true))
    })
}

/// Replaces consent only if the complete package row still matches the caller's snapshot.
/// Returns `false` when any user or runtime change made the snapshot stale.
///
/// # Errors
/// Returns an error if the lockfile cannot be read or written.
pub fn record_consent_if_unchanged(
    server_name: &str,
    expected: &LockedPackage,
    permissions: &PackagePermissions,
) -> Result<bool> {
    mutate_lock(server_name, |lock| {
        let Some(package) = lock.find_mut(&expected.specifier) else {
            return Ok((false, false));
        };
        if package != expected {
            return Ok((false, false));
        }
        let changed = package.consented_permissions.as_ref() != Some(permissions);
        package.consented_permissions = Some(permissions.clone());
        Ok((true, changed))
    })
}

/// Records the version + integrity a package most recently resolved to (called after a
/// successful fetch, so an auto package can reuse it offline next load).
///
/// # Errors
/// Returns an error if the lockfile can't be loaded or saved.
pub fn record_resolution(
    server_name: &str,
    specifier: &str,
    version: &str,
    integrity: &str,
) -> Result<()> {
    mutate_lock(server_name, |lock| {
        if let Some(entry) = lock.find_mut(specifier) {
            entry.last_resolved_version = Some(version.to_string());
            entry.integrity = Some(integrity.to_string());
        } else {
            ensure_new_package_rows_have_no_retired_parameter_state(
                server_name,
                lock,
                std::iter::once(specifier),
            )?;
            let mut package = LockedPackage::new(specifier, UpdateMode::Auto);
            package.last_resolved_version = Some(version.to_string());
            package.integrity = Some(integrity.to_string());
            lock.packages.push(package);
        }
        Ok(((), true))
    })
}

/// Advances the version an installed package is **staged** to serve from *outside* a session —
/// the background checker's write once it has verified `new_version` fits the consented grant and
/// prefetched its content. Returns the `last_resolved_version` it replaced. Only an existing entry
/// is mutated, never inserted, so an install uninstalled between the caller's sweep and this write
/// stays uninstalled.
///
/// # Errors
/// Returns an error if the package isn't installed (nothing is written), or the
/// lockfile can't be loaded or saved.
pub fn stage_resolved_version(
    server_name: &str,
    specifier: &str,
    new_version: &str,
) -> Result<Option<String>> {
    mutate_lock(server_name, |lock| {
        let package = lock
            .find_mut(specifier)
            .with_context(|| format!("package {specifier} is not installed"))?;
        let previous = package
            .last_resolved_version
            .replace(new_version.to_string());
        package.integrity = None;
        if package
            .dismissed_update_version
            .as_deref()
            .is_some_and(|dismissed| !version_is_newer(dismissed, new_version))
        {
            package.dismissed_update_version = None;
        }
        Ok((previous, true))
    })
}

/// Stages one background update only while its complete lock row still matches the snapshot that
/// was evaluated before the network request. An optional accepted consent union is committed in
/// the same lockfile replacement. Returns `true` only when the snapshot still matched.
///
/// # Errors
/// Returns an error if `expected` is not an auto-update row or the lockfile cannot be read or
/// written.
pub fn stage_auto_update_if_unchanged(
    server_name: &str,
    expected: &LockedPackage,
    new_version: &str,
    accepted_consent: Option<&PackagePermissions>,
) -> Result<bool> {
    if !matches!(&expected.mode, UpdateMode::Auto) {
        anyhow::bail!(
            "cannot apply a background auto-update to pinned package {}",
            expected.specifier
        );
    }
    let mut staged = expected.clone();
    staged.stage(new_version);
    if let Some(permissions) = accepted_consent {
        staged.consented_permissions = Some(permissions.clone());
    }
    mutate_lock(server_name, |lock| {
        let Some(current) = lock.find_mut(&expected.specifier) else {
            return Ok((false, false));
        };
        if current != expected {
            return Ok((false, false));
        }
        let changed = *current != staged;
        if changed {
            *current = staged;
        }
        Ok((true, changed))
    })
}

/// Records a dismissed update only if the complete package row still matches the snapshot that
/// produced the offer. Returns `false` when the visible offer became stale.
///
/// # Errors
/// Returns an error if the lockfile cannot be read or written.
pub fn set_dismissed_update_version_if_unchanged(
    server_name: &str,
    expected: &LockedPackage,
    version: &str,
) -> Result<bool> {
    mutate_lock(server_name, |lock| {
        let Some(package) = lock.find_mut(&expected.specifier) else {
            return Ok((false, false));
        };
        if package != expected {
            return Ok((false, false));
        }
        let changed = package.dismissed_update_version.as_deref() != Some(version);
        package.dismissed_update_version = Some(version.to_string());
        Ok((true, changed))
    })
}

/// Whether `candidate` names a strictly newer version than `baseline` — semver order
/// when both parse, else a conservative "different means newer" so an unparseable
/// dismissal is kept rather than silently cleared.
fn version_is_newer(candidate: &str, baseline: &str) -> bool {
    match (
        semver::Version::parse(candidate),
        semver::Version::parse(baseline),
    ) {
        (Ok(candidate), Ok(baseline)) => candidate > baseline,
        _ => candidate != baseline,
    }
}

// ---------------------------------------------------------------------------
// Local package rename and manifest commits
// ---------------------------------------------------------------------------

/// Migrates a local folder identity and its governing state row from `old_name` to `new_name`.
///
/// The new identity is built up completely before the old one is retired: parameter values and
/// secrets are copied first, then the new lock row is added beside the old one, then the folder
/// moves, and only then are the old row, its `required_by` links, and its settings removed.
/// Every interruption point leaves either a complete old package or a complete new one. The
/// worst case is orphaned settings for the retired name, which [`reconcile_local_installs`]
/// never deletes and a package re-created under that name adopts.
///
/// # Errors
/// Returns an error for a target collision, missing source, or any failed write.
pub(crate) fn rename_local_package_state(
    server_name: &str,
    old_name: &str,
    new_name: &str,
) -> Result<bool> {
    if old_name == new_name {
        return Ok(false);
    }
    let _guard = guard(server_name);
    let dir = server_dir(server_name)?;
    let packages_dir = dir.join("packages");
    let from = packages_dir.join(old_name);
    let to = packages_dir.join(new_name);
    if !from.is_dir() {
        anyhow::bail!("no local package named {old_name}");
    }
    for entry in fs::read_dir(&packages_dir)
        .with_context(|| format!("read local package directory {}", packages_dir.display()))?
    {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.eq_ignore_ascii_case(new_name) && name != old_name {
            anyhow::bail!("a package named {new_name} already exists");
        }
    }

    let old_state = local_state_specifier(old_name);
    let new_state = local_state_specifier(new_name);
    let lock = load_lock_in(&dir)?;
    let mut destination_required_by = None;
    for package in &lock.packages {
        let Ok(parsed) = smudgy_script::SmudgySpecifier::parse(&package.specifier) else {
            continue;
        };
        if !parsed.name.eq_ignore_ascii_case(new_name) {
            continue;
        }
        if is_local_owner(&parsed.owner) {
            anyhow::bail!(
                "package name {new_name} is already used by installed package {}",
                package.specifier
            );
        }
        if destination_required_by
            .replace(package.required_by.clone())
            .is_some()
        {
            anyhow::bail!(
                "more than one published package named {new_name} is installed; remove the conflicting rows before renaming"
            );
        }
    }
    // Settings already stored under the target identity can only be orphans of an interrupted
    // rename or delete (a governing row for the name was ruled out above); the copy overwrites
    // them.
    let mut destination_required_by = destination_required_by.unwrap_or_default();
    destination_required_by.retain(|parent| {
        !parent.eq_ignore_ascii_case(&old_state) && !parent.eq_ignore_ascii_case(&new_state)
    });

    // 1. Values and secrets under the new identity. Nothing else has moved if this fails.
    if let Err(error) = copy_package_param_state(server_name, &old_state, &new_state) {
        if let Err(cleanup) = remove_package_param_state(server_name, &new_state) {
            warn!("Could not discard partially copied settings for {new_state}: {cleanup:#}");
        }
        return Err(error);
    }

    // 2. The new governing row, beside the old one.
    let new_row = lock.find(&old_state).cloned().map(|mut row| {
        row.specifier.clone_from(&new_state);
        row.integrity = None;
        row.required_by.clone_from(&destination_required_by);
        row.installed_as_requirement = false;
        row
    });
    if let Some(row) = new_row.clone() {
        mutate_lock(server_name, |lock| {
            lock.upsert(row);
            Ok(((), true))
        })?;
    }

    // 3. The folder. On failure the new identity is discarded again and the old one is intact.
    if let Err(error) = fs::rename(&from, &to)
        .with_context(|| format!("rename {} -> {}", from.display(), to.display()))
    {
        if new_row.is_some() {
            if let Err(cleanup) = mutate_lock(server_name, |lock| {
                let before = lock.packages.len();
                lock.packages
                    .retain(|package| package.specifier != new_state);
                Ok(((), lock.packages.len() != before))
            }) {
                warn!("Could not discard the unused row for {new_state}: {cleanup:#}");
            }
        }
        if let Err(cleanup) = remove_package_param_state(server_name, &new_state) {
            warn!("Could not discard copied settings for {new_state}: {cleanup:#}");
        }
        return Err(error);
    }

    // 4. Retire the old identity. The rename is complete from here; cleanup problems are
    //    reported, never allowed to undo it.
    mutate_lock(server_name, |lock| {
        let before = lock.packages.len();
        lock.packages
            .retain(|package| package.specifier != old_state);
        let mut changed = lock.packages.len() != before;
        for package in &mut lock.packages {
            if package.required_by.remove(&old_state) {
                package.required_by.insert(new_state.clone());
                changed = true;
            }
        }
        Ok(((), changed))
    })?;
    if let Err(error) = remove_package_param_state(server_name, &old_state) {
        warn!("Deferred cleanup of settings for renamed package {old_state}: {error:#}");
    }
    Ok(true)
}

/// Atomically replaces a local manifest whose `requires` declaration did not change.
///
/// # Errors
/// Returns an error for invalid or missing local package state or a failed write.
pub fn commit_local_manifest(
    server_name: &str,
    local_name: &str,
    root_specifier: &str,
    expected_manifest: &str,
    desired_manifest: &str,
) -> Result<LocalManifestCommit> {
    commit_local_manifest_inner(
        server_name,
        local_name,
        root_specifier,
        expected_manifest,
        desired_manifest,
        None,
        None,
    )
}

/// Accepts a local manifest and the complete required-package relationship state that was
/// resolved from it, only while the manifest and the complete package lock still match the
/// editor's snapshots.
///
/// # Errors
/// Returns an error for an invalid identity or plan, missing/stale package rows, or a failed
/// write.
pub fn commit_local_manifest_with_requirements_if_unchanged(
    server_name: &str,
    local_name: &str,
    root_specifier: &str,
    expected_manifest: &str,
    desired_manifest: &str,
    expected_lock: &SharedPackageLock,
    required: &[RequiredPackageInstall],
) -> Result<LocalManifestCommit> {
    commit_local_manifest_inner(
        server_name,
        local_name,
        root_specifier,
        expected_manifest,
        desired_manifest,
        Some(expected_lock),
        Some(required),
    )
}

fn commit_local_manifest_inner(
    server_name: &str,
    local_name: &str,
    root_specifier: &str,
    expected_manifest: &str,
    desired_manifest: &str,
    expected_lock: Option<&SharedPackageLock>,
    required: Option<&[RequiredPackageInstall]>,
) -> Result<LocalManifestCommit> {
    crate::models::naming::validate_package_name(local_name).map_err(anyhow::Error::msg)?;
    let parsed = smudgy_script::SmudgySpecifier::parse(root_specifier)
        .map_err(|error| anyhow::anyhow!("invalid package specifier {root_specifier}: {error}"))?;
    if !is_local_owner(&parsed.owner) || !parsed.name.eq_ignore_ascii_case(local_name) {
        anyhow::bail!(
            "local manifest {local_name} does not match governing state {root_specifier}"
        );
    }
    PackageManifest::parse(desired_manifest)
        .map_err(|error| anyhow::anyhow!("invalid desired local manifest: {error}"))?;
    if let Some(required) = required {
        validate_required_install_plan(root_specifier, required)?;
    }

    let _guard = guard(server_name);
    let manifest_path =
        crate::models::local_packages::local_manifest_path(server_name, local_name)?;
    let prior_manifest = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read {} before saving", manifest_path.display()))?;
    if prior_manifest != expected_manifest {
        return Ok(LocalManifestCommit::Stale);
    }

    let dir = server_dir(server_name)?;
    let mut lock = load_lock_in(&dir)?;
    if expected_lock.is_some_and(|expected| expected != &lock) {
        return Ok(LocalManifestCommit::StateChanged);
    }
    let matching_roots = lock
        .packages
        .iter()
        .filter(|package| package.specifier.eq_ignore_ascii_case(root_specifier))
        .count();
    if matching_roots != 1 {
        anyhow::bail!("local package {root_specifier} must have exactly one governing state row");
    }
    let governing_specifier = lock
        .packages
        .iter()
        .find(|package| package.specifier.eq_ignore_ascii_case(root_specifier))
        .map(|package| package.specifier.clone())
        .expect("one matching root was counted above");
    if let Some(required) = required {
        validate_satisfied_required_rows(&lock, required)?;
        ensure_new_package_rows_have_no_retired_parameter_state(
            server_name,
            &lock,
            required.iter().map(|item| item.specifier.as_str()),
        )?;
        apply_required_rows(&mut lock, required);
        replace_required_links(&mut lock, &governing_specifier, required)?;
    }

    write_atomic(&manifest_path, desired_manifest.as_bytes())
        .with_context(|| format!("write {}", manifest_path.display()))?;
    if required.is_some() {
        save_lock_in(&dir, &lock)?;
    }
    Ok(LocalManifestCommit::Applied)
}

// ---------------------------------------------------------------------------
// Parameter values
// ---------------------------------------------------------------------------

/// Non-secret parameter values, keyed by package specifier then parameter key.
pub type PackageParamValues = HashMap<String, HashMap<String, serde_json::Value>>;

/// The persisted location for a package parameter value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamValueScope<'a> {
    /// Store one value for every profile on the server.
    Global,
    /// Store a value for one profile only.
    Profile(&'a str),
}

/// One validated value change in a package-parameter save.
#[derive(Debug, Clone, PartialEq)]
pub enum PackageParamMutation {
    /// Store a non-secret JSON value.
    SetValue {
        key: String,
        value: serde_json::Value,
    },
    /// Remove a non-secret JSON value.
    ClearValue { key: String },
    /// Replace a write-only secret value.
    SetSecret { key: String, value: String },
}

/// Result of committing one package-parameter change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PackageParamCommit {
    Applied,
    /// The governing package row, authority, or parameter scope changed before the save acquired
    /// the package lock. No value was changed.
    StateChanged,
}

/// Copies every declared value and secret of `expected` from one profile to another, replacing
/// the destination's values for those keys. Only meaningful in per-profile scope: a global-scope
/// row, or one whose state changed since it was read, is reported as
/// [`PackageParamCommit::StateChanged`] and nothing is written.
///
/// # Errors
/// Returns an error for an invalid profile name or declared key, or a failed read or write.
pub fn copy_profile_param_values_if_unchanged(
    server_name: &str,
    expected: &LockedPackage,
    params: &[PackageParameter],
    from_profile: &str,
    to_profile: &str,
) -> Result<PackageParamCommit> {
    validate_param_profile_name(from_profile)?;
    validate_param_profile_name(to_profile)?;
    if from_profile == to_profile {
        anyhow::bail!("the source and destination profiles are the same");
    }
    let mut keys = BTreeSet::new();
    if params
        .iter()
        .any(|param| param.key.is_empty() || !keys.insert(param.key.as_str()))
    {
        anyhow::bail!("settings copy contains an invalid or duplicate declared key");
    }
    let _guard = guard(server_name);
    let lock = load_lock_in(&server_dir(server_name)?)?;
    if !row_is_current(server_name, &lock, expected)?
        || expected.parameter_scope != ParameterScope::Profile
    {
        return Ok(PackageParamCommit::StateChanged);
    }
    copy_param_values(
        server_name,
        &expected.specifier,
        params,
        ParamValueScope::Profile(from_profile),
        ParamValueScope::Profile(to_profile),
        false,
    )?;
    Ok(PackageParamCommit::Applied)
}

fn validate_param_profile_name(profile_name: &str) -> Result<()> {
    if profile_name.is_empty()
        || profile_name.contains(|character: char| {
            !character.is_alphanumeric() && character != '_' && character != '-'
        })
    {
        anyhow::bail!("Invalid profile name: {profile_name}");
    }
    Ok(())
}

fn param_dir(server_name: &str, scope: ParamValueScope<'_>) -> Result<PathBuf> {
    let server = server_dir(server_name)?;
    match scope {
        ParamValueScope::Global => Ok(server),
        ParamValueScope::Profile(profile_name) => {
            validate_param_profile_name(profile_name)?;
            Ok(server.join("profiles").join(profile_name))
        }
    }
}

fn load_param_values_in(dir: &Path) -> Result<PackageParamValues> {
    let path = dir.join(PARAMS_FILE);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display())),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(PackageParamValues::new()),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn save_param_values_in(dir: &Path, values: &PackageParamValues) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create server dir {}", dir.display()))?;
    let path = dir.join(PARAMS_FILE);
    let json = serde_json::to_string_pretty(values).context("Failed to serialize option values")?;
    write_atomic(&path, json.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Loads non-secret parameter values from one persisted scope.
///
/// # Errors
/// Returns an error if the file exists but can't be read or parsed.
pub fn load_param_values_scoped(
    server_name: &str,
    scope: ParamValueScope<'_>,
) -> Result<PackageParamValues> {
    let _guard = guard(server_name);
    load_param_values_in(&param_dir(server_name, scope)?)
}

/// Sets a single non-secret parameter value in the global scope.
///
/// # Errors
/// Returns an error if the values file can't be loaded or saved.
pub fn save_param_value(
    server_name: &str,
    specifier: &str,
    key: &str,
    value: serde_json::Value,
) -> Result<()> {
    save_param_value_scoped(server_name, ParamValueScope::Global, specifier, key, value)
}

/// Sets a single non-secret parameter value in one persisted scope.
///
/// # Errors
/// Returns an error if the values file can't be loaded or saved.
pub fn save_param_value_scoped(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    key: &str,
    value: serde_json::Value,
) -> Result<()> {
    let _guard = guard(server_name);
    let dir = param_dir(server_name, scope)?;
    let mut values = load_param_values_in(&dir)?;
    values
        .entry(specifier.to_string())
        .or_default()
        .insert(key.to_string(), value);
    save_param_values_in(&dir, &values)
}

/// Removes a single non-secret parameter value from one persisted scope. A no-op if the key was
/// never set; the package's entry is dropped once its last value is removed.
///
/// # Errors
/// Returns an error if the values file can't be loaded or saved.
pub fn clear_param_value_scoped(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    key: &str,
) -> Result<()> {
    let _guard = guard(server_name);
    let dir = param_dir(server_name, scope)?;
    let mut values = load_param_values_in(&dir)?;
    let Some(entry) = values.get_mut(specifier) else {
        return Ok(());
    };
    if entry.remove(key).is_none() {
        return Ok(());
    }
    if entry.is_empty() {
        values.remove(specifier);
    }
    save_param_values_in(&dir, &values)
}

/// Reads a non-secret parameter without treating an unreadable value store as an unset value.
///
/// # Errors
/// Returns an error if the scoped value file cannot be read or parsed.
pub fn get_param_value_scoped_checked(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    key: &str,
) -> Result<Option<serde_json::Value>> {
    let _guard = guard(server_name);
    let values = load_param_values_in(&param_dir(server_name, scope)?)?;
    Ok(values
        .get(specifier)
        .and_then(|params| params.get(key))
        .cloned())
}

/// Whether a declared parameter has an explicit value in one persisted scope, keeping
/// unavailable storage distinct from an unset parameter.
///
/// # Errors
/// Returns an error if the value file, secret metadata, or credential service is unavailable.
pub fn param_has_value_scoped_checked(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    param: &PackageParameter,
) -> Result<bool> {
    if param.secret {
        Ok(load_secret_param_scoped_checked(server_name, scope, specifier, &param.key)?.is_some())
    } else {
        Ok(get_param_value_scoped_checked(server_name, scope, specifier, &param.key)?.is_some())
    }
}

/// The required parameter keys that are unset in one persisted scope. Unknown storage state
/// blocks loading: every required key is reported when storage cannot be inspected.
#[must_use]
pub fn missing_required_params_scoped(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    params: &[PackageParameter],
) -> Vec<String> {
    missing_required_params_scoped_checked(server_name, scope, specifier, params).unwrap_or_else(
        |_| {
            params
                .iter()
                .filter(|param| param.required)
                .map(|param| param.key.clone())
                .collect()
        },
    )
}

/// Checked required-parameter inventory for one explicit value scope.
///
/// # Errors
/// Returns an error if any required parameter's storage cannot be inspected.
pub fn missing_required_params_scoped_checked(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    params: &[PackageParameter],
) -> Result<Vec<String>> {
    let _guard = guard(server_name);
    let mut missing = Vec::new();
    for param in params.iter().filter(|param| param.required) {
        if !param_has_value_scoped_checked(server_name, scope, specifier, param)? {
            missing.push(param.key.clone());
        }
    }
    Ok(missing)
}

/// The value scope a package reads for `profile_name`: its row's configured scope, or global for
/// a package with no row of its own (a pure dependency reads the server-wide store).
fn configured_param_scope<'a>(
    server_name: &str,
    profile_name: &'a str,
    specifier: &str,
) -> Result<ParamValueScope<'a>> {
    let lock = load_lock_in(&server_dir(server_name)?)?;
    let scope = lock
        .find(specifier)
        .or_else(|| lock.governing_package(specifier, true))
        .map(|package| package.parameter_scope)
        .unwrap_or_default();
    Ok(match scope {
        ParameterScope::Global => ParamValueScope::Global,
        ParameterScope::Profile => ParamValueScope::Profile(profile_name),
    })
}

/// Checked required-parameter gate for one running profile.
///
/// # Errors
/// Returns an error when required parameters exist and the package's parameter storage is
/// unavailable.
pub fn missing_required_params_for_profile_checked(
    server_name: &str,
    profile_name: &str,
    specifier: &str,
    params: &[PackageParameter],
) -> Result<Vec<String>> {
    if !params.iter().any(|param| param.required) {
        return Ok(Vec::new());
    }
    let _guard = guard(server_name);
    let scope = configured_param_scope(server_name, profile_name, specifier)?;
    missing_required_params_scoped_checked(server_name, scope, specifier, params)
}

/// Reads a configured package parameter for one running profile. A secret value is returned as
/// a JSON string.
#[must_use]
pub fn get_param_value_for_profile(
    server_name: &str,
    profile_name: &str,
    specifier: &str,
    key: &str,
) -> Option<serde_json::Value> {
    get_param_value_for_profile_checked(server_name, profile_name, specifier, key)
        .ok()
        .flatten()
}

/// Checked package-parameter read for one running profile.
///
/// # Errors
/// Returns an error when the package's configured scope or either value store is unavailable.
pub fn get_param_value_for_profile_checked(
    server_name: &str,
    profile_name: &str,
    specifier: &str,
    key: &str,
) -> Result<Option<serde_json::Value>> {
    let _guard = guard(server_name);
    let scope = configured_param_scope(server_name, profile_name, specifier)?;
    if let Some(value) = get_param_value_scoped_checked(server_name, scope, specifier, key)? {
        return Ok(Some(value));
    }
    Ok(
        load_secret_param_scoped_checked(server_name, scope, specifier, key)?
            .map(serde_json::Value::String),
    )
}

/// Validates a script-supplied value against one declared package parameter.
///
/// Lists and tables cannot contain nested containers. A table row can omit columns, but it
/// cannot contain undeclared columns. Dropdown values must match one declared option.
///
/// # Errors
/// Returns an error that identifies the first shape mismatch.
pub fn validate_package_param_value(
    param: &PackageParameter,
    value: &serde_json::Value,
) -> Result<()> {
    fn validate_scalar(param: &PackageParameter, value: &serde_json::Value) -> Result<()> {
        match param.kind {
            ParamKind::String if value.is_string() => Ok(()),
            ParamKind::Bool if value.is_boolean() => Ok(()),
            ParamKind::Number if value.is_number() => Ok(()),
            ParamKind::Dropdown => {
                let Some(choice) = value.as_str() else {
                    anyhow::bail!("parameter '{}' must be a string", param.key);
                };
                if param.options.iter().any(|option| option.value == choice) {
                    Ok(())
                } else {
                    anyhow::bail!(
                        "parameter '{}' does not allow the value '{}'",
                        param.key,
                        choice
                    )
                }
            }
            ParamKind::String => anyhow::bail!("parameter '{}' must be a string", param.key),
            ParamKind::Bool => anyhow::bail!("parameter '{}' must be a boolean", param.key),
            ParamKind::Number => anyhow::bail!("parameter '{}' must be a number", param.key),
            ParamKind::List | ParamKind::Table => {
                anyhow::bail!("parameter '{}' contains a nested container", param.key)
            }
        }
    }

    match param.kind {
        ParamKind::List => {
            let items = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("parameter '{}' must be an array", param.key))?;
            let element = param.fields.first().ok_or_else(|| {
                anyhow::anyhow!("list parameter '{}' has no element declaration", param.key)
            })?;
            for item in items {
                validate_scalar(element, item)?;
            }
            Ok(())
        }
        ParamKind::Table => {
            let rows = value
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("parameter '{}' must be an array", param.key))?;
            for (index, row) in rows.iter().enumerate() {
                let object = row.as_object().ok_or_else(|| {
                    anyhow::anyhow!(
                        "row {} of parameter '{}' must be an object",
                        index + 1,
                        param.key
                    )
                })?;
                for (column, cell) in object {
                    let field = param
                        .fields
                        .iter()
                        .find(|field| field.key == *column)
                        .ok_or_else(|| {
                            anyhow::anyhow!(
                                "row {} of parameter '{}' contains undeclared column '{}'",
                                index + 1,
                                param.key,
                                column
                            )
                        })?;
                    validate_scalar(field, cell)?;
                }
            }
            Ok(())
        }
        _ => validate_scalar(param, value),
    }
}

/// Validates and saves one declared parameter in the scope configured for the active profile.
/// Secret parameters use keyring storage. Other parameters use the JSON value store.
///
/// # Errors
/// Returns an error for a shape mismatch or an unavailable settings store.
pub fn save_package_param_for_profile(
    server_name: &str,
    profile_name: &str,
    specifier: &str,
    param: &PackageParameter,
    value: serde_json::Value,
) -> Result<()> {
    validate_package_param_value(param, &value)?;
    let scope = configured_param_scope(server_name, profile_name, specifier)?;
    if param.secret {
        let secret = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("secret parameter '{}' must be a string", param.key))?;
        save_secret_param_scoped(server_name, scope, specifier, &param.key, secret)
    } else {
        save_param_value_scoped(server_name, scope, specifier, &param.key, value)
    }
}

/// Copies one package's declared values between parameter scopes. With `only_missing`, existing
/// destination values are preserved; otherwise the destination mirrors the source exactly,
/// including clears. Secret values move through the keyring and never enter the JSON store.
fn copy_param_values(
    server_name: &str,
    specifier: &str,
    params: &[PackageParameter],
    from: ParamValueScope<'_>,
    to: ParamValueScope<'_>,
    only_missing: bool,
) -> Result<()> {
    if from == to {
        return Ok(());
    }
    for param in params {
        if param.secret {
            let destination =
                load_secret_param_scoped_checked(server_name, to, specifier, &param.key)?;
            if only_missing && destination.is_some() {
                continue;
            }
            match load_secret_param_scoped_checked(server_name, from, specifier, &param.key)? {
                Some(value) => {
                    save_secret_param_scoped(server_name, to, specifier, &param.key, &value)?;
                }
                None if !only_missing && destination.is_some() => {
                    clear_secret_param_scoped(server_name, to, specifier, &param.key)?;
                }
                None => {}
            }
        } else {
            let destination =
                get_param_value_scoped_checked(server_name, to, specifier, &param.key)?;
            if only_missing && destination.is_some() {
                continue;
            }
            match get_param_value_scoped_checked(server_name, from, specifier, &param.key)? {
                Some(value) => {
                    save_param_value_scoped(server_name, to, specifier, &param.key, value)?;
                }
                None if !only_missing && destination.is_some() => {
                    clear_param_value_scoped(server_name, to, specifier, &param.key)?;
                }
                None => {}
            }
        }
    }
    Ok(())
}

/// Migrates declared parameter values and changes one package's configured scope.
///
/// `expected` is the complete governing row shown by the caller and `profiles` the profile
/// inventory it rendered. A global-to-profile change seeds every profile's missing values from
/// the global values without replacing values already present there; existing profile values are
/// never overwritten. A profile-to-global change requires `source_profile`, which becomes the
/// exact global source, including clears. Values are copied first and the scope is committed
/// last, so an interrupted migration leaves the old scope in force with extra inactive values.
///
/// # Errors
/// Returns an error for invalid arguments or unreadable package, value, or secret state.
pub fn migrate_parameter_scope_if_unchanged(
    server_name: &str,
    expected: &LockedPackage,
    target: ParameterScope,
    source_profile: Option<&str>,
    profiles: &BTreeSet<String>,
    params: &[PackageParameter],
) -> Result<PackageParamCommit> {
    for profile in profiles {
        validate_param_profile_name(profile)?;
    }
    let mut keys = BTreeSet::new();
    if params
        .iter()
        .any(|param| param.key.is_empty() || !keys.insert(param.key.as_str()))
    {
        anyhow::bail!("parameter-scope migration contains an invalid or duplicate declared key");
    }

    let _guard = guard(server_name);
    let lock = load_lock_in(&server_dir(server_name)?)?;
    if !row_is_current(server_name, &lock, expected)? {
        return Ok(PackageParamCommit::StateChanged);
    }
    if expected.parameter_scope == target {
        return Ok(PackageParamCommit::Applied);
    }
    let specifier = expected.specifier.as_str();
    match (expected.parameter_scope, target, source_profile) {
        (ParameterScope::Global, ParameterScope::Profile, None) => {
            for profile in profiles {
                copy_param_values(
                    server_name,
                    specifier,
                    params,
                    ParamValueScope::Global,
                    ParamValueScope::Profile(profile),
                    true,
                )?;
            }
        }
        (ParameterScope::Profile, ParameterScope::Global, Some(profile))
            if profiles.contains(profile) =>
        {
            copy_param_values(
                server_name,
                specifier,
                params,
                ParamValueScope::Profile(profile),
                ParamValueScope::Global,
                false,
            )?;
        }
        (ParameterScope::Global, ParameterScope::Profile, Some(_)) => {
            anyhow::bail!("global-to-profile migration cannot name a source profile");
        }
        (ParameterScope::Profile, ParameterScope::Global, None) => {
            anyhow::bail!("profile-to-global migration requires a source profile");
        }
        (ParameterScope::Profile, ParameterScope::Global, Some(_)) => {
            return Ok(PackageParamCommit::StateChanged);
        }
        _ => unreachable!("equal source and target scopes returned above"),
    }

    match mutate_row_if_unchanged(server_name, expected, |package| {
        package.parameter_scope = target;
    })? {
        Cas::Applied => Ok(PackageParamCommit::Applied),
        Cas::StateChanged => Ok(PackageParamCommit::StateChanged),
    }
}

/// Whether `expected`'s configured scope matches the scope being written.
fn scope_matches(expected: &LockedPackage, scope: ParamValueScope<'_>) -> bool {
    matches!(
        (expected.parameter_scope, scope),
        (ParameterScope::Global, ParamValueScope::Global)
            | (ParameterScope::Profile, ParamValueScope::Profile(_))
    )
}

/// Commits all parameter changes from one UI Save while the complete governing row, authority,
/// and configured value scope still match the editor snapshot.
///
/// Non-secret values are projected into one `smudgy.params.json` replacement; secret values are
/// then written to the keyring one by one.
///
/// # Errors
/// Returns an error if current state cannot be read, the batch is invalid, or a write fails.
pub fn commit_package_params_scoped_if_unchanged(
    server_name: &str,
    scope: ParamValueScope<'_>,
    expected: &LockedPackage,
    mutations: &[PackageParamMutation],
) -> Result<PackageParamCommit> {
    let _guard = guard(server_name);
    let lock = load_lock_in(&server_dir(server_name)?)?;
    if !scope_matches(expected, scope) || !row_is_current(server_name, &lock, expected)? {
        return Ok(PackageParamCommit::StateChanged);
    }
    if mutations.is_empty() {
        return Ok(PackageParamCommit::Applied);
    }
    let specifier = expected.specifier.as_str();

    let dir = param_dir(server_name, scope)?;
    let mut values = load_param_values_in(&dir)?;
    let mut secrets = Vec::new();
    let mut keys = BTreeSet::new();
    for mutation in mutations {
        let key = match mutation {
            PackageParamMutation::SetValue { key, .. }
            | PackageParamMutation::ClearValue { key }
            | PackageParamMutation::SetSecret { key, .. } => key,
        };
        if key.is_empty() || !keys.insert(key.as_str()) {
            anyhow::bail!("package parameter save contains an invalid or duplicate key");
        }
        match mutation {
            PackageParamMutation::SetValue { key, value } => {
                values
                    .entry(specifier.to_string())
                    .or_default()
                    .insert(key.clone(), value.clone());
            }
            PackageParamMutation::ClearValue { key } => {
                if let Some(package_values) = values.get_mut(specifier) {
                    package_values.remove(key);
                    if package_values.is_empty() {
                        values.remove(specifier);
                    }
                }
            }
            PackageParamMutation::SetSecret { key, value } => secrets.push((key, value)),
        }
    }
    save_param_values_in(&dir, &values)?;
    for (key, value) in secrets {
        save_secret_param_scoped(server_name, scope, specifier, key, value)?;
    }
    Ok(PackageParamCommit::Applied)
}

/// Clears one secret only while the complete governing package row, authority, and configured
/// value scope still match the editor snapshot.
///
/// # Errors
/// Returns an error if state cannot be read or the secret cannot be removed.
pub fn clear_secret_param_scoped_if_unchanged(
    server_name: &str,
    scope: ParamValueScope<'_>,
    expected: &LockedPackage,
    key: &str,
) -> Result<PackageParamCommit> {
    let _guard = guard(server_name);
    let lock = load_lock_in(&server_dir(server_name)?)?;
    if !scope_matches(expected, scope) || !row_is_current(server_name, &lock, expected)? {
        return Ok(PackageParamCommit::StateChanged);
    }
    clear_secret_param_scoped(server_name, scope, &expected.specifier, key)?;
    Ok(PackageParamCommit::Applied)
}

fn package_param_dirs(server_name: &str) -> Result<Vec<PathBuf>> {
    let server = server_dir(server_name)?;
    let mut dirs = vec![server.clone()];
    let profiles = server.join("profiles");
    match fs::read_dir(&profiles) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.with_context(|| {
                    format!("Failed to read an entry in {}", profiles.display())
                })?;
                if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                    dirs.push(entry.path());
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", profiles.display()));
        }
    }
    Ok(dirs)
}

/// Reports whether an exact package identity has any persisted parameter state.
///
/// A stale index marker is treated as state: a false positive is safer than letting a newly
/// created package identity inherit a credential meant for an older package.
pub(crate) fn package_param_state_exists(server_name: &str, specifier: &str) -> Result<bool> {
    let _guard = guard(server_name);
    for dir in package_param_dirs(server_name)? {
        if load_param_values_in(&dir)?.contains_key(specifier) {
            return Ok(true);
        }
        if load_secrets_file_checked(&dir)?
            .keys()
            .any(|slot| indexed_secret_scope(server_name, slot, specifier).is_some())
        {
            return Ok(true);
        }
    }
    let mut slots = load_secret_index_raw(server_name)?;
    slots.extend(load_secret_tombstones(server_name)?);
    Ok(slots
        .iter()
        .any(|slot| indexed_secret_scope(server_name, slot, specifier).is_some()))
}

/// Prevents a newly-created installed row from adopting coordinate-keyed settings that cleanup
/// left behind for an older installation. Existing rows intentionally keep their settings.
pub(crate) fn ensure_new_package_rows_have_no_retired_parameter_state<'a>(
    server_name: &str,
    lock: &SharedPackageLock,
    specifiers: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let missing = specifiers
        .into_iter()
        .filter(|specifier| lock.find(specifier).is_none())
        .map(str::to_string)
        .collect::<BTreeSet<_>>();
    for specifier in missing {
        if package_param_state_exists(server_name, &specifier)? {
            remove_package_param_state(server_name, &specifier).with_context(|| {
                format!(
                    "package {specifier} cannot be installed because settings from an older installation still need cleanup"
                )
            })?;
            if package_param_state_exists(server_name, &specifier)? {
                anyhow::bail!(
                    "package {specifier} cannot be installed because settings from an older installation still remain after cleanup"
                );
            }
        }
    }
    Ok(())
}

/// Splits an indexed slot into `(profile, key)` when it belongs to `specifier` on this server.
fn indexed_secret_scope<'a>(
    server_name: &str,
    slot: &'a str,
    specifier: &str,
) -> Option<(Option<&'a str>, &'a str)> {
    let marker = format!(":{specifier}:");
    let (prefix, key) = slot.split_once(&marker)?;
    let server_prefix = format!("pkgparam:{server_name}");
    if prefix == server_prefix {
        return Some((None, key));
    }
    prefix
        .strip_prefix(&format!("{server_prefix}:profile:"))
        .map(|profile| (Some(profile), key))
}

/// Removes all global and profile-specific parameter state for one package.
///
/// # Errors
/// Returns an error if persisted parameter state cannot be loaded or saved. Failed keyring
/// deletions remain tombstoned and are included in the error.
pub fn remove_package_param_state(server_name: &str, specifier: &str) -> Result<()> {
    let _guard = guard(server_name);
    for dir in package_param_dirs(server_name)? {
        let mut values = load_param_values_in(&dir)?;
        if values.remove(specifier).is_some() {
            save_param_values_in(&dir, &values)?;
        }
    }
    let mut slots = load_secret_index(server_name)?;
    slots.extend(load_secret_tombstones(server_name)?);
    let mut failures = Vec::new();
    for slot in slots {
        let Some((profile, key)) = indexed_secret_scope(server_name, &slot, specifier) else {
            continue;
        };
        let scope = profile.map_or(ParamValueScope::Global, ParamValueScope::Profile);
        if let Err(error) = clear_secret_param_scoped(server_name, scope, specifier, key) {
            failures.push(error.to_string());
        }
    }
    if !failures.is_empty() {
        anyhow::bail!(
            "Some package secrets could not be deleted: {}",
            failures.join("; ")
        );
    }
    Ok(())
}

/// Copies every value and secret from one package identity to another, in every scope. The
/// source is left untouched, so a caller can retire it only once the destination is complete.
///
/// # Errors
/// Returns an error if a values file or secret cannot be read or written.
pub(crate) fn copy_package_param_state(server_name: &str, from: &str, to: &str) -> Result<()> {
    let _guard = guard(server_name);
    for dir in package_param_dirs(server_name)? {
        let mut values = load_param_values_in(&dir)?;
        if let Some(copied) = values.get(from).cloned() {
            values.insert(to.to_string(), copied);
            save_param_values_in(&dir, &values)?;
        }
    }
    let slots = load_secret_index(server_name)?;
    for slot in slots {
        let Some((profile, key)) = indexed_secret_scope(server_name, &slot, from) else {
            continue;
        };
        let scope = profile.map_or(ParamValueScope::Global, ParamValueScope::Profile);
        if let Some(value) = load_secret_param_scoped_checked(server_name, scope, from, key)? {
            save_secret_param_scoped(server_name, scope, to, key, &value)?;
        }
    }
    Ok(())
}

/// The running smudgy's release version: `CARGO_PKG_VERSION` with any prerelease
/// (build-channel) suffix dropped. Dev/RC builds carry the channel as a prerelease tag
/// (`0.3.3-dev`, `0.3.3-rc1` — see `crate::models::settings::build_channel`) but have the
/// feature set of the release they are built toward, so version-floor checks compare
/// against the bare `X.Y.Z` (a floor of `0.3.3` admits a `0.3.3-dev` build).
///
/// # Panics
/// Panics if `CARGO_PKG_VERSION` is not valid semver, which cargo itself rejects at
/// build time — unreachable in a built binary.
#[must_use]
pub fn running_smudgy_release() -> semver::Version {
    let mut version = semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .expect("CARGO_PKG_VERSION is valid semver");
    version.pre = semver::Prerelease::EMPTY;
    version.build = semver::BuildMetadata::EMPTY;
    version
}

/// A `min_smudgy_version` floor folded over one or more package manifests (a root and its
/// dependency closure): the highest declared floor wins, and an unparseable declaration
/// poisons the floor entirely — fail-closed, like a malformed dependency range, because a
/// floor that can't be read must not silently pass.
#[derive(Debug, Clone, Default)]
pub struct SmudgyVersionFloor {
    /// The highest parsed floor so far, with the display name of the package declaring it.
    highest: Option<(semver::Version, String)>,
    /// The first unparseable declaration seen: (raw value, declaring package).
    invalid: Option<(String, String)>,
}

impl SmudgyVersionFloor {
    /// Folds one manifest's declared floor. `None` (or a blank string, the hand-edited
    /// equivalent of absent) declares no floor and contributes nothing.
    pub fn fold(&mut self, declared_by: &str, min_smudgy_version: Option<&str>) {
        let Some(raw) = min_smudgy_version.map(str::trim) else {
            return;
        };
        if raw.is_empty() {
            return;
        }
        match semver::Version::parse(raw) {
            Ok(version) => {
                if self
                    .highest
                    .as_ref()
                    .is_none_or(|(highest, _)| version > *highest)
                {
                    self.highest = Some((version, declared_by.to_string()));
                }
            }
            Err(_) => {
                if self.invalid.is_none() {
                    self.invalid = Some((raw.to_string(), declared_by.to_string()));
                }
            }
        }
    }

    /// Why this floor refuses to run on `running` (callers pass
    /// [`running_smudgy_release`]), or `None` when the floor is absent or satisfied.
    #[must_use]
    pub fn refusal(&self, running: &semver::Version) -> Option<String> {
        if let Some((raw, declared_by)) = &self.invalid {
            return Some(format!(
                "{declared_by} declares an unusable min_smudgy_version (\"{raw}\" is not a \
                 semver version); the package needs a corrected release"
            ));
        }
        let (min, declared_by) = self.highest.as_ref()?;
        (*min > *running).then(|| {
            format!(
                "{declared_by} requires smudgy {min} or newer \u{2014} this smudgy is \
                 {running}; update smudgy to use it"
            )
        })
    }
}

// ---------------------------------------------------------------------------
// Secret parameter values (OS keyring, obfuscated-file fallback)
// ---------------------------------------------------------------------------

/// The keyring slot for a package's secret parameter. Unique per (server, scope, package, key).
fn secret_slot(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    key: &str,
) -> String {
    match scope {
        ParamValueScope::Global => format!("pkgparam:{server_name}:{specifier}:{key}"),
        ParamValueScope::Profile(profile_name) => {
            format!("pkgparam:{server_name}:profile:{profile_name}:{specifier}:{key}")
        }
    }
}

fn secret_keyring_entry(slot: &str) -> keyring::Result<keyring::Entry> {
    // Same dev-aware service as the session token, so a dev build's package secrets are
    // isolated from a release build's alongside its login.
    keyring::Entry::new(crate::models::auth::keyring_service(), slot)
}

/// Stores a secret parameter value in one persisted scope: the OS keyring, with an
/// obfuscated-file fallback when no keyring is available. Never written to plain JSON or logged.
///
/// # Errors
/// Returns an error if neither the keyring nor fallback file can store the value, or its index and
/// tombstone state cannot be updated.
pub fn save_secret_param_scoped(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    key: &str,
    value: &str,
) -> Result<()> {
    let _guard = guard(server_name);
    let slot = secret_slot(server_name, scope, specifier, key);
    // Make the credential discoverable for cleanup before it can be created externally. A marker
    // for a failed write is harmless; an unindexed credential is not.
    add_secret_index_slot(server_name, &slot)?;
    let dir = param_dir(server_name, scope)?;
    match secret_keyring_entry(&slot).and_then(|entry| entry.set_password(value)) {
        // A fallback value means it is authoritative. Remove it after a successful keyring write
        // so a later keyring outage cannot reveal an older value.
        Ok(()) => remove_secret_from_file(&dir, &slot)?,
        Err(e) => {
            warn!(
                "OS keyring unavailable for package secret, falling back to obfuscated file: {e}"
            );
            save_secret_to_file(&dir, &slot, value)?;
        }
    }
    remove_secret_tombstone(server_name, &slot)
}

/// Whether a keyring-read failure has already been warned about this process. Secret reads
/// happen on the per-line hot path (a script may `get()` a secret in a trigger), so on a
/// keyring-unavailable host the warning would otherwise flood the log; warn once.
static KEYRING_READ_WARNED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Reads a secret while preserving the distinction between an unset value and an unavailable
/// credential service.
///
/// A fallback-file value remains usable while the OS keyring is unavailable. `Ok(None)` means the
/// keyring positively reported that the credential is absent (or its slot is tombstoned).
///
/// # Errors
/// Returns an error if tombstone state is unreadable or the keyring is unavailable and no fallback
/// value exists.
pub fn load_secret_param_scoped_checked(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    key: &str,
) -> Result<Option<String>> {
    let _guard = guard(server_name);
    let slot = secret_slot(server_name, scope, specifier, key);
    if load_secret_tombstones(server_name)?.contains(&slot) {
        return Ok(None);
    }
    // A fallback is written only when a keyring update fails, so it is the authoritative copy.
    let fallback = load_secret_from_file_checked(&param_dir(server_name, scope)?, &slot)?;
    if fallback.is_some() {
        return Ok(fallback);
    }
    match secret_keyring_entry(&slot).and_then(|entry| entry.get_password()) {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            if matches!(error, keyring::Error::NoEntry) {
                return Ok(None);
            }
            if !KEYRING_READ_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed) {
                warn!(
                    "Failed to read a package secret from the OS keyring (further occurrences suppressed): {error}"
                );
            }
            Err(anyhow::anyhow!(
                "Package secret is unavailable because the OS keyring could not be read: {error}"
            ))
        }
    }
}

/// Removes a secret parameter value from one persisted scope, from both the keyring and the
/// fallback file.
///
/// # Errors
/// Returns an error if an existing keyring entry could not be removed; the slot stays tombstoned
/// for a later retry.
pub fn clear_secret_param_scoped(
    server_name: &str,
    scope: ParamValueScope<'_>,
    specifier: &str,
    key: &str,
) -> Result<()> {
    let _guard = guard(server_name);
    let slot = secret_slot(server_name, scope, specifier, key);
    let dir = param_dir(server_name, scope)?;
    clear_secret_slot(server_name, &dir, &slot)
}

fn load_secret_index_raw(server_name: &str) -> Result<BTreeSet<String>> {
    let path = server_dir(server_name)?.join(SECRET_INDEX_FILE);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn load_secret_index(server_name: &str) -> Result<BTreeSet<String>> {
    let mut slots = load_secret_index_raw(server_name)?;
    let before = slots.len();
    discover_legacy_secret_slots(server_name, &server_dir(server_name)?, &mut slots)?;
    if slots.len() != before
        && let Err(error) = save_secret_index(server_name, &slots)
    {
        warn!("Failed to persist the backfilled package secret index: {error:#}");
    }
    Ok(slots)
}

/// Adds the slots that installs made before the index existed could have created, derived from
/// their manifests' secret parameters.
fn discover_legacy_secret_slots(
    server_name: &str,
    server: &Path,
    slots: &mut BTreeSet<String>,
) -> Result<()> {
    let mut scope_dirs = vec![(None, server.to_path_buf())];
    let profiles = server.join("profiles");
    match fs::read_dir(&profiles) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.with_context(|| {
                    format!("Failed to read an entry in {}", profiles.display())
                })?;
                if entry.file_type().is_ok_and(|kind| kind.is_dir())
                    && let Some(profile) = entry.file_name().to_str().map(str::to_string)
                {
                    scope_dirs.push((Some(profile), entry.path()));
                }
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| format!("Failed to read {}", profiles.display()));
        }
    }

    // Fallback files already contain their complete opaque slots and are authoritative markers.
    for (_, dir) in &scope_dirs {
        slots.extend(load_secrets_file_checked(dir)?.into_keys());
    }

    let lock = load_lock_in(server).unwrap_or_default();
    for package in lock.packages {
        let Ok(specifier) = smudgy_script::SmudgySpecifier::parse(&package.specifier) else {
            continue;
        };
        let local_manifest = server
            .join("packages")
            .join(&specifier.name)
            .join("smudgy.package.json");
        let manifest = fs::read_to_string(local_manifest)
            .ok()
            .and_then(|text| PackageManifest::parse(&text).ok())
            .or_else(|| {
                let version = package.staged_version()?;
                let meta = get_smudgy_home()
                    .ok()?
                    .join("cache")
                    .join("packages")
                    .join("meta")
                    .join(&specifier.owner)
                    .join(&specifier.name)
                    .join(format!("{version}.json"));
                let value: serde_json::Value =
                    serde_json::from_str(&fs::read_to_string(meta).ok()?).ok()?;
                serde_json::from_value(value.get("manifest")?.clone()).ok()
            });
        let Some(manifest) = manifest else {
            continue;
        };
        for param in manifest.params.iter().filter(|param| param.secret) {
            for (profile, _) in &scope_dirs {
                let scope = profile
                    .as_deref()
                    .map_or(ParamValueScope::Global, ParamValueScope::Profile);
                slots.insert(secret_slot(
                    server_name,
                    scope,
                    &package.specifier,
                    &param.key,
                ));
            }
        }
    }
    Ok(())
}

fn save_secret_index(server_name: &str, slots: &BTreeSet<String>) -> Result<()> {
    let dir = server_dir(server_name)?;
    fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = dir.join(SECRET_INDEX_FILE);
    let json = serde_json::to_string_pretty(slots).context("serialize package secret index")?;
    write_atomic(&path, json.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn add_secret_index_slot(server_name: &str, slot: &str) -> Result<()> {
    let mut slots = load_secret_index_raw(server_name)?;
    if slots.insert(slot.to_string()) {
        save_secret_index(server_name, &slots)?;
    }
    Ok(())
}

fn remove_secret_index_slot(server_name: &str, slot: &str) -> Result<()> {
    let mut slots = load_secret_index_raw(server_name)?;
    if slots.remove(slot) {
        save_secret_index(server_name, &slots)?;
    }
    Ok(())
}

fn load_secret_tombstones(server_name: &str) -> Result<BTreeSet<String>> {
    let path = server_dir(server_name)?.join(SECRET_TOMBSTONES_FILE);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeSet::new()),
        Err(error) => Err(error).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn save_secret_tombstones(server_name: &str, slots: &BTreeSet<String>) -> Result<()> {
    let dir = server_dir(server_name)?;
    fs::create_dir_all(&dir).with_context(|| format!("Failed to create {}", dir.display()))?;
    let path = dir.join(SECRET_TOMBSTONES_FILE);
    let json =
        serde_json::to_string_pretty(slots).context("serialize package secret tombstones")?;
    write_atomic(&path, json.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn add_secret_tombstone(server_name: &str, slot: &str) -> Result<()> {
    let mut slots = load_secret_tombstones(server_name)?;
    if slots.insert(slot.to_string()) {
        save_secret_tombstones(server_name, &slots)?;
    }
    Ok(())
}

fn remove_secret_tombstone(server_name: &str, slot: &str) -> Result<()> {
    let mut slots = load_secret_tombstones(server_name)?;
    if slots.remove(slot) {
        save_secret_tombstones(server_name, &slots)?;
    }
    Ok(())
}

fn clear_secret_slot(server_name: &str, dir: &Path, slot: &str) -> Result<()> {
    // Persist the deny marker first. From this point onward reads fail closed even if the keyring
    // deletion fails or the process exits before cleanup finishes.
    add_secret_tombstone(server_name, slot)?;
    remove_secret_from_file(dir, slot)?;
    match secret_keyring_entry(slot).and_then(|entry| entry.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => {}
        Err(error) => {
            return Err(anyhow::anyhow!(
                "Failed to delete package secret from the OS keyring: {error}"
            ));
        }
    }
    remove_secret_index_slot(server_name, slot)?;
    remove_secret_tombstone(server_name, slot)
}

/// The directory whose fallback file holds `slot`, when the slot is a well-formed package secret
/// slot for this server.
fn secret_dir_for_slot(server_name: &str, slot: &str) -> Option<PathBuf> {
    let server = server_dir(server_name).ok()?;
    let prefix = format!("pkgparam:{server_name}:");
    let rest = slot.strip_prefix(&prefix)?;
    let (profile, package_and_key) =
        if let Some(profile_and_specifier) = rest.strip_prefix("profile:") {
            let (profile, package) = profile_and_specifier.split_once(":smudgy://")?;
            validate_param_profile_name(profile).ok()?;
            (Some(profile), format!("smudgy://{package}"))
        } else {
            (None, rest.to_string())
        };
    let (specifier, key) = package_and_key.rsplit_once(':')?;
    if key.is_empty() || smudgy_script::SmudgySpecifier::parse(specifier).is_err() {
        return None;
    }
    Some(match profile {
        Some(profile) => server.join("profiles").join(profile),
        None => server,
    })
}

/// Retries credential deletions left by an earlier keyring outage.
///
/// # Errors
/// Returns an error if tombstone state is invalid or one or more credentials still cannot be
/// deleted. Those credentials remain tombstoned.
pub fn retry_secret_tombstones(server_name: &str) -> Result<()> {
    let _guard = guard(server_name);
    let tombstones = load_secret_tombstones(server_name)?;
    let mut failures = Vec::new();
    for slot in tombstones {
        let Some(dir) = secret_dir_for_slot(server_name, &slot) else {
            failures.push(format!("unrecognized secret slot {slot}"));
            continue;
        };
        if let Err(error) = clear_secret_slot(server_name, &dir, &slot) {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "Some package secrets could not be deleted: {}",
            failures.join("; ")
        )
    }
}

/// Deletes every package-parameter credential associated with a server.
///
/// Indexed slots and deletion tombstones are treated as one inventory. Returns success only after
/// every fallback and OS-keyring credential is absent; a failure leaves the remaining slots
/// indexed and tombstoned, so callers must not delete the server directory after an error.
///
/// # Errors
/// Returns an error if any credential could not be deleted.
pub fn clear_server_param_secrets(server_name: &str) -> Result<()> {
    let _guard = guard(server_name);
    let mut indexed = load_secret_index(server_name)?;
    indexed.extend(load_secret_tombstones(server_name)?);
    if indexed.is_empty() {
        save_secret_index(server_name, &BTreeSet::new())?;
        save_secret_tombstones(server_name, &BTreeSet::new())?;
        return Ok(());
    }

    // Make every candidate fail closed before deleting any external credential.
    save_secret_index(server_name, &indexed)?;
    save_secret_tombstones(server_name, &indexed)?;

    let mut remaining = indexed;
    let mut failures = Vec::new();
    for slot in remaining.clone() {
        let Some(dir) = secret_dir_for_slot(server_name, &slot) else {
            failures.push(format!("unrecognized secret slot {slot}"));
            continue;
        };
        if let Err(error) = remove_secret_from_file(&dir, &slot) {
            failures.push(format!("{slot}: {error}"));
            continue;
        }
        match secret_keyring_entry(&slot).and_then(|entry| entry.delete_credential()) {
            Ok(()) | Err(keyring::Error::NoEntry) => {
                remaining.remove(&slot);
            }
            Err(error) => failures.push(format!("{slot}: {error}")),
        }
    }

    save_secret_index(server_name, &remaining)?;
    save_secret_tombstones(server_name, &remaining)?;
    if !failures.is_empty() || !remaining.is_empty() {
        anyhow::bail!(
            "Some package secrets could not be deleted: {}",
            failures.join("; ")
        );
    }
    Ok(())
}

/// Removes indexed keyring secrets for a deleted profile.
///
/// # Errors
/// Returns an error if index or tombstone state cannot be saved, or one or more credentials cannot
/// be deleted. Failed credentials remain tombstoned and indexed for retry.
pub fn clear_profile_param_secrets(server_name: &str, profile_name: &str) -> Result<()> {
    let _guard = guard(server_name);
    let prefix = format!("pkgparam:{server_name}:profile:{profile_name}:");
    let slots = load_secret_index(server_name)?;
    let removed = slots
        .iter()
        .filter(|slot| slot.starts_with(&prefix))
        .cloned()
        .collect::<Vec<_>>();
    if removed.is_empty() {
        return Ok(());
    }
    let dir = param_dir(server_name, ParamValueScope::Profile(profile_name))?;
    let mut failures = Vec::new();
    for slot in &removed {
        if let Err(error) = clear_secret_slot(server_name, &dir, slot) {
            if load_secret_tombstones(server_name)?.contains(slot) {
                // The external credential remains, but the tombstone makes deletion of the
                // profile directory safe. Keep the index marker for a later retry.
                warn!("Deferred cleanup of a deleted profile's package secret: {error:#}");
            } else {
                failures.push(error.to_string());
            }
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        anyhow::bail!(
            "Some profile package secrets could not be deleted: {}",
            failures.join("; ")
        )
    }
}

/// The obfuscated secrets fallback map: slot → hex(obfuscate(value)).
type SecretsFile = HashMap<String, String>;

fn load_secrets_file_checked(dir: &Path) -> Result<SecretsFile> {
    let path = dir.join(SECRETS_FILE);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(SecretsFile::new()),
        Err(error) => Err(error).with_context(|| format!("Failed to read {}", path.display())),
    }
}

fn save_secret_to_file(dir: &Path, slot: &str, value: &str) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create server dir {}", dir.display()))?;
    let mut secrets = load_secrets_file_checked(dir)?;
    secrets.insert(slot.to_string(), hex_encode(&obfuscate(value.as_bytes())));
    let path = dir.join(SECRETS_FILE);
    let json = serde_json::to_string(&secrets).context("Failed to serialize package secrets")?;
    write_atomic(&path, json.as_bytes())
        .with_context(|| format!("Failed to write {}", path.display()))
}

fn load_secret_from_file_checked(dir: &Path, slot: &str) -> Result<Option<String>> {
    let secrets = load_secrets_file_checked(dir)?;
    let Some(encoded) = secrets.get(slot) else {
        return Ok(None);
    };
    let bytes = hex_decode(encoded)
        .with_context(|| format!("Package secret fallback slot {slot} is not valid hex"))?;
    let value = String::from_utf8(obfuscate(&bytes))
        .with_context(|| format!("Package secret fallback slot {slot} is not valid UTF-8"))?;
    Ok(Some(value))
}

fn remove_secret_from_file(dir: &Path, slot: &str) -> Result<()> {
    let path = dir.join(SECRETS_FILE);
    let mut secrets = load_secrets_file_checked(dir)?;
    if secrets.remove(slot).is_some() {
        let json =
            serde_json::to_string(&secrets).context("Failed to serialize package secrets")?;
        write_atomic(&path, json.as_bytes())
            .with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn use_temp_smudgy_home() {
        static TEST_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        TEST_HOME.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "smudgy-shared-packages-test-home-{}",
                std::process::id()
            ));
            fs::create_dir_all(&dir).expect("create temp home");
            crate::set_smudgy_home(dir.clone());
            dir
        });
    }

    fn test_server(label: &str) -> String {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        use_temp_smudgy_home();
        let name = format!(
            "spk-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        fs::create_dir_all(get_smudgy_home().unwrap().join(&name)).unwrap();
        name
    }

    fn selected(profiles: &[&str]) -> ProfileActivation {
        ProfileActivation::Selected {
            profiles: profiles.iter().map(|p| (*p).to_string()).collect(),
        }
    }

    fn param(key: &str, required: bool, secret: bool) -> PackageParameter {
        PackageParameter {
            key: key.to_string(),
            label: None,
            secret,
            required,
            kind: ParamKind::String,
            default: None,
            options: Vec::new(),
            fields: Vec::new(),
        }
    }

    #[test]
    fn lock_round_trips_and_missing_lock_is_empty() {
        let server = test_server("roundtrip");
        assert!(load_lock(&server).unwrap().packages.is_empty());
        install_package(&server, "smudgy://wbk/mapper", UpdateMode::Auto, true).unwrap();
        let lock = load_lock(&server).unwrap();
        let row = lock.find("smudgy://wbk/mapper").unwrap();
        assert_eq!(row.activation(), ProfileActivation::All);
        assert!(row.enabled);
        assert!(!row.trusted);
    }

    #[test]
    fn legacy_enabled_and_selected_activation_are_downgrade_safe() {
        let json = r#"{"packages":[{"specifier":"smudgy://a/b","enabled":false}]}"#;
        let lock: SharedPackageLock = serde_json::from_str(json).unwrap();
        assert_eq!(lock.packages[0].activation(), ProfileActivation::None);

        let mut row = LockedPackage::new("smudgy://a/b", UpdateMode::Auto);
        row.set_activation(selected(&["Main"]));
        assert!(
            !row.enabled,
            "selected scopes mirror enabled=false for older clients"
        );
        assert!(row.is_enabled_for("Main"));
        assert!(!row.is_enabled_for("Alt"));
    }

    #[test]
    fn local_row_governs_its_leaf_for_every_author() {
        let mut lock = SharedPackageLock::default();
        let mut published = LockedPackage::new("smudgy://wbk/mapper", UpdateMode::Auto);
        published.set_activation(ProfileActivation::All);
        lock.upsert(published);
        assert!(lock.is_effectively_enabled_for("smudgy://wbk/mapper", "Main"));

        let mut local = LockedPackage::new("smudgy://local/mapper", UpdateMode::Auto);
        local.set_activation(selected(&["Alt"]));
        lock.upsert(local);
        assert_eq!(
            lock.governing_specifier("smudgy://wbk/mapper"),
            Some("smudgy://local/mapper")
        );
        assert!(!lock.is_effectively_enabled_for("smudgy://wbk/mapper", "Main"));
        assert!(lock.is_effectively_enabled_for("smudgy://wbk/mapper", "Alt"));
    }

    #[test]
    fn required_packages_follow_active_parents_per_profile() {
        let mut lock = SharedPackageLock::default();
        let mut root = LockedPackage::new("smudgy://a/root", UpdateMode::Auto);
        root.set_activation(selected(&["Main"]));
        lock.upsert(root);
        let mut dep = LockedPackage::new("smudgy://a/dep", UpdateMode::Auto);
        dep.set_activation(ProfileActivation::None);
        dep.installed_as_requirement = true;
        dep.requirement_lineage_known = true;
        dep.required_by.insert("smudgy://a/root".into());
        lock.upsert(dep);
        assert!(lock.is_effectively_enabled_for("smudgy://a/dep", "Main"));
        assert!(!lock.is_effectively_enabled_for("smudgy://a/dep", "Alt"));

        let plan = lock.plan_removal_from_links("smudgy://a/root");
        assert!(plan.breaks.is_empty());
        assert_eq!(plan.orphans, ["smudgy://a/dep"]);
    }

    #[test]
    fn install_with_requirements_commits_the_whole_plan_or_nothing() {
        let server = test_server("requirements");
        let expected = load_lock(&server).unwrap();
        let required = [RequiredPackageInstall {
            specifier: "smudgy://a/dep".into(),
            version: "1.2.0".into(),
            permissions: PackagePermissions::default(),
            already_satisfied: false,
        }];
        assert!(
            install_package_with_requirements_if_unchanged(
                &server,
                &expected,
                "smudgy://a/root",
                "2.0.0",
                &PackagePermissions::default(),
                UpdateMode::Auto,
                ProfileActivation::All,
                &required,
            )
            .unwrap()
        );
        let lock = load_lock(&server).unwrap();
        let dep = lock.find("smudgy://a/dep").unwrap();
        assert!(dep.installed_as_requirement);
        assert!(dep.requirement_lineage_known);
        assert_eq!(dep.staged_version(), Some("1.2.0"));
        assert!(dep.required_by.contains("smudgy://a/root"));
        assert_eq!(
            lock.find("smudgy://a/root").unwrap().staged_version(),
            Some("2.0.0")
        );

        // A stale snapshot commits nothing.
        assert!(
            !install_package_with_requirements_if_unchanged(
                &server,
                &expected,
                "smudgy://a/other",
                "1.0.0",
                &PackagePermissions::default(),
                UpdateMode::Auto,
                ProfileActivation::All,
                &[],
            )
            .unwrap()
        );
        assert!(
            load_lock(&server)
                .unwrap()
                .find("smudgy://a/other")
                .is_none()
        );
    }

    #[test]
    fn uninstall_commit_demotes_required_direct_installs_and_removes_orphans() {
        let server = test_server("uninstall");
        let expected = load_lock(&server).unwrap();
        let required = [RequiredPackageInstall {
            specifier: "smudgy://a/dep".into(),
            version: "1.0.0".into(),
            permissions: PackagePermissions::default(),
            already_satisfied: false,
        }];
        install_package_with_requirements_if_unchanged(
            &server,
            &expected,
            "smudgy://a/root",
            "1.0.0",
            &PackagePermissions::default(),
            UpdateMode::Auto,
            ProfileActivation::All,
            &required,
        )
        .unwrap();
        install_package(&server, "smudgy://a/dep", UpdateMode::Auto, true).unwrap();

        let lock = load_lock(&server).unwrap();
        assert_eq!(
            commit_uninstall_if_unchanged(&server, &lock, "smudgy://a/dep", true).unwrap(),
            UninstallCommit::DirectInstallRemoved
        );
        let stale = lock;
        assert_eq!(
            commit_uninstall_if_unchanged(&server, &stale, "smudgy://a/root", true).unwrap(),
            UninstallCommit::Stale
        );
        let lock = load_lock(&server).unwrap();
        assert_eq!(
            commit_uninstall_if_unchanged(&server, &lock, "smudgy://a/root", true).unwrap(),
            UninstallCommit::PackagesRemoved(vec![
                "smudgy://a/root".into(),
                "smudgy://a/dep".into()
            ])
        );
        assert!(load_lock(&server).unwrap().packages.is_empty());
    }

    #[test]
    fn row_settings_compare_and_swap() {
        let server = test_server("cas");
        install_package(&server, "smudgy://a/b", UpdateMode::Auto, true).unwrap();
        let row = load_lock(&server)
            .unwrap()
            .find("smudgy://a/b")
            .cloned()
            .unwrap();
        assert_eq!(
            set_governing_activation_if_unchanged(
                &server,
                "smudgy://a/b",
                &row,
                selected(&["Main"])
            )
            .unwrap(),
            Cas::Applied
        );
        assert_eq!(
            set_governing_trusted_if_unchanged(&server, "smudgy://a/b", &row, true).unwrap(),
            Cas::StateChanged
        );
        let row = load_lock(&server)
            .unwrap()
            .find("smudgy://a/b")
            .cloned()
            .unwrap();
        assert_eq!(row.activation(), selected(&["Main"]));
        assert_eq!(
            set_update_mode_if_unchanged(
                &server,
                &row,
                UpdateMode::Pinned {
                    version: "1.0.0".into()
                }
            )
            .unwrap(),
            Cas::Applied
        );
        assert_eq!(
            load_lock(&server)
                .unwrap()
                .find("smudgy://a/b")
                .unwrap()
                .pinned_version(),
            Some("1.0.0")
        );
    }

    #[test]
    fn stage_and_dismiss_track_offers() {
        let server = test_server("stage");
        install_package(&server, "smudgy://a/b", UpdateMode::Auto, true).unwrap();
        let row = load_lock(&server)
            .unwrap()
            .find("smudgy://a/b")
            .cloned()
            .unwrap();
        assert!(set_dismissed_update_version_if_unchanged(&server, &row, "1.1.0").unwrap());
        assert!(!set_dismissed_update_version_if_unchanged(&server, &row, "1.2.0").unwrap());
        assert_eq!(
            stage_resolved_version(&server, "smudgy://a/b", "1.1.0").unwrap(),
            None
        );
        let row = load_lock(&server)
            .unwrap()
            .find("smudgy://a/b")
            .cloned()
            .unwrap();
        assert_eq!(row.dismissed_update_version, None);
        assert!(stage_auto_update_if_unchanged(&server, &row, "1.3.0", None).unwrap());
        assert_eq!(
            load_lock(&server)
                .unwrap()
                .find("smudgy://a/b")
                .unwrap()
                .staged_version(),
            Some("1.3.0")
        );
    }

    #[test]
    fn remove_profile_activation_only_touches_that_name() {
        let server = test_server("prune");
        install_package(&server, "smudgy://a/b", UpdateMode::Auto, true).unwrap();
        set_activation(&server, "smudgy://a/b", selected(&["Main", "Alt"])).unwrap();
        remove_profile_activation(&server, "Alt").unwrap();
        assert_eq!(
            load_lock(&server)
                .unwrap()
                .find("smudgy://a/b")
                .unwrap()
                .activation(),
            selected(&["Main"])
        );
    }

    #[test]
    fn parameters_are_read_from_the_configured_scope() {
        let server = test_server("params");
        install_package(&server, "smudgy://a/b", UpdateMode::Auto, true).unwrap();
        let spec = "smudgy://a/b";
        save_param_value(&server, spec, "color", serde_json::json!("red")).unwrap();
        assert_eq!(
            get_param_value_for_profile(&server, "Main", spec, "color"),
            Some(serde_json::json!("red"))
        );
        // A package without a row of its own reads global values.
        save_param_value(&server, "smudgy://a/dep", "k", serde_json::json!(1)).unwrap();
        assert_eq!(
            get_param_value_for_profile(&server, "Main", "smudgy://a/dep", "k"),
            Some(serde_json::json!(1))
        );

        let params = [param("color", true, false), param("size", false, false)];
        let row = load_lock(&server).unwrap().find(spec).cloned().unwrap();
        let profiles = ["Main", "Alt"].into_iter().map(String::from).collect();
        assert_eq!(
            migrate_parameter_scope_if_unchanged(
                &server,
                &row,
                ParameterScope::Profile,
                None,
                &profiles,
                &params
            )
            .unwrap(),
            PackageParamCommit::Applied
        );
        let row = load_lock(&server).unwrap().find(spec).cloned().unwrap();
        assert_eq!(row.parameter_scope, ParameterScope::Profile);
        for profile in ["Main", "Alt"] {
            assert_eq!(
                get_param_value_for_profile(&server, profile, spec, "color"),
                Some(serde_json::json!("red")),
                "{profile} was seeded from the global value"
            );
        }
        assert_eq!(
            commit_package_params_scoped_if_unchanged(
                &server,
                ParamValueScope::Profile("Alt"),
                &row,
                &[PackageParamMutation::SetValue {
                    key: "color".into(),
                    value: serde_json::json!("blue")
                }]
            )
            .unwrap(),
            PackageParamCommit::Applied
        );
        assert_eq!(
            get_param_value_for_profile(&server, "Alt", spec, "color"),
            Some(serde_json::json!("blue"))
        );
        assert_eq!(
            get_param_value_for_profile(&server, "Main", spec, "color"),
            Some(serde_json::json!("red"))
        );
        // A global-scope write against a profile-scoped row is stale.
        assert_eq!(
            commit_package_params_scoped_if_unchanged(&server, ParamValueScope::Global, &row, &[])
                .unwrap(),
            PackageParamCommit::StateChanged
        );
        assert_eq!(
            missing_required_params_for_profile_checked(&server, "Alt", spec, &params).unwrap(),
            Vec::<String>::new()
        );

        // Returning to global adopts the chosen profile's values exactly.
        assert_eq!(
            migrate_parameter_scope_if_unchanged(
                &server,
                &row,
                ParameterScope::Global,
                Some("Alt"),
                &profiles,
                &params
            )
            .unwrap(),
            PackageParamCommit::Applied
        );
        assert_eq!(
            get_param_value_for_profile(&server, "Main", spec, "color"),
            Some(serde_json::json!("blue"))
        );
    }

    #[test]
    fn uninstall_removes_parameter_state_in_every_scope() {
        let server = test_server("param-cleanup");
        let spec = "smudgy://a/b";
        install_package(&server, spec, UpdateMode::Auto, true).unwrap();
        save_param_value(&server, spec, "k", serde_json::json!(1)).unwrap();
        save_param_value_scoped(
            &server,
            ParamValueScope::Profile("Main"),
            spec,
            "k",
            serde_json::json!(2),
        )
        .unwrap();
        assert!(package_param_state_exists(&server, spec).unwrap());
        uninstall_package(&server, spec).unwrap();
        assert!(!package_param_state_exists(&server, spec).unwrap());
        assert!(load_lock(&server).unwrap().packages.is_empty());
    }

    #[test]
    fn stale_conditional_uninstall_keeps_the_row_and_its_parameter_state() {
        let server = test_server("param-cleanup-stale-cas");
        let spec = "smudgy://a/b";
        install_package(&server, spec, UpdateMode::Auto, true).unwrap();
        save_param_value(&server, spec, "k", serde_json::json!(1)).unwrap();
        save_param_value_scoped(
            &server,
            ParamValueScope::Profile("Main"),
            spec,
            "k",
            serde_json::json!(2),
        )
        .unwrap();
        // The snapshot an async check evaluated, superseded by a user edit before it lands.
        let mut stale = load_lock(&server).unwrap().find(spec).cloned().unwrap();
        stale.set_activation(ProfileActivation::None);

        assert!(!uninstall_package_if_unchanged(&server, &stale).unwrap());

        assert!(load_lock(&server).unwrap().find(spec).is_some());
        assert!(package_param_state_exists(&server, spec).unwrap());
        assert_eq!(
            get_param_value_for_profile(&server, "Main", spec, "k"),
            Some(serde_json::json!(1)),
            "global-scope value survives a stale uninstall attempt"
        );
    }

    #[test]
    fn secret_slots_are_parsed_and_tombstones_deny_reads() {
        let server = test_server("secrets");
        let spec = "smudgy://a/b";
        let global = secret_slot(&server, ParamValueScope::Global, spec, "token");
        let profile = secret_slot(&server, ParamValueScope::Profile("Main"), spec, "token");
        assert_eq!(
            indexed_secret_scope(&server, &global, spec),
            Some((None, "token"))
        );
        assert_eq!(
            indexed_secret_scope(&server, &profile, spec),
            Some((Some("Main"), "token"))
        );
        assert_eq!(indexed_secret_scope(&server, &global, "smudgy://a/c"), None);
        assert_eq!(
            secret_dir_for_slot(&server, &profile),
            Some(server_dir(&server).unwrap().join("profiles").join("Main"))
        );

        // A fallback-file value is readable until its slot is tombstoned.
        let dir = server_dir(&server).unwrap();
        save_secret_to_file(&dir, &global, "hunter2").unwrap();
        assert_eq!(
            load_secret_param_scoped_checked(&server, ParamValueScope::Global, spec, "token")
                .unwrap(),
            Some("hunter2".to_string())
        );
        add_secret_tombstone(&server, &global).unwrap();
        assert_eq!(
            load_secret_param_scoped_checked(&server, ParamValueScope::Global, spec, "token")
                .unwrap(),
            None
        );
        assert!(package_param_state_exists(&server, spec).unwrap());
    }

    #[test]
    fn rename_moves_state_to_the_new_local_identity() {
        let server = test_server("rename");
        let home = get_smudgy_home().unwrap();
        let packages = home.join(&server).join("packages");
        fs::create_dir_all(packages.join("old")).unwrap();
        fs::write(packages.join("old").join("smudgy.package.json"), "{}").unwrap();
        install_package(&server, "smudgy://local/old", UpdateMode::Auto, true).unwrap();
        save_param_value(&server, "smudgy://local/old", "k", serde_json::json!(1)).unwrap();
        save_param_value_scoped(
            &server,
            ParamValueScope::Profile("Alt"),
            "smudgy://local/old",
            "k",
            serde_json::json!(2),
        )
        .unwrap();
        mutate_lock(&server, |lock| {
            let row = lock.find_mut("smudgy://local/old").unwrap();
            row.trusted = true;
            row.parameter_scope = ParameterScope::Profile;
            let mut lib = LockedPackage::new("smudgy://wbk/lib", UpdateMode::Auto);
            lib.required_by.insert("smudgy://local/old".to_string());
            lib.installed_as_requirement = true;
            lock.packages.push(lib);
            Ok(((), true))
        })
        .unwrap();

        assert!(rename_local_package_state(&server, "old", "new").unwrap());
        assert!(packages.join("new").is_dir());
        assert!(!packages.join("old").exists());
        let lock = load_lock(&server).unwrap();
        assert!(lock.find("smudgy://local/old").is_none());
        let renamed = lock.find("smudgy://local/new").unwrap();
        assert!(renamed.trusted, "trust travels with the identity");
        assert_eq!(renamed.parameter_scope, ParameterScope::Profile);
        let lib = lock.find("smudgy://wbk/lib").unwrap();
        assert!(lib.required_by.contains("smudgy://local/new"));
        assert!(!lib.required_by.contains("smudgy://local/old"));
        assert_eq!(
            get_param_value_scoped_checked(
                &server,
                ParamValueScope::Global,
                "smudgy://local/new",
                "k"
            )
            .unwrap(),
            Some(serde_json::json!(1)),
            "global values travel with the identity"
        );
        assert_eq!(
            get_param_value_scoped_checked(
                &server,
                ParamValueScope::Profile("Alt"),
                "smudgy://local/new",
                "k"
            )
            .unwrap(),
            Some(serde_json::json!(2)),
            "per-profile values travel with the identity"
        );
        assert!(!package_param_state_exists(&server, "smudgy://local/old").unwrap());

        fs::create_dir_all(packages.join("taken")).unwrap();
        assert!(rename_local_package_state(&server, "new", "TAKEN").is_err());
    }

    #[test]
    fn copying_profile_values_replaces_the_destination_and_needs_profile_scope() {
        let server = test_server("copy-profile-values");
        let spec = "smudgy://a/b";
        install_package(&server, spec, UpdateMode::Auto, true).unwrap();
        let params = vec![param("color", false, false), param("size", false, false)];
        for (profile, key, value) in [
            ("Main", "color", serde_json::json!("blue")),
            ("Alt", "color", serde_json::json!("red")),
            ("Alt", "size", serde_json::json!(3)),
        ] {
            save_param_value_scoped(&server, ParamValueScope::Profile(profile), spec, key, value)
                .unwrap();
        }

        // Global scope: nothing is copied.
        let global_row = load_lock(&server).unwrap().find(spec).cloned().unwrap();
        assert_eq!(
            copy_profile_param_values_if_unchanged(&server, &global_row, &params, "Main", "Alt")
                .unwrap(),
            PackageParamCommit::StateChanged
        );
        assert_eq!(
            get_param_value_scoped_checked(&server, ParamValueScope::Profile("Alt"), spec, "color")
                .unwrap(),
            Some(serde_json::json!("red"))
        );

        mutate_lock(&server, |lock| {
            lock.find_mut(spec).unwrap().parameter_scope = ParameterScope::Profile;
            Ok(((), true))
        })
        .unwrap();
        let row = load_lock(&server).unwrap().find(spec).cloned().unwrap();
        assert_eq!(
            copy_profile_param_values_if_unchanged(&server, &row, &params, "Main", "Alt").unwrap(),
            PackageParamCommit::Applied
        );
        assert_eq!(
            get_param_value_scoped_checked(&server, ParamValueScope::Profile("Alt"), spec, "color")
                .unwrap(),
            Some(serde_json::json!("blue"))
        );
        assert_eq!(
            get_param_value_scoped_checked(&server, ParamValueScope::Profile("Alt"), spec, "size")
                .unwrap(),
            None,
            "a key the source lacks is cleared at the destination, not left stale"
        );
        assert_eq!(
            get_param_value_scoped_checked(
                &server,
                ParamValueScope::Profile("Main"),
                spec,
                "color"
            )
            .unwrap(),
            Some(serde_json::json!("blue")),
            "the source is untouched"
        );
        assert!(
            copy_profile_param_values_if_unchanged(&server, &row, &params, "Main", "Main").is_err()
        );
        assert_eq!(
            copy_profile_param_values_if_unchanged(&server, &global_row, &params, "Main", "Alt")
                .unwrap(),
            PackageParamCommit::StateChanged,
            "a stale row snapshot writes nothing"
        );
    }

    #[test]
    fn reconcile_drops_a_folderless_local_row_but_keeps_its_settings() {
        let server = test_server("reconcile-keeps-settings");
        let home = get_smudgy_home().unwrap();
        fs::create_dir_all(home.join(&server).join("packages")).unwrap();
        install_package(&server, "smudgy://local/gone", UpdateMode::Auto, true).unwrap();
        save_param_value(&server, "smudgy://local/gone", "k", serde_json::json!(1)).unwrap();

        assert_eq!(
            reconcile_local_installs(&server).unwrap(),
            vec!["smudgy://local/gone".to_string()]
        );
        assert!(
            load_lock(&server)
                .unwrap()
                .find("smudgy://local/gone")
                .is_none()
        );
        assert!(
            package_param_state_exists(&server, "smudgy://local/gone").unwrap(),
            "an absent folder is not a confirmed deletion; settings stay for adoption"
        );
    }

    #[test]
    fn version_floor_folds_highest_and_poisons_on_invalid() {
        let running = semver::Version::parse("0.5.0").unwrap();
        let mut floor = SmudgyVersionFloor::default();
        floor.fold("a", Some("0.4.0"));
        assert!(floor.refusal(&running).is_none());
        floor.fold("b", Some("0.6.0"));
        assert!(
            floor
                .refusal(&running)
                .unwrap()
                .contains("b requires smudgy 0.6.0")
        );
        floor.fold("c", Some("nope"));
        assert!(floor.refusal(&running).unwrap().contains("unusable"));
    }

    #[test]
    fn script_param_values_must_match_the_declared_shape() {
        let scalar = |key: &str, kind: ParamKind| PackageParameter {
            key: key.to_string(),
            label: None,
            secret: false,
            required: false,
            kind,
            default: None,
            options: Vec::new(),
            fields: Vec::new(),
        };
        let mut dropdown = scalar("mode", ParamKind::Dropdown);
        dropdown.options = vec![smudgy_script::ParamOption {
            value: "safe".to_string(),
            label: None,
        }];
        let mut list = scalar("tags", ParamKind::List);
        list.fields = vec![scalar("tag", ParamKind::String)];
        let mut table = scalar("routes", ParamKind::Table);
        table.fields = vec![
            scalar("from", ParamKind::String),
            scalar("priority", ParamKind::Number),
        ];

        assert!(validate_package_param_value(&dropdown, &serde_json::json!("safe")).is_ok());
        assert!(validate_package_param_value(&dropdown, &serde_json::json!("fast")).is_err());
        assert!(validate_package_param_value(&list, &serde_json::json!(["a", "b"])).is_ok());
        assert!(validate_package_param_value(&list, &serde_json::json!(["a", 2])).is_err());
        assert!(
            validate_package_param_value(
                &table,
                &serde_json::json!([{"from": "square", "priority": 10}, {"from": "gate"}])
            )
            .is_ok()
        );
        assert!(
            validate_package_param_value(&table, &serde_json::json!([{"unknown": true}])).is_err()
        );
        assert!(validate_package_param_value(&table, &serde_json::json!([null])).is_err());
    }
}
