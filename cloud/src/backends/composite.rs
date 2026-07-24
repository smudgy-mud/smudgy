//! Tiered fan-out backend: an ephemeral store and a local store alongside a
//! cloud backend.
//!
//! A session's mapper owns one [`CompositeBackend`] that presents the tiers
//! as one set of areas/atlases. Each area and atlas belongs to exactly one
//! tier; the composite routes every operation by membership:
//!
//! - **ephemeral** areas live in memory for the session only (auto-mapping's
//!   landing zone; the composite owns this tier — it needs no configuration);
//! - **local** areas/atlases live on disk forever (available signed out);
//! - **cloud** areas/atlases sync through the existing cached cloud backend.
//!
//! The local membership set is refreshed on every `list_*` and sync-row
//! synthesis, and updated incrementally on create/delete; the ephemeral set
//! only ever changes through this backend's own create/delete, so it needs no
//! refresh. Atlases (folders) exist only in the local and cloud tiers.
//!
//! ## Sync safety
//!
//! The mapper's sync engine prunes any cached area whose id is absent from the
//! `/sync` row set (that is how a revoked share, or a previous account's
//! areas, leave the tree). Local and ephemeral areas never appear in the
//! cloud's `/sync`, so [`sync_state`](CompositeBackend::sync_state)
//! **synthesizes a stable row per local and ephemeral area** and folds them
//! into the cloud rows — otherwise every sync tick would wipe those tiers.
//! Owner-fingerprinted and carrying the area's real rev, those rows stay quiet
//! across ticks (no needless refetch) yet still let an edit's rev bump flow
//! through the same reconciliation path.

use std::{collections::HashSet, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::sync::OnceCell;
use uuid::Uuid;

use super::{EphemeralBackend, LEGACY_ACCESS_FINGERPRINT, MapperBackend};
use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CloudResult, CreateAreaRequest, SyncRow,
    mutation::{MutationEnvelope, MutationResult},
};

type DynBackend = Arc<dyn MapperBackend + Send + Sync>;

/// Fans area/atlas operations across an ephemeral, a local, and a cloud tier.
pub struct CompositeBackend {
    ephemeral: DynBackend,
    local: DynBackend,
    cloud: DynBackend,
    /// Ids the ephemeral tier owns; only this backend's create/delete touch it.
    ephemeral_areas: RwLock<HashSet<AreaId>>,
    /// Ids the local tier owns, refreshed on every list / sync synthesis.
    local_areas: RwLock<HashSet<AreaId>>,
    local_atlases: RwLock<HashSet<AtlasId>>,
    /// Guards a one-time seed of the routing sets, so a cold direct
    /// `get_area`/atlas op (before any `list_*`/`sync_state`) still routes to
    /// the right tier. In the normal flow `load_all_areas` seeds them first;
    /// this is belt-and-suspenders against reordering.
    routing_seeded: OnceCell<()>,
}

impl std::fmt::Debug for CompositeBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CompositeBackend").finish_non_exhaustive()
    }
}

impl CompositeBackend {
    /// Combines a `local` tier (always available) with a `cloud` tier
    /// (available when signed in), plus an internally-owned ephemeral tier
    /// (in-memory, session-lifetime — it takes no configuration, so callers
    /// never construct it).
    #[must_use]
    pub fn new(local: DynBackend, cloud: DynBackend) -> Self {
        Self {
            ephemeral: Arc::new(EphemeralBackend::new()),
            local,
            cloud,
            ephemeral_areas: RwLock::new(HashSet::new()),
            local_areas: RwLock::new(HashSet::new()),
            local_atlases: RwLock::new(HashSet::new()),
            routing_seeded: OnceCell::new(),
        }
    }

    fn is_ephemeral_area(&self, area_id: AreaId) -> bool {
        self.ephemeral_areas.read().contains(&area_id)
    }

    fn is_local_area(&self, area_id: AreaId) -> bool {
        self.local_areas.read().contains(&area_id)
    }

    fn is_local_atlas(&self, atlas_id: AtlasId) -> bool {
        self.local_atlases.read().contains(&atlas_id)
    }

    /// Seeds the routing sets from the local tier once, so the first routing
    /// decision is correct even if it precedes any `list_*`/`sync_state`.
    /// Later `list_*`/`sync_state` calls keep refreshing them.
    async fn ensure_routing_seeded(&self) {
        self.routing_seeded
            .get_or_init(|| async {
                if let Ok(areas) = self.local.list_areas().await {
                    self.refresh_local_areas(&areas);
                }
                if let Ok(atlases) = self.local.list_atlases().await {
                    *self.local_atlases.write() = atlases.iter().map(|atlas| atlas.id).collect();
                }
            })
            .await;
    }

    /// The tier that owns `area_id`, seeding the routing sets first so an
    /// unknown id isn't wrongly sent to cloud on a cold start. Cloud remains
    /// the fallback for ids genuinely not in the ephemeral or local tiers.
    async fn area_backend(&self, area_id: AreaId) -> &DynBackend {
        self.ensure_routing_seeded().await;
        if self.is_ephemeral_area(area_id) {
            &self.ephemeral
        } else if self.is_local_area(area_id) {
            &self.local
        } else {
            &self.cloud
        }
    }

    /// Folds the listed local ids into the routing set without clobbering an
    /// id a concurrent `create_area` just inserted (a wholesale replace could
    /// race that insert away). Local ids only leave the set through this
    /// backend's own `delete_area`, so a union never leaks a stale id.
    fn refresh_local_areas(&self, areas: &[Area]) {
        self.local_areas
            .write()
            .extend(areas.iter().map(|area| area.id));
    }

    /// Synthesizes a sync row per local and ephemeral area from each tier's
    /// metadata, so the sync engine never prunes either tier.
    async fn non_cloud_sync_rows(&self) -> CloudResult<Vec<SyncRow>> {
        let areas = self.local.list_areas().await?;
        self.refresh_local_areas(&areas);
        let mut rows: Vec<SyncRow> = areas.iter().map(synthesized_row).collect();
        rows.extend(
            self.ephemeral
                .list_areas()
                .await?
                .iter()
                .map(synthesized_row),
        );
        Ok(rows)
    }
}

/// A stable owner-fingerprinted sync row for an area no `/sync` covers.
fn synthesized_row(area: &Area) -> SyncRow {
    SyncRow {
        area_id: area.id,
        rev: area.rev,
        access_fingerprint: area.access.map_or_else(
            || LEGACY_ACCESS_FINGERPRINT.to_string(),
            |access| access.fingerprint(),
        ),
    }
}

#[async_trait]
impl MapperBackend for CompositeBackend {
    // ===== AREA OPERATIONS =====

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        // An explicit ephemeral request routes to the in-memory tier
        // regardless of sign-in state or folders (ephemeral areas are loose).
        if request.ephemeral {
            let area = self.ephemeral.create_area(request).await?;
            self.ephemeral_areas.write().insert(area.id);
            return Ok(area);
        }
        // Otherwise route by the target atlas; a loose area follows the active
        // tier (cloud when signed in, local otherwise — the only option signed
        // out).
        self.ensure_routing_seeded().await;
        let go_local = match request.atlas_id {
            Some(atlas_id) => self.is_local_atlas(atlas_id),
            None => !self.cloud.has_credential(),
        };
        let area = if go_local {
            self.local.create_area(request).await?
        } else {
            self.cloud.create_area(request).await?
        };
        if go_local {
            self.local_areas.write().insert(area.id);
        }
        Ok(area)
    }

    async fn import_local_area(&self, details: AreaWithDetails) -> CloudResult<()> {
        // Import is local-only: persist to the local tier and register the id so later ops
        // (get_area/export/sync-row synthesis) route to local.
        let area_id = details.area.id;
        self.local.import_local_area(details).await?;
        self.local_areas.write().insert(area_id);
        Ok(())
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        // Ephemeral and local always; cloud only when signed in. A cloud
        // failure must not sink the other tiers — the sync engine surfaces
        // cloud connectivity.
        let mut all = self.ephemeral.list_areas().await?;
        let local = self.local.list_areas().await?;
        self.refresh_local_areas(&local);
        all.extend(local);
        if self.cloud.has_credential() {
            match self.cloud.list_areas().await {
                Ok(cloud) => all.extend(cloud),
                Err(err) => log::warn!("composite: cloud list_areas failed: {err}"),
            }
        }
        Ok(all)
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.area_backend(*area_id).await.get_area(area_id).await
    }

    fn last_area_source(&self, area_id: &AreaId) -> AreaLoadSource {
        // Sync method: route on the current sets without seeding (a best-effort
        // load-source hint; cloud is a fine fallback for an unseeded id).
        if self.is_ephemeral_area(*area_id) {
            self.ephemeral.last_area_source(area_id)
        } else if self.is_local_area(*area_id) {
            self.local.last_area_source(area_id)
        } else {
            self.cloud.last_area_source(area_id)
        }
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.area_backend(*area_id)
            .await
            .update_area(area_id, updates)
            .await
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        let result = self.area_backend(*area_id).await.delete_area(area_id).await;
        self.ephemeral_areas.write().remove(area_id);
        self.local_areas.write().remove(area_id);
        result
    }

    // ===== VERSIONED MUTATIONS =====

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        // Route to the owning tier with the envelope untouched: preconditions
        // are the addressed backend's to judge, never this layer's.
        self.area_backend(*area_id)
            .await
            .execute_mutation(area_id, envelope)
            .await
    }

    async fn move_area_to_atlas(
        &self,
        area_id: &AreaId,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<()> {
        self.ensure_routing_seeded().await;
        // Cross-tier moves are a data migration, not a metadata update —
        // reject them so a foreign atlas id never reaches the wrong backend. (`None`, pulling an area loose, is valid in either
        // tier.) The UI also filters cross-tier targets out of the picker; this
        // is the load-bearing backstop. The ephemeral tier has no folders at
        // all, so any filing of a session map is cross-tier by definition.
        if let Some(target) = atlas_id {
            if self.is_ephemeral_area(*area_id) {
                return Err(CloudError::InvalidInput(
                    "session maps can't be filed into folders — save the map first".to_string(),
                ));
            }
            if self.is_local_area(*area_id) != self.is_local_atlas(target) {
                return Err(CloudError::InvalidInput(
                    "moving a map between the local and cloud tiers isn't supported".to_string(),
                ));
            }
        }
        self.area_backend(*area_id)
            .await
            .move_area_to_atlas(area_id, atlas_id)
            .await
    }

    // ===== ATLAS (FOLDER) OPERATIONS =====

    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        let local = self.local.list_atlases().await?;
        // Union (not wholesale replace) for the same reason as
        // `refresh_local_areas`: don't race a concurrent `create_atlas` away.
        self.local_atlases
            .write()
            .extend(local.iter().map(|atlas| atlas.id));
        let mut all = local;
        if self.cloud.has_credential() {
            match self.cloud.list_atlases().await {
                Ok(cloud) => all.extend(cloud),
                Err(err) => log::warn!("composite: cloud list_atlases failed: {err}"),
            }
        }
        Ok(all)
    }

    async fn create_atlas(&self, name: &str) -> CloudResult<Atlas> {
        // No explicit hint: signed out => local (only option); signed in =>
        // cloud, the default tier.
        self.create_atlas_in(name, false).await
    }

    async fn create_atlas_in(&self, name: &str, prefer_local: bool) -> CloudResult<Atlas> {
        // Honor the choice; fall back to local only when cloud is unavailable
        // (signed out), the one case where a cloud folder can't be created.
        let go_local = prefer_local || !self.cloud.has_credential();
        let atlas = if go_local {
            self.local.create_atlas(name).await?
        } else {
            self.cloud.create_atlas(name).await?
        };
        if go_local {
            self.local_atlases.write().insert(atlas.id);
        }
        Ok(atlas)
    }

    async fn rename_atlas(&self, atlas_id: &AtlasId, name: &str) -> CloudResult<Atlas> {
        self.ensure_routing_seeded().await;
        if self.is_local_atlas(*atlas_id) {
            self.local.rename_atlas(atlas_id, name).await
        } else {
            self.cloud.rename_atlas(atlas_id, name).await
        }
    }

    async fn delete_atlas(&self, atlas_id: &AtlasId) -> CloudResult<()> {
        self.ensure_routing_seeded().await;
        let result = if self.is_local_atlas(*atlas_id) {
            self.local.delete_atlas(atlas_id).await
        } else {
            self.cloud.delete_atlas(atlas_id).await
        };
        self.local_atlases.write().remove(atlas_id);
        result
    }

    // ===== SYNC / IDENTITY =====

    fn supports_sync(&self) -> bool {
        // The cloud tier drives /sync; the local rows ride along (see module
        // docs) so the engine never prunes them.
        self.cloud.supports_sync()
    }

    fn local_atlas_ids(&self) -> HashSet<AtlasId> {
        self.local_atlases.read().clone()
    }

    fn local_area_ids(&self) -> HashSet<AreaId> {
        self.local_areas.read().clone()
    }

    fn ephemeral_area_ids(&self) -> HashSet<AreaId> {
        self.ephemeral_areas.read().clone()
    }

    async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
        let local_rows = self.non_cloud_sync_rows().await?;
        // Without a cloud credential, reconcile against the local tier only and
        // never touch (or even log an attempt at) `/sync`: the startup sync and
        // every `sync_now` are no-ops on the cloud tier until the user signs in.
        if !self.cloud.has_credential() {
            return Ok(Some(local_rows));
        }
        match self.cloud.sync_state().await {
            Ok(Some(mut rows)) => {
                rows.extend(local_rows);
                Ok(Some(rows))
            }
            // Legacy / no-`/sync` cloud (`Ok(None)`, or a uniform 404 on
            // `/sync`): synthesize the cloud rows here, propagating a cloud
            // `list_areas` failure as a hard error so the engine backs off
            // rather than reconciling against a row set that silently dropped
            // the whole cloud tier (which would prune every cloud area).
            Ok(None) | Err(CloudError::NotFoundOrNoAccess) if self.cloud.has_credential() => {
                let cloud_areas = self.cloud.list_areas().await?;
                let mut rows: Vec<SyncRow> = cloud_areas
                    .into_iter()
                    .map(|area| SyncRow {
                        area_id: area.id,
                        rev: area.rev,
                        access_fingerprint: area.access.map_or_else(
                            || LEGACY_ACCESS_FINGERPRINT.to_string(),
                            |access| access.fingerprint(),
                        ),
                    })
                    .collect();
                rows.extend(local_rows);
                Ok(Some(rows))
            }
            // Signed out: reconcile against local rows only, so the engine
            // keeps the local tier and stays Idle instead of LoggedOut.
            Ok(None) | Err(CloudError::Unauthorized(_)) => Ok(Some(local_rows)),
            // Email-unverified / transport errors: surface to the engine,
            // which falls back to `list_areas` (incl. local) or backs off.
            // Neither path prunes the cache.
            Err(err) => Err(err),
        }
    }

    async fn note_sync_rows(&self, rows: &[SyncRow]) {
        // Only the cloud tier caches `get_area` bytes; forward its rows alone
        // (local/ephemeral ids would just be inert extras in the cloud's
        // known set).
        let cloud_rows: Vec<SyncRow> = rows
            .iter()
            .filter(|row| !self.is_local_area(row.area_id) && !self.is_ephemeral_area(row.area_id))
            .cloned()
            .collect();
        self.cloud.note_sync_rows(&cloud_rows).await;
    }

    async fn purge_area(&self, area_id: &AreaId) {
        self.area_backend(*area_id).await.purge_area(area_id).await;
    }

    async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
        // No cloud credential => no identity to resolve; the local tier needs
        // none. (Skipping the /me call keeps the engine off the LoggedOut
        // path while signed out.)
        if self.cloud.has_credential() {
            self.cloud.viewer_identity().await
        } else {
            Ok(None)
        }
    }

    fn auth_generation(&self) -> u64 {
        // Credential changes are the cloud tier's; a bump triggers the
        // engine's re-resolve + full resync (which prunes the prior account's
        // cloud areas while leaving the local tier untouched).
        self.cloud.auth_generation()
    }

    fn has_credential(&self) -> bool {
        // The local tier is always serviceable, so the composite always "has
        // a credential": callers (session load, atlas refetch) should attempt
        // it rather than skip. Cloud-specific gating happens per-call.
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AreaAccess, CloudError, CreateAreaRequest, RoomUpdates, backends::LocalBackend};
    use chrono::Utc;
    use parking_lot::Mutex;
    use std::path::PathBuf;

    /// Minimal cloud-tier double: scriptable `list_areas`/`get_area`/`sync_state`
    /// and a fixed credential flag. Methods the composite tests don't exercise
    /// are inert.
    struct StubCloud {
        areas: Mutex<Vec<Area>>,
        details: Mutex<std::collections::HashMap<AreaId, AreaWithDetails>>,
        sync: Mutex<CloudResult<Option<Vec<SyncRow>>>>,
        has_cred: bool,
    }

    impl StubCloud {
        fn new(has_cred: bool) -> Self {
            Self {
                areas: Mutex::new(Vec::new()),
                details: Mutex::new(std::collections::HashMap::new()),
                sync: Mutex::new(Ok(Some(Vec::new()))),
                has_cred,
            }
        }

        fn add_area(&self, area: AreaWithDetails) {
            self.areas.lock().push(area.area.clone());
            self.details.lock().insert(area.area.id, area);
        }

        fn set_sync(&self, rows: CloudResult<Option<Vec<SyncRow>>>) {
            *self.sync.lock() = rows;
        }
    }

    #[async_trait]
    impl MapperBackend for StubCloud {
        async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
            let area = owned_area(AreaId(Uuid::new_v4()), &request.name, 1).area;
            self.areas.lock().push(area.clone());
            Ok(area)
        }
        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            Ok(self.areas.lock().clone())
        }
        async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            self.details
                .lock()
                .get(area_id)
                .cloned()
                .ok_or(CloudError::NotFoundOrNoAccess)
        }
        async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
            self.sync.lock().clone()
        }
        fn supports_sync(&self) -> bool {
            true
        }
        fn has_credential(&self) -> bool {
            self.has_cred
        }
        async fn update_area(&self, _: &AreaId, _: AreaUpdates) -> CloudResult<()> {
            Ok(())
        }
        async fn delete_area(&self, _: &AreaId) -> CloudResult<()> {
            Ok(())
        }
        // The stub never accepts envelopes, so a routing mistake (a local
        // area's mutation reaching the cloud tier) fails the test loudly.
        async fn execute_mutation(
            &self,
            _: &AreaId,
            _: &MutationEnvelope,
        ) -> CloudResult<MutationResult> {
            Err(CloudError::NotFoundOrNoAccess)
        }
    }

    fn owned_area(id: AreaId, name: &str, rev: i64) -> AreaWithDetails {
        AreaWithDetails {
            area: Area {
                id,
                user_id: None,
                atlas_id: None,
                atlas_name: None,
                name: name.to_string(),
                created_at: Utc::now(),
                rev,
                access: Some(AreaAccess::OWNER),
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
            },
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
            properties: Vec::new(),
            rooms: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
            linked_areas: Vec::new(),
        }
    }

    fn temp_root() -> PathBuf {
        std::env::temp_dir().join(format!("smudgy-composite-test-{}", Uuid::new_v4()))
    }

    async fn local_with_one_area(root: &PathBuf) -> (Arc<LocalBackend>, AreaId) {
        let local = Arc::new(LocalBackend::new(root));
        let area = local
            .create_area(CreateAreaRequest {
                name: "Local".to_string(),
                atlas_id: None,
                ephemeral: false,
            })
            .await
            .expect("local create");
        (local, area.id)
    }

    #[tokio::test]
    async fn list_areas_merges_both_tiers_and_get_area_routes() {
        let root = temp_root();
        let (local, local_id) = local_with_one_area(&root).await;

        let cloud = Arc::new(StubCloud::new(true));
        let cloud_id = AreaId(Uuid::new_v4());
        cloud.add_area(owned_area(cloud_id, "Cloud", 3));

        let composite = CompositeBackend::new(local, cloud);

        let listed = composite.list_areas().await.expect("list"); // also seeds routing
        let ids: HashSet<AreaId> = listed.iter().map(|a| a.id).collect();
        assert!(ids.contains(&local_id) && ids.contains(&cloud_id));

        assert_eq!(
            composite.get_area(&local_id).await.unwrap().area.name,
            "Local",
            "local id routes to the local tier"
        );
        assert_eq!(
            composite.get_area(&cloud_id).await.unwrap().area.name,
            "Cloud",
            "cloud id routes to the cloud tier"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// The headline sync-safety property: a synthesized local row joins the
    /// cloud rows, so the sync engine never prunes the local tier.
    #[tokio::test]
    async fn sync_state_folds_in_local_rows() {
        let root = temp_root();
        let (local, local_id) = local_with_one_area(&root).await;

        let cloud = Arc::new(StubCloud::new(true));
        let cloud_id = AreaId(Uuid::new_v4());
        cloud.set_sync(Ok(Some(vec![SyncRow {
            area_id: cloud_id,
            rev: 9,
            access_fingerprint: "fp".to_string(),
        }])));

        let composite = CompositeBackend::new(local, cloud);
        let rows = composite.sync_state().await.unwrap().expect("rows");
        let ids: HashSet<AreaId> = rows.iter().map(|r| r.area_id).collect();
        assert!(
            ids.contains(&cloud_id) && ids.contains(&local_id),
            "sync rows must cover both the cloud row and the synthesized local row"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Cross-tier moves (local area → cloud folder, or vice-versa) are
    /// rejected so a foreign atlas id never reaches the wrong backend; a
    /// same-tier move (here, pulling a local area loose) is allowed.
    #[tokio::test]
    async fn cross_tier_move_is_rejected_same_tier_is_allowed() {
        let root = temp_root();
        let (local, local_area_id) = local_with_one_area(&root).await;
        let cloud = Arc::new(StubCloud::new(true));
        let composite = CompositeBackend::new(local, cloud);

        // A cloud atlas id (never created locally) belongs to the cloud tier.
        let cloud_atlas = AtlasId(Uuid::new_v4());
        let cross = composite
            .move_area_to_atlas(&local_area_id, Some(cloud_atlas))
            .await;
        assert!(
            matches!(cross, Err(CloudError::InvalidInput(_))),
            "moving a local area into a cloud folder must be rejected, got {cross:?}"
        );

        // Pulling the local area loose stays within the local tier.
        assert!(
            composite
                .move_area_to_atlas(&local_area_id, None)
                .await
                .is_ok()
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// `create_atlas_in(.., true)` lands the folder in the local tier even
    /// when signed in (the explicit "save on this device" choice).
    #[tokio::test]
    async fn create_atlas_in_honors_an_explicit_local_choice() {
        let root = temp_root();
        let local = Arc::new(LocalBackend::new(&root));
        let cloud = Arc::new(StubCloud::new(true)); // signed in
        let composite = CompositeBackend::new(local, cloud);

        let atlas = composite
            .create_atlas_in("On device", true)
            .await
            .expect("local atlas");
        assert!(
            composite.local_atlas_ids().contains(&atlas.id),
            "an explicit local choice must route to the local tier"
        );

        std::fs::remove_dir_all(&root).ok();
    }

    /// Signed out, even a cloud-preferring create falls back to local (the
    /// only tier that can serve a folder without a credential).
    #[tokio::test]
    async fn create_atlas_falls_back_to_local_when_signed_out() {
        let root = temp_root();
        let local = Arc::new(LocalBackend::new(&root));
        let cloud = Arc::new(StubCloud::new(false)); // signed out
        let composite = CompositeBackend::new(local, cloud);

        let atlas = composite
            .create_atlas_in("Folder", false)
            .await
            .expect("atlas");
        assert!(composite.local_atlas_ids().contains(&atlas.id));

        std::fs::remove_dir_all(&root).ok();
    }

    /// The ephemeral tier: an `ephemeral: true` create routes to the
    /// in-memory tier no matter the sign-in state, its rows join `sync_state`
    /// (so the engine never prunes the session map), filing it into a folder
    /// is rejected, and delete drops it from the membership set.
    #[tokio::test]
    async fn ephemeral_create_routes_syncs_and_rejects_folders() {
        let root = temp_root();
        let local = Arc::new(LocalBackend::new(&root));
        let cloud = Arc::new(StubCloud::new(true)); // signed in — ephemeral must still win
        let composite = CompositeBackend::new(local, cloud);

        let area = composite
            .create_area(CreateAreaRequest {
                name: "Session map".to_string(),
                atlas_id: None,
                ephemeral: true,
            })
            .await
            .expect("ephemeral create");
        assert!(composite.ephemeral_area_ids().contains(&area.id));
        assert!(
            !composite.local_area_ids().contains(&area.id),
            "ephemeral areas are not local-tier areas"
        );

        // Routing: reads and writes reach the in-memory tier (the cloud stub
        // rejects every envelope, so success proves the routing).
        composite
            .execute_mutation(
                &area.id,
                &MutationEnvelope {
                    operation_id: Uuid::new_v4(),
                    preconditions: vec![crate::mutation::Precondition {
                        resource: crate::mutation::ResourceKind::Area,
                        id: area.id.0,
                        expected_rev: 1,
                        access_fingerprint: None,
                    }],
                    payload: vec![crate::mutation::AreaMutation::UpsertRoom {
                        room_number: crate::RoomNumber(1),
                        body: RoomUpdates {
                            title: Some("Gate".to_string()),
                            ..RoomUpdates::default()
                        },
                    }],
                },
            )
            .await
            .expect("room write routes to ephemeral");
        let details = composite.get_area(&area.id).await.expect("get");
        assert_eq!(details.rooms.len(), 1);

        // Sync safety: the synthesized row keeps the engine from pruning it.
        let rows = composite.sync_state().await.unwrap().expect("rows");
        assert!(rows.iter().any(|row| row.area_id == area.id));

        // No folders in the ephemeral tier.
        let filed = composite
            .move_area_to_atlas(&area.id, Some(AtlasId(Uuid::new_v4())))
            .await;
        assert!(matches!(filed, Err(CloudError::InvalidInput(_))));

        composite.delete_area(&area.id).await.expect("delete");
        assert!(!composite.ephemeral_area_ids().contains(&area.id));
        assert!(matches!(
            composite.get_area(&area.id).await,
            Err(CloudError::NotFoundOrNoAccess)
        ));

        std::fs::remove_dir_all(&root).ok();
    }

    /// A mutation envelope routes to the tier that owns the area, envelope
    /// unchanged: a local area's envelope lands on the local backend's CAS
    /// (the cloud stub rejects every envelope, so success proves routing).
    #[tokio::test]
    async fn execute_mutation_routes_to_the_owning_tier() {
        let root = temp_root();
        let (local, local_id) = local_with_one_area(&root).await;
        let cloud = Arc::new(StubCloud::new(true)); // signed in — routing must still pick local
        let composite = CompositeBackend::new(local, cloud);

        let envelope = MutationEnvelope {
            operation_id: Uuid::new_v4(),
            preconditions: vec![crate::mutation::Precondition {
                resource: crate::mutation::ResourceKind::Area,
                id: local_id.0,
                expected_rev: 1,
                access_fingerprint: None,
            }],
            payload: vec![crate::mutation::AreaMutation::UpsertRoom {
                room_number: crate::RoomNumber(1),
                body: RoomUpdates {
                    title: Some("Hall".to_string()),
                    ..RoomUpdates::default()
                },
            }],
        };
        let result = composite
            .execute_mutation(&local_id, &envelope)
            .await
            .expect("envelope reaches the local tier");
        assert_eq!(result.versions[0].rev, 2);

        let details = composite.get_area(&local_id).await.expect("get");
        assert_eq!(details.area.rev, 2);
        assert_eq!(details.rooms.len(), 1);
        assert_eq!(details.rooms[0].title, "Hall");

        std::fs::remove_dir_all(&root).ok();
    }

    /// Signed out, `sync_state` returns the local rows (not the cloud's auth
    /// error), so the engine stays Idle and keeps the local tier.
    #[tokio::test]
    async fn signed_out_sync_state_returns_local_rows_not_error() {
        let root = temp_root();
        let (local, local_id) = local_with_one_area(&root).await;

        let cloud = Arc::new(StubCloud::new(false));
        cloud.set_sync(Err(CloudError::Unauthorized("no credential".to_string())));

        let composite = CompositeBackend::new(local, cloud);
        assert!(composite.has_credential(), "local tier keeps the door open");
        let rows = composite
            .sync_state()
            .await
            .expect("must not surface the cloud auth error")
            .expect("some rows");
        let ids: HashSet<AreaId> = rows.iter().map(|r| r.area_id).collect();
        assert_eq!(ids.len(), 1);
        assert!(ids.contains(&local_id));

        std::fs::remove_dir_all(&root).ok();
    }
}
