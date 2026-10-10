//! Client for the cloud package-sharing + discovery API.
//!
//! [`PackageApiClient`] is the sibling of [`CloudApiClient`](crate::CloudApiClient):
//! the mapper covers area content, `CloudApiClient` covers identity/social/sharing,
//! and this covers shared **packages** — publish, resolve (a package address →
//! manifest + module sources), discovery search, ratings, comments, host alignment,
//! and package grants. All three share one [`CredentialSource`] and the
//! `{success, data, error}` envelope.
//!
//! A published package's name is global, so its modern address is `smudgy:@name`.
//! Owner-scoped `smudgy://owner/name` remains supported through 0.6.0 and the server
//! checks the owning user's nickname. Addresses are a *client-side*
//! construct: on the wire an address is an optional `owner` and a `name`, never the URI.
//! Every owner field on this wire is optional: a clan's package, and a package whose owner
//! has no nickname, has none.
//!
//! Module bodies travel and rest as zstd. A version's **bodies** are its distinct content
//! hashes in canonical order (first appearance in its module list); each travels as one
//! zstd **frame**, and a **bundle** is frames concatenated with nothing between or after
//! them. Publish uploads one bundle of every body; install and update download one bundle
//! of the bodies they lack from the signed URL a resolve carries, split it by the declared
//! frame sizes, and verify each decoded body against its uncompressed length and SHA-256.
//!
//! The contract here is **mirrored, not shared** with the server and the in-memory test
//! mock (`cloud/tests/integration_packages.rs`): a change to a wire shape must move all
//! three together.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use log::debug;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use zstd::zstd_safe::{self, DCtx, DParameter, InBuffer, OutBuffer};

use crate::{
    CloudError, CloudResult,
    backends::{Credential, CredentialSource},
};

/// Advertises support for [`AvailableWithSmudgyUpgrade`] and advisory-only discovery
/// results. Client-relative compatibility projection itself is keyed by the existing
/// client-version header, including for older clients. Keeping this package-specific
/// avoids sending registry protocol details to signed bundle URLs.
const PACKAGE_COMPATIBILITY_HEADER: &str = "x-smudgy-package-compatibility";
const PACKAGE_COMPATIBILITY_VERSION: &str = "1";

/// The zstd level publish compresses each body at.
const BODY_COMPRESSION_LEVEL: i32 = 19;

/// How long a publish whose upload met a body under garbage collection waits before it begins
/// again. A collection deletes the body's frame in the same sweep that marks it.
const COLLECTION_RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// The standard zstd frame magic number (`0xFD2FB528`) as it opens every body frame.
/// Skippable and legacy frames start with anything else.
const ZSTD_FRAME_MAGIC: [u8; 4] = [0x28, 0xB5, 0x2F, 0xFD];

/// Body frames declare a window of at most 16 MiB (`Window_Size` ≤ 2^24), which also
/// bounds the decoder's memory.
const MAX_FRAME_WINDOW_LOG: u32 = 24;

/// Decoded output is reserved up to this much ahead of time; anything larger grows as the
/// frame actually produces it, so a size the server merely claims never allocates.
const INITIAL_BODY_CAPACITY: usize = 1024 * 1024;

// ===========================================================================
// Wire types (mirror smudgy-api `src/models.rs` package DTOs)
// ===========================================================================

/// A newer package version that this client may advertise but cannot run until
/// Smudgy itself is upgraded. The server limits these advisories to its per-request
/// public advertising ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailableWithSmudgyUpgrade {
    pub package_version: String,
    pub minimum_smudgy_version: String,
}

/// Who owns a package: a user (the default, `owner_kind` omitted) or a clan.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageOwnerKind {
    #[default]
    User,
    Clan,
}

impl PackageOwnerKind {
    #[must_use]
    pub const fn is_user(&self) -> bool {
        matches!(self, Self::User)
    }
}

/// A package (`POST /packages`, `GET /packages/{id}`). Its owner is a user or, with
/// [`PackageOwnerKind::Clan`], the clan whose ID is `owner_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageView {
    pub id: Uuid,
    pub owner_id: Uuid,
    #[serde(default, skip_serializing_if = "PackageOwnerKind::is_user")]
    pub owner_kind: PackageOwnerKind,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub is_public: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The owner's nickname (omitted to the owner themselves, and for an owner without one,
    /// such as a clan).
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_nickname: Option<String>,
}

impl PackageView {
    /// Whether a clan owns this package.
    #[must_use]
    pub const fn is_clan_owned(&self) -> bool {
        matches!(self.owner_kind, PackageOwnerKind::Clan)
    }
}

/// An optional owner nickname: absent, `null` and `""` all mean the package has none.
fn optional_owner<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let owner: Option<String> = Option::deserialize(deserializer)?;
    Ok(owner.filter(|owner| !owner.is_empty()))
}

/// Full detail for one package (`GET /packages/{id}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageDetail {
    #[serde(flatten)]
    pub package: PackageView,
    /// The latest version selected for this caller (compatible on advisory-capable servers).
    #[serde(default)]
    pub latest_version: Option<String>,
    /// A newer release hidden behind a Smudgy version floor, when the client advertised
    /// advisory support.
    #[serde(default)]
    pub available_with_smudgy_upgrade: Option<AvailableWithSmudgyUpgrade>,
    #[serde(default)]
    pub version_count: i64,
    #[serde(default)]
    pub aligned_hosts: Vec<String>,
    #[serde(default)]
    pub avg_rating: Option<f64>,
    #[serde(default)]
    pub rating_count: i64,
    /// Unique resolvers (the popularity signal, excluding the author).
    #[serde(default)]
    pub install_count: i64,
    /// The latest non-yanked version's README (markdown), surfaced in discovery.
    #[serde(default)]
    pub readme: Option<String>,
    /// Whether the caller may administer (publish/align/share) this package.
    #[serde(default)]
    pub viewer_can_admin: bool,
}

/// One result from discovery search (`GET /packages/search`). Public packages only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageSearchResult {
    pub package_id: Uuid,
    /// The owner's nickname; `None` for an owner without one (a clan's package).
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_nickname: Option<String>,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The latest version selected for this caller (compatible on advisory-capable servers).
    #[serde(default)]
    pub latest_version: Option<String>,
    #[serde(default)]
    pub available_with_smudgy_upgrade: Option<AvailableWithSmudgyUpgrade>,
    #[serde(default)]
    pub aligned_hosts: Vec<String>,
    /// Whether the package is host-agnostic (no aligned hosts).
    #[serde(default)]
    pub host_agnostic: bool,
    #[serde(default)]
    pub avg_rating: Option<f64>,
    #[serde(default)]
    pub rating_count: i64,
    /// Unique resolvers (the popularity signal feeding the ranking).
    #[serde(default)]
    pub install_count: i64,
}

/// One published version (`GET /packages/{id}/versions`), newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VersionListItem {
    pub version: String,
    #[serde(default)]
    pub yanked: bool,
    /// True for a number that was published then hard-deleted: its content is gone (resolves
    /// 404) but the number is permanently reserved and can never be re-published. Consumers
    /// building a selectable/installable list must filter these out (and usually `yanked`);
    /// the owner UI shows them so authors see the number is spent. Defaulted for servers
    /// predating the field.
    #[serde(default)]
    pub deleted: bool,
    pub published_at: DateTime<Utc>,
}

/// A package resolved to a concrete version (`GET /packages/resolve`). This is the
/// install/auto-load path: the manifest, every module, and where to fetch their bodies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPackageWire {
    pub package_id: Uuid,
    /// The owner's nickname; `None` for an owner without one (a clan's package).
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_nickname: Option<String>,
    pub name: String,
    pub version: String,
    /// A newer release that becomes selectable after upgrading Smudgy.
    #[serde(default)]
    pub available_with_smudgy_upgrade: Option<AvailableWithSmudgyUpgrade>,
    /// The package manifest (`smudgy.package.json`) as stored. Parsed client-side into
    /// `smudgy_script::PackageManifest` by the runtime's package provider.
    #[serde(default)]
    pub manifest: Value,
    #[serde(default)]
    pub is_public: bool,
    #[serde(default)]
    pub aligned_hosts: Vec<String>,
    /// The resolved version's README (markdown), if any — for the inspect pane.
    #[serde(default)]
    pub readme: Option<String>,
    pub modules: Vec<ResolvedModuleWire>,
    /// The version's distinct bodies in canonical order: the order of the version's bundle
    /// and the index space of a bundle request's `want` bitmap.
    #[serde(default)]
    pub bodies: Vec<BundleBody>,
    /// A signed, expiring (15 minute) `GET` URL for the version's bodies as one bundle;
    /// [`PackageApiClient::fetch_bodies`] reads it.
    #[serde(default)]
    pub bundle_url: String,
    /// The resolved version's locked package dependencies (referrer-aware
    /// resolution). Servers predating the field omit it → empty (referrer-blind fallback).
    #[serde(default)]
    pub dependencies: Vec<ResolvedDependency>,
}

/// One distinct body of a resolved version. Its frame is `compressed_size` bytes of the
/// version's bundle and decodes to `byte_size` bytes hashing to `content_hash`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BundleBody {
    /// Lowercase-hex SHA-256 of the uncompressed body.
    pub content_hash: String,
    /// The uncompressed length.
    pub byte_size: u64,
    /// The length of the body's zstd frame.
    pub compressed_size: u64,
}

/// The relationship represented by a [`ResolvedDependency`].
///
/// An omitted kind is an old-server dependency edge and defaults to
/// [`Dependency`](Self::Dependency). Unknown future values fail to deserialize instead of
/// being treated as executable imports: that is the safe failure mode for a relation whose
/// runtime semantics are not known to this client.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DependencyKind {
    /// Import this package's code into the dependent package's isolate.
    #[default]
    Dependency,
    /// Install and run this package as its own top-level root; consume it through interop.
    Requires,
}

/// One locked package relationship carried on a [`ResolvedPackageWire`]: the target's
/// owner nickname (`None` when its owner has none), name, declared `range`, concrete locked
/// version, and relation kind.
/// Only [`DependencyKind::Dependency`] participates in the importing package's module,
/// permission, and version-solver closure. A [`DependencyKind::Requires`] edge instead names
/// a separately installed and executed root.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDependency {
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_nickname: Option<String>,
    pub name: String,
    #[serde(default)]
    pub range: String,
    pub resolved_version: String,
    /// Missing on older servers and cache records, where every returned edge was a code
    /// dependency. Defaults accordingly.
    #[serde(default)]
    pub kind: DependencyKind,
}

/// One module within a [`ResolvedPackageWire`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModuleWire {
    /// File path within the package, e.g. `index.ts`, `lib/util.ts`.
    pub subpath: String,
    /// Lowercase-hex SHA-256 of the uncompressed body; the key into the version's
    /// [`bodies`](ResolvedPackageWire::bodies).
    pub content_hash: String,
    #[serde(default = "default_media_type")]
    pub media_type: String,
    #[serde(default)]
    pub byte_size: i64,
    #[serde(default)]
    pub is_entry: bool,
}

/// One module to publish. [`PackageApiClient::publish_version`] runs the begin → upload →
/// finalize flow: the bodies travel as one zstd bundle to a signed URL, and the registry
/// API only ever sees each module's hash and sizes. `content` is raw bytes, so binaries
/// publish too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishModule {
    pub subpath: String,
    pub content: Vec<u8>,
    pub media_type: String,
    pub is_entry: bool,
}

/// One module's metadata in a publish request (`begin` + `finalize`). The body rides in
/// the bundle; only this metadata crosses the registry API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PublishModuleMeta {
    subpath: String,
    content_hash: String,
    byte_size: i64,
    /// The length of the body's frame in the bundle; equal for every module sharing a
    /// `content_hash`.
    compressed_size: u64,
    media_type: String,
    is_entry: bool,
}

/// `…/versions/begin` response: where to upload the publish's bundle.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct BeginVersionResponse {
    bundle: BundleUpload,
}

/// The bundle upload `begin` grants: PUT exactly `size` bytes to `url` with exactly
/// `headers`. The signed URL covers the publish, the declared size and an expiry.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct BundleUpload {
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    size: u64,
}

/// One publish's bodies, compressed: the bundle and each body's frame length, both in
/// canonical order.
struct CompressedBodies {
    bundle: Vec<u8>,
    frame_sizes: Vec<u64>,
}

/// A published version, metadata only (no module bodies).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PublishedVersionView {
    pub id: Uuid,
    pub package_id: Uuid,
    pub version: String,
    #[serde(default)]
    pub manifest: Value,
    pub modules: Vec<ModuleMetaView>,
    pub published_at: DateTime<Utc>,
}

/// Module metadata for a published version (no body).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleMetaView {
    pub subpath: String,
    pub content_hash: String,
    pub media_type: String,
    pub byte_size: i64,
    pub is_entry: bool,
}

/// One comment (`GET /packages/{id}/comments`), newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommentView {
    pub id: Uuid,
    pub user_id: Uuid,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_nickname: Option<String>,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

/// One friend-share grant on a package (owner's view; `GET/POST/DELETE
/// /packages/{id}/grants`). Either a specific friend (`grantee_id`/`grantee_nickname`)
/// or the dynamic `all_friends` grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageGrantView {
    pub id: Uuid,
    #[serde(default)]
    pub all_friends: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantee_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantee_nickname: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// One package dependency declared at publish — the client locks the declared range to a
/// concrete version and sends both (`…/versions/begin` and `…/versions/finalize`). The
/// owner is sent only when the manifest spelled the address `smudgy://owner/name`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishDependency {
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_nickname: Option<String>,
    pub name: String,
    pub range: String,
    pub resolved_version: String,
}

/// One entry in a package's share closure (`GET /packages/{id}/share-closure`): a
/// transitive `smudgy://` dependency and whether the prospective grantee can reach it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareClosureItem {
    pub package_id: Uuid,
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner_nickname: Option<String>,
    pub name: String,
    #[serde(default)]
    pub is_public: bool,
    /// The sharer owns this dep, so they can grant it to the same grantee.
    #[serde(default)]
    pub owned_by_sharer: bool,
    /// The grantee can already reach it (public, owned, or already shared).
    #[serde(default)]
    pub grantee_can_see: bool,
}

/// One stale dependency (`GET /packages/stale-deps`): a published version of the caller's
/// pins an older version of a dep that now has a newer release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaleDependencyView {
    pub package_id: Uuid,
    pub package_name: String,
    pub version: String,
    pub dep_handle: String,
    pub pinned_version: String,
    pub latest_version: String,
}

/// One lockfile install to check (`POST /packages/check-updates` request `entries`).
/// The server caps a request at 64 entries (400 beyond).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckUpdatesEntry {
    /// The owner segment of a `smudgy://owner/name` address; `None` for `smudgy:@name`.
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner: Option<String>,
    pub name: String,
    /// The installed (staged) version, when known — the server reports its
    /// yanked/deleted status in [`CheckUpdatesResult::installed`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
}

/// One closure node the client already holds cached metadata for (request `have`, capped
/// at 512): the server elides it from every result's `closure` — the client's meta cache
/// is immutable truth, so re-sending the manifest would be pure waste.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckUpdatesHave {
    /// Ignored by the server (a `have` row matches by name and version); sent when known.
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner: Option<String>,
    pub name: String,
    pub version: String,
}

/// `POST /packages/check-updates` response: one result per request entry, in request
/// order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckUpdatesResponse {
    #[serde(default)]
    pub results: Vec<CheckUpdatesResult>,
}

/// One entry's update-check verdict.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CheckUpdatesResult {
    /// The entry's owner, echoed; `None` when the entry had none.
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner: Option<String>,
    pub name: String,
    /// `"ok"`, or the uniform `"not_found"` — absent OR not visible to this viewer,
    /// indistinguishable by design (existence privacy), so it must never trigger
    /// destructive cleanup.
    pub status: String,
    /// Status of the entry's `installed` version; `None` when no version was sent or
    /// the server doesn't know it.
    #[serde(default)]
    pub installed: Option<UpdateCheckInstalled>,
    /// The newest non-yanked version selected for this caller. When no version is runnable,
    /// advisory-capable servers retain the absolute newest live version here as a deletion-safety
    /// fallback; `None` therefore continues to mean that no non-yanked live versions remain.
    #[serde(default)]
    pub latest: Option<UpdateCheckLatest>,
    /// A newer package version that the running Smudgy cannot use yet.
    #[serde(default)]
    pub available_with_smudgy_upgrade: Option<AvailableWithSmudgyUpgrade>,
    /// The distinct transitive nodes of `latest`'s dependency closure at their locked
    /// versions (walked over `kind = "dependency"` edges only), minus anything the
    /// request's `have` covered — and additionally filtered to viewer-visible packages,
    /// so it can be INCOMPLETE relative to the dependency edges. A caller must detect
    /// an uncoverable closure (an edge whose node is neither here nor in its own meta
    /// cache) rather than assume completeness. Always present (`[]` when empty).
    #[serde(default)]
    pub closure: Vec<UpdateCheckClosureNode>,
}

/// Yank/delete status of an installed version. `deleted` alone is not a cleanup
/// signal — only `deleted` combined with `latest: None` (no live versions remain) is
/// the definitive one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckInstalled {
    #[serde(default)]
    pub yanked: bool,
    /// Published then hard-deleted: the number stays permanently reserved but its
    /// content is gone (a resolve would 404).
    #[serde(default)]
    pub deleted: bool,
}

/// The newest live version selected for this caller in a check-updates result — or the absolute
/// newest live fallback when none is runnable — with everything a cached resolution needs
/// *except* a bundle URL (no signed URLs anywhere in this endpoint), so every part is
/// immutably cacheable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateCheckLatest {
    pub version: String,
    pub published_at: DateTime<Utc>,
    /// The manifest (`smudgy.package.json`) as stored, verbatim.
    #[serde(default)]
    pub manifest: Value,
    #[serde(default)]
    pub modules: Vec<ModuleMetaView>,
    /// The version's locked dependency edges — BOTH `kind = "dependency"` (import
    /// closure) and `kind = "requires"` (co-install) rows.
    #[serde(default)]
    pub dependencies: Vec<UpdateCheckDependency>,
}

/// One locked dependency edge in a check-updates result. Unlike [`ResolvedDependency`]
/// the owner field is named `owner` on this wire, and the edge carries its relation
/// `kind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpdateCheckDependency {
    /// The target's owner nickname; `None` when its owner has none.
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner: Option<String>,
    pub name: String,
    #[serde(default)]
    pub range: String,
    pub resolved_version: String,
    /// `"dependency"` (imported into the dependent's isolate) or `"requires"`
    /// (co-installed alongside it). An omitted kind defaults to `"dependency"` —
    /// the closure walks filter on that kind, and an elided field must never drop
    /// an edge from them.
    #[serde(default = "default_dependency_kind")]
    pub kind: String,
}

fn default_dependency_kind() -> String {
    "dependency".to_string()
}

/// One transitive node of a `latest`'s dependency closure: metadata for permission
/// folds and meta-cache writes, never for code loads — closure nodes carry no module
/// lists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UpdateCheckClosureNode {
    /// The node's owner nickname; `None` when its owner has none.
    #[serde(
        default,
        deserialize_with = "optional_owner",
        skip_serializing_if = "Option::is_none"
    )]
    pub owner: Option<String>,
    pub name: String,
    pub version: String,
    /// The node's manifest as stored, verbatim.
    #[serde(default)]
    pub manifest: Value,
    #[serde(default)]
    pub dependencies: Vec<UpdateCheckDependency>,
}

fn default_media_type() -> String {
    "text/plain".to_string()
}

/// Pick the highest published, non-yanked [`VersionListItem`] whose version satisfies
/// `range` (`None` = any version), returning its original version string. This is the
/// publish-time dep-lock picker: a manifest declares a `smudgy://` dependency as a semver
/// range and we record the concrete version it then resolves to.
///
/// Yanked and hard-deleted versions are skipped — a yanked one shouldn't be auto-locked
/// and a deleted one's content is gone (it would resolve 404). Versions that don't parse
/// as semver are skipped too. Returns `Ok(None)` when no eligible version satisfies `range`.
///
/// # Errors
/// Returns a [`semver::Error`] when `range` is `Some` but not a valid semver requirement.
pub fn highest_satisfying_version(
    versions: &[VersionListItem],
    range: Option<&str>,
) -> Result<Option<String>, semver::Error> {
    let req = match range {
        Some(raw) => semver::VersionReq::parse(raw)?,
        None => semver::VersionReq::STAR,
    };
    let mut best: Option<(semver::Version, &str)> = None;
    for item in versions {
        if item.yanked || item.deleted {
            continue;
        }
        let Ok(parsed) = semver::Version::parse(&item.version) else {
            continue;
        };
        if req.matches(&parsed) && best.as_ref().is_none_or(|(b, _)| parsed > *b) {
            best = Some((parsed, item.version.as_str()));
        }
    }
    Ok(best.map(|(_, version)| version.to_string()))
}

/// Discovery search scope (the MUD-specific / universal / both toggle).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchCategory {
    /// Aligned to the connected MUD + host-agnostic, ranked with the alignment boost.
    #[default]
    Both,
    /// Only packages aligned to the connected MUD.
    MudSpecific,
    /// Only host-agnostic (universal) packages.
    Universal,
}

impl SearchCategory {
    fn as_param(self) -> &'static str {
        match self {
            SearchCategory::Both => "both",
            SearchCategory::MudSpecific => "mud",
            SearchCategory::Universal => "universal",
        }
    }
}

// ===========================================================================
// Client
// ===========================================================================

/// HTTP client for the cloud package-sharing + discovery endpoints.
///
/// Whether a package request carries the caller's credential.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Auth {
    /// Credential required; error out if absent. Owned/shared/write/social
    /// endpoints (`mine`, `shared-with-me`, publish, ratings, comments, grants).
    Required,
    /// Send the credential when one is present, omit it otherwise. The public
    /// read surface (`search`, `resolve`, package detail, version list): the
    /// server treats a credential-less request as the anonymous, public-only
    /// viewer, so a logged-out client can browse + install + run public packages
    /// while a signed-in client still sees its private ones in the same call.
    Optional,
}

/// Cheap to clone; clones share the connection pool and the hot-swappable
/// [`CredentialSource`].
#[derive(Debug, Clone)]
pub struct PackageApiClient {
    client: Client,
    base_url: String,
    credentials: CredentialSource,
    upgrade_available: Arc<ArcSwap<Option<String>>>,
}

impl PackageApiClient {
    /// Creates a client for the API at `base_url` (trailing slashes trimmed) sharing
    /// the hot-swappable credential source.
    #[must_use]
    pub fn new(base_url: impl Into<String>, credentials: CredentialSource) -> Self {
        let mut base_url = base_url.into();
        base_url.truncate(base_url.trim_end_matches('/').len());
        Self {
            client: crate::versioned_http_client(),
            base_url,
            credentials,
            upgrade_available: Arc::new(ArcSwap::from_pointee(None)),
        }
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn credentials(&self) -> &CredentialSource {
        &self.credentials
    }

    /// The newest client version the server has advertised this session, if any.
    #[must_use]
    pub fn upgrade_available(&self) -> Option<String> {
        self.upgrade_available.load_full().as_ref().clone()
    }

    // ===== package operations =============================================

    /// Resolves the package `name` at `version` (`None`/`"latest"` = newest) to a
    /// concrete version, manifest, and module list (`GET /packages/resolve`). `owner` is
    /// the owner segment of a `smudgy://owner/name` address and `None` for
    /// `smudgy:@name`; the server checks its form and resolves by name either way.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a missing/unauthorized package (404), or
    /// transport/parse failure.
    pub async fn resolve_package(
        &self,
        owner: Option<&str>,
        name: &str,
        version: Option<&str>,
    ) -> CloudResult<ResolvedPackageWire> {
        let mut query = Vec::with_capacity(3);
        if let Some(owner) = owner.filter(|owner| !owner.is_empty()) {
            query.push(("owner", owner.to_string()));
        }
        query.push(("name", name.to_string()));
        query.push(("version", version.unwrap_or("latest").to_string()));
        self.get_with_query_public("/packages/resolve", &query)
            .await
    }

    /// Lists a package's published versions, newest first (`GET /packages/{id}/versions`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a missing/unauthorized package, or
    /// transport/parse failure.
    pub async fn list_versions(&self, package_id: Uuid) -> CloudResult<Vec<VersionListItem>> {
        self.get_public(&format!("/packages/{package_id}/versions"))
            .await
    }

    /// Searches public packages, optionally scoped to an aligned `host` (with host
    /// aliasing) and/or a keyword `q` (`GET /packages/search`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure or transport/parse failure.
    pub async fn search_packages(
        &self,
        host: Option<&str>,
        q: Option<&str>,
        category: SearchCategory,
    ) -> CloudResult<Vec<PackageSearchResult>> {
        let mut query: Vec<(&str, String)> = vec![("category", category.as_param().to_string())];
        if let Some(host) = host {
            query.push(("host", host.to_string()));
        }
        if let Some(q) = q {
            query.push(("q", q.to_string()));
        }
        self.get_with_query_public("/packages/search", &query).await
    }

    /// Full detail for one package (`GET /packages/{id}`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a missing/unauthorized package, or
    /// transport/parse failure.
    pub async fn get_package(&self, package_id: Uuid) -> CloudResult<PackageDetail> {
        self.get_public(&format!("/packages/{package_id}")).await
    }

    /// Packages the caller owns (`GET /packages/mine`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure or transport/parse failure.
    pub async fn list_my_packages(&self) -> CloudResult<Vec<PackageDetail>> {
        self.get("/packages/mine").await
    }

    /// Packages shared with the caller by friends (`GET /packages/shared-with-me`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure or transport/parse failure.
    pub async fn list_shared_packages(&self) -> CloudResult<Vec<PackageDetail>> {
        self.get("/packages/shared-with-me").await
    }

    /// Creates (or returns) the caller's package `name` (`POST /packages`). Draft names
    /// are unique within an owner. The first publication reserves the global name.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a verification gate (403),
    /// invalid input, or
    /// transport/parse failure.
    pub async fn create_package(&self, name: &str, description: &str) -> CloudResult<PackageView> {
        let body = json!({ "name": name, "description": description });
        self.post("/packages", Some(&body)).await
    }

    /// Creates (or returns) a package `name` the clan `clan_id` owns (`POST /packages`
    /// with `clan_id`), for a member holding `package.create` in it.
    ///
    /// # Errors
    /// The errors of [`Self::create_package`]; a clan the caller is not in, or holds no
    /// `package.create` in, is [`CloudError::NotFoundOrNoAccess`].
    pub async fn create_clan_package(
        &self,
        clan_id: Uuid,
        name: &str,
        description: &str,
    ) -> CloudResult<PackageView> {
        let body = json!({ "name": name, "description": description, "clan_id": clan_id });
        self.post("/packages", Some(&body)).await
    }

    /// Edits a package's description and/or visibility (`PATCH /packages/{id}`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport/parse failure.
    pub async fn patch_package(
        &self,
        package_id: Uuid,
        description: Option<&str>,
        is_public: Option<bool>,
    ) -> CloudResult<PackageView> {
        let mut body = serde_json::Map::new();
        if let Some(description) = description {
            body.insert("description".into(), json!(description));
        }
        if let Some(is_public) = is_public {
            body.insert("is_public".into(), json!(is_public));
        }
        self.patch(&format!("/packages/{package_id}"), &Value::Object(body))
            .await
    }

    /// Publishes an immutable version via the begin → upload → finalize flow: each distinct
    /// body is compressed once, `begin` validates the metadata and grants one signed bundle
    /// upload, the client PUTs every body as one bundle, then `finalize` commits. Module
    /// bodies are arbitrary bytes (binaries publish too).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, a cap including the bundle
    /// size cap ([`CloudError::TooLarge`]), a duplicate version (409), an upload/integrity
    /// failure, [`CloudError::BodyBeingCollected`] when garbage collection refused the upload
    /// twice, or transport/parse failure.
    pub async fn publish_version(
        &self,
        package_id: Uuid,
        version: &str,
        manifest: &Value,
        modules: &[PublishModule],
        dependencies: &[PublishDependency],
        readme: Option<&str>,
    ) -> CloudResult<PublishedVersionView> {
        self.publish_version_checked(
            package_id,
            version,
            manifest,
            modules,
            dependencies,
            readme,
            || Ok(()),
        )
        .await
    }

    /// Publishes an immutable version and runs `pre_finalize` after the bundle upload,
    /// immediately before the irreversible finalize request. A caller that builds the payload from
    /// mutable local state can use this hook to prove that state still matches its snapshot.
    ///
    /// # Errors
    /// Returns the same errors as [`Self::publish_version`], or the error returned by
    /// `pre_finalize`. Uploaded content-addressed bodies can remain unreferenced when that check
    /// fails; no package version has been committed at that point.
    // This mirrors `publish_version`'s wire fields and adds exactly one lifecycle hook. Bundling
    // the established public arguments into a second request type would make the two APIs drift.
    #[allow(clippy::too_many_arguments)]
    pub async fn publish_version_checked<F>(
        &self,
        package_id: Uuid,
        version: &str,
        manifest: &Value,
        modules: &[PublishModule],
        dependencies: &[PublishDependency],
        readme: Option<&str>,
        pre_finalize: F,
    ) -> CloudResult<PublishedVersionView>
    where
        F: FnOnce() -> CloudResult<()>,
    {
        // Hash every module, then gather the distinct bodies in canonical order (first
        // appearance in the module list): the bundle's frame order.
        let hashes: Vec<String> = modules.iter().map(|m| sha256_hex(&m.content)).collect();
        let mut body_index: HashMap<&str, usize> = HashMap::new();
        let mut sources: Vec<Vec<u8>> = Vec::new();
        for (module, hash) in modules.iter().zip(&hashes) {
            if !body_index.contains_key(hash.as_str()) {
                body_index.insert(hash, sources.len());
                sources.push(module.content.clone());
            }
        }
        // Level-19 compression of a large package takes seconds; it runs on the blocking
        // pool, never on the caller's executor thread.
        let compressed = tokio::task::spawn_blocking(move || compress_bodies(&sources))
            .await
            .map_err(|error| {
                CloudError::InternalError(format!("package body compression stopped: {error}"))
            })??;

        let metas: Vec<PublishModuleMeta> = modules
            .iter()
            .zip(&hashes)
            .map(|(m, content_hash)| PublishModuleMeta {
                subpath: m.subpath.clone(),
                content_hash: content_hash.clone(),
                byte_size: i64::try_from(m.content.len()).unwrap_or(i64::MAX),
                compressed_size: compressed.frame_sizes[body_index[content_hash.as_str()]],
                media_type: m.media_type.clone(),
                is_entry: m.is_entry,
            })
            .collect();
        let body = json!({
            "version": version,
            "manifest": manifest,
            "modules": metas,
            "dependencies": dependencies,
            "readme": readme,
        });

        // 1. begin — validate and get the signed bundle upload; 2. upload every body as one
        // bundle. An upload that meets a body garbage collection is deleting stores nothing,
        // and once the collection finishes a fresh begin stores that body anew, so the first
        // such refusal begins the publish again.
        let bundle_len = u64::try_from(compressed.bundle.len()).unwrap_or(u64::MAX);
        let mut bundle = compressed.bundle;
        let mut retried = false;
        loop {
            let begin: BeginVersionResponse = self
                .post(
                    &format!("/packages/{package_id}/versions/begin"),
                    Some(&body),
                )
                .await?;
            if begin.bundle.size != bundle_len {
                return Err(CloudError::SerializationError(format!(
                    "the server expects a {}-byte package bundle, but this publish's bundle is {bundle_len} bytes",
                    begin.bundle.size
                )));
            }
            // The first attempt keeps the bundle for the retry; the retry sends it.
            let attempt = if retried {
                std::mem::take(&mut bundle)
            } else {
                bundle.clone()
            };
            match self.upload_bundle(&begin.bundle, attempt).await {
                Ok(()) => break,
                Err(CloudError::BodyBeingCollected) if !retried => {
                    retried = true;
                    tokio::time::sleep(COLLECTION_RETRY_DELAY).await;
                }
                Err(error) => return Err(error),
            }
        }

        // 3. finalize — commit the bodies the bundle recorded.
        pre_finalize()?;
        self.post(
            &format!("/packages/{package_id}/versions/finalize"),
            Some(&body),
        )
        .await
    }

    /// PUTs a publish's bundle to its signed URL with exactly the headers `begin` named. No
    /// `Authorization` and no registry headers: the URL itself carries the grant.
    async fn upload_bundle(&self, upload: &BundleUpload, bundle: Vec<u8>) -> CloudResult<()> {
        debug!("PUT <package bundle>");
        let mut request = self.client.put(&upload.url).body(bundle);
        for (name, value) in &upload.headers {
            request = request.header(name, value);
        }
        let response = request.send().await?;
        let status = response.status();
        if status.is_success() {
            return Ok(());
        }
        // A refused frame is a 400 naming the module's subpath; keep that message. A body
        // under garbage collection is a 409 `body_being_collected`; servers that predate the
        // code answer a 500 `body … is being collected; retry the upload`.
        match Self::error_for(status.as_u16(), response).await {
            CloudError::NetworkError(message)
                if status.as_u16() == 500 && message.contains("is being collected") =>
            {
                Err(CloudError::BodyBeingCollected)
            }
            error => Err(error),
        }
    }

    /// Yanks or un-yanks a published version (`PATCH /packages/{id}/versions/{version}`);
    /// a yanked version drops out of latest/search but stays resolvable by an exact pin.
    /// Returns the updated version list.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, a missing version, or
    /// transport/parse failure.
    pub async fn set_version_yanked(
        &self,
        package_id: Uuid,
        version: &str,
        yanked: bool,
    ) -> CloudResult<Vec<VersionListItem>> {
        let body = json!({ "yanked": yanked });
        let response = self
            .send(
                Method::PATCH,
                &format!("/packages/{package_id}/versions/{version}"),
                &[],
                Some(&body),
                Auth::Required,
            )
            .await?;
        Self::parse_data(response).await
    }

    /// Hard-deletes a published version (`DELETE /packages/{id}/versions/{version}`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport failure.
    pub async fn delete_version(&self, package_id: Uuid, version: &str) -> CloudResult<()> {
        self.delete(&format!("/packages/{package_id}/versions/{version}"))
            .await
    }

    /// Hard-deletes a whole package and all its versions (`DELETE /packages/{id}`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport failure.
    pub async fn delete_package(&self, package_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/packages/{package_id}")).await
    }

    /// Sets the caller's 1–5 star rating for a public package (`PUT /packages/{id}/rating`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a missing/unauthorized package, or
    /// transport/parse failure.
    pub async fn rate_package(&self, package_id: Uuid, stars: i16) -> CloudResult<PackageDetail> {
        let body = json!({ "stars": stars });
        let response = self
            .send(
                Method::PUT,
                &format!("/packages/{package_id}/rating"),
                &[],
                Some(&body),
                Auth::Required,
            )
            .await?;
        Self::parse_data(response).await
    }

    /// Removes the caller's rating (`DELETE /packages/{id}/rating`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure or transport failure.
    pub async fn unrate_package(&self, package_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/packages/{package_id}/rating")).await
    }

    /// Lists a package's comments, newest first (`GET /packages/{id}/comments`).
    /// Part of the public read surface: a logged-out client reads a public
    /// package's discussion (anonymous = public-only); posting still needs auth.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on a missing/unauthorized package or transport/parse failure.
    pub async fn list_comments(&self, package_id: Uuid) -> CloudResult<Vec<CommentView>> {
        self.get_public(&format!("/packages/{package_id}/comments"))
            .await
    }

    /// Adds a comment (`POST /packages/{id}/comments`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a missing/unauthorized package, or
    /// transport/parse failure.
    pub async fn add_comment(&self, package_id: Uuid, body: &str) -> CloudResult<CommentView> {
        let payload = json!({ "body": body });
        self.post(&format!("/packages/{package_id}/comments"), Some(&payload))
            .await
    }

    /// Declares an aligned MUD host for a package (`POST /packages/{id}/hosts`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport/parse failure.
    pub async fn add_host(&self, package_id: Uuid, host: &str) -> CloudResult<Vec<String>> {
        let body = json!({ "host": host });
        self.post(&format!("/packages/{package_id}/hosts"), Some(&body))
            .await
    }

    /// Removes a host alignment (`DELETE /packages/{id}/hosts/{mud_host_id}`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport failure.
    pub async fn remove_host(&self, package_id: Uuid, mud_host_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/packages/{package_id}/hosts/{mud_host_id}"))
            .await
    }

    // ===== friend-sharing grants ==========================================

    /// Lists who a private package is shared with (owner-only;
    /// `GET /packages/{id}/grants`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport/parse failure.
    pub async fn list_grants(&self, package_id: Uuid) -> CloudResult<Vec<PackageGrantView>> {
        self.get(&format!("/packages/{package_id}/grants")).await
    }

    /// Shares a private package with a specific friend (`POST /packages/{id}/grants`);
    /// returns the updated grant list. A non-friend / blocked grantee is a uniform 404.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership/non-friend (404), or
    /// transport/parse failure.
    pub async fn share_with_friend(
        &self,
        package_id: Uuid,
        grantee_id: Uuid,
    ) -> CloudResult<Vec<PackageGrantView>> {
        let body = json!({ "grantee_id": grantee_id });
        self.post(&format!("/packages/{package_id}/grants"), Some(&body))
            .await
    }

    /// Shares a private package with all current + future friends (a dynamic grant);
    /// returns the updated grant list.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport/parse failure.
    pub async fn share_with_all_friends(
        &self,
        package_id: Uuid,
    ) -> CloudResult<Vec<PackageGrantView>> {
        let body = json!({ "all_friends": true });
        self.post(&format!("/packages/{package_id}/grants"), Some(&body))
            .await
    }

    /// Revokes a grant by id (`DELETE /packages/{id}/grants/{grant_id}`); returns the
    /// updated grant list.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport/parse failure.
    pub async fn revoke_grant(
        &self,
        package_id: Uuid,
        grant_id: Uuid,
    ) -> CloudResult<Vec<PackageGrantView>> {
        let response = self
            .send(
                Method::DELETE,
                &format!("/packages/{package_id}/grants/{grant_id}"),
                &[],
                None,
                Auth::Required,
            )
            .await?;
        Self::parse_data(response).await
    }

    /// Previews a package's share closure against a prospective grantee — each transitive
    /// `smudgy://` dep and whether the grantee can reach it
    /// (`GET /packages/{id}/share-closure?grantee_id=`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, or transport/parse failure.
    pub async fn share_closure(
        &self,
        package_id: Uuid,
        grantee_id: Uuid,
    ) -> CloudResult<Vec<ShareClosureItem>> {
        let query = [("grantee_id", grantee_id.to_string())];
        self.get_with_query(&format!("/packages/{package_id}/share-closure"), &query)
            .await
    }

    /// Lists the caller's published packages whose pinned `smudgy://` deps are behind a
    /// newer release (`GET /packages/stale-deps`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure or transport/parse failure.
    pub async fn stale_deps(&self) -> CloudResult<Vec<StaleDependencyView>> {
        self.get("/packages/stale-deps").await
    }

    /// Checks a whole lockfile's installs for updates in one call
    /// (`POST /packages/check-updates`). Part of the public read surface: anonymous
    /// callers get the public-only view, per-entry, as the uniform `"not_found"`
    /// status — the same visibility rule as `resolve`. Results come back in request
    /// order; `have` lists closure nodes the caller already holds cached metadata for,
    /// which the server elides from every `closure`. Server caps: 64 `entries`, 512
    /// `have` (400 beyond either).
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] means the server predates the route (the
    /// endpoint reports per-entry misses as `status: "not_found"` results, never as an
    /// HTTP 404) — callers fall back to the legacy per-package `resolve` +
    /// `list_versions` probe. [`CloudError::InvalidInput`] for an over-cap request;
    /// otherwise transport/parse failures.
    pub async fn check_updates(
        &self,
        entries: &[CheckUpdatesEntry],
        have: &[CheckUpdatesHave],
    ) -> CloudResult<CheckUpdatesResponse> {
        let body = json!({ "entries": entries, "have": have });
        self.post_public("/packages/check-updates", Some(&body))
            .await
    }

    /// Fetches the bodies named by `content_hashes` from a resolved version's bundle
    /// (`bundle_url` and `bodies` from [`ResolvedPackageWire`]) in one request, and returns
    /// each uncompressed body keyed by its content hash as `content_hashes` spells it: every
    /// requested hash has an entry.
    ///
    /// The request selects exactly the named bodies (a `want` bitmap unless that is all of
    /// them), and is not sent when one declares a frame larger than zstd ever makes for its
    /// size. The response is split by the declared frame sizes; each frame must be one
    /// standard zstd frame recording its content size, is decoded without ever producing
    /// more than the body's declared `byte_size`, and must match that length and its
    /// SHA-256. Duplicate hashes are fetched once; no hashes means no request. The URL is
    /// signed, so no `Authorization` header is sent.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] for a bad or expired bundle URL;
    /// [`CloudError::SerializationError`] for a hash the version lists no body for, an
    /// impossible frame size, a response of the wrong length, a malformed or oversized
    /// frame, or an integrity mismatch (`"... integrity mismatch ..."`); otherwise
    /// transport errors.
    pub async fn fetch_bodies(
        &self,
        bundle_url: &str,
        bodies: &[BundleBody],
        content_hashes: &[&str],
    ) -> CloudResult<HashMap<String, Vec<u8>>> {
        // Each selected body remembers the spelling it was asked for by, which keys its result.
        let mut requested: Vec<Option<&str>> = vec![None; bodies.len()];
        for &hash in content_hashes {
            let index = bodies
                .iter()
                .position(|body| body.content_hash.eq_ignore_ascii_case(hash))
                .ok_or_else(|| {
                    CloudError::SerializationError(format!(
                        "the resolved version lists no body for content hash {hash}"
                    ))
                })?;
            requested[index].get_or_insert(hash);
        }
        let selected: Vec<bool> = requested.iter().map(Option::is_some).collect();
        if !selected.contains(&true) {
            return Ok(HashMap::new());
        }
        let wanted: Vec<(&str, &BundleBody)> = requested
            .iter()
            .zip(bodies)
            .filter_map(|(key, body)| key.map(|key| (key, body)))
            .collect();
        // No zstd frame of a body is larger than this bound, so a larger declared size is
        // refused before a byte of it is read.
        if let Some((_, body)) = wanted
            .iter()
            .find(|(_, body)| body.compressed_size > compress_bound(body.byte_size))
        {
            return Err(CloudError::SerializationError(format!(
                "package body {} declares a {}-byte frame, more than any frame of {} bytes",
                body.content_hash, body.compressed_size, body.byte_size
            )));
        }
        let expected = wanted
            .iter()
            .try_fold(0u64, |total, (_, body)| {
                total.checked_add(body.compressed_size)
            })
            .ok_or_else(|| {
                CloudError::SerializationError(
                    "the resolved version's frame sizes overflow".to_string(),
                )
            })?;

        debug!("GET <package bundle>");
        let url = match want_bitmap(&selected) {
            Some(want) => bundle_request_url(bundle_url, &want),
            None => bundle_url.to_string(),
        };
        let mut response = self.client.get(&url).send().await?;
        if !response.status().is_success() {
            return Err(CloudError::from_status(
                response.status().as_u16(),
                "failed to fetch package bundle",
            ));
        }
        if let Some(length) = response.content_length()
            && length != expected
        {
            return Err(bundle_length_error(expected, length));
        }
        let mut bundle = Vec::with_capacity(
            usize::try_from(expected)
                .unwrap_or(usize::MAX)
                .min(INITIAL_BODY_CAPACITY),
        );
        while let Some(chunk) = response.chunk().await? {
            let received = u64::try_from(bundle.len() + chunk.len()).unwrap_or(u64::MAX);
            if received > expected {
                return Err(bundle_length_error(expected, received));
            }
            bundle.extend_from_slice(&chunk);
        }
        let received = u64::try_from(bundle.len()).unwrap_or(u64::MAX);
        if received != expected {
            return Err(bundle_length_error(expected, received));
        }

        let mut fetched = HashMap::with_capacity(wanted.len());
        let mut rest = bundle.as_slice();
        for (key, body) in wanted {
            // The total matched, so every declared frame fits in what remains.
            let (frame, tail) = rest.split_at(usize::try_from(body.compressed_size).unwrap_or(0));
            rest = tail;
            fetched.insert(key.to_string(), decode_body_frame(frame, body)?);
        }
        Ok(fetched)
    }

    /// Fetches one body from a resolved version's bundle: [`Self::fetch_bodies`] for a
    /// single content hash.
    ///
    /// # Errors
    /// The same errors as [`Self::fetch_bodies`].
    pub async fn fetch_body(
        &self,
        bundle_url: &str,
        bodies: &[BundleBody],
        content_hash: &str,
    ) -> CloudResult<Vec<u8>> {
        self.fetch_bodies(bundle_url, bodies, &[content_hash])
            .await?
            .into_values()
            .next()
            .ok_or_else(|| {
                CloudError::SerializationError(format!(
                    "the package bundle did not carry body {content_hash}"
                ))
            })
    }

    // ===== internal plumbing ==============================================

    fn credential(&self) -> CloudResult<Credential> {
        self.credentials
            .get()
            .ok_or_else(|| CloudError::Unauthorized("no credential configured".to_string()))
    }

    /// Sends a request. Bodies are never logged (they may carry option/secret
    /// values); only the URL (sans query) and the response status, at debug
    /// level. Most package endpoints require a credential; the public read
    /// surface ([`Auth::Optional`]) sends one only when present. Signed bundle
    /// URLs are fetched separately in [`Self::fetch_bodies`].
    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
        auth: Auth,
    ) -> CloudResult<reqwest::Response> {
        let url = format!("{}{}", self.base_url, path);
        debug!("{method} {url}");

        let mut request = self.client.request(method.clone(), &url);
        request = request.header(PACKAGE_COMPATIBILITY_HEADER, PACKAGE_COMPATIBILITY_VERSION);
        let credential = match auth {
            Auth::Required => Some(self.credential()?),
            Auth::Optional => self.credentials.get(),
        };
        if let Some(credential) = &credential {
            request = request.header("authorization", credential.header_value());
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await?;
        debug!("{method} {url} - {}", response.status());
        if let Some(credential) = &credential
            && response.status() == reqwest::StatusCode::UNAUTHORIZED
        {
            self.credentials.note_refused(credential);
        }

        if let Some(newest) = response
            .headers()
            .get("x-smudgy-upgrade-available")
            .and_then(|value| value.to_str().ok())
        {
            self.upgrade_available
                .store(Arc::new(Some(newest.to_owned())));
        }

        Ok(response)
    }

    async fn parse_data<T>(response: reqwest::Response) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let status = response.status();
        if status.is_success() {
            let mut envelope: Value = response.json().await?;
            match envelope.get_mut("data") {
                Some(data) => Ok(serde_json::from_value(data.take())?),
                None => Err(CloudError::SerializationError(
                    "missing data field in response envelope".to_string(),
                )),
            }
        } else {
            Err(Self::error_for(status.as_u16(), response).await)
        }
    }

    async fn parse_unit(response: reqwest::Response) -> CloudResult<()> {
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(Self::error_for(status.as_u16(), response).await)
        }
    }

    async fn error_for(status: u16, response: reqwest::Response) -> CloudError {
        let text = response.text().await.unwrap_or_default();
        let message = serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            })
            .unwrap_or(text);
        CloudError::from_status(status, &message)
    }

    async fn get<T>(&self, path: &str) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.get_with_query(path, &[]).await
    }

    async fn get_with_query<T>(&self, path: &str, query: &[(&str, String)]) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self
            .send(Method::GET, path, query, None, Auth::Required)
            .await?;
        Self::parse_data(response).await
    }

    /// Like [`Self::get`] but for the public read surface: sends the credential
    /// only when signed in, so a logged-out client gets the anonymous,
    /// public-only view rather than an `Unauthorized` short-circuit.
    async fn get_public<T>(&self, path: &str) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        self.get_with_query_public(path, &[]).await
    }

    /// Public-read counterpart of [`Self::get_with_query`] (see [`Self::get_public`]).
    async fn get_with_query_public<T>(&self, path: &str, query: &[(&str, String)]) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self
            .send(Method::GET, path, query, None, Auth::Optional)
            .await?;
        Self::parse_data(response).await
    }

    async fn post<T>(&self, path: &str, body: Option<&Value>) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self
            .send(Method::POST, path, &[], body, Auth::Required)
            .await?;
        Self::parse_data(response).await
    }

    /// Public-read counterpart of [`Self::post`] (see [`Self::get_public`]): sends the
    /// credential only when signed in, so a logged-out client gets the anonymous,
    /// public-only view rather than an `Unauthorized` short-circuit.
    async fn post_public<T>(&self, path: &str, body: Option<&Value>) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self
            .send(Method::POST, path, &[], body, Auth::Optional)
            .await?;
        Self::parse_data(response).await
    }

    async fn patch<T>(&self, path: &str, body: &Value) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self
            .send(Method::PATCH, path, &[], Some(body), Auth::Required)
            .await?;
        Self::parse_data(response).await
    }

    async fn delete(&self, path: &str) -> CloudResult<()> {
        let response = self
            .send(Method::DELETE, path, &[], None, Auth::Required)
            .await?;
        Self::parse_unit(response).await
    }
}

/// Lowercase-hex SHA-256 of `bytes`.
fn sha256_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::with_capacity(64), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Compresses each body into one zstd frame at [`BODY_COMPRESSION_LEVEL`], with a content
/// checksum and no dictionary, and lays the frames end to end. Single-shot compression
/// pledges the whole source, so every frame records its content size.
fn compress_bodies(sources: &[Vec<u8>]) -> CloudResult<CompressedBodies> {
    let compression_error = |error: std::io::Error| {
        CloudError::InternalError(format!("package body compression: {error}"))
    };
    let mut compressor =
        zstd::bulk::Compressor::new(BODY_COMPRESSION_LEVEL).map_err(compression_error)?;
    compressor
        .set_parameter(zstd_safe::CParameter::ChecksumFlag(true))
        .map_err(compression_error)?;
    compressor
        .set_parameter(zstd_safe::CParameter::ContentSizeFlag(true))
        .map_err(compression_error)?;
    let mut bundle = Vec::new();
    let mut frame_sizes = Vec::with_capacity(sources.len());
    for source in sources {
        let frame = compressor.compress(source).map_err(compression_error)?;
        frame_sizes.push(u64::try_from(frame.len()).unwrap_or(u64::MAX));
        bundle.extend_from_slice(&frame);
    }
    Ok(CompressedBodies {
        bundle,
        frame_sizes,
    })
}

/// The largest frame zstd produces for `byte_size` bytes of content (libzstd's
/// `ZSTD_COMPRESSBOUND`): `n + (n >> 8)`, plus `(131072 - n) >> 11` below 128 KiB. The
/// registry refuses any body declaring a larger frame, and so does the client.
fn compress_bound(byte_size: u64) -> u64 {
    const SMALL_SOURCE_LIMIT: u64 = 128 * 1024;
    let margin = if byte_size < SMALL_SOURCE_LIMIT {
        (SMALL_SOURCE_LIMIT - byte_size) >> 11
    } else {
        0
    };
    byte_size
        .saturating_add(byte_size >> 8)
        .saturating_add(margin)
}

/// The `want` value selecting the `selected` bodies: lowercase hex of a bitset in which bit
/// `i` selects `bodies[i]`, byte 0 holding bits 0–7 least significant bit first, trailing
/// zero bytes omitted. `None` when every body is selected, which is what no `want` means.
fn want_bitmap(selected: &[bool]) -> Option<String> {
    use std::fmt::Write as _;
    if selected.iter().all(|&chosen| chosen) {
        return None;
    }
    let mut bits = vec![0u8; selected.len().div_ceil(8)];
    for (index, _) in selected.iter().enumerate().filter(|(_, chosen)| **chosen) {
        bits[index / 8] |= 1 << (index % 8);
    }
    while bits.last() == Some(&0) {
        bits.pop();
    }
    Some(
        bits.iter()
            .fold(String::with_capacity(bits.len() * 2), |mut out, byte| {
                let _ = write!(out, "{byte:02x}");
                out
            }),
    )
}

/// `bundle_url` with `want` appended as one more query parameter.
fn bundle_request_url(bundle_url: &str, want: &str) -> String {
    let separator = if bundle_url.contains('?') { '&' } else { '?' };
    format!("{bundle_url}{separator}want={want}")
}

fn bundle_length_error(expected: u64, received: u64) -> CloudError {
    CloudError::SerializationError(format!(
        "the package bundle is {received} bytes where its frames total {expected}"
    ))
}

/// The window a frame header declares (RFC 8878 §3.1.1.1.2): its content size when the
/// frame is a single segment, otherwise what its `Window_Descriptor` encodes. `None` for a
/// header too short to say.
fn frame_window_size(frame: &[u8], content_size: u64) -> Option<u64> {
    let frame_header_descriptor = *frame.get(4)?;
    if frame_header_descriptor & 0x20 != 0 {
        return Some(content_size);
    }
    let window_descriptor = *frame.get(5)?;
    let base = 1u64 << (10 + u32::from(window_descriptor >> 3));
    Some(base + base / 8 * u64::from(window_descriptor & 0x07))
}

/// Decodes one body's frame and verifies it: exactly one standard zstd frame filling the
/// segment, recording a content size equal to the body's `byte_size`, needing no dictionary,
/// with a window of at most 16 MiB. Decoding stops the moment output would pass `byte_size`,
/// whatever the frame claims, and the result must match `byte_size` and `content_hash`.
fn decode_body_frame(frame: &[u8], body: &BundleBody) -> CloudResult<Vec<u8>> {
    let malformed = |reason: &str| {
        CloudError::SerializationError(format!(
            "package body {} is not a valid frame: {reason}",
            body.content_hash
        ))
    };
    if !frame.starts_with(&ZSTD_FRAME_MAGIC) {
        return Err(malformed("it does not start a standard zstd frame"));
    }
    match zstd_safe::get_frame_content_size(frame) {
        Ok(Some(size)) if size == body.byte_size => {}
        Ok(Some(size)) => {
            return Err(malformed(&format!(
                "it records {size} bytes of content, not {}",
                body.byte_size
            )));
        }
        Ok(None) => return Err(malformed("it does not record its content size")),
        Err(_) => return Err(malformed("its header is corrupt")),
    }
    if zstd_safe::get_dict_id_from_frame(frame).is_some() {
        return Err(malformed("it needs a dictionary"));
    }
    if frame_window_size(frame, body.byte_size)
        .is_none_or(|window| window > 1 << MAX_FRAME_WINDOW_LOG)
    {
        return Err(malformed("its window is larger than 16 MiB"));
    }
    let limit = usize::try_from(body.byte_size).map_err(|_| malformed("it is too large"))?;

    let decoder_error = |code: usize| malformed(zstd_safe::get_error_name(code));
    let mut decoder = DCtx::try_create().ok_or_else(|| malformed("no decoder is available"))?;
    decoder
        .set_parameter(DParameter::WindowLogMax(MAX_FRAME_WINDOW_LOG))
        .map_err(decoder_error)?;
    let mut output = Vec::with_capacity(limit.min(INITIAL_BODY_CAPACITY));
    let mut chunk = vec![0u8; DCtx::out_size()];
    let mut input = InBuffer::around(frame);
    loop {
        // Room for one byte past the limit, so an overrun shows without decoding further.
        let room = (limit - output.len()).saturating_add(1).min(chunk.len());
        let mut out = OutBuffer::around(&mut chunk[..room]);
        let remaining = decoder
            .decompress_stream(&mut out, &mut input)
            .map_err(decoder_error)?;
        let produced = out.pos();
        if produced > limit - output.len() {
            return Err(CloudError::SerializationError(format!(
                "package body {} decodes to more than its {} bytes",
                body.content_hash, body.byte_size
            )));
        }
        output.extend_from_slice(&chunk[..produced]);
        if remaining == 0 {
            break;
        }
        if produced == 0 && input.pos() == frame.len() {
            return Err(malformed("it is truncated"));
        }
    }
    if input.pos() != frame.len() {
        return Err(malformed("bytes follow the frame"));
    }
    if output.len() != limit {
        return Err(malformed(&format!(
            "it decodes to {} bytes, not {}",
            output.len(),
            body.byte_size
        )));
    }
    let actual = sha256_hex(&output);
    if !actual.eq_ignore_ascii_case(&body.content_hash) {
        return Err(CloudError::SerializationError(format!(
            "package module integrity mismatch: expected {}, got {actual}",
            body.content_hash
        )));
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolved_package_parses() {
        let json = serde_json::json!({
            "package_id": "00000000-0000-0000-0000-000000000001",
            "owner_nickname": "wbk",
            "name": "mapper",
            "version": "1.4.0",
            "manifest": { "name": "mapper", "version": "1.4.0" },
            "is_public": true,
            "aligned_hosts": ["mud.arctic.org"],
            "modules": [
                {
                    "subpath": "index.ts",
                    "content_hash": "abc123",
                    "media_type": "application/typescript",
                    "byte_size": 12,
                    "is_entry": true
                }
            ],
            "bodies": [
                { "content_hash": "abc123", "byte_size": 12, "compressed_size": 21 }
            ],
            "bundle_url": "https://example.com/bundles/v1?expires=1&sig=abc"
        });
        let resolved: ResolvedPackageWire = serde_json::from_value(json).unwrap();
        assert_eq!(resolved.version, "1.4.0");
        assert_eq!(resolved.modules.len(), 1);
        assert!(resolved.modules[0].is_entry);
        assert_eq!(
            resolved.bodies,
            vec![BundleBody {
                content_hash: "abc123".to_string(),
                byte_size: 12,
                compressed_size: 21,
            }]
        );
        assert_eq!(
            resolved.bundle_url,
            "https://example.com/bundles/v1?expires=1&sig=abc"
        );
        assert_eq!(resolved.aligned_hosts, vec!["mud.arctic.org"]);
        // A response without `dependencies` (an older server) parses to an empty set.
        assert!(resolved.dependencies.is_empty());
        assert!(resolved.available_with_smudgy_upgrade.is_none());
    }

    #[test]
    fn resolved_package_parses_locked_dependencies() {
        let json = serde_json::json!({
            "package_id": "00000000-0000-0000-0000-000000000001",
            "owner_nickname": "wbk",
            "name": "app",
            "version": "1.0.0",
            "available_with_smudgy_upgrade": {
                "package_version": "1.1.0",
                "minimum_smudgy_version": "0.5.9"
            },
            "manifest": { "name": "app", "version": "1.0.0" },
            "modules": [],
            "dependencies": [
                { "owner_nickname": "wbk", "name": "util", "range": "^1.2", "resolved_version": "1.4.0" },
                { "owner_nickname": "wbk", "name": "events", "range": "^2", "resolved_version": "2.1.0", "kind": "requires" }
            ]
        });
        let resolved: ResolvedPackageWire = serde_json::from_value(json).unwrap();
        assert_eq!(resolved.dependencies.len(), 2);
        assert_eq!(resolved.dependencies[0].name, "util");
        assert_eq!(resolved.dependencies[0].range, "^1.2");
        assert_eq!(resolved.dependencies[0].resolved_version, "1.4.0");
        assert_eq!(
            resolved.dependencies[0].kind,
            DependencyKind::Dependency,
            "an old server's omitted kind is a code dependency"
        );
        assert_eq!(resolved.dependencies[1].kind, DependencyKind::Requires);
        assert_eq!(
            resolved
                .available_with_smudgy_upgrade
                .map(|upgrade| upgrade.package_version),
            Some("1.1.0".to_string())
        );
    }

    #[test]
    fn resolved_dependency_rejects_unknown_relation_kinds() {
        let unknown = serde_json::json!({
            "owner_nickname": "wbk",
            "name": "future",
            "range": "*",
            "resolved_version": "1.0.0",
            "kind": "suggests"
        });
        assert!(
            serde_json::from_value::<ResolvedDependency>(unknown).is_err(),
            "a future relation must not silently become executable code"
        );
    }

    #[test]
    fn check_updates_result_parses_the_mirrored_contract() {
        // The full ok-shaped result: installed status, latest with modules +
        // kind-carrying dependencies (owner named `owner` on this wire, not
        // `owner_nickname`), and a closure node without modules.
        let json = serde_json::json!({
            "owner": "wbk", "name": "duo", "status": "ok",
            "installed": { "yanked": false, "deleted": false },
            "latest": {
                "version": "1.3.0",
                "published_at": "2026-06-20T00:00:00Z",
                "manifest": { "name": "duo", "version": "1.3.0" },
                "modules": [
                    { "subpath": "index.ts", "content_hash": "abc", "media_type":
                      "application/typescript", "byte_size": 12, "is_entry": true }
                ],
                "dependencies": [
                    { "owner": "wbk", "name": "duo-core", "range": "^1",
                      "resolved_version": "1.1.0", "kind": "dependency" },
                    { "owner": "wbk", "name": "duo-data", "range": "^2",
                      "resolved_version": "2.0.0", "kind": "requires" },
                    { "owner": "wbk", "name": "duo-extra", "range": "^1",
                      "resolved_version": "1.0.0" }
                ]
            },
            "available_with_smudgy_upgrade": {
                "package_version": "1.4.0",
                "minimum_smudgy_version": "0.5.9"
            },
            "closure": [
                { "owner": "wbk", "name": "duo-core", "version": "1.1.0",
                  "manifest": { "name": "duo-core", "version": "1.1.0" },
                  "dependencies": [] }
            ]
        });
        let result: CheckUpdatesResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.status, "ok");
        assert_eq!(
            result.installed,
            Some(UpdateCheckInstalled {
                yanked: false,
                deleted: false
            })
        );
        let latest = result.latest.expect("latest");
        assert_eq!(latest.version, "1.3.0");
        assert!(latest.modules[0].is_entry);
        assert_eq!(latest.dependencies[0].owner.as_deref(), Some("wbk"));
        assert_eq!(latest.dependencies[0].kind, "dependency");
        assert_eq!(latest.dependencies[1].kind, "requires");
        assert_eq!(
            latest.dependencies[2].kind, "dependency",
            "an omitted kind defaults to the dependency relation, never an empty \
             string a kind filter would drop"
        );
        assert_eq!(result.closure.len(), 1);
        assert_eq!(result.closure[0].version, "1.1.0");
        let advisory = result
            .available_with_smudgy_upgrade
            .expect("upgrade advisory");
        assert_eq!(advisory.package_version, "1.4.0");
        assert_eq!(advisory.minimum_smudgy_version, "0.5.9");

        // The uniform miss: nulls for installed/latest, an empty closure.
        let miss: CheckUpdatesResult = serde_json::from_value(serde_json::json!({
            "owner": "wbk", "name": "ghost", "status": "not_found",
            "installed": null, "latest": null, "closure": []
        }))
        .unwrap();
        assert_eq!(miss.status, "not_found");
        assert_eq!(miss.installed, None);
        assert!(miss.latest.is_none());
        assert!(miss.available_with_smudgy_upgrade.is_none());
        assert!(miss.closure.is_empty());
    }

    #[test]
    fn check_updates_entry_omits_an_absent_installed_version() {
        // `installed` is optional on the request wire — an entry without one
        // serializes without the key at all (mirroring the server's contract).
        let bare = serde_json::to_value(CheckUpdatesEntry {
            owner: Some("wbk".into()),
            name: "duo".into(),
            installed: None,
        })
        .unwrap();
        assert_eq!(bare, serde_json::json!({ "owner": "wbk", "name": "duo" }));
    }

    #[test]
    fn ownerless_addresses_omit_the_owner_on_the_wire() {
        // `smudgy:@name` sends the name alone: no owner key at all.
        let entry = serde_json::to_value(CheckUpdatesEntry {
            owner: None,
            name: "duo".into(),
            installed: Some("1.0.0".into()),
        })
        .unwrap();
        assert_eq!(
            entry,
            serde_json::json!({ "name": "duo", "installed": "1.0.0" })
        );
        let have = serde_json::to_value(CheckUpdatesHave {
            owner: None,
            name: "duo-core".into(),
            version: "1.1.0".into(),
        })
        .unwrap();
        assert_eq!(
            have,
            serde_json::json!({ "name": "duo-core", "version": "1.1.0" })
        );
        let edge = serde_json::to_value(PublishDependency {
            owner_nickname: None,
            name: "lib".into(),
            range: "^1".into(),
            resolved_version: "1.4.0".into(),
        })
        .unwrap();
        assert_eq!(
            edge,
            serde_json::json!({ "name": "lib", "range": "^1", "resolved_version": "1.4.0" })
        );
    }

    #[test]
    fn a_clan_package_parses_without_an_owner_nickname() {
        // A clan's package: `owner_kind: "clan"`, the clan's ID as `owner_id`, and no
        // nickname — `null` on resolve, omitted on edges and closure nodes, `""` in search.
        let view: PackageView = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000002",
            "owner_id": "00000000-0000-0000-0000-00000000c1a0",
            "owner_kind": "clan",
            "name": "guild-tools",
            "created_at": "2026-06-20T00:00:00Z",
            "updated_at": "2026-06-20T00:00:00Z"
        }))
        .unwrap();
        assert!(view.is_clan_owned());
        assert_eq!(view.owner_nickname, None);
        let user: PackageView = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000002",
            "owner_id": "00000000-0000-0000-0000-000000000003",
            "name": "speedwalk",
            "created_at": "2026-06-20T00:00:00Z",
            "updated_at": "2026-06-20T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(user.owner_kind, PackageOwnerKind::User);
        assert!(
            serde_json::to_value(&user)
                .unwrap()
                .get("owner_kind")
                .is_none(),
            "a user's package omits owner_kind"
        );

        let resolved: ResolvedPackageWire = serde_json::from_value(serde_json::json!({
            "package_id": "00000000-0000-0000-0000-000000000001",
            "owner_nickname": null,
            "name": "guild-tools",
            "version": "1.0.0",
            "modules": [],
            "dependencies": [
                { "name": "guild-lib", "range": "^1", "resolved_version": "1.2.0" }
            ]
        }))
        .unwrap();
        assert_eq!(resolved.owner_nickname, None);
        assert_eq!(resolved.dependencies[0].owner_nickname, None);

        let search: PackageSearchResult = serde_json::from_value(serde_json::json!({
            "package_id": "00000000-0000-0000-0000-000000000002",
            "owner_nickname": "",
            "name": "guild-tools"
        }))
        .unwrap();
        assert_eq!(search.owner_nickname, None, "an empty nickname is none");

        let result: CheckUpdatesResult = serde_json::from_value(serde_json::json!({
            "name": "guild-tools", "status": "ok", "installed": null,
            "latest": {
                "version": "1.0.0", "published_at": "2026-06-20T00:00:00Z",
                "dependencies": [ { "name": "guild-lib", "range": "^1",
                                    "resolved_version": "1.2.0", "kind": "dependency" } ]
            },
            "closure": [ { "name": "guild-lib", "version": "1.2.0", "dependencies": [] } ]
        }))
        .unwrap();
        assert_eq!(result.owner, None);
        assert_eq!(result.latest.unwrap().dependencies[0].owner, None);
        assert_eq!(result.closure[0].owner, None);
    }

    #[test]
    fn search_result_parses_with_absent_rating() {
        let json = serde_json::json!({
            "package_id": "00000000-0000-0000-0000-000000000002",
            "owner_nickname": "wbk",
            "name": "speedwalk",
            "host_agnostic": true
        });
        let result: PackageSearchResult = serde_json::from_value(json).unwrap();
        assert!(result.host_agnostic);
        assert_eq!(result.avg_rating, None);
        assert!(result.aligned_hosts.is_empty());
        assert!(result.available_with_smudgy_upgrade.is_none());
    }

    #[test]
    fn discovery_shapes_parse_package_upgrade_advisories() {
        let advisory = serde_json::json!({
            "package_version": "2.0.0",
            "minimum_smudgy_version": "0.6.0"
        });
        let search: PackageSearchResult = serde_json::from_value(serde_json::json!({
            "package_id": "00000000-0000-0000-0000-000000000002",
            "owner_nickname": "wbk",
            "name": "speedwalk",
            "available_with_smudgy_upgrade": advisory
        }))
        .unwrap();
        assert_eq!(
            search
                .available_with_smudgy_upgrade
                .as_ref()
                .map(|upgrade| upgrade.package_version.as_str()),
            Some("2.0.0")
        );

        let detail: PackageDetail = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000002",
            "owner_id": "00000000-0000-0000-0000-000000000003",
            "name": "speedwalk",
            "created_at": "2026-06-20T00:00:00Z",
            "updated_at": "2026-06-20T00:00:00Z",
            "available_with_smudgy_upgrade": {
                "package_version": "2.0.0",
                "minimum_smudgy_version": "0.6.0"
            }
        }))
        .unwrap();
        assert_eq!(
            detail
                .available_with_smudgy_upgrade
                .map(|upgrade| upgrade.minimum_smudgy_version),
            Some("0.6.0".to_string())
        );
    }

    #[test]
    fn package_grant_view_parses_both_shapes() {
        // A specific-friend grant carries a grantee.
        let specific: PackageGrantView = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000003",
            "all_friends": false,
            "grantee_id": "00000000-0000-0000-0000-000000000009",
            "grantee_nickname": "pal#1",
            "created_at": "2026-06-20T00:00:00Z"
        }))
        .unwrap();
        assert!(!specific.all_friends);
        assert_eq!(specific.grantee_nickname.as_deref(), Some("pal#1"));

        // An all_friends grant omits the grantee fields.
        let all: PackageGrantView = serde_json::from_value(serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000004",
            "all_friends": true,
            "created_at": "2026-06-20T00:00:00Z"
        }))
        .unwrap();
        assert!(all.all_friends);
        assert_eq!(all.grantee_id, None);
    }

    fn version_item(version: &str, yanked: bool) -> VersionListItem {
        VersionListItem {
            version: version.to_string(),
            yanked,
            deleted: false,
            published_at: "2026-06-20T00:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn picks_highest_satisfying_version() {
        let versions = [
            version_item("1.2.0", false),
            version_item("1.4.0", false),
            version_item("1.3.0", false),
            version_item("2.0.1", false),
        ];
        // `^1.2` collapses to the highest published `1.x`.
        assert_eq!(
            highest_satisfying_version(&versions, Some("^1.2"))
                .unwrap()
                .as_deref(),
            Some("1.4.0")
        );
        // An incompatible-major range picks within its own major.
        assert_eq!(
            highest_satisfying_version(&versions, Some("^2"))
                .unwrap()
                .as_deref(),
            Some("2.0.1")
        );
        // No range means "any" -> the highest overall.
        assert_eq!(
            highest_satisfying_version(&versions, None)
                .unwrap()
                .as_deref(),
            Some("2.0.1")
        );
    }

    #[test]
    fn excludes_yanked_and_reports_no_match() {
        let versions = [
            version_item("1.2.0", false),
            version_item("1.4.0", true), // yanked -> not a candidate
        ];
        // The yanked 1.4.0 is skipped, so `^1.2` collapses to 1.2.0.
        assert_eq!(
            highest_satisfying_version(&versions, Some("^1.2"))
                .unwrap()
                .as_deref(),
            Some("1.2.0")
        );
        // Nothing satisfies a 3.x range.
        assert_eq!(
            highest_satisfying_version(&versions, Some("^3")).unwrap(),
            None
        );
        // A malformed range is an error, not a silent no-match.
        assert!(highest_satisfying_version(&versions, Some("not a range")).is_err());
    }

    #[test]
    fn excludes_deleted_versions_from_dep_lock() {
        // A hard-deleted number is permanently reserved but its content is gone, so it must
        // never be auto-locked as a dependency version — fall back to the highest live one.
        let versions = [
            version_item("1.2.0", false),
            VersionListItem {
                version: "1.4.0".to_string(),
                yanked: false,
                deleted: true, // highest match, but content purged
                published_at: "2026-06-20T00:00:00Z".parse().unwrap(),
            },
        ];
        assert_eq!(
            highest_satisfying_version(&versions, Some("^1.2"))
                .unwrap()
                .as_deref(),
            Some("1.2.0")
        );
    }

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("abc")
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn want_bitmap_is_lowercase_hex_least_significant_bit_first() {
        assert_eq!(
            want_bitmap(&[true, true, true]),
            None,
            "all bodies is no want"
        );
        assert_eq!(want_bitmap(&[true, false]).as_deref(), Some("01"));
        assert_eq!(want_bitmap(&[false, true, false]).as_deref(), Some("02"));
        let mut ten = [false; 10];
        ten[0] = true;
        ten[9] = true;
        assert_eq!(
            want_bitmap(&ten).as_deref(),
            Some("0102"),
            "byte 0 holds bits 0-7"
        );
        let mut twelve = [false; 12];
        twelve[3] = true;
        twelve[7] = true;
        assert_eq!(
            want_bitmap(&twelve).as_deref(),
            Some("88"),
            "trailing zero bytes are omitted"
        );
        let mut seventeen = [false; 17];
        seventeen[12] = true;
        assert_eq!(want_bitmap(&seventeen).as_deref(), Some("0010"));
    }

    #[test]
    fn want_joins_the_signed_query() {
        assert_eq!(
            bundle_request_url("https://r.example/b/v?expires=9&sig=ab", "02"),
            "https://r.example/b/v?expires=9&sig=ab&want=02"
        );
        assert_eq!(
            bundle_request_url("https://r.example/b/v", "02"),
            "https://r.example/b/v?want=02"
        );
    }

    #[test]
    fn compress_bound_is_libzstds() {
        for size in [
            0usize,
            1,
            255,
            256,
            2047,
            2048,
            131_071,
            131_072,
            131_073,
            10 << 20,
        ] {
            assert_eq!(
                compress_bound(size as u64),
                zstd_safe::compress_bound(size) as u64,
                "{size}"
            );
        }
        assert_eq!(compress_bound(0), 64);
        assert_eq!(compress_bound(10 << 20), (10 << 20) + (10 << 12));
    }

    fn body_for(content: &[u8], frame: &[u8]) -> BundleBody {
        BundleBody {
            content_hash: sha256_hex(content),
            byte_size: content.len() as u64,
            compressed_size: frame.len() as u64,
        }
    }

    /// One frame of `content` from a level-1 compressor the test configures.
    fn frame_with(content: &[u8], configure: impl FnOnce(&mut zstd::bulk::Compressor)) -> Vec<u8> {
        let mut compressor = zstd::bulk::Compressor::new(1).unwrap();
        configure(&mut compressor);
        compressor.compress(content).unwrap()
    }

    /// Where a frame's `Frame_Content_Size` field sits and how wide it is.
    fn content_size_field(frame: &[u8]) -> (usize, usize) {
        let descriptor = frame[4];
        let single_segment = descriptor & 0x20 != 0;
        let dictionary_id_len = [0, 1, 2, 4][usize::from(descriptor & 0x03)];
        let field_len = match descriptor >> 6 {
            0 => usize::from(single_segment),
            1 => 2,
            2 => 4,
            _ => 8,
        };
        (
            5 + usize::from(!single_segment) + dictionary_id_len,
            field_len,
        )
    }

    #[test]
    fn published_frames_record_size_and_checksum_and_round_trip() {
        let sources = vec![
            b"export const x = 1;".to_vec(),
            Vec::new(),
            vec![0, 159, 146, 150, 255],
            b"lorem ipsum ".repeat(40_000),
        ];
        let compressed = compress_bodies(&sources).unwrap();
        assert_eq!(compressed.frame_sizes.len(), sources.len());
        assert_eq!(
            compressed.frame_sizes.iter().sum::<u64>(),
            compressed.bundle.len() as u64,
            "the bundle is the frames and nothing else"
        );
        let mut rest = compressed.bundle.as_slice();
        for (source, &size) in sources.iter().zip(&compressed.frame_sizes) {
            let (frame, tail) = rest.split_at(usize::try_from(size).unwrap());
            rest = tail;
            assert!(frame.starts_with(&ZSTD_FRAME_MAGIC));
            assert_eq!(
                zstd_safe::get_frame_content_size(frame).unwrap(),
                Some(source.len() as u64),
                "the pledged source size is recorded, even for an empty body"
            );
            assert_ne!(frame[4] & 0x04, 0, "the content checksum flag is set");
            assert_eq!(zstd_safe::get_dict_id_from_frame(frame), None);
            assert_eq!(
                &decode_body_frame(frame, &body_for(source, frame)).unwrap(),
                source
            );
        }
    }

    fn decode_error(frame: &[u8], body: &BundleBody) -> String {
        match decode_body_frame(frame, body) {
            Err(CloudError::SerializationError(message)) => message,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_that_decodes_past_its_byte_size_is_refused() {
        // Frames whose headers claim half the content their blocks hold. A 1 KiB window
        // makes libzstd decode through a ring buffer far smaller than the claim, so nothing
        // inside the decoder stops at the claimed size.
        let bomb = |claimed: u32| {
            let content = vec![0u8; claimed as usize * 2];
            let mut frame = frame_with(&content, |c| {
                c.set_parameter(zstd_safe::CParameter::WindowLog(10))
                    .unwrap();
            });
            let (offset, len) = content_size_field(&frame);
            assert_eq!(len, 4);
            frame[offset..offset + 4].copy_from_slice(&claimed.to_le_bytes());
            let body = BundleBody {
                content_hash: sha256_hex(&content[..claimed as usize]),
                byte_size: u64::from(claimed),
                compressed_size: frame.len() as u64,
            };
            (frame, body)
        };

        // Larger than one output chunk: the frame streams, and the output limit stops it
        // one byte past the claimed size.
        let (frame, body) = bomb(300_000);
        let message = decode_error(&frame, &body);
        assert!(
            message.contains("decodes to more than its 300000 bytes"),
            "{message}"
        );

        // Within one output chunk: libzstd decodes it in one pass into a buffer one byte
        // larger than the claim, and refuses to overrun that buffer.
        let (frame, body) = bomb(100_000);
        let message = decode_error(&frame, &body);
        assert!(message.contains(&body.content_hash), "{message}");
        assert!(
            message.contains("Destination buffer is too small"),
            "{message}"
        );
    }

    #[test]
    fn malformed_frames_are_refused() {
        let content = b"export const answer = 42;\n".repeat(400);
        let frame = frame_with(&content, |_| {});
        let body = body_for(&content, &frame);

        let mut truncated = frame.clone();
        truncated.truncate(frame.len() - 3);
        assert!(decode_error(&truncated, &body).contains("not a valid frame"));

        let mut trailing = frame.clone();
        trailing.push(0);
        assert!(decode_error(&trailing, &body).contains("bytes follow the frame"));

        let mut two_frames = frame.clone();
        two_frames.extend_from_slice(&frame);
        assert!(decode_error(&two_frames, &body).contains("bytes follow the frame"));

        let mut skippable = vec![0x50, 0x2A, 0x4D, 0x18, 0, 0, 0, 0];
        skippable.extend_from_slice(&frame);
        assert!(decode_error(&skippable, &body).contains("does not start a standard zstd frame"));

        assert!(decode_error(b"not zstd at all", &body).contains("does not start"));

        let without_size = frame_with(&content, |c| {
            c.set_parameter(zstd_safe::CParameter::ContentSizeFlag(false))
                .unwrap();
        });
        assert!(decode_error(&without_size, &body).contains("does not record its content size"));

        let shorter = frame_with(&content[1..], |_| {});
        assert!(decode_error(&shorter, &body).contains(&format!(
            "records {} bytes of content, not {}",
            content.len() - 1,
            content.len()
        )));

        // The same frame, re-headered to name dictionary 7.
        let mut with_dictionary = frame.clone();
        assert_ne!(with_dictionary[4] & 0x20, 0, "a small body is one segment");
        with_dictionary[4] |= 0x01;
        with_dictionary.insert(5, 7);
        assert!(decode_error(&with_dictionary, &body).contains("needs a dictionary"));

        // A multi-segment frame re-headered to declare a 32 MiB window.
        let mut wide = frame_with(&content, |c| {
            c.set_parameter(zstd_safe::CParameter::WindowLog(10))
                .unwrap();
        });
        assert_eq!(
            wide[4] & 0x20,
            0,
            "a 1 KiB window splits this body into segments"
        );
        wide[5] = 15 << 3;
        assert!(decode_error(&wide, &body).contains("window is larger than 16 MiB"));
    }

    #[test]
    fn a_frame_of_other_content_is_an_integrity_mismatch() {
        let content = b"export const x = 1;";
        let impostor = b"export const y = 2;";
        let frame = frame_with(impostor, |_| {});
        let message = decode_error(&frame, &body_for(content, &frame));
        assert!(message.contains("integrity mismatch"), "{message}");
        assert!(message.contains(&sha256_hex(content)), "{message}");
    }
}
