//! Client for the cloud identity, social, and sharing API.
//!
//! [`CloudApiClient`] complements [`CloudMapper`](crate::CloudMapper): the
//! mapper covers area/room content, this client covers accounts, friends,
//! blocks, share grants, secret marks, previews, and copies. Both are meant
//! to share one [`CredentialSource`], so logging in upgrades every consumer
//! at once.
//!
//! Every JSON response is wrapped in the `{success, data, error}` envelope;
//! non-2xx statuses map onto the client error taxonomy via
//! [`CloudError::from_status`].
//!
//! # Security
//!
//! Request bodies, tokens, and one-time codes are never logged — only the
//! URL (without query string) and the response status, at `debug` level.
//! Types carrying credential material ([`CreatedApiKey`], [`AuthSession`])
//! redact it from their `Debug` output.

use std::fmt;
use std::sync::Arc;

use arc_swap::ArcSwap;
use chrono::{DateTime, Utc};
use log::debug;
use reqwest::{Client, Method};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    Area, AreaId, AreaWithDetails, AtlasId, ExitId, LabelId, CloudError, CloudResult, ShapeId,
    SyncRow, backends::CredentialSource,
};

// ===========================================================================
// Wire types
// ===========================================================================

/// The caller's own profile (`GET /me`; the `user` field of the verify-email
/// response). `email` is serialized only on this type, never on social
/// shapes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserProfile {
    pub id: Uuid,
    pub email: String,
    #[serde(default)]
    pub nickname: Option<String>,
    #[serde(default)]
    pub requested_nickname: Option<String>,
    #[serde(default)]
    pub email_verified_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub nickname_updated_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl UserProfile {
    /// Whether the account's email is verified (the server gates social and
    /// sharing endpoints on this).
    #[must_use]
    pub const fn is_verified(&self) -> bool {
        self.email_verified_at.is_some()
    }
}

/// A freshly minted session: bearer token plus the authenticated profile.
///
/// `needs_nickname` is sent only when this verification could not claim the
/// requested nickname because it was already taken, so the user must choose a
/// different one (the field is omitted from the wire when false).
#[derive(Clone, Deserialize)]
pub struct AuthSession {
    pub session_token: String,
    pub user: UserProfile,
    #[serde(default)]
    pub needs_nickname: bool,
}

// The session token is credential material; keep it out of Debug output.
impl fmt::Debug for AuthSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthSession")
            .field("user", &self.user)
            .field("needs_nickname", &self.needs_nickname)
            .finish_non_exhaustive()
    }
}

/// One row of `GET /me/api-keys`. Never carries key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiKeyInfo {
    pub id: Uuid,
    #[serde(default)]
    pub key_suffix: Option<String>,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
}

/// `POST /me/api-keys` response. `api_key` is the full key material, shown
/// exactly once by the server; it is deliberately excluded from `Debug`
/// output (only the listing-safe `key_suffix` appears).
#[derive(Clone, Deserialize)]
pub struct CreatedApiKey {
    pub id: Uuid,
    pub api_key: String,
    pub key_suffix: String,
    pub created_at: DateTime<Utc>,
}

impl fmt::Debug for CreatedApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CreatedApiKey")
            .field("id", &self.id)
            .field("key_suffix", &self.key_suffix)
            .field("created_at", &self.created_at)
            .finish_non_exhaustive()
    }
}

/// One row of `GET /me/sessions` (unexpired sessions only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    pub id: Uuid,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
}

/// `GET /users/lookup` result: an exact-nickname hit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserRef {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
}

/// One accepted friendship (`GET /friends`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendView {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    pub since: DateTime<Utc>,
}

/// One pending friend request (either direction).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendRequestView {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// `GET /friends/requests` response: pending requests, both directions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FriendRequests {
    pub incoming: Vec<FriendRequestView>,
    pub outgoing: Vec<FriendRequestView>,
}

/// One row of `GET /blocks`: a user the caller has blocked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockView {
    pub user_id: Uuid,
    #[serde(default)]
    pub nickname: Option<String>,
    pub created_at: DateTime<Utc>,
}

/// Subject of a share grant: exactly one of an area or an atlas.
///
/// Serializes to exactly `{"area_id": "<uuid>"}` or `{"atlas_id": "<uuid>"}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ShareScope {
    Area { area_id: AreaId },
    Atlas { atlas_id: AtlasId },
}

/// `POST /shares` body. All four capability flags are serialized explicitly
/// (never omitted).
#[allow(clippy::struct_excessive_bools)] // mirrors the wire contract
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CreateShareRequest {
    pub grantee_id: Uuid,
    pub scope: ShareScope,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    pub include_secrets: bool,
    /// Full-deputy. Accepted only from the true owner on an owner-minted root
    /// (server-enforced); implies all lower caps including `can_reshare`.
    pub can_admin: bool,
    /// Grantor-authored advisory host strings (`"host"` or `"host:port"`)
    /// snapshotting the hosts of the atlas's associated server entries at the
    /// consent moment (§4.2 of the map-server-scoping plan). Helps the recipient
    /// home the atlas onto the right local session; affects only recipient-side
    /// default grouping. Omitted when absent (skip-when-none keeps old-server
    /// compatibility; the server has `#[serde(default)]`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host_hints: Option<Vec<String>>,
}

/// A share grant row as served by the API. Exactly one of `area_id` /
/// `atlas_id` is set.
#[allow(clippy::struct_excessive_bools)] // mirrors the wire contract
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareGrant {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub grantor_id: Uuid,
    pub grantee_id: Uuid,
    #[serde(default)]
    pub area_id: Option<AreaId>,
    #[serde(default)]
    pub atlas_id: Option<AtlasId>,
    pub can_edit: bool,
    pub can_reshare: bool,
    pub can_copy: bool,
    pub include_secrets: bool,
    /// Full-deputy flag (owner-minted, root-only). Older servers omit it → false.
    #[serde(default)]
    pub can_admin: bool,
    #[serde(default)]
    pub parent_grant_id: Option<Uuid>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    /// The nickname of the immediate grantor — *who shared it to
    /// you*. Populated on `GET /shares` rows (both directions); omitted when
    /// that user has no handle yet. Lets the by-sharer grouping read the
    /// grantor handle straight off the received row instead of joining
    /// `GET /friends` (grantors are always current friends, but the join had a
    /// brief unresolvable window mid-refresh right after an unfriend).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grantor_nickname: Option<String>,
    /// The nickname of the *original owner*. Populated on
    /// `GET /shares` rows; omitted when unallocated. Differs from
    /// `grantor_nickname` on a re-share (show the owner handle then).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_nickname: Option<String>,
    /// Grantor-authored advisory host hints snapshotted at share creation
    /// (`"host"`/`"host:port"`; §4.2 of the map-server-scoping plan). Flows to
    /// the grantee on `GET /shares` rows (both directions) and grant-tree nodes
    /// so they can home the atlas onto a local session. A snapshot: never
    /// inherited from a parent grant, never PATCH-updated. Older servers omit
    /// it → `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_hints: Option<Vec<String>>,
}

/// One row of `GET /shares`: the grant plus its depth below the root grant
/// (root = 0).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShareGrantRow {
    #[serde(flatten)]
    pub grant: ShareGrant,
    pub depth: i32,
}

/// One node of `GET /areas/{id}/shares`: the grant, its depth from the
/// caller's visible root, and the grantee's handle when allocated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantTreeNode {
    #[serde(flatten)]
    pub grant: ShareGrant,
    pub depth: i32,
    /// The grantee's nickname; omitted from the wire when
    /// the grantee has no handle.
    #[serde(default)]
    pub grantee_nickname: Option<String>,
}

/// `PATCH /shares/{id}` body; `None` fields are omitted (left unchanged).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct SharePatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_edit: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_reshare: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_copy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub include_secrets: Option<bool>,
    /// Set/raise/remove the full-deputy flag (owner-only, owner-minted root).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub can_admin: Option<bool>,
}

/// Direction selector for `GET /transfers?direction=offered|received`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferDirection {
    /// Offers the caller initiated (`from_user_id`).
    Offered,
    /// Offers addressed to the caller (`to_user_id`).
    Received,
}

impl TransferDirection {
    #[must_use]
    pub const fn as_query_value(self) -> &'static str {
        match self {
            Self::Offered => "offered",
            Self::Received => "received",
        }
    }
}

/// A `pending_transfers` row as served by `GET /transfers` and the offer/accept
/// responses. `expires_at` is null (offers are non-expiring).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransferView {
    pub id: Uuid,
    /// `"area"` or `"atlas"`.
    pub subject_kind: String,
    #[serde(default)]
    pub area_id: Option<AreaId>,
    #[serde(default)]
    pub atlas_id: Option<AtlasId>,
    pub from_user_id: Uuid,
    pub to_user_id: Uuid,
    /// `Offered` | `Accepted` | `Declined` | `Cancelled` | `Expired`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub responded_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub expires_at: Option<DateTime<Utc>>,
    /// The subject's display name (area or atlas).
    #[serde(default)]
    pub subject_name: Option<String>,
    /// The nickname of the initiator / recipient when allocated.
    #[serde(default)]
    pub from_nickname: Option<String>,
    #[serde(default)]
    pub to_nickname: Option<String>,
}

/// Identifies one room property in a [`SecretMarksRequest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RoomPropertyRef {
    pub room_number: i32,
    pub name: String,
}

/// `POST /areas/{id}/secret-marks` body: set or clear `is_secret` in bulk.
/// Empty lists are serialized as-is (the server defaults them anyway).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretMarksRequest {
    pub secret: bool,
    pub rooms: Vec<i32>,
    pub exits: Vec<ExitId>,
    pub labels: Vec<LabelId>,
    pub shapes: Vec<ShapeId>,
    pub room_properties: Vec<RoomPropertyRef>,
    pub area_properties: Vec<String>,
}

/// Per-type counts of rows actually changed by a secret-marks call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretMarksResult {
    pub rooms: u64,
    pub exits: u64,
    pub labels: u64,
    pub shapes: u64,
    pub room_properties: u64,
    pub area_properties: u64,
}

/// Entity kind in the owner's secret-audit list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretEntityKind {
    Room,
    Exit,
    Label,
    Shape,
    RoomProperty,
    AreaProperty,
}

/// One row of `GET /areas/{id}/secrets`. Fields irrelevant to the kind are
/// omitted from the wire: `id` for exits/labels/shapes, `room_number` for
/// rooms/room properties, `name` for properties.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretEntity {
    pub kind: SecretEntityKind,
    #[serde(default)]
    pub id: Option<Uuid>,
    #[serde(default)]
    pub room_number: Option<i32>,
    #[serde(default)]
    pub name: Option<String>,
}

/// `POST /areas/{id}/copy` body; `None` fields are omitted (server defaults:
/// name `<source name> (copy)`, no atlas).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct CopyAreaRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub atlas_id: Option<AtlasId>,
}

/// One row of `GET /me/area-prefs`: the viewer's explicit per-area preference,
/// the cross-device sync home for the local `disabled_map_areas` set.
///
/// A **present** row is an *explicit* preference; an **absent** area defaults
/// to enabled. `updated_at` is server-set (RFC3339) and is the basis for the
/// last-write-wins reconcile. `GET` only returns rows for areas the viewer can
/// currently see, so a locally-known pref missing from the response is for an
/// area access was lost to (moot — drop it locally) — see the reconcile in the
/// UI layer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AreaPref {
    pub area_id: AreaId,
    pub disabled: bool,
    pub updated_at: DateTime<Utc>,
}

/// `POST /atlases/{id}/copy` response: the new atlas plus which source
/// members were copied or skipped (viewable-but-not-copyable).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtlasCopyReport {
    pub atlas_id: AtlasId,
    pub name: String,
    pub copied: Vec<AreaId>,
    pub skipped: Vec<AreaId>,
}

/// Which side of share grants to list (`GET /shares?direction=`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShareDirection {
    /// Grants where the caller is the grantor.
    Given,
    /// Grants where the caller is the grantee.
    Received,
}

impl ShareDirection {
    /// The exact query-parameter value the server expects.
    #[must_use]
    pub const fn as_query_value(self) -> &'static str {
        match self {
            Self::Given => "given",
            Self::Received => "received",
        }
    }
}

/// Audience simulated by `GET /areas/{id}/preview`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreviewAudience {
    /// Anonymous worst case: a random viewer with all-false capabilities.
    WorstCase,
    /// Simulate the grantee of the given share, when that grant reaches the
    /// area (a bogus id silently degrades to the worst case server-side).
    Share(Uuid),
    /// Simulate a specific user verbatim.
    AsUser(Uuid),
}

// ===========================================================================
// Client
// ===========================================================================

/// Whether a request attaches the current credential.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Auth {
    /// Public auth endpoint: no `Authorization` header, even when logged in.
    Public,
    /// Credential required; fails fast with [`CloudError::Unauthorized`] when
    /// none is configured.
    Required,
}

/// HTTP client for the cloud identity, social, and sharing endpoints.
///
/// Cheap to clone; clones share the underlying connection pool and the
/// hot-swappable [`CredentialSource`].
#[derive(Debug, Clone)]
pub struct CloudApiClient {
    client: Client,
    base_url: String,
    credentials: CredentialSource,
    /// Newest client version the server last advertised via the
    /// `x-smudgy-upgrade-available` response header (`None` until seen). Updated
    /// on every `send`; clones share it, so the UI can read the soft upgrade hint.
    upgrade_available: Arc<ArcSwap<Option<String>>>,
}

impl CloudApiClient {
    /// Creates a client for the API at `base_url` (trailing slashes are
    /// trimmed) using the shared, hot-swappable credential source.
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

    /// The newest client version the server has advertised via the
    /// `x-smudgy-upgrade-available` header this session, if any. Drives the soft
    /// "upgrade available" prompt; `None` until/unless the server signals it.
    #[must_use]
    pub fn upgrade_available(&self) -> Option<String> {
        self.upgrade_available.load_full().as_ref().clone()
    }

    /// Polls the unauthenticated `GET /health` so the client — even signed out —
    /// can check for a newer version. Two outcomes feed the existing prompts,
    /// reusing the version-gate headers rather than any new comparison logic:
    /// a `426` surfaces as [`CloudError::UpgradeRequired`] (this build is below
    /// the server floor), and an in-range-but-behind build has its newest
    /// version captured into [`Self::upgrade_available`] from the response header
    /// by `send`. This is the only smudgy-web request a logged-out client makes,
    /// and only when the user has left automatic update checks enabled.
    ///
    /// # Errors
    /// [`CloudError::UpgradeRequired`] when this build is below the server's
    /// floor, or a transport error.
    pub async fn check_for_updates(&self) -> CloudResult<()> {
        let response = self
            .send(Method::GET, "/health", &[], None, Auth::Public)
            .await?;
        Self::parse_unit(response).await
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    #[must_use]
    pub fn credentials(&self) -> &CredentialSource {
        &self.credentials
    }

    // ===== internal plumbing ==============================================

    fn auth_header(&self) -> CloudResult<String> {
        self.credentials
            .get()
            .map(|credential| credential.header_value())
            .ok_or_else(|| CloudError::Unauthorized("no credential configured".to_string()))
    }

    /// Sends a request. Bodies are deliberately never logged — they may
    /// carry one-time codes or token material; only the URL (sans query
    /// string) and the response status are, at debug level.
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
        if !query.is_empty() {
            request = request.query(query);
        }
        if auth == Auth::Required {
            request = request.header("authorization", self.auth_header()?);
        }
        if let Some(body) = body {
            request = request.json(body);
        }

        let response = request.send().await?;
        debug!("{method} {url} - {}", response.status());

        // Soft upgrade nudge: the server tags responses for an in-range (allowed
        // but behind) client with the newest version. Stash it for the UI. Only
        // set when present — the version is fixed per process, so never clear.
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

    /// Unwraps the `data` field of the `{success, data, error}` envelope on
    /// a 2xx response; maps error statuses onto the client error taxonomy.
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

    /// Accepts any 2xx — `200` with `data: null`, `202` accepted, or an
    /// empty `204` — as success; maps error statuses onto the taxonomy.
    async fn parse_unit(response: reqwest::Response) -> CloudResult<()> {
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(Self::error_for(status.as_u16(), response).await)
        }
    }

    /// Extracts the envelope `error` string (falling back to the raw body)
    /// and maps the status onto [`CloudError`].
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

    async fn post<T>(&self, path: &str, body: Option<&Value>, auth: Auth) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self.send(Method::POST, path, &[], body, auth).await?;
        Self::parse_data(response).await
    }

    async fn post_unit(&self, path: &str, body: Option<&Value>, auth: Auth) -> CloudResult<()> {
        let response = self.send(Method::POST, path, &[], body, auth).await?;
        Self::parse_unit(response).await
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

    async fn put_unit(&self, path: &str) -> CloudResult<()> {
        let response = self
            .send(Method::PUT, path, &[], None, Auth::Required)
            .await?;
        Self::parse_unit(response).await
    }

    async fn put<T>(&self, path: &str, body: &Value) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let response = self
            .send(Method::PUT, path, &[], Some(body), Auth::Required)
            .await?;
        Self::parse_data(response).await
    }

    // ===== auth (public endpoints — no credential attached) ===============

    /// `POST /auth/signup` — requests account creation and mails a
    /// verification code. The server replies `202` regardless of outcome
    /// (enumeration resistance); an existing email is mailed an "account
    /// exists" notice instead of a code.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn signup(&self, email: &str, nickname: &str) -> CloudResult<()> {
        let body = json!({ "email": email, "nickname": nickname });
        self.post_unit("/auth/signup", Some(&body), Auth::Public)
            .await
    }

    /// `POST /auth/login` — the unified passwordless entry. Mails a sign-in
    /// code, **creating the account on first sight** when the email is unknown
    /// (a nickname-less account — `verify_email` resolves that to
    /// `needs_nickname`), so one email-only call serves both new and returning
    /// users. `202` always, known email or not (enumeration resistance). Each
    /// call supersedes any earlier open code, so this doubles as "resend code" —
    /// there is no dedicated resend endpoint.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn login(&self, email: &str) -> CloudResult<()> {
        let body = json!({ "email": email });
        self.post_unit("/auth/login", Some(&body), Auth::Public)
            .await
    }

    /// `POST /auth/verify-email` with the emailed one-time code. Consumes
    /// the code and returns a fresh session. The first verification also
    /// marks the email verified and allocates the handle; a returning login
    /// just mints the session.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] uniformly for a wrong, expired, or
    /// consumed code, an unknown email, or too many attempts; other failures
    /// via [`CloudError::from_status`].
    pub async fn verify_email(&self, email: &str, code: &str) -> CloudResult<AuthSession> {
        let body = json!({ "email": email, "code": code });
        self.post("/auth/verify-email", Some(&body), Auth::Public)
            .await
    }

    // ===== identity (authenticated) =======================================

    /// `POST /auth/logout` (`204`) — deletes the presented session
    /// server-side; idempotent. The current credential is sent as-is; the
    /// server requires it to be a session token.
    ///
    /// # Errors
    /// [`CloudError::Unauthorized`] when no credential is configured or the
    /// credential is an API key; other failures via [`CloudError::from_status`].
    pub async fn logout(&self) -> CloudResult<()> {
        self.post_unit("/auth/logout", None, Auth::Required).await
    }

    /// `POST /auth/refresh` — slides the presented session's idle deadline
    /// forward (the server resets it to 365 days out) and returns the updated
    /// session row. The token itself is unchanged, so there is nothing new to
    /// persist; this is purely a keep-alive the client fires on launch and
    /// roughly daily so an actively-used install never lapses.
    ///
    /// Session-only server-side: with an API-key credential the server's
    /// `401 Session authentication required` passes through as
    /// [`CloudError::Unauthorized`], as does an expired/revoked session (which
    /// can no longer be refreshed and requires a fresh `verify-email`).
    ///
    /// # Errors
    /// [`CloudError::Unauthorized`] when no credential is configured, it is an
    /// API key, or the session is no longer live; other failures via
    /// [`CloudError::from_status`].
    pub async fn refresh(&self) -> CloudResult<SessionInfo> {
        self.post("/auth/refresh", None, Auth::Required).await
    }

    /// `GET /me` — the caller's profile.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn me(&self) -> CloudResult<UserProfile> {
        self.get("/me").await
    }

    /// `PATCH /me` — sets the caller's globally-unique nickname (which is the
    /// handle). Returns the updated profile.
    ///
    /// # Errors
    /// A `409 Conflict` (the nickname is already taken) surfaces via
    /// [`CloudError::from_status`]; other non-2xx statuses likewise; transport
    /// failures as [`CloudError::NetworkError`].
    pub async fn set_nickname(&self, nickname: &str) -> CloudResult<UserProfile> {
        let body = json!({ "nickname": nickname });
        self.patch("/me", &body).await
    }

    /// `GET /me/api-keys` — key metadata only, never key material.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn api_keys(&self) -> CloudResult<Vec<ApiKeyInfo>> {
        self.get("/me/api-keys").await
    }

    /// `POST /me/api-keys` — mints a new key; the material is shown exactly
    /// once in the response. Session-only server-side: with an API-key
    /// credential the server's `401 Session authentication required` passes
    /// through as [`CloudError::Unauthorized`].
    ///
    /// # Errors
    /// [`CloudError::Unauthorized`] without a session credential; other
    /// failures via [`CloudError::from_status`].
    pub async fn create_api_key(&self) -> CloudResult<CreatedApiKey> {
        self.post("/me/api-keys", None, Auth::Required).await
    }

    /// `DELETE /me/api-keys/{id}`.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn delete_api_key(&self, id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/me/api-keys/{id}")).await
    }

    /// `GET /me/sessions` — the caller's unexpired sessions.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn sessions(&self) -> CloudResult<Vec<SessionInfo>> {
        self.get("/me/sessions").await
    }

    /// `DELETE /me/sessions/{id}`.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn delete_session(&self, id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/me/sessions/{id}")).await
    }

    // ===== social (authenticated + verified email) ========================

    /// `GET /users/lookup?handle=<nickname>` — exact nickname lookup
    /// (case-insensitive). The handle is the user's globally-unique nickname.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] uniformly for a miss, a malformed
    /// nickname, or rate limiting; other failures via [`CloudError::from_status`].
    pub async fn lookup(&self, nickname: &str) -> CloudResult<UserRef> {
        self.get_with_query("/users/lookup", &[("nickname", nickname.to_string())])
            .await
    }

    /// `GET /friends` — accepted friendships, newest first.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn friends(&self) -> CloudResult<Vec<FriendView>> {
        self.get("/friends").await
    }

    /// `GET /friends/requests` — pending requests, both directions.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn friend_requests(&self) -> CloudResult<FriendRequests> {
        self.get("/friends/requests").await
    }

    /// `POST /friends/requests` — `202` always. The server never
    /// distinguishes created / duplicate / blocked / capped outcomes
    /// (enumeration resistance), and neither does this client.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`]. Never an outcome-specific error.
    pub async fn send_friend_request(&self, user_id: Uuid) -> CloudResult<()> {
        let body = json!({ "user_id": user_id });
        self.post_unit("/friends/requests", Some(&body), Auth::Required)
            .await
    }

    /// `POST /friends/requests/{requester_id}/accept` (`204`).
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] uniformly when there is no such
    /// pending request for the caller; other failures via
    /// [`CloudError::from_status`].
    pub async fn accept_friend_request(&self, requester_id: Uuid) -> CloudResult<()> {
        self.post_unit(
            &format!("/friends/requests/{requester_id}/accept"),
            None,
            Auth::Required,
        )
        .await
    }

    /// `DELETE /friends/requests/{user_id}` — declines (as addressee) or
    /// cancels (as requester) the pending request; idempotent `204`.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn cancel_friend_request(&self, user_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/friends/requests/{user_id}")).await
    }

    /// `DELETE /friends/{user_id}` — removes the friendship (and, server
    /// side, all share grants between the pair); idempotent `204`.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn unfriend(&self, user_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/friends/{user_id}")).await
    }

    /// `PUT /blocks/{user_id}` — blocks the user; idempotent `204`. Server
    /// side this also severs any friendship and revokes shares between the
    /// pair in both directions.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn block(&self, user_id: Uuid) -> CloudResult<()> {
        self.put_unit(&format!("/blocks/{user_id}")).await
    }

    /// `DELETE /blocks/{user_id}` — unblocks; restores nothing.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn unblock(&self, user_id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/blocks/{user_id}")).await
    }

    /// `GET /blocks` — the caller's blocks, newest first.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn blocks(&self) -> CloudResult<Vec<BlockView>> {
        self.get("/blocks").await
    }

    // ===== shares (authenticated + verified email) ========================

    /// `POST /shares` — creates (or idempotently updates) a grant.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] uniformly for every denial
    /// (nonexistent subject, not friends, blocked, flags exceed parent, …);
    /// other failures via [`CloudError::from_status`].
    pub async fn create_share(&self, request: CreateShareRequest) -> CloudResult<ShareGrant> {
        let body = serde_json::to_value(request)?;
        self.post("/shares", Some(&body), Auth::Required).await
    }

    /// `GET /shares?direction=given|received` — flat list with depth.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn shares(&self, direction: ShareDirection) -> CloudResult<Vec<ShareGrantRow>> {
        self.get_with_query(
            "/shares",
            &[("direction", direction.as_query_value().to_string())],
        )
        .await
    }

    /// `PATCH /shares/{id}` — adjusts capability flags. Lowering flags
    /// clamps or deletes descendant grants server-side.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] uniformly for nonexistent grants,
    /// unauthorized callers, or escalation attempts; other failures via
    /// [`CloudError::from_status`].
    pub async fn update_share(&self, id: Uuid, patch: SharePatch) -> CloudResult<ShareGrant> {
        let body = serde_json::to_value(patch)?;
        self.patch(&format!("/shares/{id}"), &body).await
    }

    /// `DELETE /shares/{id}` — revokes the grant; the descendant subtree
    /// cascades server-side.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn revoke_share(&self, id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/shares/{id}")).await
    }

    /// `GET /areas/{id}/shares` — the grant tree reaching this area, scoped
    /// to what the caller may see (owner: full tree; re-sharer: own subtree;
    /// grantee: own rows).
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn area_shares(&self, area_id: AreaId) -> CloudResult<Vec<GrantTreeNode>> {
        self.get(&format!("/areas/{area_id}/shares")).await
    }

    // ===== ownership transfer (authenticated) =============================

    /// `POST /areas/{id}/transfer` — offer to transfer area ownership. Raw
    /// `is_owner`-only server-side (a `can_admin` deputy cannot transfer).
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] for a non-owner / nonexistent subject /
    /// gate failure; a `409` (an offer is already live) via [`CloudError::from_status`].
    pub async fn offer_area_transfer(
        &self,
        area_id: AreaId,
        to_user_id: Uuid,
    ) -> CloudResult<TransferView> {
        let body = serde_json::json!({ "to_user_id": to_user_id });
        self.post(&format!("/areas/{area_id}/transfer"), Some(&body), Auth::Required)
            .await
    }

    /// `POST /atlases/{id}/transfer` — offer to transfer atlas ownership (is_owner-only).
    ///
    /// # Errors
    /// As [`Self::offer_area_transfer`].
    pub async fn offer_atlas_transfer(
        &self,
        atlas_id: AtlasId,
        to_user_id: Uuid,
    ) -> CloudResult<TransferView> {
        let body = serde_json::json!({ "to_user_id": to_user_id });
        self.post(&format!("/atlases/{atlas_id}/transfer"), Some(&body), Auth::Required)
            .await
    }

    /// `GET /transfers?direction=offered|received` — the caller's live offers.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport as [`CloudError::NetworkError`].
    pub async fn transfers(&self, direction: TransferDirection) -> CloudResult<Vec<TransferView>> {
        self.get_with_query(
            "/transfers",
            &[("direction", direction.as_query_value().to_string())],
        )
        .await
    }

    /// `POST /transfers/{id}/accept` — recipient accepts; optional atomic rename
    /// and/or refile into a **caller-owned** atlas.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] when the offer is not live / not addressed
    /// to the caller; other failures via [`CloudError::from_status`].
    pub async fn accept_transfer(
        &self,
        id: Uuid,
        name: Option<String>,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<TransferView> {
        let mut body = serde_json::Map::new();
        if let Some(n) = name {
            body.insert("name".to_string(), Value::from(n));
        }
        if let Some(a) = atlas_id {
            body.insert("atlas_id".to_string(), serde_json::to_value(a)?);
        }
        self.post(
            &format!("/transfers/{id}/accept"),
            Some(&Value::Object(body)),
            Auth::Required,
        )
        .await
    }

    /// `POST /transfers/{id}/decline` — recipient declines.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`].
    pub async fn decline_transfer(&self, id: Uuid) -> CloudResult<()> {
        self.post_unit(&format!("/transfers/{id}/decline"), None, Auth::Required)
            .await
    }

    /// `DELETE /transfers/{id}` — initiator cancels a live offer.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`].
    pub async fn cancel_transfer(&self, id: Uuid) -> CloudResult<()> {
        self.delete(&format!("/transfers/{id}")).await
    }

    // ===== per-viewer area prefs (authenticated) ==========================

    /// `GET /me/area-prefs` — the viewer's explicit per-area preferences, the
    /// sync home for the local `disabled_map_areas` set. Returns rows **only**
    /// for currently-viewable areas: a previously-known pref that's absent here
    /// belongs to an area access was lost to and is moot.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn area_prefs(&self) -> CloudResult<Vec<AreaPref>> {
        self.get("/me/area-prefs").await
    }

    /// `PUT /me/area-prefs/{area_id}` — stores an explicit preference and
    /// returns the server-stamped row. Only call this for an area in the
    /// viewer's current `GET /areas` list: the server returns a uniform `404`
    /// when the caller lacks `can_view` (revoked grant, or not theirs), which
    /// is **not an error to surface** — it surfaces here as
    /// [`CloudError::NotFoundOrNoAccess`] and the reconcile treats it as a no-op.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] when the area isn't viewable;
    /// [`CloudError::InvalidInput`] (`400`) for a malformed id; other failures
    /// via [`CloudError::from_status`].
    pub async fn set_area_pref(&self, area_id: AreaId, disabled: bool) -> CloudResult<AreaPref> {
        let body = json!({ "disabled": disabled });
        self.put(&format!("/me/area-prefs/{area_id}"), &body).await
    }

    /// `DELETE /me/area-prefs/{area_id}` — removes the explicit preference
    /// (reverting the area to the enabled default); idempotent `204`.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn delete_area_pref(&self, area_id: AreaId) -> CloudResult<()> {
        self.delete(&format!("/me/area-prefs/{area_id}")).await
    }

    // ===== secrets, preview, copies =======================================

    /// `POST /areas/{id}/secret-marks` — bulk set/clear `is_secret`.
    /// Requires clearance (`can_edit` and owner-or-`include_secrets`);
    /// foreign ids are silently ignored by the server.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] when not cleared; other failures via
    /// [`CloudError::from_status`].
    pub async fn secret_marks(
        &self,
        area_id: AreaId,
        request: &SecretMarksRequest,
    ) -> CloudResult<SecretMarksResult> {
        let body = serde_json::to_value(request)?;
        self.post(
            &format!("/areas/{area_id}/secret-marks"),
            Some(&body),
            Auth::Required,
        )
        .await
    }

    /// `GET /areas/{id}/secrets` — owner-only flat audit list of every
    /// secret-marked entity in the area.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn area_secrets(&self, area_id: AreaId) -> CloudResult<Vec<SecretEntity>> {
        self.get(&format!("/areas/{area_id}/secrets")).await
    }

    /// `GET /areas/{id}/preview` — owner-only "what does this audience see"
    /// simulation. `Ok(None)` means the audience sees nothing (the server
    /// replies `200` with `data: null`).
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] for non-owners; other failures via
    /// [`CloudError::from_status`].
    pub async fn preview(
        &self,
        area_id: AreaId,
        audience: PreviewAudience,
    ) -> CloudResult<Option<AreaWithDetails>> {
        let query: Vec<(&str, String)> = match audience {
            PreviewAudience::WorstCase => Vec::new(),
            PreviewAudience::Share(id) => vec![("share_id", id.to_string())],
            PreviewAudience::AsUser(id) => vec![("as_user", id.to_string())],
        };
        self.get_with_query(&format!("/areas/{area_id}/preview"), &query)
            .await
    }

    /// `POST /areas/{id}/copy` — clones the caller's redacted projection
    /// into a new owned area. The response carries `copied_from_*`
    /// provenance, which [`Area`] already models.
    ///
    /// # Errors
    /// [`CloudError::NotFoundOrNoAccess`] uniformly when the source is not
    /// visible/copyable or the target atlas is not owned; other failures via
    /// [`CloudError::from_status`].
    pub async fn copy_area(&self, area_id: AreaId, request: &CopyAreaRequest) -> CloudResult<Area> {
        let body = serde_json::to_value(request)?;
        self.post(&format!("/areas/{area_id}/copy"), Some(&body), Auth::Required)
            .await
    }

    /// `POST /atlases/{id}/copy` — copies every member the caller can copy
    /// into a new atlas, reporting copied and skipped members.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn copy_atlas(
        &self,
        atlas_id: AtlasId,
        name: Option<String>,
    ) -> CloudResult<AtlasCopyReport> {
        let mut body = serde_json::Map::new();
        if let Some(name) = name {
            body.insert("name".to_string(), Value::String(name));
        }
        let body = Value::Object(body);
        self.post(
            &format!("/atlases/{atlas_id}/copy"),
            Some(&body),
            Auth::Required,
        )
        .await
    }

    /// `GET /sync` — one row per viewable area: projected rev plus access
    /// fingerprint.
    ///
    /// # Errors
    /// Non-2xx statuses via [`CloudError::from_status`]; transport failures as
    /// [`CloudError::NetworkError`].
    pub async fn sync(&self) -> CloudResult<Vec<SyncRow>> {
        self.get("/sync").await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uuid(text: &str) -> Uuid {
        Uuid::parse_str(text).unwrap()
    }

    #[test]
    fn share_scope_serializes_to_exact_wire_shape() {
        let area = ShareScope::Area {
            area_id: AreaId(uuid("123e4567-e89b-12d3-a456-426614174000")),
        };
        assert_eq!(
            serde_json::to_value(area).unwrap(),
            json!({ "area_id": "123e4567-e89b-12d3-a456-426614174000" })
        );

        let atlas = ShareScope::Atlas {
            atlas_id: AtlasId(uuid("00000000-0000-0000-0000-000000000001")),
        };
        assert_eq!(
            serde_json::to_value(atlas).unwrap(),
            json!({ "atlas_id": "00000000-0000-0000-0000-000000000001" })
        );
    }

    #[test]
    fn area_pref_parses_wire_shape() {
        let pref: AreaPref = serde_json::from_value(json!({
            "area_id": "123e4567-e89b-12d3-a456-426614174000",
            "disabled": true,
            "updated_at": "2026-06-14T12:00:00Z",
        }))
        .expect("area pref parses");
        assert_eq!(
            pref.area_id,
            AreaId(uuid("123e4567-e89b-12d3-a456-426614174000"))
        );
        assert!(pref.disabled);
    }

    #[test]
    fn share_grant_parses_s3_handles_and_tolerates_their_absence() {
        // Grantor/owner nickname fields present (a re-share: grantor differs from owner).
        let with: ShareGrant = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "owner_id": "00000000-0000-0000-0000-000000000002",
            "grantor_id": "00000000-0000-0000-0000-000000000003",
            "grantee_id": "00000000-0000-0000-0000-000000000004",
            "area_id": "00000000-0000-0000-0000-000000000005",
            "can_edit": false, "can_reshare": false, "can_copy": false,
            "include_secrets": false,
            "created_at": "2026-06-14T12:00:00Z",
            "updated_at": "2026-06-14T12:00:00Z",
            "grantor_nickname": "friend",
            "owner_nickname": "owner",
            "host_hints": ["arctic.org", "localhost:4000"],
        }))
        .expect("share grant with handles parses");
        assert_eq!(with.grantor_nickname.as_deref(), Some("friend"));
        assert_eq!(with.owner_nickname.as_deref(), Some("owner"));
        assert_eq!(
            with.host_hints.as_deref(),
            Some(&["arctic.org".to_string(), "localhost:4000".to_string()][..])
        );

        // Omit-when-unallocated: handles AND host_hints default to None.
        let without: ShareGrant = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "owner_id": "00000000-0000-0000-0000-000000000002",
            "grantor_id": "00000000-0000-0000-0000-000000000003",
            "grantee_id": "00000000-0000-0000-0000-000000000004",
            "area_id": "00000000-0000-0000-0000-000000000005",
            "can_edit": false, "can_reshare": false, "can_copy": false,
            "include_secrets": false,
            "created_at": "2026-06-14T12:00:00Z",
            "updated_at": "2026-06-14T12:00:00Z",
        }))
        .expect("share grant without handles parses");
        assert!(without.grantor_nickname.is_none());
        assert!(without.owner_nickname.is_none());
        assert!(without.host_hints.is_none());
    }

    #[test]
    fn area_with_family_token_parses_and_is_optional() {
        // family_token present on a list row.
        let with: Area = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000005",
            "user_id": null,
            "atlas_id": null,
            "name": "Mine",
            "created_at": "2026-06-14T12:00:00Z",
            "rev": 1,
            "family_token": "f_0123456789abcdef",
        }))
        .expect("area with family_token parses");
        assert_eq!(with.family_token.as_deref(), Some("f_0123456789abcdef"));

        // Absent for singletons.
        let without: Area = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000005",
            "user_id": null,
            "atlas_id": null,
            "name": "Mine",
            "created_at": "2026-06-14T12:00:00Z",
            "rev": 1,
        }))
        .expect("area without family_token parses");
        assert!(without.family_token.is_none());
    }

    #[test]
    fn area_atlas_name_parses_and_is_optional() {
        // §4.1: a filed area carries the denormalized atlas_name alongside atlas_id.
        let filed: Area = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000005",
            "user_id": null,
            "atlas_id": "00000000-0000-0000-0000-0000000000a1",
            "atlas_name": "Cities",
            "name": "Ironforge",
            "created_at": "2026-06-14T12:00:00Z",
            "rev": 1,
        }))
        .expect("filed area parses");
        assert_eq!(filed.atlas_name.as_deref(), Some("Cities"));

        // An atlas-less area has NO atlas_name key -> None.
        let loose: Area = serde_json::from_value(json!({
            "id": "00000000-0000-0000-0000-000000000005",
            "user_id": null,
            "atlas_id": null,
            "name": "Wilds",
            "created_at": "2026-06-14T12:00:00Z",
            "rev": 1,
        }))
        .expect("atlas-less area parses");
        assert!(loose.atlas_name.is_none());
    }

    #[test]
    fn handle_is_the_nickname() {
        let profile: UserProfile = serde_json::from_value(json!({
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "email": "wbk@example.com",
            "nickname": "wbk",
            "created_at": "2026-06-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(profile.nickname.clone(), Some("wbk".to_string()));

        let unallocated: UserProfile = serde_json::from_value(json!({
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "email": "wbk@example.com",
            "nickname": null,
            "created_at": "2026-06-01T00:00:00Z"
        }))
        .unwrap();
        assert_eq!(unallocated.nickname.clone(), None);
    }

    #[test]
    fn auth_session_parses_verify_email_shape() {
        let session: AuthSession = serde_json::from_value(json!({
            "session_token": "smudgy_sess_0123",
            "user": {
                "id": "123e4567-e89b-12d3-a456-426614174000",
                "email": "wbk@example.com",
                "nickname": null,
                "requested_nickname": "wbk",
                "email_verified_at": "2026-06-11T00:00:00Z",
                "nickname_updated_at": null,
                "created_at": "2026-06-01T00:00:00Z"
            },
            "needs_nickname": true
        }))
        .unwrap();

        assert!(session.needs_nickname);
        assert_eq!(session.session_token, "smudgy_sess_0123");
        assert!(session.user.is_verified());
        assert_eq!(session.user.nickname.clone(), None);
    }

    #[test]
    fn auth_session_parses_returning_user_shape_without_needs_nickname() {
        let session: AuthSession = serde_json::from_value(json!({
            "session_token": "smudgy_sess_4567",
            "user": {
                "id": "123e4567-e89b-12d3-a456-426614174000",
                "email": "wbk@example.com",
                "nickname": "wbk",
                "requested_nickname": "wbk",
                "email_verified_at": "2026-06-11T00:00:00Z",
                "nickname_updated_at": null,
                "created_at": "2026-06-01T00:00:00Z"
            }
        }))
        .unwrap();

        assert!(!session.needs_nickname);
        assert_eq!(session.user.nickname.clone(), Some("wbk".to_string()));
        assert!(session.user.is_verified());
    }

    #[test]
    fn secret_entity_parses_each_kind_with_omitted_fields() {
        let rows: Vec<SecretEntity> = serde_json::from_value(json!([
            { "kind": "room", "room_number": 7 },
            { "kind": "exit", "id": "123e4567-e89b-12d3-a456-426614174000" },
            { "kind": "label", "id": "123e4567-e89b-12d3-a456-426614174001" },
            { "kind": "shape", "id": "123e4567-e89b-12d3-a456-426614174002" },
            { "kind": "room_property", "room_number": 7, "name": "loot" },
            { "kind": "area_property", "name": "owner_notes" }
        ]))
        .unwrap();

        assert_eq!(rows[0].kind, SecretEntityKind::Room);
        assert_eq!(rows[0].room_number, Some(7));
        assert_eq!(rows[0].id, None);
        assert_eq!(rows[0].name, None);

        assert_eq!(rows[1].kind, SecretEntityKind::Exit);
        assert_eq!(
            rows[1].id,
            Some(uuid("123e4567-e89b-12d3-a456-426614174000"))
        );
        assert_eq!(rows[1].room_number, None);

        assert_eq!(rows[2].kind, SecretEntityKind::Label);
        assert_eq!(rows[3].kind, SecretEntityKind::Shape);

        assert_eq!(rows[4].kind, SecretEntityKind::RoomProperty);
        assert_eq!(rows[4].room_number, Some(7));
        assert_eq!(rows[4].name.as_deref(), Some("loot"));

        assert_eq!(rows[5].kind, SecretEntityKind::AreaProperty);
        assert_eq!(rows[5].name.as_deref(), Some("owner_notes"));
        assert_eq!(rows[5].id, None);
        assert_eq!(rows[5].room_number, None);
    }

    #[test]
    fn created_api_key_debug_redacts_key_material() {
        let key: CreatedApiKey = serde_json::from_value(json!({
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "api_key": "smudgy_deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefcafe1234",
            "key_suffix": "cafe1234",
            "created_at": "2026-06-01T00:00:00Z"
        }))
        .unwrap();

        let debug = format!("{key:?}");
        assert!(!debug.contains("smudgy_"));
        assert!(!debug.contains("deadbeef"));
        // The listing-safe suffix is the only key-derived content shown.
        assert!(debug.contains("cafe1234"));
    }

    #[test]
    fn grant_tree_node_parses_flattened_grant() {
        let with_nickname = json!({
            "id": "00000000-0000-0000-0000-00000000000a",
            "owner_id": "00000000-0000-0000-0000-00000000000b",
            "grantor_id": "00000000-0000-0000-0000-00000000000b",
            "grantee_id": "00000000-0000-0000-0000-00000000000c",
            "area_id": "00000000-0000-0000-0000-00000000000d",
            "atlas_id": null,
            "can_edit": true,
            "can_reshare": false,
            "can_copy": true,
            "include_secrets": false,
            "parent_grant_id": null,
            "created_at": "2026-06-01T00:00:00Z",
            "updated_at": "2026-06-02T00:00:00Z",
            "depth": 1,
            "grantee_nickname": "wbk",
            "host_hints": ["arctic.org"]
        });
        let node: GrantTreeNode = serde_json::from_value(with_nickname).unwrap();
        assert_eq!(node.depth, 1);
        assert_eq!(node.grantee_nickname.as_deref(), Some("wbk"));
        assert!(node.grant.can_edit);
        assert!(!node.grant.can_reshare);
        assert!(node.grant.can_copy);
        assert!(!node.grant.include_secrets);
        assert_eq!(
            node.grant.area_id,
            Some(AreaId(uuid("00000000-0000-0000-0000-00000000000d")))
        );
        assert_eq!(node.grant.atlas_id, None);
        assert_eq!(node.grant.parent_grant_id, None);
        assert_eq!(
            node.grant.host_hints.as_deref(),
            Some(&["arctic.org".to_string()][..])
        );

        let without_nickname = json!({
            "id": "00000000-0000-0000-0000-00000000000a",
            "owner_id": "00000000-0000-0000-0000-00000000000b",
            "grantor_id": "00000000-0000-0000-0000-00000000000b",
            "grantee_id": "00000000-0000-0000-0000-00000000000c",
            "area_id": null,
            "atlas_id": "00000000-0000-0000-0000-00000000000e",
            "can_edit": false,
            "can_reshare": false,
            "can_copy": false,
            "include_secrets": false,
            "parent_grant_id": "00000000-0000-0000-0000-00000000000f",
            "created_at": "2026-06-01T00:00:00Z",
            "updated_at": "2026-06-02T00:00:00Z",
            "depth": 0
        });
        let node: GrantTreeNode = serde_json::from_value(without_nickname).unwrap();
        assert_eq!(node.depth, 0);
        assert_eq!(node.grantee_nickname, None);
        assert!(node.grant.host_hints.is_none());
        assert_eq!(
            node.grant.atlas_id,
            Some(AtlasId(uuid("00000000-0000-0000-0000-00000000000e")))
        );
        assert_eq!(
            node.grant.parent_grant_id,
            Some(uuid("00000000-0000-0000-0000-00000000000f"))
        );
    }

    #[test]
    fn share_patch_omits_unset_flags() {
        assert_eq!(
            serde_json::to_value(SharePatch::default()).unwrap(),
            json!({})
        );
        let patch = SharePatch {
            can_edit: Some(false),
            ..SharePatch::default()
        };
        assert_eq!(
            serde_json::to_value(patch).unwrap(),
            json!({ "can_edit": false })
        );
    }

    #[test]
    fn create_share_request_serializes_all_flags_explicitly() {
        let request = CreateShareRequest {
            grantee_id: uuid("00000000-0000-0000-0000-000000000001"),
            scope: ShareScope::Area {
                area_id: AreaId(uuid("00000000-0000-0000-0000-000000000002")),
            },
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            include_secrets: false,
            can_admin: false,
            host_hints: None,
        };
        // All five flags explicit; host_hints omitted when None (old-server compat).
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "grantee_id": "00000000-0000-0000-0000-000000000001",
                "scope": { "area_id": "00000000-0000-0000-0000-000000000002" },
                "can_edit": false,
                "can_reshare": false,
                "can_copy": false,
                "include_secrets": false,
                "can_admin": false
            })
        );

        // When present, host_hints rides on the wire as an array.
        let with_hints = CreateShareRequest {
            grantee_id: uuid("00000000-0000-0000-0000-000000000001"),
            scope: ShareScope::Area {
                area_id: AreaId(uuid("00000000-0000-0000-0000-000000000002")),
            },
            can_edit: false,
            can_reshare: false,
            can_copy: false,
            include_secrets: false,
            can_admin: false,
            host_hints: Some(vec!["arctic.org".to_string()]),
        };
        assert_eq!(
            serde_json::to_value(with_hints).unwrap()["host_hints"],
            json!(["arctic.org"])
        );
    }
}
