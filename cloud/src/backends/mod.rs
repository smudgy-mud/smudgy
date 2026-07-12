use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CreateAreaRequest, Exit, ExitArgs, ExitId, ExitUpdates, Label, LabelArgs, LabelId, LabelUpdates,
    CloudError, CloudResult, Room, RoomUpdates, Shape, ShapeArgs, ShapeId, ShapeUpdates, SyncRow,
    mapper::RoomKey,
};
use async_trait::async_trait;
use uuid::Uuid;

pub mod cached;
pub mod cloud;
pub mod composite;
pub mod local;

pub use cached::{CachedBackend, CachedCloudMapper};
pub use cloud::{CloudMapper, Credential, CredentialSource};
pub use composite::CompositeBackend;
pub use local::LocalBackend;

/// Fingerprint sentinel synthesized for areas served without an access block
/// (legacy servers). Pairs with a client-side fingerprint of `None`; caching
/// layers normalize the two representations when comparing.
pub const LEGACY_ACCESS_FINGERPRINT: &str = "legacy";

/// Core trait defining all mapping operations
#[async_trait]
pub trait MapperBackend: Send + Sync {
    // ===== AREA OPERATIONS =====

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area>;

    async fn list_areas(&self) -> CloudResult<Vec<Area>>;

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails>;

    fn last_area_source(&self, _area_id: &AreaId) -> AreaLoadSource {
        AreaLoadSource::Unknown
    }

    /// Persist a full, already-finalized area to the LOCAL tier in one shot — the JSON-import
    /// fast path (avoids replaying it room-by-room). Default: unsupported; only the local and
    /// composite backends implement it, since import is local-only.
    async fn import_local_area(&self, _details: AreaWithDetails) -> CloudResult<()> {
        Err(CloudError::InternalError(
            "this backend does not support local area import".to_string(),
        ))
    }

    // ===== SYNC / IDENTITY =====

    /// One row per viewable area: projected rev + access fingerprint.
    /// `Ok(None)` means the backend has no `/sync` support and callers should
    /// fall back to `list_areas` reconciliation.
    async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
        Ok(None)
    }

    /// The authenticated user's id, when the backend can resolve one. Used to
    /// scope on-disk caches per viewer.
    async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
        Ok(None)
    }

    /// Bumped whenever the backend's credential changes; pollers use it to
    /// detect login/logout and trigger a full resync.
    fn auth_generation(&self) -> u64 {
        0
    }

    /// Whether the backend currently holds any credential. Credential-less
    /// backends fail every request; callers can skip work (and user-facing
    /// noise) instead of attempting it.
    fn has_credential(&self) -> bool {
        true
    }

    /// Drop every cached copy of an area (memory and disk). Default no-op for
    /// backends without a cache.
    async fn purge_area(&self, _area_id: &AreaId) {}

    /// Record the latest server sync rows so later `get_area` calls bypass
    /// stale caches; cached entries absent from `rows` are evicted (their
    /// bytes may hold secrets the viewer no longer has access to). Default
    /// no-op for backends without a cache.
    async fn note_sync_rows(&self, _rows: &[SyncRow]) {}

    /// Whether this backend serves real `/sync` data worth polling.
    fn supports_sync(&self) -> bool {
        false
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()>;

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()>;

    // ===== ATLAS (FOLDER) OPERATIONS =====
    //
    // Atlases are folders grouping the viewer's *own* areas. Only owned
    // atlases are ever listed (atlases shared *to* the viewer surface through
    // the by-sharer area grouping, never here). Backends without a folder
    // notion inherit the no-op/unsupported defaults.

    /// List the viewer's own atlases. Default: none.
    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        Ok(Vec::new())
    }

    /// Create an empty atlas (folder). Default: unsupported.
    async fn create_atlas(&self, _name: &str) -> CloudResult<Atlas> {
        Err(CloudError::InvalidInput(
            "this backend does not support atlases".to_string(),
        ))
    }

    /// Create an empty atlas with an explicit tier preference (`prefer_local`).
    /// Single-tier backends ignore the hint; a composite backend routes by it.
    /// Default: delegate to [`Self::create_atlas`].
    async fn create_atlas_in(&self, name: &str, prefer_local: bool) -> CloudResult<Atlas> {
        let _ = prefer_local;
        self.create_atlas(name).await
    }

    /// Rename an atlas. Default: unsupported.
    async fn rename_atlas(&self, _atlas_id: &AtlasId, _name: &str) -> CloudResult<Atlas> {
        Err(CloudError::InvalidInput(
            "this backend does not support atlases".to_string(),
        ))
    }

    /// Delete an atlas. Member areas survive and become loose
    /// (`atlas_id -> NULL`); they are not deleted. Default: unsupported.
    async fn delete_atlas(&self, _atlas_id: &AtlasId) -> CloudResult<()> {
        Err(CloudError::InvalidInput(
            "this backend does not support atlases".to_string(),
        ))
    }

    /// File `area_id` into `atlas_id` (`Some`) or pull it loose (`None`).
    ///
    /// Sends *only* the `atlas_id` key — a name-only rename must omit it
    /// (present+null clears, absent leaves unchanged). The provided
    /// implementation routes through [`Self::update_area`], so caching layers
    /// invalidate the moved area automatically.
    async fn move_area_to_atlas(
        &self,
        area_id: &AreaId,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<()> {
        self.update_area(
            area_id,
            AreaUpdates {
                name: None,
                atlas_id: Some(atlas_id),
            },
        )
        .await
    }

    // ===== TIER INTROSPECTION (multi-tier backends only) =====
    //
    // A single-tier backend owns everything it serves, so the defaults are
    // empty (callers read "nothing is specifically local-tier"). A composite
    // backend overrides these so the UI can gate tier-specific affordances
    // (cloud-only sharing; keeping cross-tier moves out of the picker).

    /// Atlas ids served by a *local* (never-synced, on-disk) tier.
    fn local_atlas_ids(&self) -> std::collections::HashSet<AtlasId> {
        std::collections::HashSet::new()
    }

    /// Area ids served by a *local* tier.
    fn local_area_ids(&self) -> std::collections::HashSet<AreaId> {
        std::collections::HashSet::new()
    }

    // ===== AREA PROPERTIES =====

    async fn set_area_property(&self, area_id: &AreaId, name: &str, value: &str) -> CloudResult<()>;

    async fn delete_area_property(&self, area_id: &AreaId, name: &str) -> CloudResult<()>;

    // ===== ROOM OPERATIONS =====

    async fn update_room(&self, room_key: &RoomKey, updates: RoomUpdates) -> CloudResult<Room>;

    async fn delete_room(&self, room_key: &RoomKey) -> CloudResult<()>;

    // ===== ROOM PROPERTIES =====

    async fn set_room_property(&self, room_key: &RoomKey, name: &str, value: &str)
    -> CloudResult<()>;

    async fn delete_room_property(&self, room_key: &RoomKey, name: &str) -> CloudResult<()>;

    // ===== ROOM TAGS =====

    /// Add a tag to a room (idempotent). `tag` is pre-normalized (UPPERCASE).
    async fn add_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()>;

    /// Remove a tag from a room. `tag` is pre-normalized (UPPERCASE).
    async fn remove_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()>;

    // ===== EXIT OPERATIONS =====

    async fn create_room_exit(&self, room_key: &RoomKey, exit_data: ExitArgs) -> CloudResult<Exit>;

    async fn update_exit(
        &self,
        area_id: &AreaId,
        exit_id: &ExitId,
        updates: ExitUpdates,
    ) -> CloudResult<()>;

    async fn delete_exit(&self, area_id: &AreaId, exit_id: &ExitId) -> CloudResult<()>;

    // ===== LABEL OPERATIONS =====

    async fn create_label(&self, area_id: &AreaId, label_data: LabelArgs) -> CloudResult<Label>;

    async fn update_label(
        &self,
        area_id: &AreaId,
        label_id: &LabelId,
        updates: LabelUpdates,
    ) -> CloudResult<()>;

    async fn delete_label(&self, area_id: &AreaId, label_id: &LabelId) -> CloudResult<()>;

    // ===== SHAPE OPERATIONS =====

    async fn create_shape(&self, area_id: &AreaId, shape_data: ShapeArgs) -> CloudResult<Shape>;

    async fn update_shape(
        &self,
        area_id: &AreaId,
        shape_id: &ShapeId,
        updates: ShapeUpdates,
    ) -> CloudResult<()>;

    async fn delete_shape(&self, area_id: &AreaId, shape_id: &ShapeId) -> CloudResult<()>;
}
