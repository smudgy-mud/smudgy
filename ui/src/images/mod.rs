//! The `<Image>` widget's loader: the [`smudgy_widgets::ImageFetcher`] implementation.
//!
//! The widget/store side (`smudgy_widgets::image_store`) owns lifecycle, memoization, and
//! handle identity; THIS side owns all I/O and decode policy:
//!
//! - **Remote** srcs go through [`http_cache`] — an RFC-9111-subset disk cache under
//!   `<smudgy_home>/cache/images/` with per-server metadata namespaces (plan D5/D10) — on a
//!   dedicated `reqwest` client with no default headers (the shared cloud client stamps
//!   `x-smudgy-client-version`, which package-chosen hosts must not see) and **manual**
//!   redirect following, so every hop re-validates against a sandboxed package's `net`
//!   grants (a client-level redirect policy can't carry per-package state).
//! - **Local files** re-check confinement post-canonicalization (`dunce`, so Windows
//!   doesn't grow `\\?\` prefixes) — the build op's check was lexical; symlinks resolve here.
//! - **All raster bytes pre-decode off-thread** with explicit [`image::Limits`] (decode
//!   bombs), EXIF orientation applied (iced only orients in its own decode path; RGBA
//!   handles bypass it), bounded to [`MAX_CONCURRENT_DECODES`] at a time.
//! - **SVG is rejected** (raster-only v1). When SVG lands it must NOT go through iced's
//!   svg pipeline — usvg's default href resolver reads arbitrary local files.
//!
//! The MSSP `ICON` pipeline ([`server_icon`]) shares this module's decode distrust but
//! carries its own, tighter fetch posture and a per-server-directory cache.

mod http_cache;
pub mod server_icon;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use smudgy_cloud::image_source::{ImageSourcePolicy, ResolvedImageSource};
use smudgy_cloud::package_api::PackageApiClient;
use smudgy_core::session::runtime::image_assets::PackageAssetCache;
use smudgy_widgets::{DecodedImage, FetchError, FileStamp, ImageFetcher, ImageStore};
use tokio::sync::Semaphore;

/// Hard cap on any encoded image body (HTTP response, local file, package asset).
pub const MAX_BODY_BYTES: u64 = 32 * 1024 * 1024;

/// Decode-time pixel bounds: dimensions and a transient-allocation cap. A 16384² RGBA
/// decode would transiently allocate 1 GiB — the alloc cap is what actually bounds memory.
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_DECODE_ALLOC_BYTES: u64 = 128 * 1024 * 1024;

/// Concurrent decode bound: decodes are CPU work on the store's small runtime; more than
/// this just thrashes and delays the first image.
const MAX_CONCURRENT_DECODES: usize = 2;

/// Concurrent *remote-fetch* bound. Distinct-source fetch spawns are already budgeted at
/// the canvas/widget layer, but a burst (or a hostile bound scene naming nonce URLs each
/// write) must not turn into an unbounded pile of live sockets and disk-cache writes.
const MAX_CONCURRENT_FETCHES: usize = 8;

/// The process-global [`ImageStore`], built once on first use and shared by every session
/// (entries are keyed by content source, so cross-session sharing dedups fetch/decode/upload).
pub fn image_store() -> ImageStore {
    static STORE: std::sync::OnceLock<ImageStore> = std::sync::OnceLock::new();
    STORE
        .get_or_init(|| ImageStore::new(Arc::new(UiImageFetcher::new())))
        .clone()
}

/// The authenticated package-API client the `PackageAsset` arm uses to re-resolve a version
/// when an asset blob is missing locally (signed bundle URLs are ephemeral and never
/// cached, so a late first display needs a fresh resolve). Sessions register theirs at
/// construction; every session shares one account, so last-write-wins is correct, and the
/// [`CredentialSource`](smudgy_cloud::backends::CredentialSource) inside hot-swaps on
/// login/logout.
fn package_client_slot() -> &'static arc_swap::ArcSwapOption<PackageApiClient> {
    static SLOT: std::sync::OnceLock<arc_swap::ArcSwapOption<PackageApiClient>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(arc_swap::ArcSwapOption::empty)
}

/// Register the package-API client asset fetches re-resolve through (see
/// [`package_client_slot`]). Called by `session_store` per session; idempotent in effect.
pub fn set_package_client(client: PackageApiClient) {
    package_client_slot().store(Some(Arc::new(client)));
}

/// The disk cache handle for management operations (`None` when no home dir resolves —
/// the same condition under which the fetcher runs uncached).
fn disk_cache() -> Option<http_cache::HttpImageCache> {
    smudgy_core::get_smudgy_home()
        .ok()
        .map(|home| http_cache::HttpImageCache::new(home.join("cache").join("images")))
}

/// Approximate on-disk bytes of `server`'s cached images (plan D10's usage display).
/// Blocking I/O — call from a `Task::perform` executor.
#[must_use]
pub fn server_image_cache_usage_bytes(server: &str) -> u64 {
    disk_cache().map_or(0, |cache| cache.server_usage_bytes(server))
}

/// "Clear image cache" for one server: drop its metadata namespace, sweep unreferenced
/// blobs, then flush the in-memory store (global — entries are content-keyed and shared
/// across servers, so the flush is deliberately heavy-handed: plan D10). Blocking I/O.
pub fn clear_server_image_cache(server: &str) {
    // Memory first: `clear()` aborts in-flight fetches, so none of them can land a
    // `write_entry` that re-persists into the namespace we are about to drop.
    image_store().clear();
    if let Some(cache) = disk_cache() {
        cache.clear_server(server);
    }
}

/// Startup housekeeping: drop namespaces of servers that no longer exist, trim to the
/// `image_cache_max_mb` budget (LRU by fetch time), sweep unreferenced blobs. Blocking
/// I/O — run on a background thread at app start.
pub fn startup_image_cache_sweep(keep_servers: &[String], max_mb: u64) {
    if let Some(cache) = disk_cache() {
        cache.startup_sweep(keep_servers, max_mb.saturating_mul(1024 * 1024));
    }
}

struct UiImageFetcher {
    client: reqwest::Client,
    cache: Option<http_cache::HttpImageCache>,
    decode_permits: Arc<Semaphore>,
    fetch_permits: Arc<Semaphore>,
}

impl UiImageFetcher {
    fn new() -> Self {
        // Dedicated client: no default headers, no cookies, explicit timeouts, and NO
        // automatic redirects — hops are followed (and policy-checked) manually.
        // One exception to "no default headers": a bare product token as User-Agent.
        // reqwest sends no UA at all by default, and UA-less requests trip WAF/CDN
        // bot rules (observed: CloudFront 403s on ropmud.com). Version-less on
        // purpose — package-chosen hosts must not learn the client version (the
        // same reason this client doesn't share the cloud client's headers).
        let client = reqwest::Client::builder()
            .user_agent("smudgy")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(std::time::Duration::from_secs(60))
            .build()
            .expect("a plain reqwest client always builds");
        let cache = smudgy_core::get_smudgy_home()
            .ok()
            .map(|home| http_cache::HttpImageCache::new(home.join("cache").join("images")));
        Self {
            client,
            cache,
            decode_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_DECODES)),
            fetch_permits: Arc::new(Semaphore::new(MAX_CONCURRENT_FETCHES)),
        }
    }

    async fn load_bytes(
        &self,
        source: &ResolvedImageSource,
        policy: &ImageSourcePolicy,
    ) -> Result<(Vec<u8>, Option<FileStamp>), FetchError> {
        match source {
            ResolvedImageSource::Remote(url) => {
                // Bound concurrent remote loads: a flood of distinct URLs (however it got
                // past the resolve-layer budgets) queues here instead of piling up
                // sockets and disk-cache writes.
                let _permit = self
                    .fetch_permits
                    .acquire()
                    .await
                    .map_err(|_| FetchError::transient("fetcher shut down"))?;
                let grants = (!policy.trusted).then(|| policy.net_grants.clone());
                let bytes = match &self.cache {
                    Some(cache) => {
                        cache
                            .fetch(&self.client, url, &policy.server_name, grants.as_ref())
                            .await?
                    }
                    None => http_cache::fetch_uncached(&self.client, url, grants.as_ref()).await?,
                };
                Ok((bytes, None))
            }
            ResolvedImageSource::LocalFile(path) => read_local(path, policy),
            ResolvedImageSource::Data { uri, .. } => {
                Ok((decode_data_uri(uri).map_err(FetchError::permanent)?, None))
            }
            ResolvedImageSource::PackageAsset {
                owner,
                name,
                version,
                subpath,
            } => load_package_asset(owner, name, version, subpath, policy).await,
        }
    }
}

/// Serve a membership-validated package asset (plan D4's `PackageAsset` arm):
///
/// 1. **Local dev-override** — the author's working tree at
///    `<server>/packages/<name>/<subpath>`: read from disk with the same
///    canonicalize-and-confine recheck as [`read_local`], and stamp for the freshness
///    probe (editing an asset repaints).
/// 2. **Installed** — content-addressed blob from the shared package cache, keyed by the
///    hash the load-time resolution metadata recorded for `subpath`.
/// 3. **Blob miss** — re-resolve the exact version over the network for a fresh signed
///    bundle URL, verify the hash matches the metadata, fetch that one body (decoded and
///    SHA-256-verified in the client), enforce the size cap, and cache the body forever
///    (immutable versions).
async fn load_package_asset(
    owner: &str,
    name: &str,
    version: &str,
    subpath: &str,
    policy: &ImageSourcePolicy,
) -> Result<(Vec<u8>, Option<FileStamp>), FetchError> {
    if policy.hosted_packages.is_local_override(owner, name) {
        return read_local_package_asset(&policy.packages_root, name, subpath);
    }

    let cache = PackageAssetCache::open();
    let cached_hash = cache
        .as_ref()
        .and_then(|c| c.module_hash(owner, name, version, subpath));
    if let (Some(cache), Some(hash)) = (&cache, &cached_hash)
        && let Some(bytes) = cache.read_blob_bytes(hash)
    {
        if bytes.len() as u64 > MAX_BODY_BYTES {
            return Err(FetchError::permanent(format!(
                "{owner}/{name}@{version}/{subpath} is {} bytes; images are capped at {MAX_BODY_BYTES}",
                bytes.len()
            )));
        }
        return Ok((bytes, None));
    }

    // Blob miss: a fresh resolve for an ephemeral signed bundle URL. Requires the client a
    // session registered; without one (headless tests) the asset is transiently absent.
    let Some(client) = package_client_slot().load_full() else {
        return Err(FetchError::transient(format!(
            "{owner}/{name}@{version}/{subpath}: asset not cached and no package client is available"
        )));
    };
    let wire = client
        .resolve_package(Some(owner), name, Some(version))
        .await
        .map_err(|err| FetchError::transient(format!("{owner}/{name}@{version}: {err}")))?;
    let module = wire
        .modules
        .iter()
        .find(|m| m.subpath == subpath)
        .ok_or_else(|| {
            FetchError::permanent(format!(
                "{owner}/{name}@{version} has no module {subpath:?}"
            ))
        })?;
    // The same refusal module_hash applies locally: a CODE module is not an image, and the
    // network fallback must not become the bypass (nor waste a resolve + download per app
    // run on a src typo that could never decode).
    if smudgy_core::session::runtime::image_assets::is_code_module(
        &module.media_type,
        &module.subpath,
    ) {
        return Err(FetchError::permanent(format!(
            "{owner}/{name}@{version}/{subpath} is a code module, not an image asset"
        )));
    }
    // The load-time metadata is the integrity anchor when present: a re-resolve of the
    // same immutable version must agree with what the package loaded as.
    if let Some(hash) = &cached_hash
        && !module.content_hash.eq_ignore_ascii_case(hash)
    {
        return Err(FetchError::permanent(format!(
            "{owner}/{name}@{version}/{subpath}: re-resolved content hash disagrees with the loaded resolution"
        )));
    }
    if u64::try_from(module.byte_size).is_ok_and(|size| size > MAX_BODY_BYTES) {
        return Err(FetchError::permanent(format!(
            "{owner}/{name}@{version}/{subpath} is {} bytes; images are capped at {MAX_BODY_BYTES}",
            module.byte_size
        )));
    }
    let bytes = client
        .fetch_body(&wire.bundle_url, &wire.bodies, &module.content_hash)
        .await
        .map_err(|err| {
            FetchError::transient(format!("{owner}/{name}@{version}/{subpath}: {err}"))
        })?;
    if bytes.len() as u64 > MAX_BODY_BYTES {
        return Err(FetchError::permanent(format!(
            "{owner}/{name}@{version}/{subpath} is {} bytes; images are capped at {MAX_BODY_BYTES}",
            bytes.len()
        )));
    }
    if let Some(cache) = &cache {
        cache.write_blob_bytes(&module.content_hash, &bytes);
    }
    Ok((bytes, None))
}

/// The on-disk path of a local dev-override package asset. Lexical only — callers either
/// stat it (probe) or canonicalize-and-confine before reading.
fn local_package_asset_path(packages_root: &Path, name: &str, subpath: &str) -> PathBuf {
    let mut path = packages_root.join(name);
    for part in subpath.split('/') {
        path.push(part);
    }
    path
}

/// Read a local dev-override package asset, re-verifying post-canonicalization that the
/// file stays inside the package's directory (the resolve-time check was lexical; symlinks
/// resolve here). Mirrors [`read_local`], confined to `<packages_root>/<name>/`.
fn read_local_package_asset(
    packages_root: &Path,
    name: &str,
    subpath: &str,
) -> Result<(Vec<u8>, Option<FileStamp>), FetchError> {
    let root = dunce::canonicalize(packages_root.join(name))
        .map_err(|e| FetchError::transient(format!("{name}: {e}")))?;
    let path = local_package_asset_path(packages_root, name, subpath);
    let canonical = dunce::canonicalize(&path)
        .map_err(|e| FetchError::transient(format!("{}: {e}", path.display())))?;
    if !canonical.starts_with(&root) {
        return Err(FetchError::permanent(format!(
            "{} escapes its package directory after resolving links",
            path.display()
        )));
    }
    let stamp = stamp_of(&canonical);
    if let Some(stamp) = &stamp
        && stamp.size > MAX_BODY_BYTES
    {
        return Err(FetchError::permanent(format!(
            "{} is {} bytes; images are capped at {MAX_BODY_BYTES}",
            canonical.display(),
            stamp.size
        )));
    }
    let bytes = std::fs::read(&canonical)
        .map_err(|e| FetchError::transient(format!("{}: {e}", canonical.display())))?;
    Ok((bytes, stamp))
}

impl ImageFetcher for UiImageFetcher {
    fn fetch(
        &self,
        source: ResolvedImageSource,
        policy: Arc<ImageSourcePolicy>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<DecodedImage, FetchError>> + Send + 'static>>
    {
        // `self` is behind an `Arc<dyn ImageFetcher>` owned by the store; clone the cheap
        // parts into the future instead of threading a self-Arc through the trait.
        let client = self.client.clone();
        let cache = self.cache.clone();
        let permits = self.decode_permits.clone();
        let fetch_permits = self.fetch_permits.clone();
        Box::pin(async move {
            let loader = UiImageFetcher {
                client,
                cache,
                decode_permits: permits.clone(),
                fetch_permits,
            };
            let (bytes, file_stamp) = loader.load_bytes(&source, &policy).await?;
            // Bound concurrent decodes; the permit spans only the CPU-bound decode.
            let _permit = permits
                .acquire()
                .await
                .map_err(|_| FetchError::transient("decoder shut down"))?;
            // Decode failures are permanent: the same bytes will fail the same way (a
            // *changed* remote image arrives as a different fetch, not a retry).
            let mut decoded = tokio::task::block_in_place(|| decode_and_orient(&bytes))
                .map_err(FetchError::permanent)?;
            decoded.file_stamp = file_stamp;
            Ok(decoded)
        })
    }

    fn probe(
        &self,
        source: ResolvedImageSource,
        policy: Arc<ImageSourcePolicy>,
    ) -> std::pin::Pin<Box<dyn Future<Output = Option<FileStamp>> + Send + 'static>> {
        Box::pin(async move {
            match &source {
                ResolvedImageSource::LocalFile(path) => stamp_of(path),
                // A local dev-override's assets are working-tree files: stat them so an
                // edited asset repaints (published assets are content-addressed, immutable).
                ResolvedImageSource::PackageAsset {
                    owner,
                    name,
                    subpath,
                    ..
                } if policy.hosted_packages.is_local_override(owner, name) => stamp_of(
                    &local_package_asset_path(&policy.packages_root, name, subpath),
                ),
                _ => None,
            }
        })
    }
}

fn stamp_of(path: &Path) -> Option<FileStamp> {
    let meta = std::fs::metadata(path).ok()?;
    Some(FileStamp {
        mtime: meta.modified().ok()?,
        size: meta.len(),
    })
}

/// Read a policy-checked local file, re-verifying confinement on the *canonical* path (the
/// build-op check was lexical; symlinks resolve here). Trusted isolates skip the grant
/// recheck — they hold unrestricted read anyway.
///
/// Error kinds: I/O failures are transient (the file may appear or become readable — local
/// dev sees this constantly); a grant escape or the size cap is permanent.
fn read_local(
    path: &Path,
    policy: &ImageSourcePolicy,
) -> Result<(Vec<u8>, Option<FileStamp>), FetchError> {
    let canonical = dunce::canonicalize(path)
        .map_err(|e| FetchError::transient(format!("{}: {e}", path.display())))?;
    if !policy.trusted {
        let canonical_roots: Vec<PathBuf> = policy
            .read_grants
            .iter()
            .filter_map(|root| dunce::canonicalize(root).ok())
            .collect();
        if !canonical_roots
            .iter()
            .any(|root| canonical.starts_with(root))
        {
            return Err(FetchError::permanent(format!(
                "{} escapes the granted read roots after resolving links",
                path.display()
            )));
        }
    }
    let stamp = stamp_of(&canonical);
    if let Some(stamp) = &stamp
        && stamp.size > MAX_BODY_BYTES
    {
        return Err(FetchError::permanent(format!(
            "{} is {} bytes; images are capped at {MAX_BODY_BYTES}",
            canonical.display(),
            stamp.size
        )));
    }
    let bytes = std::fs::read(&canonical)
        .map_err(|e| FetchError::transient(format!("{}: {e}", canonical.display())))?;
    Ok((bytes, stamp))
}

/// Decode a `data:image/...;base64,` URI's payload (shape + cap already validated at
/// resolve time; this decodes the actual bytes).
fn decode_data_uri(uri: &str) -> Result<Vec<u8>, String> {
    let payload = uri
        .split_once("base64,")
        .map(|(_, payload)| payload)
        .ok_or_else(|| "malformed data: URI".to_string())?;
    base64::engine::general_purpose::STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("data: URI base64: {e}"))
}

/// Decode encoded bytes to straight RGBA8 with explicit limits, then apply EXIF
/// orientation. Runs inside `block_in_place` under the decode semaphore.
fn decode_and_orient(bytes: &[u8]) -> Result<DecodedImage, String> {
    if bytes.len() as u64 > MAX_BODY_BYTES {
        return Err(format!(
            "image is {} bytes; capped at {MAX_BODY_BYTES}",
            bytes.len()
        ));
    }
    if looks_like_svg(bytes) {
        return Err("SVG sources are not supported yet (raster images only)".to_string());
    }
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("unrecognized image data: {e}"))?;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_DECODE_ALLOC_BYTES);
    reader.limits(limits);
    let decoded = reader.decode().map_err(|e| format!("decode: {e}"))?;
    // One conversion to RGBA8 (free when the codec already produced RGBA8), then orient
    // with in-place flips / single-copy rotations — `DynamicImage`-level combinators would
    // clone the full image once per step.
    use image::imageops;
    let mut rgba = decoded.into_rgba8();
    match exif_orientation(bytes) {
        Some(2) => imageops::flip_horizontal_in_place(&mut rgba),
        Some(3) => imageops::rotate180_in_place(&mut rgba),
        Some(4) => imageops::flip_vertical_in_place(&mut rgba),
        Some(5) => {
            rgba = imageops::rotate90(&rgba);
            imageops::flip_horizontal_in_place(&mut rgba);
        }
        Some(6) => rgba = imageops::rotate90(&rgba),
        Some(7) => {
            rgba = imageops::rotate270(&rgba);
            imageops::flip_horizontal_in_place(&mut rgba);
        }
        Some(8) => rgba = imageops::rotate270(&rgba),
        _ => {}
    }
    let (width, height) = rgba.dimensions();
    Ok(DecodedImage {
        width,
        height,
        rgba: rgba.into_raw(),
        file_stamp: None,
    })
}

/// The EXIF `Orientation` tag (1..=8), if the container carries EXIF (JPEG mainly).
fn exif_orientation(bytes: &[u8]) -> Option<u32> {
    let mut cursor = std::io::Cursor::new(bytes);
    let exif = exif::Reader::new().read_from_container(&mut cursor).ok()?;
    exif.get_field(exif::Tag::Orientation, exif::In::PRIMARY)?
        .value
        .get_uint(0)
}

/// Cheap SVG sniff on the leading bytes (post-whitespace/BOM `<` + "svg" nearby, or the
/// svgz gzip magic — svgz is svg, and both are rejected in v1).
fn looks_like_svg(bytes: &[u8]) -> bool {
    if bytes.starts_with(&[0x1f, 0x8b]) {
        // gzip: could be svgz. No raster format smudgy accepts is gzip-wrapped, so treat
        // any gzip payload as not-an-image (and say svg — the only plausible intent).
        return true;
    }
    let head = &bytes[..bytes.len().min(512)];
    let text = String::from_utf8_lossy(head);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with('<') && (trimmed.contains("<svg") || trimmed.contains("<?xml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png_1x1() -> Vec<u8> {
        // A valid 1x1 opaque-red PNG, generated with the image crate itself.
        let img = image::RgbaImage::from_pixel(1, 1, image::Rgba([255, 0, 0, 255]));
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    }

    #[test]
    fn decodes_png_to_rgba() {
        let decoded = decode_and_orient(&png_1x1()).unwrap();
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert_eq!(decoded.rgba, vec![255, 0, 0, 255]);
    }

    #[test]
    fn rejects_svg_and_svgz() {
        assert!(decode_and_orient(b"  <svg xmlns='x'></svg>").is_err());
        assert!(decode_and_orient(&[0x1f, 0x8b, 0x08, 0x00]).is_err());
    }

    #[test]
    fn data_uri_round_trips() {
        let png = png_1x1();
        let uri = format!(
            "data:image/png;base64,{}",
            base64::engine::general_purpose::STANDARD.encode(&png)
        );
        assert_eq!(decode_data_uri(&uri).unwrap(), png);
    }

    #[test]
    fn garbage_is_a_decode_error_not_a_panic() {
        assert!(decode_and_orient(&[0u8; 64]).is_err());
    }

    #[test]
    fn local_package_assets_stay_confined() {
        let dir = std::env::temp_dir().join(format!("smudgy-imgpkg-test-{}", std::process::id()));
        let packages = dir.join("packages");
        let pkg = packages.join("hud");
        std::fs::create_dir_all(pkg.join("assets")).unwrap();
        std::fs::write(pkg.join("assets").join("logo.png"), b"not-really-png").unwrap();
        std::fs::write(dir.join("outside.png"), b"outside").unwrap();

        // A validated subpath reads fine and stamps for the freshness probe.
        let (bytes, stamp) = read_local_package_asset(&packages, "hud", "assets/logo.png").unwrap();
        assert_eq!(bytes, b"not-really-png");
        assert!(stamp.is_some());

        // A symlink pointing out of the package directory fails the canonical confinement
        // recheck even though the lexical subpath looked fine.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                dir.join("outside.png"),
                pkg.join("assets").join("sneaky.png"),
            )
            .unwrap();
            let err = read_local_package_asset(&packages, "hud", "assets/sneaky.png")
                .expect_err("symlink escape must be refused");
            assert!(!err.transient, "confinement violations are permanent");
            assert!(err.reason.contains("escapes"));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
