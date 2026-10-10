//! Package-asset lookups for the `<Image>` side-channel (plan D4/PR2): a thin **pub**
//! facade over the private, content-addressed package cache, so the `ui` fetcher can serve
//! `PackageAsset` sources without the script-engine module tree going public.
//!
//! The flow the ui fetcher runs (see `ui/src/images/mod.rs`):
//! 1. Local dev-override packages read straight from `<server>/packages/<name>/…` — not
//!    through here.
//! 2. Published packages: [`PackageAssetCache::module_hash`] maps `(owner, name, version,
//!    subpath)` to the module's content hash via the resolution metadata the provider
//!    persisted at load time, then [`read_blob_bytes`](PackageAssetCache::read_blob_bytes)
//!    serves the body if it was ever downloaded.
//! 3. On a blob miss the fetcher re-resolves the version over the network for a fresh
//!    signed bundle URL (they are ephemeral and deliberately never cached), fetches that
//!    one body (decoded and SHA-verified), and hands it back through
//!    [`write_blob_bytes`](PackageAssetCache::write_blob_bytes) — content-addressed and
//!    immutable, so it is cached forever after.

use smudgy_script::PackageKey;

use crate::session::runtime::script_engine::package_cache::PackageCache;
pub use crate::session::runtime::script_engine::package_cache::is_code_module;

/// A read/write handle on the shared package blob/metadata cache, scoped to what the image
/// side-channel needs.
pub struct PackageAssetCache {
    cache: PackageCache,
}

impl PackageAssetCache {
    /// Open the cache under the smudgy home. `None` when the home directory cannot be
    /// determined (the fetcher then works network-only).
    #[must_use]
    pub fn open() -> Option<Self> {
        PackageCache::new().ok().map(|cache| Self { cache })
    }

    /// The content hash of `subpath` within `owner/name@version`, from the persisted
    /// resolution metadata. The subpath match is **exact** (already component-validated at
    /// resolve time; installed-package subpaths are matched against the published module
    /// list, never a case-folding filesystem — plan edge 18). Only ASSET modules resolve
    /// here: addressing a code module as an image is refused (its bytes would never decode,
    /// and the image pipeline must not become a code-exfiltration side door).
    ///
    /// Tries the identity's spelling, then the folded spelling (metadata directories are
    /// keyed by whatever case the resolve used).
    #[must_use]
    pub fn module_hash(
        &self,
        owner: &str,
        name: &str,
        version: &str,
        subpath: &str,
    ) -> Option<String> {
        let exact = PackageKey {
            owner: owner.to_string(),
            name: name.to_string(),
        };
        let meta = self
            .cache
            .read_meta(&exact, version)
            .or_else(|| self.cache.read_meta(&exact.folded(), version))?;
        meta.modules
            .iter()
            .find(|m| m.subpath == subpath && !is_code_module(&m.media_type, &m.subpath))
            .map(|m| m.content_hash.clone())
    }

    /// A cached asset body whose bytes still match the content hash.
    #[must_use]
    pub fn read_blob_bytes(&self, content_hash: &str) -> Option<Vec<u8>> {
        self.cache.read_blob_bytes(content_hash)
    }

    /// Store a hash-verified asset body (best-effort; the caller keeps its in-memory copy).
    pub fn write_blob_bytes(&self, content_hash: &str, body: &[u8]) {
        if let Err(err) = self.cache.write_blob_bytes(content_hash, body) {
            log::warn!("smudgy images: caching package asset {content_hash} failed: {err:#}");
        }
    }
}
