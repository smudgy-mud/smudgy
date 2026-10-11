use std::{
    collections::{HashMap, HashSet},
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use log::warn;
use parking_lot::RwLock;
use tokio::task;
use uuid::Uuid;

use super::{AreaMergeCommit, AreaMergePlan, MapperBackend, cloud::CloudMapper};
use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CloudResult, CreateAreaRequest, MapStorage, SourceId, SyncRow,
    cloud_api::{SecretChange, SecretGrant, SecretGrantChange, SecretSummary},
    mutation::{MoveRequest, MoveResult, MutationEnvelope, MutationResult},
};

/// Case-insensitive check for the `.json` cache-file extension.
fn has_json_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

/// Current cloud cache namespace. Older namespaces stay on disk for upgrade
/// recovery, but are never read into the current projection. In particular, v4
/// can contain optimistic overlays from queues written before recovery existed.
pub const CACHE_FORMAT_NAMESPACE: &str = "v5";

/// What we last learned about an area's server-side state: the token over
/// the caller's projection, which moves with content and with access alike.
/// A cache hit requires the cached copy to carry the same token.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KnownAreaState {
    token: String,
}

/// A write can finish before `/me` resolves. Retain that generation's dirty
/// bit until a verified viewer supplies its scope. One lock serializes reset,
/// identity installation and publication, so old writes cannot dirty a new viewer.
struct CloudHintState {
    generation: u64,
    scope: Option<Arc<super::cloud_changes::CloudChanges>>,
    dirty: bool,
}

impl CloudHintState {
    fn new(generation: u64) -> Self {
        Self {
            generation,
            scope: None,
            dirty: false,
        }
    }
}

/// Generic caching layer that keeps `get_area` responses in memory and on
/// disk (namespaced per viewer) while always forwarding `list_areas` to the
/// upstream backend.
pub struct CachedBackend<T>
where
    T: MapperBackend + Send + Sync,
{
    inner: T,
    cache_dir: PathBuf,
    /// The authenticated viewer the disk cache is namespaced under; `None`
    /// (the `anon` directory) until an identity is resolved.
    viewer: RwLock<Option<Uuid>>,
    /// The credential generation the caches were populated under; reads
    /// compare it so a credential switch stops serving the previous
    /// viewer's data immediately (see [`Self::check_auth_generation`]).
    cloud_hints: RwLock<CloudHintState>,
    writer_id: Uuid,
    area_cache: RwLock<HashMap<AreaId, Arc<AreaWithDetails>>>,
    /// Serialize a read's cache installation with invalidation after a write.
    /// Otherwise an older in-flight GET can repopulate a just-invalidated cache.
    area_io: RwLock<HashMap<AreaId, Arc<tokio::sync::Mutex<()>>>>,
    known: RwLock<HashMap<AreaId, KnownAreaState>>,
    last_sources: RwLock<HashMap<AreaId, AreaLoadSource>>,
}

impl<T> fmt::Debug for CachedBackend<T>
where
    T: MapperBackend + Send + Sync + fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CachedBackend")
            .field("inner", &self.inner)
            .field("cache_dir", &self.cache_dir)
            .finish_non_exhaustive()
    }
}

impl<T> CachedBackend<T>
where
    T: MapperBackend + Send + Sync,
{
    /// Wrap a backend instance with caching.
    #[must_use]
    pub fn new(inner: T, cache_dir: impl Into<PathBuf>) -> Self {
        let cache_dir = cache_dir.into();
        if inner.supports_sync() {
            // Cloud bytes are written only after `/me` proves their viewer
            // namespace. Remove anonymous files left by older clients so
            // same-id/same-rev areas cannot collide across accounts.
            let anonymous = cache_dir.join(CACHE_FORMAT_NAMESPACE).join("anon");
            if let Err(error) = fs::remove_dir_all(&anonymous)
                && error.kind() != io::ErrorKind::NotFound
            {
                warn!(
                    "Failed to remove legacy anonymous cloud cache {}: {error}",
                    anonymous.display()
                );
            }
        }
        let cloud_hints = RwLock::new(CloudHintState::new(inner.auth_generation()));
        Self {
            inner,
            cache_dir,
            viewer: RwLock::new(None),
            cloud_hints,
            writer_id: Uuid::new_v4(),
            area_cache: RwLock::new(HashMap::new()),
            area_io: RwLock::new(HashMap::new()),
            known: RwLock::new(HashMap::new()),
            last_sources: RwLock::new(HashMap::new()),
        }
    }

    async fn cache_area(&self, area: &AreaWithDetails) {
        let token = area.area.view_token();

        // Disk persistence is best-effort: a read-only or full disk must not
        // turn a successful network fetch into a failed read.
        let disk_namespace_is_proven = !self.inner.supports_sync() || self.viewer.read().is_some();
        if disk_namespace_is_proven {
            if let Err(err) = self.write_area_to_disk(area, &token).await {
                warn!(
                    "Failed to persist area {} to the disk cache: {err}",
                    area.area.id
                );
            }
        }

        {
            let mut cache = self.area_cache.write();
            cache.insert(area.area.id, Arc::new(area.clone()));
        }

        {
            let mut known = self.known.write();
            known.insert(
                area.area.id,
                KnownAreaState {
                    token: token.clone(),
                },
            );
        }

        // Remove every other on-disk token for this area by scan:
        // the sync engine records fresh revs into `known` *before* refetching,
        // so a previously-known-state comparison cannot identify stale files.
        if disk_namespace_is_proven {
            let keep = self
                .cache_file_path(&area.area.id, &token)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            self.remove_area_files(&area.area.id, keep).await;
        }
    }

    fn area_io(&self, area: AreaId) -> Arc<tokio::sync::Mutex<()>> {
        self.area_io.write().entry(area).or_default().clone()
    }

    async fn try_cache_hit(&self, area_id: &AreaId) -> Option<AreaWithDetails> {
        let known = { self.known.read().get(area_id).cloned() }?;

        if let Some(area) = self
            .area_cache
            .read()
            .get(area_id)
            .filter(|area| area.area.view_token() == known.token)
            .map(|area| (**area).clone())
        {
            self.record_source(area_id, AreaLoadSource::Cache);
            return Some(area);
        }

        if !self.inner.supports_sync() || self.viewer.read().is_some() {
            if let Some(area) = self.read_area_from_disk(area_id, &known.token).await {
                let mut cache = self.area_cache.write();
                cache.insert(*area_id, Arc::new(area.clone()));
                self.record_source(area_id, AreaLoadSource::Cache);
                return Some(area);
            }
        }

        None
    }

    async fn invalidate_area(&self, area_id: &AreaId) {
        let _io = self.area_io(*area_id).lock_owned().await;
        {
            let mut cache = self.area_cache.write();
            cache.remove(area_id);
        }

        let previous = {
            let mut known = self.known.write();
            known.remove(area_id)
        };

        if let Some(prev) = previous {
            self.remove_cached_file(area_id, &prev.token).await;
        }
        self.last_sources.write().remove(area_id);
    }

    /// After an accepted write to an area under a captured credential
    /// generation: drop the area's cached bytes while that credential is
    /// still current (a changed one has its own viewer cache), then announce
    /// the change.
    async fn settle_area_write(&self, area_id: &AreaId, auth_generation: u64, hint: u64) {
        if self.inner.auth_generation() == auth_generation {
            self.invalidate_area(area_id).await;
        } else {
            self.check_auth_generation();
        }
        self.publish_cloud_change(hint);
    }

    /// Evicts every cached area whose id is not in `ids`, deleting their
    /// on-disk files under the current viewer directory as well.
    async fn purge_ids_not_in(&self, ids: &HashSet<AreaId>) {
        let vanished: Vec<AreaId> = {
            let known = self.known.read();
            known
                .keys()
                .filter(|id| !ids.contains(id))
                .copied()
                .collect()
        };

        {
            let mut cache = self.area_cache.write();
            cache.retain(|area_id, _| ids.contains(area_id));
        }
        {
            let mut known = self.known.write();
            known.retain(|area_id, _| ids.contains(area_id));
        }
        {
            let mut sources = self.last_sources.write();
            sources.retain(|area_id, _| ids.contains(area_id));
        }

        for area_id in &vanished {
            self.remove_area_files(area_id, None).await;
        }
    }

    fn update_known_revs(&self, areas: &[Area]) {
        let mut known = self.known.write();
        for area in areas {
            known.insert(
                area.id,
                KnownAreaState {
                    token: area.view_token(),
                },
            );
        }
    }

    async fn remember_server_state(&self, areas: &[Area]) {
        let ids: HashSet<AreaId> = areas.iter().map(|area| area.id).collect();
        self.purge_ids_not_in(&ids).await;
        self.update_known_revs(areas);
    }

    fn record_source(&self, area_id: &AreaId, source: AreaLoadSource) {
        let mut sources = self.last_sources.write();
        sources.insert(*area_id, source);
    }

    /// Synchronous credential-switch guard, run at the top of every read:
    /// the moment the credential generation moves, the previous viewer's
    /// in-memory data and disk-namespace selection stop being served —
    /// without waiting for the sync engine to resolve the new identity over
    /// the network. `viewer_identity` re-establishes the namespace later.
    fn check_auth_generation(&self) {
        let mut hints = self.cloud_hints.write();
        let generation = self.inner.auth_generation();
        if hints.generation != generation {
            *hints = CloudHintState::new(generation);
            *self.viewer.write() = None;
            self.area_cache.write().clear();
            self.known.write().clear();
            self.last_sources.write().clear();
        }
    }

    fn install_viewer_identity(&self, identity: Option<Uuid>, generation: u64) -> bool {
        let mut hints = self.cloud_hints.write();
        if hints.generation != generation || self.inner.auth_generation() != generation {
            return false;
        }
        hints.scope = identity.and_then(|id| {
            self.inner
                .mutation_journal_namespace()
                .map(|origin| super::cloud_changes::CloudChanges::for_viewer(&origin, id))
        });
        let changed = {
            let mut viewer = self.viewer.write();
            let changed = *viewer != identity;
            *viewer = identity;
            changed
        };
        if changed {
            // A different viewer must never see another viewer's cache.
            self.area_cache.write().clear();
            self.known.write().clear();
            self.last_sources.write().clear();
        }
        if hints.dirty
            && let Some(scope) = &hints.scope
        {
            scope.publish(self.writer_id);
            hints.dirty = false;
        }
        true
    }

    fn cloud_write_generation(&self) -> u64 {
        self.check_auth_generation();
        self.cloud_hints.read().generation
    }

    fn publish_cloud_change(&self, generation: u64) {
        if !self.inner.supports_sync() || self.inner.local_snapshot().is_some() {
            return;
        }
        let mut hints = self.cloud_hints.write();
        if hints.generation != generation || self.inner.auth_generation() != generation {
            return;
        }
        if let Some(scope) = &hints.scope {
            scope.publish(self.writer_id);
        } else {
            hints.dirty = true;
        }
    }

    /// Disk cache directory for the current viewer (`anon` until known),
    /// inside the versioned [`CACHE_FORMAT_NAMESPACE`].
    fn viewer_dir(&self) -> PathBuf {
        let viewer = *self.viewer.read();
        let name = viewer.map_or_else(|| "anon".to_string(), |id| id.to_string());
        self.cache_dir.join(CACHE_FORMAT_NAMESPACE).join(name)
    }

    fn cache_file_path(&self, area_id: &AreaId, token: &str) -> PathBuf {
        self.viewer_dir().join(format!("{area_id}-{token}.json"))
    }

    async fn write_area_to_disk(&self, area: &AreaWithDetails, token: &str) -> CloudResult<()> {
        let path = self.cache_file_path(&area.area.id, token);
        let dir = self.viewer_dir();
        let area_clone = area.clone();

        task::spawn_blocking(move || -> CloudResult<()> {
            fs::create_dir_all(&dir)?;
            let json = serde_json::to_vec(&area_clone)?;
            fs::write(&path, json)?;
            Ok(())
        })
        .await
        .map_err(|err| CloudError::InternalError(err.to_string()))??;

        Ok(())
    }

    async fn read_area_from_disk(&self, area_id: &AreaId, token: &str) -> Option<AreaWithDetails> {
        let path = self.cache_file_path(area_id, token);

        match task::spawn_blocking(move || -> CloudResult<AreaWithDetails> {
            let bytes = fs::read(&path)?;
            let area = serde_json::from_slice(&bytes)?;
            Ok(area)
        })
        .await
        {
            Ok(Ok(area)) => Some(area),
            Ok(Err(err)) => {
                warn!("Failed to read cached area {area_id} ({token}): {err}");
                None
            }
            Err(join_err) => {
                warn!("Cache read task for area {area_id} ({token}) failed: {join_err}");
                None
            }
        }
    }

    async fn remove_cached_file(&self, area_id: &AreaId, token: &str) {
        let path = self.cache_file_path(area_id, token);

        if let Err(err) = task::spawn_blocking(move || -> Result<(), io::Error> {
            match fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e),
            }
        })
        .await
        .map_err(io::Error::other)
        .and_then(|res| res)
        {
            warn!("Failed to remove cached file for area {area_id} ({token}): {err}");
        }
    }

    /// Deletes every on-disk cache file for an area under the **current**
    /// viewer directory (other viewers' namespaces are left alone), except
    /// the optionally-named file to keep.
    async fn remove_area_files(&self, area_id: &AreaId, keep_filename: Option<String>) {
        let dir = self.viewer_dir();
        let prefix = format!("{area_id}-");
        let id = *area_id;

        if let Err(err) = task::spawn_blocking(move || -> Result<(), io::Error> {
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e),
            };
            for entry in entries {
                let entry = entry?;
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                if keep_filename.as_deref() == Some(name) {
                    continue;
                }
                if name.starts_with(&prefix) && has_json_extension(name) {
                    match fs::remove_file(entry.path()) {
                        Ok(()) => {}
                        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e),
                    }
                }
            }
            Ok(())
        })
        .await
        .map_err(io::Error::other)
        .and_then(|res| res)
        {
            warn!("Failed to remove cached files for area {id}: {err}");
        }
    }
}

impl CachedBackend<CloudMapper> {
    /// Convenience constructor for the cloud backend.
    #[must_use]
    pub fn new_cloud(base_url: String, api_key: String, cache_dir: impl Into<PathBuf>) -> Self {
        Self::new(CloudMapper::new(base_url, api_key), cache_dir)
    }
}

pub type CachedCloudMapper = CachedBackend<CloudMapper>;

#[async_trait]
impl<T> MapperBackend for CachedBackend<T>
where
    T: MapperBackend + Send + Sync,
{
    async fn create_area(&self, request: CreateAreaRequest) -> CloudResult<Area> {
        let hint = self.cloud_write_generation();
        let area = self.inner.create_area(request).await?;
        self.invalidate_area(&area.id).await;
        self.publish_cloud_change(hint);
        Ok(area)
    }

    async fn create_area_at(
        &self,
        request: CreateAreaRequest,
        storage: MapStorage,
    ) -> CloudResult<Area> {
        let hint = self.cloud_write_generation();
        let area = self.inner.create_area_at(request, storage).await?;
        self.invalidate_area(&area.id).await;
        self.publish_cloud_change(hint);
        Ok(area)
    }

    async fn copy_cloud_area(
        &self,
        source: &AreaId,
        name: &str,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<Option<Area>> {
        let hint = self.cloud_write_generation();
        let copied = self.inner.copy_cloud_area(source, name, atlas_id).await?;
        if let Some(area) = &copied {
            self.invalidate_area(&area.id).await;
        }
        self.publish_cloud_change(hint);
        Ok(copied)
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        let auth_generation = self.inner.auth_generation();
        self.check_auth_generation();
        let areas = self.inner.list_areas().await?;
        if self.inner.auth_generation() != auth_generation {
            self.check_auth_generation();
            return Err(CloudError::CredentialChanged);
        }
        self.remember_server_state(&areas).await;
        Ok(areas)
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.get_area_at_generation(area_id, self.inner.auth_generation())
            .await
    }

    async fn get_area_at_generation(
        &self,
        area_id: &AreaId,
        auth_generation: u64,
    ) -> CloudResult<AreaWithDetails> {
        let _io = self.area_io(*area_id).lock_owned().await;
        if self.inner.auth_generation() != auth_generation {
            self.check_auth_generation();
            return Err(CloudError::CredentialChanged);
        }
        self.check_auth_generation();
        if let Some(area) = self.try_cache_hit(area_id).await {
            return Ok(area);
        }
        let fetched = self
            .inner
            .get_area_at_generation(area_id, auth_generation)
            .await?;
        if self.inner.auth_generation() != auth_generation {
            self.check_auth_generation();
            return Err(CloudError::CredentialChanged);
        }
        self.cache_area(&fetched).await;
        self.record_source(area_id, AreaLoadSource::Remote);
        Ok(fetched)
    }

    async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
        self.check_auth_generation();
        self.inner.sync_state().await
    }

    async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
        self.check_auth_generation();
        let auth_generation = self.inner.auth_generation();
        // Errors propagate without touching the current viewer.
        let identity = self.inner.viewer_identity().await?;
        if self.inner.auth_generation() != auth_generation {
            self.check_auth_generation();
            return Err(CloudError::CredentialChanged);
        }

        if !self.install_viewer_identity(identity, auth_generation) {
            self.check_auth_generation();
            return Err(CloudError::CredentialChanged);
        }
        Ok(identity)
    }

    async fn viewer_identity_at_generation(
        &self,
        auth_generation: u64,
    ) -> CloudResult<Option<Uuid>> {
        self.check_auth_generation();
        if self.inner.auth_generation() != auth_generation {
            self.check_auth_generation();
            return Err(CloudError::CredentialChanged);
        }
        let identity = self
            .inner
            .viewer_identity_at_generation(auth_generation)
            .await?;
        if self.inner.auth_generation() != auth_generation {
            self.check_auth_generation();
            return Err(CloudError::CredentialChanged);
        }
        if !self.install_viewer_identity(identity, auth_generation) {
            self.check_auth_generation();
            return Err(CloudError::CredentialChanged);
        }
        Ok(identity)
    }

    fn auth_generation(&self) -> u64 {
        self.inner.auth_generation()
    }

    fn has_credential(&self) -> bool {
        self.inner.has_credential()
    }

    fn default_storage(&self) -> MapStorage {
        self.inner.default_storage()
    }

    async fn purge_area(&self, area_id: &AreaId) {
        let _io = self.area_io(*area_id).lock_owned().await;
        {
            let mut cache = self.area_cache.write();
            cache.remove(area_id);
        }
        {
            let mut known = self.known.write();
            known.remove(area_id);
        }
        {
            let mut sources = self.last_sources.write();
            sources.remove(area_id);
        }
        self.remove_area_files(area_id, None).await;
    }

    async fn note_sync_rows(&self, rows: &[SyncRow]) {
        let ids: HashSet<AreaId> = rows.iter().map(|row| row.area_id).collect();
        self.purge_ids_not_in(&ids).await;

        let mut known = self.known.write();
        for row in rows {
            known.insert(
                row.area_id,
                KnownAreaState {
                    token: row.projection_token.clone(),
                },
            );
        }
    }

    fn supports_sync(&self) -> bool {
        self.inner.supports_sync()
    }

    fn mutation_journal_namespace(&self) -> Option<String> {
        self.inner.mutation_journal_namespace()
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner.update_area(area_id, updates).await?;
        self.invalidate_area(area_id).await;
        self.publish_cloud_change(hint);
        Ok(())
    }

    async fn update_area_at_generation(
        &self,
        area_id: &AreaId,
        updates: AreaUpdates,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner
            .update_area_at_generation(area_id, updates, auth_generation)
            .await?;
        if self.inner.auth_generation() == auth_generation {
            self.invalidate_area(area_id).await;
        } else {
            self.check_auth_generation();
        }
        self.publish_cloud_change(hint);
        Ok(())
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner.delete_area(area_id).await?;
        self.invalidate_area(area_id).await;
        self.publish_cloud_change(hint);
        Ok(())
    }

    async fn delete_area_at_generation(
        &self,
        area_id: &AreaId,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner
            .delete_area_at_generation(area_id, auth_generation)
            .await?;
        if self.inner.auth_generation() == auth_generation {
            self.invalidate_area(area_id).await;
        } else {
            self.check_auth_generation();
        }
        self.publish_cloud_change(hint);
        Ok(())
    }

    // Expected-rev forms forward so the precondition reaches the enforcing
    // backend; the cache is invalidated only after an accepted delete (a
    // revision-conflict refusal leaves the area — and its cache entry —
    // standing).
    async fn review_local_move(
        &self,
        area_id: &AreaId,
        auth_generation: u64,
    ) -> CloudResult<crate::relocation::LocalMoveReview> {
        self.inner.review_local_move(area_id, auth_generation).await
    }

    async fn finish_local_move(
        &self,
        area_id: &AreaId,
        guard: &crate::relocation::LocalMoveGuard,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner
            .finish_local_move(area_id, guard, auth_generation)
            .await?;
        if self.inner.auth_generation() == auth_generation {
            self.invalidate_area(area_id).await;
        } else {
            self.check_auth_generation();
        }
        self.publish_cloud_change(hint);
        Ok(())
    }

    async fn delete_area_expecting(
        &self,
        area_id: &AreaId,
        expected_rev: Option<i64>,
    ) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner
            .delete_area_expecting(area_id, expected_rev)
            .await?;
        self.invalidate_area(area_id).await;
        self.publish_cloud_change(hint);
        Ok(())
    }

    async fn delete_area_expecting_at_generation(
        &self,
        area_id: &AreaId,
        expected_rev: Option<i64>,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner
            .delete_area_expecting_at_generation(area_id, expected_rev, auth_generation)
            .await?;
        if self.inner.auth_generation() == auth_generation {
            self.invalidate_area(area_id).await;
        } else {
            self.check_auth_generation();
        }
        self.publish_cloud_change(hint);
        Ok(())
    }

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        let hint = self.cloud_write_generation();
        // Pure passthrough of the envelope — preconditions and conflicts are
        // the upstream's verdict. A success moved the area, so its cached
        // bytes are stale; a failure (including a revision conflict) changed
        // nothing and keeps the cache.
        let result = self.inner.execute_mutation(area_id, envelope).await?;
        self.invalidate_area(area_id).await;
        self.publish_cloud_change(hint);
        Ok(result)
    }

    async fn execute_mutation_at_generation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
        auth_generation: u64,
    ) -> CloudResult<MutationResult> {
        let hint = self.cloud_write_generation();
        let result = self
            .inner
            .execute_mutation_at_generation(area_id, envelope, auth_generation)
            .await?;
        if self.inner.auth_generation() == auth_generation {
            self.invalidate_area(area_id).await;
        } else {
            self.check_auth_generation();
        }
        self.publish_cloud_change(hint);
        Ok(result)
    }

    // ===== SECRETS AND MOVES =====
    //
    // Each accepted write moved a source of the map, so its cached bytes are
    // stale; a refusal changed nothing and keeps them.

    async fn create_secret_as(
        &self,
        area_id: &AreaId,
        secret: &crate::clan_secrets::NewSecret,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let hint = self.cloud_write_generation();
        let secret = self
            .inner
            .create_secret_as(area_id, secret, auth_generation)
            .await?;
        self.settle_area_write(area_id, auth_generation, hint).await;
        Ok(secret)
    }

    async fn update_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        change: &SecretChange,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let hint = self.cloud_write_generation();
        let updated = self
            .inner
            .update_secret(area_id, secret, change, auth_generation)
            .await?;
        self.settle_area_write(area_id, auth_generation, hint).await;
        Ok(updated)
    }

    async fn rename_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        name: &str,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let hint = self.cloud_write_generation();
        let renamed = self
            .inner
            .rename_secret(area_id, secret, name, auth_generation)
            .await?;
        self.settle_area_write(area_id, auth_generation, hint).await;
        Ok(renamed)
    }

    async fn recolor_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        color: Option<&str>,
        auth_generation: u64,
    ) -> CloudResult<SecretSummary> {
        let hint = self.cloud_write_generation();
        let recolored = self
            .inner
            .recolor_secret(area_id, secret, color, auth_generation)
            .await?;
        self.settle_area_write(area_id, auth_generation, hint).await;
        Ok(recolored)
    }

    async fn delete_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner
            .delete_secret(area_id, secret, auth_generation)
            .await?;
        self.settle_area_write(area_id, auth_generation, hint).await;
        Ok(())
    }

    // Grant writes move no revision and change nothing the caller reads, so
    // the cached map stays as it is.

    async fn secret_grants(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        auth_generation: u64,
    ) -> CloudResult<Vec<SecretGrant>> {
        self.inner
            .secret_grants(area_id, secret, auth_generation)
            .await
    }

    async fn grant_secret(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        grantee_id: Uuid,
        actions: &[&str],
        auth_generation: u64,
    ) -> CloudResult<SecretGrant> {
        self.inner
            .grant_secret(area_id, secret, grantee_id, actions, auth_generation)
            .await
    }

    async fn update_secret_grant(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        grant_id: Uuid,
        change: &SecretGrantChange,
        auth_generation: u64,
    ) -> CloudResult<SecretGrant> {
        self.inner
            .update_secret_grant(area_id, secret, grant_id, change, auth_generation)
            .await
    }

    async fn revoke_secret_grant(
        &self,
        area_id: &AreaId,
        secret: &SourceId,
        grant_id: Uuid,
        auth_generation: u64,
    ) -> CloudResult<()> {
        self.inner
            .revoke_secret_grant(area_id, secret, grant_id, auth_generation)
            .await
    }

    async fn move_content(
        &self,
        area_id: &AreaId,
        request: &MoveRequest,
        auth_generation: u64,
    ) -> CloudResult<MoveResult> {
        let hint = self.cloud_write_generation();
        let result = self
            .inner
            .move_content(area_id, request, auth_generation)
            .await?;
        self.settle_area_write(area_id, auth_generation, hint).await;
        Ok(result)
    }

    async fn review_move_content(
        &self,
        area_id: &AreaId,
        request: &MoveRequest,
        auth_generation: u64,
    ) -> CloudResult<crate::access_review::AccessReview> {
        self.inner
            .review_move_content(area_id, request, auth_generation)
            .await
    }

    // ===== MULTI-AREA TRANSACTIONS =====
    async fn review_filing(
        &self,
        area_id: &AreaId,
        atlas_id: Option<AtlasId>,
        auth_generation: u64,
    ) -> CloudResult<crate::access_review::AccessReview> {
        self.inner
            .review_filing(area_id, atlas_id, auth_generation)
            .await
    }

    async fn commit_reviewed_filing(
        &self,
        area_id: &AreaId,
        atlas_id: Option<AtlasId>,
        token: &str,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner
            .commit_reviewed_filing(area_id, atlas_id, token, auth_generation)
            .await?;
        self.settle_area_write(area_id, auth_generation, hint).await;
        Ok(())
    }

    async fn merge_areas(&self, plan: &AreaMergePlan) -> CloudResult<AreaMergeCommit> {
        // Pure passthrough of the plan; the upstream judges its revisions. A
        // commit rewrote the destination, removed the sources and may have
        // rewritten any third party, so every cached copy the plan names is
        // stale. A refusal (including a revision conflict) changed nothing
        // and keeps the cache.
        let commit = self.inner.merge_areas(plan).await?;
        for id in plan.touched_areas() {
            self.invalidate_area(&id).await;
        }
        Ok(commit)
    }

    // Atlas operations are metadata-only (folders, not area bytes), so they
    // pass straight through; the area cache is untouched. `move_area_to_atlas`
    // is inherited from the trait default — it routes through `update_area`
    // above, which already invalidates the moved area.

    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        self.inner.list_atlases().await
    }

    async fn list_atlases_in(&self, storage: MapStorage) -> CloudResult<Vec<AtlasListItem>> {
        self.inner.list_atlases_in(storage).await
    }

    async fn create_atlas(&self, name: &str) -> CloudResult<Atlas> {
        let hint = self.cloud_write_generation();
        let result = self.inner.create_atlas(name).await;
        if result.is_ok() {
            self.publish_cloud_change(hint);
        }
        result
    }

    async fn create_atlas_at(&self, name: &str, storage: MapStorage) -> CloudResult<Atlas> {
        let hint = self.cloud_write_generation();
        let result = self.inner.create_atlas_at(name, storage).await;
        if result.is_ok() {
            self.publish_cloud_change(hint);
        }
        result
    }

    async fn rename_atlas(&self, atlas_id: &AtlasId, name: &str) -> CloudResult<Atlas> {
        let hint = self.cloud_write_generation();
        let result = self.inner.rename_atlas(atlas_id, name).await;
        if result.is_ok() {
            self.publish_cloud_change(hint);
        }
        result
    }

    async fn delete_atlas(&self, atlas_id: &AtlasId) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        let result = self.inner.delete_atlas(atlas_id).await;
        if result.is_ok() {
            self.publish_cloud_change(hint);
        }
        result
    }

    async fn finish_local_atlas_move(
        &self,
        atlas_id: &AtlasId,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let hint = self.cloud_write_generation();
        self.inner
            .finish_local_atlas_move(atlas_id, auth_generation)
            .await?;
        self.publish_cloud_change(hint);
        Ok(())
    }

    fn cloud_changes(&self) -> Option<tokio::sync::watch::Receiver<u64>> {
        self.check_auth_generation();
        let hints = self.cloud_hints.read();
        if hints.generation != self.inner.auth_generation() {
            return None;
        }
        hints
            .scope
            .as_ref()
            .map(|scope| scope.subscribe(self.writer_id))
    }

    fn local_backend(&self) -> Option<&dyn MapperBackend> {
        self.inner.local_backend()
    }

    fn local_snapshot(&self) -> Option<Arc<super::local::LocalSnapshot>> {
        self.inner.local_snapshot()
    }

    async fn subscribe_local(&self) -> CloudResult<Option<tokio::sync::watch::Receiver<u64>>> {
        self.inner.subscribe_local().await
    }

    async fn refresh_local(&self) -> CloudResult<()> {
        self.inner.refresh_local().await
    }

    fn local_atlas_ids(&self) -> HashSet<AtlasId> {
        self.inner.local_atlas_ids()
    }

    fn local_area_ids(&self) -> HashSet<AreaId> {
        self.inner.local_area_ids()
    }

    fn ephemeral_area_ids(&self) -> HashSet<AreaId> {
        self.inner.ephemeral_area_ids()
    }

    fn last_area_source(&self, area_id: &AreaId) -> AreaLoadSource {
        self.last_sources
            .read()
            .get(area_id)
            .copied()
            .unwrap_or(AreaLoadSource::Unknown)
    }
}

/// Removes what this client keeps on disk for one cloud viewer: its area
/// cache under `cache_dir` (the directory a [`CachedBackend`] was built
/// with), and its queued writes under each mutation-journal root in
/// `journal_roots`. For an account that no longer exists, whose maps must not
/// outlive it on this computer and whose queued writes can never be sent.
/// Best-effort: a missing directory is fine, and other failures are logged.
pub fn forget_viewer_on_disk(cache_dir: &Path, journal_roots: &[PathBuf], viewer: Uuid) {
    let mut doomed: Vec<_> = ["", "v2", "v3", "v4", CACHE_FORMAT_NAMESPACE]
        .into_iter()
        .map(|version| cache_dir.join(version).join(viewer.to_string()))
        .collect();
    for root in journal_roots {
        doomed.extend(crate::mapper::pending::viewer_journal_directories(
            root, viewer,
        ));
    }
    for directory in doomed {
        match fs::remove_dir_all(&directory) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => warn!(
                "Failed to remove {} for a deleted account: {error}",
                directory.display()
            ),
        }
    }
}

#[cfg(test)]
mod forget_viewer_tests {
    use super::*;

    #[test]
    fn forgetting_a_viewer_removes_its_cache_and_journals_and_nothing_else() {
        let root = std::env::temp_dir().join(format!("smudgy-forget-viewer-{}", Uuid::new_v4()));
        let gone = Uuid::new_v4();
        let kept = Uuid::new_v4();
        let cache = root.join("maps");
        let journal = root
            .join("Arctic")
            .join("local")
            .join("pending-cloud-mutations");
        let viewer_journal = |viewer: Uuid| {
            journal
                .join("servers")
                .join("abc123")
                .join("viewers")
                .join(viewer.to_string())
        };
        for directory in [
            cache.join(CACHE_FORMAT_NAMESPACE).join(gone.to_string()),
            cache.join(CACHE_FORMAT_NAMESPACE).join(kept.to_string()),
            viewer_journal(gone),
            viewer_journal(kept),
            viewer_journal(gone).join("recovery/pre-format-3/journal"),
            viewer_journal(kept).join("recovery/pre-format-3/journal"),
            cache.join("v2").join(gone.to_string()),
            cache.join("v2").join(kept.to_string()),
            journal.join("local").join("active"),
        ] {
            fs::create_dir_all(&directory).unwrap();
            fs::write(directory.join("x.json"), b"{}").unwrap();
        }

        forget_viewer_on_disk(&cache, &[journal.clone(), root.join("Nowhere")], gone);

        assert!(
            !cache
                .join(CACHE_FORMAT_NAMESPACE)
                .join(gone.to_string())
                .exists()
        );
        assert!(!viewer_journal(gone).exists());
        assert!(!cache.join("v2").join(gone.to_string()).exists());
        assert!(cache.join("v2").join(kept.to_string()).exists());
        assert!(
            viewer_journal(kept)
                .join("recovery/pre-format-3/journal/x.json")
                .exists()
        );
        assert!(
            cache
                .join(CACHE_FORMAT_NAMESPACE)
                .join(kept.to_string())
                .exists()
        );
        assert!(viewer_journal(kept).exists());
        assert!(journal.join("local").join("active").join("x.json").exists());
        fs::remove_dir_all(root).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AreaAccess, CloudError, RoomNumber,
        backends::{AreaMergeOutcome, AreaMergeSource, Translate},
        mutation::VersionInfo,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use parking_lot::Mutex;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };
    use uuid::Uuid;

    #[derive(Clone, Debug, Default)]
    struct MockBackend {
        storage: Arc<Mutex<HashMap<AreaId, AreaWithDetails>>>,
        list_calls: Arc<AtomicUsize>,
        get_calls: Arc<AtomicUsize>,
        get_gate: Arc<Mutex<Option<Arc<tokio::sync::Semaphore>>>>,
        viewer: Arc<Mutex<Option<Uuid>>>,
        generation: Arc<AtomicU64>,
        cloud_origin: Option<String>,
    }

    impl MockBackend {
        fn new(areas: Vec<AreaWithDetails>) -> Self {
            let storage = areas.into_iter().map(|area| (area.area.id, area)).collect();

            Self {
                storage: Arc::new(Mutex::new(storage)),
                list_calls: Arc::new(AtomicUsize::new(0)),
                get_calls: Arc::new(AtomicUsize::new(0)),
                get_gate: Arc::default(),
                viewer: Arc::new(Mutex::new(None)),
                generation: Arc::new(AtomicU64::new(0)),
                cloud_origin: None,
            }
        }

        fn area(&self, area_id: &AreaId) -> AreaWithDetails {
            self.storage
                .lock()
                .get(area_id)
                .expect("area missing")
                .clone()
        }

        fn update_area(&self, area: AreaWithDetails) {
            self.storage.lock().insert(area.area.id, area);
        }

        fn set_viewer(&self, viewer: Option<Uuid>) {
            *self.viewer.lock() = viewer;
        }
    }

    #[async_trait]
    impl MapperBackend for MockBackend {
        async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
            Err(CloudError::NetworkError("not needed".to_string()))
        }

        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            self.list_calls.fetch_add(1, Ordering::Relaxed);
            Ok(self
                .storage
                .lock()
                .values()
                .map(|area| area.area.clone())
                .collect())
        }

        async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            self.get_calls.fetch_add(1, Ordering::Relaxed);
            let area = self.area(area_id);
            let gate = self.get_gate.lock().take();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            Ok(area)
        }

        async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
            Ok(*self.viewer.lock())
        }

        fn auth_generation(&self) -> u64 {
            self.generation.load(Ordering::Acquire)
        }

        fn supports_sync(&self) -> bool {
            self.cloud_origin.is_some()
        }

        fn mutation_journal_namespace(&self) -> Option<String> {
            self.cloud_origin.clone()
        }

        async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_area(&self, _area_id: &AreaId) -> CloudResult<()> {
            Ok(())
        }

        // Scripted success: bumps the stored rev and echoes it, so tests can
        // observe whether the caching layer keeps serving pre-mutation bytes.
        async fn execute_mutation(
            &self,
            area_id: &AreaId,
            envelope: &MutationEnvelope,
        ) -> CloudResult<MutationResult> {
            let mut storage = self.storage.lock();
            let area = storage
                .get_mut(area_id)
                .ok_or(CloudError::NotFoundOrNoAccess)?;
            area.area.rev += 1;
            Ok(MutationResult {
                operation_id: envelope.operation_id,
                versions: vec![VersionInfo::map_source(area_id.0, area.area.rev)],
                data: Vec::new(),
            })
        }

        // Scripted merge: honors the expected revisions, bumps the
        // destination and drops the sources, so tests can observe which
        // cached copies survive a commit and which survive a refusal.
        async fn merge_areas(&self, plan: &AreaMergePlan) -> CloudResult<AreaMergeCommit> {
            let mut storage = self.storage.lock();
            for (id, expected_rev) in &plan.expected {
                let current_rev = storage
                    .get(id)
                    .ok_or(CloudError::AreaNotFound(*id))?
                    .area
                    .rev;
                if current_rev != *expected_rev {
                    return Err(CloudError::RevisionConflict {
                        id: id.0,
                        expected_rev: *expected_rev,
                        current_rev,
                    });
                }
            }
            let into = storage.get_mut(&plan.into).expect("destination present");
            into.area.rev += 1;
            let destination = into.clone();
            for id in plan.deleted_areas() {
                storage.remove(&id);
            }
            Ok(AreaMergeCommit {
                outcome: AreaMergeOutcome {
                    rooms: Vec::new(),
                    versions: vec![VersionInfo::map_source(plan.into.0, destination.area.rev)],
                },
                documents: vec![destination],
            })
        }
    }

    const SHARED_VIEW: AreaAccess = AreaAccess {
        is_owner: false,
        can_edit: false,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
        include_secrets: false,
    };

    const SHARED_EDIT: AreaAccess = AreaAccess {
        is_owner: false,
        can_edit: true,
        can_reshare: false,
        can_copy: false,
        can_admin: false,
        include_secrets: false,
    };

    fn sample_area(area_id: AreaId, rev: i64, access: Option<AreaAccess>) -> AreaWithDetails {
        AreaWithDetails {
            room_data: Vec::new(),
            sources: Vec::new(),
            area: Area {
                projection_token: None,
                id: area_id,
                user_id: None,
                atlas_id: None,
                atlas_name: None,
                name: format!("Area {rev}"),
                created_at: Utc::now(),
                rev,
                access,
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                clan_id: None,
                clan_name: None,
                actions: None,
                clan_ownership: crate::clan_maps::ClanOwnership::default(),
            },
            format_version: crate::AREA_FORMAT_VERSION,
            properties: vec![],
            rooms: vec![],
            labels: vec![],
            shapes: vec![],
            connections: vec![],
            linked_areas: vec![],
        }
    }

    fn sample_area_with_rev(area_id: AreaId, rev: i64) -> AreaWithDetails {
        sample_area(area_id, rev, None)
    }

    fn temp_cache_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("smudgy-map-cache-test-{}", Uuid::new_v4()));
        fs::create_dir_all(&dir).expect("Failed to create temp cache dir");
        dir
    }

    fn area_files_in(dir: &std::path::Path, area_id: &AreaId) -> Vec<String> {
        let prefix = format!("{area_id}-");
        fs::read_dir(dir).map_or_else(
            |_| Vec::new(),
            |entries| {
                entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.file_name().to_string_lossy().into_owned())
                    .filter(|name| name.starts_with(&prefix) && has_json_extension(name))
                    .collect()
            },
        )
    }

    #[tokio::test]
    async fn invalidation_cannot_be_undone_by_an_older_get() {
        let id = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![sample_area_with_rev(id, 1)]);
        let cached = CachedBackend::new(backend.clone(), temp_cache_dir());
        let gate = Arc::new(tokio::sync::Semaphore::new(0));
        *backend.get_gate.lock() = Some(gate.clone());
        let (old, ()) = tokio::join!(cached.get_area(&id), async {
            while backend.get_calls.load(Ordering::Relaxed) == 0 {
                tokio::task::yield_now().await;
            }
            backend.update_area(sample_area_with_rev(id, 2));
            let invalidation = cached.invalidate_area(&id);
            tokio::pin!(invalidation);
            tokio::select! {
                () = &mut invalidation => panic!("invalidation must wait for the older read"),
                () = tokio::task::yield_now() => {}
            }
            gate.add_permits(1);
            invalidation.await;
        });
        assert_eq!(old.unwrap().area.rev, 1);
        assert_eq!(cached.get_area(&id).await.unwrap().area.rev, 2);
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 2);
        fs::remove_dir_all(&cached.cache_dir).unwrap();
    }

    #[tokio::test]
    async fn reuses_cached_area_when_rev_matches() {
        let area_id = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![sample_area_with_rev(area_id, 1)]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        let areas = cached.list_areas().await.expect("list ok");
        assert_eq!(areas.len(), 1);

        let first = cached.get_area(&area_id).await.expect("first fetch");
        assert_eq!(first.area.rev, 1);

        let second = cached.get_area(&area_id).await.expect("cached fetch");
        assert_eq!(second.area.rev, 1);

        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 1);

        fs::remove_dir_all(cache_dir).ok();
    }

    #[tokio::test]
    async fn refetches_when_rev_changes() {
        let area_id = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![sample_area_with_rev(area_id, 1)]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_id).await.expect("first fetch");

        let mut updated = backend.area(&area_id);
        updated.area.rev = 2;
        backend.update_area(updated);

        cached.list_areas().await.expect("list ok");
        let refreshed = cached.get_area(&area_id).await.expect("second fetch");
        assert_eq!(refreshed.area.rev, 2);

        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 2);

        fs::remove_dir_all(cache_dir).ok();
    }

    /// A downward move after a database reconstruction must still trigger a
    /// refetch.
    #[tokio::test]
    async fn refetches_when_rev_moves_backwards() {
        let area_id = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![sample_area_with_rev(area_id, 5)]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        cached.list_areas().await.expect("list ok");
        let first = cached.get_area(&area_id).await.expect("first fetch");
        assert_eq!(first.area.rev, 5);

        let mut updated = backend.area(&area_id);
        updated.area.rev = 3;
        backend.update_area(updated);

        cached.list_areas().await.expect("list ok");
        let refreshed = cached.get_area(&area_id).await.expect("second fetch");
        assert_eq!(refreshed.area.rev, 3);

        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 2);

        fs::remove_dir_all(cache_dir).ok();
    }

    /// An access change moves the projection token but no revision; the
    /// token alone must invalidate.
    #[tokio::test]
    async fn refetches_when_the_token_changes_with_same_rev() {
        let area_id = AreaId(Uuid::new_v4());
        let mut viewing = sample_area(area_id, 1, Some(SHARED_VIEW));
        viewing.area.projection_token = Some("p_view".to_string());
        let backend = MockBackend::new(vec![viewing]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_id).await.expect("first fetch");

        let mut editing = sample_area(area_id, 1, Some(SHARED_EDIT));
        editing.area.projection_token = Some("p_edit".to_string());
        backend.update_area(editing);

        cached.list_areas().await.expect("list ok");
        let refreshed = cached.get_area(&area_id).await.expect("second fetch");
        assert_eq!(refreshed.area.access, Some(SHARED_EDIT));

        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 2);

        fs::remove_dir_all(cache_dir).ok();
    }

    #[tokio::test]
    async fn viewer_switch_clears_memory_and_uses_distinct_disk_dir() {
        let area_id = AreaId(Uuid::new_v4());
        let viewer_a = Uuid::new_v4();
        let viewer_b = Uuid::new_v4();

        let backend = MockBackend::new(vec![sample_area_with_rev(area_id, 1)]);
        backend.set_viewer(Some(viewer_a));
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        let identity = cached.viewer_identity().await.expect("identity ok");
        assert_eq!(identity, Some(viewer_a));

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_id).await.expect("first fetch");
        cached.get_area(&area_id).await.expect("cache hit");
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 1);
        assert_eq!(
            area_files_in(
                &cache_dir
                    .join(CACHE_FORMAT_NAMESPACE)
                    .join(viewer_a.to_string()),
                &area_id
            )
            .len(),
            1
        );

        backend.set_viewer(Some(viewer_b));
        let identity = cached.viewer_identity().await.expect("identity ok");
        assert_eq!(identity, Some(viewer_b));

        // Memory and known state are gone: the next read refetches and lands
        // in viewer B's directory; viewer A's files are untouched.
        cached.get_area(&area_id).await.expect("refetch");
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 2);
        assert_eq!(
            area_files_in(
                &cache_dir
                    .join(CACHE_FORMAT_NAMESPACE)
                    .join(viewer_b.to_string()),
                &area_id
            )
            .len(),
            1
        );
        assert_eq!(
            area_files_in(
                &cache_dir
                    .join(CACHE_FORMAT_NAMESPACE)
                    .join(viewer_a.to_string()),
                &area_id
            )
            .len(),
            1
        );

        fs::remove_dir_all(cache_dir).ok();
    }

    #[tokio::test]
    async fn purge_area_removes_disk_files() {
        let area_id = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![sample_area_with_rev(area_id, 1)]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_id).await.expect("first fetch");

        let viewer_dir = cache_dir.join(CACHE_FORMAT_NAMESPACE).join("anon");
        assert_eq!(area_files_in(&viewer_dir, &area_id).len(), 1);

        cached.purge_area(&area_id).await;
        assert!(area_files_in(&viewer_dir, &area_id).is_empty());

        // Memory cache and known state are gone too: next read refetches.
        cached.get_area(&area_id).await.expect("refetch");
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 2);

        fs::remove_dir_all(cache_dir).ok();
    }

    /// A successful mutation invalidates the cached copy — the next read goes
    /// upstream and observes the post-mutation area.
    #[tokio::test]
    async fn execute_mutation_invalidates_the_cached_area() {
        let area_id = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![sample_area_with_rev(area_id, 1)]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_id).await.expect("first fetch");
        cached.get_area(&area_id).await.expect("cache hit");
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 1);

        let result = cached
            .execute_mutation(
                &area_id,
                &MutationEnvelope {
                    source: crate::SourceId::map(),
                    operation_id: Uuid::new_v4(),
                    preconditions: Vec::new(),
                    payload: Vec::new(),
                },
            )
            .await
            .expect("mock accepts the envelope");
        assert_eq!(result.versions[0].rev, 2);

        let refreshed = cached.get_area(&area_id).await.expect("refetch");
        assert_eq!(refreshed.area.rev, 2, "the pre-mutation copy is not served");
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 2);

        fs::remove_dir_all(cache_dir).ok();
    }

    #[tokio::test]
    async fn note_sync_rows_updates_known_state_and_purges_vanished() {
        let area_a = AreaId(Uuid::new_v4());
        let area_b = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![
            sample_area(area_a, 1, Some(SHARED_VIEW)),
            sample_area(area_b, 1, Some(SHARED_VIEW)),
        ]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_a).await.expect("fetch a");
        cached.get_area(&area_b).await.expect("fetch b");
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 2);

        // Area B vanished from the row set; area A moved to rev 2.
        let moved = sample_area(area_a, 2, Some(SHARED_VIEW));
        backend.update_area(moved.clone());
        cached
            .note_sync_rows(&[SyncRow::synthesized(&moved.area)])
            .await;

        let viewer_dir = cache_dir.join(CACHE_FORMAT_NAMESPACE).join("anon");
        assert!(area_files_in(&viewer_dir, &area_b).is_empty());

        // A's known rev moved, so the stale cached copy is bypassed.
        let refreshed = cached.get_area(&area_a).await.expect("refetch a");
        assert_eq!(refreshed.area.rev, 2);
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 3);

        fs::remove_dir_all(cache_dir).ok();
    }

    /// Old bytes remain available for recovery; fresh reads use a new namespace.
    #[tokio::test]
    async fn construction_preserves_but_does_not_load_old_cache_namespaces() {
        let area_id = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![sample_area_with_rev(area_id, 1)]);
        let cache_dir = temp_cache_dir();

        // Old layouts: a pre-namespace root file and a pre-v2 viewer dir.
        fs::write(cache_dir.join(format!("{area_id}-1.json")), b"{}").expect("root file");
        let old_viewer_dir = cache_dir.join(Uuid::new_v4().to_string());
        fs::create_dir_all(&old_viewer_dir).expect("old viewer dir");
        fs::write(old_viewer_dir.join(format!("{area_id}-1-none.json")), b"{}")
            .expect("old viewer file");

        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());
        assert!(
            old_viewer_dir.exists(),
            "the old per-viewer namespace is preserved"
        );
        assert!(
            cache_dir.join(format!("{area_id}-1.json")).exists(),
            "unattributed files are preserved without loading them"
        );

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_id).await.expect("fetch");
        assert_eq!(
            area_files_in(
                &cache_dir.join(CACHE_FORMAT_NAMESPACE).join("anon"),
                &area_id
            )
            .len(),
            1,
            "fresh fetches land inside the current namespace"
        );

        fs::remove_dir_all(cache_dir).ok();
    }

    // ===== area merges =====

    fn merge_plan(into: AreaId, source: AreaId, third: AreaId, expected_rev: i64) -> AreaMergePlan {
        AreaMergePlan {
            into,
            sources: vec![AreaMergeSource {
                id: source,
                translate: Translate::default(),
                rooms: None,
            }],
            inbound: vec![third],
            expected: vec![
                (into, expected_rev),
                (source, expected_rev),
                (third, expected_rev),
            ],
            number_floor: RoomNumber(1),
            vacant_targets: Vec::new(),
        }
    }

    /// Three areas warm in memory, on disk and in the known-state map.
    async fn warm_cache(
        backend: &MockBackend,
        cached: &CachedBackend<MockBackend>,
        ids: &[AreaId],
    ) {
        cached.list_areas().await.expect("list ok");
        for id in ids {
            cached.get_area(id).await.expect("fetch");
        }
        for id in ids {
            cached.get_area(id).await.expect("cache hit");
        }
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), ids.len());
    }

    fn cached_ids(cached: &CachedBackend<MockBackend>) -> (HashSet<AreaId>, HashSet<AreaId>) {
        (
            cached.area_cache.read().keys().copied().collect(),
            cached.known.read().keys().copied().collect(),
        )
    }

    /// A committed merge invalidates every area the plan names — the
    /// destination, the sources and the third parties — so the next read of
    /// any survivor goes upstream and observes the post-merge document.
    #[tokio::test]
    async fn merge_areas_invalidates_every_touched_area_on_success() {
        let into = AreaId(Uuid::new_v4());
        let source = AreaId(Uuid::new_v4());
        let third = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![
            sample_area_with_rev(into, 1),
            sample_area_with_rev(source, 1),
            sample_area_with_rev(third, 1),
        ]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());
        warm_cache(&backend, &cached, &[into, source, third]).await;

        let commit = cached
            .merge_areas(&merge_plan(into, source, third, 1))
            .await
            .expect("mock accepts the plan");
        assert_eq!(commit.documents[0].area.rev, 2);

        let (in_memory, known) = cached_ids(&cached);
        for id in [into, source, third] {
            assert!(!in_memory.contains(&id), "{id} left in memory");
            assert!(!known.contains(&id), "{id} left in the known-state map");
            assert!(
                area_files_in(&cache_dir.join(CACHE_FORMAT_NAMESPACE).join("anon"), &id).is_empty(),
                "{id} left on disk"
            );
        }
        let refreshed = cached.get_area(&into).await.expect("refetch");
        assert_eq!(refreshed.area.rev, 2, "the pre-merge copy is not served");
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 4);

        fs::remove_dir_all(cache_dir).ok();
    }

    /// A refused merge changed nothing upstream, so every cached copy stays
    /// valid and keeps serving.
    #[tokio::test]
    async fn merge_areas_keeps_every_cached_area_on_refusal() {
        let into = AreaId(Uuid::new_v4());
        let source = AreaId(Uuid::new_v4());
        let third = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![
            sample_area_with_rev(into, 1),
            sample_area_with_rev(source, 1),
            sample_area_with_rev(third, 1),
        ]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());
        warm_cache(&backend, &cached, &[into, source, third]).await;

        let result = cached
            .merge_areas(&merge_plan(into, source, third, 7))
            .await;
        assert!(
            matches!(result, Err(CloudError::RevisionConflict { .. })),
            "the upstream verdict passes through, got {result:?}"
        );

        let (in_memory, known) = cached_ids(&cached);
        for id in [into, source, third] {
            assert!(in_memory.contains(&id));
            assert!(known.contains(&id));
        }
        for id in [into, source, third] {
            cached.get_area(&id).await.expect("cache hit");
        }
        assert_eq!(
            backend.get_calls.load(Ordering::Relaxed),
            3,
            "no read went upstream"
        );

        fs::remove_dir_all(cache_dir).ok();
    }

    fn cloud_hint_backend(viewer: Uuid, origin: &str) -> CachedBackend<MockBackend> {
        let mut backend = MockBackend::new(vec![]);
        backend.cloud_origin = Some(origin.to_string());
        backend.set_viewer(Some(viewer));
        CachedBackend::new(backend, temp_cache_dir())
    }

    #[tokio::test]
    async fn cloud_hints_preserve_writes_completed_before_verified_identity() {
        let viewer = Uuid::new_v4();
        let cached = cloud_hint_backend(viewer, "https://startup-hints.example/api");
        let observer = super::super::cloud_changes::CloudChanges::for_viewer(
            "https://startup-hints.example/api",
            viewer,
        );
        let mut changes = observer.subscribe(Uuid::new_v4());
        for _ in 0..2 {
            cached
                .update_area(&AreaId(Uuid::new_v4()), AreaUpdates::default())
                .await
                .unwrap();
        }
        assert!(
            !changes.has_changed().unwrap(),
            "unverified writes carry no viewer scope"
        );
        cached.viewer_identity_at_generation(0).await.unwrap();
        assert!(
            changes.has_changed().unwrap(),
            "verification flushes the pending hint"
        );
        assert_eq!(*changes.borrow_and_update(), 1, "startup writes coalesce");
        cached.viewer_identity_at_generation(0).await.unwrap();
        assert!(
            !changes.has_changed().unwrap(),
            "installation does not replay a flushed hint"
        );
        fs::remove_dir_all(&cached.cache_dir).unwrap();
    }

    #[tokio::test]
    async fn cloud_hints_use_identity_resolved_while_a_write_was_in_flight() {
        let viewer = Uuid::new_v4();
        let cached = cloud_hint_backend(viewer, "https://in-flight-hints.example/api");
        let started = cached.cloud_write_generation();
        cached.viewer_identity().await.unwrap();
        let changes = cached
            .cloud_hints
            .read()
            .scope
            .as_ref()
            .unwrap()
            .subscribe(Uuid::new_v4());
        cached.publish_cloud_change(started);
        assert!(changes.has_changed().unwrap());
        fs::remove_dir_all(&cached.cache_dir).unwrap();
    }

    #[tokio::test]
    async fn cloud_hints_keep_the_new_scope_after_a_credential_switch() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let cached = cloud_hint_backend(first, "https://switch-hints.example/api");
        cached.viewer_identity_at_generation(0).await.unwrap();
        let old_scope = super::super::cloud_changes::CloudChanges::for_viewer(
            "https://switch-hints.example/api",
            first,
        );
        let old_changes = old_scope.subscribe(Uuid::new_v4());
        cached.inner.set_viewer(Some(second));
        cached.inner.generation.store(1, Ordering::Release);
        assert_eq!(
            cached.viewer_identity_at_generation(1).await.unwrap(),
            Some(second)
        );
        let changes = cached
            .cloud_hints
            .read()
            .scope
            .as_ref()
            .unwrap()
            .subscribe(Uuid::new_v4());
        assert_eq!(*cached.viewer.read(), Some(second));
        cached
            .update_area(&AreaId(Uuid::new_v4()), AreaUpdates::default())
            .await
            .unwrap();
        assert!(changes.has_changed().unwrap());
        assert!(!old_changes.has_changed().unwrap());
        assert!(
            !cached.install_viewer_identity(Some(first), 0),
            "stale identity cannot replace the scope"
        );
        assert_eq!(*cached.viewer.read(), Some(second));
        fs::remove_dir_all(&cached.cache_dir).unwrap();
    }

    #[tokio::test]
    async fn cloud_hints_discard_old_dirty_state_and_late_completions_on_switch() {
        let first = Uuid::new_v4();
        let second = Uuid::new_v4();
        let cached = cloud_hint_backend(first, "https://dirty-hints.example/api");
        let old_write = cached.cloud_write_generation();
        cached.publish_cloud_change(old_write);
        assert!(cached.cloud_hints.read().dirty);
        cached.inner.set_viewer(Some(second));
        cached.inner.generation.store(1, Ordering::Release);
        let new_scope = super::super::cloud_changes::CloudChanges::for_viewer(
            "https://dirty-hints.example/api",
            second,
        );
        let changes = new_scope.subscribe(Uuid::new_v4());
        cached.viewer_identity_at_generation(1).await.unwrap();
        cached.publish_cloud_change(old_write);
        assert!(
            !changes.has_changed().unwrap(),
            "old writes must not wake the new viewer"
        );
        assert!(!cached.cloud_hints.read().dirty);
        cached.publish_cloud_change(cached.cloud_write_generation());
        assert!(changes.has_changed().unwrap());
        fs::remove_dir_all(&cached.cache_dir).unwrap();
    }
}
