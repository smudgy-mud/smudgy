//! Event-driven `/sync` reconciliation engine.
//!
//! Spawned per [`Mapper`](super::Mapper) when the backend reports
//! [`supports_sync`](crate::backends::MapperBackend::supports_sync). It syncs
//! once on spawn (session start) and thereafter only when explicitly woken via
//! [`Mapper::sync_now`](super::Mapper::sync_now) — a local edit, a login, or the
//! map editor's Sync button. There is no periodic poll: the cloud is contacted
//! only on user or app action. Each tick polls the backend's sync rows and
//! reconciles the shared atlas cache:
//! refetching areas whose projected rev or access fingerprint moved, dropping
//! areas the viewer lost, and refreshing areas whose `to_unknown` exits may
//! have resolved when the row set changed. Areas with in-flight local writes
//! are never overwritten; their refetch is deferred to a later tick.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use log::warn;

use super::{Inner, area_cache::AreaCache};
use crate::{
    AreaId, CloudError, CloudResult, SyncRow,
    backends::{LEGACY_ACCESS_FINGERPRINT, MapperBackend},
};

/// Coarse state of the sync engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncState {
    /// Synced; idle until the next sync request.
    Idle,
    /// A tick is currently running.
    Syncing,
    /// The last tick failed at the transport level; retry with `sync_now`
    /// (e.g. the map editor's Sync button) — there is no automatic retry.
    Offline,
    /// The credential is missing or invalid; the user must (re-)authenticate.
    LoggedOut,
    /// `/sync` is gated until the account's email is verified; solo mapping
    /// keeps syncing through the `list_areas` fallback meanwhile.
    EmailUnverified,
    /// The server rejected this client as too old (426). Terminal until the
    /// user updates — no retry helps, so the engine stops reporting `Offline`.
    UpgradeRequired,
    /// The backend has no sync support; the engine was never spawned.
    Disabled,
}

/// Snapshot of the sync engine's status, readable via
/// [`Mapper::sync_status`](super::Mapper::sync_status).
#[derive(Debug, Clone)]
pub struct SyncStatus {
    pub state: SyncState,
    /// Human-readable description of the most recent failure, if any.
    pub last_error: Option<String>,
    /// When the last successful tick completed.
    pub last_sync: Option<Instant>,
}

/// Spawns the polling task on the current tokio runtime. The task holds only
/// a weak reference to the mapper internals and exits once they are dropped.
pub(super) fn spawn(inner: &Arc<Inner>) {
    let weak = Arc::downgrade(inner);
    let notify = inner.sync_notify.clone();

    tokio::spawn(async move {
        let mut engine = Engine::new();

        loop {
            {
                let Some(inner) = weak.upgrade() else { break };
                engine.tick(&inner).await;
            }

            // Sync once on spawn (above = session start), then wait for an
            // explicit request: a local edit, a login, or the map editor's Sync
            // button all call `Mapper::sync_now`. No periodic timer — the cloud
            // is contacted only on user or app action.
            notify.notified().await;
        }
    });
}

/// Per-task reconciliation state.
struct Engine {
    /// The row set as of the last fully-applied tick; ids whose refetch was
    /// deferred are kept dirty here so the next tick retries them.
    prev_rows: HashMap<AreaId, SyncRow>,
    /// Credential generation last observed; `None` forces the first tick to
    /// resolve the viewer identity.
    last_auth_generation: Option<u64>,
}

impl Engine {
    fn new() -> Self {
        Self {
            prev_rows: HashMap::new(),
            last_auth_generation: None,
        }
    }

    async fn tick(&mut self, inner: &Inner) {
        set_state(inner, SyncState::Syncing);

        let auth_generation = inner.backend.auth_generation();
        if self.last_auth_generation != Some(auth_generation) {
            // Credential changed (or first tick): the viewer may differ, so
            // forget prior rows — a full resync follows naturally — and let
            // the caching layer re-namespace its disk cache. The generation is
            // only consumed once identity resolution succeeds; otherwise the
            // next tick retries before any reconciliation can write into (or
            // purge) the wrong viewer's namespace.
            self.prev_rows.clear();
            match inner.backend.viewer_identity().await {
                // `Ok(None)` (no identity support) and uniform-404 (old server
                // without /me) both mean "no viewer namespace" — proceed.
                Ok(_) | Err(CloudError::NotFoundOrNoAccess) => {
                    self.last_auth_generation = Some(auth_generation);
                }
                Err(err) => {
                    warn!("Failed to resolve viewer identity: {err}");
                    Self::record_failure(inner, &err);
                    return;
                }
            }
        }

        // Snapshot the atlas membership *before* fetching rows: every area in
        // this set that the fresh row set doesn't cover gets pruned, which
        // (a) removes the previous account's areas after a credential switch
        // and (b) repairs any membership drift (e.g. a concurrent
        // `load_all_areas` re-inserting a just-revoked area). Areas inserted
        // concurrently with this tick are not in the snapshot and are spared.
        let pre_fetch_ids: HashSet<AreaId> = inner
            .atlas_cache
            .load()
            .areas()
            .map(|area| *area.get_id())
            .collect();

        let (rows, email_unverified) = match resolve_rows(&*inner.backend).await {
            Ok(resolved) => resolved,
            Err(err) => {
                Self::record_failure(inner, &err);
                return;
            }
        };

        self.reconcile(inner, &rows, &pre_fetch_ids).await;

        let state = if email_unverified {
            SyncState::EmailUnverified
        } else {
            SyncState::Idle
        };
        inner.sync_status.store(Arc::new(SyncStatus {
            state,
            last_error: None,
            last_sync: Some(Instant::now()),
        }));
    }

    fn record_failure(inner: &Inner, err: &CloudError) {
        // A failed tick surfaces a state but never schedules a retry — the
        // engine is event-driven, so the user (or the next edit/login) drives
        // the next attempt via `sync_now`.
        if err.is_upgrade_required() {
            set_status(inner, SyncState::UpgradeRequired, Some(err.to_string()));
        } else if err.is_auth_error() {
            set_status(inner, SyncState::LoggedOut, Some(err.to_string()));
        } else if err.is_transport_error() {
            set_status(inner, SyncState::Offline, Some(err.to_string()));
        } else {
            warn!("Sync tick failed: {err}");
            set_status(inner, SyncState::Idle, Some(err.to_string()));
        }
    }

    /// Diffs `rows` against the previous tick and applies the changes to the
    /// backend caches and the shared atlas cache. `pre_fetch_ids` is the
    /// atlas membership snapshotted before the row fetch: anything in it that
    /// the fresh row set no longer covers is removed, even when the previous
    /// row state never knew about it (account switch, concurrent full loads).
    async fn reconcile(&mut self, inner: &Inner, rows: &[SyncRow], pre_fetch_ids: &HashSet<AreaId>) {
        let prev = std::mem::take(&mut self.prev_rows);
        let new_rows: HashMap<AreaId, SyncRow> =
            rows.iter().map(|row| (row.area_id, row.clone())).collect();

        let removed: Vec<AreaId> = prev
            .keys()
            .chain(pre_fetch_ids.iter())
            .filter(|id| !new_rows.contains_key(id))
            .collect::<HashSet<_>>()
            .into_iter()
            .copied()
            .collect();

        // (id, fingerprint_changed) pairs needing a refetch.
        let mut to_refetch: Vec<(AreaId, bool)> = Vec::new();
        let mut row_set_changed = !removed.is_empty();

        for row in rows {
            if let Some(prev_row) = prev.get(&row.area_id) {
                let fingerprint_changed = prev_row.access_fingerprint != row.access_fingerprint;
                if fingerprint_changed {
                    row_set_changed = true;
                }
                if fingerprint_changed || prev_row.rev != row.rev {
                    to_refetch.push((row.area_id, fingerprint_changed));
                }
            } else {
                row_set_changed = true;
                to_refetch.push((row.area_id, false));
            }
        }

        for area_id in &removed {
            inner.backend.purge_area(area_id).await;
            if inner.atlas_cache.load().get_area(area_id).is_some() {
                inner
                    .atlas_cache
                    .rcu(|cache| Arc::new(cache.delete_area(*area_id)));
                inner.sync_revision.fetch_add(1, Ordering::AcqRel);
            }
        }

        if !to_refetch.is_empty() {
            // One batch update so the refetches below (and any concurrent
            // get_area callers) miss the now-stale cached copies.
            inner.backend.note_sync_rows(rows).await;
        }

        // Ids whose refetch was put off: `deferred` keeps the old prev row
        // (the rev/fingerprint delta re-triggers next tick), `dirtied` drops
        // the id entirely so it re-presents as newly added.
        let mut deferred: HashSet<AreaId> = HashSet::new();
        let mut dirtied: HashSet<AreaId> = HashSet::new();
        let mut refreshed: HashSet<AreaId> = HashSet::new();

        for (area_id, fingerprint_changed) in &to_refetch {
            if *fingerprint_changed {
                // Capability flip: the cached bytes may hold secrets the
                // viewer just lost — drop them before anything else, from
                // the UI-facing atlas too. A deferred or failed refetch must
                // blank the area rather than keep rendering the old
                // projection (the refetch re-adds it on success).
                inner.backend.purge_area(area_id).await;
                inner
                    .atlas_cache
                    .rcu(|cache| Arc::new(cache.delete_area(*area_id)));
            }
            if pending_writes(inner, area_id) > 0 || !refetch_area(inner, area_id).await {
                deferred.insert(*area_id);
            } else {
                refreshed.insert(*area_id);
            }
        }

        if row_set_changed {
            // A row-set change can turn `to_unknown` links real (or hide
            // them) without the host area's own rev moving; refresh every
            // cached area that still points at an unknown destination — and,
            // in the other direction, every area holding a *real* link into a
            // target that just vanished from the row set (its exits must
            // re-redact to `to_unknown`; the raw UUIDs may not linger).
            let removed_set: HashSet<AreaId> = removed.iter().copied().collect();
            let snapshot = inner.atlas_cache.load_full();
            let stale: Vec<AreaId> = snapshot
                .areas()
                .filter(|area| {
                    let id = area.get_id();
                    new_rows.contains_key(id)
                        && !refreshed.contains(id)
                        && !deferred.contains(id)
                        && (has_unknown_exit(area) || has_exit_into(area, &removed_set))
                })
                .map(|area| *area.get_id())
                .collect();

            for area_id in stale {
                if pending_writes(inner, &area_id) > 0 {
                    dirtied.insert(area_id);
                    continue;
                }
                // The area's own row didn't move, so the caching layer would
                // happily serve the stale copy; purge to force a remote fetch.
                inner.backend.purge_area(&area_id).await;
                if !refetch_area(inner, &area_id).await {
                    dirtied.insert(area_id);
                }
            }
        }

        self.prev_rows = new_rows;
        for area_id in &deferred {
            match prev.get(area_id) {
                Some(row) => {
                    self.prev_rows.insert(*area_id, row.clone());
                }
                None => {
                    self.prev_rows.remove(area_id);
                }
            }
        }
        for area_id in &dirtied {
            self.prev_rows.remove(area_id);
        }
    }
}

/// Resolves the authoritative row set, falling back to `list_areas` synthesis
/// when `/sync` is unsupported (older server) or gated behind email
/// verification — `GET /areas` has no verified gate, so solo mapping keeps
/// syncing. The boolean is true when the server reported `email_not_verified`.
async fn resolve_rows(backend: &dyn MapperBackend) -> CloudResult<(Vec<SyncRow>, bool)> {
    match backend.sync_state().await {
        Ok(Some(rows)) => Ok((rows, false)),
        Ok(None) | Err(CloudError::NotFoundOrNoAccess) => Ok((synthesize_rows(backend).await?, false)),
        Err(CloudError::EmailNotVerified) => Ok((synthesize_rows(backend).await?, true)),
        Err(err) => Err(err),
    }
}

/// Builds sync rows from `list_areas`, fingerprinting access blocks
/// client-side (the legacy sentinel stands in when the server sends none).
async fn synthesize_rows(backend: &dyn MapperBackend) -> CloudResult<Vec<SyncRow>> {
    let areas = backend.list_areas().await?;
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

/// Fetches an area and swaps it into the atlas cache, bumping the sync
/// revision. The swap is suppressed when the server's content hash proves the
/// projection is byte-identical to the cached copy (no re-render needed).
/// Returns false when the fetch failed and should be retried next tick.
async fn refetch_area(inner: &Inner, area_id: &AreaId) -> bool {
    let details = match inner.backend.get_area(area_id).await {
        Ok(details) => details,
        Err(err) => {
            warn!("Sync refetch of area {area_id} failed: {err}");
            return false;
        }
    };

    // Suppress the swap only when the projection is byte-identical AND the access
    // block is unchanged. An ownership transfer (or a flag change) can leave the
    // projected content identical while `access` flips (is_owner true→false, can_admin
    // toggling, …); the access block drives owner/admin affordance gating, so reconcile
    // it independently of the content hash — otherwise a transferred-away former owner
    // keeps stale "owned" affordances until a full reload.
    let unchanged = details.content_hash.is_some()
        && inner
            .atlas_cache
            .load()
            .get_area(area_id)
            .is_some_and(|cached| {
                let meta = cached.meta();
                meta.content_hash == details.content_hash && meta.access == details.area.access
            });
    if unchanged {
        return true;
    }

    let area_cache = Arc::new(AreaCache::new_with_area(details));
    inner
        .atlas_cache
        .rcu(|cache| Arc::new(cache.insert_area(*area_id, area_cache.clone())));
    inner.sync_revision.fetch_add(1, Ordering::AcqRel);
    true
}

fn has_unknown_exit(area: &AreaCache) -> bool {
    area.get_rooms()
        .iter()
        .any(|room| room.get_exits().iter().any(|exit| exit.to_unknown))
}

/// True when any exit in `area` points at one of `targets` (used to re-redact
/// hosts whose link target just left the viewer's row set).
fn has_exit_into(area: &AreaCache, targets: &HashSet<AreaId>) -> bool {
    if targets.is_empty() {
        return false;
    }
    area.get_rooms().iter().any(|room| {
        room.get_exits()
            .iter()
            .any(|exit| exit.to_area_id.is_some_and(|id| targets.contains(&id)))
    })
}

fn pending_writes(inner: &Inner, area_id: &AreaId) -> u64 {
    inner
        .pending_by_area
        .lock()
        .get(area_id)
        .copied()
        .unwrap_or(0)
}

fn set_state(inner: &Inner, state: SyncState) {
    let previous = inner.sync_status.load();
    inner.sync_status.store(Arc::new(SyncStatus {
        state,
        last_error: previous.last_error.clone(),
        last_sync: previous.last_sync,
    }));
}

fn set_status(inner: &Inner, state: SyncState, last_error: Option<String>) {
    let last_sync = inner.sync_status.load().last_sync;
    inner.sync_status.store(Arc::new(SyncStatus {
        state,
        last_error,
        last_sync,
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Area, AreaAccess, AreaUpdates, AreaWithDetails, CreateAreaRequest, Exit, ExitArgs,
        ExitDirection, ExitId, ExitStyle, ExitUpdates, Label, LabelArgs, LabelId, LabelUpdates,
        Room, RoomNumber, RoomUpdates, RoomWithDetails, Shape, ShapeArgs, ShapeId, ShapeUpdates,
        mapper::{Mapper, RoomKey},
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use parking_lot::Mutex;
    use std::path::PathBuf;
    use tokio::sync::Semaphore;
    use uuid::Uuid;

    /// Backend with scripted `/sync` + `get_area` responses, recording the
    /// calls the engine makes (follows the `MockBackend` pattern in
    /// `backends::cached` tests).
    #[derive(Clone)]
    struct ScriptedBackend {
        sync_rows: Arc<Mutex<CloudResult<Option<Vec<SyncRow>>>>>,
        areas: Arc<Mutex<HashMap<AreaId, AreaWithDetails>>>,
        get_calls: Arc<Mutex<Vec<AreaId>>>,
        purge_calls: Arc<Mutex<Vec<AreaId>>>,
        /// When set, `update_area` blocks until the test adds a permit.
        update_gate: Arc<Mutex<Option<Arc<Semaphore>>>>,
        /// Scripted credential generation (bump to simulate login/logout).
        auth_gen: Arc<Mutex<u64>>,
    }

    impl ScriptedBackend {
        fn new() -> Self {
            Self {
                sync_rows: Arc::new(Mutex::new(Ok(Some(Vec::new())))),
                areas: Arc::new(Mutex::new(HashMap::new())),
                get_calls: Arc::new(Mutex::new(Vec::new())),
                purge_calls: Arc::new(Mutex::new(Vec::new())),
                update_gate: Arc::new(Mutex::new(None)),
                auth_gen: Arc::new(Mutex::new(0)),
            }
        }

        fn set_rows(&self, rows: Vec<SyncRow>) {
            *self.sync_rows.lock() = Ok(Some(rows));
        }

        fn set_sync_error(&self, err: CloudError) {
            *self.sync_rows.lock() = Err(err);
        }

        fn put_area(&self, area: AreaWithDetails) {
            self.areas.lock().insert(area.area.id, area);
        }

        fn get_count(&self, area_id: &AreaId) -> usize {
            self.get_calls.lock().iter().filter(|id| *id == area_id).count()
        }

        fn purged(&self, area_id: &AreaId) -> bool {
            self.purge_calls.lock().contains(area_id)
        }

        fn row_for(area: &AreaWithDetails) -> SyncRow {
            SyncRow {
                area_id: area.area.id,
                rev: area.area.rev,
                access_fingerprint: area.area.access.map_or_else(
                    || LEGACY_ACCESS_FINGERPRINT.to_string(),
                    |access| access.fingerprint(),
                ),
            }
        }
    }

    #[async_trait]
    impl MapperBackend for ScriptedBackend {
        async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
            Err(CloudError::NetworkError("not needed".to_string()))
        }

        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            Ok(self
                .areas
                .lock()
                .values()
                .map(|area| area.area.clone())
                .collect())
        }

        async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            self.get_calls.lock().push(*area_id);
            self.areas
                .lock()
                .get(area_id)
                .cloned()
                .ok_or(CloudError::NotFoundOrNoAccess)
        }

        async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
            self.sync_rows.lock().clone()
        }

        async fn purge_area(&self, area_id: &AreaId) {
            self.purge_calls.lock().push(*area_id);
        }

        fn supports_sync(&self) -> bool {
            true
        }

        fn auth_generation(&self) -> u64 {
            *self.auth_gen.lock()
        }

        async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
            let gate = self.update_gate.lock().clone();
            if let Some(gate) = gate {
                let permit = gate.acquire().await.expect("gate closed");
                permit.forget();
            }
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
            Err(CloudError::NetworkError("not needed".to_string()))
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
            Err(CloudError::NetworkError("not needed".to_string()))
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
            Err(CloudError::NetworkError("not needed".to_string()))
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

    fn sample_area(
        area_id: AreaId,
        rev: i64,
        access: Option<AreaAccess>,
        content_hash: Option<&str>,
    ) -> AreaWithDetails {
        AreaWithDetails {
            area: Area {
                id: area_id,
                user_id: None,
                atlas_id: None,
                atlas_name: None,
                name: format!("Area rev {rev}"),
                created_at: Utc::now(),
                rev,
                access,
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
            },
            content_hash: content_hash.map(ToString::to_string),
            properties: vec![],
            rooms: vec![],
            labels: vec![],
            shapes: vec![],
            linked_areas: vec![],
        }
    }

    fn room_with_exit(to_area_id: Option<AreaId>, to_unknown: bool) -> RoomWithDetails {
        RoomWithDetails {
            room_number: RoomNumber(1),
            external_id: None,
            title: "Room".to_string(),
            description: String::new(),
            level: 0,
            x: 0.0,
            y: 0.0,
            color: String::new(),
            properties: vec![],
            exits: vec![Exit {
                id: ExitId(Uuid::new_v4()),
                from_direction: ExitDirection::North,
                to_area_id,
                to_room_number: None,
                to_direction: None,
                path: String::new(),
                is_hidden: false,
                is_closed: false,
                is_locked: false,
                weight: 1.0,
                command: String::new(),
                style: ExitStyle::Normal,
                color: String::new(),
                to_unknown,
                to_area_token: to_unknown.then(|| "token-1".to_string()),
                is_secret: false,
            }],
            tags: Default::default(),
            is_secret: false,
        }
    }

    fn temp_cache_dir() -> PathBuf {
        std::env::temp_dir().join(format!("smudgy-sync-engine-test-{}", Uuid::new_v4()))
    }

    async fn wait_until(mut condition: impl FnMut() -> bool) {
        for _ in 0..1000u32 {
            if condition() {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        }
        assert!(condition(), "condition not met within timeout");
    }

    /// Builds a mapper (the engine ticks only on spawn and via
    /// [`Mapper::sync_now`]) and settles the immediate startup tick so later
    /// script changes cannot race a tick in flight.
    async fn new_mapper(backend: &ScriptedBackend) -> Mapper {
        let mapper = Mapper::new(Arc::new(backend.clone()), temp_cache_dir());
        wait_until(|| mapper.sync_status().last_sync.is_some()).await;
        mapper
    }

    /// Forces one full tick and waits for it to complete.
    async fn tick(mapper: &Mapper) {
        let before = mapper.sync_status().last_sync;
        mapper.sync_now();
        wait_until(|| mapper.sync_status().last_sync != before).await;
    }

    #[tokio::test]
    async fn grant_appearing_lands_area_in_atlas_cache() {
        let backend = ScriptedBackend::new();
        let mapper = new_mapper(&backend).await;

        let area_id = AreaId(Uuid::new_v4());
        assert!(mapper.get_current_atlas().get_area(&area_id).is_none());

        let area = sample_area(area_id, 1, Some(SHARED_VIEW), Some("h1"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        tick(&mapper).await;

        let cached = mapper
            .get_current_atlas()
            .get_area(&area_id)
            .expect("area should land in the atlas cache");
        assert_eq!(cached.get_rev(), 1);
        assert_eq!(cached.meta().access, Some(SHARED_VIEW));
        assert_eq!(mapper.sync_status().state, SyncState::Idle);
    }

    #[tokio::test]
    async fn rev_change_refetches_but_equal_content_hash_skips_swap() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 1, Some(SHARED_VIEW), Some("same-hash"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        let mapper = new_mapper(&backend).await;
        assert_eq!(backend.get_count(&area_id), 1);
        let revision_before = mapper.sync_revision();

        // Server rev moves but the projected content is byte-identical.
        let updated = sample_area(area_id, 2, Some(SHARED_VIEW), Some("same-hash"));
        backend.put_area(updated.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&updated)]);

        tick(&mapper).await;

        assert_eq!(backend.get_count(&area_id), 2, "rev change must refetch");
        assert_eq!(
            mapper.sync_revision(),
            revision_before,
            "equal content hash must suppress the atlas swap"
        );
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.get_rev(), 1, "old projection must remain cached");
    }

    /// Opaque revs: a *downward* move must still refetch.
    #[tokio::test]
    async fn rev_moving_backwards_refetches() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 5, Some(SHARED_VIEW), Some("h5"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        let mapper = new_mapper(&backend).await;
        assert_eq!(backend.get_count(&area_id), 1);

        let updated = sample_area(area_id, 3, Some(SHARED_VIEW), Some("h3"));
        backend.put_area(updated.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&updated)]);

        tick(&mapper).await;

        assert_eq!(backend.get_count(&area_id), 2);
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.get_rev(), 3);
    }

    #[tokio::test]
    async fn fingerprint_change_purges_then_refetches() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 1, Some(SHARED_VIEW), Some("h1"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        let mapper = new_mapper(&backend).await;
        assert!(!backend.purged(&area_id));
        assert_eq!(backend.get_count(&area_id), 1);

        // Capability flip: same rev, different access fingerprint.
        let updated = sample_area(area_id, 1, Some(SHARED_EDIT), Some("h2"));
        backend.put_area(updated.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&updated)]);

        tick(&mapper).await;

        assert!(
            backend.purged(&area_id),
            "fingerprint change must purge possibly-secret cached bytes"
        );
        assert_eq!(backend.get_count(&area_id), 2);
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.meta().access, Some(SHARED_EDIT));
    }

    #[tokio::test]
    async fn vanished_row_removes_area_from_atlas_cache() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 1, Some(SHARED_VIEW), Some("h1"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        let mapper = new_mapper(&backend).await;
        assert!(mapper.get_current_atlas().get_area(&area_id).is_some());

        backend.set_rows(vec![]);
        tick(&mapper).await;

        assert!(
            mapper.get_current_atlas().get_area(&area_id).is_none(),
            "revoked area must leave the atlas cache"
        );
        assert!(backend.purged(&area_id));
    }

    #[tokio::test]
    async fn row_set_change_refetches_cached_areas_with_unknown_exits() {
        let backend = ScriptedBackend::new();
        let area_a = AreaId(Uuid::new_v4());
        let area_b = AreaId(Uuid::new_v4());

        let mut a = sample_area(area_a, 1, Some(SHARED_VIEW), Some("a1"));
        a.rooms = vec![room_with_exit(None, true)];
        backend.put_area(a.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&a)]);

        let mapper = new_mapper(&backend).await;
        assert_eq!(backend.get_count(&area_a), 1);

        // Area B becomes visible; A's own rev does not move, but its unknown
        // link now resolves to B.
        let b = sample_area(area_b, 1, Some(SHARED_VIEW), Some("b1"));
        backend.put_area(b.clone());
        let mut resolved_a = sample_area(area_a, 1, Some(SHARED_VIEW), Some("a2"));
        resolved_a.rooms = vec![room_with_exit(Some(area_b), false)];
        backend.put_area(resolved_a);
        backend.set_rows(vec![
            ScriptedBackend::row_for(&a),
            ScriptedBackend::row_for(&b),
        ]);

        tick(&mapper).await;

        assert_eq!(
            backend.get_count(&area_a),
            2,
            "row-set change must refetch areas holding to_unknown exits"
        );
        assert!(backend.purged(&area_a), "stale cached copy must be purged first");
        let atlas = mapper.get_current_atlas();
        assert!(atlas.get_area(&area_b).is_some());
        let cached_a = atlas.get_area(&area_a).unwrap();
        assert!(!has_unknown_exit(&cached_a), "unknown link should be resolved");
    }

    #[tokio::test]
    async fn email_unverified_falls_back_to_list_areas_and_reports_status() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        backend.put_area(sample_area(area_id, 1, None, None));
        backend.set_sync_error(CloudError::EmailNotVerified);

        let mapper = new_mapper(&backend).await;

        assert_eq!(mapper.sync_status().state, SyncState::EmailUnverified);
        assert!(
            mapper.get_current_atlas().get_area(&area_id).is_some(),
            "list_areas fallback must keep reconciling"
        );
    }

    /// Regression test: switching accounts must remove the previous
    /// account's areas from the atlas cache, even though the engine's
    /// row-diff state was reset by the credential change.
    #[tokio::test]
    async fn account_switch_prunes_previous_accounts_areas() {
        let backend = ScriptedBackend::new();
        let area_a = AreaId(Uuid::new_v4());
        let a = sample_area(area_a, 1, Some(SHARED_VIEW), Some("a1"));
        backend.put_area(a.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&a)]);

        let mapper = new_mapper(&backend).await;
        assert!(mapper.get_current_atlas().get_area(&area_a).is_some());

        // Account switch: new credential generation, a different row set.
        let area_b = AreaId(Uuid::new_v4());
        let b = sample_area(area_b, 1, Some(SHARED_VIEW), Some("b1"));
        backend.put_area(b.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&b)]);
        *backend.auth_gen.lock() += 1;

        tick(&mapper).await;

        let atlas = mapper.get_current_atlas();
        assert!(
            atlas.get_area(&area_a).is_none(),
            "previous account's area must leave the atlas cache on switch"
        );
        assert!(atlas.get_area(&area_b).is_some());
        assert!(backend.purged(&area_a));
    }

    /// Regression test: when a linked target vanishes from the row set, host
    /// areas holding real links into it are refetched so their exits
    /// re-redact (the raw UUID may not linger in the atlas).
    #[tokio::test]
    async fn losing_link_target_refetches_host_area() {
        let backend = ScriptedBackend::new();
        let area_a = AreaId(Uuid::new_v4());
        let area_b = AreaId(Uuid::new_v4());

        let mut a = sample_area(area_a, 1, Some(SHARED_VIEW), Some("a1"));
        a.rooms = vec![room_with_exit(Some(area_b), false)];
        let b = sample_area(area_b, 1, Some(SHARED_VIEW), Some("b1"));
        backend.put_area(a.clone());
        backend.put_area(b.clone());
        backend.set_rows(vec![
            ScriptedBackend::row_for(&a),
            ScriptedBackend::row_for(&b),
        ]);

        let mapper = new_mapper(&backend).await;
        assert_eq!(backend.get_count(&area_a), 1);

        // B is revoked; the server would now serve A with that exit
        // redacted to to_unknown.
        let mut redacted_a = sample_area(area_a, 1, Some(SHARED_VIEW), Some("a2"));
        redacted_a.rooms = vec![room_with_exit(None, true)];
        backend.put_area(redacted_a);
        backend.areas.lock().remove(&area_b);
        backend.set_rows(vec![ScriptedBackend::row_for(&a)]);

        tick(&mapper).await;

        assert_eq!(
            backend.get_count(&area_a),
            2,
            "host area with a real link into the lost target must refetch"
        );
        let atlas = mapper.get_current_atlas();
        assert!(atlas.get_area(&area_b).is_none());
        let cached_a = atlas.get_area(&area_a).expect("A stays");
        assert!(
            has_unknown_exit(&cached_a),
            "the link must now be redacted to unknown"
        );
        assert!(!has_exit_into(&cached_a, &std::iter::once(area_b).collect()));
    }

    #[tokio::test]
    async fn pending_local_writes_defer_refetch_until_drained() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 1, Some(SHARED_EDIT), Some("h1"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        let mapper = new_mapper(&backend).await;
        assert_eq!(backend.get_count(&area_id), 1);

        // Block the write pipeline, then queue a local rename.
        let gate = Arc::new(Semaphore::new(0));
        *backend.update_gate.lock() = Some(gate.clone());
        mapper.rename_area(area_id, "Local Edit");

        // The server moves on while our write is still in flight.
        let updated = sample_area(area_id, 2, Some(SHARED_EDIT), Some("h2"));
        backend.put_area(updated.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&updated)]);

        tick(&mapper).await;

        assert_eq!(
            backend.get_count(&area_id),
            1,
            "refetch must be deferred while local writes are pending"
        );
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.get_name(), "Local Edit", "local edit must survive");

        // Release the write; once drained the deferred refetch goes through.
        gate.add_permits(1);
        wait_until(|| mapper.get_sync_stats().pending_operations() == 0).await;

        tick(&mapper).await;

        assert_eq!(backend.get_count(&area_id), 2);
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.get_name(), "Area rev 2");
    }
}
