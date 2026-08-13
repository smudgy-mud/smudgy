use std::{fmt, sync::Arc};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use log::{info, trace};
use reqwest::Client;
use serde_json::json;
use uuid::Uuid;

use super::MapperBackend;
use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CloudResult, CreateAreaRequest, MapStorage, SyncRow,
};

/// A cloud API credential. The server dispatches on the token prefix:
/// `smudgy_sess_…` hits the sessions table, anything else the API keys.
#[derive(Clone, PartialEq, Eq)]
pub enum Credential {
    ApiKey(String),
    Session(String),
}

impl Credential {
    #[must_use]
    pub fn header_value(&self) -> String {
        match self {
            Self::ApiKey(token) | Self::Session(token) => format!("Bearer {token}"),
        }
    }

    #[must_use]
    pub const fn is_session(&self) -> bool {
        matches!(self, Self::Session(_))
    }

    fn suffix(&self) -> &str {
        let token = match self {
            Self::ApiKey(token) | Self::Session(token) => token,
        };
        let len = token.len();
        &token[len.saturating_sub(4)..]
    }
}

// Token material must never reach logs; only the variant and a short suffix.
impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApiKey(_) => write!(f, "Credential::ApiKey(…{})", self.suffix()),
            Self::Session(_) => write!(f, "Credential::Session(…{})", self.suffix()),
        }
    }
}

/// Shared, hot-swappable credential slot. Cloning is cheap; all clones see
/// updates immediately, so logging in upgrades every live mapper at once.
#[derive(Clone)]
pub struct CredentialSource {
    slot: Arc<ArcSwap<CredentialSnapshot>>,
}

#[derive(Clone)]
struct CredentialSnapshot {
    generation: u64,
    credential: Option<Credential>,
}

impl CredentialSource {
    #[must_use]
    pub fn new(initial: Option<Credential>) -> Self {
        Self {
            slot: Arc::new(ArcSwap::from_pointee(CredentialSnapshot {
                generation: 0,
                credential: initial,
            })),
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::new(None)
    }

    pub fn set(&self, credential: Option<Credential>) {
        // Credential and generation are one atomic ArcSwap snapshot: no
        // reader can pair a new token with an old generation, even when
        // multiple sessions update the shared source concurrently.
        self.slot.rcu(|current| {
            Arc::new(CredentialSnapshot {
                generation: current.generation.wrapping_add(1),
                credential: credential.clone(),
            })
        });
    }

    #[must_use]
    pub fn get(&self) -> Option<Credential> {
        self.slot.load().credential.clone()
    }

    #[must_use]
    fn snapshot(&self) -> (u64, Option<Credential>) {
        let snapshot = self.slot.load();
        (snapshot.generation, snapshot.credential.clone())
    }

    fn credential_at_generation(&self, generation: u64) -> CloudResult<Credential> {
        let (current, credential) = self.snapshot();
        if current != generation {
            return Err(CloudError::CredentialChanged);
        }
        credential.ok_or_else(|| CloudError::Unauthorized("no credential configured".to_string()))
    }

    /// Monotonic counter bumped on every credential change; pollers compare
    /// it to detect login/logout without holding the credential itself.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.slot.load().generation
    }
}

impl fmt::Debug for CredentialSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CredentialSource")
            .field("credential", &self.get())
            .field("generation", &self.generation())
            .finish()
    }
}

impl Default for CredentialSource {
    fn default() -> Self {
        Self::empty()
    }
}

/// HTTP client for the cloud-based map API
#[derive(Debug)]
pub struct CloudMapper {
    client: Client,
    base_url: String,
    credentials: CredentialSource,
}

impl CloudMapper {
    /// Create a new `CloudMapper` instance authenticating with a fixed API
    /// key.
    #[must_use]
    pub fn new(base_url: String, api_key: String) -> Self {
        Self::with_credentials(
            base_url,
            CredentialSource::new(Some(Credential::ApiKey(api_key))),
        )
    }

    /// Create a `CloudMapper` over a shared, hot-swappable credential source.
    #[must_use]
    pub fn with_credentials(base_url: String, credentials: CredentialSource) -> Self {
        Self {
            client: crate::versioned_http_client(),
            base_url: base_url.trim_end_matches('/').to_string(),
            credentials,
        }
    }

    #[must_use]
    pub fn credentials(&self) -> &CredentialSource {
        &self.credentials
    }

    #[must_use]
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Helper method to get authorization header
    fn auth_header(&self) -> CloudResult<String> {
        self.credentials
            .get()
            .map(|credential| credential.header_value())
            .ok_or_else(|| CloudError::Unauthorized("no credential configured".to_string()))
    }

    /// Parses a response: unwraps the `{success, data, error}` envelope on
    /// success and maps error statuses onto the client error taxonomy.
    async fn parse_data<T>(response: reqwest::Response) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let status = response.status();
        if status.is_success() {
            let json: serde_json::Value = response.json().await?;
            json.get("data").map_or_else(
                || {
                    Err(CloudError::SerializationError(
                        "Missing data field in response".to_string(),
                    ))
                },
                |data| {
                    let result: T = serde_json::from_value(data.clone())?;
                    Ok(result)
                },
            )
        } else {
            Err(Self::error_for(status.as_u16(), response).await)
        }
    }

    async fn parse_no_data(response: reqwest::Response) -> CloudResult<()> {
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(Self::error_for(status.as_u16(), response).await)
        }
    }

    async fn error_for(status: u16, response: reqwest::Response) -> CloudError {
        let text = response.text().await.unwrap_or_default();
        let body = serde_json::from_str::<serde_json::Value>(&text).ok();
        let message = body
            .as_ref()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.as_str())
                    .map(ToString::to_string)
            })
            .unwrap_or(text);
        // The CAS conflicts carry their fields in a structured `details`
        // object beside the machine-readable `error` code.
        let details = body.as_ref().and_then(|value| value.get("details"));
        CloudError::from_response(status, &message, details)
    }

    /// Helper method to make GET requests
    async fn get<T>(&self, path: &str) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);

        info!("GET {url} - (initiating)");

        let response = self
            .client
            .get(&url)
            .header("authorization", self.auth_header()?)
            .header("content-type", "application/json")
            .send()
            .await?;

        info!("GET {url} - {}", response.status());

        Self::parse_data(response).await
    }

    async fn get_with_credential<T>(&self, path: &str, credential: &Credential) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
    {
        let url = format!("{}{}", self.base_url, path);
        info!("GET {url} - (initiating)");
        let response = self
            .client
            .get(&url)
            .header("authorization", credential.header_value())
            .header("content-type", "application/json")
            .send()
            .await?;
        info!("GET {url} - {}", response.status());
        Self::parse_data(response).await
    }

    /// Helper method to make POST requests
    async fn post<T, B>(&self, path: &str, body: &B) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize,
    {
        let url = format!("{}{}", self.base_url, path);

        info!("POST {url}");
        trace!("Body: {:?}", serde_json::to_string(body));

        let response = self
            .client
            .post(&url)
            .header("authorization", self.auth_header()?)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await?;

        info!("POST {url} - {}", response.status());

        Self::parse_data(response).await
    }

    async fn post_with_credential<T, B>(
        &self,
        path: &str,
        body: &B,
        credential: &Credential,
    ) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize,
    {
        let url = format!("{}{}", self.base_url, path);
        info!("POST {url}");
        let response = self
            .client
            .post(&url)
            .header("authorization", credential.header_value())
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await?;
        info!("POST {url} - {}", response.status());
        Self::parse_data(response).await
    }

    /// Helper method to make PATCH requests
    async fn patch<T, B>(&self, path: &str, body: &B) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
        B: serde::Serialize,
    {
        let url = format!("{}{}", self.base_url, path);

        info!("PATCH {url}");
        trace!("Body: {:?}", serde_json::to_string(body));

        let response = self
            .client
            .patch(&url)
            .header("authorization", self.auth_header()?)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await?;

        info!("PATCH {url} - {}", response.status());

        Self::parse_data(response).await
    }

    /// Helper method to make PUT requests without expecting response data
    async fn put_no_response<B>(&self, path: &str, body: &B) -> CloudResult<()>
    where
        B: serde::Serialize,
    {
        let url = format!("{}{}", self.base_url, path);

        info!("PUT {url}");
        trace!("Body: {:?}", serde_json::to_string(body));

        let response = self
            .client
            .put(&url)
            .header("authorization", self.auth_header()?)
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await?;

        info!("PUT {url} - {}", response.status());

        Self::parse_no_data(response).await
    }

    async fn put_no_response_with_credential<B>(
        &self,
        path: &str,
        body: &B,
        credential: &Credential,
    ) -> CloudResult<()>
    where
        B: serde::Serialize,
    {
        let url = format!("{}{}", self.base_url, path);
        info!("PUT {url}");
        let response = self
            .client
            .put(&url)
            .header("authorization", credential.header_value())
            .header("content-type", "application/json")
            .json(body)
            .send()
            .await?;
        info!("PUT {url} - {}", response.status());
        Self::parse_no_data(response).await
    }

    /// Helper method to make DELETE requests
    async fn delete(&self, path: &str) -> CloudResult<()> {
        let url = format!("{}{}", self.base_url, path);

        info!("DELETE {url}");

        let response = self
            .client
            .delete(&url)
            .header("authorization", self.auth_header()?)
            .send()
            .await?;

        info!("DELETE {url} - {}", response.status());

        Self::parse_no_data(response).await
    }

    async fn delete_with_credential(&self, path: &str, credential: &Credential) -> CloudResult<()> {
        let url = format!("{}{}", self.base_url, path);
        info!("DELETE {url}");
        let response = self
            .client
            .delete(&url)
            .header("authorization", credential.header_value())
            .send()
            .await?;
        info!("DELETE {url} - {}", response.status());
        Self::parse_no_data(response).await
    }
}

/// `DELETE /areas/{id}` path, carrying the optional `?expected_rev=N`
/// precondition. On a smudgy-web release that knows the parameter, a
/// mismatch answers 409 `revision_conflict` (mapped to
/// [`crate::CloudError::RevisionConflict`] by the shared error path); an
/// OLDER server silently ignores the query string and deletes
/// unconditionally, which is why callers keep the client-side
/// compare-then-delete as the enforcement floor.
fn delete_area_path(area_id: &AreaId, expected_rev: Option<i64>) -> String {
    match expected_rev {
        Some(rev) => format!("/areas/{area_id}?expected_rev={rev}"),
        None => format!("/areas/{area_id}"),
    }
}

#[async_trait]
impl MapperBackend for CloudMapper {
    // ===== AREA OPERATIONS =====

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        self.post("/areas", &request).await
    }

    async fn create_area_at(
        &self,
        request: CreateAreaRequest,
        storage: MapStorage,
    ) -> CloudResult<Area> {
        if storage != MapStorage::Cloud {
            return Err(CloudError::InvalidInput(format!(
                "the cloud backend cannot create a {storage} map"
            )));
        }
        self.create_area(request).await
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        self.get("/areas").await
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.get(&format!("/areas/{area_id}")).await
    }

    async fn get_area_at_generation(
        &self,
        area_id: &AreaId,
        auth_generation: u64,
    ) -> CloudResult<AreaWithDetails> {
        let credential = self.credentials.credential_at_generation(auth_generation)?;
        let area = self
            .get_with_credential(&format!("/areas/{area_id}"), &credential)
            .await?;
        if self.credentials.generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        Ok(area)
    }

    fn last_area_source(&self, _area_id: &AreaId) -> AreaLoadSource {
        AreaLoadSource::Remote
    }

    async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
        let rows: Vec<SyncRow> = self.get("/sync").await?;
        Ok(Some(rows))
    }

    async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
        #[derive(serde::Deserialize)]
        struct Me {
            id: Uuid,
        }
        let me: Me = self.get("/me").await?;
        Ok(Some(me.id))
    }

    async fn viewer_identity_at_generation(
        &self,
        auth_generation: u64,
    ) -> CloudResult<Option<Uuid>> {
        #[derive(serde::Deserialize)]
        struct Me {
            id: Uuid,
        }
        let credential = self.credentials.credential_at_generation(auth_generation)?;
        let me: Me = self.get_with_credential("/me", &credential).await?;
        if self.credentials.generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        Ok(Some(me.id))
    }

    fn auth_generation(&self) -> u64 {
        self.credentials.generation()
    }

    fn has_credential(&self) -> bool {
        self.credentials.get().is_some()
    }

    fn supports_sync(&self) -> bool {
        true
    }

    fn mutation_journal_namespace(&self) -> Option<String> {
        Some(self.base_url.clone())
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.put_no_response(&format!("/areas/{area_id}"), &updates)
            .await
    }

    async fn update_area_at_generation(
        &self,
        area_id: &AreaId,
        updates: AreaUpdates,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let credential = self.credentials.credential_at_generation(auth_generation)?;
        self.put_no_response_with_credential(&format!("/areas/{area_id}"), &updates, &credential)
            .await
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        self.delete(&format!("/areas/{area_id}")).await
    }

    async fn delete_area_expecting(
        &self,
        area_id: &AreaId,
        expected_rev: Option<i64>,
    ) -> CloudResult<()> {
        self.delete(&delete_area_path(area_id, expected_rev)).await
    }

    async fn copy_cloud_area(
        &self,
        source: &AreaId,
        name: &str,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<Option<Area>> {
        let request = crate::cloud_api::CopyAreaRequest {
            name: Some(name.to_string()),
            atlas_id,
        };
        let area: Area = self
            .post(&format!("/areas/{source}/copy"), &request)
            .await?;
        Ok(Some(area))
    }

    async fn delete_area_at_generation(
        &self,
        area_id: &AreaId,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let credential = self.credentials.credential_at_generation(auth_generation)?;
        self.delete_with_credential(&format!("/areas/{area_id}"), &credential)
            .await
    }

    async fn delete_area_expecting_at_generation(
        &self,
        area_id: &AreaId,
        expected_rev: Option<i64>,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let credential = self.credentials.credential_at_generation(auth_generation)?;
        self.delete_with_credential(&delete_area_path(area_id, expected_rev), &credential)
            .await
    }

    // ===== VERSIONED MUTATIONS =====

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &crate::mutation::MutationEnvelope,
    ) -> CloudResult<crate::mutation::MutationResult> {
        self.post(&format!("/areas/{area_id}/mutations"), envelope)
            .await
    }

    async fn execute_mutation_at_generation(
        &self,
        area_id: &AreaId,
        envelope: &crate::mutation::MutationEnvelope,
        auth_generation: u64,
    ) -> CloudResult<crate::mutation::MutationResult> {
        let credential = self.credentials.credential_at_generation(auth_generation)?;
        self.post_with_credential(
            &format!("/areas/{area_id}/mutations"),
            envelope,
            &credential,
        )
        .await
    }

    // ===== ATLAS (FOLDER) OPERATIONS =====

    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        self.get("/atlases").await
    }

    async fn create_atlas(&self, name: &str) -> CloudResult<Atlas> {
        self.post("/atlases", &json!({ "name": name })).await
    }

    async fn create_atlas_at(&self, name: &str, storage: MapStorage) -> CloudResult<Atlas> {
        if storage != MapStorage::Cloud {
            return Err(CloudError::InvalidInput(format!(
                "the cloud backend cannot create a {storage} atlas"
            )));
        }
        self.create_atlas(name).await
    }

    async fn rename_atlas(&self, atlas_id: &AtlasId, name: &str) -> CloudResult<Atlas> {
        self.patch(&format!("/atlases/{atlas_id}"), &json!({ "name": name }))
            .await
    }

    async fn delete_atlas(&self, atlas_id: &AtlasId) -> CloudResult<()> {
        self.delete(&format!("/atlases/{atlas_id}")).await
    }

    // `move_area_to_atlas` uses the trait default (PUT /areas/{id} with only
    // `atlas_id`), routed through `update_area` above.
}
