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
//!   list for a concrete version, so a *pinned* package resolves fully offline.
//!
//! Bodies are written only after the provider verified their hash on fetch, and the
//! cache is content-addressed, so reads are trusted without re-hashing.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use smudgy_cloud::ResolvedDependency;
use smudgy_script::{PackageKey, PackageManifest};

use crate::get_smudgy_home;

/// A cached resolution of a concrete package version (no presigned URLs — those are
/// ephemeral; bodies live in the blob cache, keyed by `content_hash`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResolution {
    pub version: String,
    pub integrity: String,
    pub manifest: PackageManifest,
    pub modules: Vec<CachedModule>,
    /// The version's locked `smudgy://` deps, so an offline load can repopulate
    /// referrer-aware version selection. `default` keeps older cache files readable.
    #[serde(default)]
    pub dependencies: Vec<ResolvedDependency>,
}

/// One module's metadata within a [`CachedResolution`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedModule {
    pub subpath: String,
    pub content_hash: String,
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

    fn blob_path(&self, content_hash: &str) -> PathBuf {
        let a = content_hash.get(0..2).unwrap_or("00");
        let b = content_hash.get(2..4).unwrap_or("00");
        self.root.join("blobs").join(a).join(b).join(content_hash)
    }

    fn meta_path(&self, key: &PackageKey, version: &str) -> PathBuf {
        self.root
            .join("meta")
            .join(&key.owner)
            .join(&key.name)
            .join(format!("{version}.json"))
    }

    /// A cached module body, if present (content-addressed; trusted without re-hashing).
    #[must_use]
    pub fn read_blob(&self, content_hash: &str) -> Option<String> {
        fs::read_to_string(self.blob_path(content_hash)).ok()
    }

    /// Store a module body under its content hash. Best-effort; a write failure is not
    /// fatal (the provider keeps the in-memory copy and just re-downloads next time).
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be created or the file written.
    pub fn write_blob(&self, content_hash: &str, body: &str) -> Result<()> {
        let path = self.blob_path(content_hash);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create blob cache dir {}", parent.display()))?;
        }
        fs::write(&path, body).with_context(|| format!("write blob {}", path.display()))
    }

    /// The cached resolution metadata for a concrete version, if present.
    #[must_use]
    pub fn read_meta(&self, key: &PackageKey, version: &str) -> Option<CachedResolution> {
        let content = fs::read_to_string(self.meta_path(key, version)).ok()?;
        serde_json::from_str(&content).ok()
    }

    /// Whether every module body for a cached resolution is present (so it can be
    /// served fully offline).
    #[must_use]
    pub fn has_all_blobs(&self, resolution: &CachedResolution) -> bool {
        resolution
            .modules
            .iter()
            .all(|m| self.blob_path(&m.content_hash).exists())
    }

    /// Persist resolution metadata for a concrete version (immutable, so write-once).
    ///
    /// # Errors
    /// Returns an error if the cache directory cannot be created or the file written.
    pub fn write_meta(
        &self,
        key: &PackageKey,
        version: &str,
        resolution: &CachedResolution,
    ) -> Result<()> {
        let path = self.meta_path(key, version);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create meta cache dir {}", parent.display()))?;
        }
        let json = serde_json::to_string(resolution).context("serialize cached resolution")?;
        fs::write(&path, json).with_context(|| format!("write meta {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(cache.read_blob("abc123").is_none());
        cache.write_blob("abc123", "export const x = 1;").unwrap();
        assert_eq!(cache.read_blob("abc123").as_deref(), Some("export const x = 1;"));
    }

    #[test]
    fn meta_round_trips_and_offline_readiness() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let resolution = CachedResolution {
            version: "1.4.0".into(),
            integrity: "sum".into(),
            manifest: PackageManifest::parse(r#"{ "name": "mapper", "version": "1.4.0" }"#).unwrap(),
            modules: vec![CachedModule {
                subpath: "index.ts".into(),
                content_hash: "deadbeef".into(),
            }],
            dependencies: Vec::new(),
        };
        assert!(cache.read_meta(&key(), "1.4.0").is_none());
        cache.write_meta(&key(), "1.4.0", &resolution).unwrap();
        let loaded = cache.read_meta(&key(), "1.4.0").expect("meta round-trips");
        assert_eq!(loaded.version, "1.4.0");

        // Not offline-ready until the body is cached too.
        assert!(!cache.has_all_blobs(&loaded));
        cache.write_blob("deadbeef", "export const x = 1;").unwrap();
        assert!(cache.has_all_blobs(&loaded));
    }
}
