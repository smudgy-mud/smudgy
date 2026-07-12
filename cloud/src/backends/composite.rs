//! Two-tier fan-out backend: a local store alongside a cloud backend.
//!
//! A session's mapper owns one [`CompositeBackend`] that presents both tiers
//! as one set of areas/atlases. Each area and atlas belongs to exactly one
//! tier; the composite routes every operation by membership:
//!
//! - **local** areas/atlases live on disk forever (available signed out);
//! - **cloud** areas/atlases sync through the existing cached cloud backend.
//!
//! The membership sets are refreshed on every `list_*` and sync-row
//! synthesis, and updated incrementally on create/delete.
//!
//! ## Sync safety
//!
//! The mapper's sync engine prunes any cached area whose id is absent from the
//! `/sync` row set (that is how a revoked share, or a previous account's
//! areas, leave the tree). Local areas never appear in the cloud's `/sync`, so
//! [`sync_state`](CompositeBackend::sync_state) **synthesizes a stable row per
//! local area** and folds it into the cloud rows — otherwise every sync tick
//! would wipe the local tier. Owner-fingerprinted and carrying the area's real
//! rev, those rows stay quiet across ticks (no needless refetch) yet still let
//! a local edit's rev bump flow through the same reconciliation path.

use std::{
    collections::HashSet,
    sync::Arc,
};

use async_trait::async_trait;
use parking_lot::RwLock;
use tokio::sync::OnceCell;
use uuid::Uuid;

use super::{LEGACY_ACCESS_FINGERPRINT, MapperBackend};
use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CreateAreaRequest, Exit, ExitArgs, ExitId, ExitUpdates, Label, LabelArgs, LabelId, LabelUpdates,
    CloudError, CloudResult, Room, RoomUpdates, Shape, ShapeArgs, ShapeId, ShapeUpdates, SyncRow,
    mapper::RoomKey,
};

type DynBackend = Arc<dyn MapperBackend + Send + Sync>;

/// Fans area/atlas operations across a local tier and a cloud tier.
pub struct CompositeBackend {
    local: DynBackend,
    cloud: DynBackend,
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
    /// (available when signed in).
    #[must_use]
    pub fn new(local: DynBackend, cloud: DynBackend) -> Self {
        Self {
            local,
            cloud,
            local_areas: RwLock::new(HashSet::new()),
            local_atlases: RwLock::new(HashSet::new()),
            routing_seeded: OnceCell::new(),
        }
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
    /// the fallback for ids genuinely not in the local tier.
    async fn area_backend(&self, area_id: AreaId) -> &DynBackend {
        self.ensure_routing_seeded().await;
        if self.is_local_area(area_id) {
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
        self.local_areas.write().extend(areas.iter().map(|area| area.id));
    }

    /// Synthesizes a sync row per local area from the local tier's metadata.
    async fn local_sync_rows(&self) -> CloudResult<Vec<SyncRow>> {
        let areas = self.local.list_areas().await?;
        self.refresh_local_areas(&areas);
        Ok(areas
            .into_iter()
            .map(|area| SyncRow {
                area_id: area.id,
                rev: area.rev,
                access_fingerprint: area.access.map_or_else(
                    || LEGACY_ACCESS_FINGERPRINT.to_string(),
                    |access| access.fingerprint(),
                ),
            })
            .collect())
    }
}

#[async_trait]
impl MapperBackend for CompositeBackend {
    // ===== AREA OPERATIONS =====

    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        // Route by the target atlas; a loose area follows the active tier
        // (cloud when signed in, local otherwise — the only option signed out).
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
        // Local always; cloud only when signed in. A cloud failure must not
        // sink the local tier — the sync engine surfaces cloud connectivity.
        let mut all = self.local.list_areas().await?;
        self.refresh_local_areas(&all);
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
        if self.is_local_area(*area_id) {
            self.local.last_area_source(area_id)
        } else {
            self.cloud.last_area_source(area_id)
        }
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.area_backend(*area_id).await.update_area(area_id, updates).await
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        let result = self.area_backend(*area_id).await.delete_area(area_id).await;
        self.local_areas.write().remove(area_id);
        result
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
        // is the load-bearing backstop.
        if let Some(target) = atlas_id
            && self.is_local_area(*area_id) != self.is_local_atlas(target)
        {
            return Err(CloudError::InvalidInput(
                "moving a map between the local and cloud tiers isn't supported".to_string(),
            ));
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
        self.local_atlases.write().extend(local.iter().map(|atlas| atlas.id));
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

    async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
        let local_rows = self.local_sync_rows().await?;
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
        // (local ids would just be inert extras in the cloud's known set).
        let cloud_rows: Vec<SyncRow> = rows
            .iter()
            .filter(|row| !self.is_local_area(row.area_id))
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

    // ===== AREA PROPERTIES =====

    async fn set_area_property(&self, area_id: &AreaId, name: &str, value: &str) -> CloudResult<()> {
        self.area_backend(*area_id)
            .await
            .set_area_property(area_id, name, value)
            .await
    }

    async fn delete_area_property(&self, area_id: &AreaId, name: &str) -> CloudResult<()> {
        self.area_backend(*area_id)
            .await
            .delete_area_property(area_id, name)
            .await
    }

    // ===== ROOM OPERATIONS =====

    async fn update_room(&self, room_key: &RoomKey, updates: RoomUpdates) -> CloudResult<Room> {
        self.area_backend(room_key.area_id)
            .await
            .update_room(room_key, updates)
            .await
    }

    async fn delete_room(&self, room_key: &RoomKey) -> CloudResult<()> {
        self.area_backend(room_key.area_id).await.delete_room(room_key).await
    }

    async fn set_room_property(
        &self,
        room_key: &RoomKey,
        name: &str,
        value: &str,
    ) -> CloudResult<()> {
        self.area_backend(room_key.area_id)
            .await
            .set_room_property(room_key, name, value)
            .await
    }

    async fn delete_room_property(&self, room_key: &RoomKey, name: &str) -> CloudResult<()> {
        self.area_backend(room_key.area_id)
            .await
            .delete_room_property(room_key, name)
            .await
    }

    // ===== ROOM TAGS =====

    async fn add_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()> {
        self.area_backend(room_key.area_id)
            .await
            .add_room_tag(room_key, tag)
            .await
    }

    async fn remove_room_tag(&self, room_key: &RoomKey, tag: &str) -> CloudResult<()> {
        self.area_backend(room_key.area_id)
            .await
            .remove_room_tag(room_key, tag)
            .await
    }

    // ===== EXIT OPERATIONS =====

    async fn create_room_exit(&self, room_key: &RoomKey, exit_data: ExitArgs) -> CloudResult<Exit> {
        self.area_backend(room_key.area_id)
            .await
            .create_room_exit(room_key, exit_data)
            .await
    }

    async fn update_exit(
        &self,
        area_id: &AreaId,
        exit_id: &ExitId,
        updates: ExitUpdates,
    ) -> CloudResult<()> {
        self.area_backend(*area_id)
            .await
            .update_exit(area_id, exit_id, updates)
            .await
    }

    async fn delete_exit(&self, area_id: &AreaId, exit_id: &ExitId) -> CloudResult<()> {
        self.area_backend(*area_id).await.delete_exit(area_id, exit_id).await
    }

    // ===== LABEL OPERATIONS =====

    async fn create_label(&self, area_id: &AreaId, label_data: LabelArgs) -> CloudResult<Label> {
        self.area_backend(*area_id)
            .await
            .create_label(area_id, label_data)
            .await
    }

    async fn update_label(
        &self,
        area_id: &AreaId,
        label_id: &LabelId,
        updates: LabelUpdates,
    ) -> CloudResult<()> {
        self.area_backend(*area_id)
            .await
            .update_label(area_id, label_id, updates)
            .await
    }

    async fn delete_label(&self, area_id: &AreaId, label_id: &LabelId) -> CloudResult<()> {
        self.area_backend(*area_id)
            .await
            .delete_label(area_id, label_id)
            .await
    }

    // ===== SHAPE OPERATIONS =====

    async fn create_shape(&self, area_id: &AreaId, shape_data: ShapeArgs) -> CloudResult<Shape> {
        self.area_backend(*area_id)
            .await
            .create_shape(area_id, shape_data)
            .await
    }

    async fn update_shape(
        &self,
        area_id: &AreaId,
        shape_id: &ShapeId,
        updates: ShapeUpdates,
    ) -> CloudResult<()> {
        self.area_backend(*area_id)
            .await
            .update_shape(area_id, shape_id, updates)
            .await
    }

    async fn delete_shape(&self, area_id: &AreaId, shape_id: &ShapeId) -> CloudResult<()> {
        self.area_backend(*area_id)
            .await
            .delete_shape(area_id, shape_id)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AreaAccess, CreateAreaRequest, Label, LabelArgs, LabelId, LabelUpdates, CloudError, Room,
        RoomUpdates, Shape, ShapeArgs, ShapeId, ShapeUpdates, backends::LocalBackend,
    };
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
        async fn set_area_property(&self, _: &AreaId, _: &str, _: &str) -> CloudResult<()> {
            Ok(())
        }
        async fn delete_area_property(&self, _: &AreaId, _: &str) -> CloudResult<()> {
            Ok(())
        }
        async fn update_room(&self, room_key: &RoomKey, _: RoomUpdates) -> CloudResult<Room> {
            Err(CloudError::RoomNotFound(room_key.clone()))
        }
        async fn delete_room(&self, _: &RoomKey) -> CloudResult<()> {
            Ok(())
        }
        async fn set_room_property(&self, _: &RoomKey, _: &str, _: &str) -> CloudResult<()> {
            Ok(())
        }
        async fn delete_room_property(&self, _: &RoomKey, _: &str) -> CloudResult<()> {
            Ok(())
        }
        async fn add_room_tag(&self, _: &RoomKey, _: &str) -> CloudResult<()> {
            Ok(())
        }
        async fn remove_room_tag(&self, _: &RoomKey, _: &str) -> CloudResult<()> {
            Ok(())
        }
        async fn create_room_exit(&self, _: &RoomKey, _: ExitArgs) -> CloudResult<Exit> {
            Err(CloudError::NotFoundOrNoAccess)
        }
        async fn update_exit(&self, _: &AreaId, _: &ExitId, _: ExitUpdates) -> CloudResult<()> {
            Ok(())
        }
        async fn delete_exit(&self, _: &AreaId, _: &ExitId) -> CloudResult<()> {
            Ok(())
        }
        async fn create_label(&self, _: &AreaId, _: LabelArgs) -> CloudResult<Label> {
            Err(CloudError::NotFoundOrNoAccess)
        }
        async fn update_label(&self, _: &AreaId, _: &LabelId, _: LabelUpdates) -> CloudResult<()> {
            Ok(())
        }
        async fn delete_label(&self, _: &AreaId, _: &LabelId) -> CloudResult<()> {
            Ok(())
        }
        async fn create_shape(&self, _: &AreaId, _: ShapeArgs) -> CloudResult<Shape> {
            Err(CloudError::NotFoundOrNoAccess)
        }
        async fn update_shape(&self, _: &AreaId, _: &ShapeId, _: ShapeUpdates) -> CloudResult<()> {
            Ok(())
        }
        async fn delete_shape(&self, _: &AreaId, _: &ShapeId) -> CloudResult<()> {
            Ok(())
        }
    }

    fn owned_area(id: AreaId, name: &str, rev: i64) -> AreaWithDetails {
        AreaWithDetails {
            area: Area {
                id,
                user_id: None,
                atlas_id: None,
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
            content_hash: None,
            properties: Vec::new(),
            rooms: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
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
