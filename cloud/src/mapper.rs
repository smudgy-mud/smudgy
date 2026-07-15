use crate::backends::MapperBackend;
use crate::error::CloudResult;
use crate::mapper::area_cache::AreaCache;
use crate::{
    Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CreateAreaRequest, ExitArgs, ExitId, ExitUpdates, LabelArgs, LabelId, LabelUpdates, RoomNumber,
    RoomUpdates, ShapeArgs, ShapeId, ShapeUpdates,
};

use arc_swap::ArcSwap;
use log::warn;
use parking_lot::Mutex;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio::task::JoinHandle;
use uuid::Uuid;

pub mod area_cache;
pub mod atlas_cache;
pub mod exit_cache;
pub mod room_cache;
pub mod room_connection;
pub mod sync_engine;
pub use atlas_cache::{AtlasCache, ElsewhereMatch};
pub use sync_engine::{SyncState, SyncStatus};

/// Composite key for room lookups
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RoomKey {
    pub area_id: AreaId,
    pub room_number: RoomNumber,
}

impl RoomKey {
    #[must_use]
    pub fn new(area_id: AreaId, room_number: RoomNumber) -> Self {
        Self {
            area_id,
            room_number,
        }
    }
}

/// Canonical form of a room tag: trimmed and UPPERCASE. Tags are
/// case-insensitive, so this is applied at every write and lookup boundary
/// (client cache, sync ops, script ops) — mirroring the server's normalization.
#[must_use]
pub fn normalize_tag(tag: &str) -> String {
    tag.trim().to_uppercase()
}

/// Total rooms admitted across a session's ephemeral areas. A guard against a
/// server minting unbounded room ids through an auto-mapper, not a sizing
/// statement — procedural games legitimately reach ~1M rooms, so the cap sits
/// well above that. Updates to existing rooms are never refused.
pub const EPHEMERAL_ROOM_CAP: usize = 2_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AreaLoadSource {
    Cache,
    Remote,
    Unknown,
}

impl std::fmt::Display for AreaLoadSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AreaLoadSource::Cache => write!(f, "cache"),
            AreaLoadSource::Remote => write!(f, "remote"),
            AreaLoadSource::Unknown => write!(f, "unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct AreaLoadStat {
    pub area_id: AreaId,
    pub name: String,
    pub revision: i64,
    pub load_duration: Duration,
    pub source: AreaLoadSource,
    /// Whether this area is shared *to* the viewer (owned by someone else) rather than
    /// owned by them. Drives the owned/shared breakdown in the session-start summary.
    pub shared: bool,
}

#[derive(Debug, Clone)]
pub struct LoadMapsSummary {
    pub list_duration: Duration,
    pub areas: Vec<AreaLoadStat>,
}

#[derive(Debug)]
enum AreaSyncOperation {
    RenameArea(AreaId, String),
    MoveArea(AreaId, Option<AtlasId>),
    DeleteArea(AreaId),
    SetAreaProperty(AreaId, String, String),
    DeleteAreaProperty(AreaId, String),
    UpdateRoom(RoomKey, RoomUpdates),
    DeleteRoom(RoomKey),
    SetRoomProperty(RoomKey, String, String),
    DeleteRoomProperty(RoomKey, String),
    AddRoomTag(RoomKey, String),
    RemoveRoomTag(RoomKey, String),
    UpdateExit(AreaId, ExitId, ExitUpdates),
    DeleteExit(AreaId, ExitId),
    UpdateLabel(AreaId, LabelId, LabelUpdates),
    DeleteLabel(AreaId, LabelId),
    UpdateShape(AreaId, ShapeId, ShapeUpdates),
    DeleteShape(AreaId, ShapeId),
}

impl AreaSyncOperation {
    /// The area this operation mutates; used to defer sync-engine refetches
    /// while local writes are still in flight.
    fn area_id(&self) -> AreaId {
        match self {
            Self::RenameArea(area_id, _)
            | Self::MoveArea(area_id, _)
            | Self::DeleteArea(area_id)
            | Self::SetAreaProperty(area_id, _, _)
            | Self::DeleteAreaProperty(area_id, _)
            | Self::UpdateExit(area_id, _, _)
            | Self::DeleteExit(area_id, _)
            | Self::UpdateLabel(area_id, _, _)
            | Self::DeleteLabel(area_id, _)
            | Self::UpdateShape(area_id, _, _)
            | Self::DeleteShape(area_id, _) => *area_id,
            Self::UpdateRoom(room_key, _)
            | Self::DeleteRoom(room_key)
            | Self::SetRoomProperty(room_key, _, _)
            | Self::DeleteRoomProperty(room_key, _)
            | Self::AddRoomTag(room_key, _)
            | Self::RemoveRoomTag(room_key, _) => room_key.area_id,
        }
    }
}

/// Sync statistics for diagnostics
#[derive(Debug, Default)]
pub struct SyncStats {
    pub operations_sent: AtomicU64,
    pub operations_succeeded: AtomicU64,
    pub operations_failed: AtomicU64,
}

impl SyncStats {
    #[must_use]
    pub fn operations_sent(&self) -> u64 {
        self.operations_sent.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn operations_succeeded(&self) -> u64 {
        self.operations_succeeded.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn operations_failed(&self) -> u64 {
        self.operations_failed.load(Ordering::Relaxed)
    }

    #[must_use]
    pub fn pending_operations(&self) -> u64 {
        self.operations_sent() - self.operations_succeeded() - self.operations_failed()
    }
}

#[derive(Clone)]
pub struct Mapper {
    inner: Arc<Inner>,
}
pub struct Inner {
    atlas_id: ArcSwap<Option<AtlasId>>,
    atlas_cache: ArcSwap<AtlasCache>,

    backend: Arc<dyn MapperBackend + Send + Sync>,

    // Background sync channel
    sync_sender: tokio::sync::mpsc::UnboundedSender<AreaSyncOperation>,

    // Sync diagnostics
    sync_stats: Arc<SyncStats>,

    // Sync engine state (see mapper::sync_engine)
    sync_status: ArcSwap<SyncStatus>,
    sync_revision: AtomicU64,
    sync_notify: Arc<Notify>,
    /// In-flight local write operations per area; the sync engine defers
    /// refetching an area while its count is non-zero.
    pending_by_area: Arc<Mutex<HashMap<AreaId, u64>>>,

    /// One teaching warning when the ephemeral room cap refuses a creation.
    ephemeral_cap_warned: AtomicBool,
}

impl std::fmt::Debug for Mapper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[Mapper]")
    }
}

impl Mapper {
    pub fn new(
        backend: Arc<dyn MapperBackend + Send + Sync>,
        cache_dir: impl Into<PathBuf>,
    ) -> Self {
        let (sync_sender, sync_receiver) = tokio::sync::mpsc::unbounded_channel();

        let cache = AtlasCache::new_with_areas(HashMap::new(), Arc::new(HashSet::new()));

        let cache_dir = cache_dir.into();
        if let Err(err) = fs::create_dir_all(&cache_dir) {
            warn!(
                "Failed to create mapper cache directory {}: {err}",
                cache_dir.display()
            );
        }

        let supports_sync = backend.supports_sync();
        let initial_state = if supports_sync {
            SyncState::Idle
        } else {
            SyncState::Disabled
        };

        let inner = Inner {
            atlas_id: ArcSwap::from_pointee(None),
            atlas_cache: ArcSwap::from_pointee(cache),
            backend,
            sync_sender,
            sync_stats: Arc::new(SyncStats::default()),
            sync_status: ArcSwap::from_pointee(SyncStatus {
                state: initial_state,
                last_error: None,
                last_sync: None,
            }),
            sync_revision: AtomicU64::new(0),
            sync_notify: Arc::new(Notify::new()),
            pending_by_area: Arc::new(Mutex::new(HashMap::new())),
            ephemeral_cap_warned: AtomicBool::new(false),
        };

        inner.spawn_sync_task(sync_receiver, inner.sync_stats.clone());

        let mapper = Self {
            inner: Arc::new(inner),
        };

        if supports_sync {
            sync_engine::spawn(&mapper.inner);
        }

        mapper
    }

    /// Wake the background sync engine for an immediate tick (no-op when the
    /// backend has no sync support).
    pub fn sync_now(&self) {
        self.inner.sync_notify.notify_one();
    }

    /// Snapshot of the sync engine's current status.
    #[must_use]
    pub fn sync_status(&self) -> SyncStatus {
        SyncStatus::clone(&self.inner.sync_status.load())
    }

    /// Monotonic counter bumped each time the sync engine swaps the atlas
    /// cache; UIs can poll it cheaply to detect background changes.
    #[must_use]
    pub fn sync_revision(&self) -> u64 {
        self.inner.sync_revision.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn get_current_atlas(&self) -> Arc<AtlasCache> {
        self.inner.get_current_atlas()
    }

    /// Replaces the set of disabled areas wholesale. Disabled areas drop out
    /// of the room-identification lookup tables and stop being routed
    /// *through* (they stay rendered and explicitly addressable). Ids not yet
    /// in the cache are kept, so an area disabled before sync lands it stays
    /// disabled when it arrives. Full table rebuild; toggles are rare.
    pub fn set_disabled_areas(&self, disabled: HashSet<AreaId>) {
        let disabled = Arc::new(disabled);
        self.inner
            .atlas_cache
            .rcu(|cache| Arc::new(cache.with_disabled_areas(disabled.clone())));
    }

    /// Convenience single-area toggle over [`Self::set_disabled_areas`].
    pub fn set_area_enabled(&self, area_id: AreaId, enabled: bool) {
        self.inner.atlas_cache.rcu(|cache| {
            let mut disabled = HashSet::clone(cache.disabled_areas());
            let changed = if enabled {
                disabled.remove(&area_id)
            } else {
                disabled.insert(area_id)
            };
            if changed {
                Arc::new(cache.with_disabled_areas(Arc::new(disabled)))
            } else {
                cache.clone()
            }
        });
    }

    /// Snapshot of the currently disabled areas.
    #[must_use]
    pub fn disabled_areas(&self) -> HashSet<AreaId> {
        HashSet::clone(self.inner.atlas_cache.load().disabled_areas())
    }

    /// Whether the area is enabled on the manual active/inactive axis only
    /// (true for areas not in the cache). Ignores per-server scope exclusion, so
    /// the editor's per-area active switch reflects only the user's toggle. Use
    /// [`Self::is_area_included`] to ask whether an area actually participates in
    /// room identification/routing.
    #[must_use]
    pub fn is_area_enabled(&self, area_id: &AreaId) -> bool {
        self.inner.atlas_cache.load().is_area_enabled(area_id)
    }

    /// Whether the area participates in room identification and routing: neither
    /// manually disabled nor per-server scope-excluded (true for areas not in
    /// the cache). This is the union enumeration/identification callers honor.
    #[must_use]
    pub fn is_area_included(&self, area_id: &AreaId) -> bool {
        self.inner.atlas_cache.load().is_area_included(area_id)
    }

    /// Cross-entry rescue probe: resolve a server-global external id against the
    /// scope-excluded areas only (maps homed on a *different* server entry).
    /// Returns the matched room plus its atlas id/name, or `None`. Used before
    /// the auto-mapper mints ephemeral rooms so a lagging sibling entry doesn't
    /// produce a duplicate map. See [`AtlasCache::find_room_elsewhere_by_external_id`].
    #[must_use]
    pub fn find_room_elsewhere_by_external_id(&self, external_id: &str) -> Option<ElsewhereMatch> {
        self.inner
            .atlas_cache
            .load()
            .find_room_elsewhere_by_external_id(external_id)
    }

    /// Replaces the per-server scope-exclusion sets wholesale (rcu-swapping the
    /// atlas cache like [`Self::set_disabled_areas`]). Scope-excluded atlases
    /// and atlas-less areas drop out of every room-identification lookup table
    /// and are treated as walls in routing — semantically identical to a manual
    /// disable, but stored on a separate axis so the user's manual toggle stays
    /// intact. Keying by atlas id means an area that later syncs into an
    /// excluded atlas is excluded automatically, with no recomputation. Full
    /// table rebuild; scope changes are rare.
    pub fn set_scope_exclusions(
        &self,
        excluded_atlases: HashSet<AtlasId>,
        excluded_areas: HashSet<AreaId>,
    ) {
        let atlases = Arc::new(excluded_atlases);
        let areas = Arc::new(excluded_areas);
        self.inner.atlas_cache.rcu(|cache| {
            Arc::new(cache.with_scope_exclusions(atlases.clone(), areas.clone()))
        });
    }

    pub fn create_area(&self, name: String) -> impl Future<Output = CloudResult<AreaId>> {
        self.inner.create_area(name)
    }

    /// Create an area in the session-lifetime ephemeral tier: in-memory,
    /// never persisted or synced, gone when the session closes. The default
    /// landing zone for protocol-driven auto-mapping; keeping one is an
    /// explicit [`Self::export_area`] → [`Self::import_areas`] copy.
    ///
    /// # Errors
    /// Propagates the backend's create error (the ephemeral tier itself is
    /// infallible; a non-composite backend without the tier routes this to
    /// its default create path).
    pub fn create_area_ephemeral(
        &self,
        name: String,
    ) -> impl Future<Output = CloudResult<AreaId>> {
        self.inner.create_area_ephemeral(name)
    }

    /// Whether `area_id` lives in the ephemeral (session-lifetime) tier.
    #[must_use]
    pub fn is_ephemeral(&self, area_id: &AreaId) -> bool {
        self.inner.backend.ephemeral_area_ids().contains(area_id)
    }

    /// Area ids of the ephemeral tier — the set the editor's atlas tree and
    /// per-area preference writes exclude.
    #[must_use]
    pub fn ephemeral_area_ids(&self) -> HashSet<AreaId> {
        self.inner.backend.ephemeral_area_ids()
    }

    /// Like [`Self::create_area`] but files the new area into `atlas_id`
    /// (`Some`) or leaves it loose (`None`), bypassing the recording target.
    ///
    /// # Errors
    /// Propagates the backend's create error (e.g. unauthorized, network).
    pub fn create_area_in(
        &self,
        name: String,
        atlas_id: Option<AtlasId>,
    ) -> impl Future<Output = CloudResult<AreaId>> {
        self.inner.create_area_in(name, atlas_id)
    }

    /// Import full areas into the local tier (fresh ids), returning their new ids. See
    /// [`Inner::import_areas`].
    ///
    /// # Errors
    /// Propagates the backend's persistence error.
    pub async fn import_areas(&self, areas: Vec<AreaWithDetails>) -> CloudResult<Vec<AreaId>> {
        self.inner.import_areas(areas).await
    }

    /// Serialize an area to its full [`AreaWithDetails`]. See [`Inner::export_area`].
    ///
    /// # Errors
    /// Propagates the backend's read error.
    pub async fn export_area(&self, area_id: AreaId) -> CloudResult<AreaWithDetails> {
        self.inner.export_area(area_id).await
    }

    /// The viewer's effective access for an area, or `None` if it isn't in the current atlas.
    #[must_use]
    pub fn area_effective_access(&self, area_id: AreaId) -> Option<AreaAccess> {
        self.inner.area_effective_access(area_id)
    }

    pub fn load_all_areas(&self) -> impl Future<Output = CloudResult<LoadMapsSummary>> {
        self.inner.load_all_areas()
    }

    pub fn rename_area(&self, area_id: AreaId, name: &str) {
        self.inner.rename_area(area_id, name);
    }

    pub fn delete_area(&self, area_id: AreaId) {
        self.inner.delete_area(area_id);
    }

    // === ATLAS (FOLDER) OPERATIONS ===

    /// List the viewer's own atlases (folders). Resolves against whichever
    /// backend(s) this mapper fans across.
    ///
    /// # Errors
    /// Propagates the backend's list error (e.g. unauthorized, network).
    pub fn list_atlases(&self) -> impl Future<Output = CloudResult<Vec<AtlasListItem>>> {
        let backend = self.inner.backend.clone();
        async move { backend.list_atlases().await }
    }

    /// List every area row visible to the viewer, straight from the backend
    /// (not the geometry cache). This is the **only** carrier of the list-only
    /// [`Area::family_token`] — `get_area` and the on-disk cache never include
    /// it — so the map editor calls this to build its per-viewer copy-family
    /// index. Callers must bucket `family_token` in memory for the current
    /// list and never persist it (see the field docs).
    ///
    /// # Errors
    /// Propagates the backend's list error (e.g. unauthorized, network).
    pub fn list_areas(&self) -> impl Future<Output = CloudResult<Vec<Area>>> {
        let backend = self.inner.backend.clone();
        async move { backend.list_areas().await }
    }

    /// Create an empty atlas (folder), routed to the backend's default tier.
    ///
    /// # Errors
    /// Propagates the backend's create error (e.g. unauthorized, network).
    pub fn create_atlas(&self, name: String) -> impl Future<Output = CloudResult<Atlas>> {
        let backend = self.inner.backend.clone();
        async move { backend.create_atlas(&name).await }
    }

    /// Create an empty atlas with an explicit tier preference (`prefer_local`).
    /// A pure-cloud mapper ignores the hint; a two-tier mapper honors it
    /// (falling back to local only when cloud is unavailable).
    ///
    /// # Errors
    /// Propagates the backend's create error (e.g. unauthorized, network).
    pub fn create_atlas_in(
        &self,
        name: String,
        prefer_local: bool,
    ) -> impl Future<Output = CloudResult<Atlas>> {
        let backend = self.inner.backend.clone();
        async move { backend.create_atlas_in(&name, prefer_local).await }
    }

    /// Rename an atlas.
    ///
    /// # Errors
    /// Propagates the backend's rename error (owner-only; uniform 404
    /// otherwise).
    pub fn rename_atlas(
        &self,
        atlas_id: AtlasId,
        name: String,
    ) -> impl Future<Output = CloudResult<Atlas>> {
        let backend = self.inner.backend.clone();
        async move { backend.rename_atlas(&atlas_id, &name).await }
    }

    /// Delete an atlas. Its member areas survive and become loose.
    ///
    /// # Errors
    /// Propagates the backend's delete error (owner-only; uniform 404
    /// otherwise).
    pub fn delete_atlas(&self, atlas_id: AtlasId) -> impl Future<Output = CloudResult<()>> {
        let backend = self.inner.backend.clone();
        async move { backend.delete_atlas(&atlas_id).await }
    }

    /// File an owned area into `atlas_id` (`Some`) or pull it loose
    /// (`None`). Optimistic: the cached area's atlas membership updates
    /// immediately so the folder regroups, with a fire-and-forget backend
    /// sync.
    pub fn move_area_to_atlas(&self, area_id: AreaId, atlas_id: Option<AtlasId>) {
        self.inner.move_area_to_atlas(area_id, atlas_id);
    }

    pub fn set_area_property(&self, area_id: AreaId, name: String, value: String) {
        self.inner.set_area_property(area_id, name, value);
    }

    pub fn delete_area_property(&self, area_id: AreaId, name: String) {
        self.inner.delete_area_property(area_id, name);
    }

    pub fn upsert_room(&self, room_key: RoomKey, updates: RoomUpdates) {
        self.inner.upsert_room(room_key, updates);
    }

    /// Upserts a batch of rooms in one cache update (one index rebuild).
    pub fn upsert_rooms(&self, area_id: AreaId, updates: Vec<(RoomNumber, RoomUpdates)>) {
        self.inner.upsert_rooms(area_id, updates);
    }

    pub fn delete_room(&self, room_key: RoomKey) {
        self.inner.delete_room(room_key);
    }

    pub fn set_room_property(&self, room_key: RoomKey, name: String, value: String) {
        self.inner.set_room_property(room_key, name, value);
    }

    pub fn delete_room_property(&self, room_key: RoomKey, name: String) {
        self.inner.delete_room_property(room_key, name);
    }

    pub fn add_room_tag(&self, room_key: RoomKey, tag: String) {
        self.inner.add_room_tag(room_key, tag);
    }

    pub fn remove_room_tag(&self, room_key: RoomKey, tag: String) {
        self.inner.remove_room_tag(room_key, tag);
    }

    pub fn update_exit(&self, room_key: RoomKey, exit_id: ExitId, updates: ExitUpdates) {
        self.inner.update_exit(room_key, exit_id, updates);
    }

    pub fn delete_exit(&self, room_key: RoomKey, exit_id: ExitId) {
        self.inner.delete_exit(room_key, exit_id);
    }

    pub fn create_exit(
        &self,
        room_key: RoomKey,
        args: ExitArgs,
    ) -> impl Future<Output = CloudResult<ExitId>> {
        self.inner.create_exit(room_key, args)
    }

    pub fn create_label(
        &self,
        area_id: AreaId,
        args: LabelArgs,
    ) -> impl Future<Output = CloudResult<LabelId>> {
        self.inner.create_label(area_id, args)
    }

    pub fn update_label(&self, area_id: AreaId, label_id: LabelId, updates: LabelUpdates) {
        self.inner.update_label(area_id, label_id, updates);
    }

    pub fn delete_label(&self, area_id: AreaId, label_id: LabelId) {
        self.inner.delete_label(area_id, label_id);
    }

    pub fn create_shape(
        &self,
        area_id: AreaId,
        args: ShapeArgs,
    ) -> impl Future<Output = CloudResult<ShapeId>> {
        self.inner.create_shape(area_id, args)
    }

    pub fn update_shape(&self, area_id: AreaId, shape_id: ShapeId, updates: ShapeUpdates) {
        self.inner.update_shape(area_id, shape_id, updates);
    }

    pub fn delete_shape(&self, area_id: AreaId, shape_id: ShapeId) {
        self.inner.delete_shape(area_id, shape_id);
    }

    pub fn wait_for_sync_completion(
        &self,
        timeout_secs: u64,
    ) -> impl Future<Output = Result<bool, ()>> {
        self.inner.wait_for_sync_completion(timeout_secs)
    }

    /// Whether the backend currently holds any credential; credential-less
    /// mappers serve cached data only and skip cloud loads.
    #[must_use]
    pub fn has_credential(&self) -> bool {
        self.inner.backend.has_credential()
    }

    /// Atlas ids served by a local (never-synced, on-disk) tier; empty for a
    /// pure-cloud mapper. Lets the UI gate cloud-only affordances (e.g. Share
    /// folder) off local folders.
    #[must_use]
    pub fn local_atlas_ids(&self) -> HashSet<AtlasId> {
        self.inner.backend.local_atlas_ids()
    }

    /// Area ids served by a local tier; empty for a pure-cloud mapper. Lets the
    /// UI keep cross-tier targets out of the move-to-folder picker.
    #[must_use]
    pub fn local_area_ids(&self) -> HashSet<AreaId> {
        self.inner.backend.local_area_ids()
    }

    #[must_use]
    pub fn get_sync_stats(&self) -> &SyncStats {
        self.inner.sync_stats()
    }
}

impl Inner {
    /// Get sync statistics for diagnostics
    #[must_use]
    pub fn sync_stats(&self) -> &SyncStats {
        &self.sync_stats
    }

    /// Wait for all sync operations to complete
    ///
    /// # Arguments
    /// * `timeout_secs` - Maximum time to wait in seconds (0 = no timeout)
    ///
    /// # Returns
    /// * `Ok(true)` if all operations completed successfully
    /// * `Ok(false)` if timeout was reached with pending operations
    /// * `Err(())` if there were failed operations
    ///
    /// # Errors
    /// Returns `Err(())` once the queue drains if any sync operation failed
    /// (the unit error simply signals "completed with failures").
    pub async fn wait_for_sync_completion(&self, timeout_secs: u64) -> Result<bool, ()> {
        let start_time = std::time::Instant::now();

        loop {
            let stats = &self.sync_stats;
            let pending = stats.pending_operations();
            let failed = stats.operations_failed();

            // Check if we're done
            if pending == 0 {
                return if failed > 0 { Err(()) } else { Ok(true) };
            }

            // Check for timeout
            if timeout_secs > 0 && start_time.elapsed().as_secs() >= timeout_secs {
                return Ok(false);
            }

            // Short sleep to avoid busy waiting
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
    }

    /// Load all areas from backend into cache
    /// # Errors
    /// Returns error if backend operations fail
    pub async fn load_all_areas(&self) -> CloudResult<LoadMapsSummary> {
        let list_start = Instant::now();
        let areas = self.backend.list_areas().await?;
        let list_duration = list_start.elapsed();

        let mut new_cache = HashMap::with_capacity(areas.len());
        let mut stats = Vec::with_capacity(areas.len());

        for area in areas {
            let load_start = Instant::now();
            match self.backend.get_area(&area.id).await {
                Ok(details) => {
                    let load_duration = load_start.elapsed();
                    let source = self.backend.last_area_source(&area.id);
                    // Classify from the list row (which carries the viewer-scoped access block
                    // and the owner handle); either signal flagging non-ownership means shared.
                    let shared = area.owner_nickname.is_some()
                        || area.access.is_some_and(|a| !a.is_owner);

                    stats.push(AreaLoadStat {
                        area_id: area.id,
                        name: details.area.name.clone(),
                        revision: details.area.rev,
                        load_duration,
                        source,
                        shared,
                    });

                    let cache = Arc::new(AreaCache::new_with_area(details));
                    new_cache.insert(area.id, cache);
                }
                Err(err) => {
                    warn!("Failed to load area {}: {err}", area.id);
                }
            }
        }

        // Carry every exclusion axis across the wholesale rebuild: a full
        // reload must never silently re-include areas the user disabled or that
        // per-server scoping excludes.
        let new_cache = Arc::new(self.atlas_cache.load().rebuild_with_areas(new_cache));

        self.atlas_cache.store(new_cache);

        // The wholesale store can race the sync engine (e.g. re-inserting an
        // area the engine removed between our list and store). Nudge the
        // engine: its next tick prunes anything the fresh row set no longer
        // covers, so any membership drift heals immediately.
        self.sync_notify.notify_one();

        Ok(LoadMapsSummary {
            list_duration,
            areas: stats,
        })
    }

    // === READ OPERATIONS (Instant, Lock-Free) ===

    #[must_use]
    pub fn get_current_atlas(&self) -> Arc<AtlasCache> {
        self.atlas_cache.load().clone()
    }

    /// Create a new area (waits for backend to assign ID)
    ///
    /// # Errors
    /// Returns an error if the backend rejects the request — auth/permission
    /// failures, a transport/HTTP error, or a server-side failure while
    /// creating the area.
    pub async fn create_area(&self, name: String) -> CloudResult<AreaId> {
        let atlas_id = Option::<&AtlasId>::cloned(self.atlas_id.load().as_ref().as_ref());
        self.create_area_in(name, atlas_id).await
    }

    /// Create a new area filed into an explicit atlas (or loose).
    ///
    /// # Errors
    /// Returns an error if the backend rejects the request — auth/permission
    /// failures, a transport/HTTP error, or a server-side failure while
    /// creating the area.
    pub async fn create_area_in(
        &self,
        name: String,
        atlas_id: Option<AtlasId>,
    ) -> CloudResult<AreaId> {
        let request = CreateAreaRequest {
            name,
            atlas_id,
            ephemeral: false,
        };
        self.create_area_from_request(request).await
    }

    /// Create an area in the ephemeral tier (see [`Mapper::create_area_ephemeral`]).
    ///
    /// # Errors
    /// Propagates the backend's create error.
    pub async fn create_area_ephemeral(&self, name: String) -> CloudResult<AreaId> {
        let request = CreateAreaRequest {
            name,
            atlas_id: None,
            ephemeral: true,
        };
        self.create_area_from_request(request).await
    }

    async fn create_area_from_request(&self, request: CreateAreaRequest) -> CloudResult<AreaId> {
        // Create area on backend first to get the real ID
        let backend_area = self.backend.create_area(request).await?;
        let area_id = backend_area.id;

        self.atlas_cache.rcu(|cache| {
            Arc::new(cache.add_area(
                area_id,
                Arc::new(AreaCache::new_with_area(AreaWithDetails {
                    area: backend_area.clone(),
                    content_hash: None,
                    properties: vec![],
                    rooms: vec![],
                    labels: vec![],
                    shapes: vec![],
                    linked_areas: vec![],
                })),
            ))
        });

        Ok(area_id)
    }

    /// Import a set of full areas into the LOCAL tier in one shot — the JSON-import fast path.
    ///
    /// Each area is given a fresh `AreaId` (and fresh exit/label/shape ids), so an import never
    /// collides with an existing area or another import. Cross-area exit targets that point *within
    /// the imported set* are remapped to the new ids; any pointing outside it are dropped (there is
    /// nothing to link to). All viewer/cloud metadata is reset to a locally-owned area. Persistence
    /// is one `store_area` per area and the atlas cache is rebuilt once for the whole set, so
    /// importing N rooms is O(N) rather than the O(N^2) of a per-room/per-exit replay.
    ///
    /// # Errors
    /// Propagates the local backend's persistence error.
    pub async fn import_areas(&self, mut areas: Vec<AreaWithDetails>) -> CloudResult<Vec<AreaId>> {
        if areas.is_empty() {
            return Ok(Vec::new());
        }

        // Mint every new id first, so cross-area exits can be remapped in the pass below.
        let mut id_map: HashMap<AreaId, AreaId> = HashMap::with_capacity(areas.len());
        for details in &areas {
            id_map.insert(details.area.id, AreaId(Uuid::new_v4()));
        }

        for details in &mut areas {
            details.area.id = id_map[&details.area.id];
            details.area.rev = 1;
            details.area.user_id = None;
            details.area.atlas_id = None;
            details.area.access = Some(AreaAccess::OWNER);
            details.area.owner_nickname = None;
            details.area.copied_from_area_id = None;
            details.area.copied_from_rev = None;
            details.area.copied_at = None;
            details.area.family_token = None;
            details.content_hash = None;
            details.linked_areas.clear();

            for label in &mut details.labels {
                label.id = LabelId(Uuid::new_v4());
                label.is_secret = false;
            }
            for shape in &mut details.shapes {
                shape.id = ShapeId(Uuid::new_v4());
                shape.is_secret = false;
            }
            for room in &mut details.rooms {
                room.is_secret = false;
                for exit in &mut room.exits {
                    exit.id = ExitId(Uuid::new_v4());
                    exit.is_secret = false;
                    exit.to_unknown = false;
                    exit.to_area_token = None;
                    exit.to_area_id = match exit.to_area_id {
                        Some(old) if id_map.contains_key(&old) => Some(id_map[&old]),
                        Some(_) => {
                            // Target is outside the imported set: drop the dangling cross-area link.
                            exit.to_room_number = None;
                            exit.to_direction = None;
                            None
                        }
                        None => None,
                    };
                }
            }
        }

        for details in &areas {
            self.backend.import_local_area(details.clone()).await?;
        }

        // Rebuild the atlas once for the whole set (one index build per area, no per-op churn).
        self.atlas_cache.rcu(|cache| {
            let mut next = cache.add_area(
                areas[0].area.id,
                Arc::new(AreaCache::new_with_area(areas[0].clone())),
            );
            for details in areas.iter().skip(1) {
                next = next.add_area(
                    details.area.id,
                    Arc::new(AreaCache::new_with_area(details.clone())),
                );
            }
            Arc::new(next)
        });

        Ok(areas.iter().map(|details| details.area.id).collect())
    }

    /// Serialize an area to its full [`AreaWithDetails`] — the JSON-export path. The bytes are the
    /// viewer-scoped, secret-redacted projection the backend already holds, so this can only ever
    /// emit what the viewer can see; the `can_copy` gate is enforced by the caller.
    ///
    /// # Errors
    /// Propagates the backend's read error.
    pub async fn export_area(&self, area_id: AreaId) -> CloudResult<AreaWithDetails> {
        self.backend.get_area(&area_id).await
    }

    /// The viewer's effective access for an area (owner-level for local/legacy areas), or `None`
    /// if the area isn't in the current atlas — used to gate export on `can_copy`.
    #[must_use]
    pub fn area_effective_access(&self, area_id: AreaId) -> Option<AreaAccess> {
        self.atlas_cache
            .load()
            .get_area(&area_id)
            .map(|area| area.effective_access())
    }

    pub fn delete_area(&self, area_id: AreaId) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id).map_or_else(|| cache.clone(), |_area| Arc::new(cache.delete_area(area_id)))
        });
        self.send_sync_operation(AreaSyncOperation::DeleteArea(area_id));
    }

    pub fn rename_area(&self, area_id: AreaId, name: &str) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id).map_or_else(|| cache.clone(), |area| {
                    Arc::new(cache.insert_area(area_id, Arc::new(area.rename(name))))
                })
        });
        self.send_sync_operation(AreaSyncOperation::RenameArea(area_id, name.to_string()));
    }

    pub fn move_area_to_atlas(&self, area_id: AreaId, atlas_id: Option<AtlasId>) {
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| Arc::new(cache.insert_area(area_id, Arc::new(area.with_atlas(atlas_id)))),
            )
        });
        self.send_sync_operation(AreaSyncOperation::MoveArea(area_id, atlas_id));
    }

    pub fn set_area_property(&self, area_id: AreaId, name: String, value: String) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id).map_or_else(|| cache.clone(), |area| {
                    Arc::new(cache.insert_area(
                        area_id,
                        Arc::new(area.set_property(name.clone(), value.clone())),
                    ))
                })
        });

        self.send_sync_operation(AreaSyncOperation::SetAreaProperty(area_id, name, value));
    }

    pub fn delete_area_property(&self, area_id: AreaId, name: String) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id).map_or_else(|| cache.clone(), |area| {
                    Arc::new(cache.insert_area(
                        area_id,
                        Arc::new(area.delete_property(name.as_str())),
                    ))
                })
        });

        self.send_sync_operation(AreaSyncOperation::DeleteAreaProperty(area_id, name));
    }

    /// Refuses room *creation* into an ephemeral area once the tier holds
    /// [`EPHEMERAL_ROOM_CAP`] rooms (updates to existing rooms always pass).
    /// The check runs before the optimistic cache write — the cache is where
    /// the memory lives, so a backend-side refusal would come too late.
    fn over_ephemeral_cap(&self, area_id: AreaId, new_rooms: &[RoomNumber]) -> bool {
        let ephemeral_ids = self.backend.ephemeral_area_ids();
        if !ephemeral_ids.contains(&area_id) {
            return false;
        }
        let cache = self.atlas_cache.load();
        let creating = cache.get_area(&area_id).map_or(new_rooms.len(), |area| {
            new_rooms
                .iter()
                .filter(|number| area.get_room(number).is_none())
                .count()
        });
        if creating == 0 {
            return false;
        }
        let total: usize = ephemeral_ids
            .iter()
            .filter_map(|id| cache.get_area(id))
            .map(|area| area.room_count())
            .sum();
        let over = total + creating > EPHEMERAL_ROOM_CAP;
        if over && !self.ephemeral_cap_warned.swap(true, Ordering::Relaxed) {
            warn!(
                "ephemeral map tier is at its {EPHEMERAL_ROOM_CAP}-room cap; \
                 further auto-mapped rooms are dropped (save and clear the session map to continue)"
            );
        }
        over
    }

    pub fn upsert_room(&self, room_key: RoomKey, updates: RoomUpdates) {
        if self.over_ephemeral_cap(room_key.area_id, std::slice::from_ref(&room_key.room_number)) {
            return;
        }
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&room_key.area_id).map_or_else(|| cache.clone(), |area| {
                    Arc::new(cache.insert_area(
                        *area.get_id(),
                        Arc::new(area.upsert_room(room_key.room_number, updates.clone())),
                    ))
                })
        });

        self.send_sync_operation(AreaSyncOperation::UpdateRoom(room_key, updates));
    }

    pub fn upsert_rooms(&self, area_id: AreaId, updates: Vec<(RoomNumber, RoomUpdates)>) {
        let numbers: Vec<RoomNumber> = updates.iter().map(|(number, _)| *number).collect();
        if self.over_ephemeral_cap(area_id, &numbers) {
            return;
        }
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(
                        cache
                            .insert_area(*area.get_id(), Arc::new(area.upsert_rooms(&updates))),
                    )
                },
            )
        });

        for (room_number, room_updates) in updates {
            self.send_sync_operation(AreaSyncOperation::UpdateRoom(
                RoomKey::new(area_id, room_number),
                room_updates,
            ));
        }
    }

    pub fn delete_room(&self, room_key: RoomKey) {
        self.atlas_cache.rcu(|cache| {
            // Drop the room from its own area and, in every loaded area
            // (including that one), null any exit that pointed at it. The
            // server cascades inbound-exit clearing on delete; mirroring it
            // here keeps the cache from showing exits dangling at the deleted
            // room until the next sync.
            let mut updated: Vec<(AreaId, Arc<AreaCache>)> = Vec::new();
            for area in cache.areas() {
                let area_id = *area.get_id();
                let reduced = (area_id == room_key.area_id)
                    .then(|| area.delete_room(room_key.room_number));
                let source = reduced.as_ref().unwrap_or(&*area);
                match source.null_inbound_exits(&room_key) {
                    Some(nulled) => updated.push((area_id, Arc::new(nulled))),
                    None => {
                        if let Some(reduced) = reduced {
                            updated.push((area_id, Arc::new(reduced)));
                        }
                    }
                }
            }

            if updated.is_empty() {
                cache.clone()
            } else {
                Arc::new(cache.with_areas_updated(updated))
            }
        });

        self.send_sync_operation(AreaSyncOperation::DeleteRoom(room_key));
    }

    pub fn set_room_property(&self, room_key: RoomKey, name: String, value: String) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&room_key.area_id)
                .and_then(|area| {
                    area.set_room_property(room_key.room_number, name.clone(), value.clone())
                        .ok()
                }).map_or_else(|| cache.clone(), |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))))
        });

        self.send_sync_operation(AreaSyncOperation::SetRoomProperty(room_key, name, value));
    }

    pub fn delete_room_property(&self, room_key: RoomKey, name: String) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&room_key.area_id)
                .and_then(|area| {
                    area.delete_room_property(room_key.room_number, name.as_str())
                        .ok()
                }).map_or_else(|| cache.clone(), |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))))
        });

        self.send_sync_operation(AreaSyncOperation::DeleteRoomProperty(room_key, name));
    }

    pub fn add_room_tag(&self, room_key: RoomKey, tag: String) {
        let tag = normalize_tag(&tag);
        if tag.is_empty() {
            return;
        }

        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&room_key.area_id)
                .and_then(|area| area.add_room_tag(room_key.room_number, &tag).ok())
                .map_or_else(|| cache.clone(), |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))))
        });

        self.send_sync_operation(AreaSyncOperation::AddRoomTag(room_key, tag));
    }

    pub fn remove_room_tag(&self, room_key: RoomKey, tag: String) {
        let tag = normalize_tag(&tag);
        if tag.is_empty() {
            return;
        }

        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&room_key.area_id)
                .and_then(|area| area.remove_room_tag(room_key.room_number, &tag).ok())
                .map_or_else(|| cache.clone(), |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))))
        });

        self.send_sync_operation(AreaSyncOperation::RemoveRoomTag(room_key, tag));
    }

    pub fn update_exit(&self, room_key: RoomKey, exit_id: ExitId, updates: ExitUpdates) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&room_key.area_id)
                .and_then(|area| {
                    area.get_room(&room_key.room_number)
                        .and_then(|room| room.get_exits().iter().find(|e| e.id == exit_id))
                        .map(|exit| (area.clone(), updates.clone().apply(exit)))
                })
                .and_then(|(area, new_exit)| area.upsert_exit(room_key.room_number, new_exit).ok()).map_or_else(|| cache.clone(), |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))))
        });

        self.send_sync_operation(AreaSyncOperation::UpdateExit(
            room_key.area_id,
            exit_id,
            updates,
        ));
    }

    pub fn delete_exit(&self, room_key: RoomKey, exit_id: ExitId) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&room_key.area_id)
                .and_then(|area| area.delete_exit(room_key.room_number, exit_id).ok()).map_or_else(|| cache.clone(), |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))))
        });

        self.send_sync_operation(AreaSyncOperation::DeleteExit(room_key.area_id, exit_id));
    }
    // === SLOW CREATE OPERATIONS (Wait for Backend ID) ===

    /// Create exit (waits for backend to assign ID)
    /// # Errors
    /// Returns error if backend operations fail
    pub async fn create_exit(&self, room_key: RoomKey, args: ExitArgs) -> CloudResult<ExitId> {
        // Create on backend first to get the real ID and data
        let backend_exit = self
            .backend
            .create_room_exit(&room_key, args.clone())
            .await?;
        let exit_id = backend_exit.id;

        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&room_key.area_id)
                .and_then(|area| {
                    area.upsert_exit(room_key.room_number, backend_exit.clone().into())
                        .ok()
                }).map_or_else(|| cache.clone(), |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))))
        });

        Ok(exit_id)
    }

    /// Create label (waits for backend to assign ID)
    /// # Errors
    /// Returns error if backend operations fail
    pub async fn create_label(&self, area_id: AreaId, args: LabelArgs) -> CloudResult<LabelId> {
        // Create on backend first to get the real ID and data
        let backend_label = self.backend.create_label(&area_id, args).await?;
        let label_id = backend_label.id;

        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id).map_or_else(|| cache.clone(), |area| {
                    Arc::new(cache.insert_area(
                        *area.get_id(),
                        Arc::new(area.upsert_label(label_id, backend_label.clone())),
                    ))
                })
        });

        Ok(label_id)
    }

    /// Create shape (waits for backend to assign ID)
    /// # Errors
    /// Returns error if backend operations fail
    pub async fn create_shape(&self, area_id: AreaId, args: ShapeArgs) -> CloudResult<ShapeId> {
        // Create on backend first to get the real ID and data
        let backend_shape = self.backend.create_shape(&area_id, args).await?;
        let shape_id = backend_shape.id;

        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id).map_or_else(|| cache.clone(), |area| {
                    Arc::new(cache.insert_area(
                        *area.get_id(),
                        Arc::new(area.upsert_shape(shape_id, backend_shape.clone())),
                    ))
                })
        });

        Ok(shape_id)
    }

    pub fn update_label(&self, area_id: AreaId, label_id: LabelId, updates: LabelUpdates) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| {
                    let new_label = area
                        .get_label(&label_id)
                        .map(|label| updates.clone().apply(label))?;
                    Some((area, new_label))
                })
                .map_or_else(
                    || cache.clone(),
                    |(area, new_label)| {
                        Arc::new(cache.insert_area(
                            *area.get_id(),
                            Arc::new(area.upsert_label(label_id, new_label)),
                        ))
                    },
                )
        });

        self.send_sync_operation(AreaSyncOperation::UpdateLabel(area_id, label_id, updates));
    }

    pub fn delete_label(&self, area_id: AreaId, label_id: LabelId) {
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(
                        cache.insert_area(*area.get_id(), Arc::new(area.delete_label(label_id))),
                    )
                },
            )
        });

        self.send_sync_operation(AreaSyncOperation::DeleteLabel(area_id, label_id));
    }

    pub fn update_shape(&self, area_id: AreaId, shape_id: ShapeId, updates: ShapeUpdates) {
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| {
                    let new_shape = area
                        .get_shape(&shape_id)
                        .map(|shape| updates.clone().apply(shape))?;
                    Some((area, new_shape))
                })
                .map_or_else(
                    || cache.clone(),
                    |(area, new_shape)| {
                        Arc::new(cache.insert_area(
                            *area.get_id(),
                            Arc::new(area.upsert_shape(shape_id, new_shape)),
                        ))
                    },
                )
        });

        self.send_sync_operation(AreaSyncOperation::UpdateShape(area_id, shape_id, updates));
    }

    pub fn delete_shape(&self, area_id: AreaId, shape_id: ShapeId) {
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(
                        cache.insert_area(*area.get_id(), Arc::new(area.delete_shape(shape_id))),
                    )
                },
            )
        });

        self.send_sync_operation(AreaSyncOperation::DeleteShape(area_id, shape_id));
    }

    pub fn get_selected_atlas_id(&self) -> Option<Uuid> {
        None
    }

    // === INTERNAL SYNC HELPERS ===

    /// Send sync operation with tracking
    fn send_sync_operation(&self, operation: AreaSyncOperation) {
        self.sync_stats
            .operations_sent
            .fetch_add(1, Ordering::Relaxed);

        let area_id = operation.area_id();
        {
            let mut pending = self.pending_by_area.lock();
            *pending.entry(area_id).or_insert(0) += 1;
        }

        if let Err(e) = self.sync_sender.send(operation) {
            self.sync_stats
                .operations_failed
                .fetch_add(1, Ordering::Relaxed);
            Self::decrement_pending(&self.pending_by_area, area_id);
            warn!("Failed to send sync operation: {e}");
        }
    }

    /// Drops one in-flight write marker for `area_id`, removing the entry
    /// once the count reaches zero.
    fn decrement_pending(pending_by_area: &Mutex<HashMap<AreaId, u64>>, area_id: AreaId) {
        let mut pending = pending_by_area.lock();
        if let Some(count) = pending.get_mut(&area_id) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                pending.remove(&area_id);
            }
        }
    }

    // === INDEX MANAGEMENT ===

    /// Spawn background sync task
    fn spawn_sync_task(
        &self,
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<AreaSyncOperation>,
        stats: Arc<SyncStats>,
    ) -> JoinHandle<()> {
        let backend = self.backend.clone();
        let pending_by_area = self.pending_by_area.clone();

        tokio::spawn(async move {
            while let Some(operation) = receiver.recv().await {
                let area_id = operation.area_id();
                let result = Self::handle_sync_operation(&*backend, operation).await;
                // Decrement before the stats update so observing "no pending
                // operations" implies the per-area counters are settled too.
                Self::decrement_pending(&pending_by_area, area_id);
                match result {
                    Ok(()) => {
                        stats.operations_succeeded.fetch_add(1, Ordering::Relaxed);
                    }
                    Err(e) => {
                        stats.operations_failed.fetch_add(1, Ordering::Relaxed);
                        warn!("Failed to handle sync operation: {e}");
                    }
                }
            }
        })
    }

    /// Handle individual sync operations
    async fn handle_sync_operation(
        backend: &dyn MapperBackend,
        operation: AreaSyncOperation,
    ) -> CloudResult<()> {
        match operation {
            AreaSyncOperation::RenameArea(area_id, name) => {
                backend
                    .update_area(
                        &area_id,
                        AreaUpdates {
                            name: Some(name),
                            atlas_id: None,
                        },
                    )
                    .await?;
                Ok(())
            }
            AreaSyncOperation::MoveArea(area_id, atlas_id) => {
                backend.move_area_to_atlas(&area_id, atlas_id).await?;
                Ok(())
            }
            AreaSyncOperation::SetAreaProperty(area_id, name, value) => {
                backend.set_area_property(&area_id, &name, &value).await?;
                Ok(())
            }
            AreaSyncOperation::UpdateRoom(room_key, updates) => {
                backend.update_room(&room_key, updates).await?;
                Ok(())
            }
            AreaSyncOperation::SetRoomProperty(room_key, name, value) => {
                backend.set_room_property(&room_key, &name, &value).await?;
                Ok(())
            }
            AreaSyncOperation::DeleteArea(area_id) => {
                backend.delete_area(&area_id).await?;
                Ok(())
            }
            AreaSyncOperation::DeleteAreaProperty(area_id, name) => {
                backend.delete_area_property(&area_id, &name).await?;
                Ok(())
            }
            AreaSyncOperation::DeleteRoom(room_key) => {
                backend.delete_room(&room_key).await?;
                Ok(())
            }
            AreaSyncOperation::DeleteRoomProperty(room_key, name) => {
                backend.delete_room_property(&room_key, &name).await?;
                Ok(())
            }
            AreaSyncOperation::AddRoomTag(room_key, tag) => {
                backend.add_room_tag(&room_key, &tag).await?;
                Ok(())
            }
            AreaSyncOperation::RemoveRoomTag(room_key, tag) => {
                backend.remove_room_tag(&room_key, &tag).await?;
                Ok(())
            }
            AreaSyncOperation::UpdateExit(area_id, exit_id, updates) => {
                backend.update_exit(&area_id, &exit_id, updates).await?;
                Ok(())
            }
            AreaSyncOperation::DeleteExit(area_id, exit_id) => {
                backend.delete_exit(&area_id, &exit_id).await?;
                Ok(())
            }
            AreaSyncOperation::UpdateLabel(area_id, label_id, updates) => {
                backend.update_label(&area_id, &label_id, updates).await?;
                Ok(())
            }
            AreaSyncOperation::DeleteLabel(area_id, label_id) => {
                backend.delete_label(&area_id, &label_id).await?;
                Ok(())
            }
            AreaSyncOperation::UpdateShape(area_id, shape_id, updates) => {
                backend.update_shape(&area_id, &shape_id, updates).await?;
                Ok(())
            }
            AreaSyncOperation::DeleteShape(area_id, shape_id) => {
                backend.delete_shape(&area_id, &shape_id).await?;
                Ok(())
            }
        }
    }
}

impl Mapper {
    /// Optimistically mirrors a `POST /areas/{id}/secret-marks` change onto
    /// the cached atlas: flips `is_secret` on the referenced entities and
    /// bumps the area rev by one (like other local edits, so open editors
    /// notice and resync their inspectors).
    ///
    /// No sync operation is enqueued — the server already owns the change;
    /// its bumped rev arrives through the sync engine (callers typically
    /// follow a successful POST with [`Self::sync_now`]).
    #[allow(clippy::too_many_arguments)]
    pub fn apply_local_secret_marks(
        &self,
        area_id: AreaId,
        secret: bool,
        rooms: &[RoomNumber],
        exits: &[ExitId],
        labels: &[LabelId],
        shapes: &[ShapeId],
        room_properties: &[(RoomNumber, String)],
        area_properties: &[String],
    ) {
        self.inner.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(cache.insert_area(
                        *area.get_id(),
                        Arc::new(area.apply_secret_marks(
                            secret,
                            rooms,
                            exits,
                            labels,
                            shapes,
                            room_properties,
                            area_properties,
                        )),
                    ))
                },
            )
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Area, AreaAccess, AreaWithDetails, CreateAreaRequest, Exit, Label, CloudError, Room,
        RoomWithDetails, Shape,
    };
    use async_trait::async_trait;
    use chrono::Utc;

    /// Read-only backend serving a fixed set of areas; local write syncs are
    /// accepted (or fail harmlessly) — enough for cache-behavior tests.
    struct FixedBackend {
        areas: HashMap<AreaId, AreaWithDetails>,
    }

    impl FixedBackend {
        fn new(areas: Vec<AreaWithDetails>) -> Self {
            Self {
                areas: areas.into_iter().map(|a| (a.area.id, a)).collect(),
            }
        }
    }

    #[async_trait]
    impl MapperBackend for FixedBackend {
        async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
            Err(CloudError::NetworkError("read-only".to_string()))
        }

        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            Ok(self.areas.values().map(|a| a.area.clone()).collect())
        }

        async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            self.areas
                .get(area_id)
                .cloned()
                .ok_or(CloudError::NotFoundOrNoAccess)
        }

        async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_area(&self, _area_id: &AreaId) -> CloudResult<()> {
            Ok(())
        }

        async fn set_area_property(
            &self,
            _area_id: &AreaId,
            _name: &str,
            _value: &str,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_area_property(&self, _area_id: &AreaId, _name: &str) -> CloudResult<()> {
            Ok(())
        }

        async fn update_room(&self, room_key: &RoomKey, _updates: RoomUpdates) -> CloudResult<Room> {
            Err(CloudError::RoomNotFound(room_key.clone()))
        }

        async fn delete_room(&self, _room_key: &RoomKey) -> CloudResult<()> {
            Ok(())
        }

        async fn set_room_property(
            &self,
            _room_key: &RoomKey,
            _name: &str,
            _value: &str,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_room_property(&self, _room_key: &RoomKey, _name: &str) -> CloudResult<()> {
            Ok(())
        }

        async fn add_room_tag(&self, _room_key: &RoomKey, _tag: &str) -> CloudResult<()> {
            Ok(())
        }

        async fn remove_room_tag(&self, _room_key: &RoomKey, _tag: &str) -> CloudResult<()> {
            Ok(())
        }

        async fn create_room_exit(
            &self,
            _room_key: &RoomKey,
            _exit_data: ExitArgs,
        ) -> CloudResult<Exit> {
            Err(CloudError::NetworkError("read-only".to_string()))
        }

        async fn update_exit(
            &self,
            _area_id: &AreaId,
            _exit_id: &ExitId,
            _updates: ExitUpdates,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_exit(&self, _area_id: &AreaId, _exit_id: &ExitId) -> CloudResult<()> {
            Ok(())
        }

        async fn create_label(
            &self,
            _area_id: &AreaId,
            _label_data: LabelArgs,
        ) -> CloudResult<Label> {
            Err(CloudError::NetworkError("read-only".to_string()))
        }

        async fn update_label(
            &self,
            _area_id: &AreaId,
            _label_id: &LabelId,
            _updates: LabelUpdates,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_label(&self, _area_id: &AreaId, _label_id: &LabelId) -> CloudResult<()> {
            Ok(())
        }

        async fn create_shape(
            &self,
            _area_id: &AreaId,
            _shape_data: ShapeArgs,
        ) -> CloudResult<Shape> {
            Err(CloudError::NetworkError("read-only".to_string()))
        }

        async fn update_shape(
            &self,
            _area_id: &AreaId,
            _shape_id: &ShapeId,
            _updates: ShapeUpdates,
        ) -> CloudResult<()> {
            Ok(())
        }

        async fn delete_shape(&self, _area_id: &AreaId, _shape_id: &ShapeId) -> CloudResult<()> {
            Ok(())
        }
    }

    fn sample_area(area_id: AreaId, room_title: &str) -> AreaWithDetails {
        AreaWithDetails {
            area: Area {
                id: area_id,
                user_id: None,
                atlas_id: None,
                name: format!("area {area_id}"),
                created_at: Utc::now(),
                rev: 1,
                access: Some(AreaAccess::OWNER),
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                atlas_name: None,
            },
            content_hash: None,
            properties: vec![],
            rooms: vec![RoomWithDetails {
                room_number: RoomNumber(1),
                title: room_title.to_string(),
                description: String::new(),
                level: 0,
                x: 0.0,
                y: 0.0,
                color: String::new(),
                properties: vec![],
                exits: vec![],
                tags: Default::default(),
                is_secret: false,
                external_id: None,
            }],
            labels: vec![],
            shapes: vec![],
            linked_areas: vec![],
        }
    }

    fn temp_cache_dir() -> PathBuf {
        std::env::temp_dir().join(format!("smudgy-mapper-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn disabled_set_survives_unrelated_mutation() {
        let a_id = AreaId(Uuid::new_v4());
        let b_id = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![
            sample_area(a_id, "Plaza"),
            sample_area(b_id, "Plaza"),
        ]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        mapper.set_area_enabled(b_id, false);
        assert!(!mapper.is_area_enabled(&b_id));

        // An unrelated mutation rebuilds the cache; the disabled set must
        // ride through instead of silently re-enabling the area.
        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(2)),
            RoomUpdates {
                title: Some("Annex".to_string()),
                ..RoomUpdates::default()
            },
        );

        let atlas = mapper.get_current_atlas();
        assert!(!atlas.is_area_enabled(&b_id));
        assert!(atlas.is_area_enabled(&a_id));
        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Plaza")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![a_id]);
        assert!(
            atlas.get_room(&RoomKey::new(a_id, RoomNumber(2))).is_some(),
            "the unrelated mutation itself must land"
        );
    }

    #[tokio::test]
    async fn disabling_unknown_area_is_harmless_and_survives_full_reload() {
        let a_id = AreaId(Uuid::new_v4());
        let phantom = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![sample_area(a_id, "Plaza")]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());

        // Disable an area the cache has never seen, before anything loads.
        mapper.set_disabled_areas(std::iter::once(phantom).collect());
        assert!(mapper.disabled_areas().contains(&phantom));
        assert!(!mapper.is_area_enabled(&phantom));

        mapper.load_all_areas().await.expect("load");

        assert!(
            mapper.disabled_areas().contains(&phantom),
            "wholesale reload must preserve the disabled set"
        );
        assert!(mapper.is_area_enabled(&a_id));

        // Toggling back removes it.
        mapper.set_area_enabled(phantom, true);
        assert!(mapper.disabled_areas().is_empty());
    }

    fn exit_to(id: u128, to_area: AreaId, to_room: i32) -> Exit {
        Exit {
            id: ExitId(Uuid::from_u128(id)),
            from_direction: crate::ExitDirection::North,
            to_area_id: Some(to_area),
            to_room_number: Some(RoomNumber(to_room)),
            to_direction: Some(crate::ExitDirection::South),
            path: String::new(),
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: String::new(),
            style: crate::ExitStyle::Normal,
            color: String::new(),
            to_unknown: false,
            to_area_token: None,
            is_secret: false,
        }
    }

    fn room_with_exits(number: i32, exits: Vec<Exit>) -> RoomWithDetails {
        RoomWithDetails {
            room_number: RoomNumber(number),
            title: format!("room {number}"),
            description: String::new(),
            level: 0,
            x: 0.0,
            y: 0.0,
            color: String::new(),
            properties: vec![],
            exits,
            tags: Default::default(),
            is_secret: false,
            external_id: None,
        }
    }

    fn area_with_rooms(area_id: AreaId, rooms: Vec<RoomWithDetails>) -> AreaWithDetails {
        AreaWithDetails {
            area: Area {
                id: area_id,
                user_id: None,
                atlas_id: None,
                name: format!("area {area_id}"),
                created_at: Utc::now(),
                rev: 1,
                access: Some(AreaAccess::OWNER),
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                atlas_name: None,
            },
            content_hash: None,
            properties: vec![],
            rooms,
            labels: vec![],
            shapes: vec![],
            linked_areas: vec![],
        }
    }

    #[tokio::test]
    async fn delete_room_clears_inbound_exits_across_areas() {
        let a_id = AreaId(Uuid::new_v4());
        let b_id = AreaId(Uuid::new_v4());

        // A:1 --(north)--> A:2 (same area) and A:1 --> B:5 (cross area, an
        // unrelated link). B:5 --> A:2 (cross-area inbound to the victim).
        let a = area_with_rooms(
            a_id,
            vec![
                room_with_exits(1, vec![exit_to(1, a_id, 2), exit_to(2, b_id, 5)]),
                room_with_exits(2, vec![]),
            ],
        );
        let b = area_with_rooms(b_id, vec![room_with_exits(5, vec![exit_to(3, a_id, 2)])]);

        let backend = FixedBackend::new(vec![a, b]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Delete A:2 — every exit pointing at it must lose its destination.
        mapper.delete_room(RoomKey::new(a_id, RoomNumber(2)));

        let atlas = mapper.get_current_atlas();
        assert!(
            atlas.get_room(&RoomKey::new(a_id, RoomNumber(2))).is_none(),
            "room removed"
        );

        let find = |room: &Arc<crate::mapper::room_cache::RoomCache>, id: u128| {
            room.get_exits()
                .iter()
                .find(|e| e.id == ExitId(Uuid::from_u128(id)))
                .expect("exit present")
                .clone()
        };

        // Same-area inbound (A:1 -> A:2) is cleared, destination and direction.
        let a1 = atlas
            .get_room(&RoomKey::new(a_id, RoomNumber(1)))
            .expect("A:1");
        let cleared = find(&a1, 1);
        assert_eq!(cleared.to_area_id, None);
        assert_eq!(cleared.to_room_number, None);
        assert_eq!(cleared.to_direction, None);

        // The unrelated exit (A:1 -> B:5) is untouched.
        let untouched = find(&a1, 2);
        assert_eq!(untouched.to_area_id, Some(b_id));
        assert_eq!(untouched.to_room_number, Some(RoomNumber(5)));

        // Cross-area inbound (B:5 -> A:2) is cleared too.
        let b5 = atlas
            .get_room(&RoomKey::new(b_id, RoomNumber(5)))
            .expect("B:5");
        let cross = find(&b5, 3);
        assert_eq!(cross.to_area_id, None);
        assert_eq!(cross.to_room_number, None);
    }

    #[tokio::test]
    async fn scope_exclusion_hides_area_without_touching_the_manual_axis() {
        let a_id = AreaId(Uuid::new_v4());
        let b_id = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![
            sample_area(a_id, "Midgaard"),
            sample_area(b_id, "Midgaard"),
        ]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Scope-exclude B (by area id, since these sample areas are atlas-less).
        mapper.set_scope_exclusions(HashSet::new(), std::iter::once(b_id).collect());

        let atlas = mapper.get_current_atlas();
        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Midgaard")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![a_id], "the scope-excluded stock zone drops out");

        // The manual axis is untouched: B is still "enabled", disabled set empty.
        assert!(mapper.is_area_enabled(&b_id));
        assert!(mapper.disabled_areas().is_empty());
        assert!(!mapper.is_area_included(&b_id), "but it no longer participates");
        assert!(mapper.is_area_included(&a_id));

        // Scope exclusion survives an unrelated mutation (cache rebuild).
        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(9)),
            RoomUpdates {
                title: Some("Annex".to_string()),
                ..RoomUpdates::default()
            },
        );
        assert!(!mapper.get_current_atlas().is_area_included(&b_id));
    }

    #[tokio::test]
    async fn ephemeral_area_survives_scope_exclusion_of_everything_else() {
        use crate::backends::EphemeralBackend;

        let mapper = Mapper::new(Arc::new(EphemeralBackend::new()), temp_cache_dir());
        let area_id = mapper
            .create_area_ephemeral("Session".to_string())
            .await
            .expect("create ephemeral");
        mapper.upsert_room(
            RoomKey::new(area_id, RoomNumber(1)),
            RoomUpdates {
                title: Some("Wilderness".to_string()),
                ..RoomUpdates::default()
            },
        );

        // Only cloud atlas/area ids ever enter the scope store, so excluding a
        // pile of arbitrary cloud ids can never touch a session-tier area.
        mapper.set_scope_exclusions(
            std::iter::once(AtlasId(Uuid::new_v4())).collect(),
            std::iter::once(AreaId(Uuid::new_v4())).collect(),
        );

        assert!(mapper.is_area_included(&area_id), "ephemeral areas are never scope-excluded");
        let by_title: Vec<AreaId> = mapper
            .get_current_atlas()
            .get_rooms_by_title("Wilderness")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![area_id]);
    }
}
