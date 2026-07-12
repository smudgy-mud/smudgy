//! Client for the cloud package-sharing + discovery API.
//!
//! [`PackageApiClient`] is the sibling of [`CloudApiClient`](crate::CloudApiClient):
//! the mapper covers area content, `CloudApiClient` covers identity/social/sharing,
//! and this covers shared **packages** — publish, resolve (`smudgy://owner/name`
//! → manifest + module sources), discovery search, ratings, comments, host alignment,
//! and package grants. All three share one [`CredentialSource`] and the
//! `{success, data, error}` envelope.
//!
//! The `smudgy://` URI is a *client-side* construct: this client decomposes it into
//! `owner` (the globally-unique nickname), `name`, and `subpath`, and never
//! sends the URI on the wire. Module bodies are fetched from a content-addressed URL
//! the resolve response carries (a presigned object URL in production), and integrity
//! is verified against the per-module content hash.
//!
//! The contract here is **mirrored, not shared** with the server
//! (`smudgy-web/smudgy-api`) and the in-memory test mock (`map/tests/support/`): a
//! change to a wire shape must move all three together.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use log::debug;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{backends::CredentialSource, CloudError, CloudResult};

// ===========================================================================
// Wire types (mirror smudgy-api `src/models.rs` package DTOs)
// ===========================================================================

/// A package namespace owned by a user (`POST /packages`, `GET /packages/{id}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackageView {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub is_public: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The owner's nickname (omitted to the owner themselves).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_nickname: Option<String>,
}

/// Full detail for one package (`GET /packages/{id}`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PackageDetail {
    #[serde(flatten)]
    pub package: PackageView,
    #[serde(default)]
    pub latest_version: Option<String>,
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
    pub owner_nickname: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub latest_version: Option<String>,
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
/// install/auto-load path: the manifest plus every module's content-addressed URL.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPackageWire {
    pub package_id: Uuid,
    pub owner_nickname: String,
    pub name: String,
    pub version: String,
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
    /// The resolved version's locked `smudgy://` dependencies (referrer-aware
    /// resolution). Servers predating the field omit it → empty (referrer-blind fallback).
    #[serde(default)]
    pub dependencies: Vec<ResolvedDependency>,
}

/// One locked `smudgy://` dependency carried on a [`ResolvedPackageWire`]: the dep's
/// owner nickname, name, declared `range`, and the concrete version this package
/// locked. The runtime selects each importer's dependency version from its own map
/// (referrer-aware resolution); the `range` lets it honor an author exact-version pin
/// (`@=x`, exempt from upgrade-collapse) and show declared intent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDependency {
    pub owner_nickname: String,
    pub name: String,
    #[serde(default)]
    pub range: String,
    pub resolved_version: String,
}

/// One module within a [`ResolvedPackageWire`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedModuleWire {
    /// File path within the package, e.g. `index.ts`, `lib/util.ts`.
    pub subpath: String,
    /// Lowercase-hex SHA-256 of the body; verified after fetch.
    pub content_hash: String,
    #[serde(default = "default_media_type")]
    pub media_type: String,
    #[serde(default)]
    pub byte_size: i64,
    #[serde(default)]
    pub is_entry: bool,
    /// Where to fetch the body (a presigned object URL in production).
    pub content_url: String,
}

/// One module to publish. The body is uploaded directly to S3 via a presigned PUT
/// ([`PackageApiClient::publish_version`] runs the begin → upload → finalize flow); the
/// server only ever sees the hash + size. `content` is raw bytes, so binaries publish too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishModule {
    pub subpath: String,
    pub content: Vec<u8>,
    pub media_type: String,
    pub is_entry: bool,
}

/// One module's metadata in a publish request (`begin` + `finalize`) — mirrors the server's
/// `PublishModuleMeta`. The body rides to S3 separately; only this metadata crosses our wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct PublishModuleMeta {
    subpath: String,
    content_hash: String,
    byte_size: i64,
    media_type: String,
    is_entry: bool,
}

/// `…/versions/begin` response — a presigned PUT per module body not already in S3.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct BeginVersionResponse {
    #[serde(default)]
    uploads: Vec<PresignedUpload>,
}

/// One presigned upload from `begin`: PUT the body to `url` with exactly `headers` (which
/// include `x-amz-checksum-sha256`, binding the body to its declared hash).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct PresignedUpload {
    content_hash: String,
    url: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
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

/// One `smudgy://` dependency declared at publish — the client locks the declared range
/// to a concrete version and sends both (`POST /packages/{id}/versions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishDependency {
    pub owner_nickname: String,
    pub name: String,
    pub range: String,
    pub resolved_version: String,
}

/// One entry in a package's share closure (`GET /packages/{id}/share-closure`): a
/// transitive `smudgy://` dependency and whether the prospective grantee can reach it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareClosureItem {
    pub package_id: Uuid,
    pub owner_nickname: String,
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

    /// Resolves `owner/name` at `version` (`None`/`"latest"` = newest) to a
    /// concrete version, manifest, and module list (`GET /packages/resolve`).
    /// `owner_nickname` is the owner's globally-unique nickname.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a missing/unauthorized package (404), or
    /// transport/parse failure.
    pub async fn resolve_package(
        &self,
        owner_nickname: &str,
        name: &str,
        version: Option<&str>,
    ) -> CloudResult<ResolvedPackageWire> {
        let query = [
            ("owner", owner_nickname.to_string()),
            ("name", name.to_string()),
            ("version", version.unwrap_or("latest").to_string()),
        ];
        self.get_with_query_public("/packages/resolve", &query).await
    }

    /// Lists a package's published versions, newest first (`GET /packages/{id}/versions`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a missing/unauthorized package, or
    /// transport/parse failure.
    pub async fn list_versions(&self, package_id: Uuid) -> CloudResult<Vec<VersionListItem>> {
        self.get_public(&format!("/packages/{package_id}/versions")).await
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

    /// Creates (or returns) the caller's package namespace `name` (`POST /packages`).
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, a verification gate (403), a name
    /// conflict (409), or transport/parse failure.
    pub async fn create_package(&self, name: &str, description: &str) -> CloudResult<PackageView> {
        let body = json!({ "name": name, "description": description });
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

    /// Publishes an immutable version via the presigned begin → upload → finalize flow:
    /// `begin` validates + returns a presigned PUT per body not already in S3, the client PUTs
    /// each missing body directly to S3, then `finalize` commits. Module bodies are arbitrary
    /// bytes (binaries publish too); the server only ever sees the hash + size.
    ///
    /// # Errors
    /// Returns a [`CloudError`] on auth failure, non-ownership, a cap (413), a duplicate
    /// version (409), an upload/integrity failure, or transport/parse failure.
    pub async fn publish_version(
        &self,
        package_id: Uuid,
        version: &str,
        manifest: &Value,
        modules: &[PublishModule],
        dependencies: &[PublishDependency],
        readme: Option<&str>,
    ) -> CloudResult<PublishedVersionView> {
        // Hash each body once; build the metadata wire list + a content_hash -> bytes lookup.
        let mut by_hash: HashMap<String, &[u8]> = HashMap::new();
        let metas: Vec<PublishModuleMeta> = modules
            .iter()
            .map(|m| {
                let content_hash = sha256_hex(&m.content);
                by_hash.insert(content_hash.clone(), m.content.as_slice());
                PublishModuleMeta {
                    subpath: m.subpath.clone(),
                    content_hash,
                    byte_size: i64::try_from(m.content.len()).unwrap_or(i64::MAX),
                    media_type: m.media_type.clone(),
                    is_entry: m.is_entry,
                }
            })
            .collect();
        let body = json!({
            "version": version,
            "manifest": manifest,
            "modules": metas,
            "dependencies": dependencies,
            "readme": readme,
        });

        // 1. begin — validate + get a presigned PUT per body not already in S3.
        let begin: BeginVersionResponse = self
            .post(&format!("/packages/{package_id}/versions/begin"), Some(&body))
            .await?;

        // 2. upload each missing body directly to S3 (presigned, no auth header).
        for upload in &begin.uploads {
            let bytes = by_hash.get(upload.content_hash.as_str()).ok_or_else(|| {
                CloudError::SerializationError(format!(
                    "server requested upload of an unknown blob {}",
                    upload.content_hash
                ))
            })?;
            self.upload_blob(&upload.url, &upload.headers, bytes.to_vec())
                .await?;
        }

        // 3. finalize — confirm the uploads + commit.
        self.post(&format!("/packages/{package_id}/versions/finalize"), Some(&body))
            .await
    }

    /// PUT a module body directly to its presigned URL. No `Authorization` (the URL is
    /// presigned); `headers` come verbatim from `begin` (they include the checksum binding).
    async fn upload_blob(
        &self,
        url: &str,
        headers: &BTreeMap<String, String>,
        bytes: Vec<u8>,
    ) -> CloudResult<()> {
        debug!("PUT <module body>");
        let mut request = self.client.put(url).body(bytes);
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let response = request.send().await?;
        if !response.status().is_success() {
            return Err(CloudError::from_status(
                response.status().as_u16(),
                "failed to upload package module body",
            ));
        }
        Ok(())
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
        self.get_public(&format!("/packages/{package_id}/comments")).await
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

    /// Fetches a module body as raw bytes from its content-addressed URL and verifies its
    /// SHA-256 against `expected_hash` (lowercase hex). The URL is presigned in production, so
    /// no `Authorization` header is sent. Use this for binary modules; text callers can use
    /// [`Self::fetch_module_body`].
    ///
    /// # Errors
    /// Returns [`CloudError::SerializationError`] on an integrity mismatch, or a transport error.
    pub async fn fetch_module_bytes(
        &self,
        content_url: &str,
        expected_hash: &str,
    ) -> CloudResult<Vec<u8>> {
        debug!("GET <module body>");
        let response = self.client.get(content_url).send().await?;
        if !response.status().is_success() {
            return Err(CloudError::from_status(
                response.status().as_u16(),
                "failed to fetch package module body",
            ));
        }
        let bytes = response.bytes().await?;
        let actual = sha256_hex(&bytes);
        if !actual.eq_ignore_ascii_case(expected_hash.trim_start_matches("sha256-")) {
            return Err(CloudError::SerializationError(format!(
                "package module integrity mismatch: expected {expected_hash}, got {actual}"
            )));
        }
        Ok(bytes.to_vec())
    }

    /// Fetches a module body as UTF-8 text (verifying its hash). Errors on a non-UTF-8 body —
    /// use [`Self::fetch_module_bytes`] for binaries.
    ///
    /// # Errors
    /// Returns [`CloudError::SerializationError`] on an integrity mismatch or non-UTF-8 body,
    /// or a transport error.
    pub async fn fetch_module_body(
        &self,
        content_url: &str,
        expected_hash: &str,
    ) -> CloudResult<String> {
        let bytes = self.fetch_module_bytes(content_url, expected_hash).await?;
        String::from_utf8(bytes).map_err(|err| {
            CloudError::SerializationError(format!("package module body is not valid UTF-8: {err}"))
        })
    }

    // ===== internal plumbing ==============================================

    fn auth_header(&self) -> CloudResult<String> {
        self.credentials
            .get()
            .map(|credential| credential.header_value())
            .ok_or_else(|| CloudError::Unauthorized("no credential configured".to_string()))
    }

    /// Sends a request. Bodies are never logged (they may carry option/secret
    /// values); only the URL (sans query) and the response status, at debug
    /// level. Most package endpoints require a credential; the public read
    /// surface ([`Auth::Optional`]) sends one only when present. Presigned
    /// object URLs are fetched separately in [`Self::fetch_module_body`].
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
        match auth {
            Auth::Required => {
                request = request.header("authorization", self.auth_header()?);
            }
            Auth::Optional => {
                if let Some(credential) = self.credentials.get() {
                    request = request.header("authorization", credential.header_value());
                }
            }
        }
        if !query.is_empty() {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await?;
        debug!("{method} {url} - {}", response.status());

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
        let response = self.send(Method::GET, path, query, None, Auth::Required).await?;
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
        let response = self.send(Method::GET, path, query, None, Auth::Optional).await?;
        Self::parse_data(response).await
    }

    async fn post<T>(&self, path: &str, body: Option<&Value>) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self.send(Method::POST, path, &[], body, Auth::Required).await?;
        Self::parse_data(response).await
    }

    async fn patch<T>(&self, path: &str, body: &Value) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self.send(Method::PATCH, path, &[], Some(body), Auth::Required).await?;
        Self::parse_data(response).await
    }

    async fn delete(&self, path: &str) -> CloudResult<()> {
        let response = self.send(Method::DELETE, path, &[], None, Auth::Required).await?;
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
                    "is_entry": true,
                    "content_url": "https://example.com/obj/abc123"
                }
            ]
        });
        let resolved: ResolvedPackageWire = serde_json::from_value(json).unwrap();
        assert_eq!(resolved.version, "1.4.0");
        assert_eq!(resolved.modules.len(), 1);
        assert!(resolved.modules[0].is_entry);
        assert_eq!(resolved.aligned_hosts, vec!["mud.arctic.org"]);
        // A response without `dependencies` (an older server) parses to an empty set.
        assert!(resolved.dependencies.is_empty());
    }

    #[test]
    fn resolved_package_parses_locked_dependencies() {
        let json = serde_json::json!({
            "package_id": "00000000-0000-0000-0000-000000000001",
            "owner_nickname": "wbk",
            "name": "app",
            "version": "1.0.0",
            "manifest": { "name": "app", "version": "1.0.0" },
            "modules": [],
            "dependencies": [
                { "owner_nickname": "wbk", "name": "util", "range": "^1.2", "resolved_version": "1.4.0" }
            ]
        });
        let resolved: ResolvedPackageWire = serde_json::from_value(json).unwrap();
        assert_eq!(resolved.dependencies.len(), 1);
        assert_eq!(resolved.dependencies[0].name, "util");
        assert_eq!(resolved.dependencies[0].range, "^1.2");
        assert_eq!(resolved.dependencies[0].resolved_version, "1.4.0");
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
            highest_satisfying_version(&versions, Some("^1.2")).unwrap().as_deref(),
            Some("1.4.0")
        );
        // An incompatible-major range picks within its own major.
        assert_eq!(
            highest_satisfying_version(&versions, Some("^2")).unwrap().as_deref(),
            Some("2.0.1")
        );
        // No range means "any" -> the highest overall.
        assert_eq!(
            highest_satisfying_version(&versions, None).unwrap().as_deref(),
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
            highest_satisfying_version(&versions, Some("^1.2")).unwrap().as_deref(),
            Some("1.2.0")
        );
        // Nothing satisfies a 3.x range.
        assert_eq!(highest_satisfying_version(&versions, Some("^3")).unwrap(), None);
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
            highest_satisfying_version(&versions, Some("^1.2")).unwrap().as_deref(),
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
}
