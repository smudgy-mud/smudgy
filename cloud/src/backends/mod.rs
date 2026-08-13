use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CloudResult, CreateAreaRequest, MapStorage, SyncRow,
    mutation::{MutationEnvelope, MutationResult},
};
use async_trait::async_trait;
use uuid::Uuid;

pub(crate) mod area_edits;
pub mod cached;
pub mod cloud;
pub mod composite;
pub mod ephemeral;
pub mod local;
pub mod local_migration;

pub use cached::{CachedBackend, CachedCloudMapper};
pub use cloud::{CloudMapper, Credential, CredentialSource};
pub use composite::CompositeBackend;
pub use ephemeral::EphemeralBackend;
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

    /// Create an area in an explicit storage tier. A backend must either
    /// prove it implements that tier or reject the request; silently ignoring
    /// the tier would make the canonical API lie about where data landed.
    async fn create_area_at(
        &self,
        _request: CreateAreaRequest,
        storage: MapStorage,
    ) -> CloudResult<Area> {
        Err(CloudError::InvalidInput(format!(
            "this backend does not support explicit {storage} map creation"
        )))
    }

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

    /// Server-side clone of one **cloud-tier** area into a new cloud area
    /// (`POST /areas/{id}/copy`): one transaction on the server instead of
    /// replaying the content as serial CAS envelopes. The server mints fresh
    /// area/connection/exit identities, preserves room numbers, and records
    /// `copied_from_*` provenance. `Ok(None)` means no server-side copy is
    /// available for this area (not cloud-tier, signed out, or no server
    /// behind the backend); callers fall back to content replay, which every
    /// tier supports.
    async fn copy_cloud_area(
        &self,
        _source: &AreaId,
        _name: &str,
        _atlas_id: Option<AtlasId>,
    ) -> CloudResult<Option<Area>> {
        Ok(None)
    }

    // ===== SYNC / IDENTITY =====

    /// One row per viewable area: shared revision + access fingerprint.
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

    /// Resolve identity using the exact credential generation captured by the
    /// caller. Cloud backends override this to build the request from one
    /// atomic credential snapshot.
    async fn viewer_identity_at_generation(
        &self,
        auth_generation: u64,
    ) -> CloudResult<Option<Uuid>> {
        if self.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        let identity = self.viewer_identity().await?;
        if self.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        Ok(identity)
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

    /// Stable namespace for durable cloud writes. Cloud-capable wrappers must
    /// forward the canonical API origin so identical viewer/area UUIDs from
    /// different deployments can never share a replay queue.
    fn mutation_journal_namespace(&self) -> Option<String> {
        None
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()>;

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()>;

    /// Read an area using the exact credential generation captured by the
    /// caller, rejecting a response that straddled a credential change.
    async fn get_area_at_generation(
        &self,
        area_id: &AreaId,
        auth_generation: u64,
    ) -> CloudResult<AreaWithDetails> {
        if self.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        let area = self.get_area(area_id).await?;
        if self.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        Ok(area)
    }

    /// Metadata-write counterparts to the generation-bound mutation API.
    async fn update_area_at_generation(
        &self,
        area_id: &AreaId,
        updates: AreaUpdates,
        auth_generation: u64,
    ) -> CloudResult<()> {
        if self.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        self.update_area(area_id, updates).await
    }

    async fn delete_area_at_generation(
        &self,
        area_id: &AreaId,
        auth_generation: u64,
    ) -> CloudResult<()> {
        if self.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        self.delete_area(area_id).await
    }

    // ===== VERSIONED MUTATIONS (the CAS envelope) =====

    /// Applies one mutation envelope to an area atomically, honoring its
    /// preconditions (revision + access fingerprint) and its idempotent
    /// operation id. This is the one write path every mapper content
    /// mutation compiles to.
    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult>;

    /// Execute with the credential generation captured when this operation's
    /// viewer was activated. Cloud backends override this to build the request
    /// from one atomic credential snapshot.
    async fn execute_mutation_at_generation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
        auth_generation: u64,
    ) -> CloudResult<MutationResult> {
        if self.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        self.execute_mutation(area_id, envelope).await
    }

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

    /// Create an atlas in an explicit durable storage tier. As with areas,
    /// the default rejects rather than silently choosing a backend.
    async fn create_atlas_at(&self, _name: &str, storage: MapStorage) -> CloudResult<Atlas> {
        Err(CloudError::InvalidInput(format!(
            "this backend does not support explicit {storage} atlas creation"
        )))
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

    async fn move_area_to_atlas_at_generation(
        &self,
        area_id: &AreaId,
        atlas_id: Option<AtlasId>,
        auth_generation: u64,
    ) -> CloudResult<()> {
        self.update_area_at_generation(
            area_id,
            AreaUpdates {
                name: None,
                atlas_id: Some(atlas_id),
            },
            auth_generation,
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

    /// Area ids served by an *ephemeral* (in-memory, session-lifetime) tier.
    /// Ephemeral areas are never persisted or synced, are excluded from the
    /// editor's atlas tree and per-area preference writes, and their room
    /// growth is capped by the mapper.
    fn ephemeral_area_ids(&self) -> std::collections::HashSet<AreaId> {
        std::collections::HashSet::new()
    }
}
