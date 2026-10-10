use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CloudResult, CreateAreaRequest, MapStorage, SourceId, SyncRow,
    cloud_api::{SecretChange, SecretGrant, SecretSummary},
    mutation::{MoveRequest, MoveResult, MutationEnvelope, MutationResult},
};
use async_trait::async_trait;
use uuid::Uuid;

pub(crate) mod area_edits;
pub mod area_merge;
pub mod cached;
pub mod cloud;
mod cloud_changes;
pub mod composite;
pub mod ephemeral;
pub mod local;
pub mod local_migration;
pub(crate) mod local_privacy;
pub(crate) mod source_document;

pub use area_merge::{
    AreaMergeCommit, AreaMergeOutcome, AreaMergePlan, AreaMergeSource, RoomRemap, Translate,
    apply_area_merge,
};
pub use cached::{CachedBackend, CachedCloudMapper};
pub use cloud::{CloudMapper, Credential, CredentialSource};
pub use composite::CompositeBackend;
pub use ephemeral::EphemeralBackend;
pub use local::LocalBackend;

/// The refusal every backend without Secrets gives.
fn secrets_unsupported() -> CloudError {
    CloudError::InvalidInput("only cloud maps have Secrets".to_string())
}

/// Core trait defining all mapping operations
#[async_trait]
pub trait MapperBackend: Send + Sync {
    /// The explicit local tier, including ids removed from its current snapshot.
    fn local_backend(&self) -> Option<&dyn MapperBackend> {
        None
    }

    /// Execute a queued local write without inferring its tier from live membership.
    async fn execute_local_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        if let Some(local) = self.local_backend() {
            local.execute_mutation(area_id, envelope).await
        } else if !self.supports_sync() {
            self.execute_mutation(area_id, envelope).await
        } else {
            Err(CloudError::AreaNotFound(*area_id))
        }
    }

    /// Subscribe after resolving identity. Hints contain no data and only
    /// request a credential-bound reconciliation by the receiving session.
    fn cloud_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        None
    }

    /// The current committed local generation, if this backend owns a local store.
    fn local_snapshot(&self) -> Option<std::sync::Arc<local::LocalSnapshot>> {
        None
    }

    /// Initialize the local store and subscribe to coalesced generation changes.
    /// Subscribe before loading a snapshot to avoid missing a concurrent commit.
    async fn subscribe_local(&self) -> CloudResult<Option<tokio::sync::watch::Receiver<u64>>> {
        Ok(None)
    }

    /// Subscribe to exact address changes before loading the local projection.
    /// Decorators opt into store-wide remaps by forwarding both this method
    /// and `execute_local_mutation_with_room_remap`. Without this capability,
    /// Mapper still publishes issuer-only remaps after successful commits.
    async fn subscribe_local_room_remaps(
        &self,
    ) -> CloudResult<Option<std::sync::Arc<local::LocalRoomRemapQueue>>> {
        Ok(None)
    }

    /// Client-side address metadata joins the local redo decision; it never
    /// enters the HTTP mutation contract.
    async fn execute_local_mutation_with_room_remap(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
        _remap: Option<local::LocalRoomRemap>,
    ) -> CloudResult<MutationResult> {
        // Decorators may gate, authorize or instrument this entry point.
        // Backends supporting durable remap metadata opt in explicitly.
        self.execute_local_mutation(area_id, envelope).await
    }

    /// Explicitly reload local files modified outside this process.
    async fn refresh_local(&self) -> CloudResult<()> {
        Ok(())
    }

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

    /// One row per viewable area: its projection token and source revisions.
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

    /// Where a map created with no folder and no storage named lives.
    fn default_storage(&self) -> MapStorage {
        MapStorage::Cloud
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

    /// Review a personal cloud map's sharing for a move to local storage.
    async fn review_local_move(
        &self,
        _area_id: &AreaId,
        _auth_generation: u64,
    ) -> CloudResult<crate::relocation::LocalMoveReview> {
        Err(CloudError::InvalidInput(
            "this backend does not support guarded cloud-to-local moves".into(),
        ))
    }

    /// Delete only the exact cloud snapshot copied and reviewed. Backends must
    /// opt in: falling back to ordinary DELETE can destroy uncopied content.
    async fn finish_local_move(
        &self,
        _area_id: &AreaId,
        _guard: &crate::relocation::LocalMoveGuard,
        _auth_generation: u64,
    ) -> CloudResult<()> {
        Err(CloudError::InvalidInput(
            "this backend does not support guarded cloud-to-local moves".into(),
        ))
    }

    /// Delete an area only while its authoritative revision still equals
    /// `expected_rev` (`None` = unconditional, exactly [`Self::delete_area`]).
    /// A mismatch is [`CloudError::RevisionConflict`] carrying the current
    /// revision, and the area survives.
    ///
    /// The default IGNORES the precondition and deletes unconditionally —
    /// the behavior of any backend (or remote server) that predates the
    /// feature. Callers MUST therefore keep their own compare-then-delete
    /// check as the enforcement floor and treat this as belt-and-braces:
    /// only a backend that actually implements the precondition (the cloud
    /// backend against a smudgy-web release with the expected-rev DELETE)
    /// closes the remaining TOCTOU window.
    async fn delete_area_expecting(
        &self,
        area_id: &AreaId,
        expected_rev: Option<i64>,
    ) -> CloudResult<()> {
        let _ = expected_rev;
        self.delete_area(area_id).await
    }

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

    /// [`Self::delete_area_expecting`] bound to a captured credential
    /// generation; same default-ignores-the-precondition caveat.
    async fn delete_area_expecting_at_generation(
        &self,
        area_id: &AreaId,
        expected_rev: Option<i64>,
        auth_generation: u64,
    ) -> CloudResult<()> {
        if self.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        self.delete_area_expecting(area_id, expected_rev).await
    }

    // ===== VERSIONED MUTATIONS (the CAS envelope) =====

    /// Applies one mutation envelope to an area atomically, honoring its
    /// precondition (the written source's revision) and its idempotent
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

    // ===== MULTI-AREA TRANSACTIONS =====

    /// Folds the plan's sources into its destination as one transaction:
    /// every document the plan names is read at its expected revision,
    /// [`apply_area_merge`] runs over those authoritative copies, and then
    /// either every post-image lands and every source is removed, or
    /// nothing changes. The commit carries the applier's outcome and the
    /// written documents, so the caller republishes without a second read.
    /// A revision that moved since the plan was built is
    /// [`CloudError::RevisionConflict`] and nothing is written.
    ///
    /// The default REFUSES with [`CloudError::StructuralConflict`]
    /// `merge_areas_unsupported_storage` — the behavior of any backend that
    /// does not own full documents it can rewrite together. The local and
    /// ephemeral tiers implement it; the cloud tier keeps the default until
    /// the server applies the same function. Every id in `plan.expected`
    /// must belong to one tier: a multi-tier backend refuses a plan that
    /// straddles tiers with `merge_areas_mixed_tiers` instead of forwarding
    /// it to any of them.
    async fn merge_areas(&self, plan: &AreaMergePlan) -> CloudResult<AreaMergeCommit> {
        let _ = plan;
        Err(CloudError::StructuralConflict(
            "merge_areas_unsupported_storage".to_string(),
        ))
    }

    // ===== SECRETS AND MOVES =====
    //
    // A Secret is a named source of a cloud map. Only the cloud tier keeps
    // them; every other backend inherits the refusals. Each call is bound to
    // the credential generation the caller captured, like the mutation path.

    /// `POST /areas/{id}/secrets`: creates an owner Secret on a map, drawn in
    /// `color` (`#rrggbb`) or, with `None`, in the palette's color.
    async fn create_secret(
        &self,
        area_id: &AreaId,
        name: &str,
        color: Option<&str>,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        self.create_secret_as(
            area_id,
            &crate::clan_secrets::NewSecret::owner(name, color),
            auth_generation,
        )
        .await
    }

    /// `POST /areas/{id}/secrets`: creates a Secret owned as `secret` says:
    /// an owner Secret, or a Member-owned or Clan-owned Clan Secret.
    async fn create_secret_as(
        &self,
        area_id: &AreaId,
        secret: &crate::clan_secrets::NewSecret,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let _ = (area_id, secret, auth_generation);
        Err(secrets_unsupported())
    }

    /// `PATCH /secrets/{id}`: renames and recolors one of a map's Secrets
    /// in one request.
    async fn update_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        change: &SecretChange,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let _ = (area_id, secret, change, auth_generation);
        Err(secrets_unsupported())
    }

    /// `PATCH /secrets/{id}`: renames one of a map's Secrets.
    async fn rename_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        name: &str,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let _ = (area_id, secret, name, auth_generation);
        Err(secrets_unsupported())
    }

    /// `PATCH /secrets/{id}` with `{color}`: sets a Secret's color
    /// (`#rrggbb`), or with `None` leaves it to the palette.
    async fn recolor_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        color: Option<&str>,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let _ = (area_id, secret, color, auth_generation);
        Err(secrets_unsupported())
    }

    /// `DELETE /secrets/{id}`: deletes one of a map's Secrets and everything
    /// it holds.
    async fn delete_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let _ = (area_id, secret, auth_generation);
        Err(secrets_unsupported())
    }

    /// `GET /secrets/{id}/grants`: the Secret's grants the caller may see.
    async fn secret_grants(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        auth_generation: u64,
    ) -> CloudResult<Vec<SecretGrant>> {
        let _ = (area_id, secret, auth_generation);
        Err(secrets_unsupported())
    }

    /// `POST /secrets/{id}/grants`: shares a Secret with a friend, replacing
    /// the caller's earlier grant to them.
    async fn grant_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        grantee_id: Uuid,
        actions: &[&str],
        auth_generation: u64,
    ) -> CloudResult<SecretGrant> {
        let _ = (area_id, secret, grantee_id, actions, auth_generation);
        Err(secrets_unsupported())
    }

    /// `PATCH /secrets/{id}/grants/{grant}`: replaces a grant's actions.
    async fn update_secret_grant(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        grant_id: Uuid,
        actions: &[&str],
        auth_generation: u64,
    ) -> CloudResult<SecretGrant> {
        let _ = (area_id, secret, grant_id, actions, auth_generation);
        Err(secrets_unsupported())
    }

    /// `DELETE /secrets/{id}/grants/{grant}`: revokes a grant.
    async fn revoke_secret_grant(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        grant_id: Uuid,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let _ = (area_id, secret, grant_id, auth_generation);
        Err(secrets_unsupported())
    }

    /// `POST /areas/{id}/moves`: moves rooms, labels and shapes between two
    /// sources of one map as one transaction.
    async fn move_content(
        &self,
        area_id: &AreaId,
        request: &MoveRequest,
        auth_generation: u64,
    ) -> CloudResult<MoveResult> {
        let _ = (area_id, request, auth_generation);
        Err(secrets_unsupported())
    }

    /// Reviews the exact transfer request, without moving its content.
    async fn review_move_content(
        &self,
        area_id: &AreaId,
        request: &MoveRequest,
        auth_generation: u64,
    ) -> CloudResult<crate::access_review::AccessReview> {
        let _ = (area_id, request, auth_generation);
        Err(secrets_unsupported())
    }

    async fn review_filing(
        &self,
        area_id: &AreaId,
        atlas_id: Option<AtlasId>,
        auth_generation: u64,
    ) -> CloudResult<crate::access_review::AccessReview> {
        let _ = (area_id, atlas_id, auth_generation);
        Err(secrets_unsupported())
    }

    async fn commit_reviewed_filing(
        &self,
        area_id: &AreaId,
        atlas_id: Option<AtlasId>,
        token: &str,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let _ = (area_id, atlas_id, token, auth_generation);
        Err(secrets_unsupported())
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

    /// List the atlases in one durable `storage`, failing when that storage
    /// can't be read rather than leaving it out. Default: a single tier's
    /// list, split the way [`Self::local_atlas_ids`] splits it.
    async fn list_atlases_in(&self, storage: MapStorage) -> CloudResult<Vec<AtlasListItem>> {
        if storage == MapStorage::Session {
            return Ok(Vec::new());
        }
        let atlases = self.list_atlases().await?;
        let local = self.local_atlas_ids();
        Ok(atlases
            .into_iter()
            .filter(|atlas| local.contains(&atlas.id) == (storage == MapStorage::Local))
            .collect())
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

    /// Finish moving a personal cloud atlas only if it is still empty.
    async fn finish_local_atlas_move(
        &self,
        _atlas_id: &AtlasId,
        _auth_generation: u64,
    ) -> CloudResult<()> {
        Err(CloudError::InvalidInput(
            "this backend does not support guarded cloud-to-local atlas moves".into(),
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
