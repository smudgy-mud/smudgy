use std::{
    collections::{HashMap, HashSet},
    fmt, fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use log::warn;
use parking_lot::RwLock;
use tokio::task;
use uuid::Uuid;

use super::{LEGACY_ACCESS_FINGERPRINT, MapperBackend, cloud::CloudMapper};
use crate::{
    Area, AreaId, AreaLoadSource, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CloudResult, CreateAreaRequest, SyncRow,
    mutation::{MutationEnvelope, MutationResult},
};

/// Case-insensitive check for the `.json` cache-file extension.
fn has_json_extension(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

/// The versioned sub-namespace all cache files live under. Bumped with the
/// area document format ([`crate::AREA_FORMAT_VERSION`]): cloud cache files
/// are disposable, so a format change simply abandons the old namespace and
/// refetches — a v1 cache file is never deserialized as v2.
const CACHE_FORMAT_NAMESPACE: &str = "v2";

/// Removes cache state from earlier formats: pre-viewer-namespace files
/// (`{area_id}-{rev}.json` directly in the cache root) and the pre-`v2/`
/// per-viewer directories (any subdirectory other than the current
/// namespace). Old files are never read by the current scheme and may hold
/// map data in a superseded format (or that per-viewer isolation now
/// guards); best-effort, synchronous (runs once at construction, before any
/// async context exists).
fn remove_legacy_cache_files(cache_dir: &Path) {
    let Ok(entries) = fs::read_dir(cache_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_legacy_file = path.is_file()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(has_json_extension);
        if is_legacy_file {
            if let Err(err) = fs::remove_file(&path) {
                warn!(
                    "Failed to remove legacy cache file {}: {err}",
                    path.display()
                );
            }
            continue;
        }
        let is_old_namespace = path.is_dir()
            && path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name != CACHE_FORMAT_NAMESPACE);
        if is_old_namespace && let Err(err) = fs::remove_dir_all(&path) {
            warn!(
                "Failed to remove old-format cache directory {}: {err}",
                path.display()
            );
        }
    }
}

/// What we last learned about an area's server-side state. A cache hit
/// requires the cached copy to match on **both** fields: `rev` is opaque
/// (equality only — projected revs can move down when capabilities change)
/// and `fingerprint` detects capability flips that bump no rev.
#[derive(Debug, Clone, PartialEq, Eq)]
struct KnownAreaState {
    rev: i64,
    fingerprint: Option<String>,
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
    seen_auth_generation: AtomicU64,
    area_cache: RwLock<HashMap<AreaId, Arc<AreaWithDetails>>>,
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
        remove_legacy_cache_files(&cache_dir);
        let seen_auth_generation = AtomicU64::new(inner.auth_generation());
        Self {
            inner,
            cache_dir,
            viewer: RwLock::new(None),
            seen_auth_generation,
            area_cache: RwLock::new(HashMap::new()),
            known: RwLock::new(HashMap::new()),
            last_sources: RwLock::new(HashMap::new()),
        }
    }

    /// Client-side fingerprint of an area's access block; `None` for legacy
    /// servers that send no access block. Computing it locally keeps
    /// `GET /areas` and `GET /sync` reconciliation consistent.
    fn fingerprint_of(area: &Area) -> Option<String> {
        area.access.map(|access| access.fingerprint())
    }

    /// Maps a server sync-row fingerprint onto the client representation:
    /// the synthesized legacy sentinel becomes `None` so it compares equal
    /// to areas served without an access block.
    fn normalize_fingerprint(fingerprint: &str) -> Option<String> {
        (fingerprint != LEGACY_ACCESS_FINGERPRINT).then(|| fingerprint.to_string())
    }

    async fn cache_area(&self, area: &AreaWithDetails) {
        let fingerprint = Self::fingerprint_of(&area.area);

        // Disk persistence is best-effort: a read-only or full disk must not
        // turn a successful network fetch into a failed read.
        if let Err(err) = self.write_area_to_disk(area, fingerprint.as_deref()).await {
            warn!(
                "Failed to persist area {} to the disk cache: {err}",
                area.area.id
            );
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
                    rev: area.area.rev,
                    fingerprint: fingerprint.clone(),
                },
            );
        }

        // Remove every other on-disk rev/fingerprint for this area by scan:
        // the sync engine records fresh revs into `known` *before* refetching,
        // so a previously-known-state comparison cannot identify stale files.
        let keep = self
            .cache_file_path(&area.area.id, area.area.rev, fingerprint.as_deref())
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        self.remove_area_files(&area.area.id, keep).await;
    }

    async fn try_cache_hit(&self, area_id: &AreaId) -> Option<AreaWithDetails> {
        let known = { self.known.read().get(area_id).cloned() }?;

        if let Some(area) = self
            .area_cache
            .read()
            .get(area_id)
            .filter(|area| {
                area.area.rev == known.rev && Self::fingerprint_of(&area.area) == known.fingerprint
            })
            .map(|area| (**area).clone())
        {
            self.record_source(area_id, AreaLoadSource::Cache);
            return Some(area);
        }

        if let Some(area) = self
            .read_area_from_disk(area_id, known.rev, known.fingerprint.as_deref())
            .await
        {
            let mut cache = self.area_cache.write();
            cache.insert(*area_id, Arc::new(area.clone()));
            self.record_source(area_id, AreaLoadSource::Cache);
            return Some(area);
        }

        None
    }

    async fn invalidate_area(&self, area_id: &AreaId) {
        {
            let mut cache = self.area_cache.write();
            cache.remove(area_id);
        }

        let previous = {
            let mut known = self.known.write();
            known.remove(area_id)
        };

        if let Some(prev) = previous {
            self.remove_cached_file(area_id, prev.rev, prev.fingerprint.as_deref())
                .await;
        }
        self.last_sources.write().remove(area_id);
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
                    rev: area.rev,
                    fingerprint: Self::fingerprint_of(area),
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
        let generation = self.inner.auth_generation();
        if self.seen_auth_generation.swap(generation, Ordering::AcqRel) != generation {
            *self.viewer.write() = None;
            self.area_cache.write().clear();
            self.known.write().clear();
            self.last_sources.write().clear();
        }
    }

    /// Disk cache directory for the current viewer (`anon` until known),
    /// inside the versioned [`CACHE_FORMAT_NAMESPACE`].
    fn viewer_dir(&self) -> PathBuf {
        let viewer = *self.viewer.read();
        let name = viewer.map_or_else(|| "anon".to_string(), |id| id.to_string());
        self.cache_dir.join(CACHE_FORMAT_NAMESPACE).join(name)
    }

    fn cache_file_path(&self, area_id: &AreaId, rev: i64, fingerprint: Option<&str>) -> PathBuf {
        let fp = fingerprint.unwrap_or("none");
        self.viewer_dir().join(format!("{area_id}-{rev}-{fp}.json"))
    }

    async fn write_area_to_disk(
        &self,
        area: &AreaWithDetails,
        fingerprint: Option<&str>,
    ) -> CloudResult<()> {
        let path = self.cache_file_path(&area.area.id, area.area.rev, fingerprint);
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

    async fn read_area_from_disk(
        &self,
        area_id: &AreaId,
        rev: i64,
        fingerprint: Option<&str>,
    ) -> Option<AreaWithDetails> {
        let path = self.cache_file_path(area_id, rev, fingerprint);

        match task::spawn_blocking(move || -> CloudResult<AreaWithDetails> {
            let bytes = fs::read(&path)?;
            let area = serde_json::from_slice(&bytes)?;
            Ok(area)
        })
        .await
        {
            Ok(Ok(area)) => Some(area),
            Ok(Err(err)) => {
                warn!("Failed to read cached area {area_id}:{rev}: {err}");
                None
            }
            Err(join_err) => {
                warn!("Cache read task for area {area_id}:{rev} failed: {join_err}");
                None
            }
        }
    }

    async fn remove_cached_file(&self, area_id: &AreaId, rev: i64, fingerprint: Option<&str>) {
        let path = self.cache_file_path(area_id, rev, fingerprint);

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
            warn!("Failed to remove cached file for area {area_id} rev {rev}: {err}");
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
        let area = self.inner.create_area(request).await?;
        self.invalidate_area(&area.id).await;
        Ok(area)
    }

    async fn list_areas(&self) -> CloudResult<Vec<Area>> {
        self.check_auth_generation();
        let areas = self.inner.list_areas().await?;
        self.remember_server_state(&areas).await;
        Ok(areas)
    }

    async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
        self.check_auth_generation();
        if let Some(area) = self.try_cache_hit(area_id).await {
            return Ok(area);
        }

        let fetched = self.inner.get_area(area_id).await?;
        self.cache_area(&fetched).await;
        self.record_source(area_id, AreaLoadSource::Remote);
        Ok(fetched)
    }

    async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
        self.check_auth_generation();
        self.inner.sync_state().await
    }

    async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
        // Errors propagate without touching the current viewer.
        let identity = self.inner.viewer_identity().await?;

        if let Some(id) = identity {
            let changed = {
                let mut viewer = self.viewer.write();
                if *viewer == Some(id) {
                    false
                } else {
                    *viewer = Some(id);
                    true
                }
            };

            if changed {
                // A different viewer must never see another viewer's cache.
                self.area_cache.write().clear();
                self.known.write().clear();
                self.last_sources.write().clear();
            }
        }

        Ok(identity)
    }

    fn auth_generation(&self) -> u64 {
        self.inner.auth_generation()
    }

    fn has_credential(&self) -> bool {
        self.inner.has_credential()
    }

    async fn purge_area(&self, area_id: &AreaId) {
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
                    rev: row.rev,
                    fingerprint: Self::normalize_fingerprint(&row.access_fingerprint),
                },
            );
        }
    }

    fn supports_sync(&self) -> bool {
        self.inner.supports_sync()
    }

    async fn update_area(&self, area_id: &AreaId, updates: AreaUpdates) -> CloudResult<()> {
        self.inner.update_area(area_id, updates).await?;
        self.invalidate_area(area_id).await;
        Ok(())
    }

    async fn delete_area(&self, area_id: &AreaId) -> CloudResult<()> {
        self.inner.delete_area(area_id).await?;
        self.invalidate_area(area_id).await;
        Ok(())
    }

    async fn execute_mutation(
        &self,
        area_id: &AreaId,
        envelope: &MutationEnvelope,
    ) -> CloudResult<MutationResult> {
        // Pure passthrough of the envelope — preconditions and conflicts are
        // the upstream's verdict. A success moved the area, so its cached
        // bytes are stale; a failure (including a revision conflict) changed
        // nothing and keeps the cache.
        let result = self.inner.execute_mutation(area_id, envelope).await?;
        self.invalidate_area(area_id).await;
        Ok(result)
    }

    // Atlas operations are metadata-only (folders, not area bytes), so they
    // pass straight through; the area cache is untouched. `move_area_to_atlas`
    // is inherited from the trait default — it routes through `update_area`
    // above, which already invalidates the moved area.

    async fn list_atlases(&self) -> CloudResult<Vec<AtlasListItem>> {
        self.inner.list_atlases().await
    }

    async fn create_atlas(&self, name: &str) -> CloudResult<Atlas> {
        self.inner.create_atlas(name).await
    }

    async fn rename_atlas(&self, atlas_id: &AtlasId, name: &str) -> CloudResult<Atlas> {
        self.inner.rename_atlas(atlas_id, name).await
    }

    async fn delete_atlas(&self, atlas_id: &AtlasId) -> CloudResult<()> {
        self.inner.delete_atlas(atlas_id).await
    }

    fn last_area_source(&self, area_id: &AreaId) -> AreaLoadSource {
        self.last_sources
            .read()
            .get(area_id)
            .copied()
            .unwrap_or(AreaLoadSource::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AreaAccess, CloudError};
    use async_trait::async_trait;
    use chrono::Utc;
    use parking_lot::Mutex;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use uuid::Uuid;

    #[derive(Clone, Debug, Default)]
    struct MockBackend {
        storage: Arc<Mutex<HashMap<AreaId, AreaWithDetails>>>,
        list_calls: Arc<AtomicUsize>,
        get_calls: Arc<AtomicUsize>,
        viewer: Arc<Mutex<Option<Uuid>>>,
    }

    impl MockBackend {
        fn new(areas: Vec<AreaWithDetails>) -> Self {
            let storage = areas.into_iter().map(|area| (area.area.id, area)).collect();

            Self {
                storage: Arc::new(Mutex::new(storage)),
                list_calls: Arc::new(AtomicUsize::new(0)),
                get_calls: Arc::new(AtomicUsize::new(0)),
                viewer: Arc::new(Mutex::new(None)),
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
            Ok(self.area(area_id))
        }

        async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
            Ok(*self.viewer.lock())
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
                versions: vec![crate::mutation::VersionInfo {
                    resource: crate::mutation::ResourceKind::Area,
                    id: area_id.0,
                    rev: area.area.rev,
                    deleted: false,
                }],
                data: Vec::new(),
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
            area: Area {
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
            },
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
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

    /// Revs are opaque: a *downward* move (projected rev shrinking after a
    /// capability change) must still trigger a refetch.
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

    /// Capability flips bump no rev; the fingerprint alone must invalidate.
    #[tokio::test]
    async fn refetches_when_fingerprint_changes_with_same_rev() {
        let area_id = AreaId(Uuid::new_v4());
        let backend = MockBackend::new(vec![sample_area(area_id, 1, Some(SHARED_VIEW))]);
        let cache_dir = temp_cache_dir();
        let cached = CachedBackend::new(backend.clone(), cache_dir.clone());

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_id).await.expect("first fetch");

        backend.update_area(sample_area(area_id, 1, Some(SHARED_EDIT)));

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
            area_files_in(&cache_dir.join("v2").join(viewer_a.to_string()), &area_id).len(),
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
            area_files_in(&cache_dir.join("v2").join(viewer_b.to_string()), &area_id).len(),
            1
        );
        assert_eq!(
            area_files_in(&cache_dir.join("v2").join(viewer_a.to_string()), &area_id).len(),
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

        let viewer_dir = cache_dir.join("v2").join("anon");
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
        backend.update_area(sample_area(area_a, 2, Some(SHARED_VIEW)));
        cached
            .note_sync_rows(&[SyncRow {
                area_id: area_a,
                rev: 2,
                access_fingerprint: SHARED_VIEW.fingerprint(),
            }])
            .await;

        let viewer_dir = cache_dir.join("v2").join("anon");
        assert!(area_files_in(&viewer_dir, &area_b).is_empty());

        // A's known rev moved, so the stale cached copy is bypassed.
        let refreshed = cached.get_area(&area_a).await.expect("refetch a");
        assert_eq!(refreshed.area.rev, 2);
        assert_eq!(backend.get_calls.load(Ordering::Relaxed), 3);

        fs::remove_dir_all(cache_dir).ok();
    }

    /// Cache files are disposable: construction abandons pre-`v2/` state
    /// (root-level files and old per-viewer directories) best-effort, and
    /// fresh fetches land inside the `v2/` namespace.
    #[tokio::test]
    async fn construction_discards_the_old_cache_namespace() {
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
            !old_viewer_dir.exists(),
            "the old per-viewer namespace is discarded"
        );
        assert!(
            !cache_dir.join(format!("{area_id}-1.json")).exists(),
            "pre-namespace root files are discarded"
        );

        cached.list_areas().await.expect("list ok");
        cached.get_area(&area_id).await.expect("fetch");
        assert_eq!(
            area_files_in(&cache_dir.join("v2").join("anon"), &area_id).len(),
            1,
            "fresh fetches land inside the v2 namespace"
        );

        fs::remove_dir_all(cache_dir).ok();
    }
}
