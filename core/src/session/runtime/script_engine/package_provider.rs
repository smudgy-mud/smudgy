//! The concrete [`PackageProvider`]: resolves `smudgy://` packages over the cloud package
//! API, verifies integrity, honors the per-server lockfile (auto-latest by default, opt-in
//! pin), and caches fetched module sets.
//!
//! **Per-isolate** (see `script/PACKAGE-ISOLATES-RESOLUTION.md`). One provider instance
//! serves one isolate: the engine builds a base provider for the main isolate and [`fork`]s a
//! sibling for each sandboxed package isolate. The forks share the expensive,
//! isolate-independent bits (the HTTP `client`, the content-addressed `disk_cache`, the
//! per-server `lock`) but each owns its solve state, so every isolate resolves its own closure
//! independently — within an isolate the collapse/coexist/pin rules apply, but
//! across isolates there is no collapse (main may run `util@1.4` while a sandbox runs `util@1.2`).
//!
//! Runs on the session thread under deno's event loop (driven by
//! `ModuleLoadResponse::Async` in `smudgy_script`), never under a nested `block_on` —
//! the HTTP it does is async (`PackageApiClient`).
//!
//! [`fork`]: SmudgyPackageProvider::fork

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use smudgy_cloud::{CloudError, PackageApiClient, ResolvedDependency, ResolvedModuleWire};
use smudgy_script::{
    PackageError, PackageKey, PackageManifest, PackageModuleSource, PackageParameter,
    PackagePermissions, PackageProvider, ReferrerRef, ResolvedPackage,
};

use super::package_cache::{CachedModule, CachedResolution, PackageCache};
use super::package_solver::{self, DepEdge, DepRequirement, Solve};
use crate::models::shared_packages::{self, LockedPackage, SharedPackageLock, UpdateMode};

/// Builds the **main isolate's** package provider from an optional cloud client (the engine
/// [`fork`](SmudgyPackageProvider::fork)s a sibling per sandboxed isolate). Returns `None`
/// (disabling `smudgy://` imports) when the session has no cloud client. Returns the **concrete**
/// type (not `Rc<dyn PackageProvider>`) so the engine can run the per-isolate solve + drain
/// auto-update notices after load; coerce to the trait object for the runtime via `as`.
#[must_use]
pub fn build_package_provider(
    client: Option<PackageApiClient>,
    server_name: Arc<String>,
) -> Option<Rc<SmudgyPackageProvider>> {
    client.map(|client| Rc::new(SmudgyPackageProvider::new(client, server_name)))
}

/// One auto-update notice: a package whose resolved version changed since last load.
pub type VersionChange = (String, String, String);

/// Why [`cap_version`](SmudgyPackageProvider::cap_version) refused to pick a version — the engine
/// picks the session notice from this. The three refusals need different user guidance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CapRefusal {
    /// Candidate versions exist, but none's closure permission union fits the consented grant:
    /// the user must review and grant the update.
    Permissions,
    /// At least one candidate fits the grant but its closure `min_smudgy_version` floor is
    /// above this smudgy. Carries the newest such candidate's
    /// [`SmudgyVersionFloor::refusal`](shared_packages::SmudgyVersionFloor::refusal) reason.
    NeedsNewerSmudgy(String),
    /// No candidate version could be enumerated at all. The causes are alternatives: the
    /// specifier doesn't parse; the cloud doesn't know the package (never published, deleted,
    /// or a reserved owner such as a stale `smudgy://local/…` install whose folder is gone) or
    /// can't be reached about it *and* no prior resolution is cached to fall back on; or every
    /// published version is deleted/yanked. There is nothing a grant could unlock, so unlike
    /// [`Permissions`](Self::Permissions) the fix is to remove or reinstall the package.
    NoVersions,
}

/// One resolved package instance's locked dependencies: dependency package →
/// `(locked version, is_exact_pin)`.
type LockedDeps = HashMap<PackageKey, (String, bool)>;

/// Resolves and fetches `smudgy://` packages over the cloud API for **one isolate**. Built once
/// for the main isolate, then [`fork`](Self::fork)ed per sandboxed package isolate; the forks
/// share `client` / `disk_cache` / `lock` and each owns its solve state (`PACKAGE-ISOLATES-RESOLUTION.md`).
pub struct SmudgyPackageProvider {
    client: PackageApiClient,
    server_name: Arc<String>,
    /// In-memory view of the server lockfile (modes + last-resolved versions). **Shared** across
    /// every isolate's provider (one `Rc` per session) so partitioned lockfile writes stay
    /// consistent: each install belongs to exactly one isolate (`PACKAGE-ISOLATES-RESOLUTION.md`),
    /// and a shared view means one fork's `record_resolution` can't clobber another's via a
    /// stale snapshot.
    lock: Rc<RefCell<SharedPackageLock>>,
    /// Auto packages whose resolved version changed vs the lockfile this load —
    /// `(specifier, from_version, to_version)` — drained by the engine for a session line.
    version_changes: RefCell<Vec<VersionChange>>,
    /// Fetched module sets, keyed by `(package, resolved version)`.
    cache: RefCell<HashMap<(PackageKey, String), Rc<ResolvedPackage>>>,
    /// The version each package resolved to in this isolate (so repeated imports within it agree).
    resolved_versions: RefCell<HashMap<PackageKey, String>>,
    /// Each resolved package *instance*'s locked deps: importing `(package, version)` →
    /// (dependency package → `(locked version, is_exact_pin)`). Keyed by version too so two
    /// coexisting versions of one importer keep distinct dep maps; the pin flag (author
    /// `@=x`) marks deps exempt from the closure's upgrade-collapse. Populated from each
    /// resolve's wire `dependencies`; drives referrer-aware selection.
    locked_deps: RefCell<HashMap<(PackageKey, String), LockedDeps>>,
    /// This isolate's cross-tree coexistence solve, computed by the `solve_closure`
    /// pre-pass over *this isolate's* closure before module loading. `None` until the pre-pass
    /// runs (or when it's skipped). Per-isolate: no collapse across isolates (`fork`).
    solve: RefCell<Option<Solve>>,
    /// The solved version each top-level install resolves to (auto installs collapse to the
    /// highest compatible locked version; user-pins keep their exact version). Lets a
    /// top-level / user import adopt the closure solve too, not just transitive edges.
    top_level_solved: RefCell<HashMap<PackageKey, String>>,
    /// Packages this isolate's solve found at ≥2 distinct versions this load — drained by the
    /// engine for a duplicate-version warning (session line + inspect note). Per-isolate, so the
    /// warning means an **intra-isolate** collision; a cross-isolate duplicate never appears here
    /// because it lives in two different providers' closures (`PACKAGE-ISOLATES-RESOLUTION.md`).
    duplicate_warnings: RefCell<Vec<(PackageKey, Vec<String>)>>,
    /// Each top-level install's declared params `(specifier, params)`, collected during the
    /// `solve_closure` pre-pass so the host can run the required-param load-gate before
    /// evaluation. Published installs only; a local dev package is the author's own.
    installed_params: RefCell<Vec<(String, Vec<PackageParameter>)>>,
    /// Each top-level install's declared `min_smudgy_version` `(specifier, raw floor)`,
    /// collected during the `solve_closure` pre-pass so the host can run the version-floor
    /// load-gate per package (refuse + notice) instead of letting the resolution-time gate
    /// fail the whole isolate load. Roots that declare no floor are absent.
    installed_min_versions: RefCell<Vec<(String, String)>>,
    /// The deno-native permission union over this isolate's closure, folded during
    /// `solve_closure` from every closure package's manifest `permissions`
    /// (`PACKAGE-ISOLATES-ENFORCEMENT.md`). The engine reads it via
    /// [`closure_permissions`](PackageProvider::closure_permissions) to build the sandboxed
    /// isolate's restricted container. Per-isolate, like the rest of the solve state.
    closure_permission_union: RefCell<PackagePermissions>,
    /// Persistent, content-addressed on-disk cache (immutable versions cached forever;
    /// enables offline + skips re-downloading bodies). `None` if it couldn't be opened.
    disk_cache: Option<PackageCache>,
    /// The current account's nickname, for the local-package dev-override
    /// (resolve `smudgy://<yourhandle>/<name>` to a local folder). `None` when
    /// logged out / no handle allocated.
    account_nickname: Option<String>,
    /// The packages whose interop home is this provider's isolate (interop.md §3), folded
    /// like the home registry's keys. `None` until the engine calls
    /// [`set_home_packages`](PackageProvider::set_home_packages): every load is home, no
    /// scrub. Per-isolate (a fork starts unset).
    home_packages: RefCell<Option<std::collections::HashSet<PackageKey>>>,
    /// Non-home loads that had interop-handle exports scrubbed — read by the engine to
    /// dress a subsequent link failure with the scheme-import hint.
    scrubbed: RefCell<Vec<PackageKey>>,
    /// User-level (`file://`-referred) code imports of packages — read by the engine to
    /// warn when the target declares interop handles (interop.md §1/§3 residual).
    user_imports: RefCell<Vec<PackageKey>>,
}

impl SmudgyPackageProvider {
    /// Creates a provider, seeding the lockfile view from disk.
    #[must_use]
    pub fn new(client: PackageApiClient, server_name: Arc<String>) -> Self {
        let lock = shared_packages::load_lock(&server_name).unwrap_or_else(|err| {
            warn!("Failed to load package lockfile for {server_name}: {err:#}");
            SharedPackageLock::default()
        });
        Self {
            client,
            server_name,
            lock: Rc::new(RefCell::new(lock)),
            version_changes: RefCell::new(Vec::new()),
            cache: RefCell::new(HashMap::new()),
            resolved_versions: RefCell::new(HashMap::new()),
            locked_deps: RefCell::new(HashMap::new()),
            solve: RefCell::new(None),
            top_level_solved: RefCell::new(HashMap::new()),
            duplicate_warnings: RefCell::new(Vec::new()),
            installed_params: RefCell::new(Vec::new()),
            installed_min_versions: RefCell::new(Vec::new()),
            closure_permission_union: RefCell::new(PackagePermissions::default()),
            disk_cache: PackageCache::new().ok(),
            account_nickname: crate::models::auth::load_account().and_then(|a| a.nickname),
            home_packages: RefCell::new(None),
            scrubbed: RefCell::new(Vec::new()),
            user_imports: RefCell::new(Vec::new()),
        }
    }

    /// Build a sibling provider for another isolate (`PACKAGE-ISOLATES-RESOLUTION.md`). Each
    /// isolate resolves its closure independently, so the fork starts with
    /// its **own** empty solve state; the expensive, isolate-independent bits are shared by cheap
    /// clone / `Rc`:
    ///
    /// - `client` — clones share the connection pool; fetching bytes is isolate-independent.
    /// - `disk_cache` — content-addressed immutable blobs; a hit in one isolate serves all.
    /// - `lock` — one in-memory view per session (`Rc`), so partitioned lockfile writes across
    ///   isolates stay consistent.
    ///
    /// Within the fork's isolate the collapse/coexist/pin rules apply; across isolates
    /// there is no collapse (main may land `util@1.4` while a sandboxed isolate lands `util@1.2` —
    /// different heaps, nothing to collapse). Each instance runs the same solver over its own closure.
    #[must_use]
    pub fn fork(&self) -> Self {
        Self {
            client: self.client.clone(),
            server_name: Arc::clone(&self.server_name),
            lock: Rc::clone(&self.lock),
            disk_cache: self.disk_cache.clone(),
            account_nickname: self.account_nickname.clone(),
            // Per-isolate solve state — each starts empty and solves its own closure.
            version_changes: RefCell::new(Vec::new()),
            cache: RefCell::new(HashMap::new()),
            resolved_versions: RefCell::new(HashMap::new()),
            locked_deps: RefCell::new(HashMap::new()),
            solve: RefCell::new(None),
            top_level_solved: RefCell::new(HashMap::new()),
            duplicate_warnings: RefCell::new(Vec::new()),
            installed_params: RefCell::new(Vec::new()),
            installed_min_versions: RefCell::new(Vec::new()),
            closure_permission_union: RefCell::new(PackagePermissions::default()),
            // Interop-home + diagnostic tracking is per-isolate too: the engine configures
            // each fork for the isolate it serves.
            home_packages: RefCell::new(None),
            scrubbed: RefCell::new(Vec::new()),
            user_imports: RefCell::new(Vec::new()),
        }
    }

    /// If the current account is authoring a local package matching `key`, resolve it
    /// from `<server>/packages/<name>/` (npm-link-style override) so it's tested under
    /// its real specifier before publishing. On a code load (`track`) it's cached like a
    /// normal resolution so install + import share one instance; a stub fetch
    /// (`track == false`) leaves the cache untouched, so consuming a local producer over
    /// `smudgy:state|events/…` records no code-load footprint (`loaded_packages()` /
    /// the stumble diagnostic stays quiet) — the same contract the network + offline
    /// branches keep.
    ///
    /// Known gap: a local package's `locked_deps` are **not** populated here — its
    /// manifest carries dependency *ranges*, not the concrete versions a published resolve
    /// supplies, so deriving them would need the same async range-resolution `publish`
    /// does. So a locally-developed package's transitive `smudgy://` imports resolve via
    /// the lockfile / latest rather than referrer-locked versions until it is published.
    /// The `resolved_versions` write is left to the caller so it can gate on top-level.
    fn try_local_override(&self, key: &PackageKey, track: bool) -> Option<Rc<ResolvedPackage>> {
        if !self.is_local_owner_segment(&key.owner) {
            return None;
        }
        let local =
            crate::models::local_packages::load_local_package(&self.server_name, &key.name)
                .ok()
                .flatten()?;
        let version = local.manifest.version.clone();
        let modules = local
            .modules
            .into_iter()
            // Local modules are loaded as text; a binary local module (any bytes are stored, but
            // loading binaries is out of scope) is SKIPPED rather than fed lossy garbage to v8.
            .filter_map(|m| {
                String::from_utf8(m.content)
                    .ok()
                    .map(|text| PackageModuleSource { subpath: m.subpath, text })
            })
            .collect();
        let resolved = Rc::new(ResolvedPackage {
            key: key.clone(),
            resolved_version: version.clone(),
            manifest: local.manifest,
            integrity: "local".to_string(),
            modules,
        });
        // Only a code load records the served set: a stub fetch (`track == false`) of a
        // local producer must leave no code-load footprint, exactly as the network +
        // offline branches gate their inserts on `track`. Otherwise consuming a
        // locally-authored producer over `smudgy:events/…` would land it in this isolate's
        // `cache`, and the code-import stumble diagnostic would misfire on a consumer that
        // never imported the producer's code.
        if track {
            self.cache
                .borrow_mut()
                .insert((key.clone(), version.clone()), resolved.clone());
        }
        Some(resolved)
    }

    /// Whether `key` resolves to a **local dev-override** — the account (or, signed out, the
    /// reserved [`LOCAL_OWNER`](crate::models::local_packages::LOCAL_OWNER) placeholder)
    /// authoring its own package under `<server>/packages/<name>/` (the [`try_local_override`]
    /// shadow path). The engine reads this to (a) skip version-capping — the local folder is the
    /// version on disk — and (b) source the isolate's enforced grant from the package's OWN
    /// on-disk manifest rather than a consented closure union (a local package has no consent
    /// record; the manifest IS its grant table). A local package therefore still runs
    /// **sandboxed to its manifest**, NOT allow-all — `PACKAGE-ISOLATES-ENFORCEMENT.md`. Allow-all
    /// is opt-in only, via the separate **trust** escape hatch, which promotes the package to the
    /// main isolate and never reaches this path.
    ///
    /// [`try_local_override`]: Self::try_local_override
    #[must_use]
    pub fn is_local_override(&self, key: &PackageKey) -> bool {
        self.is_local_owner_segment(&key.owner)
            && crate::models::local_packages::load_local_package(&self.server_name, &key.name)
                .ok()
                .flatten()
                .is_some()
    }

    /// The owner segment local packages run under: the account nickname when signed in, else the
    /// reserved [`LOCAL_OWNER`](crate::models::local_packages::LOCAL_OWNER) placeholder so local
    /// packages still resolve, enable, and run while signed out (matching the UI's
    /// `local_own_spec`).
    fn local_owner(&self) -> &str {
        self.account_nickname
            .as_deref()
            .unwrap_or(crate::models::local_packages::LOCAL_OWNER)
    }

    /// Whether `owner` addresses this account's own local packages: the current
    /// [`local_owner`](Self::local_owner), or the reserved `local` placeholder regardless of
    /// sign-in state. An install written signed out (`smudgy://local/<name>`) must keep
    /// resolving to its folder after the account gains a nickname — the owner segment records
    /// the sign-in state at install time, not a different package. The placeholder is reserved
    /// server-side, so accepting it never shadows a real cloud package.
    fn is_local_owner_segment(&self, owner: &str) -> bool {
        owner == crate::models::local_packages::LOCAL_OWNER || owner == self.local_owner()
    }

    /// Build a resolved package entirely from the on-disk cache (for offline use). `None`
    /// unless the version's metadata and *every* module body are cached.
    fn build_from_cache(&self, key: &PackageKey, version: &str) -> Option<Rc<ResolvedPackage>> {
        let cache = self.disk_cache.as_ref()?;
        let meta = cache.read_meta(key, version)?;
        if !cache.has_all_blobs(&meta) {
            return None;
        }
        // Repopulate this instance's locked deps so its transitive imports stay
        // referrer-aware offline, matching the network path. Empty for cache
        // entries written before the field existed -> graceful referrer-blind fallback.
        self.store_locked_deps(key, &meta.version, &meta.dependencies);
        let mut modules = Vec::with_capacity(meta.modules.len());
        for module in &meta.modules {
            modules.push(PackageModuleSource {
                subpath: module.subpath.clone(),
                text: cache.read_blob(&module.content_hash)?,
            });
        }
        Some(Rc::new(ResolvedPackage {
            key: key.clone(),
            resolved_version: meta.version.clone(),
            manifest: meta.manifest.clone(),
            integrity: meta.integrity.clone(),
            modules,
        }))
    }

    /// Persists a package's resolved version + integrity to the lockfile (for offline
    /// reuse and reproducibility). Best-effort: a write failure is logged, not fatal.
    ///
    /// Persistence is an entry-level read-modify-write against the **on-disk** lock — never a
    /// flush of this session's whole in-memory view, which can be seconds stale by the time a
    /// resolve completes and would silently clobber concurrent Automations-window writes (an
    /// uninstalled entry would resurrect; a fresh enable/disable would revert). An entry that
    /// was installed when this session loaded but is gone from disk was uninstalled meanwhile:
    /// its resolution metadata dies with it.
    fn record_resolution(&self, specifier: &str, version: &str, integrity: &str) {
        let fresh_entry = || LockedPackage {
            specifier: specifier.to_string(),
            mode: UpdateMode::Auto,
            last_resolved_version: Some(version.to_string()),
            integrity: Some(integrity.to_string()),
            trusted: false,
            consented_permissions: None,
            enabled: true,
            installed_as_requirement: false,
        };
        let known_install = {
            let mut lock = self.lock.borrow_mut();
            if let Some(entry) = lock.packages.iter_mut().find(|p| p.specifier == specifier) {
                // An AUTO package that resolved to a new version since last load: record a
                // notice (a pin, or a first-ever resolve with no prior, never notifies).
                if matches!(entry.mode, UpdateMode::Auto)
                    && let Some(prior) = &entry.last_resolved_version
                        && prior != version {
                            self.version_changes.borrow_mut().push((
                                specifier.to_string(),
                                prior.clone(),
                                version.to_string(),
                            ));
                        }
                entry.last_resolved_version = Some(version.to_string());
                entry.integrity = Some(integrity.to_string());
                true
            } else {
                lock.upsert(fresh_entry());
                false
            }
        };
        let persisted = shared_packages::load_lock(&self.server_name).and_then(|mut disk| {
            if let Some(entry) = disk.packages.iter_mut().find(|p| p.specifier == specifier) {
                entry.last_resolved_version = Some(version.to_string());
                entry.integrity = Some(integrity.to_string());
                shared_packages::save_lock(&self.server_name, &disk)
            } else if known_install {
                // Uninstalled since this session loaded — don't resurrect it just to stamp
                // resolution metadata on it.
                Ok(())
            } else {
                // Not an install at all (a top-level resolve outside the lockfile): record it
                // on disk the same way it was recorded in memory.
                disk.upsert(fresh_entry());
                shared_packages::save_lock(&self.server_name, &disk)
            }
        });
        if let Err(err) = persisted {
            warn!("Failed to persist package lock for {specifier}: {err:#}");
        }
    }

    /// Record a resolved package instance's locked deps (keyed by `(package, version)`), so
    /// a later import made from inside *that version* resolves at the version it locked.
    /// Each dep's declared range is classified into an exact-pin flag (author `@=x`).
    fn store_locked_deps(&self, importer: &PackageKey, version: &str, deps: &[ResolvedDependency]) {
        if deps.is_empty() {
            return;
        }
        let map: LockedDeps = deps
            .iter()
            .filter_map(|dep| {
                let key = dep_package_key(&dep.owner_nickname, &dep.name)?;
                Some((key, (dep.resolved_version.clone(), package_solver::is_exact_pin(&dep.range))))
            })
            .collect();
        if !map.is_empty() {
            self.locked_deps
                .borrow_mut()
                .insert((importer.clone(), version.to_string()), map);
        }
    }

    /// The `(locked version, is_exact_pin)` the `referrer` instance recorded for the
    /// dependency `target`, if any.
    fn referrer_locked_version(
        &self,
        referrer: &ReferrerRef,
        target: &PackageKey,
    ) -> Option<(String, bool)> {
        self.locked_deps
            .borrow()
            .get(&(referrer.key.clone(), referrer.version.clone()))?
            .get(target)
            .cloned()
    }

    /// Apply the cross-tree solve to a referrer edge's locked version: a non-pin
    /// dep collapses to the highest compatible version any dependent locked; a pin keeps
    /// its exact version. With no solve (pre-pass skipped), the locked version is returned.
    fn solve_resolve(&self, target: &PackageKey, version: &str, is_pin: bool) -> String {
        self.solve
            .borrow()
            .as_ref()
            .map_or_else(|| version.to_string(), |solve| solve.resolve(target, version, is_pin))
    }

    /// Pre-pass: walk the install closure to gather every requirement on each
    /// shared package, solve the cross-tree collapse/coexistence, and stash the result so
    /// both the referrer-aware `resolve_package` and the top-level installs read solved
    /// versions. Records the duplicate-version warning set over the *actually-loaded*
    /// closure. Best-effort: a package that can't be resolved (offline / missing) is
    /// skipped, degrading that subtree to per-edge selection.
    ///
    /// Single-pass over the locked closure: the collapsed version is always one of the
    /// locked versions, so it is discovered and later cached by the lazy load.
    pub async fn solve_closure(&self, installs: &[String]) {
        self.solve_closure_inner(installs, &HashMap::new()).await;
    }

    /// Like [`solve_closure`](Self::solve_closure), but each listed install resolves its **root** at
    /// the given capped version — the highest version whose closure permission union fits the user's
    /// consented grant (`script/PACKAGE-ISOLATES-CONSENT-TRUST.md`) — instead of latest. Used for
    /// sandboxed isolates so a newer version that demands more access than was granted is never
    /// loaded (the package stays at the capped version; if nothing fits the engine doesn't load it
    /// at all). Trusted packages run allow-all and are never capped, so the main isolate keeps using
    /// the plain `solve_closure`.
    pub async fn solve_closure_capped(&self, installs: &[(String, String)]) {
        let forced: HashMap<String, String> = installs.iter().cloned().collect();
        let specifiers: Vec<String> = installs.iter().map(|(spec, _)| spec.clone()).collect();
        self.solve_closure_inner(&specifiers, &forced).await;
    }

    async fn solve_closure_inner(
        &self,
        installs: &[String],
        forced_root_versions: &HashMap<String, String>,
    ) {
        let mut requirements: Vec<DepRequirement> = Vec::new();
        let mut roots: Vec<DepRequirement> = Vec::new();
        let mut edges: Vec<DepEdge> = Vec::new();
        let mut installed_params: Vec<(String, Vec<PackageParameter>)> = Vec::new();
        let mut installed_min_versions: Vec<(String, String)> = Vec::new();
        // The deno-native permission union over the whole closure — folded from each
        // distinct closure package's manifest, read by the engine to sandbox this isolate.
        let mut permissions_union = PackagePermissions::default();
        let mut seen: HashSet<(PackageKey, String)> = HashSet::new();
        // (package, forced version | None = latest/user-pin, is_pin, is_top_level).
        let mut stack: Vec<(PackageKey, Option<String>, bool, bool)> = Vec::new();
        for specifier in installs {
            let Ok(spec) = smudgy_script::SmudgySpecifier::parse(specifier) else {
                continue;
            };
            // A permission-capped root resolves at exactly its capped version (treated as a pin for
            // this load, exempt from collapse). Otherwise honor a user install-pin, else latest.
            let (forced, is_pin) = if let Some(version) = forced_root_versions.get(specifier) {
                (Some(version.clone()), true)
            } else {
                let pin = self
                    .lock
                    .borrow()
                    .find(specifier)
                    .and_then(|locked| locked.pinned_version().map(str::to_string));
                let is_pin = pin.is_some();
                (pin, is_pin)
            };
            stack.push((spec.package_key(), forced, is_pin, true));
        }

        while let Some((key, forced, is_pin, is_top_level)) = stack.pop() {
            let wire = match self
                .client
                .resolve_package(&key.owner, &key.name, forced.as_deref())
                .await
            {
                Ok(wire) => wire,
                Err(_) => continue,
            };
            let version = wire.version.clone();
            let requirement = DepRequirement {
                package: key.clone(),
                version: version.clone(),
                is_pin,
            };
            requirements.push(requirement.clone());
            // Parse the manifest once: the top-level params gate and the closure permission
            // union both read it (a parse failure degrades both, never aborts the walk).
            let manifest = PackageManifest::parse(&wire.manifest.to_string()).ok();
            if is_top_level {
                roots.push(requirement);
                if let Some(manifest) = &manifest {
                    // Collect the install's declared params for the required-param load-gate.
                    if !manifest.params.is_empty() {
                        installed_params.push((key.to_user_specifier(), manifest.params.clone()));
                    }
                    // And its declared version floor for the version-floor load-gate.
                    if let Some(min) = &manifest.min_smudgy_version {
                        installed_min_versions.push((key.to_user_specifier(), min.clone()));
                    }
                }
            }
            // Walk each distinct (package, version) once, but count every requirement.
            if !seen.insert((key.clone(), version.clone())) {
                continue;
            }
            // Fold this closure package's declared permissions into the isolate union:
            // every distinct closure package contributes (root and transitive deps alike).
            if let Some(manifest) = &manifest {
                permissions_union.merge(&manifest.permissions);
            }
            for dep in &wire.dependencies {
                let Some(dep_key) = dep_package_key(&dep.owner_nickname, &dep.name) else {
                    continue;
                };
                let dep_pin = package_solver::is_exact_pin(&dep.range);
                requirements.push(DepRequirement {
                    package: dep_key.clone(),
                    version: dep.resolved_version.clone(),
                    is_pin: dep_pin,
                });
                edges.push(DepEdge {
                    importer: key.clone(),
                    importer_version: version.clone(),
                    dep: dep_key.clone(),
                    dep_version: dep.resolved_version.clone(),
                    dep_is_pin: dep_pin,
                });
                stack.push((dep_key, Some(dep.resolved_version.clone()), dep_pin, false));
            }
        }

        let solve = package_solver::solve(&requirements);
        // Each top-level install loads at its solved version (auto -> collapsed-highest;
        // user-pin -> exact), so it joins the same instance as transitive edges in its
        // class instead of floating to non-yanked latest.
        let top_level_solved = roots
            .iter()
            .map(|root| (root.package.clone(), solve.resolve(&root.package, &root.version, root.is_pin)))
            .collect();
        // Warn over the ACTUALLY-loaded closure (BFS from solved roots), so deps of a
        // collapsed-away version don't produce a phantom coexistence warning.
        *self.duplicate_warnings.borrow_mut() = solve.loaded_duplicates(&roots, &edges);
        *self.top_level_solved.borrow_mut() = top_level_solved;
        *self.installed_params.borrow_mut() = installed_params;
        *self.installed_min_versions.borrow_mut() = installed_min_versions;
        *self.closure_permission_union.borrow_mut() = permissions_union;
        *self.solve.borrow_mut() = Some(solve);
    }

    /// Permission- and version-floor-aware version selection
    /// (`script/PACKAGE-ISOLATES-CONSENT-TRUST.md`): the highest version of `specifier` whose
    /// **closure** permission union fits the user's `consented` grant *and* whose closure
    /// `min_smudgy_version` floor this smudgy satisfies. `Err` is a [`CapRefusal`] saying why the
    /// package must not load — [`CapRefusal::Permissions`] when candidate versions exist but every
    /// one demands more access than was granted, [`CapRefusal::NeedsNewerSmudgy`] when one fits the
    /// grant but needs a newer smudgy, or [`CapRefusal::NoVersions`] when no candidate version
    /// could be enumerated at all (a grant can't fix that). The caller feeds the chosen version to
    /// [`solve_closure_capped`](Self::solve_closure_capped).
    ///
    /// - A user **install-pin** is exact: the only candidate is the pinned version (it loads iff its
    ///   closure fits, else refused).
    /// - Otherwise (auto): walk the package's published, non-deleted, non-yanked versions newest-first
    ///   and return the first whose closure union `is_within` consent and whose closure floor is
    ///   satisfied. Walking by semver-descending order means the package auto-upgrades as far as
    ///   the grant and this smudgy allow, and otherwise stays at the highest fitting (typically
    ///   the previously-consented / previously-loadable) version.
    ///
    /// Each candidate's closure union + floor is computed by
    /// [`closure_union_for`](Self::closure_union_for); the common case (latest already fits)
    /// costs a single check.
    pub async fn cap_version(
        &self,
        specifier: &str,
        consented: &PackagePermissions,
    ) -> Result<String, CapRefusal> {
        let Ok(spec) = smudgy_script::SmudgySpecifier::parse(specifier) else {
            return Err(CapRefusal::NoVersions);
        };
        let key = spec.package_key();

        let pin = self
            .lock
            .borrow()
            .find(specifier)
            .and_then(|locked| locked.pinned_version().map(str::to_string));
        let candidates: Vec<String> = if let Some(pin) = pin {
            vec![pin]
        } else {
            // Resolve once to learn the package id (and a latest-version fallback), then list its
            // versions newest-first. If listing fails, fall back to just the latest.
            match self
                .client
                .resolve_package(&key.owner, &key.name, None)
                .await
            {
                Ok(latest) => match self.client.list_versions(latest.package_id).await {
                    Ok(list) => {
                        let mut versions: Vec<semver::Version> = list
                            .into_iter()
                            // Skip hard-deleted (content gone, would 404) and yanked numbers, matching
                            // normal auto-resolution — a yanked version drops out of latest/auto and is
                            // only reachable by an exact pin (which takes the `pin` branch above).
                            .filter(|v| !v.deleted && !v.yanked)
                            .filter_map(|v| semver::Version::parse(&v.version).ok())
                            .collect();
                        versions.sort();
                        versions.reverse();
                        versions.into_iter().map(|v| v.to_string()).collect()
                    }
                    Err(_) => vec![latest.version],
                },
                // Offline, or signed out and the package isn't public (the anonymous viewer
                // can't see it): we can't shop for a newer version, so fall back to the last
                // version we resolved. It's cached and already consented, so an installed
                // auto-update package keeps running without the cloud instead of silently
                // dropping out. (Its closure union is recomputed below; when the cloud is
                // unreachable that resolves to the empty set, which fits the prior consent,
                // so the cached version loads with exactly the permissions already granted.)
                Err(_) => self
                    .lock
                    .borrow()
                    .find(specifier)
                    .and_then(|locked| locked.last_resolved_version.clone())
                    .into_iter()
                    .collect(),
            }
        };

        // Nothing to consider at all — the install target no longer exists anywhere (and no
        // grant or smudgy update could change that), distinct from candidates that exist but
        // don't fit.
        if candidates.is_empty() {
            return Err(CapRefusal::NoVersions);
        }
        // The newest consent-fitting candidate refused only by its version floor, if any —
        // the actionable refusal ("update smudgy") when nothing loads.
        let running = shared_packages::running_smudgy_release();
        let mut floor_refusal: Option<String> = None;
        for candidate in candidates {
            let (union, floor) = self.closure_union_for(&key, &candidate).await;
            if !union.is_within(consented) {
                continue;
            }
            if let Some(reason) = floor.refusal(&running) {
                if floor_refusal.is_none() {
                    floor_refusal = Some(reason);
                }
                continue;
            }
            return Ok(candidate);
        }
        Err(floor_refusal.map_or(CapRefusal::Permissions, CapRefusal::NeedsNewerSmudgy))
    }

    /// The deno-native permission union and `min_smudgy_version` floor over the closure rooted
    /// at `root_key@root_version` — the same fold [`solve_closure`](Self::solve_closure) does,
    /// but for a *specific* root version and without mutating solve state, so
    /// [`cap_version`](Self::cap_version) can evaluate candidate versions. Best-effort (a dep
    /// that won't resolve is skipped) and dedups by `(package, version)` so diamonds/cycles
    /// terminate. Each dep is resolved at its locked `resolved_version`. A manifest that won't
    /// parse contributes to neither fold; the resolution-time `InvalidManifest` refusal covers
    /// that package if it is actually loaded.
    async fn closure_union_for(
        &self,
        root_key: &PackageKey,
        root_version: &str,
    ) -> (PackagePermissions, shared_packages::SmudgyVersionFloor) {
        let mut union = PackagePermissions::default();
        let mut floor = shared_packages::SmudgyVersionFloor::default();
        let mut seen: HashSet<(PackageKey, String)> = HashSet::new();
        let mut stack: Vec<(PackageKey, String)> =
            vec![(root_key.clone(), root_version.to_string())];
        while let Some((key, version)) = stack.pop() {
            if !seen.insert((key.clone(), version.clone())) {
                continue;
            }
            let Ok(wire) = self
                .client
                .resolve_package(&key.owner, &key.name, Some(&version))
                .await
            else {
                continue;
            };
            if let Ok(manifest) = PackageManifest::parse(&wire.manifest.to_string()) {
                union.merge(&manifest.permissions);
                floor.fold(&key.name, manifest.min_smudgy_version.as_deref());
            }
            for dep in &wire.dependencies {
                if let Some(dep_key) = dep_package_key(&dep.owner_nickname, &dep.name) {
                    stack.push((dep_key, dep.resolved_version.clone()));
                }
            }
        }
        (union, floor)
    }

    /// Each top-level install's `(specifier, declared params)`, collected by the last
    /// `solve_closure` — the required-param load-gate's input.
    #[must_use]
    pub fn installed_params(&self) -> Vec<(String, Vec<PackageParameter>)> {
        self.installed_params.borrow().clone()
    }

    /// Each top-level install's `(specifier, declared min_smudgy_version)`, collected by the
    /// last `solve_closure` — the version-floor load-gate's input. Roots with no floor are
    /// absent.
    #[must_use]
    pub fn installed_min_versions(&self) -> Vec<(String, String)> {
        self.installed_min_versions.borrow().clone()
    }

    /// Drain the auto-update notices collected this load (the engine surfaces them as a
    /// session line — auto-update is silent except for this nudge).
    pub fn take_version_changes(&self) -> Vec<VersionChange> {
        self.version_changes.borrow_mut().drain(..).collect()
    }

    /// Drain the duplicate-version warnings the solve found this load (a package resolved
    /// to ≥2 coexisting versions — the shared-isolate side-effect-collision risk).
    pub fn take_duplicate_warnings(&self) -> Vec<(PackageKey, Vec<String>)> {
        self.duplicate_warnings.borrow_mut().drain(..).collect()
    }
}

/// Why `manifest`'s own `min_smudgy_version` floor refuses to run on this smudgy, if it
/// does — the single-manifest form of the closure fold in `closure_union_for`, used where a
/// package is gated one manifest at a time (each closure member passes through
/// `resolve_package` itself, so per-manifest checks still cover the whole closure).
fn manifest_floor_refusal(name: &str, manifest: &PackageManifest) -> Option<String> {
    let mut floor = shared_packages::SmudgyVersionFloor::default();
    floor.fold(name, manifest.min_smudgy_version.as_deref());
    floor.refusal(&shared_packages::running_smudgy_release())
}

/// Build a [`PackageKey`] from a resolve dependency's owner nickname + name.
fn dep_package_key(owner_nickname: &str, name: &str) -> Option<PackageKey> {
    if owner_nickname.is_empty() {
        return None;
    }
    Some(PackageKey {
        owner: owner_nickname.to_string(),
        name: name.to_string(),
    })
}

impl SmudgyPackageProvider {
    // Genuinely multi-path: in-session dedup, local dev-override, network resolve with
    // offline fallback, content-addressed body cache, and metadata persistence.
    //
    // `track` separates a code load from a kind-scheme stub fetch: a code load records the
    // instance in `cache` (whose keys are `loaded_packages()`, the stumble diagnostic's
    // input) and, top-level, reports the resolution into the lockfile; a stub fetch
    // (`track == false`) must leave no code-load or install footprint — notably,
    // `record_resolution` would UPSERT a lock entry for an unknown package, silently
    // installing a producer someone merely consumed.
    #[allow(clippy::too_many_lines)]
    async fn resolve_impl(
        &self,
        key: &PackageKey,
        referrer: Option<&ReferrerRef>,
        track: bool,
    ) -> Result<Rc<ResolvedPackage>, PackageError> {
        let specifier = key.to_user_specifier();

        // Mode + offline fallback version from the lockfile (don't hold the borrow over
        // the await below).
        let (pinned, last_known) = {
            let lock = self.lock.borrow();
            let entry = lock.find(&specifier);
            (
                entry.and_then(|p| p.pinned_version().map(str::to_string)),
                entry.and_then(|p| p.last_resolved_version.clone()),
            )
        };

        // Version selection, refined by the closure solve:
        //  - a referrer (transitive) edge takes the version *this importer* locked,
        //    collapsed to the highest compatible version any dependent locked (a pin keeps
        //    its exact version);
        //  - a top-level / user import takes its install's solved version (auto installs
        //    also collapse to the class's highest lock — not just non-yanked latest).
        // Either falls back to the lockfile pin, then latest, when the solve has no entry.
        let solved = match referrer {
            Some(r) => self
                .referrer_locked_version(r, key)
                .map(|(version, is_pin)| self.solve_resolve(key, &version, is_pin)),
            None => self.top_level_solved.borrow().get(key).cloned(),
        };
        let selected = solved.or_else(|| pinned.clone());

        // Already resolved this version this session → reuse that instance. Keyed by the
        // *selected* version, so two importers that locked different versions coexist (two
        // canonical URLs) while identical selections share one instance. With no explicit
        // selection (auto-latest), fall back to the prior session resolve for this key.
        let dedup_version = selected
            .clone()
            .or_else(|| self.resolved_versions.borrow().get(key).cloned());
        if let Some(version) = dedup_version
            && let Some(package) = self.cache.borrow().get(&(key.clone(), version)).cloned()
        {
            return Ok(package);
        }

        // Local dev-override: a package you're authoring under <server>/packages/<name>/
        // shadows the published one, so you test it under its real specifier first.
        if let Some(local) = self.try_local_override(key, track) {
            if track && referrer.is_none() {
                self.resolved_versions
                    .borrow_mut()
                    .insert(key.clone(), local.resolved_version.clone());
            }
            return Ok(local);
        }

        let wire = match self
            .client
            .resolve_package(&key.owner, &key.name, selected.as_deref())
            .await
        {
            Ok(wire) => wire,
            Err(err) => {
                // Offline: serve from the in-memory session cache, then the persistent
                // disk cache (works for pinned + auto, the latter via last-resolved).
                if let Some(version) = selected.clone().or(last_known) {
                    if let Some(package) =
                        self.cache.borrow().get(&(key.clone(), version.clone())).cloned()
                    {
                        return Ok(package);
                    }
                    if let Some(package) = self.build_from_cache(key, &version) {
                        // The disk cache was written by a resolve that passed the version-floor
                        // gate — but under a possibly NEWER smudgy since downgraded, so re-check
                        // the cached manifest's floor before serving it.
                        if let Some(reason) = manifest_floor_refusal(&key.name, &package.manifest)
                        {
                            return Err(PackageError::Other(format!(
                                "{specifier} not loaded: {reason}"
                            )));
                        }
                        if track {
                            self.cache
                                .borrow_mut()
                                .insert((key.clone(), version.clone()), package.clone());
                            // Only a top-level (referrer-less) edge owns the reported version.
                            if referrer.is_none() {
                                self.resolved_versions
                                    .borrow_mut()
                                    .insert(key.clone(), version);
                            }
                        }
                        return Ok(package);
                    }
                }
                return Err(PackageError::Network(format!("resolving {specifier}: {err}")));
            }
        };

        let version = wire.version.clone();
        // Record this instance's locked deps so imports IT makes resolve referrer-aware.
        self.store_locked_deps(key, &version, &wire.dependencies);
        if let Some(package) = self.cache.borrow().get(&(key.clone(), version.clone())).cloned() {
            if track && referrer.is_none() {
                self.resolved_versions
                    .borrow_mut()
                    .insert(key.clone(), version);
            }
            return Ok(package);
        }

        let manifest = PackageManifest::parse(&wire.manifest.to_string())
            .map_err(|err| PackageError::InvalidManifest(format!("{specifier}: {err}")))?;

        // Version-floor load-gate, at RESOLUTION time like the required-params gate below, so a
        // too-new package pulled in transitively (its dep edges carry locked versions the
        // pre-pass gates don't walk) is refused with a clear reason instead of evaluating
        // against script APIs this smudgy doesn't have.
        if let Some(reason) = manifest_floor_refusal(&key.name, &manifest) {
            return Err(PackageError::Other(format!("{specifier} not loaded: {reason}")));
        }

        // Required-param load-gate, at RESOLUTION time so it also catches a package
        // pulled in transitively (the top-level gate only prunes the install entry; a
        // blocked package that's also a dependency would otherwise evaluate misconfigured).
        // A package with unmet required params must not evaluate; failing here surfaces a
        // clear load error (and fails any dependent that needs it).
        let missing =
            crate::models::shared_packages::missing_required_params(&self.server_name, &specifier, &manifest.params);
        if !missing.is_empty() {
            return Err(PackageError::Other(format!(
                "{specifier} not loaded: required param(s) {} are unset; configure them in settings",
                missing.join(", ")
            )));
        }

        let mut modules = Vec::with_capacity(wire.modules.len());
        for module in &wire.modules {
            // Content-addressed: a cached body for this hash never changes, so reuse it
            // and only download misses (then cache them).
            let cached = self
                .disk_cache
                .as_ref()
                .and_then(|cache| cache.read_blob(&module.content_hash));
            let text = if let Some(text) = cached {
                text
            } else {
                let text = self
                    .client
                    .fetch_module_body(&module.content_url, &module.content_hash)
                    .await
                    .map_err(|err| fetch_error(&specifier, module, &err))?;
                if let Some(cache) = &self.disk_cache {
                    let _ = cache.write_blob(&module.content_hash, &text);
                }
                text
            };
            modules.push(PackageModuleSource {
                subpath: module.subpath.clone(),
                text,
            });
        }

        let integrity = package_integrity(&wire.modules);
        let resolved = Rc::new(ResolvedPackage {
            key: key.clone(),
            resolved_version: version.clone(),
            manifest,
            integrity: integrity.clone(),
            modules,
        });

        // Persist the version's metadata (incl. its locked deps) so a pinned package
        // resolves fully offline next load and its transitive imports stay referrer-aware
        // (bodies are already in the blob cache above).
        if let Some(cache) = &self.disk_cache {
            let meta = CachedResolution {
                version: version.clone(),
                integrity: integrity.clone(),
                manifest: resolved.manifest.clone(),
                modules: wire
                    .modules
                    .iter()
                    .map(|module| CachedModule {
                        subpath: module.subpath.clone(),
                        content_hash: module.content_hash.clone(),
                    })
                    .collect(),
                dependencies: wire.dependencies.clone(),
            };
            let _ = cache.write_meta(key, &version, &meta);
        }

        if track {
            self.cache
                .borrow_mut()
                .insert((key.clone(), version.clone()), resolved.clone());
            // The referrer affects version *reads* (selection), not lockfile/report *writes*:
            // only a top-level (referrer-less) edge records the reported version and persists
            // the install's lockfile baseline. A transitive edge leaves both untouched, so it
            // can't clobber the top-level install's entry, integrity, or auto-update notice.
            if referrer.is_none() {
                self.resolved_versions
                    .borrow_mut()
                    .insert(key.clone(), version.clone());
                self.record_resolution(&specifier, &version, &integrity);
            }
        }

        Ok(resolved)
    }
}

#[async_trait::async_trait(?Send)]
impl PackageProvider for SmudgyPackageProvider {
    async fn resolve_package(
        &self,
        key: &PackageKey,
        referrer: Option<&ReferrerRef>,
    ) -> Result<Rc<ResolvedPackage>, PackageError> {
        self.resolve_impl(key, referrer, true).await
    }

    /// A stub fetch is a read of the producer's declarations, not a code load: nothing lands
    /// in the served set (`loaded_packages()` / the stumble diagnostic stays quiet) and
    /// nothing is recorded as an install — consuming an uninstalled producer leaves it
    /// uninstalled.
    async fn resolve_package_for_stub(
        &self,
        key: &PackageKey,
    ) -> Result<Rc<ResolvedPackage>, PackageError> {
        self.resolve_impl(key, None, false).await
    }

    fn get_cached(&self, key: &PackageKey, version: &str) -> Option<Rc<ResolvedPackage>> {
        self.cache
            .borrow()
            .get(&(key.clone(), version.to_string()))
            .cloned()
    }

    fn get_resolved(&self, key: &PackageKey) -> Option<Rc<ResolvedPackage>> {
        let version = self.resolved_versions.borrow().get(key).cloned()?;
        self.cache.borrow().get(&(key.clone(), version)).cloned()
    }

    /// The closure permission union folded by the last `solve_closure` over this isolate's
    /// closure (`PACKAGE-ISOLATES-ENFORCEMENT.md`). Empty (deny-all) until that pre-pass
    /// runs — the engine always runs it on the fork before reading this.
    fn closure_permissions(&self) -> PackagePermissions {
        self.closure_permission_union.borrow().clone()
    }

    /// Every package fetched-for-import through this provider so far. `cache` is only populated
    /// on the resolve paths (an import asked for the package), never by the `solve_closure`
    /// manifest walk, so its keys are this isolate's actually-served package set — what the
    /// engine's code-import stumble diagnostic inspects after module loading.
    fn loaded_packages(&self) -> Vec<PackageKey> {
        let mut keys: Vec<PackageKey> = self.cache.borrow().keys().map(|(key, _)| key.clone()).collect();
        keys.sort_by(|a, b| (&a.owner, &a.name).cmp(&(&b.owner, &b.name)));
        keys.dedup();
        keys
    }

    fn set_home_packages(&self, homes: Vec<PackageKey>) {
        *self.home_packages.borrow_mut() = Some(homes.iter().map(smudgy_script::PackageKey::folded).collect());
    }

    fn is_home_load(&self, key: &PackageKey) -> bool {
        self.home_packages
            .borrow()
            .as_ref()
            .is_none_or(|set| set.contains(&key.folded()))
    }

    fn note_scrubbed(&self, key: &PackageKey) {
        let mut scrubbed = self.scrubbed.borrow_mut();
        if !scrubbed.contains(key) {
            scrubbed.push(key.clone());
        }
    }

    fn scrubbed_packages(&self) -> Vec<PackageKey> {
        self.scrubbed.borrow().clone()
    }

    fn note_user_code_import(&self, key: &PackageKey) {
        let mut imports = self.user_imports.borrow_mut();
        if !imports.contains(key) {
            imports.push(key.clone());
        }
    }

    fn user_code_imports(&self) -> Vec<PackageKey> {
        self.user_imports.borrow().clone()
    }
}

/// A deterministic package-level integrity fingerprint over the per-module content
/// hashes (each is already a SHA-256). Detects any module change for the lockfile.
fn package_integrity(modules: &[ResolvedModuleWire]) -> String {
    let mut entries: Vec<String> = modules
        .iter()
        .map(|module| format!("{}={}", module.subpath, module.content_hash))
        .collect();
    entries.sort();
    entries.join(";")
}

/// Maps a module-body fetch error onto a [`PackageError`], distinguishing an integrity
/// failure (never serve unverified bytes) from a transport error.
fn fetch_error(specifier: &str, module: &ResolvedModuleWire, err: &CloudError) -> PackageError {
    let message = err.to_string();
    if message.contains("integrity mismatch") {
        PackageError::IntegrityMismatch {
            specifier: format!("{specifier}/{}", module.subpath),
            expected: module.content_hash.clone(),
            actual: message,
        }
    } else {
        PackageError::Network(format!("fetching {} for {specifier}: {message}", module.subpath))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::{Credential, CredentialSource};

    fn pkg_key(name: &str) -> PackageKey {
        PackageKey {
            owner: "wbk".into(),
            name: name.into(),
        }
    }

    fn referrer(name: &str, version: &str) -> ReferrerRef {
        ReferrerRef {
            key: pkg_key(name),
            version: version.into(),
        }
    }

    fn dep(name: &str, range: &str, version: &str) -> ResolvedDependency {
        ResolvedDependency {
            owner_nickname: "wbk".into(),
            name: name.into(),
            range: range.into(),
            resolved_version: version.into(),
        }
    }

    /// A provider with a non-functional client (no calls are made in these tests). The
    /// constructor reads a (missing) lockfile and opens the on-disk cache; both no-op
    /// gracefully, and the referrer mapping under test is independent of either.
    fn test_provider() -> SmudgyPackageProvider {
        let client = PackageApiClient::new(
            "http://127.0.0.1:0",
            CredentialSource::new(Some(Credential::ApiKey("test".into()))),
        );
        SmudgyPackageProvider::new(client, Arc::new("ReferrerProviderTest".to_string()))
    }

    #[test]
    fn dep_package_key_builds_from_nickname_and_name() {
        assert_eq!(dep_package_key("wbk", "util"), Some(pkg_key("util")));
        // An empty owner nickname is rejected.
        assert!(dep_package_key("", "util").is_none());
    }

    #[test]
    fn referrer_locked_version_scopes_selection_to_each_importer() {
        let provider = test_provider();
        let util = pkg_key("util");

        // app@1.0.0 locked util@1.3.0 (a range); other@1.0.0 pinned util@=2.0.0.
        provider.store_locked_deps(&pkg_key("app"), "1.0.0", &[dep("util", "^1.3", "1.3.0")]);
        provider.store_locked_deps(&pkg_key("other"), "1.0.0", &[dep("util", "=2.0.0", "2.0.0")]);

        // The heart of referrer-aware resolution: each importer selects the version IT
        // locked, with the declared range classified into the exact-pin flag.
        assert_eq!(
            provider.referrer_locked_version(&referrer("app", "1.0.0"), &util),
            Some(("1.3.0".to_string(), false))
        );
        assert_eq!(
            provider.referrer_locked_version(&referrer("other", "1.0.0"), &util),
            Some(("2.0.0".to_string(), true)),
            "an author =x dep is captured as an exact pin"
        );
        // An importer with no lock for the target falls through (None -> lockfile/latest).
        assert_eq!(
            provider.referrer_locked_version(&referrer("app", "1.0.0"), &pkg_key("absent")),
            None
        );
        assert_eq!(provider.referrer_locked_version(&referrer("unknown", "1.0.0"), &util), None);
    }

    #[tokio::test]
    async fn cap_version_offline_falls_back_to_last_resolved() {
        // `test_provider`'s client points at a dead address, so every resolve fails —
        // standing in for "signed out and the package isn't public" (or simply offline).
        let provider = test_provider();
        let locked: LockedPackage = serde_json::from_str(
            r#"{"specifier":"smudgy://wbk/util","mode":{"mode":"auto"},"last_resolved_version":"1.2.0","enabled":true}"#,
        )
        .expect("locked package");
        provider.lock.borrow_mut().packages.push(locked);

        // An installed auto-update package whose latest can't be resolved falls back to its
        // cached, already-consented last-resolved version instead of refusing to load. (Its
        // closure union and version floor resolve to empty/none while offline, which fit the
        // prior consent and any smudgy.)
        let capped = provider
            .cap_version("smudgy://wbk/util", &PackagePermissions::default())
            .await;
        assert_eq!(capped.as_deref(), Ok("1.2.0"));
    }

    #[tokio::test]
    async fn cap_version_reports_no_versions_for_an_unknown_uncached_package() {
        // The dead-address client stands in for "the cloud has no such package"; with no
        // `last_resolved_version` cached either, there is nothing to consider — the denial
        // must say so instead of claiming a permission problem. This is the stale lockfile
        // entry left when a `smudgy://local/…` install's folder is deleted.
        let provider = test_provider();
        let locked: LockedPackage = serde_json::from_str(
            r#"{"specifier":"smudgy://local/duo","mode":{"mode":"auto"},"enabled":true}"#,
        )
        .expect("locked package");
        provider.lock.borrow_mut().packages.push(locked);

        let capped = provider
            .cap_version("smudgy://local/duo", &PackagePermissions::default())
            .await;
        assert_eq!(capped, Err(CapRefusal::NoVersions));
    }

    fn util_req(version: &str, is_pin: bool) -> DepRequirement {
        DepRequirement {
            package: pkg_key("util"),
            version: version.into(),
            is_pin,
        }
    }

    #[test]
    fn fork_shares_lock_but_not_solve_state() {
        // A fork shares the expensive isolate-independent bits — crucially the lockfile view, so
        // partitioned lockfile writes across isolates stay consistent — but starts
        // with its own empty solve state: the whole point of per-isolate resolution.
        let base = test_provider();
        let fork = base.fork();
        assert!(
            Rc::ptr_eq(&base.lock, &fork.lock),
            "the per-server lockfile view is shared across isolates"
        );
        *base.solve.borrow_mut() = Some(package_solver::solve(&[util_req("1.0.0", false)]));
        assert!(base.solve.borrow().is_some());
        assert!(
            fork.solve.borrow().is_none(),
            "a fork's solve state is its own, independent of the base"
        );
    }

    #[test]
    fn forks_record_resolutions_into_one_shared_lock_without_clobber() {
        // Lockfile partition: main and a sandboxed isolate each top-level-install a
        // DIFFERENT package. Because their providers share one lock (`Rc<RefCell<…>>`), each fork's
        // `record_resolution` writes its own entry into the SAME in-memory lockfile, so neither
        // clobbers the other. The shared lock is what makes this safe: with
        // per-fork copies, each `record_resolution` would persist a stale snapshot missing the
        // other's entry (`PACKAGE-ISOLATES-RESOLUTION.md`). Disk persistence itself
        // (`save_lock` / `load_lock`) is covered in `shared_packages`.
        let main = test_provider();
        let sandbox = main.fork();

        main.record_resolution("smudgy://wbk/mapper", "1.4.0", "main-integrity");
        sandbox.record_resolution("smudgy://cor/combat", "2.0.0", "sandbox-integrity");

        // One shared lock holds BOTH installs at their own versions — partitioned, not clobbered.
        for provider in [&main, &sandbox] {
            let lock = provider.lock.borrow();
            assert_eq!(
                lock.find("smudgy://wbk/mapper").and_then(|p| p.last_resolved_version.as_deref()),
                Some("1.4.0"),
                "main's install survives the sandbox's later write into the shared lock"
            );
            assert_eq!(
                lock.find("smudgy://cor/combat").and_then(|p| p.last_resolved_version.as_deref()),
                Some("2.0.0"),
                "the sandbox's install is recorded into the same shared lock"
            );
        }
    }

    #[test]
    fn forked_isolates_resolve_a_shared_dep_independently() {
        // Each isolate has its OWN provider (a fork sharing only
        // client/cache/lock), so the SAME dependency can resolve to a different version per isolate
        // — no cross-isolate collapse (`PACKAGE-ISOLATES-RESOLUTION.md`). `solve_closure`'s
        // network walk is covered by the integration suite; here we feed its pure solver directly
        // (as `solve_closure` does after the walk) to assert the per-isolate state is independent.
        let base = test_provider();
        let main = base.fork();
        let sandbox = base.fork();
        let util = pkg_key("util");

        // main's closure locked util@1.4.0; the sandboxed isolate's closure locked util@1.2.0.
        *main.solve.borrow_mut() = Some(package_solver::solve(&[util_req("1.4.0", false)]));
        *sandbox.solve.borrow_mut() = Some(package_solver::solve(&[util_req("1.2.0", false)]));
        main.top_level_solved.borrow_mut().insert(util.clone(), "1.4.0".into());
        sandbox.top_level_solved.borrow_mut().insert(util.clone(), "1.2.0".into());

        // Each isolate resolves the dep at its own collapsed version.
        assert_eq!(main.solve_resolve(&util, "1.4.0", false), "1.4.0");
        assert_eq!(sandbox.solve_resolve(&util, "1.2.0", false), "1.2.0");
        // Independence is load-bearing: were the solve shared, the sandbox's 1.2.0 would collapse
        // UP to 1.4.0 (same compat class). It does not — the sandbox keeps 1.2.0...
        assert_eq!(sandbox.solve_resolve(&util, "1.2.0", false), "1.2.0");
        // ...while main, asked about 1.2.0, collapses to ITS OWN 1.4.0 — two distinct solve heaps.
        assert_eq!(main.solve_resolve(&util, "1.2.0", false), "1.4.0");
        // Top-level reads are isolate-local too.
        assert_eq!(main.top_level_solved.borrow().get(&util).map(String::as_str), Some("1.4.0"));
        assert_eq!(sandbox.top_level_solved.borrow().get(&util).map(String::as_str), Some("1.2.0"));
    }

    #[test]
    fn duplicate_warning_is_intra_isolate_only() {
        // The duplicate-version warning is intra-isolate under per-isolate providers
        // (`PACKAGE-ISOLATES-RESOLUTION.md`): each provider computes `loaded_duplicates` over its
        // OWN closure, so a cross-isolate duplicate never appears in one provider's closure.

        // Seed a provider's solve + duplicate-warning set exactly as `solve_closure` does after its
        // network walk: each importer is a top-level root at 1.0.0 depending on util at `version`.
        fn seed(provider: &SmudgyPackageProvider, importers: &[(&str, &str)]) {
            let util = pkg_key("util");
            let mut requirements = Vec::new();
            let mut roots = Vec::new();
            let mut edges = Vec::new();
            for (name, version) in importers {
                let root = DepRequirement {
                    package: pkg_key(name),
                    version: "1.0.0".into(),
                    is_pin: false,
                };
                requirements.push(root.clone());
                roots.push(root);
                requirements.push(DepRequirement {
                    package: util.clone(),
                    version: (*version).into(),
                    is_pin: false,
                });
                edges.push(DepEdge {
                    importer: pkg_key(name),
                    importer_version: "1.0.0".into(),
                    dep: util.clone(),
                    dep_version: (*version).into(),
                    dep_is_pin: false,
                });
            }
            let solve = package_solver::solve(&requirements);
            *provider.duplicate_warnings.borrow_mut() = solve.loaded_duplicates(&roots, &edges);
            *provider.solve.borrow_mut() = Some(solve);
        }

        let base = test_provider();

        // Cross-isolate: main's closure has only util@1.4.0, the sandboxed isolate's only util@1.2.0.
        // Each is a single version within its own closure → neither warns.
        let main = base.fork();
        let sandbox = base.fork();
        seed(&main, &[("app", "1.4.0")]);
        seed(&sandbox, &[("combat", "1.2.0")]);
        assert!(
            main.take_duplicate_warnings().is_empty(),
            "one util version in main's closure → no warning"
        );
        assert!(
            sandbox.take_duplicate_warnings().is_empty(),
            "the sandbox runs a different util version, but that cross-isolate duplicate is benign → no warning"
        );

        // Intra-isolate: a SINGLE isolate whose closure pulls two incompatible majors still warns.
        let mixed = base.fork();
        seed(&mixed, &[("a", "1.4.0"), ("b", "2.0.1")]);
        let warnings = mixed.take_duplicate_warnings();
        assert_eq!(
            warnings.len(),
            1,
            "two coexisting util majors in ONE isolate is a real collision → a warning"
        );
        assert_eq!(warnings[0].0, pkg_key("util"));
        assert_eq!(warnings[0].1, vec!["1.4.0", "2.0.1"]);
    }

    #[test]
    fn locked_deps_are_keyed_per_importer_version() {
        // Two coexisting versions of the SAME importer lock different dep versions; the
        // map must keep them distinct (else their transitive imports would collapse).
        let provider = test_provider();
        let util = pkg_key("util");
        provider.store_locked_deps(&pkg_key("app"), "1.0.0", &[dep("util", "^2", "2.0.0")]);
        provider.store_locked_deps(&pkg_key("app"), "2.0.0", &[dep("util", "^2.5", "2.5.0")]);

        assert_eq!(
            provider.referrer_locked_version(&referrer("app", "1.0.0"), &util),
            Some(("2.0.0".to_string(), false))
        );
        assert_eq!(
            provider.referrer_locked_version(&referrer("app", "2.0.0"), &util),
            Some(("2.5.0".to_string(), false))
        );
    }
}
