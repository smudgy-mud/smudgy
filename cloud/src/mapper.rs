use crate::backends::{MapperBackend, area_edits};
use crate::error::CloudResult;
use crate::mapper::area_cache::AreaCache;
use crate::mutation::{
    AreaMutation, MAX_MUTATION_OPERATIONS, MutationEnvelope, OperationId, Precondition,
    ResourceKind,
};
use crate::{
    Area, AreaAccess, AreaId, AreaUpdates, AreaWithDetails, Atlas, AtlasId, AtlasListItem,
    CloudError, CreateAreaRequest, ExitArgs, ExitId, ExitUpdates, LabelArgs, LabelId, LabelUpdates,
    RoomNumber, RoomUpdates, ShapeArgs, ShapeId, ShapeUpdates,
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
pub mod pending;
pub mod room_cache;
pub mod room_connection;
pub mod sync_engine;
pub use atlas_cache::{AtlasCache, ElsewhereMatch};
pub use pending::{AreaSaveStatus, MapperEvent};
pub use sync_engine::{SyncState, SyncStatus};

use pending::{PendingEnvelope, PendingQueue, StructuralPrecondition, TransportVerdict};

/// How a display rebuild folds a pending envelope that no longer applies
/// to the fresh confirmed projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplayMode {
    /// Stop at the first failing envelope: it and everything after it stay
    /// pending but undisplayed. The conflict-detection fold — the caller
    /// pauses the queue targeting the reported envelope.
    StopAtFailure,
    /// Skip failing envelopes and keep folding the rest — the best-effort
    /// display fold for sync refetches. A skipped envelope stays
    /// pending and still goes to the server, whose verdict is
    /// authoritative.
    SkipFailures,
    /// Ignore client-only create/update preconditions while still applying
    /// the operations through the shared applier. Used only after an
    /// explicit Keep-mine decision.
    KeepMine,
}

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

/// One area document as the JSON import surface accepts it (§8.4),
/// dispatched on `format_version` at deserialization:
///
/// - **absent or 1** — parsed through the explicit
///   [`crate::backends::local_migration::LegacyAreaV1`] DTO and migrated by
///   [`crate::backends::local_migration::migrate_v1`] (which reports
///   reciprocal-looking pairs that stayed one-way through the log channel);
/// - **2** — taken verbatim ([`Mapper::import_areas`] still runs the
///   invariant checks before any write);
/// - **newer** — rejected outright, without a partial import.
///
/// The v2 types themselves never tolerate v1 input; this wrapper is the one
/// place the two formats meet.
#[derive(Debug, Clone)]
pub struct AreaImportDocument(pub AreaWithDetails);

impl AreaImportDocument {
    #[must_use]
    pub fn into_inner(self) -> AreaWithDetails {
        self.0
    }
}

impl<'de> serde::Deserialize<'de> for AreaImportDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        let value = serde_json::Value::deserialize(deserializer)?;
        let version = value
            .get("format_version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        match version {
            1 => {
                let legacy: crate::backends::local_migration::LegacyAreaV1 =
                    serde_json::from_value(value).map_err(D::Error::custom)?;
                Ok(Self(crate::backends::local_migration::migrate_v1(legacy)))
            }
            2 => serde_json::from_value(value)
                .map(Self)
                .map_err(D::Error::custom),
            newer => Err(D::Error::custom(format!(
                "area document format v{newer} is newer than this client \
                 (max v{}); refusing the import",
                crate::AREA_FORMAT_VERSION
            ))),
        }
    }
}

/// The §8.4 v2 import invariants, checked before any write: every exit's
/// `connection_id` resolves in the document's `connections`, and every
/// Connection has one or two member exits. A violation rejects the whole
/// import.
fn validate_import_document(details: &AreaWithDetails) -> CloudResult<()> {
    let mut members: HashMap<crate::ConnectionId, u32> = details
        .connections
        .iter()
        .map(|connection| (connection.id, 0))
        .collect();
    for room in &details.rooms {
        for exit in &room.exits {
            let Some(count) = members.get_mut(&exit.connection_id) else {
                return Err(CloudError::InvalidInput(format!(
                    "import of area {} ({}) rejected: exit {} references connection {}, \
                     which is not in the document",
                    details.area.name, details.area.id, exit.id, exit.connection_id
                )));
            };
            *count += 1;
        }
    }
    if let Some((id, count)) = members.iter().find(|(_, count)| !(1..=2).contains(*count)) {
        return Err(CloudError::InvalidInput(format!(
            "import of area {} ({}) rejected: connection {id} has {count} member exits \
             (a Connection has one or two)",
            details.area.name, details.area.id
        )));
    }
    Ok(())
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

/// The whole-area lifecycle operations that stay on the legacy
/// fire-and-forget sync channel. Content mutations never appear here — they
/// compile to CAS envelopes and travel through the pending queue instead.
#[derive(Debug)]
enum AreaSyncOperation {
    Rename(AreaId, String),
    Move(AreaId, Option<AtlasId>),
    Delete(AreaId),
}

impl AreaSyncOperation {
    /// The area this operation mutates; used to defer sync-engine refetches
    /// while local writes are still in flight.
    fn area_id(&self) -> AreaId {
        match self {
            Self::Rename(area_id, _) | Self::Move(area_id, _) | Self::Delete(area_id) => *area_id,
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

    /// The CAS pending-write store: every content mutation waits here as an
    /// envelope until the backend acknowledges it (see [`mapper::pending`]).
    ///
    /// [`mapper::pending`]: crate::mapper::pending
    pending: Arc<PendingQueue>,

    /// One teaching warning when the ephemeral room cap refuses a creation.
    ephemeral_cap_warned: AtomicBool,

    /// Initial-load gate for presence-checked imports: `None` until the first
    /// [`Inner::load_all_areas`] completes, then whether it succeeded.
    initial_load: tokio::sync::watch::Sender<Option<bool>>,
    /// Serializes presence-checked imports so two concurrent seeds cannot
    /// both miss (and then both import) the same area name.
    import_gate: tokio::sync::Mutex<()>,
}

/// The outcome of a presence-checked import: what was added and what was
/// already present (by name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AreasImportedIfAbsent {
    /// Ids of the areas imported by this call.
    pub added: Vec<AreaId>,
    /// Names skipped because a resident area already bears them.
    pub skipped: Vec<String>,
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
            pending: Arc::new(PendingQueue::new()),
            ephemeral_cap_warned: AtomicBool::new(false),
            initial_load: tokio::sync::watch::channel(None).0,
            import_gate: tokio::sync::Mutex::new(()),
        };

        inner.spawn_sync_task(sync_receiver, inner.sync_stats.clone());

        let mapper = Self {
            inner: Arc::new(inner),
        };

        Inner::spawn_mutation_worker(&mapper.inner);

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
        self.inner
            .atlas_cache
            .rcu(|cache| Arc::new(cache.with_scope_exclusions(atlases.clone(), areas.clone())));
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
    pub fn create_area_ephemeral(&self, name: String) -> impl Future<Output = CloudResult<AreaId>> {
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

    /// Presence-checked variant of [`Self::import_areas`]: imports only the areas whose name no
    /// resident area already bears, and reports the rest as skipped. See
    /// [`Inner::import_areas_if_absent`] for the exact contract (initial-load gate, unfiltered
    /// presence check, serialization against concurrent seeds).
    ///
    /// # Errors
    /// Errors when the initial area load failed or the local backend's persistence fails.
    pub async fn import_areas_if_absent(
        &self,
        areas: Vec<AreaWithDetails>,
    ) -> CloudResult<AreasImportedIfAbsent> {
        self.inner.import_areas_if_absent(areas).await
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

    /// Applies an ordered compound edit to the optimistic cache and enqueues
    /// it as one CAS mutation/undo unit.
    ///
    /// # Errors
    /// Returns the shared local validation error without changing the cache
    /// or queue when the proposed final Connection graph is invalid.
    pub fn mutate_area(
        &self,
        area_id: AreaId,
        operations: Vec<AreaMutation>,
        description: impl Into<String>,
    ) -> CloudResult<OperationId> {
        self.inner
            .mutate_area(area_id, operations, description.into())
    }

    /// Creates an exit with a client-minted id: the cache updates
    /// optimistically and the envelope queues for the backend, so the
    /// returned future is already resolved (the future-shaped signature is
    /// retained for call-site stability).
    pub fn create_exit(
        &self,
        room_key: RoomKey,
        args: ExitArgs,
    ) -> impl Future<Output = CloudResult<ExitId>> {
        std::future::ready(self.inner.create_exit(room_key, args))
    }

    /// Creates a label with a client-minted id; immediately resolved like
    /// [`Self::create_exit`].
    pub fn create_label(
        &self,
        area_id: AreaId,
        args: LabelArgs,
    ) -> impl Future<Output = CloudResult<LabelId>> {
        std::future::ready(self.inner.create_label(area_id, args))
    }

    pub fn update_label(&self, area_id: AreaId, label_id: LabelId, updates: LabelUpdates) {
        self.inner.update_label(area_id, label_id, updates);
    }

    pub fn delete_label(&self, area_id: AreaId, label_id: LabelId) {
        self.inner.delete_label(area_id, label_id);
    }

    /// Creates a shape with a client-minted id; immediately resolved like
    /// [`Self::create_exit`].
    pub fn create_shape(
        &self,
        area_id: AreaId,
        args: ShapeArgs,
    ) -> impl Future<Output = CloudResult<ShapeId>> {
        std::future::ready(self.inner.create_shape(area_id, args))
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

    // === PENDING-QUEUE SURFACE ===

    /// Subscribe to pending-queue lifecycle events: acknowledgements,
    /// conflicts, permanent failures, per-area save-status changes, and
    /// upgrade pauses.
    #[must_use]
    pub fn subscribe_mapper_events(&self) -> tokio::sync::broadcast::Receiver<MapperEvent> {
        self.inner.pending.subscribe()
    }

    /// The area-specific save status derived from its pending queue.
    #[must_use]
    pub fn area_save_status(&self, area_id: AreaId) -> AreaSaveStatus {
        self.inner.pending.save_status(area_id)
    }

    /// The operation currently paused for conflict review in this area.
    #[must_use]
    pub fn conflicted_operation_id(&self, area_id: AreaId) -> Option<OperationId> {
        self.inner.pending.conflicted_operation_id(area_id)
    }

    /// Whether an operation is still present in this area's pending queue.
    #[must_use]
    pub fn is_operation_pending(&self, area_id: AreaId, operation_id: OperationId) -> bool {
        self.inner.pending.contains_operation(area_id, operation_id)
    }

    /// Resolves a conflict-paused area. Keep mine keeps every pending
    /// operation (a deliberate overwrite of the remote edit): the displayed
    /// area is rebuilt as the backend's projection plus all pending
    /// operations, then the queue resumes sending against the fresh
    /// revision. Keep theirs discards exactly the conflicted operation and
    /// rebuilds the displayed area from the backend's projection plus the
    /// operations still pending.
    pub async fn resolve_conflict(&self, area_id: AreaId, keep_mine: bool) {
        self.inner.resolve_conflict(area_id, keep_mine).await;
    }

    /// Resolves a permanently-failed area. Retry re-arms the parked
    /// operation; discard drops it and rebuilds the displayed area from the
    /// backend's projection plus the operations still pending.
    pub async fn resolve_failed(&self, area_id: AreaId, retry: bool) {
        self.inner.resolve_failed(area_id, retry).await;
    }

    /// Cancels a queued-but-unsent operation — the local undo of
    /// unacknowledged work. Returns whether the operation was found and
    /// removable: the head of a non-idle queue is not (in flight it is on
    /// the wire; parked it belongs to [`Self::resolve_conflict`] /
    /// [`Self::resolve_failed`]). On success the displayed area is rebuilt
    /// without the canceled operation's optimistic effect.
    pub async fn cancel_pending(&self, area_id: AreaId, operation_id: OperationId) -> bool {
        self.inner.cancel_pending(area_id, operation_id).await
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

            // Done only when both write pipelines drain: the legacy channel's
            // counters and the CAS pending queue (parked envelopes count —
            // they are unsent work awaiting a user decision).
            if pending == 0 && self.pending.total_pending() == 0 {
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
        let result = self.load_all_areas_inner().await;
        // Open the initial-load gate either way: presence-checked imports wait
        // on it, and distinguish success from failure by the flag's value.
        self.initial_load.send_replace(Some(result.is_ok()));
        result
    }

    async fn load_all_areas_inner(&self) -> CloudResult<LoadMapsSummary> {
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
                    let shared =
                        area.owner_nickname.is_some() || area.access.is_some_and(|a| !a.is_owner);

                    stats.push(AreaLoadStat {
                        area_id: area.id,
                        name: details.area.name.clone(),
                        revision: details.area.rev,
                        load_duration,
                        source,
                        shared,
                    });

                    // The fetched document is backend truth: record its
                    // revision and access fingerprint for CAS preconditions.
                    self.pending.note_confirmed_rev(
                        area.id,
                        details.area.rev,
                        details.area.access.map(|access| access.fingerprint()),
                    );

                    // A wholesale rebuild is still a swap: any envelopes
                    // already pending (e.g. from an entry script racing the
                    // load) must keep their optimistic effect, so fold them
                    // over the fetched document rather than dropping them.
                    let details = if self.pending.pending_for(area.id).is_empty() {
                        details
                    } else {
                        self.fold_pending(area.id, &details, ReplayMode::SkipFailures)
                            .0
                    };
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

        // The created row is backend truth for the new area's revision.
        self.pending.note_confirmed_rev(
            area_id,
            backend_area.rev,
            backend_area.access.map(|access| access.fingerprint()),
        );

        self.atlas_cache.rcu(|cache| {
            Arc::new(cache.add_area(
                area_id,
                Arc::new(AreaCache::new_with_area(AreaWithDetails {
                    area: backend_area.clone(),
                    format_version: crate::AREA_FORMAT_VERSION,
                    content_hash: None,
                    properties: vec![],
                    rooms: vec![],
                    labels: vec![],
                    shapes: vec![],
                    connections: vec![],
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

        // §8.4: the whole import is validated before any write — one invalid
        // document rejects the batch (a v1 document migrated on the way in
        // passes by construction).
        for details in &areas {
            validate_import_document(details)?;
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
            // Fresh Connection identities, keeping the exits' membership
            // references consistent (validated above, so the lookups below
            // cannot miss).
            let connection_map: HashMap<crate::ConnectionId, crate::ConnectionId> = details
                .connections
                .iter()
                .map(|connection| (connection.id, crate::ConnectionId::new()))
                .collect();
            for connection in &mut details.connections {
                connection.id = connection_map[&connection.id];
            }
            for room in &mut details.rooms {
                room.is_secret = false;
                for exit in &mut room.exits {
                    exit.id = ExitId(Uuid::new_v4());
                    exit.connection_id = connection_map[&exit.connection_id];
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
            // Dropped cross-area links (and cleared `to_unknown` markers)
            // can leave an External Connection with no member that still
            // leaves the area: it becomes Dangling, exactly as a live edit
            // would convert it.
            let leaves_area: HashSet<crate::ConnectionId> = details
                .rooms
                .iter()
                .flat_map(|room| room.exits.iter())
                .filter(|exit| exit.to_area_id.is_some_and(|to| to != details.area.id))
                .map(|exit| exit.connection_id)
                .collect();
            for connection in &mut details.connections {
                if connection.kind == crate::ConnectionKind::External
                    && !leaves_area.contains(&connection.id)
                {
                    connection.kind = crate::ConnectionKind::Dangling;
                }
            }
        }

        for details in &areas {
            self.backend.import_local_area(details.clone()).await?;
            // The stored document is backend truth for the imported area.
            self.pending.note_confirmed_rev(
                details.area.id,
                details.area.rev,
                details.area.access.map(|access| access.fingerprint()),
            );
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

    /// Waits for the first [`Inner::load_all_areas`] to complete. Presence-checked imports gate on
    /// this so they never compare against a not-yet-populated atlas — package entry scripts run
    /// before the session loads its maps, and an empty atlas would make every name look absent.
    ///
    /// # Errors
    /// Errors when the initial load failed; a caller should skip its import rather than seed
    /// blind into an unknown atlas.
    async fn wait_for_initial_load(&self) -> CloudResult<()> {
        let mut gate = self.initial_load.subscribe();
        let outcome = *gate.wait_for(Option::is_some).await.map_err(|_| {
            crate::CloudError::InternalError(
                "mapper dropped before its initial area load".to_string(),
            )
        })?;
        if outcome == Some(true) {
            Ok(())
        } else {
            Err(crate::CloudError::InternalError(
                "initial area load failed; refusing a presence-checked import".to_string(),
            ))
        }
    }

    /// Presence-checked import — the offer-once seeding primitive. Imports (via
    /// [`Inner::import_areas`]) only the areas whose name no resident area already bears, and
    /// reports the rest as skipped.
    ///
    /// Three properties make this safe to call unconditionally, from any load order:
    /// - It waits for the initial area load, so an entry script's seed compares against the real
    ///   atlas rather than the empty pre-load cache.
    /// - The presence check reads the **unfiltered** resident set: shared, manually-disabled, and
    ///   per-server scope-excluded areas all count as present, so a map parked for another server
    ///   entry (or hidden from identification) is never re-imported as a duplicate.
    /// - Concurrent presence-checked imports serialize on one gate, so two packages seeding the
    ///   same name cannot both miss it and both import.
    ///
    /// # Errors
    /// Errors when the initial area load failed or the local backend's persistence fails.
    pub async fn import_areas_if_absent(
        &self,
        areas: Vec<AreaWithDetails>,
    ) -> CloudResult<AreasImportedIfAbsent> {
        self.wait_for_initial_load().await?;
        let _gate = self.import_gate.lock().await;
        let existing: HashSet<String> = self
            .get_current_atlas()
            .areas()
            .map(|area| area.get_name().to_string())
            .collect();
        let (missing, present): (Vec<_>, Vec<_>) = areas
            .into_iter()
            .partition(|details| !existing.contains(&details.area.name));
        let skipped = present
            .into_iter()
            .map(|details| details.area.name)
            .collect();
        let added = self.import_areas(missing).await?;
        Ok(AreasImportedIfAbsent { added, skipped })
    }

    /// Serialize an area to its full [`AreaWithDetails`] — the JSON-export path. The bytes are the
    /// viewer-scoped, secret-redacted projection the backend already holds, so this can only ever
    /// emit what the viewer can see; the `can_copy` gate is enforced by the caller.
    ///
    /// §8.4: the export is v2 with `connections` stably id-sorted (the
    /// server already serves them sorted; local documents are sorted here)
    /// so repeated exports diff meaningfully. Route points keep their stored
    /// order — it is the path.
    ///
    /// # Errors
    /// Propagates the backend's read error.
    pub async fn export_area(&self, area_id: AreaId) -> CloudResult<AreaWithDetails> {
        let mut details = self.backend.get_area(&area_id).await?;
        details.connections.sort_by_key(|connection| connection.id);
        Ok(details)
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
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |_area| Arc::new(cache.delete_area(area_id)),
            )
        });
        self.send_sync_operation(AreaSyncOperation::Delete(area_id));
    }

    pub fn rename_area(&self, area_id: AreaId, name: &str) {
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| Arc::new(cache.insert_area(area_id, Arc::new(area.rename(name)))),
            )
        });
        self.send_sync_operation(AreaSyncOperation::Rename(area_id, name.to_string()));
    }

    pub fn move_area_to_atlas(&self, area_id: AreaId, atlas_id: Option<AtlasId>) {
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| Arc::new(cache.insert_area(area_id, Arc::new(area.with_atlas(atlas_id)))),
            )
        });
        self.send_sync_operation(AreaSyncOperation::Move(area_id, atlas_id));
    }

    pub fn set_area_property(&self, area_id: AreaId, name: String, value: String) {
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(cache.insert_area(
                        area_id,
                        Arc::new(area.set_property(name.clone(), value.clone())),
                    ))
                },
            )
        });

        let description = format!("Set area property {name}");
        self.enqueue_content(
            area_id,
            vec![AreaMutation::UpsertAreaProperty {
                name,
                value,
                is_secret: None,
            }],
            &description,
        );
    }

    pub fn delete_area_property(&self, area_id: AreaId, name: String) {
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(
                        cache.insert_area(area_id, Arc::new(area.delete_property(name.as_str()))),
                    )
                },
            )
        });

        let description = format!("Delete area property {name}");
        self.enqueue_content(
            area_id,
            vec![AreaMutation::DeleteAreaProperty { name }],
            &description,
        );
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

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn upsert_room(&self, room_key: RoomKey, updates: RoomUpdates) {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        if self.over_ephemeral_cap(area_id, std::slice::from_ref(&room_number)) {
            return;
        }
        let room_precondition = Mutex::new(None);
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    *room_precondition.lock() = Some(if area.get_room(&room_number).is_none() {
                        StructuralPrecondition::RoomAbsent(room_number)
                    } else {
                        StructuralPrecondition::RoomPresent(room_number)
                    });
                    Arc::new(cache.insert_area(
                        *area.get_id(),
                        Arc::new(area.upsert_room(room_number, updates.clone())),
                    ))
                },
            )
        });

        let description = format!("Update room {room_number}");
        self.enqueue_content_guarded(
            area_id,
            vec![AreaMutation::UpsertRoom {
                room_number,
                body: updates,
            }],
            &description,
            room_precondition.into_inner().into_iter().collect(),
        );
    }

    pub fn upsert_rooms(&self, area_id: AreaId, updates: Vec<(RoomNumber, RoomUpdates)>) {
        if updates.is_empty() {
            return;
        }
        let numbers: Vec<RoomNumber> = updates.iter().map(|(number, _)| *number).collect();
        if self.over_ephemeral_cap(area_id, &numbers) {
            return;
        }
        let absent_rooms = Mutex::new(HashSet::new());
        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    *absent_rooms.lock() = numbers
                        .iter()
                        .filter(|number| area.get_room(number).is_none())
                        .copied()
                        .collect();
                    Arc::new(
                        cache.insert_area(*area.get_id(), Arc::new(area.upsert_rooms(&updates))),
                    )
                },
            )
        });
        let absent_rooms = absent_rooms.into_inner();

        // The batch rides one envelope, so it lands (and any conflict
        // review treats it) atomically — up to the server-enforced
        // per-envelope operation cap. An oversized batch must split before
        // enqueue (the server rejects it outright); the chunks land in
        // order on the area's queue, so atomicity becomes per-chunk.
        let description = if updates.len() == 1 {
            format!("Update room {}", updates[0].0)
        } else {
            format!("Update {} rooms", updates.len())
        };
        let mut ops: Vec<AreaMutation> = updates
            .into_iter()
            .map(|(room_number, body)| AreaMutation::UpsertRoom { room_number, body })
            .collect();
        while ops.len() > MAX_MUTATION_OPERATIONS {
            let rest = ops.split_off(MAX_MUTATION_OPERATIONS);
            let preconditions = Self::room_structural_preconditions(&ops, &absent_rooms);
            self.enqueue_content_guarded(area_id, ops, &description, preconditions);
            ops = rest;
        }
        let preconditions = Self::room_structural_preconditions(&ops, &absent_rooms);
        self.enqueue_content_guarded(area_id, ops, &description, preconditions);
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
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
                let reduced =
                    (area_id == room_key.area_id).then(|| area.delete_room(room_key.room_number));
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

        // The envelope carries only the deletion; the server owns the
        // inbound-exit cascade mirrored above.
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let description = format!("Delete room {room_number}");
        self.enqueue_content(
            area_id,
            vec![AreaMutation::DeleteRoom { room_number }],
            &description,
        );
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn set_room_property(&self, room_key: RoomKey, name: String, value: String) {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| {
                    area.set_room_property(room_number, name.clone(), value.clone())
                        .ok()
                })
                .map_or_else(
                    || cache.clone(),
                    |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))),
                )
        });

        let description = format!("Set property {name} on room {room_number}");
        self.enqueue_content(
            area_id,
            vec![AreaMutation::UpsertRoomProperty {
                room_number,
                name,
                value,
                is_secret: None,
            }],
            &description,
        );
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn delete_room_property(&self, room_key: RoomKey, name: String) {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| area.delete_room_property(room_number, name.as_str()).ok())
                .map_or_else(
                    || cache.clone(),
                    |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))),
                )
        });

        let description = format!("Delete property {name} on room {room_number}");
        self.enqueue_content(
            area_id,
            vec![AreaMutation::DeleteRoomProperty { room_number, name }],
            &description,
        );
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn add_room_tag(&self, room_key: RoomKey, tag: String) {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let tag = normalize_tag(&tag);
        if tag.is_empty() {
            return;
        }

        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| area.add_room_tag(room_number, &tag).ok())
                .map_or_else(
                    || cache.clone(),
                    |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))),
                )
        });

        let description = format!("Add tag {tag} to room {room_number}");
        self.enqueue_content(
            area_id,
            vec![AreaMutation::AddRoomTag { room_number, tag }],
            &description,
        );
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn remove_room_tag(&self, room_key: RoomKey, tag: String) {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let tag = normalize_tag(&tag);
        if tag.is_empty() {
            return;
        }

        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| area.remove_room_tag(room_number, &tag).ok())
                .map_or_else(
                    || cache.clone(),
                    |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))),
                )
        });

        let description = format!("Remove tag {tag} from room {room_number}");
        self.enqueue_content(
            area_id,
            vec![AreaMutation::RemoveRoomTag { room_number, tag }],
            &description,
        );
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn update_exit(&self, room_key: RoomKey, exit_id: ExitId, updates: ExitUpdates) {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| {
                    area.get_room(&room_number)
                        .and_then(|room| room.get_exits().iter().find(|e| e.id == exit_id))
                        .map(|exit| (area.clone(), updates.clone().apply(exit)))
                })
                .and_then(|(area, new_exit)| area.upsert_exit(room_number, new_exit).ok())
                .map_or_else(
                    || cache.clone(),
                    |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))),
                )
        });

        self.enqueue_content(
            area_id,
            vec![AreaMutation::UpdateExit {
                exit_id,
                body: updates,
            }],
            "Update exit",
        );
    }

    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn delete_exit(&self, room_key: RoomKey, exit_id: ExitId) {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| area.delete_exit(room_number, exit_id).ok())
                .map_or_else(
                    || cache.clone(),
                    |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))),
                )
        });

        self.enqueue_content(
            area_id,
            vec![AreaMutation::DeleteExit { exit_id }],
            "Delete exit",
        );
    }
    // === CREATE OPERATIONS (Client-Minted Ids) ===

    /// Create an exit. The id is client-minted before enqueue, the cache
    /// updates optimistically, and the envelope queues for the backend —
    /// there is no round-trip to wait for.
    ///
    /// # Errors
    /// Infallible today; the result type is retained so call sites keep
    /// handling a backend verdict where one used to surface.
    #[allow(clippy::needless_pass_by_value)] // the by-value key is the established public signature
    pub fn create_exit(&self, room_key: RoomKey, mut args: ExitArgs) -> CloudResult<ExitId> {
        let RoomKey {
            area_id,
            room_number,
        } = room_key;
        let exit_id = args.id.unwrap_or_else(|| ExitId(Uuid::new_v4()));
        args.id = Some(exit_id);
        // The Connection membership is a placeholder here: the cache's
        // optimistic upsert attaches the real one (auto-pair or a fresh
        // Connection), and every backend applier re-derives it from the
        // same rules when the envelope lands.
        let exit = area_edits::exit_from_args(args.clone(), crate::ConnectionId::default());

        self.atlas_cache.rcu(|cache| {
            cache
                .get_area(&area_id)
                .and_then(|area| area.upsert_exit(room_number, exit.clone().into()).ok())
                .map_or_else(
                    || cache.clone(),
                    |area| Arc::new(cache.insert_area(*area.get_id(), Arc::new(area))),
                )
        });

        let description = format!(
            "Create exit {} from room {room_number}",
            exit.from_direction
        );
        self.enqueue_content(
            area_id,
            vec![AreaMutation::CreateExit {
                room_number,
                body: args,
            }],
            &description,
        );

        Ok(exit_id)
    }

    /// Create a label with a client-minted id; see [`Self::create_exit`].
    ///
    /// # Errors
    /// Infallible today; the result type is retained so call sites keep
    /// handling a backend verdict where one used to surface.
    pub fn create_label(&self, area_id: AreaId, mut args: LabelArgs) -> CloudResult<LabelId> {
        let label_id = args.id.unwrap_or_else(|| LabelId(Uuid::new_v4()));
        args.id = Some(label_id);
        let label = area_edits::label_from_args(args.clone());

        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(cache.insert_area(
                        *area.get_id(),
                        Arc::new(area.upsert_label(label_id, label.clone())),
                    ))
                },
            )
        });

        self.enqueue_content(
            area_id,
            vec![AreaMutation::CreateLabel { body: args }],
            "Create label",
        );

        Ok(label_id)
    }

    /// Create a shape with a client-minted id; see [`Self::create_exit`].
    ///
    /// # Errors
    /// Infallible today; the result type is retained so call sites keep
    /// handling a backend verdict where one used to surface.
    pub fn create_shape(&self, area_id: AreaId, mut args: ShapeArgs) -> CloudResult<ShapeId> {
        let shape_id = args.id.unwrap_or_else(|| ShapeId(Uuid::new_v4()));
        args.id = Some(shape_id);
        let shape = area_edits::shape_from_args(args.clone());

        self.atlas_cache.rcu(|cache| {
            cache.get_area(&area_id).map_or_else(
                || cache.clone(),
                |area| {
                    Arc::new(cache.insert_area(
                        *area.get_id(),
                        Arc::new(area.upsert_shape(shape_id, shape.clone())),
                    ))
                },
            )
        });

        self.enqueue_content(
            area_id,
            vec![AreaMutation::CreateShape { body: args }],
            "Create shape",
        );

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

        self.enqueue_content(
            area_id,
            vec![AreaMutation::UpdateLabel {
                label_id,
                body: updates,
            }],
            "Update label",
        );
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

        self.enqueue_content(
            area_id,
            vec![AreaMutation::DeleteLabel { label_id }],
            "Delete label",
        );
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

        self.enqueue_content(
            area_id,
            vec![AreaMutation::UpdateShape {
                shape_id,
                body: updates,
            }],
            "Update shape",
        );
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

        self.enqueue_content(
            area_id,
            vec![AreaMutation::DeleteShape { shape_id }],
            "Delete shape",
        );
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

    // === CAS PENDING QUEUE ===

    /// Queues one content-mutation envelope: mints the operation id, records
    /// the send in the sync stats and the per-area in-flight counters (the
    /// sync engine defers refetches while these are non-zero, protecting the
    /// optimistic overlay), and hands the envelope to the pending store.
    fn enqueue_content(
        &self,
        area_id: AreaId,
        ops: Vec<AreaMutation>,
        description: &str,
    ) -> OperationId {
        self.enqueue_content_guarded(area_id, ops, description, Vec::new())
    }

    fn enqueue_content_guarded(
        &self,
        area_id: AreaId,
        ops: Vec<AreaMutation>,
        description: &str,
        structural_preconditions: Vec<StructuralPrecondition>,
    ) -> OperationId {
        let operation_id = Uuid::new_v4();
        self.sync_stats
            .operations_sent
            .fetch_add(1, Ordering::Relaxed);
        {
            let mut pending = self.pending_by_area.lock();
            *pending.entry(area_id).or_insert(0) += 1;
        }
        self.pending.enqueue(
            area_id,
            PendingEnvelope {
                operation_id,
                ops,
                description: description.to_string(),
                structural_preconditions,
                attempts: 0,
            },
        );
        operation_id
    }

    fn room_structural_preconditions(
        operations: &[AreaMutation],
        absent_rooms: &HashSet<RoomNumber>,
    ) -> Vec<StructuralPrecondition> {
        operations
            .iter()
            .filter_map(|operation| match operation {
                AreaMutation::UpsertRoom { room_number, .. } => {
                    Some(if absent_rooms.contains(room_number) {
                        StructuralPrecondition::RoomAbsent(*room_number)
                    } else {
                        StructuralPrecondition::RoomPresent(*room_number)
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// The compound counterpart to the ergonomic single-operation helpers.
    /// The shared document applier gives the live cache exactly the same
    /// all-or-nothing semantics as local/ephemeral backends and conflict
    /// replay; only after it succeeds is the envelope made visible to the
    /// pending worker.
    fn mutate_area(
        &self,
        area_id: AreaId,
        operations: Vec<AreaMutation>,
        description: String,
    ) -> CloudResult<OperationId> {
        if operations.is_empty() {
            return Err(CloudError::InvalidInput(
                "a mutation must contain at least one operation".to_string(),
            ));
        }
        if operations.len() > MAX_MUTATION_OPERATIONS {
            return Err(CloudError::InvalidInput(format!(
                "a mutation may contain at most {MAX_MUTATION_OPERATIONS} operations"
            )));
        }

        let failure = Mutex::new(None);
        let structural_preconditions = Mutex::new(Vec::new());
        self.atlas_cache.rcu(|cache| {
            let Some(area) = cache.get_area(&area_id) else {
                *failure.lock() = Some(CloudError::AreaNotFound(area_id));
                return cache.clone();
            };
            let mut details = area.to_details();
            let existing_rooms: HashSet<_> =
                details.rooms.iter().map(|room| room.room_number).collect();
            *structural_preconditions.lock() = operations
                .iter()
                .filter_map(|operation| match operation {
                    AreaMutation::UpsertRoom { room_number, .. } => {
                        Some(if existing_rooms.contains(room_number) {
                            StructuralPrecondition::RoomPresent(*room_number)
                        } else {
                            StructuralPrecondition::RoomAbsent(*room_number)
                        })
                    }
                    _ => None,
                })
                .collect::<HashSet<_>>()
                .into_iter()
                .collect();
            let envelope = MutationEnvelope {
                operation_id: Uuid::new_v4(),
                preconditions: vec![Precondition {
                    resource: ResourceKind::Area,
                    id: area_id.0,
                    expected_rev: details.area.rev,
                    access_fingerprint: details.area.access.map(|access| access.fingerprint()),
                }],
                payload: operations.clone(),
            };
            match area_edits::apply_envelope(&mut details, area_id, &envelope) {
                Ok(_) => Arc::new(
                    cache.insert_area(area_id, Arc::new(AreaCache::new_with_area(details))),
                ),
                Err(error) => {
                    *failure.lock() = Some(error);
                    cache.clone()
                }
            }
        });
        if let Some(error) = failure.into_inner() {
            return Err(error);
        }
        Ok(self.enqueue_content_guarded(
            area_id,
            operations,
            &description,
            structural_preconditions.into_inner(),
        ))
    }

    /// Spawns the pending-queue worker: one task draining ready envelopes
    /// across all areas, sleeping until the store wakes it or the earliest
    /// backoff deadline passes. Holds only a weak reference to the mapper
    /// internals and exits once they are dropped.
    fn spawn_mutation_worker(inner: &Arc<Self>) {
        let weak = Arc::downgrade(inner);
        let pending = inner.pending.clone();
        tokio::spawn(async move {
            loop {
                let deadline = {
                    let Some(inner) = weak.upgrade() else { break };
                    let (ready, deadline) = inner.pending.take_ready(Instant::now());
                    if let Some((area_id, envelope, rev, fingerprint)) = ready {
                        inner
                            .dispatch_envelope(area_id, envelope, rev, fingerprint)
                            .await;
                        continue;
                    }
                    deadline
                };
                // Idle: a store wake (enqueue, acknowledge, resolve) or the
                // earliest backoff deadline resumes the loop.
                if let Some(deadline) = deadline {
                    let _ = tokio::time::timeout_at(
                        tokio::time::Instant::from_std(deadline),
                        pending.notify.notified(),
                    )
                    .await;
                } else {
                    pending.notify.notified().await;
                }
            }
        });
    }

    /// Sends one envelope to the backend and routes the verdict back into
    /// the pending store and the sync stats.
    async fn dispatch_envelope(
        &self,
        area_id: AreaId,
        envelope: PendingEnvelope,
        confirmed_rev: Option<i64>,
        fingerprint: Option<String>,
    ) {
        // The precondition rides the last backend-confirmed revision; until
        // any backend truth lands, the cached revision stands in. The server
        // requires the access fingerprint on every area precondition, so an
        // unrecorded fingerprint falls back to the cached area's access
        // block; local/ephemeral areas have no access block and their
        // backends ignore the field.
        let (expected_rev, fingerprint) = {
            let cache = self.atlas_cache.load();
            let area = cache.get_area(&area_id);
            (
                confirmed_rev.unwrap_or_else(|| area.as_ref().map_or(0, |a| a.get_rev())),
                fingerprint.or_else(|| area.map(|a| a.effective_access().fingerprint())),
            )
        };
        let operation_id = envelope.operation_id;
        let wire = MutationEnvelope {
            operation_id,
            preconditions: vec![Precondition {
                resource: ResourceKind::Area,
                id: area_id.0,
                expected_rev,
                access_fingerprint: fingerprint,
            }],
            payload: envelope.ops,
        };
        match self.backend.execute_mutation(&area_id, &wire).await {
            Ok(result) => {
                let own_rev = result
                    .versions
                    .iter()
                    .find(|version| {
                        version.resource == ResourceKind::Area && version.id == area_id.0
                    })
                    .map(|version| version.rev);
                self.pending.acknowledge(area_id, operation_id, own_rev);
                self.sync_stats
                    .operations_succeeded
                    .fetch_add(1, Ordering::Relaxed);
                Self::decrement_pending(&self.pending_by_area, area_id);

                // A compound mutation can move aggregates beyond its own
                // area (cross-area cascades). Record their confirmed
                // revisions and nudge the sync engine to refetch the
                // affected projections.
                let mut foreign_versions = false;
                for version in &result.versions {
                    if version.resource == ResourceKind::Area && version.id != area_id.0 {
                        self.pending
                            .note_confirmed_rev(AreaId(version.id), version.rev, None);
                        foreign_versions = true;
                    }
                }
                if foreign_versions {
                    self.sync_notify.notify_one();
                }
            }
            Err(CloudError::RevisionConflict { .. } | CloudError::ProjectionChanged { .. }) => {
                self.reconcile_conflict(area_id).await;
            }
            Err(CloudError::UpgradeRequired) => {
                // Pause every cloud queue without discarding anything, and
                // surface the terminal state through the sync engine's
                // status so the UI opens the upgrade path.
                self.pending.pause_for_upgrade();
                sync_engine::set_status(
                    self,
                    SyncState::UpgradeRequired,
                    Some(CloudError::UpgradeRequired.to_string()),
                );
            }
            Err(err) if err.is_transport_error() => {
                self.transport_failure_with_accounting(area_id);
            }
            Err(err) => {
                // Validation/authorization verdicts never spin: park the
                // envelope for Retry / Discard and close its accounting.
                self.pending.permanent_failure(area_id, err.to_string());
                self.sync_stats
                    .operations_failed
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Reports a transport failure to the store and, when the attempt budget
    /// just expired and parked the envelope, records the terminal failure in
    /// the stats. The count keys off the store's returned verdict — issued
    /// exactly once per park, under the transition's own lock — so a
    /// concurrent resolution can never skew the accounting.
    fn transport_failure_with_accounting(&self, area_id: AreaId) {
        if self.pending.transport_failure(area_id, Instant::now()) == TransportVerdict::Parked {
            self.sync_stats
                .operations_failed
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The conflict path (§2.5): refetch the area's confirmed projection,
    /// replay the pending queue over it, and either resend the head against
    /// the fresh revision (everything still applies — a sane operation's
    /// reapply is a deliberate overwrite by design) or pause the queue
    /// targeting the envelope that failed the sanity check.
    async fn reconcile_conflict(&self, area_id: AreaId) {
        self.backend.purge_area(&area_id).await;
        match self.backend.get_area(&area_id).await {
            Ok(fresh) => {
                match self.replay_pending_over(area_id, &fresh, ReplayMode::StopAtFailure) {
                    None => self.pending.ready_resend(area_id),
                    Some(failed) => self.pending.pause_conflict(area_id, failed),
                }
            }
            Err(err) => {
                // The refetch itself failed; back off and retry like any
                // transport failure (the envelope stays queued).
                warn!("Conflict refetch of area {area_id} failed: {err}");
                self.transport_failure_with_accounting(area_id);
            }
        }
    }

    /// Folds every pending envelope for `area_id` over `fresh`, in order,
    /// per `mode`. Envelopes are atomic: each applies to a scratch copy so
    /// a partly applicable envelope leaves no trace in the fold. Returns
    /// the folded document and the first failing envelope's operation id.
    fn fold_pending(
        &self,
        area_id: AreaId,
        fresh: &AreaWithDetails,
        mode: ReplayMode,
    ) -> (AreaWithDetails, Option<OperationId>) {
        let mut working = fresh.clone();
        let mut first_failed = None;
        for envelope in self.pending.pending_for(area_id) {
            let mut scratch = working.clone();
            let preconditions_hold =
                mode == ReplayMode::KeepMine || envelope.structural_preconditions_hold(&working);
            if preconditions_hold
                && envelope
                    .ops
                    .iter()
                    .all(|op| area_edits::apply_mutation(&mut scratch, op).is_ok())
            {
                working = scratch;
            } else {
                if first_failed.is_none() {
                    first_failed = Some(envelope.operation_id);
                }
                if mode == ReplayMode::StopAtFailure {
                    break;
                }
            }
        }
        (working, first_failed)
    }

    /// Rebuilds the displayed area as `fresh confirmed projection + pending
    /// envelopes` (folded per `mode`), records the fetched revision and
    /// fingerprint as backend truth, and swaps the rebuilt cache in.
    ///
    /// The fold races concurrent enqueues: an envelope whose optimistic
    /// effect landed on the pre-swap cache would vanish from a swap folded
    /// without it, so the store's enqueue epoch is compared across each
    /// snapshot→swap window and the fold re-runs on a change. The retry is
    /// bounded — a sustained write storm keeps its later envelopes queued
    /// either way, and the sync engine heals any display residue.
    ///
    /// Returns the operation id of the first envelope that failed to
    /// apply, from the last fold performed (`None` = everything applied).
    fn replay_pending_over(
        &self,
        area_id: AreaId,
        fresh: &AreaWithDetails,
        mode: ReplayMode,
    ) -> Option<OperationId> {
        const MAX_REPLAY_FOLDS: u32 = 3;
        self.pending.note_confirmed_rev(
            area_id,
            fresh.area.rev,
            fresh.area.access.map(|access| access.fingerprint()),
        );
        let mut first_failed = None;
        for fold in 1..=MAX_REPLAY_FOLDS {
            let epoch = self.pending.enqueue_epoch();
            let (working, failed) = self.fold_pending(area_id, fresh, mode);
            first_failed = failed;
            self.swap_area_details(working);
            if self.pending.enqueue_epoch() == epoch || fold == MAX_REPLAY_FOLDS {
                break;
            }
        }
        first_failed
    }

    /// Swaps a full area document into the atlas cache the way a sync
    /// refetch lands one, bumping the sync revision so pollers notice. The
    /// rcu preserves every other area and every exclusion axis.
    fn swap_area_details(&self, details: AreaWithDetails) {
        let area_id = details.area.id;
        let area_cache = Arc::new(AreaCache::new_with_area(details));
        self.atlas_cache
            .rcu(|cache| Arc::new(cache.insert_area(area_id, area_cache.clone())));
        self.sync_revision.fetch_add(1, Ordering::AcqRel);
    }

    /// Purges any cached copy of `area_id` and refetches the backend's
    /// confirmed projection for a display rebuild. A failed refetch is
    /// logged and yields `None` — the caller's queue state is already
    /// correct, and the sync engine heals the display later.
    async fn refetch_confirmed(&self, area_id: AreaId) -> Option<AreaWithDetails> {
        self.backend.purge_area(&area_id).await;
        match self.backend.get_area(&area_id).await {
            Ok(fresh) => Some(fresh),
            Err(err) => {
                warn!("Refetch of area {area_id} for a display rebuild failed: {err}");
                None
            }
        }
    }

    /// Rebuild after an envelope leaves the queue outside the acknowledge
    /// path (discard, cancel): the departed operation's optimistic effect
    /// must leave the display, so the area is rebuilt from backend truth
    /// plus whatever remains pending. A remaining operation that depended
    /// on the removed one pauses at its own sanity check, targeted by id.
    async fn rebuild_after_removal(&self, area_id: AreaId) {
        if let Some(fresh) = self.refetch_confirmed(area_id).await
            && let Some(failed) =
                self.replay_pending_over(area_id, &fresh, ReplayMode::StopAtFailure)
        {
            self.pending.pause_conflict(area_id, failed);
        }
    }

    /// The Keep-mine display rebuild: refetch the confirmed projection and
    /// fold *every* pending envelope over it best-effort, so the operations
    /// the user chose to keep are visible again. An envelope that cannot
    /// apply locally stays pending (undisplayed) and still goes to the
    /// server, whose verdict is authoritative — it may well accept what the
    /// local sanity check could not model.
    async fn rebuild_keeping_pending(&self, area_id: AreaId) {
        if let Some(fresh) = self.refetch_confirmed(area_id).await {
            self.replay_pending_over(area_id, &fresh, ReplayMode::KeepMine);
        }
    }

    /// Resolves a conflict-paused area; see [`Mapper::resolve_conflict`].
    pub async fn resolve_conflict(&self, area_id: AreaId, keep_mine: bool) {
        let resolution = self.pending.resolve_conflict(area_id, keep_mine);
        if !resolution.resolved {
            return;
        }
        if resolution.discarded.is_some() {
            // The discarded envelope was sent-tracked but can never
            // succeed; close its accounting so the counters settle.
            self.sync_stats
                .operations_failed
                .fetch_add(1, Ordering::Relaxed);
            Self::decrement_pending(&self.pending_by_area, area_id);
            self.rebuild_after_removal(area_id).await;
        } else if keep_mine {
            // The store held the queue paused for exactly this window:
            // rebuild the display with the kept operations first, then
            // release the resend so it cannot race the fold.
            self.rebuild_keeping_pending(area_id).await;
            self.pending.ready_resend(area_id);
        }
        // Keep theirs with nothing to discard (the conflicted envelope was
        // independently canceled): the cancel already settled accounting
        // and rebuilt the display.
    }

    /// Resolves a permanently-failed area; see [`Mapper::resolve_failed`].
    pub async fn resolve_failed(&self, area_id: AreaId, retry: bool) {
        // Parked envelopes were terminally counted at park time: a retry
        // reopens that accounting (the envelope will close it again when it
        // next acknowledges or parks), a discard closes the per-area
        // counter the park left in place. Both key off the store's returned
        // resolution — decided under the transition's own lock — never off
        // a status re-read a concurrent park could race.
        let resolution = self.pending.resolve_failure(area_id, retry);
        if !resolution.unparked {
            return;
        }
        if retry {
            self.sync_stats
                .operations_failed
                .fetch_sub(1, Ordering::Relaxed);
        } else if resolution.discarded.is_some() {
            Self::decrement_pending(&self.pending_by_area, area_id);
            self.rebuild_after_removal(area_id).await;
        }
    }

    /// Cancels a queued-but-unsent envelope; see [`Mapper::cancel_pending`].
    pub async fn cancel_pending(&self, area_id: AreaId, operation_id: OperationId) -> bool {
        if self.pending.cancel(area_id, operation_id).is_none() {
            return false;
        }
        // The canceled envelope never produced a backend verdict: unwind its
        // enqueue-time bookkeeping rather than recording an outcome.
        self.sync_stats
            .operations_sent
            .fetch_sub(1, Ordering::Relaxed);
        Self::decrement_pending(&self.pending_by_area, area_id);
        self.rebuild_after_removal(area_id).await;
        true
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
            AreaSyncOperation::Rename(area_id, name) => {
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
            AreaSyncOperation::Move(area_id, atlas_id) => {
                backend.move_area_to_atlas(&area_id, atlas_id).await?;
                Ok(())
            }
            AreaSyncOperation::Delete(area_id) => {
                backend.delete_area(&area_id).await?;
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
        Area, AreaAccess, AreaWithDetails, CloudError, CreateAreaRequest, Exit, RoomWithDetails,
    };
    use async_trait::async_trait;
    use chrono::Utc;

    /// Backend serving a set of areas from memory; content mutations apply
    /// through the shared CAS applier so the pending queue drains against
    /// real revision semantics, and each received envelope is logged for
    /// receipt/batching assertions.
    struct FixedBackend {
        areas: Mutex<HashMap<AreaId, AreaWithDetails>>,
        /// One `(operation id, operation count)` entry per received
        /// envelope, in arrival order.
        mutations: Mutex<Vec<(Uuid, usize)>>,
        /// When set, every received envelope fails with this error instead
        /// of applying (scripted server verdicts / outages).
        fail_with: Mutex<Option<CloudError>>,
    }

    impl FixedBackend {
        fn new(areas: Vec<AreaWithDetails>) -> Self {
            Self {
                areas: Mutex::new(areas.into_iter().map(|a| (a.area.id, a)).collect()),
                mutations: Mutex::new(Vec::new()),
                fail_with: Mutex::new(None),
            }
        }

        fn fail_mutations_with(&self, error: Option<CloudError>) {
            *self.fail_with.lock() = error;
        }
    }

    #[async_trait]
    impl MapperBackend for FixedBackend {
        async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
            Err(CloudError::NetworkError("read-only".to_string()))
        }

        async fn import_local_area(&self, _details: AreaWithDetails) -> CloudResult<()> {
            // Cache-side effects are all the import tests observe.
            Ok(())
        }

        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            Ok(self.areas.lock().values().map(|a| a.area.clone()).collect())
        }

        async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            self.areas
                .lock()
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

        async fn execute_mutation(
            &self,
            area_id: &AreaId,
            envelope: &crate::mutation::MutationEnvelope,
        ) -> CloudResult<crate::mutation::MutationResult> {
            self.mutations
                .lock()
                .push((envelope.operation_id, envelope.payload.len()));
            if let Some(err) = self.fail_with.lock().clone() {
                return Err(err);
            }
            let mut areas = self.areas.lock();
            let details = areas
                .get_mut(area_id)
                .ok_or(CloudError::NotFoundOrNoAccess)?;
            // All-or-nothing like the server: apply to a working copy and
            // commit only a fully-successful envelope.
            let mut working = details.clone();
            let result = area_edits::apply_envelope(&mut working, *area_id, envelope)?;
            *details = working;
            Ok(result)
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
            format_version: crate::AREA_FORMAT_VERSION,
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
            connections: vec![],
            linked_areas: vec![],
        }
    }

    fn temp_cache_dir() -> PathBuf {
        std::env::temp_dir().join(format!("smudgy-mapper-test-{}", Uuid::new_v4()))
    }

    #[tokio::test]
    async fn import_areas_if_absent_skips_resident_names_even_when_scope_excluded() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![sample_area(a_id, "Plaza")]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        let resident_name = format!("area {a_id}");

        // Scope-exclude the resident area: invisible to identification and to
        // a filtered area listing, but it must still count as present.
        mapper.set_scope_exclusions(HashSet::new(), std::iter::once(a_id).collect());

        let mut duplicate = sample_area(AreaId(Uuid::new_v4()), "Copy");
        duplicate.area.name.clone_from(&resident_name);
        let mut fresh = sample_area(AreaId(Uuid::new_v4()), "Hall");
        fresh.area.name = "Newtown".to_string();

        let outcome = mapper
            .import_areas_if_absent(vec![duplicate.clone(), fresh.clone()])
            .await
            .expect("import");
        assert_eq!(outcome.skipped, vec![resident_name.clone()]);
        assert_eq!(outcome.added.len(), 1, "only the new name imports");

        // A repeat offer is fully absorbed: the fresh import is resident now.
        let again = mapper
            .import_areas_if_absent(vec![duplicate, fresh])
            .await
            .expect("import again");
        assert!(again.added.is_empty());
        assert_eq!(again.skipped.len(), 2);
    }

    #[tokio::test]
    async fn import_areas_if_absent_waits_for_the_initial_load() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = FixedBackend::new(vec![sample_area(a_id, "Plaza")]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());

        // Fire the presence-checked import BEFORE any areas load — the entry-
        // script ordering. It must block on the initial-load gate and then
        // compare against the loaded atlas, not the empty pre-load cache.
        let mut duplicate = sample_area(AreaId(Uuid::new_v4()), "Copy");
        duplicate.area.name = format!("area {a_id}");
        let early = tokio::spawn({
            let mapper = mapper.clone();
            async move { mapper.import_areas_if_absent(vec![duplicate]).await }
        });
        tokio::task::yield_now().await;
        assert!(!early.is_finished(), "must wait for the initial load");

        mapper.load_all_areas().await.expect("load");
        let outcome = early.await.expect("join").expect("import");
        assert!(
            outcome.added.is_empty(),
            "the resident name was only visible because the import waited"
        );
        assert_eq!(outcome.skipped.len(), 1);
    }

    /// A v2 document with one reciprocal exit pair sharing one Connection,
    /// plus an External Connection for a cross-area exit.
    fn sample_v2_document(area_id: AreaId, foreign: AreaId) -> AreaWithDetails {
        use crate::{
            Connection, ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionKind,
            ConnectionRouting, CornerStyle, PortMode, RoomSide, SegmentShape,
        };
        let pair = ConnectionId::new();
        let external = ConnectionId::new();
        let endpoint = |room: i32, side: RoomSide| ConnectionEndpoint {
            room_number: RoomNumber(room),
            side,
            port_offset: 0.5,
            port_mode: PortMode::AutoPinned,
        };
        let connection = |id: ConnectionId,
                          a: ConnectionEndpoint,
                          b: Option<ConnectionEndpoint>,
                          kind: ConnectionKind| Connection {
            id,
            endpoint_a: a,
            endpoint_b: b,
            kind,
            routing: ConnectionRouting::Simple,
            segment_shape: SegmentShape::Direct,
            corner: CornerStyle::Sharp,
            route_points: Vec::new(),
            dash: ConnectionDash::Solid,
            color: crate::DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: crate::DEFAULT_CONNECTION_THICKNESS,
        };
        let exit = |n: u128,
                    from: crate::ExitDirection,
                    to: Option<(AreaId, i32)>,
                    connection_id: ConnectionId| Exit {
            id: ExitId(Uuid::from_u128(n)),
            from_direction: from,
            to_area_id: to.map(|(area, _)| area),
            to_room_number: to.map(|(_, room)| RoomNumber(room)),
            to_direction: None,
            path: String::new(),
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight: 1.0,
            command: String::new(),
            connection_id,
            to_unknown: false,
            to_area_token: None,
            is_secret: false,
        };
        let mut details = sample_area(area_id, "Origin");
        details.rooms[0].exits = vec![
            exit(1, crate::ExitDirection::East, Some((area_id, 2)), pair),
            exit(3, crate::ExitDirection::North, Some((foreign, 1)), external),
        ];
        details.rooms.push(RoomWithDetails {
            room_number: RoomNumber(2),
            title: "Far".to_string(),
            description: String::new(),
            level: 0,
            x: 2.0,
            y: 0.0,
            color: String::new(),
            properties: vec![],
            exits: vec![exit(
                2,
                crate::ExitDirection::West,
                Some((area_id, 1)),
                pair,
            )],
            tags: std::collections::BTreeSet::default(),
            is_secret: false,
            external_id: None,
        });
        details.connections = vec![
            connection(
                pair,
                endpoint(1, RoomSide::East),
                Some(endpoint(2, RoomSide::West)),
                ConnectionKind::Internal,
            ),
            connection(
                external,
                endpoint(1, RoomSide::North),
                None,
                ConnectionKind::External,
            ),
        ];
        details
    }

    /// §8.4 import: fresh Connection identities with exits' membership kept
    /// consistent, and a dropped outside-the-set cross-area link converts
    /// its External Connection to Dangling.
    #[tokio::test]
    async fn import_remaps_connection_ids_and_repairs_dropped_links() {
        let source_id = AreaId(Uuid::new_v4());
        let foreign = AreaId(Uuid::new_v4());
        let document = sample_v2_document(source_id, foreign);
        let old_pair = document.connections[0].id;

        let backend = FixedBackend::new(vec![]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        let ids = mapper.import_areas(vec![document]).await.expect("import");
        assert_eq!(ids.len(), 1);

        let cached = mapper
            .get_current_atlas()
            .get_area(&ids[0])
            .expect("imported area cached");
        let room1 = cached.get_room(&RoomNumber(1)).expect("room 1");
        let room2 = cached.get_room(&RoomNumber(2)).expect("room 2");
        let pair_ids: Vec<_> = room1
            .get_exits()
            .iter()
            .chain(room2.get_exits().iter())
            .filter(|exit| exit.to_area_id == Some(ids[0]))
            .map(|exit| exit.connection_id)
            .collect();
        assert_eq!(pair_ids.len(), 2, "both pair members survive");
        assert_eq!(pair_ids[0], pair_ids[1], "membership stays consistent");
        assert_ne!(pair_ids[0], old_pair, "connection ids are re-minted");

        // The cross-area link pointed outside the imported set: dropped, and
        // its External Connection became Dangling.
        let dropped = room1
            .get_exits()
            .iter()
            .find(|exit| exit.to_area_id.is_none())
            .expect("dropped link became dangling")
            .connection_id;
        let dangling = cached
            .get_room_connections()
            .iter()
            .find(|rc| rc.connection_id == dropped)
            .expect("the dangling Connection renders");
        assert_eq!(dangling.kind, crate::ConnectionKind::Dangling);
    }

    /// §8.4 import: invariant violations reject the whole import before any
    /// write.
    #[tokio::test]
    async fn import_rejects_invariant_violations_wholesale() {
        let backend = FixedBackend::new(vec![]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // (a) an exit referencing a connection the document does not carry;
        let broken_id = AreaId(Uuid::new_v4());
        let mut broken = sample_v2_document(broken_id, AreaId(Uuid::new_v4()));
        broken.rooms[0].exits[0].connection_id = crate::ConnectionId::new();
        // (b) a healthy sibling that must NOT import alongside it.
        let healthy = sample_v2_document(AreaId(Uuid::new_v4()), AreaId(Uuid::new_v4()));

        let err = mapper
            .import_areas(vec![healthy, broken])
            .await
            .expect_err("a broken document rejects the whole import");
        assert!(
            err.to_string().contains("references connection"),
            "the rejection names the violation: {err}"
        );
        assert!(
            mapper.get_current_atlas().areas().next().is_none(),
            "nothing imported"
        );

        // Member-count violation: an orphan Connection row.
        let orphan_id = AreaId(Uuid::new_v4());
        let mut orphan = sample_v2_document(orphan_id, AreaId(Uuid::new_v4()));
        orphan.connections.push(crate::Connection {
            id: crate::ConnectionId::new(),
            ..orphan.connections[0].clone()
        });
        let err = mapper
            .import_areas(vec![orphan])
            .await
            .expect_err("an orphan Connection rejects the import");
        assert!(err.to_string().contains("member exits"), "{err}");
    }

    /// §8.4 import surface: a v1 document deserializes through
    /// [`AreaImportDocument`] (migrating on the way in), and a newer format
    /// is rejected at the boundary.
    #[tokio::test]
    async fn import_document_dispatches_on_format_version() {
        let area_id = AreaId(Uuid::new_v4());
        let v1 = serde_json::json!({
            "id": area_id.0,
            "user_id": null,
            "atlas_id": null,
            "name": "Legacy Import",
            "created_at": "2025-01-01T00:00:00Z",
            "rev": 2,
            "properties": [],
            "rooms": [{
                "room_number": 1, "title": "Hall", "description": "",
                "level": 0, "x": 0.0, "y": 0.0, "color": "", "properties": [],
                "exits": [{
                    "id": Uuid::from_u128(0xAB), "from_direction": "North",
                    "to_area_id": null, "to_room_number": null, "to_direction": null,
                    "path": "", "is_hidden": false, "is_closed": false,
                    "is_locked": false, "weight": 1.0, "command": "",
                    "style": "Stub", "color": "#224466"
                }]
            }],
            "labels": [],
            "shapes": []
        });
        let document: AreaImportDocument =
            serde_json::from_value(v1).expect("a v1 document is accepted via migration");
        let migrated = document.into_inner();
        assert_eq!(migrated.format_version, crate::AREA_FORMAT_VERSION);
        assert_eq!(migrated.connections.len(), 1);
        assert_eq!(
            migrated.connections[0].routing,
            crate::ConnectionRouting::Stub
        );
        assert_eq!(migrated.connections[0].color, "#224466");

        let backend = FixedBackend::new(vec![]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        let ids = mapper.import_areas(vec![migrated]).await.expect("import");
        assert_eq!(ids.len(), 1, "the migrated document imports cleanly");

        let v3 = serde_json::json!({ "format_version": 3, "id": Uuid::new_v4(), "name": "Future" });
        let err = serde_json::from_value::<AreaImportDocument>(v3)
            .expect_err("a newer format is rejected without partial import");
        assert!(err.to_string().contains("newer than this client"), "{err}");
    }

    /// §8.4 export: connections ride out stably id-sorted.
    #[tokio::test]
    async fn export_sorts_connections_by_id() {
        let area_id = AreaId(Uuid::new_v4());
        let mut details = sample_v2_document(area_id, AreaId(Uuid::new_v4()));
        details.connections.sort_by_key(|connection| connection.id);
        details.connections.reverse(); // serve them deliberately unsorted
        let backend = FixedBackend::new(vec![details]);
        let mapper = Mapper::new(Arc::new(backend), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        let exported = mapper.export_area(area_id).await.expect("export");
        let ids: Vec<_> = exported.connections.iter().map(|c| c.id).collect();
        let mut sorted = ids.clone();
        sorted.sort();
        assert_eq!(ids, sorted, "export orders connections stably by id");
    }

    #[tokio::test]
    async fn disabled_set_survives_unrelated_mutation() {
        let a_id = AreaId(Uuid::new_v4());
        let b_id = AreaId(Uuid::new_v4());
        let backend =
            FixedBackend::new(vec![sample_area(a_id, "Plaza"), sample_area(b_id, "Plaza")]);
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
            connection_id: crate::ConnectionId::new(),
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
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
            properties: vec![],
            rooms,
            labels: vec![],
            shapes: vec![],
            connections: vec![],
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
        assert_eq!(
            by_title,
            vec![a_id],
            "the scope-excluded stock zone drops out"
        );

        // The manual axis is untouched: B is still "enabled", disabled set empty.
        assert!(mapper.is_area_enabled(&b_id));
        assert!(mapper.disabled_areas().is_empty());
        assert!(
            !mapper.is_area_included(&b_id),
            "but it no longer participates"
        );
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

        assert!(
            mapper.is_area_included(&area_id),
            "ephemeral areas are never scope-excluded"
        );
        let by_title: Vec<AreaId> = mapper
            .get_current_atlas()
            .get_rooms_by_title("Wilderness")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![area_id]);
    }

    /// Polls until `condition` holds — the mutation worker drains queues on
    /// its own task, so tests wait for the store to settle.
    async fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..1000u32 {
            if condition() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(condition(), "condition not met within timeout");
    }

    #[tokio::test]
    async fn content_edit_enqueues_and_worker_drains_to_acknowledgement() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        assert_eq!(mapper.inner.pending.confirmed_rev(a_id).0, Some(1));

        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(2)),
            RoomUpdates {
                title: Some("Annex".to_string()),
                ..RoomUpdates::default()
            },
        );

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        // The backend applied one envelope and the confirmed revision moved.
        assert_eq!(backend.mutations.lock().len(), 1);
        assert_eq!(mapper.inner.pending.confirmed_rev(a_id).0, Some(2));
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 1);
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        // The optimistic room stays displayed and the backend stored it.
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(2)))
                .is_some()
        );
        assert!(
            backend.areas.lock()[&a_id]
                .rooms
                .iter()
                .any(|room| room.room_number == RoomNumber(2))
        );
    }

    #[tokio::test]
    async fn revision_conflict_refetches_replays_and_resends_the_same_receipt() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Someone else's edit moves the backend past our confirmed revision.
        backend.areas.lock().get_mut(&a_id).expect("area").area.rev = 2;

        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(2)),
            RoomUpdates {
                title: Some("Annex".to_string()),
                ..RoomUpdates::default()
            },
        );

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        // The first send conflicted; the refetch replayed the pending edit
        // cleanly and the identical receipt went out again.
        let mutations = backend.mutations.lock().clone();
        assert_eq!(mutations.len(), 2, "conflicted send plus the resend");
        assert_eq!(
            mutations[0].0, mutations[1].0,
            "the resend carries the same operation id"
        );
        assert_eq!(mapper.inner.pending.confirmed_rev(a_id).0, Some(3));
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
    }

    #[tokio::test]
    async fn new_room_number_taken_during_conflict_pauses_instead_of_overwriting() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // The stale client chose room 2 while it was vacant. Before its
        // envelope reached the server, another editor created room 2.
        {
            let mut areas = backend.areas.lock();
            let details = areas.get_mut(&a_id).expect("area");
            details.area.rev = 2;
            details.rooms.push(RoomWithDetails {
                room_number: RoomNumber(2),
                title: "Remote room".to_string(),
                description: String::new(),
                level: 0,
                x: 8.0,
                y: 3.0,
                color: String::new(),
                properties: vec![],
                exits: vec![],
                tags: Default::default(),
                is_secret: false,
                external_id: None,
            });
        }

        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(2)),
            RoomUpdates {
                title: Some("My room".to_string()),
                ..RoomUpdates::default()
            },
        );

        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;

        let displayed = mapper
            .get_current_atlas()
            .get_room(&RoomKey::new(a_id, RoomNumber(2)))
            .expect("fresh remote room is displayed");
        assert_eq!(displayed.get_title(), "Remote room");
        assert_eq!(backend.mutations.lock().len(), 1, "no automatic resend");

        mapper.resolve_conflict(a_id, false).await;
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        assert_eq!(
            backend.areas.lock()[&a_id].rooms[1].title,
            "Remote room",
            "Keep theirs preserves the remotely-created room"
        );
    }

    #[tokio::test]
    async fn keep_mine_explicitly_overrides_a_taken_new_room_number() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        {
            let mut areas = backend.areas.lock();
            let details = areas.get_mut(&a_id).expect("area");
            details.area.rev = 2;
            details.rooms.push(RoomWithDetails {
                room_number: RoomNumber(2),
                title: "Remote room".to_string(),
                description: String::new(),
                level: 0,
                x: 8.0,
                y: 3.0,
                color: String::new(),
                properties: vec![],
                exits: vec![],
                tags: Default::default(),
                is_secret: false,
                external_id: None,
            });
        }
        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(2)),
            RoomUpdates {
                title: Some("My room".to_string()),
                ..RoomUpdates::default()
            },
        );
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;

        mapper.resolve_conflict(a_id, true).await;
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        assert_eq!(backend.areas.lock()[&a_id].rooms[1].title, "My room");
        assert_eq!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(2)))
                .expect("kept room displayed")
                .get_title(),
            "My room"
        );
    }

    #[tokio::test]
    async fn room_edit_does_not_recreate_a_room_deleted_during_conflict() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        {
            let mut areas = backend.areas.lock();
            let details = areas.get_mut(&a_id).expect("area");
            details.area.rev = 2;
            details.rooms.clear();
        }

        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(1)),
            RoomUpdates {
                title: Some("My edit".to_string()),
                ..RoomUpdates::default()
            },
        );
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;
        assert!(backend.areas.lock()[&a_id].rooms.is_empty());

        mapper.resolve_conflict(a_id, false).await;
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(1)))
                .is_none()
        );
    }

    #[tokio::test]
    async fn conflicting_delete_pauses_for_review_and_keep_theirs_discards() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Someone else already deleted room 1 (and the area moved on), so
        // our pending delete fails the structural sanity check on refetch.
        {
            let mut areas = backend.areas.lock();
            let details = areas.get_mut(&a_id).expect("area");
            details.area.rev = 2;
            details.rooms.clear();
        }

        mapper.delete_room(RoomKey::new(a_id, RoomNumber(1)));

        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;

        // Keep theirs: the delete is discarded and the queue drains.
        mapper.resolve_conflict(a_id, false).await;

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;
        assert_eq!(mapper.inner.pending.total_pending(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 1);
        assert_eq!(
            stats.operations_failed(),
            1,
            "a discarded operation closes its accounting as failed"
        );
        // The display converged on the backend's truth.
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(1)))
                .is_none()
        );
        // Only the conflicted send ever reached the backend.
        assert_eq!(backend.mutations.lock().len(), 1);
    }

    #[tokio::test]
    async fn upsert_rooms_batches_into_one_envelope() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        let rooms: Vec<(RoomNumber, RoomUpdates)> = (2..=4)
            .map(|number| {
                (
                    RoomNumber(number),
                    RoomUpdates {
                        title: Some(format!("Room {number}")),
                        ..RoomUpdates::default()
                    },
                )
            })
            .collect();
        mapper.upsert_rooms(a_id, rooms);

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        let mutations = backend.mutations.lock().clone();
        assert_eq!(mutations.len(), 1, "one envelope for the whole batch");
        assert_eq!(mutations[0].1, 3, "every room rides that envelope");
        assert_eq!(mapper.get_sync_stats().operations_sent(), 1);
        assert_eq!(backend.areas.lock()[&a_id].rooms.len(), 4);
    }

    #[tokio::test]
    async fn oversized_room_batch_splits_into_capped_envelopes() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // One over the wire cap: the batch must split, not be rejected.
        let count = MAX_MUTATION_OPERATIONS + 1;
        let rooms: Vec<(RoomNumber, RoomUpdates)> = (0..count)
            .map(|index| {
                (
                    RoomNumber(i32::try_from(index).expect("small") + 2),
                    RoomUpdates {
                        title: Some(format!("Room {index}")),
                        ..RoomUpdates::default()
                    },
                )
            })
            .collect();
        mapper.upsert_rooms(a_id, rooms);

        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        let mutations = backend.mutations.lock().clone();
        assert_eq!(mutations.len(), 2, "a capped chunk plus the remainder");
        assert_eq!(mutations[0].1, MAX_MUTATION_OPERATIONS);
        assert_eq!(mutations[1].1, 1);
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 2);
        assert_eq!(stats.operations_succeeded(), 2);
        assert_eq!(stats.operations_failed(), 0);
        // Every room landed exactly once (plus the pre-existing room 1).
        assert_eq!(backend.areas.lock()[&a_id].rooms.len(), count + 1);
    }

    /// The finding-1 scenario end-to-end: the envelope that fails the
    /// post-refetch sanity check is a *later* one, and the conflict flow
    /// must target it — never the sane head.
    #[tokio::test]
    async fn later_envelope_conflict_targets_that_envelope_and_keep_theirs_spares_the_rest() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");
        let mut events = mapper.subscribe_mapper_events();

        // Someone else's edit moves the backend past our confirmed revision.
        backend.areas.lock().get_mut(&a_id).expect("area").area.rev = 2;

        // Head: a sane upsert. Follower: a delete of a room that never
        // existed, which fails the sanity check on refetch.
        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(2)),
            RoomUpdates {
                title: Some("Annex".to_string()),
                ..RoomUpdates::default()
            },
        );
        mapper.delete_room(RoomKey::new(a_id, RoomNumber(9)));
        let queued = mapper.inner.pending.pending_for(a_id);
        assert_eq!(queued.len(), 2);
        let head_id = queued[0].operation_id;
        let failing_id = queued[1].operation_id;

        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;

        // The conflict event names the failing follower, not the head.
        let conflicted = loop {
            if let MapperEvent::MutationConflict {
                operation_id,
                description,
                ..
            } = events.try_recv().expect("conflict event emitted")
            {
                break (operation_id, description);
            }
        };
        assert_eq!(conflicted.0, failing_id);
        assert_eq!(conflicted.1, "Delete room 9");
        // The sane head's optimistic effect stays displayed at the pause.
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(2)))
                .is_some(),
            "the clean prefix of the queue stays displayed"
        );

        // Keep theirs: exactly the failing envelope is discarded; the sane
        // head survives, resends under its original receipt, and lands.
        mapper.resolve_conflict(a_id, false).await;
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        assert_eq!(mapper.inner.pending.total_pending(), 0);
        let mutations = backend.mutations.lock().clone();
        assert_eq!(mutations.len(), 2, "conflicted send plus the head's resend");
        assert_eq!(
            mutations[0].0, mutations[1].0,
            "the resend carries the head's original operation id"
        );
        assert_eq!(mutations[0].0, head_id);
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 2);
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 1, "only the discarded follower");
        assert_eq!(stats.pending_operations(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        // The head's edit reached the backend; the discarded delete did not.
        assert!(
            backend.areas.lock()[&a_id]
                .rooms
                .iter()
                .any(|room| room.room_number == RoomNumber(2))
        );
    }

    /// The finding-4 scenario: Keep mine must rebuild the display with the
    /// kept operations, not leave them invisible until some later sync.
    #[tokio::test]
    async fn keep_mine_restores_kept_edits_to_the_display() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // Someone else's edit moves the backend past our confirmed revision.
        backend.areas.lock().get_mut(&a_id).expect("area").area.rev = 2;

        // Head: a delete that fails the sanity check (room 9 never
        // existed). Follower: a sane room the user expects to keep seeing.
        mapper.delete_room(RoomKey::new(a_id, RoomNumber(9)));
        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(3)),
            RoomUpdates {
                title: Some("Later".to_string()),
                ..RoomUpdates::default()
            },
        );

        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::ConflictNeedsReview
            )
        })
        .await;
        // The pause fold stopped at the failing head, hiding the follower.
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(3)))
                .is_none(),
            "precondition: the pause hid the follower's optimistic effect"
        );

        // Keep mine: by the time the resolution returns, the kept edits are
        // displayed again (fresh + all pending, best-effort).
        mapper.resolve_conflict(a_id, true).await;
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(3)))
                .is_some(),
            "keep mine restores the kept edits to the display"
        );

        // The kept head goes to the server as-is, whose verdict (room 9
        // does not exist) parks it permanently; discarding it lets the
        // sane follower drain.
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::CouldNotSave(_)
            )
        })
        .await;
        mapper.resolve_failed(a_id, false).await;
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 2);
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 1);
        assert_eq!(stats.pending_operations(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        assert!(
            backend.areas.lock()[&a_id]
                .rooms
                .iter()
                .any(|room| room.room_number == RoomNumber(3)),
            "the follower landed on the backend"
        );
        assert!(
            mapper
                .get_current_atlas()
                .get_room(&RoomKey::new(a_id, RoomNumber(3)))
                .is_some()
        );
    }

    /// Findings 2 + 8: a permanently-parked head refuses cancellation (its
    /// park already counted terminally), and the verdict-driven retry
    /// resolution reopens the accounting so every counter settles.
    #[tokio::test]
    async fn parked_head_refuses_cancel_and_retry_settles_the_accounting() {
        let a_id = AreaId(Uuid::new_v4());
        let backend = Arc::new(FixedBackend::new(vec![sample_area(a_id, "Plaza")]));
        let mapper = Mapper::new(backend.clone(), temp_cache_dir());
        mapper.load_all_areas().await.expect("load");

        // The server rejects the envelope with a permanent verdict.
        backend.fail_mutations_with(Some(CloudError::PermissionDenied(
            "read-only share".to_string(),
        )));
        mapper.upsert_room(
            RoomKey::new(a_id, RoomNumber(2)),
            RoomUpdates {
                title: Some("Annex".to_string()),
                ..RoomUpdates::default()
            },
        );
        wait_until(|| {
            matches!(
                mapper.area_save_status(a_id),
                AreaSaveStatus::CouldNotSave(_)
            )
        })
        .await;

        let head_id = mapper.inner.pending.pending_for(a_id)[0].operation_id;
        assert!(
            !mapper.cancel_pending(a_id, head_id).await,
            "a parked head resolves through resolve_failed, never cancel"
        );
        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 1);
        assert_eq!(stats.operations_failed(), 1);
        assert_eq!(stats.pending_operations(), 0, "no drift from the refusal");
        assert_eq!(mapper.inner.pending.total_pending(), 1);

        // Retry after the outage clears: the park's terminal count reopens
        // and the envelope drains normally.
        backend.fail_mutations_with(None);
        mapper.resolve_failed(a_id, true).await;
        wait_until(|| matches!(mapper.area_save_status(a_id), AreaSaveStatus::Saved)).await;

        let stats = mapper.get_sync_stats();
        assert_eq!(stats.operations_sent(), 1);
        assert_eq!(stats.operations_succeeded(), 1);
        assert_eq!(stats.operations_failed(), 0);
        assert_eq!(stats.pending_operations(), 0);
        assert!(mapper.inner.pending_by_area.lock().is_empty());
        assert_eq!(
            mapper.wait_for_sync_completion(5).await,
            Ok(true),
            "the settled counters unblock the quit-time flush"
        );
    }
}
