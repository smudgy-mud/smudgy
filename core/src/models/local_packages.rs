//! Local (authored) `smudgy://` packages: a package you're developing as a folder under
//! `<smudgy_home>/<server>/packages/<name>/` (per-server, beside `modules/`). The folder
//! holds a `smudgy.package.json` manifest plus the module files.
//!
//! While a local package exists, the session's package provider resolves every
//! `smudgy://<owner>/<name>` request with that leaf name to this folder (an npm-link-style
//! override). Its persistent settings live on a reserved `smudgy://local/<name>` lock row so
//! mutable local code never inherits trust, consent, or secrets from a same-name published
//! install. Publishing reads the folder and uploads it (create-or-get namespace, then an
//! immutable version). See `smudgy/script/PACKAGES.md`.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use smudgy_cloud::cloud_api::UserProfile;
use smudgy_cloud::{
    DependencyKind, PackageApiClient, PublishDependency, PublishModule, ResolvedPackageWire, Uuid,
    highest_satisfying_version,
};
use smudgy_script::{PackageManifest, SmudgySpecifier};

use crate::models::shared_packages::{
    self, Cas, LockedPackage, PackageStateTxn, SharedPackageLock,
};
use crate::{get_smudgy_home, models::persistence::write_atomic};

const MANIFEST_FILE: &str = "smudgy.package.json";
/// Durable link from an authored folder to the cloud namespace it published into. Dot-prefixed
/// so [`collect_modules`] never includes it in a published version.
const PUBLICATION_BINDING_FILE: &str = ".smudgy-publication.json";
/// First-publish intent. This is written before `POST /packages`, so a lost response or a
/// successful namespace claim followed by a failed binding write cannot make the folder appear
/// unpublished and renameable. A retry under the same account repeats create-or-get, validates the
/// returned namespace, and replaces this intent with [`PUBLICATION_BINDING_FILE`].
const PUBLICATION_CLAIM_FILE: &str = ".smudgy-publication-claim.json";

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct PublicationClaimIntent {
    version: u32,
    account_id: Uuid,
    account_nickname: String,
    leaf: String,
}

/// The owner segment a local package's persistent state lives under: `smudgy://local/<name>`.
/// Reserved on the server so no real account can publish under it and collide.
pub const LOCAL_OWNER: &str = "local";

/// The `smudgy:core` ambient declarations, made available to the publish-time `.d.ts`
/// generator so a package's `import … from "smudgy:core"` resolves while emitting.
const SMUDGY_CORE_DTS: &str = include_str!("script_typings/smudgy-core.d.ts");
/// The `mapper` ambient declarations (global `Mapper`/`Area`/`Room`/...).
const SMUDGY_MAPPER_DTS: &str = include_str!("script_typings/smudgy-mapper.d.ts");
/// The `smudgy:widgets` + `smudgy:widgets/jsx-runtime` ambient declarations.
const SMUDGY_WIDGETS_DTS: &str = include_str!("script_typings/smudgy-widgets.d.ts");
/// Package parameter declarations, so packages that read their manifest settings through
/// `smudgy:params` type-check at publish time as they do in the editor.
const SMUDGY_PARAMS_DTS: &str = include_str!("script_typings/smudgy-params.d.ts");

fn snapshot_field(hasher: &mut Sha256, tag: &[u8], value: &[u8]) -> Result<()> {
    let tag_len = u64::try_from(tag.len()).context("snapshot tag is too large")?;
    let value_len = u64::try_from(value.len()).context("snapshot field is too large")?;
    hasher.update(tag_len.to_le_bytes());
    hasher.update(tag);
    hasher.update(value_len.to_le_bytes());
    hasher.update(value);
    Ok(())
}

/// A digest of everything a publish uploads, so the folder can be checked for edits between the
/// upload's preparation and its irreversible remote commit.
fn package_snapshot_digest(package: &LocalPackage) -> Result<String> {
    let mut hasher = Sha256::new();
    snapshot_field(&mut hasher, b"format", b"smudgy-local-package-snapshot-v4")?;
    snapshot_field(&mut hasher, b"name", package.name.as_bytes())?;
    snapshot_field(
        &mut hasher,
        b"manifest",
        &serde_json::to_vec(&package.manifest)?,
    )?;
    match &package.readme {
        Some(readme) => {
            snapshot_field(&mut hasher, b"readme-present", b"1")?;
            snapshot_field(&mut hasher, b"readme", readme.as_bytes())?;
        }
        None => snapshot_field(&mut hasher, b"readme-present", b"0")?,
    }
    snapshot_field(
        &mut hasher,
        b"module-count",
        &u64::try_from(package.modules.len())
            .context("too many package modules")?
            .to_le_bytes(),
    )?;
    for module in &package.modules {
        snapshot_field(&mut hasher, b"module-subpath", module.subpath.as_bytes())?;
        snapshot_field(&mut hasher, b"module-content", &module.content)?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// What a successful publish reports back: the published version plus the outcome of the
/// publish-time TypeScript declaration generation. Declaration generation is **best-effort
/// and never fatal** — a package always publishes even if typings can't be produced.
#[derive(Debug, Clone)]
pub struct PublishSummary {
    /// The package namespace that owns the newly published version.
    pub package_id: Uuid,
    /// The namespace's current visibility, returned by create-or-get before publishing.
    pub is_public: bool,
    /// The published version (the manifest's `version`).
    pub version: String,
    /// The server commit time for the published version.
    pub published_at: DateTime<Utc>,
    /// How many `.d.ts` modules shipped with the version (0 if none were generated).
    pub typings_generated: usize,
    /// Non-fatal warnings from declaration generation (tsc diagnostics, a failed/empty run),
    /// surfaced to the author. Empty on a clean typings pass.
    pub typings_warnings: Vec<String>,
    /// What each `smudgy://` dependency locked to this publish: `(specifier, resolved_version)`.
    pub locked_dependencies: Vec<(String, String)>,
    /// Non-fatal warnings about dependency locking — e.g. a declared range that excludes a *newer*
    /// published version (most notably the 0.0.x caret footgun).
    pub dependency_warnings: Vec<String>,
    /// Non-fatal interop-declaration warnings (interop.md §4).
    pub interop_warnings: Vec<String>,
    /// A successful remote publish whose local bookkeeping could not be confirmed at completion.
    /// The version is already live and must not be retried.
    pub publication_warnings: Vec<PublicationWarning>,
}

/// A recoverable publish outcome that needs a precise, localized explanation in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PublicationWarning {
    /// The final response was lost, but resolving the target version confirmed that its exact
    /// manifest, README, module hashes, and locked dependencies match this publish request.
    VersionPresentAfterLostResponse { name: String, version: String },
    /// Finalize returned a success body that did not identify the requested namespace and version,
    /// but an independent resolve confirmed the exact immutable payload at the intended target.
    InconsistentResponseRecovered { name: String, version: String },
    /// A retry found the exact immutable payload already live.
    ExistingVersionRecovered { name: String, version: String },
    /// The cloud version is live, but the local namespace sidecar could not be confirmed.
    MissingLocalBinding { name: String, version: String },
    /// The cloud version is live, but reading the namespace sidecar failed.
    LocalBindingUnverified {
        name: String,
        version: String,
        error: String,
    },
    /// The cloud version contains the captured snapshot, but the local folder changed or became
    /// unreadable while the irreversible remote request was running.
    LocalSnapshotChanged { name: String, version: String },
    /// The cloud version is live, but the package-level description could not be updated to match
    /// the description captured in that version's manifest.
    DescriptionUpdateFailed {
        name: String,
        version: String,
        error: String,
    },
}

/// The cloud namespace permanently associated with one local package folder.
///
/// Published package names are immutable. Once this sidecar exists, the local folder cannot be
/// renamed; authors make a differently-named copy instead.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PublicationBinding {
    pub package_id: Uuid,
    pub leaf: String,
}
const README_FILE: &str = "README.md";
/// The editor-only `tsconfig.json` a copied ("Make a copy") package carries so VS Code types it
/// against the server-level smudgy project. It's scaffolding, never package content, so it is
/// excluded from publishing — treated like a dotfile by [`collect_modules`].
const TSCONFIG_FILE: &str = "tsconfig.json";
/// The body written into a copied package's [`TSCONFIG_FILE`].
const PACKAGE_TSCONFIG: &str = "{ \"extends\": \"../../tsconfig.json\" }\n";
const STARTER_MANIFEST: &str =
    "{\n  \"version\": \"0.1.0\",\n  \"description\": \"\",\n  \"entry\": \"index.ts\"\n}\n";
const STARTER_ENTRY: &str = "// smudgy package entry\nexport {};\n";
/// Directories never published even if present in a package folder — dependency/build cruft
/// that the exclude-list would otherwise recurse into and ship wholesale.
const SKIP_DIRS: [&str; 6] = ["node_modules", "target", "dist", "build", "out", "coverage"];

/// A local package loaded from disk: its manifest, README, and module files.
#[derive(Debug, Clone, PartialEq)]
pub struct LocalPackage {
    pub name: String,
    pub manifest: PackageManifest,
    /// The package's `README.md` (markdown), if present.
    pub readme: Option<String>,
    pub modules: Vec<LocalModule>,
}

/// One module file within a [`LocalPackage`] (`subpath` is relative to the package dir,
/// always forward-slashed).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalModule {
    pub subpath: String,
    /// Raw file bytes — any file in the package dir (text or binary) is publishable.
    pub content: Vec<u8>,
}

/// Result of deleting a local package. Warnings describe settings cleanup that could not finish;
/// the package is no longer discoverable or runnable even when this list is not empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeleteLocalPackageSummary {
    pub warnings: Vec<String>,
}

/// Result of replacing one authored package file with an exact compare-and-swap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalFileWriteOutcome {
    Saved,
    /// The file no longer contains the editor's baseline, so nothing was written.
    Conflict,
}

fn packages_dir_in(home: &Path, server_name: &str) -> PathBuf {
    home.join(server_name).join("packages")
}

fn checked_package_name(name: &str) -> Result<&str> {
    let name = name.trim();
    crate::models::naming::validate_package_name(name).map_err(|error| anyhow!(error))?;
    Ok(name)
}

fn checked_module_subpath(subpath: &str) -> Result<&str> {
    let subpath = subpath.trim();
    crate::models::naming::validate_module_subpath(subpath).map_err(|error| anyhow!(error))?;
    Ok(subpath)
}

/// `<smudgy_home>/<server>/packages/`.
///
/// # Errors
/// Returns an error if the smudgy home directory cannot be determined.
pub fn packages_dir(server_name: &str) -> Result<PathBuf> {
    Ok(packages_dir_in(&get_smudgy_home()?, server_name))
}

/// Exact path of one validated local package manifest.
pub(crate) fn local_manifest_path(server_name: &str, name: &str) -> Result<PathBuf> {
    let name = checked_package_name(name)?;
    Ok(packages_dir(server_name)?.join(name).join(MANIFEST_FILE))
}

/// Runs a local-folder operation under the server's package lock.
pub(crate) fn with_local_package_transaction<R>(
    server_name: &str,
    operation: impl FnOnce(&Path, &PackageStateTxn<'_>) -> Result<R>,
) -> Result<R> {
    let home = get_smudgy_home()?;
    shared_packages::with_local_package_transaction(server_name, |transaction| {
        operation(&home, transaction)
    })
}

fn local_state_specifier(name: &str) -> String {
    format!("smudgy://{LOCAL_OWNER}/{name}")
}

fn is_real_directory(metadata: &fs::Metadata) -> bool {
    metadata.is_dir() && !metadata.file_type().is_symlink()
}

fn is_real_file(metadata: &fs::Metadata) -> bool {
    metadata.is_file() && !metadata.file_type().is_symlink()
}

fn real_directory_exists(path: &Path, label: &str) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if is_real_directory(&metadata) => Ok(true),
        Ok(_) => bail!("{label} is not a real directory: {}", path.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).with_context(|| format!("inspect {}", path.display())),
    }
}

fn local_package_dir_if_real_in(
    home: &Path,
    server_name: &str,
    name: &str,
) -> Result<Option<PathBuf>> {
    let packages = packages_dir_in(home, server_name);
    if !real_directory_exists(&packages, "local package root")? {
        return Ok(None);
    }
    let package = packages.join(name);
    if !real_directory_exists(&package, "local package")? {
        return Ok(None);
    }
    Ok(Some(package))
}

fn require_local_package_dir_in(home: &Path, server_name: &str, name: &str) -> Result<PathBuf> {
    local_package_dir_if_real_in(home, server_name, name)?
        .ok_or_else(|| anyhow!("no local package named {name}"))
}

fn require_local_manifest(package_dir: &Path, name: &str) -> Result<()> {
    if !real_directory_exists(package_dir, "local package")? {
        bail!("no local package named {name}");
    }
    let manifest = package_dir.join(MANIFEST_FILE);
    match fs::symlink_metadata(&manifest) {
        Ok(metadata) if is_real_file(&metadata) => Ok(()),
        Ok(_) => bail!("{} is not a file", manifest.display()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            bail!("no local package named {name}")
        }
        Err(error) => Err(error).with_context(|| format!("inspect {}", manifest.display())),
    }
}

// ---------------------------------------------------------------------------
// Publication sidecars
// ---------------------------------------------------------------------------

fn publication_claim_path_in(home: &Path, server_name: &str, name: &str) -> PathBuf {
    packages_dir_in(home, server_name)
        .join(name)
        .join(PUBLICATION_CLAIM_FILE)
}

fn read_sidecar_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<Option<T>> {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content)
            .with_context(|| format!("parse {}", path.display()))
            .map(Some),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("read {}", path.display())),
    }
}

fn load_publication_claim_in(
    home: &Path,
    server_name: &str,
    name: &str,
) -> Result<Option<PublicationClaimIntent>> {
    let name = checked_package_name(name)?;
    let Some(package_dir) = local_package_dir_if_real_in(home, server_name, name)? else {
        return Ok(None);
    };
    let Some(intent) =
        read_sidecar_json::<PublicationClaimIntent>(&package_dir.join(PUBLICATION_CLAIM_FILE))?
    else {
        return Ok(None);
    };
    if intent.version != 1 {
        bail!(
            "unsupported publication-claim version {} for local package {name}",
            intent.version
        );
    }
    checked_package_name(&intent.account_nickname)
        .context("publication claim contains an invalid account nickname")?;
    checked_package_name(&intent.leaf).context("publication claim contains an invalid leaf")?;
    if intent.account_id.is_nil() {
        bail!("publication claim for local package {name} has no account identity");
    }
    if !crate::models::naming::names_conflict(name, &intent.leaf) {
        bail!(
            "publication claim for local package {name} names a different leaf: {}",
            intent.leaf
        );
    }
    Ok(Some(intent))
}

fn ensure_publication_claim_in(
    home: &Path,
    server_name: &str,
    name: &str,
    account_id: Uuid,
    account_nickname: &str,
) -> Result<PublicationClaimIntent> {
    let name = checked_package_name(name)?;
    let account_nickname = checked_package_name(account_nickname)?;
    if account_id.is_nil() {
        bail!("cannot publish local package {name} without an account identity");
    }
    if let Some(intent) = load_publication_claim_in(home, server_name, name)? {
        if intent.account_id != account_id {
            bail!(
                "local package {name} has an unfinished publication claim for a different account"
            );
        }
        if crate::models::naming::names_conflict(&intent.account_nickname, account_nickname) {
            return Ok(intent);
        }
        // Nicknames can change, but the immutable user UUID is the account authority. Refresh the
        // descriptive handle so validation and recovery messages use the current coordinate.
        let refreshed = PublicationClaimIntent {
            account_nickname: account_nickname.to_string(),
            ..intent
        };
        let json = serde_json::to_vec_pretty(&refreshed)
            .context("serialize refreshed publication claim")?;
        let path = publication_claim_path_in(home, server_name, name);
        write_atomic(&path, &json).with_context(|| format!("write {}", path.display()))?;
        return Ok(refreshed);
    }

    let package_dir = require_local_package_dir_in(home, server_name, name)?;
    require_local_manifest(&package_dir, name)?;
    let intent = PublicationClaimIntent {
        version: 1,
        account_id,
        account_nickname: account_nickname.to_string(),
        leaf: name.to_string(),
    };
    let json = serde_json::to_vec_pretty(&intent).context("serialize publication claim")?;
    let path = publication_claim_path_in(home, server_name, name);
    write_atomic(&path, &json).with_context(|| format!("write {}", path.display()))?;
    Ok(intent)
}

fn remove_publication_claim_in(home: &Path, server_name: &str, name: &str) -> Result<()> {
    let name = checked_package_name(name)?;
    let Some(package_dir) = local_package_dir_if_real_in(home, server_name, name)? else {
        return Ok(());
    };
    let path = package_dir.join(PUBLICATION_CLAIM_FILE);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("remove {}", path.display())),
    }
}

fn remove_publication_claim_if_unchanged_in(
    home: &Path,
    server_name: &str,
    name: &str,
    expected: &PublicationClaimIntent,
) -> Result<bool> {
    if load_publication_claim_in(home, server_name, name)?.as_ref() != Some(expected) {
        return Ok(false);
    }
    remove_publication_claim_in(home, server_name, name)?;
    Ok(true)
}

fn prepare_publication_namespace_state(
    server_name: &str,
    name: &str,
    expected_snapshot: &str,
    account_id: Uuid,
    account_nickname: &str,
) -> Result<(Option<PublicationBinding>, Option<PublicationClaimIntent>)> {
    with_local_package_transaction(server_name, |home, _| {
        ensure_local_snapshot_matches_in(home, server_name, name, expected_snapshot)?;
        let binding = load_publication_binding_in(home, server_name, name)?;
        let claim = if binding.is_none() {
            Some(ensure_publication_claim_in(
                home,
                server_name,
                name,
                account_id,
                account_nickname,
            )?)
        } else {
            load_publication_claim_in(home, server_name, name)?
        };
        Ok((binding, claim))
    })
}

fn clear_publication_claim_if_unchanged(
    server_name: &str,
    name: &str,
    expected: &PublicationClaimIntent,
) -> Result<bool> {
    with_local_package_transaction(server_name, |home, _| {
        remove_publication_claim_if_unchanged_in(home, server_name, name, expected)
    })
}

fn commit_publication_binding(
    server_name: &str,
    name: &str,
    package_id: Uuid,
    published_leaf: &str,
    expected_claim: &PublicationClaimIntent,
) -> Result<()> {
    with_local_package_transaction(server_name, |home, _| {
        save_publication_binding_in(home, server_name, name, package_id, published_leaf)?;
        let _ = remove_publication_claim_if_unchanged_in(home, server_name, name, expected_claim)?;
        Ok(())
    })
}

fn validate_claimed_namespace(
    intent: &PublicationClaimIntent,
    view: &smudgy_cloud::PackageView,
) -> Result<()> {
    if view.owner_id != intent.account_id {
        bail!(
            "the claimed cloud namespace for local package {} belongs to a different account",
            intent.leaf
        );
    }
    if !crate::models::naming::names_conflict(&view.name, &intent.leaf) {
        bail!(
            "the claimed cloud namespace {} does not match local package {}",
            view.name,
            intent.leaf
        );
    }
    Ok(())
}

/// Reads the cloud-namespace binding for a local package. Older, never-published folders
/// have no sidecar and return `None`.
///
/// # Errors
/// Returns an error when the package name is invalid, or when an existing sidecar is unreadable,
/// malformed, or names a different leaf.
pub fn load_publication_binding(
    server_name: &str,
    name: &str,
) -> Result<Option<PublicationBinding>> {
    with_local_package_transaction(server_name, |home, _| {
        load_publication_binding_in(home, server_name, name)
    })
}

fn load_publication_binding_in(
    home: &Path,
    server_name: &str,
    name: &str,
) -> Result<Option<PublicationBinding>> {
    let name = checked_package_name(name)?;
    let Some(package_dir) = local_package_dir_if_real_in(home, server_name, name)? else {
        return Ok(None);
    };
    let Some(binding) =
        read_sidecar_json::<PublicationBinding>(&package_dir.join(PUBLICATION_BINDING_FILE))?
    else {
        return Ok(None);
    };
    checked_package_name(&binding.leaf)?;
    if binding.package_id.is_nil() {
        bail!("publication binding for local package {name} has no namespace identity");
    }
    if !crate::models::naming::names_conflict(name, &binding.leaf) {
        bail!(
            "publication binding for local package {name} names a different leaf: {}",
            binding.leaf
        );
    }
    Ok(Some(binding))
}

/// Binds an existing local package folder to its published cloud namespace.
///
/// This is also the backfill API for folders published before sidecars existed.
///
/// # Errors
/// Returns an error when either leaf is invalid, the published leaf differs from the local folder,
/// the local package does not exist, or the sidecar cannot be written.
pub fn save_publication_binding(
    server_name: &str,
    local_name: &str,
    package_id: Uuid,
    published_leaf: &str,
) -> Result<()> {
    with_local_package_transaction(server_name, |home, _| {
        save_publication_binding_in(home, server_name, local_name, package_id, published_leaf)
    })
}

fn save_publication_binding_in(
    home: &Path,
    server_name: &str,
    local_name: &str,
    package_id: Uuid,
    published_leaf: &str,
) -> Result<()> {
    let local_name = checked_package_name(local_name)?;
    let published_leaf = checked_package_name(published_leaf)?;
    if !crate::models::naming::names_conflict(local_name, published_leaf) {
        bail!("published leaf {published_leaf} does not match local package {local_name}");
    }
    if package_id.is_nil() {
        bail!("published namespace for local package {local_name} has no identity");
    }
    let package_dir = require_local_package_dir_in(home, server_name, local_name)?;
    require_local_manifest(&package_dir, local_name)?;
    let binding = PublicationBinding {
        package_id,
        leaf: published_leaf.to_string(),
    };
    if let Some(existing) = load_publication_binding_in(home, server_name, local_name)? {
        if existing.package_id == binding.package_id
            && crate::models::naming::names_conflict(&existing.leaf, &binding.leaf)
        {
            return Ok(());
        }
        bail!("local package {local_name} is already bound to a different cloud namespace");
    }
    let json = serde_json::to_vec_pretty(&binding).context("serialize publication binding")?;
    let path = package_dir.join(PUBLICATION_BINDING_FILE);
    write_atomic(&path, &json).with_context(|| format!("write {}", path.display()))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

/// Reads one raw file (`subpath` relative to the package dir) from a local package.
///
/// # Errors
/// Returns an error if the smudgy home can't be resolved or the file can't be read.
pub fn read_local_file(server_name: &str, name: &str, subpath: &str) -> Result<String> {
    with_local_package_transaction(server_name, |home, _| {
        let name = checked_package_name(name)?;
        let subpath = checked_module_subpath(subpath)?;
        let path = require_local_package_dir_in(home, server_name, name)?.join(subpath);
        fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))
    })
}

/// Writes one raw file (`subpath` relative to the package dir) into a local package,
/// creating parent directories as needed.
///
/// # Errors
/// Returns an error if the smudgy home can't be resolved or the file can't be written.
pub fn write_local_file(server_name: &str, name: &str, subpath: &str, content: &str) -> Result<()> {
    with_local_package_transaction(server_name, |home, _| {
        let name = checked_package_name(name)?;
        let subpath = checked_module_subpath(subpath)?;
        let path = require_local_package_dir_in(home, server_name, name)?.join(subpath);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        write_atomic(&path, content.as_bytes()).with_context(|| format!("write {}", path.display()))
    })
}

/// Replaces one local-package text file only when it still contains the text the editor opened.
///
/// # Errors
/// Returns an error for an invalid name or path, or a failed read or write.
pub fn write_local_file_if_unchanged(
    server_name: &str,
    name: &str,
    subpath: &str,
    expected: &str,
    content: &str,
) -> Result<LocalFileWriteOutcome> {
    with_local_package_transaction(server_name, |home, _| {
        let name = checked_package_name(name)?;
        let subpath = checked_module_subpath(subpath)?;
        let path = require_local_package_dir_in(home, server_name, name)?.join(subpath);
        let current = fs::read_to_string(&path)
            .with_context(|| format!("read {} before saving", path.display()))?;
        if current != expected {
            return Ok(LocalFileWriteOutcome::Conflict);
        }
        write_atomic(&path, content.as_bytes())
            .with_context(|| format!("write {}", path.display()))?;
        Ok(LocalFileWriteOutcome::Saved)
    })
}

// ---------------------------------------------------------------------------
// Inventory and governing rows
// ---------------------------------------------------------------------------

/// Names of the local packages authored for `server_name`.
///
/// # Errors
/// Returns an error if the packages directory can't be read or two folders differ only by
/// letter case.
pub fn list_local_packages(server_name: &str) -> Result<Vec<String>> {
    with_local_package_transaction(server_name, |home, _| {
        list_local_packages_in(home, server_name)
    })
}

pub(crate) fn list_local_packages_in(home: &Path, server_name: &str) -> Result<Vec<String>> {
    let dir = packages_dir_in(home, server_name);
    let mut names = Vec::new();
    if !real_directory_exists(&dir, "local package root")? {
        return Ok(names);
    }
    for entry in fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry.with_context(|| format!("read entry in {}", dir.display()))?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let metadata = fs::symlink_metadata(entry.path())
            .with_context(|| format!("inspect {}", entry.path().display()))?;
        if !is_real_directory(&metadata) || checked_package_name(&name).is_err() {
            continue;
        }
        // The directory itself reserves this leaf. A missing or damaged manifest must make the
        // local override fail closed rather than let a same-leaf published fallback start.
        names.push(name);
    }
    names.sort();
    checked_unique_local_names(&names)
}

fn checked_unique_local_names(local_names: &[String]) -> Result<Vec<String>> {
    let mut seen = BTreeMap::<String, String>::new();
    let mut unique = Vec::new();
    for name in local_names {
        let folded = name.to_ascii_lowercase();
        match seen.get(&folded) {
            Some(existing) if existing != name => {
                bail!(
                    "local package folders {existing} and {name} differ only by letter case; rename or remove one before packages can load"
                );
            }
            Some(_) => {}
            None => {
                seen.insert(folded, name.clone());
                unique.push(name.clone());
            }
        }
    }
    Ok(unique)
}

/// Validates the distinct local state row and globally unique published fallback invariant.
fn validate_local_governing_rows(lock: &SharedPackageLock, local_names: &[String]) -> Result<()> {
    for name in local_names {
        let mut local_rows = 0;
        let mut remote_rows = 0;
        for package in &lock.packages {
            let specifier = SmudgySpecifier::parse(&package.specifier).map_err(|error| {
                anyhow!(
                    "package state contains invalid identity {}: {error}",
                    package.specifier
                )
            })?;
            if !specifier.name.eq_ignore_ascii_case(name) {
                continue;
            }
            if specifier.owner.eq_ignore_ascii_case(LOCAL_OWNER) {
                local_rows += 1;
            } else {
                remote_rows += 1;
            }
        }
        if local_rows > 1 {
            bail!("more than one local state row exists for package {name}");
        }
        if remote_rows > 1 {
            bail!(
                "more than one published package named {name} is installed; remove the conflicting rows before using the local package"
            );
        }
    }
    Ok(())
}

/// Adds a governing `smudgy://local/<name>` row for every local name that lacks one.
///
/// A new row seeds only activation, update mode, and required-install lineage from one genuinely
/// remote same-leaf fallback, so a local copy keeps running where the package it replaces ran,
/// and every package the fallback required now also lists the local row as a requirer, so those
/// requirements stay effective under the override. Security, parameter values, version
/// metadata, and provenance start at safe local defaults. With no fallback the row starts
/// disabled in auto mode. Returns whether anything was added.
fn materialize_local_governing_rows_in(
    lock: &mut SharedPackageLock,
    local_names: &[String],
) -> Result<bool> {
    validate_local_governing_rows(lock, local_names)?;
    let mut additions = Vec::new();
    for name in local_names {
        let state_specifier = local_state_specifier(name);
        let mut remote = None;
        let mut existing = false;
        for package in &lock.packages {
            let Ok(specifier) = SmudgySpecifier::parse(&package.specifier) else {
                continue;
            };
            if !specifier.name.eq_ignore_ascii_case(name) {
                continue;
            }
            if specifier.owner.eq_ignore_ascii_case(LOCAL_OWNER) {
                existing = true;
            } else {
                remote = Some(package);
            }
        }
        if existing {
            continue;
        }
        let mode = remote.map_or_else(Default::default, |fallback| fallback.mode.clone());
        let activation = remote.map_or(
            crate::models::profile_activation::ProfileActivation::None,
            LockedPackage::activation,
        );
        let mut governing = LockedPackage::new(state_specifier, mode);
        governing.set_activation(activation);
        if let Some(fallback) = remote {
            governing.required_by.clone_from(&fallback.required_by);
        }
        // A local folder is always an explicit, user-authored root. It may also satisfy a
        // published fallback's `requires` links, but it must keep its own Settings/activation.
        governing.installed_as_requirement = false;
        additions.push((remote.map(|fallback| fallback.specifier.clone()), governing));
    }
    let changed = !additions.is_empty();
    for (fallback, governing) in additions {
        if let Some(fallback) = fallback {
            for package in &mut lock.packages {
                if package.required_by.contains(&fallback) {
                    package.required_by.insert(governing.specifier.clone());
                }
            }
        }
        lock.packages.push(governing);
    }
    Ok(changed)
}

/// Ensures every local package leaf has a distinct governing row at `smudgy://local/<name>`.
///
/// `canonical_owner` is validated for source compatibility but never owns local persistent
/// state. Returns `true` when a row was added.
///
/// # Errors
/// Returns an error for an invalid name, contradictory remote state, retired parameter state
/// that would be inherited, or a lockfile read/write failure.
pub fn materialize_governing_local_lock_rows(
    server_name: &str,
    local_names: &[String],
    canonical_owner: &str,
) -> Result<bool> {
    with_local_package_transaction(server_name, |_, transaction| {
        materialize_governing_local_lock_rows_in_transaction(
            transaction,
            local_names,
            canonical_owner,
        )
    })
}

pub(crate) fn materialize_governing_local_lock_rows_in_transaction(
    transaction: &PackageStateTxn<'_>,
    local_names: &[String],
    canonical_owner: &str,
) -> Result<bool> {
    checked_package_name(canonical_owner)?;
    let names = checked_unique_local_names(local_names)?;
    for name in &names {
        checked_package_name(name)?;
    }
    // A row under the account's own name for a leaf that has a local folder is the identity
    // local packages used before the reserved `local` owner existed. Its settings move to the
    // local identity before the row does, so an interruption leaves both readable.
    let legacy =
        legacy_account_owned_local_rows(&transaction.load_lock()?, &names, canonical_owner);
    for (from, to) in &legacy {
        transaction.copy_package_param_state(from, to)?;
    }
    let changed = transaction.mutate_lock(|lock| {
        let migrated = migrate_legacy_account_owned_local_rows_in(lock, &legacy);
        let inserted = materialize_local_governing_rows_in(lock, &names)?;
        Ok((migrated || inserted, migrated || inserted))
    })?;
    for (from, _) in &legacy {
        if let Err(error) = transaction.remove_package_param_state(from) {
            warn!("Deferred cleanup of settings for migrated local package {from}: {error:#}");
        }
    }
    Ok(changed)
}

/// `(account-owned specifier, local specifier)` for every local name that has no governing row
/// yet but does have a row under `canonical_owner`, the identity a pre-`local` client gave the
/// folder. With the reserved owner itself as `canonical_owner` there is nothing to migrate.
fn legacy_account_owned_local_rows(
    lock: &SharedPackageLock,
    local_names: &[String],
    canonical_owner: &str,
) -> Vec<(String, String)> {
    if canonical_owner.eq_ignore_ascii_case(LOCAL_OWNER) {
        return Vec::new();
    }
    let mut pairs = Vec::new();
    for name in local_names {
        let state_specifier = local_state_specifier(name);
        if lock.find(&state_specifier).is_some() {
            continue;
        }
        let legacy = lock.packages.iter().find(|package| {
            SmudgySpecifier::parse(&package.specifier).is_ok_and(|specifier| {
                specifier.name.eq_ignore_ascii_case(name)
                    && specifier.owner.eq_ignore_ascii_case(canonical_owner)
            })
        });
        if let Some(legacy) = legacy {
            pairs.push((legacy.specifier.clone(), state_specifier));
        }
    }
    pairs
}

/// Moves each legacy account-owned row onto its local identity: trust, consent, activation,
/// parameter scope, and requirement lineage carry over unchanged; version metadata resets
/// because the folder, not a published version, is now the source. The account-owned row is
/// removed rather than kept as a fallback, because it never described a published install.
/// Returns whether the lock changed.
fn migrate_legacy_account_owned_local_rows_in(
    lock: &mut SharedPackageLock,
    pairs: &[(String, String)],
) -> bool {
    let mut changed = false;
    for (from, to) in pairs {
        let Some(index) = lock
            .packages
            .iter()
            .position(|package| package.specifier == *from)
        else {
            continue;
        };
        let mut row = lock.packages.remove(index);
        row.specifier.clone_from(to);
        row.last_resolved_version = None;
        row.integrity = None;
        row.dismissed_update_version = None;
        row.installed_as_requirement = false;
        lock.packages.push(row);
        for package in &mut lock.packages {
            if package.required_by.remove(from) {
                package.required_by.insert(to.clone());
            }
        }
        changed = true;
    }
    changed
}

/// Load a local package (`None` if no folder/manifest exists).
///
/// # Errors
/// Returns an error if the manifest is unreadable or invalid, or a module file can't be
/// read.
pub fn load_local_package(server_name: &str, name: &str) -> Result<Option<LocalPackage>> {
    with_local_package_transaction(server_name, |home, _| {
        load_local_package_in(home, server_name, name)
    })
}

pub(crate) fn load_local_package_in(
    home: &Path,
    server_name: &str,
    name: &str,
) -> Result<Option<LocalPackage>> {
    let name = checked_package_name(name)?;
    let Some(dir) = local_package_dir_if_real_in(home, server_name, name)? else {
        return Ok(None);
    };
    let manifest_path = dir.join(MANIFEST_FILE);
    let manifest_text = match fs::read_to_string(&manifest_path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", manifest_path.display()));
        }
    };
    let manifest = PackageManifest::parse(&manifest_text)
        .map_err(|e| anyhow!("invalid {}: {e}", manifest_path.display()))?;
    let readme_path = dir.join(README_FILE);
    let readme = match fs::read_to_string(&readme_path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", readme_path.display()));
        }
    };
    let mut modules = Vec::new();
    collect_modules(&dir, &dir, &mut modules)?;
    modules.sort_by(|a, b| a.subpath.cmp(&b.subpath));
    Ok(Some(LocalPackage {
        name: name.to_string(),
        manifest,
        readme,
        modules,
    }))
}

fn collect_modules(root: &Path, dir: &Path, out: &mut Vec<LocalModule>) -> Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let file_name = file_name.to_string_lossy();
        // Skip dotfiles AND dot-directories (`.git`, `.cache`, `.env`, …) everywhere, plus
        // well-known dependency/build directories, so their contents are never published.
        // The editor-only `tsconfig.json` is treated like a dotfile too.
        if file_name.starts_with('.')
            || SKIP_DIRS.contains(&file_name.as_ref())
            || file_name == TSCONFIG_FILE
        {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&path).with_context(|| format!("inspect {}", path.display()))?;
        if is_real_directory(&metadata) {
            collect_modules(root, &path, out)?;
        } else if is_real_file(&metadata) {
            // Everything else is a publishable module (any bytes) EXCEPT the manifest (implied)
            // and the README (published separately as `readme`).
            if file_name == MANIFEST_FILE || file_name == README_FILE {
                continue;
            }
            let subpath = path
                .strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let content = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            out.push(LocalModule { subpath, content });
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Create, copy, rename, delete
// ---------------------------------------------------------------------------

fn starter_package() -> Result<(PackageManifest, [LocalModule; 1])> {
    let manifest = PackageManifest::parse(STARTER_MANIFEST).context("parse starter manifest")?;
    let modules = [LocalModule {
        subpath: "index.ts".to_string(),
        content: STARTER_ENTRY.as_bytes().to_vec(),
    }];
    Ok((manifest, modules))
}

/// Scaffold a new local package folder with a starter manifest + `index.ts`, without touching
/// package state.
///
/// # Errors
/// Returns an error if the package already exists or the files can't be written.
pub fn scaffold_local_package(server_name: &str, name: &str) -> Result<()> {
    with_local_package_transaction(server_name, |home, _| {
        let (manifest, modules) = starter_package()?;
        let readme = format!("# {name}\n\nDescribe your package here.\n");
        publish_staged_package(home, server_name, name, &manifest, &modules, Some(&readme))
            .map(|_| ())
    })
}

/// Scaffolds a package and creates its governing lock row. This is the UI creation path.
///
/// # Errors
/// Returns an error if the package already exists or the files or lock can't be written.
pub fn scaffold_local_package_with_state(
    server_name: &str,
    name: &str,
    canonical_owner: &str,
) -> Result<()> {
    let (manifest, modules) = starter_package()?;
    let readme = format!("# {name}\n\nDescribe your package here.\n");
    fork_to_local_with_readme_and_state(
        server_name,
        name,
        &manifest,
        &modules,
        Some(&readme),
        canonical_owner,
    )
}

/// Forks a package's files into a NEW local package at `<server>/packages/<new_name>/` and
/// creates its governing lock row. The identity is the `new_name` folder (the manifest carries
/// no name). Generated `.d.ts` declaration files are skipped: publishing the fork regenerates
/// declarations for the same subpaths.
///
/// A copy that keeps a published package's leaf name inherits that package's activation, so it
/// takes over wherever the original ran; a differently named copy starts disabled.
///
/// # Errors
/// Returns an error if a package named `new_name` already exists, the manifest declares
/// `requires` entries (those need an exact requirement plan), or the files can't be written.
pub fn fork_to_local_with_readme_and_state(
    server_name: &str,
    new_name: &str,
    source_manifest: &PackageManifest,
    modules: &[LocalModule],
    readme: Option<&str>,
    canonical_owner: &str,
) -> Result<()> {
    if !source_manifest.smudgy_requires().is_empty() {
        bail!(
            "a copied package with requires entries needs an exact requirement plan before it can be created"
        );
    }
    with_local_package_transaction(server_name, |home, transaction| {
        checked_package_name(canonical_owner)?;
        let new_name = checked_package_name(new_name)?;
        validate_local_governing_rows(&transaction.load_lock()?, &[new_name.to_string()])?;
        // Settings left under this identity by an earlier package of the same name are adopted.
        publish_staged_package(
            home,
            server_name,
            new_name,
            source_manifest,
            modules,
            readme,
        )?;
        transaction.mutate_lock(|lock| {
            let inserted = materialize_local_governing_rows_in(lock, &[new_name.to_string()])?;
            Ok(((), inserted))
        })
    })
}

/// Requirement-aware form of [`fork_to_local_with_readme_and_state`]. Every required root must
/// already exist in `expected_lock`; creation compare-and-swaps that complete snapshot and writes
/// the local governing row plus all flattened `required_by` links in one lockfile replacement.
///
/// This API intentionally cannot install or upgrade requirements. The UI must resolve the copied
/// manifest first and obtain consent through the normal install flow for any changed root.
///
/// # Errors
/// Returns an error for an invalid plan or a failed write. A changed lockfile returns
/// [`Cas::StateChanged`] without creating anything.
#[allow(clippy::too_many_arguments)]
pub fn fork_to_local_with_readme_and_existing_requirements_if_unchanged(
    server_name: &str,
    new_name: &str,
    source_manifest: &PackageManifest,
    modules: &[LocalModule],
    readme: Option<&str>,
    canonical_owner: &str,
    expected_lock: &SharedPackageLock,
    required_specifiers: &[String],
) -> Result<Cas> {
    with_local_package_transaction(server_name, |home, transaction| {
        checked_package_name(canonical_owner)?;
        let new_name = checked_package_name(new_name)?;
        if transaction.load_lock()? != *expected_lock {
            return Ok(Cas::StateChanged);
        }
        let desired_lock = planned_local_create_lock(expected_lock, new_name, required_specifiers)?;
        // Settings left under this identity by an earlier package of the same name are adopted.
        publish_staged_package(
            home,
            server_name,
            new_name,
            source_manifest,
            modules,
            readme,
        )?;
        transaction.mutate_lock(|lock| {
            if *lock != *expected_lock {
                bail!("package state changed while the local copy was being created");
            }
            lock.clone_from(&desired_lock);
            Ok((Cas::Applied, true))
        })
    })
}

/// Builds the lockfile image a requirement-aware local copy commits: the new governing row plus
/// `required_by` links from every required root to it.
fn planned_local_create_lock(
    expected_lock: &SharedPackageLock,
    name: &str,
    required_specifiers: &[String],
) -> Result<SharedPackageLock> {
    let names = vec![name.to_string()];
    let mut desired = expected_lock.clone();
    if !materialize_local_governing_rows_in(&mut desired, &names)? {
        bail!("local package {name} already has governing state in the copy snapshot");
    }
    let root_specifier = local_state_specifier(name);
    if expected_lock
        .packages
        .iter()
        .any(|package| package.required_by.contains(&root_specifier))
    {
        bail!(
            "package state already contains requirement links for missing local package {root_specifier}"
        );
    }

    let mut required = std::collections::BTreeSet::new();
    for raw in required_specifiers {
        let parsed = SmudgySpecifier::parse(raw)
            .map_err(|error| anyhow!("invalid required package identity {raw}: {error}"))?;
        if parsed.subpath.is_some() || parsed.to_user_specifier() != *raw {
            bail!("required package identity {raw} must be a canonical package root");
        }
        if parsed.name.eq_ignore_ascii_case(name) {
            bail!("local package {name} cannot require its own leaf name");
        }
        if !required.insert(raw.to_ascii_lowercase()) {
            bail!("required package {raw} is listed more than once");
        }
        expected_lock
            .find(raw)
            .with_context(|| format!("required package {raw} is not installed"))?;
        // A local folder governs its leaf for every requested author. A shadowed published
        // fallback must never receive the new parent's link.
        if expected_lock
            .governing_specifier(raw)
            .is_none_or(|governing| governing != raw)
        {
            bail!("required package {raw} is not the governing package for its leaf");
        }
    }

    for package in &mut desired.packages {
        if required.contains(&package.specifier.to_ascii_lowercase()) {
            package.required_by.insert(root_specifier.clone());
            package.requirement_lineage_known = true;
        }
    }
    Ok(desired)
}

/// Writes a complete package folder to a temporary sibling of `packages/` and then renames it
/// into place, so package discovery never observes a partially written folder.
fn publish_staged_package(
    home: &Path,
    server_name: &str,
    new_name: &str,
    source_manifest: &PackageManifest,
    modules: &[LocalModule],
    readme: Option<&str>,
) -> Result<PathBuf> {
    let new_name = checked_package_name(new_name)?;
    for module in modules {
        checked_module_subpath(&module.subpath)?;
    }
    let packages = packages_dir_in(home, server_name);
    fs::create_dir_all(&packages).with_context(|| format!("create {}", packages.display()))?;
    let dir = packages.join(new_name);
    if fs::symlink_metadata(&dir).is_ok()
        || list_local_packages_in(home, server_name)?
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(new_name))
    {
        bail!("a package named {new_name} already exists");
    }
    // Stage beside `packages/`, not inside it: package discovery scans every child, and must
    // never observe the temporary directory while it is being populated.
    let staging_parent = packages
        .parent()
        .context("packages directory has no server parent")?;
    let staging = tempfile::Builder::new()
        .prefix(".smudgy-fork-")
        .tempdir_in(staging_parent)
        .with_context(|| format!("stage local package {new_name}"))?;
    let staged_dir = staging.path();

    let manifest_json =
        serde_json::to_string_pretty(source_manifest).context("serialize forked manifest")?;
    write_atomic(&staged_dir.join(MANIFEST_FILE), manifest_json.as_bytes())
        .with_context(|| format!("write {MANIFEST_FILE} for {new_name}"))?;
    if let Some(readme) = readme {
        write_atomic(&staged_dir.join(README_FILE), readme.as_bytes())
            .with_context(|| format!("write {README_FILE} for {new_name}"))?;
    }
    for module in modules {
        if is_declaration_file(&module.subpath) {
            continue;
        }
        let subpath = checked_module_subpath(&module.subpath)?;
        let path = staged_dir.join(subpath.replace('/', std::path::MAIN_SEPARATOR_STR));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        write_atomic(&path, &module.content)
            .with_context(|| format!("write {}", path.display()))?;
    }
    // A copied package is its own editor project: drop a thin `tsconfig.json` pointing at the
    // server-level smudgy project so VS Code types it. Written last so it wins over any stale
    // `tsconfig.json` a source might have shipped (it's excluded from publish either way).
    write_atomic(&staged_dir.join(TSCONFIG_FILE), PACKAGE_TSCONFIG.as_bytes())
        .with_context(|| format!("write editor configuration for {new_name}"))?;

    fs::rename(staged_dir, &dir)
        .with_context(|| format!("publish staged local package at {}", dir.display()))?;
    // The staging directory was moved; keep `tempfile` from trying to remove it.
    let _ = staging.keep();
    Ok(dir)
}

/// Deletes a local package: its governing lock row, its saved settings, and its folder.
///
/// # Errors
/// Returns an error if the package has an unfinished publication claim, its state changed while
/// the deletion ran, or the folder cannot be removed.
pub fn delete_local_package(server_name: &str, name: &str) -> Result<DeleteLocalPackageSummary> {
    with_local_package_transaction(server_name, |home, transaction| {
        let name = checked_package_name(name)?;
        if load_publication_claim_in(home, server_name, name)?.is_some() {
            bail!(
                "local package {name} has an unfinished publication claim; retry publishing before deleting it"
            );
        }
        let dir = require_local_package_dir_in(home, server_name, name)?;
        let state_specifier = local_state_specifier(name);
        let lock = transaction.load_lock()?;
        validate_local_governing_rows(&lock, &[name.to_string()])?;
        let mut warnings = Vec::new();
        if let Some(expected) = lock.find(&state_specifier).cloned() {
            if !transaction.remove_lock_entry_if_unchanged(&expected)? {
                bail!(
                    "local package {name} changed while it was being deleted; no changes were committed"
                );
            }
        }
        if let Err(error) = transaction.remove_package_param_state(&state_specifier) {
            warnings.push(format!("saved settings cleanup is incomplete: {error:#}"));
        }
        fs::remove_dir_all(&dir).with_context(|| format!("remove {}", dir.display()))?;
        Ok(DeleteLocalPackageSummary { warnings })
    })
}

/// Renames a local package folder (`old` → `new`) under `<server>/packages/` together with its
/// governing state. The manifest carries no name, so the folder name *is* the identity.
/// Rejects a target that already exists or any package that is bound to a published cloud
/// namespace; a no-op rename returns `false`.
///
/// # Errors
/// Returns an error if `old` doesn't exist, `new` already exists, the package is published, or
/// the rename fails.
pub fn rename_local_package(server_name: &str, old: &str, new: &str) -> Result<bool> {
    with_local_package_transaction(server_name, |home, transaction| {
        let old = checked_package_name(old)?;
        let new = checked_package_name(new)?;
        if old == new {
            return Ok(false);
        }
        if load_publication_claim_in(home, server_name, old)?.is_some() {
            bail!(
                "local package {old} has an unfinished publication claim; retry publishing before renaming it"
            );
        }
        if load_publication_binding_in(home, server_name, old)?.is_some() {
            bail!("published package {old} cannot be renamed; make a copy with a new name instead");
        }
        transaction.rename_local_package_state(old, new)
    })
}

// ---------------------------------------------------------------------------
// Publishing
// ---------------------------------------------------------------------------

/// The media type to publish a module subpath as. Text/code types get a real type; anything
/// unrecognized is `application/octet-stream` so binaries publish faithfully.
fn media_type_for(subpath: &str) -> &'static str {
    let ext = Path::new(subpath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "ts" | "tsx" | "mts" | "cts" => "application/typescript",
        "js" | "jsx" | "mjs" | "cjs" => "application/javascript",
        "json" => "application/json",
        "wasm" => "application/wasm",
        "css" => "text/css",
        "html" | "htm" => "text/html",
        "wgsl" | "glsl" | "vert" | "frag" | "txt" | "md" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "mp3" => "audio/mpeg",
        _ => "application/octet-stream",
    }
}

fn ensure_local_snapshot_matches(server_name: &str, name: &str, expected: &str) -> Result<()> {
    with_local_package_transaction(server_name, |home, _| {
        ensure_local_snapshot_matches_in(home, server_name, name, expected)
    })
}

fn ensure_local_snapshot_matches_in(
    home: &Path,
    server_name: &str,
    name: &str,
    expected: &str,
) -> Result<()> {
    let current = load_local_package_in(home, server_name, name)?
        .ok_or_else(|| anyhow!("local package {name} was removed while preparing to publish"))?;
    if package_snapshot_digest(&current)? != expected {
        bail!("local package {name} changed while preparing to publish; review it and try again");
    }
    Ok(())
}

fn published_module_fingerprint(module: &PublishModule) -> (String, String, String, i64, bool) {
    (
        module.subpath.clone(),
        format!("{:x}", Sha256::digest(&module.content)),
        module.media_type.clone(),
        i64::try_from(module.content.len()).unwrap_or(i64::MAX),
        module.is_entry,
    )
}

/// Compare authored content with one immutable published version. Dependency declarations live
/// in the manifest; newer remote dependency releases are not local edits. Generated declarations
/// are reconstructed only when the remote has extra files, so they cannot hide deleted assets or
/// hand-authored declarations. This performs no uploads and does not change the local package.
///
/// # Errors
/// Returns an error when the manifest cannot be decoded or declaration generation cannot confirm
/// the remaining remote files.
pub async fn matches_published_content(
    package: &LocalPackage,
    resolved: &ResolvedPackageWire,
) -> Result<bool> {
    let remote_manifest = PackageManifest::parse(&resolved.manifest.to_string())?;
    if !crate::models::naming::names_conflict(&package.name, &resolved.name)
        || package.manifest != remote_manifest
        || package.manifest.version != resolved.version
        || package.readme != resolved.readme
    {
        return Ok(false);
    }
    let entry = package.manifest.entry.as_deref().unwrap_or("index.ts");
    let authored: Vec<_> = package
        .modules
        .iter()
        .map(|module| PublishModule {
            subpath: module.subpath.clone(),
            content: module.content.clone(),
            media_type: media_type_for(&module.subpath).to_string(),
            is_entry: module.subpath == entry,
        })
        .collect();
    let remote: BTreeMap<_, _> = resolved
        .modules
        .iter()
        .map(|module| {
            (
                module.subpath.as_str(),
                (
                    module.subpath.clone(),
                    module
                        .content_hash
                        .trim_start_matches("sha256-")
                        .to_ascii_lowercase(),
                    module.media_type.clone(),
                    module.byte_size,
                    module.is_entry,
                ),
            )
        })
        .collect();
    if remote.len() != resolved.modules.len()
        || authored.iter().any(|module| {
            remote.get(module.subpath.as_str()) != Some(&published_module_fingerprint(module))
        })
    {
        return Ok(false);
    }
    if authored.len() == remote.len() {
        return Ok(true);
    }
    if remote.keys().any(|subpath| {
        !is_declaration_file(subpath) && !authored.iter().any(|module| module.subpath == *subpath)
    }) {
        return Ok(false);
    }
    let (generated, warnings) = generate_publish_typings(&package.modules).await;
    let modules = merge_published_modules(authored, generated);
    let matches = modules.len() == remote.len()
        && modules.iter().all(|module| {
            remote.get(module.subpath.as_str()) == Some(&published_module_fingerprint(module))
        });
    if !matches && !warnings.is_empty() {
        bail!(
            "could not verify published declarations: {}",
            warnings.join("; ")
        );
    }
    Ok(matches)
}

struct PublishRequestSnapshot<'a> {
    package_id: Uuid,
    owner_nickname: &'a str,
    package_name: &'a str,
    version: &'a str,
    manifest: &'a serde_json::Value,
    modules: &'a [PublishModule],
    dependencies: &'a [PublishDependency],
    readme: Option<&'a str>,
}

/// Confirms the immutable version state, not merely that its number is occupied. This is the
/// recovery boundary for a lost finalize response: only an exact match makes retrying unsafe.
fn resolved_matches_publish_request(
    resolved: &ResolvedPackageWire,
    request: &PublishRequestSnapshot<'_>,
) -> bool {
    if resolved.package_id != request.package_id
        || !crate::models::naming::names_conflict(&resolved.owner_nickname, request.owner_nickname)
        || !crate::models::naming::names_conflict(&resolved.name, request.package_name)
        || semver::Version::parse(&resolved.version).ok()
            != semver::Version::parse(request.version).ok()
        || &resolved.manifest != request.manifest
        || resolved.readme.as_deref() != request.readme
    {
        return false;
    }

    let mut expected_modules = request
        .modules
        .iter()
        .map(published_module_fingerprint)
        .collect::<Vec<_>>();
    expected_modules.sort();
    let mut actual_modules = resolved
        .modules
        .iter()
        .map(|module| {
            (
                module.subpath.clone(),
                module
                    .content_hash
                    .trim_start_matches("sha256-")
                    .to_ascii_lowercase(),
                module.media_type.clone(),
                module.byte_size,
                module.is_entry,
            )
        })
        .collect::<Vec<_>>();
    actual_modules.sort();
    if actual_modules != expected_modules {
        return false;
    }

    if resolved
        .dependencies
        .iter()
        .any(|dependency| dependency.kind != DependencyKind::Dependency)
    {
        return false;
    }
    let mut expected_dependencies = request
        .dependencies
        .iter()
        .map(|dependency| {
            (
                dependency.owner_nickname.to_lowercase(),
                dependency.name.to_lowercase(),
                dependency.range.clone(),
                dependency.resolved_version.clone(),
            )
        })
        .collect::<Vec<_>>();
    expected_dependencies.sort();
    let mut actual_dependencies = resolved
        .dependencies
        .iter()
        .map(|dependency| {
            (
                dependency.owner_nickname.to_lowercase(),
                dependency.name.to_lowercase(),
                dependency.range.clone(),
                dependency.resolved_version.clone(),
            )
        })
        .collect::<Vec<_>>();
    actual_dependencies.sort();
    actual_dependencies == expected_dependencies
}

async fn confirm_ambiguous_publish(
    client: &PackageApiClient,
    package_id: Uuid,
    publisher_nickname: &str,
    package_name: &str,
    target_version: &str,
    request: &PublishRequestSnapshot<'_>,
    ambiguity: &str,
) -> Result<(String, DateTime<Utc>)> {
    let versions = client.list_versions(package_id).await.map_err(|confirm_error| {
        anyhow!(
            "{ambiguity}; Smudgy could not confirm the remote result: {confirm_error}. Check the package on the server before you retry"
        )
    })?;
    let Some(confirmed) = versions.into_iter().find(|version| {
        !version.deleted
            && semver::Version::parse(&version.version).ok().as_ref()
                == semver::Version::parse(target_version).ok().as_ref()
    }) else {
        return Err(anyhow!(ambiguity.to_string()));
    };
    let resolved = client
        .resolve_package(publisher_nickname, package_name, Some(target_version))
        .await
        .map_err(|confirm_error| {
            anyhow!(
                "{ambiguity}; the version number is now used, but Smudgy could not verify its content: {confirm_error}. Check the package on the server before you retry"
            )
        })?;
    if !resolved_matches_publish_request(&resolved, request) {
        bail!(
            "package version {target_version} is now used, but its content does not match this publish request; review the server version and choose a new version number"
        );
    }
    Ok((confirmed.version, confirmed.published_at))
}

/// Publish a local package: create-or-get the caller's namespace, then publish an
/// immutable version from the folder. Bump the manifest `version` to ship an update.
///
/// The service enforces one published namespace for each package leaf name. A copied package must
/// therefore be renamed before publish when that leaf is already owned by another account; the
/// backend remains authoritative and returns a conflict if the name is unavailable.
///
/// # Errors
/// Returns an error if the package is missing/invalid, or the backend rejects the publish (e.g. a
/// duplicate version → 409).
pub async fn publish_local_package(
    client: &PackageApiClient,
    server_name: &str,
    name: &str,
    publisher: &UserProfile,
) -> Result<PublishSummary> {
    if publisher.id.is_nil() {
        bail!("the signed-in account has no stable identity; sign in again");
    }
    let publisher_nickname = publisher
        .nickname
        .as_deref()
        .context("choose an account nickname before publishing a local package")?;
    checked_package_name(publisher_nickname)
        .context("the signed-in account nickname is invalid")?;
    let publisher_nickname = publisher_nickname.to_string();

    let package = load_local_package(server_name, name)?
        .ok_or_else(|| anyhow!("no local package named {name}"))?;
    let snapshot_digest = package_snapshot_digest(&package)?;
    let entry = package
        .manifest
        .entry
        .clone()
        .unwrap_or_else(|| "index.ts".to_string());

    let authored: Vec<PublishModule> = package
        .modules
        .iter()
        .map(|m| PublishModule {
            subpath: m.subpath.clone(),
            content: m.content.clone(),
            media_type: media_type_for(&m.subpath).to_string(),
            is_entry: m.subpath == entry,
        })
        .collect();
    // Image assets are the first routinely-large modules a package ships. Warn (don't
    // block — the server enforces its own caps) about anything past the loader's 32 MiB
    // per-image cap: it would publish fine and then never display.
    for module in &authored {
        const IMAGE_LOAD_CAP: usize = 32 * 1024 * 1024;
        if module.content.len() > IMAGE_LOAD_CAP {
            warn!(
                "publishing {}: {} is {} bytes — larger than the {IMAGE_LOAD_CAP}-byte cap image loading enforces, so <Image> will refuse it",
                package.name,
                module.subpath,
                module.content.len()
            );
        }
    }
    let authored_count = authored.len();

    // Publish-time TypeScript declarations (best-effort — a package always publishes even
    // if typings fail). The generated `.d.ts` ride as ordinary, non-entry modules.
    let (dts_modules, typings_warnings) = generate_publish_typings(&package.modules).await;
    let modules = merge_published_modules(authored, dts_modules);
    let typings_generated = modules.len() - authored_count;

    let manifest_value =
        serde_json::to_value(&package.manifest).context("serialize package manifest")?;

    let (dependencies, dependency_warnings) = lock_dependencies(client, &package.manifest).await?;
    let locked_dependencies = dependencies
        .iter()
        .map(|d| {
            (
                format!("smudgy://{}/{}", d.owner_nickname, d.name),
                d.resolved_version.clone(),
            )
        })
        .collect();

    // Interop-declaration validation + rename diff (interop.md §4) — best-effort and never
    // fatal, like typings.
    let interop_warnings =
        interop_publish_warnings(client, &package, &entry, &publisher_nickname).await;

    // Everything that can fail without changing cloud state has now completed. Re-read once before
    // claiming or mutating a namespace so an external edit cannot make this upload a mixed or stale
    // snapshot under a newly bound folder. No lock is held across a cloud await.
    let (publication_binding, publication_claim) = prepare_publication_namespace_state(
        server_name,
        name,
        &snapshot_digest,
        publisher.id,
        &publisher_nickname,
    )?;

    // The published namespace name is the folder name (`package.name`); the manifest carries no
    // competing identity. A binding is authoritative. Without one, the claim sidecar lets
    // create-or-get be repeated after any ambiguous response or binding-write failure without
    // making the claimed folder appear renameable.
    let view = if let Some(binding) = &publication_binding {
        let detail = client
            .get_package(binding.package_id)
            .await
            .map_err(|e| anyhow!("load bound package namespace: {e}"))?;
        if !detail.viewer_can_admin {
            bail!(
                "local package {} is bound to a namespace that this account cannot publish",
                package.name
            );
        }
        if detail.package.owner_id != publisher.id
            || detail.package.id != binding.package_id
            || !crate::models::naming::names_conflict(&detail.package.name, &binding.leaf)
            || !crate::models::naming::names_conflict(&package.name, &binding.leaf)
        {
            bail!(
                "the publication binding for local package {} does not match its cloud namespace",
                package.name
            );
        }
        if let Some(intent) = &publication_claim {
            validate_claimed_namespace(intent, &detail.package)?;
            if let Err(error) = clear_publication_claim_if_unchanged(server_name, name, intent) {
                log::warn!(
                    "The completed publication binding for {} has a redundant claim sidecar that could not be removed: {error:#}",
                    package.name
                );
            }
        }
        detail.package
    } else {
        let intent = publication_claim
            .as_ref()
            .context("first publish has no namespace-claim intent")?;
        let view = match client
            .create_package(&package.name, &package.manifest.description)
            .await
        {
            Ok(view) => view,
            Err(error) => {
                // A name-unavailable response proves this account did not claim the leaf. Other
                // failures can be ambiguous (including a committed response that could not be
                // decoded), so retain the intent and require an idempotent retry.
                if matches!(&error, smudgy_cloud::CloudError::NameUnavailable(_)) {
                    clear_publication_claim_if_unchanged(server_name, name, intent).context(
                        "the package name is unavailable, and its local claim could not be cleared",
                    )?;
                }
                return Err(anyhow!("create package namespace: {error}"));
            }
        };
        validate_claimed_namespace(intent, &view)?;
        commit_publication_binding(server_name, &package.name, view.id, &view.name, intent)
            .context("save claimed package namespace")?;
        view
    };

    let target_version = semver::Version::parse(&package.manifest.version)?.to_string();
    // The README is part of the same initial snapshot as the manifest and modules.
    let readme = package.readme.as_deref();
    let request = PublishRequestSnapshot {
        package_id: view.id,
        owner_nickname: &publisher_nickname,
        package_name: &package.name,
        version: &target_version,
        manifest: &manifest_value,
        modules: &modules,
        dependencies: &dependencies,
        readme,
    };
    let versions_before = client
        .list_versions(view.id)
        .await
        .map_err(|e| anyhow!("check published package versions: {e}"))?;
    let existing_version = versions_before.into_iter().find(|version| {
        semver::Version::parse(&version.version).ok().as_ref()
            == semver::Version::parse(&target_version).ok().as_ref()
    });

    let mut publication_warnings = Vec::new();
    let (published_version, published_at) = if let Some(existing) = existing_version {
        if existing.deleted {
            bail!("package version {target_version} was already published and deleted");
        }
        let resolved = client
            .resolve_package(
                &publisher_nickname,
                &package.name,
                Some(&target_version),
            )
            .await
            .map_err(|error| {
                anyhow!(
                    "package version {target_version} is already present, but Smudgy could not verify its content: {error}. Check the package on the server before you retry"
                )
            })?;
        if !resolved_matches_publish_request(&resolved, &request) {
            bail!(
                "package version {target_version} is already used, but its content does not match this publish request; review the server version and choose a new version number"
            );
        }
        publication_warnings.push(PublicationWarning::ExistingVersionRecovered {
            name: package.name.clone(),
            version: existing.version.clone(),
        });
        (existing.version, existing.published_at)
    } else {
        let publish_result = client
            .publish_version_checked(
                view.id,
                &target_version,
                &manifest_value,
                &modules,
                &dependencies,
                readme,
                || {
                    ensure_local_snapshot_matches(server_name, name, &snapshot_digest).map_err(
                        |error| {
                            smudgy_cloud::CloudError::InvalidInput(format!(
                                "the local package changed before the final publish step; no version was published: {error:#}"
                            ))
                        },
                    )
                },
            )
            .await;

        match publish_result {
            Ok(committed)
                if committed.package_id == view.id && committed.version == target_version =>
            {
                (committed.version, committed.published_at)
            }
            Ok(committed) => {
                // A malformed success body is no safer than a dropped response: finalize may have
                // committed, but its body cannot identify what committed. Verify the intended
                // immutable target independently before reporting success or allowing a retry.
                let ambiguity = format!(
                    "publish version {target_version}: the server returned an inconsistent success response (namespace {}, version {})",
                    committed.package_id, committed.version
                );
                let confirmed = confirm_ambiguous_publish(
                    client,
                    view.id,
                    &publisher_nickname,
                    &package.name,
                    &target_version,
                    &request,
                    &ambiguity,
                )
                .await?;
                publication_warnings.push(PublicationWarning::InconsistentResponseRecovered {
                    name: package.name.clone(),
                    version: confirmed.0.clone(),
                });
                confirmed
            }
            Err(error) => {
                // Finalize is an irreversible remote commit. A dropped response is ambiguous. A
                // used number alone is not proof: another authorized request can win the race.
                // Treat this publish as complete only when the resolved immutable payload is an
                // exact match.
                let ambiguity = format!("publish version {target_version}: {error}");
                let confirmed = confirm_ambiguous_publish(
                    client,
                    view.id,
                    &publisher_nickname,
                    &package.name,
                    &target_version,
                    &request,
                    &ambiguity,
                )
                .await?;
                publication_warnings.push(PublicationWarning::VersionPresentAfterLostResponse {
                    name: package.name.clone(),
                    version: confirmed.0.clone(),
                });
                confirmed
            }
        }
    };

    // Package metadata is mutable, but the version upload above is irreversible. Update the
    // description only after the version is confirmed live so a metadata failure cannot make an
    // unpublished version appear to have been published. Conversely, once the version is live,
    // report a failed metadata update as recovery work and never return an error that invites the
    // author to retry the immutable version number.
    if view.description != package.manifest.description {
        if let Err(error) = client
            .patch_package(view.id, Some(&package.manifest.description), None)
            .await
        {
            warn!(
                "published {}@{}, but its package description could not be updated: {error}",
                package.name, published_version
            );
            publication_warnings.push(PublicationWarning::DescriptionUpdateFailed {
                name: package.name.clone(),
                version: published_version.clone(),
                error: error.to_string(),
            });
        }
    }

    // The upload used the snapshot captured above. A change after the final precondition check can
    // race the remote request, so make that committed-but-stale result prominent and never report
    // a failure that invites retrying the immutable version number.
    let local_snapshot_matches = load_local_package(server_name, name)
        .and_then(|current| {
            current
                .map(|current| package_snapshot_digest(&current))
                .transpose()
        })
        .ok()
        .flatten()
        .as_deref()
        == Some(&snapshot_digest);
    if !local_snapshot_matches {
        warn!(
            "published {}@{}, but the local package changed before completion",
            package.name, published_version
        );
        publication_warnings.push(PublicationWarning::LocalSnapshotChanged {
            name: package.name.clone(),
            version: published_version.clone(),
        });
    }

    // Namespace binding failures after a committed version are recovery work, not publish
    // failures. Preserve the success result and tell the author not to retry the used number.
    match load_publication_binding(server_name, name) {
        Ok(Some(binding))
            if binding.package_id == view.id
                && crate::models::naming::names_conflict(&binding.leaf, &package.name) => {}
        Ok(Some(_)) => {
            warn!(
                "published {}@{}, but its local publication binding no longer matches",
                package.name, published_version
            );
            publication_warnings.push(PublicationWarning::LocalBindingUnverified {
                name: package.name.clone(),
                version: published_version.clone(),
                error: "the local publication link does not match the published package"
                    .to_string(),
            });
        }
        Ok(None) => {
            publication_warnings.push(PublicationWarning::MissingLocalBinding {
                name: package.name.clone(),
                version: published_version.clone(),
            });
        }
        Err(error) => {
            warn!(
                "published {}@{}, but its local publication binding could not be verified: {error:#}",
                package.name, published_version
            );
            publication_warnings.push(PublicationWarning::LocalBindingUnverified {
                name: package.name.clone(),
                version: published_version.clone(),
                error: format!("{error:#}"),
            });
        }
    }

    Ok(PublishSummary {
        package_id: view.id,
        is_public: view.is_public,
        version: published_version,
        published_at,
        typings_generated,
        typings_warnings,
        locked_dependencies,
        dependency_warnings,
        interop_warnings,
        publication_warnings,
    })
}

/// Interop-declaration publish validation (interop.md §4). Two sources, both best-effort
/// and never fatal:
/// - the entry's own export-shape problems (duplicate names, aliased exports), promoted
///   from boot-time logs to author-visible publish warnings;
/// - a diff of the declared handle set against the *currently published* version: a handle
///   name is the identity consumers import and persistence keys off, so one that vanishes
///   (usually an innocent const rename under name inference) gets a warning naming the
///   pinning fix. First publish, offline, or a logged-out account skip the diff silently.
async fn interop_publish_warnings(
    client: &PackageApiClient,
    package: &LocalPackage,
    entry: &str,
    publisher_nickname: &str,
) -> Vec<String> {
    use smudgy_script::interop_extract::{extract_interop_handles, fold_interop_name};

    let mut warnings = Vec::new();
    let Some(entry_module) = package.modules.iter().find(|m| m.subpath == entry) else {
        return warnings;
    };
    let Ok(url) = deno_core::ModuleSpecifier::parse(&format!("file:///{entry}")) else {
        return warnings;
    };
    let Ok(entry_text) = std::str::from_utf8(&entry_module.content) else {
        return warnings;
    };
    let Ok(extraction) = extract_interop_handles(&url, entry_text) else {
        // A parse error fails the publish elsewhere; not this check's job to report.
        return warnings;
    };
    if !extraction.duplicates.is_empty() {
        warnings.push(format!(
            "duplicate interop handle name(s): {} (first declaration wins)",
            extraction.duplicates.join(", ")
        ));
    }
    warnings.extend(extraction.export_diagnostics.iter().cloned());

    // Rename diff vs the published latest. Use the immutable profile captured beside this
    // operation's detached credential; a sign-in change must not switch authors between awaits.
    let Ok(previous) = client
        .resolve_package(publisher_nickname, &package.name, None)
        .await
    else {
        return warnings;
    };
    let prev_entry = previous
        .manifest
        .get("entry")
        .and_then(|v| v.as_str())
        .unwrap_or("index.ts")
        .to_string();
    let Some(prev_module) = previous.modules.iter().find(|m| m.subpath == prev_entry) else {
        return warnings;
    };
    let Ok(prev_text) = client
        .fetch_module_body(&prev_module.content_url, &prev_module.content_hash)
        .await
    else {
        return warnings;
    };
    let Ok(prev_url) = deno_core::ModuleSpecifier::parse(&format!("file:///{prev_entry}")) else {
        return warnings;
    };
    let Ok(prev_extraction) = extract_interop_handles(&prev_url, &prev_text) else {
        return warnings;
    };
    let current: std::collections::HashSet<_> = extraction
        .handles
        .iter()
        .map(|h| (h.kind, fold_interop_name(&h.name)))
        .collect();
    for prev in &prev_extraction.handles {
        if !current.contains(&(prev.kind, fold_interop_name(&prev.name))) {
            warnings.push(format!(
                "v{} published the {} handle {:?}, which this version drops — consumers importing it will break. If this is a rename, keep the identity by passing the old name explicitly (e.g. create…({:?}, …))",
                previous.version,
                prev.kind.as_str(),
                prev.name,
                prev.name
            ));
        }
    }
    warnings
}

/// Whether `subpath` names a TypeScript declaration file (`*.d.ts`, `*.d.mts`, `*.d.cts`).
/// A declaration file has a TS-family extension and a stem ending in `.d` (e.g. `index.d.ts`
/// → extension `ts`, stem `index.d`).
fn is_declaration_file(subpath: &str) -> bool {
    let path = Path::new(subpath);
    let is_ts = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            ["ts", "mts", "cts"]
                .iter()
                .any(|t| ext.eq_ignore_ascii_case(t))
        });
    let stem_is_decl = path
        .file_stem()
        .map(Path::new)
        .and_then(|stem| stem.extension())
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("d"));
    is_ts && stem_is_decl
}

/// Whether `subpath` is a TypeScript *source* file the declaration generator should
/// compile (a `.d.ts` is already a declaration). `.tsx` IS compiled — the generator emits
/// via the automatic JSX runtime against the `smudgy:widgets/jsx-runtime` ambient.
fn is_typescript_source(subpath: &str) -> bool {
    let path = Path::new(subpath);
    let is_ts = path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            ["ts", "mts", "cts", "tsx"]
                .iter()
                .any(|t| ext.eq_ignore_ascii_case(t))
        });
    is_ts && !is_declaration_file(subpath)
}

/// Combine a package's own modules with the publish-time generated declarations, with the
/// author's files winning on any subpath collision. A generated `.d.ts` is dropped when the
/// package already ships a module at that subpath — a hand-authored declaration the author
/// shipped deliberately — so the published payload never carries two modules with the same
/// subpath (which the server rejects, cancelling the publish).
fn merge_published_modules(
    authored: Vec<PublishModule>,
    generated: Vec<PublishModule>,
) -> Vec<PublishModule> {
    let authored_subpaths: std::collections::HashSet<String> =
        authored.iter().map(|m| m.subpath.clone()).collect();
    let mut merged = authored;
    merged.extend(
        generated
            .into_iter()
            .filter(|m| !authored_subpaths.contains(&m.subpath)),
    );
    merged
}

/// Generate `.d.ts` for a package's TypeScript modules at publish time. **Best-effort and
/// never fatal**: returns the declaration modules to ship plus any warnings (tsc
/// diagnostics, or a generation failure) to surface to the author. The (blocking,
/// isolate-constructing) compiler runs on its own thread via `spawn_blocking`, off both
/// the UI loop and any live session isolates.
async fn generate_publish_typings(modules: &[LocalModule]) -> (Vec<PublishModule>, Vec<String>) {
    let mut sources = BTreeMap::new();
    let mut has_typescript = false;
    for module in modules {
        let is_ts = is_typescript_source(&module.subpath);
        // `.json` data modules join the compile VFS so an
        // `import x from "./data.json" with { type: "json" }` resolves (resolveJsonModule);
        // tsc emits no declarations for the JSON itself — only for the TS that imports it.
        let is_json = module.subpath.ends_with(".json");
        if !is_ts && !is_json {
            continue;
        }
        if let Ok(text) = std::str::from_utf8(&module.content) {
            has_typescript |= is_ts;
            sources.insert(module.subpath.clone(), text.to_string());
        }
    }
    if !has_typescript {
        return (Vec::new(), Vec::new());
    }

    let mut ambient = BTreeMap::new();
    ambient.insert("smudgy-core.d.ts".to_string(), SMUDGY_CORE_DTS.to_string());
    ambient.insert(
        "smudgy-mapper.d.ts".to_string(),
        SMUDGY_MAPPER_DTS.to_string(),
    );
    ambient.insert(
        "smudgy-widgets.d.ts".to_string(),
        SMUDGY_WIDGETS_DTS.to_string(),
    );
    ambient.insert(
        "smudgy-params.d.ts".to_string(),
        SMUDGY_PARAMS_DTS.to_string(),
    );

    match tokio::task::spawn_blocking(move || {
        smudgy_script::dts::generate_declarations(&sources, &ambient)
    })
    .await
    {
        Ok(Ok(generated)) => {
            let warnings = generated.diagnostics;
            let dts_modules = generated
                .files
                .into_iter()
                .map(|(subpath, content)| PublishModule {
                    media_type: media_type_for(&subpath).to_string(),
                    is_entry: false,
                    subpath,
                    content: content.into_bytes(),
                })
                .collect();
            (dts_modules, warnings)
        }
        Ok(Err(e)) => (
            Vec::new(),
            vec![format!(
                "declaration generation failed — publishing without typings: {e:#}"
            )],
        ),
        Err(e) => (
            Vec::new(),
            vec![format!(
                "declaration generation panicked — publishing without typings: {e}"
            )],
        ),
    }
}

/// Resolve each declared `smudgy://` dependency range to the concrete highest published
/// version that satisfies it, recording `{specifier, range, resolved_version}`. Installers
/// reproduce this exact dependency set, and the resolution engine dedupes/coexists
/// packages by what each dependent locked. A range with no published match is a publish
/// error naming the dependency.
///
/// Returns the locked dependency set plus any non-fatal **warnings**: a declared range that
/// resolves to an *older* version than the latest published one (so the publish silently freezes
/// a back-level dependency). The most common trap is the 0.0.x caret footgun — under Cargo semver
/// `^0.0.1` and bare `0.0.1` mean `>=0.0.1, <0.0.2`, so they can never advance to `0.0.2`.
async fn lock_dependencies(
    client: &PackageApiClient,
    manifest: &PackageManifest,
) -> Result<(Vec<PublishDependency>, Vec<String>)> {
    let mut dependencies = Vec::new();
    let mut warnings = Vec::new();
    for dep in manifest.smudgy_dependencies() {
        let owner_nickname = dep.key.owner.clone();
        let name = dep.key.name;
        // A range-less dependency means "any version" (`*`); that's also what we record.
        let range = dep.range.unwrap_or_else(|| "*".to_string());

        let resolved = client
            .resolve_package(&owner_nickname, &name, None)
            .await
            .map_err(|e| anyhow!("lock dependency {owner_nickname}/{name}: {e}"))?;
        let versions = client
            .list_versions(resolved.package_id)
            .await
            .map_err(|e| anyhow!("lock dependency {owner_nickname}/{name}: {e}"))?;
        let resolved_version = highest_satisfying_version(&versions, Some(&range))
            .map_err(|e| {
                anyhow!("dependency {owner_nickname}/{name} has an invalid range {range}: {e}")
            })?
            .ok_or_else(|| {
                anyhow!("no published version of {owner_nickname}/{name} satisfies {range}")
            })?;

        // The highest published version overall is always >= the highest *within* the range (the
        // range is a subset), so if they differ the range is excluding a strictly-newer release.
        // Surface that — it's the silent "bundled an old version" trap — but never block the publish.
        if let Ok(Some(latest)) = highest_satisfying_version(&versions, None)
            && latest != resolved_version
        {
            let hint = if excludes_zero_zero_patch(&range, &resolved_version) {
                " — a caret/bare requirement on a 0.0.x version pins to that exact patch and never \
                 advances; widen it (e.g. \"*\" or \">=0.0.x\")"
            } else {
                " — widen the range or re-publish to pick it up"
            };
            warnings.push(format!(
                "dependency {owner_nickname}/{name}: locked v{resolved_version}, but v{latest} is \
                 published and your range \"{range}\" excludes it{hint}"
            ));
        }

        dependencies.push(PublishDependency {
            owner_nickname,
            name,
            range,
            resolved_version,
        });
    }
    Ok((dependencies, warnings))
}

/// Whether `range` is a caret-or-bare requirement on a `0.0.x` version (which Cargo semver pins to
/// that exact patch, never advancing) and `resolved` is itself a `0.0.x` version — the precise
/// shape of the silent "can't move past 0.0.x" footgun. A tilde (`~0.0.1`) or comparator
/// (`>=0.0.1`) range is *not* flagged: those do admit higher 0.0.x patches.
fn excludes_zero_zero_patch(range: &str, resolved: &str) -> bool {
    let core = range
        .trim()
        .strip_prefix('^')
        .unwrap_or_else(|| range.trim());
    core.starts_with("0.0.") && resolved.starts_with("0.0.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::profile_activation::ProfileActivation;
    use crate::models::shared_packages::{UpdateMode, install_package, load_lock};

    fn published_fixture(package: &LocalPackage) -> ResolvedPackageWire {
        ResolvedPackageWire {
            package_id: Uuid::new_v4(),
            owner_nickname: "author".into(),
            name: package.name.clone(),
            version: package.manifest.version.clone(),
            manifest: serde_json::to_value(&package.manifest).unwrap(),
            is_public: false,
            aligned_hosts: Vec::new(),
            readme: package.readme.clone(),
            modules: package
                .modules
                .iter()
                .map(|module| smudgy_cloud::ResolvedModuleWire {
                    subpath: module.subpath.clone(),
                    content_hash: format!("{:x}", Sha256::digest(&module.content)),
                    media_type: media_type_for(&module.subpath).into(),
                    byte_size: i64::try_from(module.content.len()).unwrap(),
                    is_entry: module.subpath
                        == package.manifest.entry.as_deref().unwrap_or("index.ts"),
                    content_url: "https://example.invalid/content".into(),
                })
                .collect(),
            dependencies: Vec::new(),
        }
    }

    #[tokio::test]
    async fn sharing_content_detects_authored_changes() {
        let package = LocalPackage {
            name: "tools".into(),
            manifest: PackageManifest::parse(r#"{"version":"0.1.0","entry":"index.js"}"#).unwrap(),
            readme: Some("# Tools".into()),
            modules: vec![
                LocalModule {
                    subpath: "index.js".into(),
                    content: b"export const x = 1;".to_vec(),
                },
                LocalModule {
                    subpath: "image.png".into(),
                    content: vec![0, 255, 8],
                },
            ],
        };
        let remote = published_fixture(&package);
        assert!(matches_published_content(&package, &remote).await.unwrap());
        let mut prefixed = remote.clone();
        prefixed.modules.reverse();
        for module in &mut prefixed.modules {
            module.content_hash = format!("sha256-{}", module.content_hash.to_uppercase());
        }
        assert!(
            matches_published_content(&package, &prefixed)
                .await
                .unwrap()
        );
        for change in 0..7 {
            let mut changed = package.clone();
            match change {
                0 => changed.modules[0].content.push(b' '),
                1 => changed.modules[1].content.push(0),
                2 => {
                    changed.modules.pop();
                }
                3 => changed.modules.push(LocalModule {
                    subpath: "new.txt".into(),
                    content: vec![1],
                }),
                4 => changed.readme = None,
                5 => changed.manifest.description = "new description".into(),
                _ => changed
                    .manifest
                    .dependencies
                    .push("smudgy://author/lib@^1.0.0".into()),
            }
            assert!(
                !matches_published_content(&changed, &remote).await.unwrap(),
                "change {change}"
            );
        }
        let mut unknown_remote = remote.clone();
        unknown_remote.manifest = serde_json::json!({"version": false});
        assert!(
            matches_published_content(&package, &unknown_remote)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn sharing_content_accounts_for_generated_declarations() {
        let package = LocalPackage {
            name: "tools".into(),
            manifest: PackageManifest::parse(r#"{"version":"0.1.0"}"#).unwrap(),
            readme: None,
            modules: vec![LocalModule {
                subpath: "index.ts".into(),
                content: b"export const answer: number = 42;".to_vec(),
            }],
        };
        let (generated, warnings) = generate_publish_typings(&package.modules).await;
        assert!(!generated.is_empty(), "{warnings:?}");
        let mut published = package.clone();
        published
            .modules
            .extend(generated.into_iter().map(|module| LocalModule {
                subpath: module.subpath,
                content: module.content,
            }));
        let remote = published_fixture(&published);
        assert!(matches_published_content(&package, &remote).await.unwrap());
        let mut authored = published;
        authored.modules[1]
            .content
            .extend_from_slice(b"\n// authored declaration\n");
        let authored_remote = published_fixture(&authored);
        assert!(
            matches_published_content(&authored, &authored_remote)
                .await
                .unwrap()
        );
        assert!(
            !matches_published_content(&package, &authored_remote)
                .await
                .unwrap()
        );
    }

    fn use_temp_smudgy_home() {
        static TEST_HOME: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
        TEST_HOME.get_or_init(|| {
            let dir = std::env::temp_dir().join(format!(
                "smudgy-local-packages-test-home-{}",
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
            "lpk-{label}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        );
        fs::create_dir_all(get_smudgy_home().unwrap().join(&name)).unwrap();
        name
    }

    fn write_bare_local_folder(server: &str, name: &str) {
        let dir = get_smudgy_home()
            .unwrap()
            .join(server)
            .join("packages")
            .join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("smudgy.package.json"), r#"{"version":"0.1.0"}"#).unwrap();
    }

    #[test]
    fn legacy_account_owned_row_migrates_onto_the_local_identity() {
        use crate::models::shared_packages::{
            LockedPackage, ParamValueScope, ParameterScope, get_param_value_scoped_checked,
            mutate_lock, package_param_state_exists, save_param_value,
        };
        let server = test_server("legacy-migration");
        write_bare_local_folder(&server, "tools");
        mutate_lock(&server, |lock| {
            let mut row = LockedPackage::new("smudgy://developer/tools", UpdateMode::Auto);
            row.trusted = true;
            row.parameter_scope = ParameterScope::Profile;
            row.set_activation(ProfileActivation::Selected {
                profiles: ["Main".to_string()].into_iter().collect(),
            });
            row.last_resolved_version = Some("1.0.0".into());
            lock.packages.push(row);
            let mut lib = LockedPackage::new("smudgy://wbk/lib", UpdateMode::Auto);
            lib.required_by
                .insert("smudgy://developer/tools".to_string());
            lib.installed_as_requirement = true;
            lock.packages.push(lib);
            Ok(((), true))
        })
        .unwrap();
        save_param_value(
            &server,
            "smudgy://developer/tools",
            "k",
            serde_json::json!(1),
        )
        .unwrap();

        assert!(
            materialize_governing_local_lock_rows(&server, &["tools".to_string()], "developer")
                .unwrap()
        );

        let lock = load_lock(&server).unwrap();
        assert!(lock.find("smudgy://developer/tools").is_none());
        let local = lock.find("smudgy://local/tools").unwrap();
        assert!(local.trusted, "trust carries over");
        assert_eq!(local.parameter_scope, ParameterScope::Profile);
        assert_eq!(
            local.activation(),
            ProfileActivation::Selected {
                profiles: ["Main".to_string()].into_iter().collect()
            }
        );
        assert_eq!(
            local.last_resolved_version, None,
            "the folder is the source now"
        );
        let lib = lock.find("smudgy://wbk/lib").unwrap();
        assert!(lib.required_by.contains("smudgy://local/tools"));
        assert!(!lib.required_by.contains("smudgy://developer/tools"));
        assert_eq!(
            get_param_value_scoped_checked(
                &server,
                ParamValueScope::Global,
                "smudgy://local/tools",
                "k"
            )
            .unwrap(),
            Some(serde_json::json!(1))
        );
        assert!(!package_param_state_exists(&server, "smudgy://developer/tools").unwrap());

        // Idempotent: a second pass finds nothing to migrate and nothing to add.
        assert!(
            !materialize_governing_local_lock_rows(&server, &["tools".to_string()], "developer")
                .unwrap()
        );
    }

    #[test]
    fn local_copy_of_a_shadowed_root_keeps_its_requirements_effective() {
        use crate::models::shared_packages::{LockedPackage, mutate_lock};
        let server = test_server("shadowed-root-requirements");
        write_bare_local_folder(&server, "app");
        mutate_lock(&server, |lock| {
            let mut app = LockedPackage::new("smudgy://wbk/app", UpdateMode::Auto);
            app.set_activation(ProfileActivation::All);
            lock.packages.push(app);
            let mut lib = LockedPackage::new("smudgy://wbk/lib", UpdateMode::Auto);
            lib.required_by.insert("smudgy://wbk/app".to_string());
            lib.installed_as_requirement = true;
            lib.requirement_lineage_known = true;
            lock.packages.push(lib);
            Ok(((), true))
        })
        .unwrap();

        assert!(
            materialize_governing_local_lock_rows(&server, &["app".to_string()], LOCAL_OWNER)
                .unwrap()
        );

        let lock = load_lock(&server).unwrap();
        let lib = lock.find("smudgy://wbk/lib").unwrap();
        assert!(lib.required_by.contains("smudgy://local/app"));
        assert!(
            lock.is_effectively_enabled_for("smudgy://wbk/lib", "Main"),
            "the override inherits the fallback's activation and keeps its requirement running"
        );
    }

    #[test]
    fn creating_a_local_package_adopts_orphaned_settings() {
        use crate::models::shared_packages::{get_param_value_for_profile, save_param_value};
        let server = test_server("adopt-orphaned-settings");
        save_param_value(&server, "smudgy://local/foo", "k", serde_json::json!(7)).unwrap();
        write_bare_local_folder(&server, "foo");

        assert!(
            materialize_governing_local_lock_rows(&server, &["foo".to_string()], LOCAL_OWNER)
                .unwrap()
        );
        assert!(
            load_lock(&server)
                .unwrap()
                .find("smudgy://local/foo")
                .is_some()
        );
        assert_eq!(
            get_param_value_for_profile(&server, "Main", "smudgy://local/foo", "k"),
            Some(serde_json::json!(7))
        );
    }

    #[test]
    fn scaffold_lists_loads_and_deletes() {
        let server = test_server("scaffold");
        scaffold_local_package_with_state(&server, "mine", "wbk").unwrap();
        assert_eq!(list_local_packages(&server).unwrap(), ["mine"]);
        let package = load_local_package(&server, "mine").unwrap().unwrap();
        assert_eq!(package.manifest.version, "0.1.0");
        assert!(package.readme.unwrap().starts_with("# mine"));
        assert_eq!(
            package.modules.len(),
            1,
            "tsconfig and README are not modules"
        );
        let row = load_lock(&server)
            .unwrap()
            .find("smudgy://local/mine")
            .cloned()
            .unwrap();
        assert_eq!(row.activation(), ProfileActivation::None);
        assert!(scaffold_local_package_with_state(&server, "MINE", "wbk").is_err());

        write_local_file(&server, "mine", "index.ts", "export const x = 1;").unwrap();
        assert_eq!(
            write_local_file_if_unchanged(&server, "mine", "index.ts", "stale", "y").unwrap(),
            LocalFileWriteOutcome::Conflict
        );
        assert_eq!(
            write_local_file_if_unchanged(&server, "mine", "index.ts", "export const x = 1;", "y")
                .unwrap(),
            LocalFileWriteOutcome::Saved
        );
        assert_eq!(read_local_file(&server, "mine", "index.ts").unwrap(), "y");

        let summary = delete_local_package(&server, "mine").unwrap();
        assert!(summary.warnings.is_empty());
        assert!(list_local_packages(&server).unwrap().is_empty());
        assert!(
            load_lock(&server)
                .unwrap()
                .find("smudgy://local/mine")
                .is_none()
        );
    }

    #[test]
    fn a_same_leaf_copy_inherits_the_published_activation() {
        let server = test_server("takeover");
        install_package(&server, "smudgy://wbk/mapper", UpdateMode::Auto, true).unwrap();
        let (manifest, modules) = starter_package().unwrap();
        fork_to_local_with_readme_and_state(&server, "mapper", &manifest, &modules, None, "wbk")
            .unwrap();
        let lock = load_lock(&server).unwrap();
        let local = lock.find("smudgy://local/mapper").unwrap();
        assert_eq!(local.activation(), ProfileActivation::All);
        assert!(
            lock.find("smudgy://wbk/mapper").is_some(),
            "the fallback row stays"
        );
        assert_eq!(
            lock.governing_specifier("smudgy://wbk/mapper"),
            Some("smudgy://local/mapper")
        );
    }

    #[test]
    fn requirement_aware_copy_links_required_roots_or_reports_stale_state() {
        let server = test_server("copy-requires");
        install_package(&server, "smudgy://a/dep", UpdateMode::Auto, true).unwrap();
        let expected = load_lock(&server).unwrap();
        let (manifest, modules) = starter_package().unwrap();
        let required = ["smudgy://a/dep".to_string()];
        assert_eq!(
            fork_to_local_with_readme_and_existing_requirements_if_unchanged(
                &server, "root", &manifest, &modules, None, "wbk", &expected, &required
            )
            .unwrap(),
            Cas::Applied
        );
        let lock = load_lock(&server).unwrap();
        assert!(
            lock.find("smudgy://a/dep")
                .unwrap()
                .required_by
                .contains("smudgy://local/root")
        );
        assert_eq!(
            fork_to_local_with_readme_and_existing_requirements_if_unchanged(
                &server,
                "other",
                &manifest,
                &modules,
                None,
                "wbk",
                &expected,
                &[]
            )
            .unwrap(),
            Cas::StateChanged
        );
        assert!(load_local_package(&server, "other").unwrap().is_none());
    }

    #[test]
    fn rename_moves_folder_and_state_but_not_published_packages() {
        let server = test_server("rename");
        scaffold_local_package_with_state(&server, "one", "wbk").unwrap();
        assert!(rename_local_package(&server, "one", "two").unwrap());
        assert_eq!(list_local_packages(&server).unwrap(), ["two"]);
        assert!(
            load_lock(&server)
                .unwrap()
                .find("smudgy://local/two")
                .is_some()
        );
        assert!(!rename_local_package(&server, "two", "two").unwrap());

        save_publication_binding(&server, "two", Uuid::new_v4(), "two").unwrap();
        assert!(rename_local_package(&server, "two", "three").is_err());
        assert_eq!(list_local_packages(&server).unwrap(), ["two"]);
    }

    #[test]
    fn declaration_detection_and_module_merging() {
        assert!(is_declaration_file("index.d.ts"));
        assert!(is_declaration_file("types/util.D.MTS"));
        assert!(!is_declaration_file("index.ts"));
        assert!(!is_declaration_file("d.ts.txt"));
        assert!(is_typescript_source("a.tsx"));
        assert!(!is_typescript_source("a.d.ts"));

        let module = |subpath: &str| PublishModule {
            subpath: subpath.into(),
            content: Vec::new(),
            media_type: "text/plain".into(),
            is_entry: false,
        };
        let merged = merge_published_modules(
            vec![module("index.ts"), module("index.d.ts")],
            vec![module("index.d.ts"), module("util.d.ts")],
        );
        let subpaths = merged
            .iter()
            .map(|m| m.subpath.as_str())
            .collect::<Vec<_>>();
        assert_eq!(subpaths, ["index.ts", "index.d.ts", "util.d.ts"]);
        assert!(excludes_zero_zero_patch("^0.0.1", "0.0.1"));
        assert!(!excludes_zero_zero_patch(">=0.0.1", "0.0.1"));
    }
}
