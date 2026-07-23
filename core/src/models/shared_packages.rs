//! Per-server state for installed `smudgy://` shared packages.
//!
//! "Installing" a package associates its specifier with a **server** so it auto-loads on
//! every session to that server (see `smudgy_script` and `DESIGN.md`). Installs are
//! per-server — not per-profile — so they sit alongside the server's aliases/triggers/
//! hotkeys/modules, which are also server-wide. Three kinds of state live here, all
//! scoped to `<smudgy_home>/<server>/`:
//!
//! - the **lockfile** (`smudgy.lock.json`) — the install list plus, per package, the
//!   update mode (auto-latest by default, or pinned to a version) and the
//!   last-resolved version + integrity hash (for offline reuse and reproducibility);
//! - **non-secret option values** (`smudgy.options.json`) — values for a package's
//!   declared non-secret parameters (`DESIGN.md`);
//! - **secret option values** — declared *secret* parameters go to the OS keyring (with
//!   an obfuscated-file fallback), never to plain JSON, mirroring the cloud-session
//!   token in [`crate::models::auth`].
//!
//! The package **manifest** types (`smudgy.package.json`) are owned by `smudgy_script`
//! and re-exported here so `core` has a single import surface.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

pub use smudgy_script::{
    ImportPolicy, ParamKind, ParamOption, PackageManifest, PackageParameter, PackagePermissions,
    SmudgyCapabilities,
};

use crate::get_smudgy_home;
use crate::models::auth::{hex_decode, hex_encode, obfuscate};

/// Lockfile name, relative to a server directory.
const LOCK_FILE: &str = "smudgy.lock.json";
/// Non-secret param-values file name, relative to a server directory.
const PARAMS_FILE: &str = "smudgy.params.json";
/// Obfuscated secret-option fallback file (used only when no OS keyring is available).
const SECRETS_FILE: &str = ".package-secrets.json";

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
    /// Whether the user has **trusted** this package, promoting it (and its closure) onto
    /// the trusted main isolate — allow-all, shared instances — instead of its own
    /// sandboxed per-package isolate (`script/PACKAGE-ISOLATES.md`). A per-profile
    /// user decision, default `false`: installed packages are sandboxed until trusted. The
    /// engine reads this to partition the install set across isolates at session start.
    #[serde(default)]
    pub trusted: bool,
    /// The deno-native permission union the user consented to at install (or last update
    /// re-consent) — the **enforced** grant for this package's sandboxed isolate and the
    /// baseline an update's delta is computed against (see [`PackagePermissions::added_since`]
    /// and `script/PACKAGE-ISOLATES-CONSENT-TRUST.md`). Stored as the whole *closure* union
    /// captured at consent time (the all-or-nothing grant records everything the closure asked
    /// for), not a hash — the delta needs the old set to subtract from the new. `None` = never
    /// consented: enforcement treats that as the empty union, denying everything ("must
    /// consent"). Moot while `trusted` — a trusted package runs allow-all on the main isolate,
    /// with nothing to enforce.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consented_permissions: Option<PackagePermissions>,
    /// Whether this installed package is **enabled** (loaded + run by the engine). A per-profile
    /// user decision: an enabled install (and its dependency closure) is loaded at session start;
    /// a disabled one is skipped entirely, so the user can install + consent now and review the
    /// code before it ever executes (toggle it on later — no re-consent — to run it). The engine
    /// reads this when partitioning the install set (`script_engine`); the manage pane's enable
    /// toggle flips it and reloads the session live. Defaults to `true` so installs predating this
    /// field (and the normal install path) keep running.
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    /// Whether this package was installed **automatically because another package `requires` it**
    /// (vs. installed explicitly by the user) — apt's "automatically installed" mark. When the last
    /// package that required it is uninstalled, an auto-installed requirement becomes an *orphan*
    /// candidate and the user is prompted to remove it too (never removed silently). An explicit
    /// (re)install clears the flag — the user owns it. Defaults to `false` so pre-existing and
    /// user-installed entries are never treated as orphans. See `script/REQUIRED-PACKAGES.md`.
    #[serde(default)]
    pub installed_as_requirement: bool,
}

/// The serde default for [`LockedPackage::enabled`] — `true`, so a lock entry written before this
/// field existed (or by any path that doesn't set it) is treated as enabled and keeps running.
fn default_enabled() -> bool {
    true
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
            trusted: false,
            consented_permissions: None,
            enabled: true,
            installed_as_requirement: false,
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

    /// Insert or replace an installed package by specifier.
    pub fn upsert(&mut self, package: LockedPackage) {
        if let Some(existing) = self
            .packages
            .iter_mut()
            .find(|p| p.specifier == package.specifier)
        {
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

    /// The auto-installed requirements ([`installed_as_requirement`](LockedPackage::installed_as_requirement))
    /// that would become **orphans** if `removing` were uninstalled — i.e. nothing left would
    /// `require` them — so the uninstall flow can offer to remove them too (apt-style; never
    /// silent). `requires_of` maps each still-installed package's specifier to the specifiers it
    /// `requires` (the caller derives this from the resolved manifests). The result is transitive:
    /// if removing A orphans B and B was the only thing requiring C, both B and C are returned.
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
    /// gone (broken), so the uninstall flow removes them alongside it (apt: "the following packages
    /// will be REMOVED"). Forward [`orphaned_by_removal`](Self::orphaned_by_removal) covers the other
    /// direction (what `removing` pulled in); this is the requirers pointing *at* it. `requires_of`
    /// maps each specifier to the specifiers it `requires`. `removing` itself is never included;
    /// order is deterministic (lockfile order). See `script/REQUIRED-PACKAGES.md`.
    #[must_use]
    pub fn requirers_of_removal(
        &self,
        removing: &str,
        requires_of: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        let mut doomed: std::collections::HashSet<&str> = std::collections::HashSet::new();
        doomed.insert(removing);
        loop {
            // The next still-living package that requires something already doomed — it breaks too.
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
    /// whole set is gone ([`RemovalPlan::orphans`]). The orphan sweep is seeded with the breaks too,
    /// so a requirement kept alive only by a breaking dependent is correctly surfaced. This is the
    /// full apt-style picture for one uninstall (`script/REQUIRED-PACKAGES.md`).
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

    /// The auto-installed requirements left orphaned once every package in `seeds` is removed: an
    /// `installed_as_requirement` package nothing *outside* the removed set still `requires`.
    /// Transitive (removing an orphan can orphan its own requirements). Seeds are never returned;
    /// order is deterministic (lockfile order).
    fn orphans_after(
        &self,
        seeds: &std::collections::HashSet<&str>,
        requires_of: &HashMap<String, Vec<String>>,
    ) -> Vec<String> {
        let mut removed: std::collections::HashSet<&str> = seeds.clone();
        loop {
            // Everything still required by a package that is NOT (yet) being removed.
            let still_required: std::collections::HashSet<&str> = self
                .packages
                .iter()
                .filter(|p| !removed.contains(p.specifier.as_str()))
                .filter_map(|p| requires_of.get(&p.specifier))
                .flatten()
                .map(String::as_str)
                .collect();
            // The next auto-installed package nothing requires anymore.
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
        // Preserve lockfile order; drop the seeds (only newly-orphaned packages are returned).
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
    /// removed — removed alongside it (forced; apt's "will be REMOVED").
    pub breaks: Vec<String>,
    /// Auto-installed requirements left unneeded once the target + `breaks` are gone (apt's
    /// "automatically installed and no longer required" — offered, not forced).
    pub orphans: Vec<String>,
}

/// `<smudgy_home>/<server>/` — where the server-wide package state lives.
fn server_dir(server_name: &str) -> Result<PathBuf> {
    Ok(get_smudgy_home()?.join(server_name))
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
    load_lock_in(&server_dir(server_name)?)
}

/// Saves a server's package lockfile, creating the server directory if needed.
///
/// # Errors
/// Returns an error if the lock can't be serialized or written.
pub fn save_lock(server_name: &str, lock: &SharedPackageLock) -> Result<()> {
    save_lock_in(&server_dir(server_name)?, lock)
}

/// Installs a package for a server (auto-update unless `mode` says otherwise), replacing
/// any existing entry for the same specifier. `enabled` is written as part of the same lock
/// write so an "install, don't enable" never transiently persists as `enabled: true`
/// (`script/PACKAGE-ISOLATES-CONSENT-TRUST.md`) — the engine only loads enabled installs.
///
/// # Errors
/// Returns an error if the lockfile can't be loaded or saved.
pub fn install_package(
    server_name: &str,
    specifier: &str,
    mode: UpdateMode,
    enabled: bool,
) -> Result<()> {
    let mut lock = load_lock(server_name)?;
    // Preserve any prior resolution metadata when re-installing.
    let mut package = lock
        .find(specifier)
        .cloned()
        .unwrap_or_else(|| LockedPackage::new(specifier, mode.clone()));
    package.mode = mode;
    package.enabled = enabled;
    // An explicit install means the user owns this package: clear the auto-installed mark so a
    // later orphan sweep never offers to remove it.
    package.installed_as_requirement = false;
    lock.upsert(package);
    save_lock(server_name, &lock)
}

/// Installs a package that was pulled in **automatically because another package `requires` it**
/// (apt's "automatically installed"). Like [`install_package`], but a *newly* created entry is
/// marked [`installed_as_requirement`](LockedPackage::installed_as_requirement) so it can be
/// offered for orphan removal later. An entry that already exists keeps its current mark — a
/// user-owned package stays user-owned even when something starts requiring it.
///
/// # Errors
/// Returns an error if the lockfile can't be loaded or saved.
pub fn install_required_package(
    server_name: &str,
    specifier: &str,
    mode: UpdateMode,
    enabled: bool,
) -> Result<()> {
    let mut lock = load_lock(server_name)?;
    let (mut package, is_new) = match lock.find(specifier) {
        Some(existing) => (existing.clone(), false),
        None => (LockedPackage::new(specifier, mode.clone()), true),
    };
    package.mode = mode;
    package.enabled = enabled;
    if is_new {
        package.installed_as_requirement = true;
    }
    lock.upsert(package);
    save_lock(server_name, &lock)
}

/// Removes a package from a server.
///
/// # Errors
/// Returns an error if the lockfile can't be loaded or saved.
pub fn uninstall_package(server_name: &str, specifier: &str) -> Result<()> {
    let mut lock = load_lock(server_name)?;
    if lock.remove(specifier) {
        save_lock(server_name, &lock)?;
    }
    Ok(())
}

/// Reconciles installs under the reserved
/// [`LOCAL_OWNER`](crate::models::local_packages::LOCAL_OWNER) placeholder with reality:
///
/// - An entry whose backing `<server>/packages/<name>/` folder no longer exists is **removed**.
///   It can never resolve again — the owner is reserved server-side, so there is no published
///   copy to fall back to — and would otherwise linger as an installed package that fails to
///   load every session (e.g. a lockfile written by an app version whose package delete left
///   install entries behind).
/// - When `nickname` is known, an entry whose folder does exist is **migrated** to
///   `smudgy://<nickname>/<name>` — the owner segment records the sign-in state the install was
///   written under, and the nickname form is the one the Automations window manages once the
///   account has a handle. If the nickname form is already installed it wins and the placeholder
///   duplicate is dropped.
///
/// Account-owned (`smudgy://<nickname>/…`) installs are never touched here: those can
/// legitimately outlive the folder by resolving to a published copy. Returns the changed
/// specifiers (removed or migrated); when it is empty the lockfile was already clean and
/// nothing is written.
///
/// # Errors
/// Returns an error if the lockfile can't be loaded or saved, or the packages directory can't
/// be determined.
pub fn reconcile_local_installs(
    server_name: &str,
    nickname: Option<&str>,
) -> Result<Vec<String>> {
    let mut lock = load_lock(server_name)?;
    let packages_dir = crate::models::local_packages::packages_dir(server_name)?;
    let prefix = format!("smudgy://{}/", crate::models::local_packages::LOCAL_OWNER);
    let mut changed: Vec<String> = Vec::new();
    let mut migrated: Vec<LockedPackage> = Vec::new();
    lock.packages.retain(|package| {
        let Some(name) = package
            .specifier
            .strip_prefix(&prefix)
            .filter(|name| !name.is_empty())
        else {
            return true;
        };
        if !packages_dir.join(name).exists() {
            changed.push(package.specifier.clone());
            return false;
        }
        if let Some(nick) = nickname {
            changed.push(package.specifier.clone());
            let mut entry = package.clone();
            entry.specifier = format!("smudgy://{nick}/{name}");
            migrated.push(entry);
            return false;
        }
        true
    });
    for entry in migrated {
        // The nickname form, when already installed, wins over the placeholder duplicate.
        if lock.find(&entry.specifier).is_none() {
            lock.packages.push(entry);
        }
    }
    if !changed.is_empty() {
        save_lock(server_name, &lock)?;
    }
    Ok(changed)
}

/// Sets the update mode (auto vs pinned) for an already-installed package.
///
/// # Errors
/// Returns an error if the package isn't installed, or the lockfile can't be saved.
pub fn set_update_mode(server_name: &str, specifier: &str, mode: UpdateMode) -> Result<()> {
    let mut lock = load_lock(server_name)?;
    let package = lock
        .packages
        .iter_mut()
        .find(|p| p.specifier == specifier)
        .with_context(|| format!("package {specifier} is not installed"))?;
    package.mode = mode;
    save_lock(server_name, &lock)
}

/// Sets whether an already-installed package is **trusted** (promoted onto the allow-all main
/// isolate) — the per-profile decision behind the trust toggle
/// (`script/PACKAGE-ISOLATES-CONSENT-TRUST.md`). Trusting *is* the consent (it grants
/// everything, so no separate permission record is needed); untrusting returns the package to
/// its sandbox + its last [`consented_permissions`](LockedPackage::consented_permissions).
/// Takes effect on the next session reload (there is no live isolate migration). Mirrors
/// [`set_update_mode`].
///
/// # Errors
/// Returns an error if the package isn't installed, or the lockfile can't be saved.
pub fn set_trusted(server_name: &str, specifier: &str, trusted: bool) -> Result<()> {
    let mut lock = load_lock(server_name)?;
    let package = lock
        .packages
        .iter_mut()
        .find(|p| p.specifier == specifier)
        .with_context(|| format!("package {specifier} is not installed"))?;
    package.trusted = trusted;
    save_lock(server_name, &lock)
}

/// Sets whether an already-installed package is **enabled** (loaded + run by the engine). A
/// disabled package — and its dependency closure — is skipped at session start, so the user can
/// install + consent now and review the code before it executes, then enable it later (no
/// re-consent) to run it. Takes effect on the next session reload (the manage pane reloads the
/// session live when toggling). Mirrors [`set_trusted`].
///
/// # Errors
/// Returns an error if the package isn't installed, or the lockfile can't be saved.
pub fn set_enabled(server_name: &str, specifier: &str, enabled: bool) -> Result<()> {
    let mut lock = load_lock(server_name)?;
    let package = lock
        .packages
        .iter_mut()
        .find(|p| p.specifier == specifier)
        .with_context(|| format!("package {specifier} is not installed"))?;
    package.enabled = enabled;
    save_lock(server_name, &lock)
}

/// Records the deno-native permission union the user consented to for an already-installed
/// package (`script/PACKAGE-ISOLATES-CONSENT-TRUST.md`) — the all-or-nothing grant the
/// install/update confirmation captures. Store the whole *closure* union; the engine
/// enforces exactly this for the package's sandboxed isolate, and an update's delta is
/// computed against it. Mirrors [`set_update_mode`]; pairs with [`install_package`] so a lock
/// entry need never be left without a consent record.
///
/// # Errors
/// Returns an error if the package isn't installed, or the lockfile can't be saved.
pub fn record_consent(
    server_name: &str,
    specifier: &str,
    permissions: &PackagePermissions,
) -> Result<()> {
    let mut lock = load_lock(server_name)?;
    let package = lock
        .packages
        .iter_mut()
        .find(|p| p.specifier == specifier)
        .with_context(|| format!("package {specifier} is not installed"))?;
    package.consented_permissions = Some(permissions.clone());
    save_lock(server_name, &lock)
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
    let mut lock = load_lock(server_name)?;
    if let Some(entry) = lock.packages.iter_mut().find(|p| p.specifier == specifier) {
        entry.last_resolved_version = Some(version.to_string());
        entry.integrity = Some(integrity.to_string());
    } else {
        lock.packages.push(LockedPackage {
            specifier: specifier.to_string(),
            mode: UpdateMode::Auto,
            last_resolved_version: Some(version.to_string()),
            integrity: Some(integrity.to_string()),
            trusted: false,
            consented_permissions: None,
            enabled: true,
            installed_as_requirement: false,
        });
    }
    save_lock(server_name, &lock)
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
    fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))
}

// ---------------------------------------------------------------------------
// Non-secret option values
// ---------------------------------------------------------------------------

/// Non-secret option values, keyed by package specifier then option key.
pub type PackageParamValues = HashMap<String, HashMap<String, serde_json::Value>>;

/// Loads non-secret option values for a server. A missing file yields an empty map.
///
/// # Errors
/// Returns an error if the file exists but can't be read or parsed.
pub fn load_param_values(server_name: &str) -> Result<PackageParamValues> {
    load_param_values_in(&server_dir(server_name)?)
}

/// Sets a single non-secret option value for a package.
///
/// # Errors
/// Returns an error if the option-values file can't be loaded or saved.
pub fn save_param_value(
    server_name: &str,
    specifier: &str,
    key: &str,
    value: serde_json::Value,
) -> Result<()> {
    let dir = server_dir(server_name)?;
    let mut values = load_param_values_in(&dir)?;
    values
        .entry(specifier.to_string())
        .or_default()
        .insert(key.to_string(), value);
    save_param_values_in(&dir, &values)
}

/// Removes a single non-secret option value for a package, leaving it unset (so `get()` reads
/// null rather than an explicit value). A no-op if the key was never set. The non-secret mirror
/// of [`clear_secret_param`]; the package's entry is dropped once its last value is removed so
/// `smudgy.params.json` doesn't accumulate empty objects.
///
/// # Errors
/// Returns an error if the option-values file can't be loaded or saved.
pub fn clear_param_value(server_name: &str, specifier: &str, key: &str) -> Result<()> {
    let dir = server_dir(server_name)?;
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

/// Whether a declared param has a value set for `specifier` on `server_name` — a secret in
/// the keyring (or fallback), a non-secret in `smudgy.params.json`. (A manifest `default` is
/// not consulted here; the load-gate requires an explicitly-set value for required params.)
#[must_use]
pub fn param_has_value(server_name: &str, specifier: &str, param: &PackageParameter) -> bool {
    if param.secret {
        load_secret_param(server_name, specifier, &param.key).is_some()
    } else {
        get_param_value(server_name, specifier, &param.key).is_some()
    }
}

/// The keys of `specifier`'s **required** params that have no value set — the load-gate
/// input. Empty means the package is fully configured and may load. Non-required and
/// already-set params are excluded.
#[must_use]
pub fn missing_required_params(
    server_name: &str,
    specifier: &str,
    params: &[PackageParameter],
) -> Vec<String> {
    params
        .iter()
        .filter(|param| param.required && !param_has_value(server_name, specifier, param))
        .map(|param| param.key.clone())
        .collect()
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
/// floor that can't be read must not silently pass. Fold each manifest's declaration with
/// [`fold`](Self::fold), then ask [`refusal`](Self::refusal) why the set can't run here,
/// if it can't.
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
                if self.highest.as_ref().is_none_or(|(highest, _)| version > *highest) {
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
    /// [`running_smudgy_release`]), or `None` when the floor is absent or satisfied. The
    /// reason names the declaring package, so a closure-fold caller surfaces the culprit
    /// even when it is a transitive dependency, and carries the case's remedy — "update
    /// smudgy" helps only a too-low version, never an unreadable declaration — so callers
    /// prepend context without appending advice.
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

/// Reads a single non-secret option value for a package, if set.
#[must_use]
pub fn get_param_value(
    server_name: &str,
    specifier: &str,
    key: &str,
) -> Option<serde_json::Value> {
    let dir = server_dir(server_name).ok()?;
    let values = load_param_values_in(&dir).ok()?;
    values.get(specifier)?.get(key).cloned()
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
    fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))
}

// ---------------------------------------------------------------------------
// Secret option values (OS keyring, obfuscated-file fallback)
// ---------------------------------------------------------------------------

/// The keyring slot for a package's secret option. Unique per
/// (server, package, option). The string is opaque — never parsed back.
fn secret_slot(server_name: &str, specifier: &str, key: &str) -> String {
    format!("pkgparam:{server_name}:{specifier}:{key}")
}

fn secret_keyring_entry(slot: &str) -> keyring::Result<keyring::Entry> {
    // Same dev-aware service as the session token, so a dev build's package secrets are
    // isolated from a release build's alongside its login.
    keyring::Entry::new(crate::models::auth::keyring_service(), slot)
}

/// Stores a secret option value in the OS keyring (obfuscated-file fallback if no
/// keyring is available). Secret values are never written to plain JSON or logged.
///
/// # Errors
/// Returns an error if both the keyring write and the fallback-file write fail.
pub fn save_secret_param(
    server_name: &str,
    specifier: &str,
    key: &str,
    value: &str,
) -> Result<()> {
    let slot = secret_slot(server_name, specifier, key);
    match secret_keyring_entry(&slot).and_then(|entry| entry.set_password(value)) {
        Ok(()) => Ok(()),
        Err(e) => {
            warn!("OS keyring unavailable for package secret, falling back to obfuscated file: {e}");
            let dir = server_dir(server_name)?;
            save_secret_to_file(&dir, &slot, value)
        }
    }
}

/// Whether a keyring-read failure has already been warned about this process. Secret reads
/// happen on the per-line hot path (a script may `get()` a secret in a trigger), so on a
/// keyring-unavailable host the warning would otherwise flood the log; warn once.
static KEYRING_READ_WARNED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Reads a secret option value, if stored. Tries the OS keyring, then the fallback
/// file. Never logs secret material.
#[must_use]
pub fn load_secret_param(server_name: &str, specifier: &str, key: &str) -> Option<String> {
    let slot = secret_slot(server_name, specifier, key);
    match secret_keyring_entry(&slot).and_then(|entry| entry.get_password()) {
        Ok(value) => Some(value),
        Err(e) => {
            // NoEntry is the normal "not stored here" case (fall through to the file). Any
            // other error (no Secret Service, locked keyring) we warn about, but only once.
            if !matches!(e, keyring::Error::NoEntry)
                && !KEYRING_READ_WARNED.swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                warn!("Failed to read a package secret from the OS keyring (further occurrences suppressed); using the obfuscated-file fallback: {e}");
            }
            let dir = server_dir(server_name).ok()?;
            load_secret_from_file(&dir, &slot)
        }
    }
}

/// Removes a secret option value from both the keyring and the fallback file.
///
/// # Errors
/// Returns an error if an existing keyring entry or fallback file couldn't be removed.
pub fn clear_secret_param(server_name: &str, specifier: &str, key: &str) -> Result<()> {
    let slot = secret_slot(server_name, specifier, key);
    let keyring_result = match secret_keyring_entry(&slot).and_then(|e| e.delete_credential()) {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(anyhow::anyhow!(
            "Failed to delete package secret from the OS keyring: {e}"
        )),
    };
    let file_result =
        server_dir(server_name).and_then(|dir| remove_secret_from_file(&dir, &slot));
    keyring_result?;
    file_result
}

/// The obfuscated secrets fallback map: slot → hex(obfuscate(value)).
type SecretsFile = HashMap<String, String>;

fn load_secrets_file(dir: &Path) -> SecretsFile {
    let path = dir.join(SECRETS_FILE);
    match fs::read_to_string(&path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or_else(|e| {
            warn!("Package secrets fallback file is malformed, ignoring it: {e}");
            SecretsFile::new()
        }),
        Err(_) => SecretsFile::new(),
    }
}

fn save_secret_to_file(dir: &Path, slot: &str, value: &str) -> Result<()> {
    fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create server dir {}", dir.display()))?;
    let mut secrets = load_secrets_file(dir);
    secrets.insert(slot.to_string(), hex_encode(&obfuscate(value.as_bytes())));
    let path = dir.join(SECRETS_FILE);
    let json = serde_json::to_string(&secrets).context("Failed to serialize package secrets")?;
    fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))
}

fn load_secret_from_file(dir: &Path, slot: &str) -> Option<String> {
    let secrets = load_secrets_file(dir);
    let encoded = secrets.get(slot)?;
    let bytes = hex_decode(encoded)?;
    String::from_utf8(obfuscate(&bytes)).ok()
}

fn remove_secret_from_file(dir: &Path, slot: &str) -> Result<()> {
    let path = dir.join(SECRETS_FILE);
    if !path.exists() {
        return Ok(());
    }
    let mut secrets = load_secrets_file(dir);
    if secrets.remove(slot).is_some() {
        let json = serde_json::to_string(&secrets).context("Failed to serialize package secrets")?;
        fs::write(&path, json).with_context(|| format!("Failed to write {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn use_temp_smudgy_home() {
        static TEST_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        TEST_HOME.get_or_init(|| {
            let dir = temp_dir("home");
            crate::set_smudgy_home(dir.clone());
            dir
        });
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "smudgy-pkg-test-{name}-{}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn lock_round_trips() {
        let dir = temp_dir("lock");
        let mut lock = SharedPackageLock::default();
        lock.upsert(LockedPackage::new("smudgy://wbk/mapper", UpdateMode::Auto));
        lock.upsert(LockedPackage {
            specifier: "smudgy://wbk/util".into(),
            mode: UpdateMode::Pinned {
                version: "1.2.0".into(),
            },
            last_resolved_version: Some("1.2.0".into()),
            integrity: Some("sha256-abc".into()),
            trusted: false,
            consented_permissions: None,
            enabled: true,
            installed_as_requirement: false,
        });

        save_lock_in(&dir, &lock).expect("save");
        assert_eq!(load_lock_in(&dir).unwrap(), lock);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_lock_is_empty() {
        let dir = temp_dir("lock-missing");
        assert_eq!(load_lock_in(&dir).unwrap(), SharedPackageLock::default());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn version_floor_absent_or_blank_is_satisfied() {
        let running = semver::Version::new(0, 3, 3);
        let mut floor = SmudgyVersionFloor::default();
        assert_eq!(floor.refusal(&running), None);
        floor.fold("mapper", None);
        floor.fold("mapper", Some("  "));
        assert_eq!(floor.refusal(&running), None);
    }

    #[test]
    fn version_floor_compares_against_running() {
        let running = semver::Version::new(0, 3, 3);
        let mut floor = SmudgyVersionFloor::default();
        floor.fold("mapper", Some("0.3.3"));
        assert_eq!(floor.refusal(&running), None, "an equal floor is satisfied");
        floor.fold("mapper", Some("0.4.0"));
        let reason = floor.refusal(&running).expect("a higher floor refuses");
        assert!(reason.contains("mapper requires smudgy 0.4.0 or newer"), "{reason}");
        assert!(reason.contains("this smudgy is 0.3.3"), "{reason}");
    }

    #[test]
    fn version_floor_keeps_the_highest_declaration() {
        // The culprit named is the package with the HIGHEST floor, whatever the fold order.
        let running = semver::Version::new(0, 3, 3);
        let mut floor = SmudgyVersionFloor::default();
        floor.fold("dep-lib", Some("0.5.0"));
        floor.fold("mapper", Some("0.4.0"));
        let reason = floor.refusal(&running).expect("refuses");
        assert!(reason.contains("dep-lib requires smudgy 0.5.0"), "{reason}");
    }

    #[test]
    fn version_floor_unparseable_is_fail_closed() {
        // A floor that can't be read refuses even on an arbitrarily new smudgy.
        let running = semver::Version::new(99, 0, 0);
        let mut floor = SmudgyVersionFloor::default();
        floor.fold("mapper", Some("banana"));
        let reason = floor.refusal(&running).expect("unparseable refuses");
        assert!(reason.contains("mapper"), "{reason}");
        assert!(reason.contains("banana"), "{reason}");
    }

    #[test]
    fn running_release_strips_the_channel_suffix() {
        // A dev/RC build of X.Y.Z satisfies a floor of X.Y.Z: the release comparison
        // point carries no prerelease, so `min > running` is false for an equal floor.
        let release = running_smudgy_release();
        assert!(release.pre.is_empty() && release.build.is_empty());
        let mut floor = SmudgyVersionFloor::default();
        floor.fold("mapper", Some(&release.to_string()));
        assert_eq!(floor.refusal(&release), None);
    }

    /// Build a lock from `(specifier, installed_as_requirement)` pairs, in order.
    fn lock_of(entries: &[(&str, bool)]) -> SharedPackageLock {
        let mut lock = SharedPackageLock::default();
        for (spec, auto) in entries {
            let mut p = LockedPackage::new(*spec, UpdateMode::Auto);
            p.installed_as_requirement = *auto;
            lock.packages.push(p);
        }
        lock
    }

    fn requires_map(entries: &[(&str, &[&str])]) -> HashMap<String, Vec<String>> {
        entries
            .iter()
            .map(|(s, reqs)| {
                ((*s).to_string(), reqs.iter().map(|r| (*r).to_string()).collect())
            })
            .collect()
    }

    #[test]
    fn orphan_sweep_returns_a_requirement_nothing_else_needs() {
        // app (user) requires lib (auto). Removing app orphans lib.
        let lock = lock_of(&[("smudgy://k/app", false), ("smudgy://k/lib", true)]);
        let reqs = requires_map(&[("smudgy://k/app", &["smudgy://k/lib"])]);
        assert_eq!(
            lock.orphaned_by_removal("smudgy://k/app", &reqs),
            vec!["smudgy://k/lib".to_string()]
        );
    }

    #[test]
    fn orphan_sweep_keeps_a_requirement_another_package_still_needs() {
        // Both app and tool require lib; removing app leaves tool needing lib → not an orphan.
        let lock = lock_of(&[
            ("smudgy://k/app", false),
            ("smudgy://k/tool", false),
            ("smudgy://k/lib", true),
        ]);
        let reqs = requires_map(&[
            ("smudgy://k/app", &["smudgy://k/lib"]),
            ("smudgy://k/tool", &["smudgy://k/lib"]),
        ]);
        assert!(lock.orphaned_by_removal("smudgy://k/app", &reqs).is_empty());
    }

    #[test]
    fn orphan_sweep_never_offers_a_user_owned_package() {
        // lib is user-installed (auto=false); even with nothing requiring it, it is never an orphan.
        let lock = lock_of(&[("smudgy://k/app", false), ("smudgy://k/lib", false)]);
        let reqs = requires_map(&[("smudgy://k/app", &["smudgy://k/lib"])]);
        assert!(lock.orphaned_by_removal("smudgy://k/app", &reqs).is_empty());
    }

    #[test]
    fn orphan_sweep_cascades_transitively() {
        // app(user) → group(auto) → prompt(auto). Removing app orphans both group and prompt.
        let lock = lock_of(&[
            ("smudgy://k/app", false),
            ("smudgy://k/group", true),
            ("smudgy://k/prompt", true),
        ]);
        let reqs = requires_map(&[
            ("smudgy://k/app", &["smudgy://k/group"]),
            ("smudgy://k/group", &["smudgy://k/prompt"]),
        ]);
        let mut orphans = lock.orphaned_by_removal("smudgy://k/app", &reqs);
        orphans.sort();
        assert_eq!(
            orphans,
            vec!["smudgy://k/group".to_string(), "smudgy://k/prompt".to_string()]
        );
    }

    #[test]
    fn orphan_sweep_keeps_transitive_dep_with_another_requirer() {
        // app(user) → group(auto) → prompt(auto); other(user) → prompt too.
        // Removing app orphans group, but prompt is still required by other.
        let lock = lock_of(&[
            ("smudgy://k/app", false),
            ("smudgy://k/other", false),
            ("smudgy://k/group", true),
            ("smudgy://k/prompt", true),
        ]);
        let reqs = requires_map(&[
            ("smudgy://k/app", &["smudgy://k/group"]),
            ("smudgy://k/group", &["smudgy://k/prompt"]),
            ("smudgy://k/other", &["smudgy://k/prompt"]),
        ]);
        assert_eq!(
            lock.orphaned_by_removal("smudgy://k/app", &reqs),
            vec!["smudgy://k/group".to_string()]
        );
    }

    #[test]
    fn requirers_sweep_returns_a_dependent_that_would_break() {
        // mapper(user) requires prompt(auto). Removing prompt would break mapper → it must go too.
        let lock = lock_of(&[("smudgy://k/mapper", false), ("smudgy://k/prompt", true)]);
        let reqs = requires_map(&[("smudgy://k/mapper", &["smudgy://k/prompt"])]);
        assert_eq!(
            lock.requirers_of_removal("smudgy://k/prompt", &reqs),
            vec!["smudgy://k/mapper".to_string()]
        );
    }

    #[test]
    fn requirers_sweep_cascades_transitively() {
        // autoloot → group → prompt. Removing prompt breaks group, which breaks autoloot.
        let lock = lock_of(&[
            ("smudgy://k/autoloot", false),
            ("smudgy://k/group", true),
            ("smudgy://k/prompt", true),
        ]);
        let reqs = requires_map(&[
            ("smudgy://k/autoloot", &["smudgy://k/group"]),
            ("smudgy://k/group", &["smudgy://k/prompt"]),
        ]);
        let mut breaks = lock.requirers_of_removal("smudgy://k/prompt", &reqs);
        breaks.sort();
        assert_eq!(
            breaks,
            vec!["smudgy://k/autoloot".to_string(), "smudgy://k/group".to_string()]
        );
    }

    #[test]
    fn requirers_sweep_ignores_packages_that_do_not_require_it() {
        // unrelated(user) requires nothing of prompt → not a requirer.
        let lock = lock_of(&[("smudgy://k/unrelated", false), ("smudgy://k/prompt", false)]);
        let reqs = requires_map(&[]);
        assert!(lock.requirers_of_removal("smudgy://k/prompt", &reqs).is_empty());
    }

    #[test]
    fn plan_removal_combines_breaks_and_orphans() {
        // mapper(user) requires prompt(auto) and group(auto); group is shared by nobody else.
        // Removing prompt breaks mapper; once mapper is gone, group (auto, nothing left needs it)
        // is an orphan. prompt itself is the target, not an orphan.
        let lock = lock_of(&[
            ("smudgy://k/mapper", false),
            ("smudgy://k/prompt", true),
            ("smudgy://k/group", true),
        ]);
        let reqs = requires_map(&[(
            "smudgy://k/mapper",
            &["smudgy://k/prompt", "smudgy://k/group"],
        )]);
        let plan = lock.plan_removal("smudgy://k/prompt", &reqs);
        assert_eq!(plan.breaks, vec!["smudgy://k/mapper".to_string()]);
        assert_eq!(plan.orphans, vec!["smudgy://k/group".to_string()]);
    }

    #[test]
    fn plan_removal_of_a_leaf_requirer_has_no_breaks() {
        // Removing the top-level requirer itself: nothing requires it, so no breaks; its auto-only
        // requirement becomes an orphan (matches orphaned_by_removal).
        let lock = lock_of(&[("smudgy://k/mapper", false), ("smudgy://k/prompt", true)]);
        let reqs = requires_map(&[("smudgy://k/mapper", &["smudgy://k/prompt"])]);
        let plan = lock.plan_removal("smudgy://k/mapper", &reqs);
        assert!(plan.breaks.is_empty());
        assert_eq!(plan.orphans, vec!["smudgy://k/prompt".to_string()]);
    }

    #[test]
    fn update_mode_defaults_to_auto() {
        // A lockfile entry without an explicit mode deserializes as Auto.
        let json = r#"{ "packages": [ { "specifier": "smudgy://wbk/mapper" } ] }"#;
        let lock: SharedPackageLock = serde_json::from_str(json).unwrap();
        assert_eq!(lock.packages[0].mode, UpdateMode::Auto);
    }

    #[test]
    fn update_mode_serializes_tagged() {
        let auto = serde_json::to_value(UpdateMode::Auto).unwrap();
        assert_eq!(auto, serde_json::json!({ "mode": "auto" }));
        let pinned = serde_json::to_value(UpdateMode::Pinned {
            version: "1.0.0".into(),
        })
        .unwrap();
        assert_eq!(pinned, serde_json::json!({ "mode": "pinned", "version": "1.0.0" }));
    }

    #[test]
    fn upsert_replaces_by_specifier() {
        let mut lock = SharedPackageLock::default();
        lock.upsert(LockedPackage::new("smudgy://wbk/mapper", UpdateMode::Auto));
        lock.upsert(LockedPackage::new(
            "smudgy://wbk/mapper",
            UpdateMode::Pinned {
                version: "2.0.0".into(),
            },
        ));
        assert_eq!(lock.packages.len(), 1);
        assert_eq!(lock.packages[0].pinned_version(), Some("2.0.0"));
    }

    #[test]
    fn record_consent_and_set_trusted_round_trip_through_the_lockfile() {
        use_temp_smudgy_home();
        // The consent record and the trust flag persist and reload. A unique server
        // under the active home keeps this disjoint from other tests; cleaned up at the end.
        let server = format!("ConsentTrustTest-{}", std::process::id());
        let spec = "smudgy://wbk/mapper";

        // A bare install has no consent record and is untrusted — the must-consent default.
        install_package(&server, spec, UpdateMode::Auto, true).unwrap();
        let lock = load_lock(&server).unwrap();
        let entry = lock.find(spec).expect("installed");
        assert_eq!(entry.consented_permissions, None, "a fresh install is un-consented");
        assert!(!entry.trusted, "a fresh install is untrusted");

        // Recording consent stores the granted union verbatim and reloads equal — including the
        // full-access-weight axes (`run`/`ffi`) and `sys`, which must survive the lockfile so the
        // enforcement container and the manage-pane banner keep seeing what was actually granted.
        let granted = PackagePermissions {
            net: vec!["comms.coreclan.org:6379".into()],
            read: vec!["$DATA/maps".into()],
            write: Vec::new(),
            env: vec!["MYPKG_TOKEN".into()],
            run: vec!["git".into()],
            ffi: vec!["$DATA/native/helper.dll".into()],
            sys: vec!["hostname".into()],
            import: ImportPolicy::Registries,
            smudgy: SmudgyCapabilities {
                send: true,
                echo: true,
                ..Default::default()
            },
        };
        record_consent(&server, spec, &granted).unwrap();
        let reloaded = load_lock(&server).unwrap();
        assert_eq!(
            reloaded.find(spec).and_then(|p| p.consented_permissions.clone()),
            Some(granted.clone()),
            "the consented union round-trips through the lockfile"
        );

        // Flipping trust persists independently and leaves the consent record intact —
        // untrusting must be able to return the package to its last consented union.
        set_trusted(&server, spec, true).unwrap();
        let reloaded = load_lock(&server).unwrap();
        let entry = reloaded.find(spec).expect("still installed");
        assert!(entry.trusted, "trust flips on and persists");
        assert_eq!(
            entry.consented_permissions.as_ref(),
            Some(&granted),
            "trusting keeps the prior consent record"
        );

        // Both helpers refuse a package that isn't installed (mirrors set_update_mode).
        assert!(record_consent(&server, "smudgy://wbk/absent", &granted).is_err());
        assert!(set_trusted(&server, "smudgy://wbk/absent", true).is_err());

        if let Ok(dir) = server_dir(&server) {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn enabled_defaults_true_for_pre_field_lock_entries() {
        // A lockfile entry written before the `enabled` field existed must deserialize as enabled
        // (true), so existing installs keep loading after the upgrade.
        let json = r#"{ "packages": [ { "specifier": "smudgy://wbk/mapper" } ] }"#;
        let lock: SharedPackageLock = serde_json::from_str(json).unwrap();
        assert!(lock.packages[0].enabled, "a pre-field entry defaults to enabled");
        // A freshly-installed package is enabled by default too.
        assert!(LockedPackage::new("smudgy://wbk/x", UpdateMode::Auto).enabled);
    }

    #[test]
    fn set_enabled_round_trips_through_the_lockfile() {
        use_temp_smudgy_home();
        // A unique server under the active home, cleaned up at the end.
        let server = format!("EnabledTest-{}", std::process::id());
        let spec = "smudgy://wbk/mapper";

        // Fresh install is enabled.
        install_package(&server, spec, UpdateMode::Auto, true).unwrap();
        assert!(load_lock(&server).unwrap().find(spec).unwrap().enabled);

        // Disabling persists and reloads (install + review before execute).
        set_enabled(&server, spec, false).unwrap();
        assert!(!load_lock(&server).unwrap().find(spec).unwrap().enabled);

        // Re-enabling persists (review done → run it, no re-consent).
        set_enabled(&server, spec, true).unwrap();
        assert!(load_lock(&server).unwrap().find(spec).unwrap().enabled);

        // Refuses a package that isn't installed (mirrors set_trusted/record_consent).
        assert!(set_enabled(&server, "smudgy://wbk/absent", false).is_err());

        if let Ok(dir) = server_dir(&server) {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn reconcile_local_installs_prunes_folderless_and_migrates_to_the_nickname() {
        use_temp_smudgy_home();
        // A unique server under the active home, cleaned up at the end.
        let server = format!("ReconcileTest-{}", std::process::id());
        let live = "smudgy://local/keeper";
        let stale = "smudgy://local/ghost";
        let account = "smudgy://wbk/ghost";

        // `keeper` has a backing folder; `ghost` does not (its folder was deleted). The
        // account-owned entry must never be touched: it can resolve to a published copy.
        let keeper_dir = crate::models::local_packages::packages_dir(&server)
            .expect("packages dir")
            .join("keeper");
        fs::create_dir_all(&keeper_dir).expect("create keeper folder");
        install_package(&server, live, UpdateMode::Auto, true).unwrap();
        install_package(&server, stale, UpdateMode::Auto, true).unwrap();
        install_package(&server, account, UpdateMode::Auto, true).unwrap();

        // Signed out: the folderless entry is pruned, the live one is left as-is.
        assert_eq!(
            reconcile_local_installs(&server, None).unwrap(),
            vec![stale.to_string()]
        );
        let lock = load_lock(&server).unwrap();
        assert!(lock.find(live).is_some());
        assert!(lock.find(stale).is_none());
        assert!(lock.find(account).is_some());

        // An already-clean lock changes nothing (and writes nothing).
        assert!(reconcile_local_installs(&server, None).unwrap().is_empty());

        // Signed in: the placeholder entry migrates to the nickname form, keeping its fields.
        assert_eq!(
            reconcile_local_installs(&server, Some("wbk")).unwrap(),
            vec![live.to_string()]
        );
        let lock = load_lock(&server).unwrap();
        assert!(lock.find(live).is_none());
        let migrated = lock.find("smudgy://wbk/keeper").expect("migrated entry");
        assert!(migrated.enabled);

        // A placeholder duplicate of an already-installed nickname form is dropped, not merged.
        install_package(&server, live, UpdateMode::Auto, false).unwrap();
        assert_eq!(
            reconcile_local_installs(&server, Some("wbk")).unwrap(),
            vec![live.to_string()]
        );
        let lock = load_lock(&server).unwrap();
        assert!(lock.find(live).is_none());
        assert!(
            lock.find("smudgy://wbk/keeper").expect("nickname entry").enabled,
            "the installed nickname entry wins over the placeholder duplicate"
        );

        if let Ok(dir) = server_dir(&server) {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn param_values_round_trip() {
        let dir = temp_dir("options");
        let mut values = PackageParamValues::new();
        values
            .entry("smudgy://wbk/mapper".into())
            .or_default()
            .insert("autosave".into(), serde_json::json!(true));
        save_param_values_in(&dir, &values).expect("save");
        assert_eq!(load_param_values_in(&dir).unwrap(), values);
        fs::remove_dir_all(&dir).ok();
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
    fn missing_required_params_tracks_only_unset_required_keys() {
        use_temp_smudgy_home();
        // A unique server under the active home, cleaned up at the end.
        let server = format!("ParamGateTest-{}", std::process::id());
        let spec = "smudgy://wbk/needsconfig";
        let declared = [
            param("name", true, false),     // required, non-secret
            param("autosave", false, false), // optional -> never gates
            param("pg.url", true, true),    // required, secret
        ];

        // Nothing set: both required params are missing (declaration order), optional excluded.
        assert_eq!(
            missing_required_params(&server, spec, &declared),
            vec!["name".to_string(), "pg.url".to_string()]
        );

        // Set the non-secret required value -> only the secret one remains missing.
        save_param_value(&server, spec, "name", serde_json::json!("Bob")).unwrap();
        assert_eq!(
            missing_required_params(&server, spec, &declared),
            vec!["pg.url".to_string()]
        );

        // Set the secret -> fully configured, gate passes.
        save_secret_param(&server, spec, "pg.url", "postgres://u:p@h/db").unwrap();
        assert!(missing_required_params(&server, spec, &declared).is_empty());

        let _ = clear_secret_param(&server, spec, "pg.url");
        if let Ok(dir) = server_dir(&server) {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn clear_param_value_unsets_and_prunes_empty_entries() {
        use_temp_smudgy_home();
        let server = format!("ParamClearTest-{}", std::process::id());
        let spec = "smudgy://wbk/needsconfig";

        save_param_value(&server, spec, "name", serde_json::json!("Bob")).unwrap();
        save_param_value(&server, spec, "autosave", serde_json::json!("on")).unwrap();
        assert!(get_param_value(&server, spec, "name").is_some());

        // Clearing one key leaves the others.
        clear_param_value(&server, spec, "name").unwrap();
        assert_eq!(get_param_value(&server, spec, "name"), None);
        assert_eq!(get_param_value(&server, spec, "autosave"), Some(serde_json::json!("on")));

        // Clearing a missing key is a no-op (idempotent).
        clear_param_value(&server, spec, "name").unwrap();

        // Removing the last key drops the package's entry entirely.
        clear_param_value(&server, spec, "autosave").unwrap();
        assert!(!load_param_values(&server).unwrap().contains_key(spec));

        if let Ok(dir) = server_dir(&server) {
            let _ = fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn secret_file_round_trips_and_is_obfuscated() {
        let dir = temp_dir("secret");
        let slot = secret_slot("arctic", "smudgy://wbk/mapper", "pg.url");
        let secret = "postgres://user:pw@host/db";

        save_secret_to_file(&dir, &slot, secret).expect("save");
        let raw = fs::read_to_string(dir.join(SECRETS_FILE)).expect("file exists");
        assert!(
            !raw.contains(secret) && !raw.contains("postgres://"),
            "secrets file must not contain the raw value"
        );
        assert_eq!(load_secret_from_file(&dir, &slot).as_deref(), Some(secret));

        remove_secret_from_file(&dir, &slot).expect("remove");
        assert_eq!(load_secret_from_file(&dir, &slot), None);
        remove_secret_from_file(&dir, &slot).expect("removing a missing slot is fine");
        fs::remove_dir_all(&dir).ok();
    }
}
