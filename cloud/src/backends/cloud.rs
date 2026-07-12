use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use log::{info, trace};
use reqwest::Client;
use serde_json::json;
use uuid::Uuid;

use super::MapperBackend;
use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CreateAreaRequest, Exit, ExitArgs, ExitId, ExitUpdates, Label, LabelArgs, LabelId, LabelUpdates,
    CloudError, CloudResult, Room, RoomUpdates, Shape, ShapeArgs, ShapeId, ShapeUpdates, SyncRow,
    mapper::RoomKey,
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
    slot: Arc<ArcSwap<Option<Credential>>>,
    generation: Arc<AtomicU64>,
}

impl CredentialSource {
    #[must_use]
    pub fn new(initial: Option<Credential>) -> Self {
        Self {
            slot: Arc::new(ArcSwap::from_pointee(initial)),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }

    #[must_use]
    pub fn empty() -> Self {
        Self::new(None)
    }

    pub fn set(&self, credential: Option<Credential>) {
        self.slot.store(Arc::new(credential));
        self.generation.fetch_add(1, Ordering::Release);
    }

    #[must_use]
    pub fn get(&self) -> Option<Credential> {
        self.slot.load().as_ref().clone()
    }

    /// Monotonic counter bumped on every credential change; pollers compare
    /// it to detect login/logout without holding the credential itself.
    #[must_use]
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
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
        let message = serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .and_then(|error| error.as_str())
                    .map(ToString::to_string)
            })
            .unwrap_or(text);
        CloudError::from_status(status, &message)
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

    /// Helper method to make PUT requests
    async fn put<T, B>(&self, path: &str, body: &B) -> CloudResult<T>
    where
        T: serde::de::DeserializeOwned,
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
}

#[async_trait]
impl MapperBackend for CloudMapper {
    // ===== AREA OPERATIONS =====

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        self.post("/areas", &request).await
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        self.get("/areas").await
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.get(&format!("/areas/{area_id}")).await
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

    fn auth_generation(&self) -> u64 {
        self.credentials.generation()
    }

    fn has_credential(&self) -> bool {
        self.credentials.get().is_some()
    }

    fn supports_sync(&self) -> bool {
        true
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.put_no_response(&format!("/areas/{area_id}"), &updates)
            .await
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        self.delete(&format!("/areas/{area_id}")).await
    }

    // ===== ATLAS (FOLDER) OPERATIONS =====

    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        self.get("/atlases").await
    }

    async fn create_atlas(&self, name: &str) -> CloudResult<Atlas> {
        self.post("/atlases", &json!({ "name": name })).await
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

    // ===== AREA PROPERTIES =====

    async fn set_area_property(&self, area_id: &AreaId, name: &str, value: &str) -> CloudResult<()> {
        let body = json!({ "value": value });
        self.put_no_response(&format!("/areas/{area_id}/properties/{name}"), &body)
            .await
    }

    async fn delete_area_property(&self, area_id: &AreaId, name: &str) -> CloudResult<()> {
        self.delete(&format!("/areas/{area_id}/properties/{name}"))
            .await
    }

    // ===== ROOM OPERATIONS =====

    async fn update_room(&self, room_key: &RoomKey, updates: RoomUpdates) -> CloudResult<Room> {
        self.put(
            &format!("/areas/{}/{}", room_key.area_id, room_key.room_number),
            &updates,
        )
        .await
    }

    async fn delete_room(&self, room_key: &RoomKey) -> CloudResult<()> {
        self.delete(&format!(
            "/areas/{}/rooms/{}",
            room_key.area_id, room_key.room_number
        ))
        .await
    }

    // ===== ROOM PROPERTIES =====

    async fn set_room_property(
        &self,
        room_key: &RoomKey,
        name: &str,
        value: &str,
    ) -> CloudResult<()> {
        let body = json!({ "value": value });
        self.put_no_response(
            &format!(
                "/areas/{}/rooms/{}/properties/{}",
                room_key.area_id, room_key.room_number, name
            ),
            &body,
        )
        .await
    }

    async fn delete_room_property(&self, room_key: &RoomKey, name: &str) -> CloudResult<()> {
        self.delete(&format!(
            "/areas/{}/rooms/{}/properties/{}",
            room_key.area_id, room_key.room_number, name
        ))
        .await
    }

    // ===== ROOM TAGS =====

    async fn add_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()> {
        // The tag is carried in the path; the body is unused by the server.
        self.put_no_response(
            &format!(
                "/areas/{}/rooms/{}/tags/{}",
                room_key.area_id, room_key.room_number, tag
            ),
            &json!({}),
        )
        .await
    }

    async fn remove_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()> {
        self.delete(&format!(
            "/areas/{}/rooms/{}/tags/{}",
            room_key.area_id, room_key.room_number, tag
        ))
        .await
    }

    // ===== EXIT OPERATIONS =====

    async fn create_room_exit(&self, room_key: &RoomKey, exit_data: ExitArgs) -> CloudResult<Exit> {
        self.post(
            &format!(
                "/areas/{}/rooms/{}/exits",
                room_key.area_id, room_key.room_number
            ),
            &exit_data,
        )
        .await
    }

    async fn update_exit(
        &self,
        area_id: &AreaId,
        exit_id: &ExitId,
        updates: ExitUpdates,
    ) -> CloudResult<()> {
        self.put_no_response(&format!("/areas/{area_id}/exits/{exit_id}"), &updates)
            .await
    }

    async fn delete_exit(&self, area_id: &AreaId, exit_id: &ExitId) -> CloudResult<()> {
        self.delete(&format!("/areas/{area_id}/exits/{exit_id}"))
            .await
    }

    // ===== LABEL OPERATIONS =====

    async fn create_label(&self, area_id: &AreaId, label_data: LabelArgs) -> CloudResult<Label> {
        self.post(&format!("/areas/{area_id}/labels"), &label_data)
            .await
    }

    async fn update_label(
        &self,
        area_id: &AreaId,
        label_id: &LabelId,
        updates: LabelUpdates,
    ) -> CloudResult<()> {
        self.put_no_response(&format!("/areas/{area_id}/labels/{label_id}"), &updates)
            .await
    }

    async fn delete_label(&self, area_id: &AreaId, label_id: &LabelId) -> CloudResult<()> {
        self.delete(&format!("/areas/{area_id}/labels/{label_id}"))
            .await
    }

    // ===== SHAPE OPERATIONS =====

    async fn create_shape(&self, area_id: &AreaId, shape_data: ShapeArgs) -> CloudResult<Shape> {
        self.post(&format!("/areas/{area_id}/shapes"), &shape_data)
            .await
    }

    async fn update_shape(
        &self,
        area_id: &AreaId,
        shape_id: &ShapeId,
        updates: ShapeUpdates,
    ) -> CloudResult<()> {
        self.put_no_response(&format!("/areas/{area_id}/shapes/{shape_id}"), &updates)
            .await
    }

    async fn delete_shape(&self, area_id: &AreaId, shape_id: &ShapeId) -> CloudResult<()> {
        self.delete(&format!("/areas/{area_id}/shapes/{shape_id}"))
            .await
    }
}
