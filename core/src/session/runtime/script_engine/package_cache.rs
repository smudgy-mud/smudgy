//! Persistent, content-addressed cache for fetched `smudgy://` packages.
//!
//! Published versions are immutable (a `name@1.4.0`'s content never changes), so once
//! fetched they can be cached **permanently** — like deno's module cache or npm's. This
//! caches two things under `<smudgy_home>/cache/packages/`:
//!
//! - **blobs** (`blobs/<hash[0:2]>/<hash[2:4]>/<hash>`): module bodies keyed by their
//!   SHA-256, so the provider only re-downloads bodies it doesn't already have, and
//!   identical bodies dedupe across packages/versions.
//! - **metadata** (`meta/<owner>/<name>/<version>.json`): the manifest + module
//!   list for a concrete version, so a *pinned* package resolves fully offline. Owner
//!   and name are ASCII-folded; unique directories from the old layout remain readable.
//!
//! Bodies are written only after the provider verified their hash on fetch. Reads verify the
//! hash again: a content-addressed filename does not protect against later disk corruption or
//! local tampering.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use smudgy_cloud::{DependencyKind, ResolvedDependency, package_api::ResolvedModuleWire};
pub use smudgy_script::PackageKey;
use smudgy_script::{PackageManifest, PackageOwner, SmudgySpecifier};

use crate::get_smudgy_home;
use crate::models::naming::validate_package_name;
use crate::models::persistence::write_atomic;

/// A cached resolution of a concrete package version (no signed bundle URLs — those are
/// ephemeral; bodies live in the blob cache, keyed by `content_hash`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResolution {
    pub version: String,
    /// The package-level fingerprint ([`package_integrity`]) a network-verified load stamps
    /// into the lockfile.
    pub integrity: String,
    pub manifest: PackageManifest,
    pub modules: Vec<CachedModule>,
    /// The version's locked `smudgy://` deps, so an offline load can repopulate
    /// referrer-aware version selection. `default` keeps older cache files readable; a file
    /// written before relation kinds were preserved has them recovered from the manifest by
    /// [`PackageCache::read_meta`].
    #[serde(default)]
    pub dependencies: Vec<ResolvedDependency>,
    /// Who owns the package, as the registry reported it when this version was resolved.
    /// `None` in files written before owners were recorded, and when the owner is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<PackageOwner>,
}

/// One module's metadata within a [`CachedResolution`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedModule {
    pub subpath: String,
    pub content_hash: String,
    /// The published media type. `default` keeps pre-field cache files readable — they
    /// deserialize as `text/plain`, i.e. code, which matches how they were treated when
    /// written. Distinguishes code (eagerly loaded into the module graph) from assets
    /// (lazily fetched by the image side-channel).
    #[serde(default = "default_cached_media_type")]
    pub media_type: String,
    /// Published byte count. Zero is the backward-compatible "not supplied" value for older
    /// servers and cache files.
    #[serde(default)]
    pub byte_size: i64,
    /// Whether the registry selected this module as the package entrypoint.
    #[serde(default)]
    pub is_entry: bool,
}

fn default_cached_media_type() -> String {
    "text/plain".to_string()
}

/// Whether a module belongs in the module graph (code/text, eagerly loaded as UTF-8) as
/// opposed to a binary asset (image and friends — lazily byte-fetched via the image
/// side-channel, never fed to V8).
///
/// `application/octet-stream` is the publish-side fallback for any unmapped extension, so
/// it is ambiguous: pre-PR2, every octet-stream module that *worked* was UTF-8 code with
/// an exotic extension (binaries failed the whole package load), so octet-stream stays
/// code unless the subpath's extension is unmistakably binary media — those become lazy
/// assets. A misclassified-as-code binary fails exactly as it always did (a per-module
/// UTF-8 error); misclassified-as-asset code merely fails its import with a clear message.
#[must_use]
pub fn is_code_module(media_type: &str, subpath: &str) -> bool {
    if media_type.starts_with("text/")
        || matches!(
            media_type,
            "application/typescript" | "application/javascript" | "application/json"
        )
    {
        return true;
    }
    if media_type == "application/octet-stream" {
        return !has_binary_media_extension(subpath);
    }
    false
}

/// Extensions that are unmistakably binary media (never importable code), for
/// classifying the ambiguous `application/octet-stream` publish fallback.
fn has_binary_media_extension(subpath: &str) -> bool {
    let ext = std::path::Path::new(subpath)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    matches!(
        ext.as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "avif"
            | "bmp"
            | "ico"
            | "tiff"
            | "tif"
            | "svg"
            | "svgz"
            | "mp3"
            | "ogg"
            | "wav"
            | "flac"
            | "mp4"
            | "webm"
            | "mkv"
            | "avi"
            | "mov"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "eot"
            | "zip"
            | "gz"
            | "tar"
            | "pdf"
    )
}

/// A deterministic package-level integrity fingerprint over the per-module content
/// hashes (each is already a SHA-256). Detects any module change for the lockfile.
/// One recipe for every writer — the engine's provider and the background update
/// checker stamp identical metas.
#[must_use]
pub fn package_integrity(modules: &[ResolvedModuleWire]) -> String {
    let mut entries: Vec<String> = modules
        .iter()
        .map(|module| format!("{}={}", module.subpath, module.content_hash))
        .collect();
    entries.sort();
    entries.join(";")
}

/// Build cache metadata from a network resolution. This is the one recipe used by the engine
/// and update checker, so both persist byte-for-byte compatible metadata and stamps.
///
/// # Errors
/// Returns an error when the requested identity or version is not a safe cache coordinate.
pub fn resolution_from_wire(
    key: &PackageKey,
    wire_version: &str,
    manifest: PackageManifest,
    modules: &[ResolvedModuleWire],
    dependencies: &[ResolvedDependency],
) -> Result<CachedResolution> {
    validate_cache_identity(key, wire_version)?;
    Ok(CachedResolution {
        version: wire_version.to_string(),
        integrity: package_integrity(modules),
        manifest,
        modules: modules
            .iter()
            .map(|module| CachedModule {
                subpath: module.subpath.clone(),
                content_hash: module.content_hash.clone(),
                media_type: module.media_type.clone(),
                byte_size: module.byte_size,
                is_entry: module.is_entry,
            })
            .collect(),
        dependencies: dependencies.to_vec(),
        owner: None,
    })
}

/// Rejects identities and versions that would escape (or alias) the cache's on-disk layout.
fn validate_cache_identity(key: &PackageKey, version: &str) -> Result<()> {
    if !key.owner.is_empty() {
        validate_package_name(&key.owner)
            .map_err(|error| anyhow!("invalid package owner: {error}"))?;
    }
    validate_package_name(&key.name).map_err(|error| anyhow!("invalid package name: {error}"))?;
    let parsed = SmudgySpecifier::parse(&key.to_user_specifier())?;
    if parsed.subpath.is_some()
        || !parsed.owner.eq_ignore_ascii_case(&key.owner)
        || !parsed.name.eq_ignore_ascii_case(&key.name)
    {
        bail!("invalid package cache identity {}", key.to_user_specifier());
    }
    let parsed_version = Version::parse(version).context("invalid package cache version")?;
    if parsed_version.to_string() != version {
        bail!("package cache version must be canonical semver: {version}");
    }
    Ok(())
}

fn normalize_sha256(value: &str) -> Option<String> {
    let hash = value.strip_prefix("sha256-").unwrap_or(value);
    (hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| hash.to_ascii_lowercase())
}

/// Disk cache rooted at `<smudgy_home>/cache/packages/`.
#[derive(Debug, Clone)]
pub struct PackageCache {
    root: PathBuf,
}

impl PackageCache {
    /// Open (or locate) the cache under the smudgy home directory.
    ///
    /// # Errors
    /// Returns an error if the smudgy home directory cannot be determined.
    pub fn new() -> Result<Self> {
        let root = get_smudgy_home()
            .context("locate smudgy home for package cache")?
            .join("cache")
            .join("packages");
        Ok(Self { root })
    }

    /// A cache rooted at an explicit directory, for callers that must not touch the
    /// process-global smudgy home (tests, tooling).
    #[must_use]
    pub fn with_root(root: PathBuf) -> Self {
        Self { root }
    }

    fn blob_path(&self, content_hash: &str) -> PathBuf {
        let hash = normalize_sha256(content_hash).unwrap_or_else(|| "0".repeat(64));
        let a = &hash[0..2];
        let b = &hash[2..4];
        self.root.join("blobs").join(a).join(b).join(hash)
    }

    /// Where a version's metadata lives: under its name alone, since a package's identity is
    /// its name and every spelling of its address (`smudgy:@name`, `smudgy://owner/name`)
    /// names it, whoever owns it now.
    fn meta_path(&self, key: &PackageKey, version: &str) -> PathBuf {
        self.root
            .join("meta")
            .join(smudgy_script::owner_segment(""))
            .join(key.name.to_ascii_lowercase())
            .join(format!("{version}.json"))
    }

    /// Old caches kept metadata under the owner segment an address was spelled with. Look in
    /// the requested spelling's directory first, then in the one owner directory holding this
    /// name and version, if exactly one does. Directory names match ignoring ASCII case, so
    /// case-sensitive filesystems find them too.
    fn legacy_meta_path(&self, key: &PackageKey, version: &str) -> Option<PathBuf> {
        self.spelled_legacy_meta_path(key, version)
            .filter(|path| path.exists())
            .or_else(|| self.any_owner_legacy_meta_path(key, version))
    }

    /// The legacy metadata file under any owner directory, if exactly one holds `key`'s name
    /// at `version`.
    fn any_owner_legacy_meta_path(&self, key: &PackageKey, version: &str) -> Option<PathBuf> {
        let file = format!("{version}.json");
        let mut found = fs::read_dir(self.root.join("meta"))
            .ok()?
            .filter_map(Result::ok)
            .filter(|owner| owner.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|owner| {
                fs::read_dir(owner.path())
                    .ok()?
                    .filter_map(Result::ok)
                    .find(|package| {
                        package.file_type().is_ok_and(|kind| kind.is_dir())
                            && package
                                .file_name()
                                .to_str()
                                .is_some_and(|name| name.eq_ignore_ascii_case(&key.name))
                    })
                    .map(|package| package.path().join(&file))
            })
            .filter(|path| path.is_file());
        let path = found.next()?;
        found.next().is_none().then_some(path)
    }

    /// The legacy metadata file under the owner segment `key` spells.
    fn spelled_legacy_meta_path(&self, key: &PackageKey, version: &str) -> Option<PathBuf> {
        fn directory(parent: &Path, name: &str) -> Option<PathBuf> {
            let mut matches = fs::read_dir(parent)
                .ok()?
                .filter_map(Result::ok)
                .filter(|entry| {
                    entry.file_type().is_ok_and(|kind| kind.is_dir())
                        && entry
                            .file_name()
                            .to_str()
                            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(name))
                });
            let path = matches.next()?.path();
            matches.next().is_none().then_some(path)
        }
        let owner = directory(
            &self.root.join("meta"),
            smudgy_script::owner_segment(&key.owner),
        )?;
        let package = directory(&owner, &key.name)?;
        Some(package.join(format!("{version}.json")))
    }

    /// Whether a module body is already cached and still matches its content hash.
    /// Writers use this to skip a fetch only after the existing bytes have been verified.
    #[must_use]
    pub fn has_blob(&self, content_hash: &str) -> bool {
        self.read_blob_bytes(content_hash).is_some()
    }

    /// A cached UTF-8 module body, if present and its SHA-256 matches `content_hash`.
    #[must_use]
    pub fn read_blob(&self, content_hash: &str) -> Option<String> {
        String::from_utf8(self.read_blob_bytes(content_hash)?).ok()
    }

    /// Byte twin of [`read_blob`](Self::read_blob), for binary (asset) bodies. A corrupt or
    /// tampered file is a cache miss, which lets an online caller fetch and atomically replace it.
    #[must_use]
    pub fn read_blob_bytes(&self, content_hash: &str) -> Option<Vec<u8>> {
        let expected = normalize_sha256(content_hash)?;
        let bytes = fs::read(self.blob_path(content_hash)).ok()?;
        let actual = format!("{:x}", Sha256::digest(&bytes));
        if actual == expected {
            Some(bytes)
        } else {
            log::warn!(
                "Ignoring package cache blob whose bytes do not match its SHA-256 name: {content_hash}"
            );
            None
        }
    }

    /// Store a module body under its content hash. Best-effort; a write failure is not
    /// fatal (the provider keeps the in-memory copy and just re-downloads next time).
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be created or the file written.
    pub fn write_blob(&self, content_hash: &str, body: &str) -> Result<()> {
        self.write_blob_bytes(content_hash, body.as_bytes())
    }

    /// Byte twin of [`write_blob`](Self::write_blob), for binary (asset) bodies.
    ///
    /// The write is atomic (temp sibling + rename): readers reject a torn body by hash, and a
    /// completed write must still replace any corrupt file a previous interrupted attempt left
    /// behind.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be created or the file written.
    pub fn write_blob_bytes(&self, content_hash: &str, body: &[u8]) -> Result<()> {
        let expected = normalize_sha256(content_hash)
            .ok_or_else(|| anyhow!("invalid package blob SHA-256: {content_hash}"))?;
        let actual = format!("{:x}", Sha256::digest(body));
        if actual != expected {
            bail!("package blob bytes do not match SHA-256 {content_hash}");
        }
        let path = self.blob_path(content_hash);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create blob cache dir {}", parent.display()))?;
        }
        write_atomic(&path, body).with_context(|| format!("write blob {}", path.display()))
    }

    /// Whether resolution metadata for a concrete version is already cached and readable.
    /// Versions are immutable, so presence means the write-once entry is final — writers use
    /// this to skip a redundant serialize + write.
    #[must_use]
    pub fn has_meta(&self, key: &PackageKey, version: &str) -> bool {
        self.read_meta(key, version).is_some()
    }

    /// The cached resolution metadata for a concrete version, if present and readable.
    ///
    /// Older cache files are accepted as written: a module without a media type is code, and a
    /// dependency edge written before relation kinds were preserved has its kind recovered from
    /// the cached manifest — an edge the manifest's `requires` list names is a
    /// [`DependencyKind::Requires`] root, every other edge a code dependency.
    #[must_use]
    pub fn read_meta(&self, key: &PackageKey, version: &str) -> Option<CachedResolution> {
        validate_cache_identity(key, version).ok()?;
        let path = self.meta_path(key, version);
        let path = if path.exists() {
            path
        } else {
            self.legacy_meta_path(key, version)?
        };
        let content = fs::read_to_string(path).ok()?;
        let value: serde_json::Value = serde_json::from_str(&content).ok()?;
        let kind_less: Vec<usize> = value
            .get("dependencies")
            .and_then(serde_json::Value::as_array)
            .map(|dependencies| {
                dependencies
                    .iter()
                    .enumerate()
                    .filter(|(_, dependency)| dependency.get("kind").is_none())
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default();
        let mut resolution: CachedResolution = serde_json::from_value(value).ok()?;
        if !kind_less.is_empty() {
            // Names are global: an edge names a `requires` root when the names agree.
            let requires: BTreeSet<String> = resolution
                .manifest
                .smudgy_requires()
                .into_iter()
                .map(|required| required.key.name.to_ascii_lowercase())
                .collect();
            for index in kind_less {
                let Some(dependency) = resolution.dependencies.get_mut(index) else {
                    continue;
                };
                let edge = dependency.name.to_ascii_lowercase();
                dependency.kind = if requires.contains(&edge) {
                    DependencyKind::Requires
                } else {
                    DependencyKind::Dependency
                };
            }
        }
        Some(resolution)
    }

    /// Whether every **code** module body for a cached resolution is present (so the
    /// module graph can be served fully offline). Asset bodies (images) are lazy — they
    /// were possibly never downloaded, and must not hold the whole package hostage.
    #[must_use]
    pub fn has_all_code_blobs(&self, resolution: &CachedResolution) -> bool {
        resolution
            .modules
            .iter()
            .filter(|m| is_code_module(&m.media_type, &m.subpath))
            .all(|m| self.has_blob(&m.content_hash))
    }

    /// Persist resolution metadata for a concrete version (immutable, so write-once).
    ///
    /// The write is atomic (temp sibling + rename), so readers never see partial JSON
    /// and a completed write replaces any torn file an interrupted attempt left behind.
    ///
    /// # Errors
    /// Returns an error if the identity is not a safe cache coordinate, or the cache directory
    /// cannot be created or the file written.
    pub fn write_meta(
        &self,
        key: &PackageKey,
        version: &str,
        resolution: &CachedResolution,
    ) -> Result<()> {
        validate_cache_identity(key, version)?;
        let path = self.meta_path(key, version);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create meta cache dir {}", parent.display()))?;
        }
        let json = serde_json::to_string(resolution).context("serialize cached resolution")?;
        write_atomic(&path, json.as_bytes())
            .with_context(|| format!("write meta {}", path.display()))
    }

    /// Persist resolution metadata unless an equally-complete copy is already cached —
    /// the self-healing form of [`write_meta`](Self::write_meta). Versions are
    /// immutable, so the common case skips a redundant serialize + write; but a cache
    /// file written before `CachedResolution` grew a field deserializes with that field
    /// defaulted (a pre-`dependencies` file folds to a root-only closure; a
    /// pre-`media_type` file misclassifies modules), and freezing it forever would
    /// silently under-report the version's true facts. So a fresh resolution that
    /// carries dependency edges or modules the cached copy lacks — or finds the cached
    /// file unreadable (torn, corrupt) — overwrites it instead of skipping.
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be created or the file written.
    pub fn refresh_meta(
        &self,
        key: &PackageKey,
        version: &str,
        resolution: &CachedResolution,
    ) -> Result<()> {
        if let Some(cached) = self.read_meta(key, version) {
            let missing_deps =
                cached.dependencies.is_empty() && !resolution.dependencies.is_empty();
            let missing_modules = cached.modules.len() < resolution.modules.len();
            let missing_owner = cached.owner.is_none() && resolution.owner.is_some();
            if !missing_deps && !missing_modules && !missing_owner {
                return Ok(());
            }
        }
        self.write_meta(key, version, resolution)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_case_cache_coordinates_reuse_new_and_legacy_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let key = PackageKey {
            owner: "Rich_E".into(),
            name: "Speedwalks".into(),
        };
        let resolution = CachedResolution {
            version: "1.0.0".into(),
            integrity: "sum".into(),
            manifest: PackageManifest::parse(r#"{"version":"1.0.0"}"#).unwrap(),
            modules: vec![],
            dependencies: vec![],
            owner: None,
        };
        // Plant the pre-fix layout directly. On case-sensitive filesystems the
        // folded request must discover the legacy directory spelling.
        let legacy = cache.root.join("meta/Rich_E/Speedwalks/1.0.0.json");
        fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        fs::write(&legacy, serde_json::to_vec(&resolution).unwrap()).unwrap();
        assert_eq!(
            cache.read_meta(&key.folded(), "1.0.0").unwrap().integrity,
            "sum"
        );
        assert_eq!(
            cache.meta_path(&key, "1.0.0"),
            cache.meta_path(&key.folded(), "1.0.0")
        );
        cache.write_meta(&key, "1.0.0", &resolution).unwrap();
        assert!(cache.read_meta(&key.folded(), "1.0.0").is_some());
        assert!(cache.read_meta(&key, "2.0.0").is_none());
    }

    #[test]
    fn metadata_is_found_by_name_whatever_the_spelling() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let spelled = |owner: &str| PackageKey {
            owner: owner.into(),
            name: "Mapper".into(),
        };
        let resolution = CachedResolution {
            version: "1.0.0".into(),
            integrity: "new".into(),
            manifest: PackageManifest::parse(r#"{"version":"1.0.0"}"#).unwrap(),
            modules: vec![],
            dependencies: vec![],
            owner: Some(PackageOwner::user("Alice")),
        };
        cache
            .write_meta(&spelled("alice"), "1.0.0", &resolution)
            .unwrap();
        // The package moved to another owner (a renamed nickname, a clan): every spelling of
        // its address still finds the version.
        for owner in ["", "alice", "Bob", "someone-else"] {
            let meta = cache.read_meta(&spelled(owner), "1.0.0");
            assert_eq!(
                meta.map(|meta| meta.integrity).as_deref(),
                Some("new"),
                "{owner:?}"
            );
        }
        assert_eq!(
            cache
                .read_meta(&spelled(""), "1.0.0")
                .and_then(|meta| meta.owner),
            Some(PackageOwner::user("alice"))
        );
    }

    #[test]
    fn legacy_metadata_under_another_owner_is_found_by_name() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let legacy = |owner: &str, integrity: &str| {
            let path = cache.root.join(format!("meta/{owner}/mapper/1.0.0.json"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let resolution = CachedResolution {
                version: "1.0.0".into(),
                integrity: integrity.into(),
                manifest: PackageManifest::parse(r#"{"version":"1.0.0"}"#).unwrap(),
                modules: vec![],
                dependencies: vec![],
                owner: None,
            };
            fs::write(&path, serde_json::to_vec(&resolution).unwrap()).unwrap();
        };
        let key = |owner: &str| PackageKey {
            owner: owner.into(),
            name: "mapper".into(),
        };
        legacy("alice", "alice's");
        for owner in ["", "alice", "clan-now"] {
            assert_eq!(
                cache
                    .read_meta(&key(owner), "1.0.0")
                    .map(|meta| meta.integrity)
                    .as_deref(),
                Some("alice's"),
                "{owner:?}"
            );
        }
        assert!(cache.read_meta(&key(""), "2.0.0").is_none());
        // Two owner directories holding the name: only the spelled one is trusted.
        legacy("bob", "bob's");
        assert_eq!(
            cache
                .read_meta(&key("bob"), "1.0.0")
                .map(|meta| meta.integrity)
                .as_deref(),
            Some("bob's")
        );
        assert!(cache.read_meta(&key("carol"), "1.0.0").is_none());
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    fn cache_in(dir: &std::path::Path) -> PackageCache {
        PackageCache {
            root: dir.join("cache").join("packages"),
        }
    }

    fn key() -> PackageKey {
        PackageKey {
            owner: "wbk".into(),
            name: "mapper".into(),
        }
    }

    #[test]
    fn blob_round_trips_and_dedupes_by_hash() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let body = "export const x = 1;";
        let hash = sha256_hex(body.as_bytes());
        assert!(cache.read_blob(&hash).is_none());
        cache.write_blob(&hash, body).unwrap();
        assert_eq!(cache.read_blob(&hash).as_deref(), Some(body));
        // Byte twins share the same content-addressed pool.
        assert_eq!(
            cache.read_blob_bytes(&hash).as_deref(),
            Some(body.as_bytes())
        );
        let binary = [0u8, 159, 146, 150];
        let binary_hash = sha256_hex(&binary);
        cache.write_blob_bytes(&binary_hash, &binary).unwrap();
        assert_eq!(
            cache.read_blob_bytes(&binary_hash).as_deref(),
            Some(binary.as_slice())
        );
        // A binary blob is not silently lossy-read as a string.
        assert!(cache.read_blob(&binary_hash).is_none());
    }

    #[test]
    fn tampered_blob_is_a_cache_miss_for_every_read_path() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let body = b"export const safe = true;";
        let hash = sha256_hex(body);
        cache.write_blob_bytes(&hash, body).unwrap();
        assert!(cache.has_blob(&hash));

        fs::write(cache.blob_path(&hash), b"export const safe = false;").unwrap();

        assert!(!cache.has_blob(&hash));
        assert!(cache.read_blob(&hash).is_none());
        assert!(cache.read_blob_bytes(&hash).is_none());
        let resolution = CachedResolution {
            version: "1.0.0".into(),
            integrity: "sum".into(),
            manifest: PackageManifest::parse(r#"{"version":"1.0.0"}"#).unwrap(),
            modules: vec![CachedModule {
                subpath: "index.ts".into(),
                content_hash: hash,
                media_type: "application/typescript".into(),
                byte_size: body.len() as i64,
                is_entry: true,
            }],
            dependencies: Vec::new(),
            owner: None,
        };
        assert!(!cache.has_all_code_blobs(&resolution));
    }

    #[test]
    fn package_integrity_is_order_independent() {
        let module = |subpath: &str, hash: &str| ResolvedModuleWire {
            subpath: subpath.into(),
            content_hash: hash.into(),
            media_type: "text/plain".into(),
            byte_size: 0,
            is_entry: false,
        };
        let forward = package_integrity(&[module("a.ts", "aa"), module("b.ts", "bb")]);
        let reversed = package_integrity(&[module("b.ts", "bb"), module("a.ts", "aa")]);
        assert_eq!(forward, reversed);
        assert_eq!(forward, "a.ts=aa;b.ts=bb");
        assert_ne!(forward, package_integrity(&[module("a.ts", "aa")]));
    }

    #[test]
    fn meta_round_trips_and_offline_readiness() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let code = "export const x = 1;";
        let code_hash = sha256_hex(code.as_bytes());
        let resolution = CachedResolution {
            version: "1.4.0".into(),
            integrity: "sum".into(),
            manifest: PackageManifest::parse(r#"{ "name": "mapper", "version": "1.4.0" }"#)
                .unwrap(),
            modules: vec![
                CachedModule {
                    subpath: "index.ts".into(),
                    content_hash: code_hash.clone(),
                    media_type: "application/typescript".into(),
                    byte_size: code.len() as i64,
                    is_entry: true,
                },
                // An asset module: lazily fetched, so its blob must NOT gate offline use.
                CachedModule {
                    subpath: "assets/logo.png".into(),
                    content_hash: "f".repeat(64),
                    media_type: "image/png".into(),
                    byte_size: 0,
                    is_entry: false,
                },
            ],
            dependencies: Vec::new(),
            owner: None,
        };
        assert!(cache.read_meta(&key(), "1.4.0").is_none());
        cache.write_meta(&key(), "1.4.0", &resolution).unwrap();
        let loaded = cache.read_meta(&key(), "1.4.0").expect("meta round-trips");
        assert_eq!(loaded.version, "1.4.0");
        assert_eq!(loaded.integrity, "sum", "the stamp is stored as written");

        // Not offline-ready until the CODE body is cached; the asset never gates.
        assert!(!cache.has_all_code_blobs(&loaded));
        cache.write_blob(&code_hash, code).unwrap();
        assert!(cache.has_all_code_blobs(&loaded));
    }

    #[test]
    fn a_fresh_write_replaces_a_torn_file() {
        // Atomic writers must be able to REPLACE a torn file left by an interrupted write.
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let body = "export const x = 1;";
        let hash = sha256_hex(body.as_bytes());
        let resolution = CachedResolution {
            version: "1.4.0".into(),
            integrity: "sum".into(),
            manifest: PackageManifest::parse(r#"{ "name": "mapper", "version": "1.4.0" }"#)
                .unwrap(),
            modules: Vec::new(),
            dependencies: Vec::new(),
            owner: None,
        };
        // Plant torn files at the final content-addressed paths.
        let meta_path = cache.meta_path(&key(), "1.4.0");
        fs::create_dir_all(meta_path.parent().unwrap()).unwrap();
        fs::write(&meta_path, r#"{"version":"1.4"#).unwrap();
        assert!(
            cache.read_meta(&key(), "1.4.0").is_none(),
            "torn JSON is unreadable"
        );
        let blob_path = cache.blob_path(&hash);
        fs::create_dir_all(blob_path.parent().unwrap()).unwrap();
        fs::write(&blob_path, "export const trunc").unwrap();
        assert!(cache.read_blob(&hash).is_none(), "torn blob is rejected");

        // A completed write (rename over the final path) heals both.
        cache.refresh_meta(&key(), "1.4.0", &resolution).unwrap();
        assert_eq!(
            cache.read_meta(&key(), "1.4.0").map(|m| m.version),
            Some("1.4.0".to_string())
        );
        cache.write_blob(&hash, body).unwrap();
        assert_eq!(cache.read_blob(&hash).as_deref(), Some(body));
    }

    #[test]
    fn refresh_meta_heals_a_legacy_deps_less_file_but_never_regresses_one() {
        // A cache file written before the `dependencies` field deserializes with an
        // empty dep list; folding it yields a root-only closure union. A fresh
        // resolution carrying the version's true edges must overwrite it.
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let manifest =
            PackageManifest::parse(r#"{ "name": "mapper", "version": "1.4.0" }"#).unwrap();
        let legacy = CachedResolution {
            version: "1.4.0".into(),
            integrity: "sum".into(),
            manifest: manifest.clone(),
            modules: Vec::new(),
            dependencies: Vec::new(),
            owner: None,
        };
        cache.write_meta(&key(), "1.4.0", &legacy).unwrap();
        let full = CachedResolution {
            dependencies: vec![ResolvedDependency {
                owner_nickname: Some("wbk".into()),
                name: "base".into(),
                range: "^1".into(),
                resolved_version: "1.0.0".into(),
                kind: DependencyKind::Dependency,
            }],
            ..legacy.clone()
        };
        cache.refresh_meta(&key(), "1.4.0", &full).unwrap();
        assert_eq!(
            cache
                .read_meta(&key(), "1.4.0")
                .map(|m| m.dependencies.len()),
            Some(1),
            "the legacy file was healed with the wire's dependency edges"
        );
        // The reverse never regresses: a deps-less resolution (a degraded wire)
        // must not clobber the healed copy.
        cache.refresh_meta(&key(), "1.4.0", &legacy).unwrap();
        assert_eq!(
            cache
                .read_meta(&key(), "1.4.0")
                .map(|m| m.dependencies.len()),
            Some(1),
            "an equally-or-less-complete resolution leaves the cached copy alone"
        );
    }

    #[test]
    fn pre_feature_meta_recovers_relation_kinds_from_the_manifest() {
        // The exact shape written before `kind`, `byte_size`, and `is_entry` existed: every
        // edge was persisted alike. The cached manifest still says which edges are `requires`
        // roots, so the file stays servable offline without any repair.
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let body = "export const cached = true;";
        let hash = sha256_hex(body.as_bytes());
        cache.write_blob(&hash, body).unwrap();
        let path = cache.meta_path(&key(), "1.4.0");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                r#"{{
                "version":"1.4.0",
                "integrity":"index.ts={hash}",
                "manifest":{{"version":"1.4.0","requires":["smudgy://WBK/Peer@^1"]}},
                "modules":[{{"subpath":"index.ts","content_hash":"{hash}","media_type":"application/typescript"}}],
                "dependencies":[
                    {{"owner_nickname":"wbk","name":"peer","range":"^1","resolved_version":"1.0.0"}},
                    {{"owner_nickname":"wbk","name":"base","range":"^2","resolved_version":"2.0.0"}}
                ]
            }}"#
            ),
        )
        .unwrap();

        let loaded = cache
            .read_meta(&key(), "1.4.0")
            .expect("a pre-feature metadata file is readable");
        assert_eq!(loaded.integrity, format!("index.ts={hash}"));
        assert_eq!(loaded.dependencies[0].kind, DependencyKind::Requires);
        assert_eq!(loaded.dependencies[1].kind, DependencyKind::Dependency);
        assert!(!loaded.modules[0].is_entry);
        assert_eq!(loaded.modules[0].byte_size, 0);
        assert!(cache.has_all_code_blobs(&loaded));

        // A file that does carry kinds is taken at its word, whatever the manifest says.
        let explicit = CachedResolution {
            dependencies: vec![ResolvedDependency {
                kind: DependencyKind::Dependency,
                ..loaded.dependencies[0].clone()
            }],
            ..loaded
        };
        cache.write_meta(&key(), "1.4.0", &explicit).unwrap();
        assert_eq!(
            cache.read_meta(&key(), "1.4.0").unwrap().dependencies[0].kind,
            DependencyKind::Dependency
        );
    }

    #[test]
    fn unsafe_cache_identity_and_hash_shapes_never_reach_disk_paths() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let unsafe_key = PackageKey {
            owner: "..".into(),
            name: "escape".into(),
        };
        let resolution = CachedResolution {
            version: "1.0.0".into(),
            integrity: String::new(),
            manifest: PackageManifest::parse(r#"{"version":"1.0.0"}"#).unwrap(),
            modules: Vec::new(),
            dependencies: Vec::new(),
            owner: None,
        };
        assert!(cache.write_meta(&unsafe_key, "1.0.0", &resolution).is_err());
        assert!(cache.read_meta(&unsafe_key, "1.0.0").is_none());
        assert!(cache.write_meta(&key(), "../escape", &resolution).is_err());
        assert!(cache.write_blob("../escape", "code").is_err());
    }

    #[test]
    fn media_type_classification() {
        for (code, subpath) in [
            ("application/typescript", "index.ts"),
            ("application/javascript", "lib/x.js"),
            ("application/json", "data.json"),
            ("text/plain", "shader.wgsl"),
            ("text/css", "style.css"),
            // The publish fallback for an unmapped extension: UTF-8 code pre-PR2 —
            // must keep loading as code (only unmistakably-binary extensions are lazy).
            ("application/octet-stream", "lib/helper.cts"),
            ("application/octet-stream", "LICENSE"),
        ] {
            assert!(
                is_code_module(code, subpath),
                "{code} {subpath} should be code"
            );
        }
        for (asset, subpath) in [
            ("image/png", "assets/logo.png"),
            ("image/svg+xml", "assets/icon.svg"),
            ("application/octet-stream", "assets/photo.avif"),
            ("application/octet-stream", "sounds/hit.ogg"),
            ("application/wasm", "lib/fast.wasm"),
            ("audio/ogg", "sounds/hit.ogg"),
            ("font/woff2", "fonts/ui.woff2"),
        ] {
            assert!(
                !is_code_module(asset, subpath),
                "{asset} {subpath} should be an asset"
            );
        }
        // Pre-field cache files deserialize to text/plain, i.e. code — matching how they
        // were treated when written.
        let old: CachedModule =
            serde_json::from_str(r#"{"subpath":"index.ts","content_hash":"aa"}"#).unwrap();
        assert!(is_code_module(&old.media_type, &old.subpath));
    }
}
