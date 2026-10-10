//! Event-driven `/sync` reconciliation engine.
//!
//! Spawned per [`Mapper`](super::Mapper) when the backend reports
//! [`supports_sync`](crate::backends::MapperBackend::supports_sync). It syncs
//! once on spawn (session start) and thereafter only when explicitly woken via
//! [`Mapper::sync_now`](super::Mapper::sync_now) — a local edit, a login, or the
//! map editor's Sync button. There is no periodic poll: the cloud is contacted
//! only on user or app action. Each tick polls the backend's sync rows and
//! reconciles the shared atlas cache:
//! refetching areas whose projection token moved (purging first when only
//! the caller's access changed, or when a Secret the published copy shows
//! left the row's revisions), dropping areas the viewer lost, and
//! refreshing areas whose `to_unknown` exits may have resolved when the row
//! set changed. Areas with in-flight local writes
//! are never overwritten; their refetch is deferred to a later tick.

use std::{
    collections::{HashMap, HashSet},
    sync::{Arc, atomic::Ordering},
    time::Instant,
};

use log::warn;

use super::{Inner, ReplayMode, area_cache::AreaCache};
use crate::{AreaId, CloudError, CloudResult, SourceId, SyncRow, backends::MapperBackend};

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

    let task = tokio::spawn(async move {
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
            tokio::select! {
                () = notify.notified() => {},
                result = async {
                    match engine.cloud_changes.as_mut() {
                        Some(changes) => changes.changed().await,
                        None => std::future::pending().await,
                    }
                } => if result.is_err() { engine.cloud_changes = None; },
            }
        }
    });
    inner.background_tasks.lock().push(task.abort_handle());
}

/// Per-task reconciliation state.
struct Engine {
    cloud_changes: Option<tokio::sync::watch::Receiver<u64>>,
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
            cloud_changes: None,
            prev_rows: HashMap::new(),
            last_auth_generation: None,
        }
    }

    async fn tick(&mut self, inner: &Inner) {
        if let Some(changes) = &mut self.cloud_changes {
            changes.borrow_and_update();
        }
        set_state(inner, SyncState::Syncing);

        let auth_generation = inner.backend.auth_generation();
        if self.last_auth_generation != Some(auth_generation) {
            self.cloud_changes = None;
            // Credential changed (or first tick): the viewer may differ, so
            // forget prior rows — a full resync follows naturally — and let
            // the caching layer re-namespace its disk cache. The generation is
            // only consumed once identity resolution succeeds; otherwise the
            // next tick retries before any reconciliation can write into (or
            // purge) the wrong viewer's namespace.
            self.prev_rows.clear();
            // Identity and row resolution are network operations and may
            // fail. Remove the prior credential generation's cloud maps
            // before either await; local and session maps are independent of
            // cloud identity and remain available.
            clear_cloud_projection(inner);
            match inner
                .backend
                .viewer_identity_at_generation(auth_generation)
                .await
            {
                // `Ok(None)` (no identity support) and uniform-404 (old server
                // without /me) both mean "no viewer namespace" — proceed.
                Ok(identity) => {
                    if !inner.activate_pending_viewer(identity, auth_generation) {
                        Self::record_failure(inner, &CloudError::CredentialChanged);
                        return;
                    }
                    self.last_auth_generation = Some(auth_generation);
                    self.cloud_changes = inner.backend.cloud_changes();
                }
                Err(CloudError::NotFoundOrNoAccess) => {
                    if !inner.activate_pending_viewer(None, auth_generation) {
                        Self::record_failure(inner, &CloudError::CredentialChanged);
                        return;
                    }
                    self.last_auth_generation = Some(auth_generation);
                    self.cloud_changes = inner.backend.cloud_changes();
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
        // Likewise each map's shown Secrets: a row fetched before a Secret
        // was created cannot carry it, so only a Secret shown before the
        // fetch can be judged lost by these rows.
        let pre_fetch_secrets = shown_secrets(inner);

        let (rows, email_unverified) = match resolve_rows(&*inner.backend).await {
            Ok(resolved) => resolved,
            Err(err) => {
                Self::record_failure(inner, &err);
                return;
            }
        };
        if inner.backend.auth_generation() != auth_generation {
            Self::record_failure(inner, &CloudError::CredentialChanged);
            return;
        }

        if !self
            .reconcile(
                inner,
                &rows,
                &pre_fetch_ids,
                &pre_fetch_secrets,
                auth_generation,
            )
            .await
        {
            Self::record_failure(inner, &CloudError::CredentialChanged);
            return;
        }

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
    /// `pre_fetch_secrets` is each map's shown Secrets snapshotted at the
    /// same point; only those can be judged lost by `rows`.
    async fn reconcile(
        &mut self,
        inner: &Inner,
        rows: &[SyncRow],
        pre_fetch_ids: &HashSet<AreaId>,
        pre_fetch_secrets: &HashMap<AreaId, HashSet<SourceId>>,
        auth_generation: u64,
    ) -> bool {
        if inner.backend.auth_generation() != auth_generation {
            return false;
        }
        // Local documents are adopted as one generation by local_projection.
        // An HTTP response must never publish individual pieces of that graph.
        let mut local = inner.local_projection.lock().known.clone();
        local.extend(inner.backend.local_area_ids());
        let cloud_rows: Vec<_> = rows
            .iter()
            .filter(|row| !local.contains(&row.area_id))
            .cloned()
            .collect();
        let rows = cloud_rows.as_slice();
        let pre_fetch_ids: HashSet<_> = pre_fetch_ids
            .iter()
            .filter(|id| !local.contains(id))
            .copied()
            .collect();
        self.prev_rows.retain(|id, _| !local.contains(id));
        note_rows_confirmed(inner, rows);

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

        // (id, purge_first) pairs needing a refetch. Every token change
        // refetches. Only a change that can leave something unreadable in
        // the cached copy purges it first (see `purges_first`); any other
        // keeps the copy on screen and on disk until the refetch replaces it.
        let mut to_refetch: Vec<(AreaId, bool)> = Vec::new();
        let mut row_set_changed = !removed.is_empty();

        for row in rows {
            let lost_secret = shows_lost_secret(inner, row, pre_fetch_secrets);
            if let Some(prev_row) = prev.get(&row.area_id) {
                let moved_alone = row.token_moved_alone_since(prev_row);
                if moved_alone {
                    row_set_changed = true;
                }
                if row.projection_token != prev_row.projection_token || lost_secret {
                    to_refetch.push((row.area_id, purges_first(row, moved_alone, lost_secret)));
                }
            } else {
                row_set_changed = true;
                to_refetch.push((row.area_id, lost_secret));
            }
        }
        // A live DELETE whose response was lost is frozen under a durable
        // intent even when the sync row itself is unchanged. Force a point
        // fetch so area presence can durably abort that intent.
        for area_id in inner.pending.recovery_area_ids() {
            if local.contains(&area_id) {
                continue;
            }
            if new_rows.contains_key(&area_id)
                && !to_refetch
                    .iter()
                    .any(|(candidate, _)| *candidate == area_id)
            {
                to_refetch.push((area_id, false));
            }
        }

        for area_id in &removed {
            if inner.backend.auth_generation() != auth_generation {
                return false;
            }
            inner.backend.purge_area(area_id).await;
            let _gate = inner.mutation_gate.lock();
            if inner.backend.auth_generation() != auth_generation {
                return false;
            }
            if inner.atlas_cache.load().get_area(area_id).is_some() {
                inner.publish_areas(&[], &[*area_id]);
            }
        }

        if !to_refetch.is_empty() {
            // One batch update so the refetches below (and any concurrent
            // get_area callers) miss the now-stale cached copies.
            inner.backend.note_sync_rows(rows).await;
            if inner.backend.auth_generation() != auth_generation {
                return false;
            }
        }

        // Ids whose refetch was put off: `deferred` keeps the old prev row
        // (the token delta re-triggers next tick), `dirtied` drops
        // the id entirely so it re-presents as newly added.
        let mut deferred: HashSet<AreaId> = HashSet::new();
        let mut dirtied: HashSet<AreaId> = HashSet::new();
        let mut refreshed: HashSet<AreaId> = HashSet::new();

        // Restored journal work can name an area absent from `/sync` (deleted
        // while the app was down, or access revoked). Probe it directly so
        // recovery cannot stay invisibly wedged forever. Uniform 404 is
        // surfaced as "unavailable" without claiming whether deletion or
        // authorization caused it; the durable edit remains exportable.
        let absent_recovery: Vec<_> = inner
            .pending
            .recovery_area_ids()
            .into_iter()
            .filter(|area_id| !new_rows.contains_key(area_id))
            .filter(|area_id| !local.contains(area_id))
            .collect();
        for area_id in absent_recovery {
            match refetch_area(inner, &area_id, auth_generation).await {
                Ok(true) => {
                    refreshed.insert(area_id);
                }
                Ok(false) => {
                    deferred.insert(area_id);
                }
                Err(CloudError::CredentialChanged) => return false,
                Err(
                    CloudError::NotFoundOrNoAccess
                    | CloudError::PermissionDenied(_)
                    | CloudError::AreaNotFound(_),
                ) => {
                    if let Err(error) = inner.recover_unavailable_area(area_id) {
                        warn!("Could not reconcile unavailable area {area_id}: {error}");
                        deferred.insert(area_id);
                    }
                }
                Err(err) => {
                    warn!("Recovery refetch of area {area_id} failed: {err}");
                    deferred.insert(area_id);
                }
            }
        }

        for (area_id, purge_first) in &to_refetch {
            if *purge_first {
                // A Secret left the projection, or a row without revisions
                // can't say none did: the cached bytes may hold Secrets the
                // viewer just lost — drop them before anything else, from
                // the UI-facing atlas (with every layer, route and lookup
                // built from them) and from disk. A deferred or failed
                // refetch must blank the area rather than keep rendering the
                // old projection (the refetch re-adds it on success).
                inner.backend.purge_area(area_id).await;
                let _gate = inner.mutation_gate.lock();
                if inner.backend.auth_generation() != auth_generation {
                    return false;
                }
                inner.publish_areas(&[], &[*area_id]);
            }
            let has_cached_base = inner.atlas_cache.load().get_area(area_id).is_some();
            let requires_recovery_base = inner.pending.requires_recovery_base(*area_id);
            if pending_writes(inner, area_id) > 0 && has_cached_base && !requires_recovery_base {
                deferred.insert(*area_id);
                continue;
            }
            match refetch_area(inner, area_id, auth_generation).await {
                Ok(true) => {
                    refreshed.insert(*area_id);
                }
                Ok(false) => {
                    deferred.insert(*area_id);
                }
                Err(CloudError::CredentialChanged) => return false,
                Err(err) => {
                    warn!("Sync refetch of area {area_id} failed: {err}");
                    deferred.insert(*area_id);
                }
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
                if inner.backend.auth_generation() != auth_generation {
                    return false;
                }
                match refetch_area(inner, &area_id, auth_generation).await {
                    Ok(true) => {}
                    Ok(false) => {
                        dirtied.insert(area_id);
                    }
                    Err(CloudError::CredentialChanged) => return false,
                    Err(err) => {
                        warn!("Sync refetch of area {area_id} failed: {err}");
                        dirtied.insert(area_id);
                    }
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
        inner.backend.auth_generation() == auth_generation
    }
}

fn clear_cloud_projection(inner: &Inner) {
    let _gate = inner.mutation_gate.lock();
    let mut local = inner.local_projection.lock().known.clone();
    local.extend(inner.backend.local_area_ids());
    let ephemeral = inner.backend.ephemeral_area_ids();
    let local_atlases = inner.backend.local_atlas_ids();
    inner.publish_cache(|cache| {
        let retained = cache
            .areas()
            .filter(|area| {
                let area_id = area.get_id();
                local.contains(area_id) || ephemeral.contains(area_id)
            })
            .map(|area| (*area.get_id(), area))
            .collect();
        Arc::new(cache.rebuild_with_areas(retained))
    });
    inner
        .atlas_storage_by_id
        .lock()
        .retain(|atlas_id, _| local_atlases.contains(atlas_id));
    inner
        .auth_projection_revision
        .fetch_add(1, Ordering::AcqRel);
}

/// Records every source revision the rows carry as backend truth for the
/// pending queue's preconditions.
fn note_rows_confirmed(inner: &Inner, rows: &[SyncRow]) {
    for row in rows {
        for (source, rev) in &row.revisions {
            inner.pending.note_source_rev(row.area_id, *source, *rev);
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
        Ok(None) | Err(CloudError::NotFoundOrNoAccess) => {
            Ok((synthesize_rows(backend).await?, false))
        }
        Err(CloudError::EmailNotVerified) => Ok((synthesize_rows(backend).await?, true)),
        Err(err) => Err(err),
    }
}

/// Builds sync rows from `list_areas` when `/sync` is unavailable.
async fn synthesize_rows(backend: &dyn MapperBackend) -> CloudResult<Vec<SyncRow>> {
    let areas = backend.list_areas().await?;
    Ok(areas.iter().map(SyncRow::synthesized).collect())
}

/// Fetches an area and swaps it into the atlas cache, bumping the sync
/// revision. Returns false when the fetch failed and should be retried next
/// tick.
///
/// The swap routes through the mapper's pending-replay fold rather than
/// landing the fetched document raw: refetches are deferred while an area
/// has pending writes, so the fold is normally over an empty queue, but an
/// envelope enqueued *during* the fetch must keep its optimistic effect
/// instead of vanishing under the swap.
pub(super) async fn refetch_area(
    inner: &Inner,
    area_id: &AreaId,
    auth_generation: u64,
) -> CloudResult<bool> {
    let confirmed_before_fetch = inner.pending.confirmed_source_versions(*area_id);
    let token_before_fetch = inner
        .atlas_cache
        .load()
        .get_area(area_id)
        .map(|area| area.meta().projection_token.clone());
    let cloud_area = !inner.backend.local_area_ids().contains(area_id)
        && !inner.backend.ephemeral_area_ids().contains(area_id);
    let fetched = if cloud_area {
        inner
            .backend
            .get_area_at_generation(area_id, auth_generation)
            .await
    } else {
        inner.backend.get_area(area_id).await
    };
    let details = fetched?;

    // Revision adoption, structural replay, and the cache swap share the same
    // gate as mutation compilation. A newer sync row can therefore never
    // become a send revision unless queued operations have first been checked
    // against that exact document.
    {
        let _mutation_guard = inner.mutation_gate.lock();
        if cloud_area && inner.backend.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        inner.pending.abort_recovered_delete(*area_id)?;
        let confirmed = inner.pending.confirmed_source_versions(*area_id);
        let token_now = inner
            .atlas_cache
            .load()
            .get_area(area_id)
            .map(|area| area.meta().projection_token.clone());
        let replaced_during_fetch = token_now != token_before_fetch
            && token_now != Some(details.area.projection_token.clone());
        let outdated_source = confirmed.iter().any(|(source, revision)| {
            let fetched = if source.is_map() {
                details.area.rev
            } else {
                details
                    .sources
                    .iter()
                    .find(|bundle| bundle.source == *source)
                    .map_or(0, |bundle| bundle.rev)
            };
            fetched_revision_is_stale(
                confirmed_before_fetch.get(source).copied(),
                Some(*revision),
                fetched,
            )
        });
        if !replaced_during_fetch && !outdated_source {
            inner.replay_pending_over_locked(*area_id, &details, ReplayMode::StopAtFailure);
            if inner.pending.recovery_base_loaded(*area_id) {
                inner
                    .sync_stats
                    .operations_failed
                    .fetch_sub(1, Ordering::Relaxed);
            }
            return Ok(true);
        }
    }
    // A caching backend may have stored this stale response. Evict it so the
    // retry cannot adopt the same old bytes after the publication race ends.
    inner.backend.purge_area(area_id).await;
    warn!("Ignoring a stale source projection for map {area_id}");
    Ok(false)
}

/// Rejects a body older than backend truth that advanced while this GET was
/// in flight. Revisions may legitimately move backward after a database
/// reconstruction, so a lower body alone is not stale.
fn fetched_revision_is_stale(
    confirmed_before_fetch: Option<i64>,
    confirmed_rev: Option<i64>,
    fetched_rev: i64,
) -> bool {
    let advanced_during_fetch = match (confirmed_before_fetch, confirmed_rev) {
        (Some(before), Some(after)) => after > before,
        (None, Some(_)) => true,
        _ => false,
    };
    advanced_during_fetch && confirmed_rev.is_some_and(|confirmed| fetched_rev < confirmed)
}

/// The Secrets each published map shows, by map.
fn shown_secrets(inner: &Inner) -> HashMap<AreaId, HashSet<SourceId>> {
    inner
        .atlas_cache
        .load()
        .areas()
        .filter_map(|area| {
            let secrets: HashSet<SourceId> = area
                .meta()
                .sources
                .iter()
                .map(|bundle| bundle.source)
                .filter(SourceId::is_secret)
                .collect();
            (!secrets.is_empty()).then(|| (*area.get_id(), secrets))
        })
        .collect()
}

/// Whether `row`'s change empties the cached copy of its map before the
/// refetch: when the copy shows a Secret the row no longer covers (revoked
/// or deleted: its content must go now, not when a refetch next succeeds),
/// and when the token moved alone on a row without revisions, which can't
/// say whether one went.
///
/// A token that moves alone on a row with revisions purges nothing: the
/// caller's actions on the map or its sources changed, or an exit into
/// another map's Secret room they are shown changed, appeared or went.
/// Neither leaves anything in the cached copy its reader can no longer
/// read: a Secret they lose shows in its own map's row, a map they lose
/// takes its row along, and an exit into a Secret the atlas no longer holds
/// is never shown ([`AtlasCache::shows_exit`]). Purging there would blank
/// the map in every view, a package's without `secrets` included, on every
/// change to such an exit, the caller's own too.
///
/// [`AtlasCache::shows_exit`]: crate::mapper::atlas_cache::AtlasCache::shows_exit
fn purges_first(row: &crate::SyncRow, moved_alone: bool, lost_secret: bool) -> bool {
    lost_secret || (moved_alone && row.revisions.is_empty())
}

/// Whether the published copy of `row`'s map shows a Secret the row does
/// not cover: one the viewer can no longer read, or that is gone. Only a
/// Secret `shown_before_fetch` lists counts; one published after the row
/// was fetched (just created) is newer than the row. A row synthesized
/// from the map list carries no revisions and so no verdict; a token
/// change there purges anyway. Private additions are the viewer's own and
/// leave only with the map.
fn shows_lost_secret(
    inner: &Inner,
    row: &crate::SyncRow,
    shown_before_fetch: &HashMap<AreaId, HashSet<SourceId>>,
) -> bool {
    if row.revisions.is_empty() {
        return false;
    }
    let Some(shown_before) = shown_before_fetch.get(&row.area_id) else {
        return false;
    };
    inner
        .atlas_cache
        .load()
        .get_area(&row.area_id)
        .is_some_and(|area| {
            area.meta().sources.iter().any(|bundle| {
                bundle.source.is_secret()
                    && shown_before.contains(&bundle.source)
                    && !row.revisions.contains_key(&bundle.source)
            })
        })
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

pub(super) fn set_status(inner: &Inner, state: SyncState, last_error: Option<String>) {
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
        Area, AreaAccess, AreaUpdates, AreaWithDetails, CreateAreaRequest, Exit, ExitDirection,
        ExitId, RoomNumber, RoomWithDetails, mapper::Mapper,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use parking_lot::Mutex;
    use std::{path::PathBuf, time::Duration};
    use tokio::sync::Semaphore;
    use uuid::Uuid;

    #[test]
    fn stale_refetch_detection_uses_the_shared_revision() {
        assert!(fetched_revision_is_stale(Some(6), Some(8), 7));
        assert!(fetched_revision_is_stale(None, Some(8), 7));
        assert!(!fetched_revision_is_stale(Some(8), Some(8), 7));
        assert!(!fetched_revision_is_stale(Some(6), Some(8), 8));
        assert!(!fetched_revision_is_stale(None, None, 1));
    }

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
        /// Scripted `/me` result.
        identity: Arc<Mutex<CloudResult<Option<Uuid>>>>,
        /// When set, `sync_state` reads its rows, then blocks until the test
        /// adds a permit (a response fetched before what happens meanwhile).
        sync_gate: Arc<Mutex<Option<Arc<Semaphore>>>>,
        sync_calls: Arc<Mutex<usize>>,
        /// When set, `get_area` fails as if offline.
        gets_offline: Arc<Mutex<bool>>,
        get_gate: Arc<Mutex<Option<Arc<Semaphore>>>>,
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
                identity: Arc::new(Mutex::new(Ok(None))),
                sync_gate: Arc::new(Mutex::new(None)),
                sync_calls: Arc::new(Mutex::new(0)),
                gets_offline: Arc::new(Mutex::new(false)),
                get_gate: Arc::new(Mutex::new(None)),
            }
        }

        fn set_rows(&self, rows: Vec<SyncRow>) {
            *self.sync_rows.lock() = Ok(Some(rows));
        }

        fn set_sync_error(&self, err: CloudError) {
            *self.sync_rows.lock() = Err(err);
        }

        fn set_identity_error(&self, err: CloudError) {
            *self.identity.lock() = Err(err);
        }

        fn put_area(&self, area: AreaWithDetails) {
            self.areas.lock().insert(area.area.id, area);
        }

        fn get_count(&self, area_id: &AreaId) -> usize {
            self.get_calls
                .lock()
                .iter()
                .filter(|id| *id == area_id)
                .count()
        }

        fn purged(&self, area_id: &AreaId) -> bool {
            self.purge_calls.lock().contains(area_id)
        }

        /// The row the server would send: the area's token and its map
        /// revision.
        fn row_for(area: &AreaWithDetails) -> SyncRow {
            SyncRow {
                area_id: area.area.id,
                projection_token: area.area.view_token(),
                revisions: std::collections::BTreeMap::from([(
                    crate::SourceId::map(),
                    area.area.rev,
                )]),
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
            if *self.gets_offline.lock() {
                return Err(CloudError::NetworkError("offline".to_string()));
            }
            let result = self
                .areas
                .lock()
                .get(area_id)
                .cloned()
                .ok_or(CloudError::NotFoundOrNoAccess);
            let gate = self.get_gate.lock().take();
            if let Some(gate) = gate {
                gate.acquire().await.unwrap().forget();
            }
            result
        }

        async fn sync_state(&self) -> CloudResult<Option<Vec<SyncRow>>> {
            let rows = self.sync_rows.lock().clone();
            *self.sync_calls.lock() += 1;
            let gate = self.sync_gate.lock().clone();
            if let Some(gate) = gate {
                let permit = gate.acquire().await.expect("gate closed");
                permit.forget();
            }
            rows
        }

        async fn viewer_identity(&self) -> CloudResult<Option<Uuid>> {
            self.identity.lock().clone()
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

        async fn execute_mutation(
            &self,
            area_id: &AreaId,
            envelope: &crate::mutation::MutationEnvelope,
        ) -> CloudResult<crate::mutation::MutationResult> {
            let mut areas = self.areas.lock();
            let details = areas
                .get_mut(area_id)
                .ok_or(CloudError::AreaNotFound(*area_id))?;
            // All-or-nothing like the server: apply to a working copy and
            // commit only a fully-successful envelope.
            let mut working = details.clone();
            let result =
                crate::backends::area_edits::apply_envelope(&mut working, *area_id, envelope)?;
            *details = working;
            Ok(result)
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
        projection_token: Option<&str>,
    ) -> AreaWithDetails {
        AreaWithDetails {
            room_data: Vec::new(),
            sources: Vec::new(),
            area: Area {
                projection_token: projection_token.map(ToString::to_string),
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
                to_source: None,
                id: ExitId(Uuid::new_v4()),
                from_direction: ExitDirection::North,
                to_area_id,
                to_room_number: None,
                to_direction: None,
                path: String::new(),
                is_hidden: false,
                door: None,
                weight: 1.0,
                command: String::new(),
                connection_id: crate::ConnectionId::new(),
                to_unknown,
                to_area_token: to_unknown.then(|| "token-1".to_string()),
            }],
            tags: Default::default(),
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
    async fn rename_or_move_lands_with_its_new_token() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 1, Some(SHARED_VIEW), Some("p_before"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        let mapper = new_mapper(&backend).await;
        let atlas_id = crate::AtlasId(Uuid::new_v4());
        let mut updated = sample_area(area_id, 2, Some(SHARED_VIEW), Some("p_after"));
        updated.area.name = "Renamed remotely".to_string();
        updated.area.atlas_id = Some(atlas_id);
        updated.area.atlas_name = Some("Moved remotely".to_string());
        backend.put_area(updated.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&updated)]);

        tick(&mapper).await;

        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.get_name(), "Renamed remotely");
        assert_eq!(cached.meta().atlas_id, Some(atlas_id));
        assert_eq!(cached.meta().atlas_name.as_deref(), Some("Moved remotely"));
        assert_eq!(cached.get_rev(), 2);
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
        assert_eq!(
            mapper.inner.pending.confirmed_rev(area_id),
            Some(3),
            "the next mutation must use the reconstructed server revision"
        );
    }

    #[tokio::test]
    async fn an_access_change_refetches_without_blanking_the_map() {
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
            !backend.purged(&area_id),
            "a token moving alone leaves nothing unreadable in the cached copy"
        );
        assert_eq!(backend.get_count(&area_id), 2);
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.meta().access, Some(SHARED_EDIT));
    }

    /// A row without revisions can't say whether a Secret went, so a token
    /// moving there purges the cached copy before the refetch.
    #[tokio::test]
    async fn a_token_moving_on_a_row_without_revisions_purges_first() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 1, Some(SHARED_VIEW), Some("h1"));
        backend.put_area(area.clone());
        let bare = |area: &AreaWithDetails| SyncRow {
            revisions: std::collections::BTreeMap::new(),
            ..ScriptedBackend::row_for(area)
        };
        backend.set_rows(vec![bare(&area)]);
        let mapper = new_mapper(&backend).await;

        let updated = sample_area(area_id, 1, Some(SHARED_EDIT), Some("h2"));
        backend.put_area(updated.clone());
        backend.set_rows(vec![bare(&updated)]);
        tick(&mapper).await;

        assert!(backend.purged(&area_id));
        assert_eq!(backend.get_count(&area_id), 2);
    }

    /// A token can move with no revision moving: an exit into a room of
    /// another map's Secret appearing for this viewer. The map is refetched
    /// and the exit arrives, read as leading into the Secret's own area.
    #[tokio::test]
    async fn a_token_moving_alone_refetches() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 7, Some(SHARED_EDIT), Some("p_before"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        let mapper = new_mapper(&backend).await;
        assert_eq!(backend.get_count(&area_id), 1);

        let (other_map, secret) = (AreaId(Uuid::new_v4()), Uuid::new_v4());
        let mut linked = sample_area(area_id, 7, Some(SHARED_EDIT), Some("p_after"));
        let mut room = room_with_exit(Some(other_map), false);
        room.exits[0].to_room_number = Some(RoomNumber(4));
        room.exits[0].to_source = Some(crate::SourceId::Secret(secret));
        linked.rooms = vec![room];
        backend.put_area(linked.clone());
        let row = ScriptedBackend::row_for(&linked);
        assert_eq!(row.revisions, ScriptedBackend::row_for(&area).revisions);
        backend.set_rows(vec![row]);

        tick(&mapper).await;

        assert_eq!(backend.get_count(&area_id), 2, "the token alone refetches");
        assert!(
            !backend.purged(&area_id),
            "the map stays on screen and on disk until the refetch replaces it"
        );
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        let exit = &cached.get_room(&RoomNumber(1)).unwrap().get_exits()[0];
        assert_eq!(exit.to_area_id, Some(AreaId(secret)));
        assert_eq!(exit.to_secret_map, Some(other_map));
        assert_eq!(exit.to_room_number, Some(RoomNumber(4)));
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
        assert!(
            backend.purged(&area_a),
            "stale cached copy must be purged first"
        );
        let atlas = mapper.get_current_atlas();
        assert!(atlas.get_area(&area_b).is_some());
        let cached_a = atlas.get_area(&area_a).unwrap();
        assert!(
            !has_unknown_exit(&cached_a),
            "unknown link should be resolved"
        );
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
    }

    #[tokio::test]
    async fn account_switch_hides_cloud_areas_before_identity_resolution() {
        let backend = ScriptedBackend::new();
        let area_a = AreaId(Uuid::new_v4());
        let a = sample_area(area_a, 1, Some(SHARED_VIEW), Some("a1"));
        backend.put_area(a.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&a)]);

        let mapper = new_mapper(&backend).await;
        assert!(mapper.get_current_atlas().get_area(&area_a).is_some());
        let auth_projection_revision = mapper.auth_projection_revision();
        let sync_revision = mapper.sync_revision();

        *backend.auth_gen.lock() += 1;
        backend.set_identity_error(CloudError::NetworkError("offline".to_string()));
        mapper.sync_now();
        wait_until(|| {
            mapper.sync_status().state == SyncState::Offline
                && mapper.get_current_atlas().get_area(&area_a).is_none()
        })
        .await;

        assert!(
            mapper.get_current_atlas().get_area(&area_a).is_none(),
            "the prior account's cloud projection must disappear even when /me fails"
        );
        assert_eq!(mapper.sync_status().state, SyncState::Offline);
        assert!(
            mapper.auth_projection_revision() > auth_projection_revision,
            "account-bound UI metadata receives an immediate invalidation signal"
        );
        assert!(
            mapper.sync_revision() > sync_revision,
            "area projection consumers are notified even though identity resolution failed"
        );
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
        assert!(!has_exit_into(
            &cached_a,
            &std::iter::once(area_b).collect()
        ));
    }

    fn secret_bundle(secret: SourceId) -> crate::SourceBundle {
        crate::SourceBundle {
            source: secret,
            name: Some("Vault".to_string()),
            ownership: Some("owner".to_string()),
            clan_id: None,
            color: None,
            rev: 1,
            actions: ["read", "add", "edit", "remove"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            properties: Vec::new(),
            rooms: Vec::new(),
            room_data: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
        }
    }

    fn shows(mapper: &Mapper, area_id: &AreaId, secret: SourceId) -> bool {
        mapper
            .get_current_atlas()
            .get_area(area_id)
            .is_some_and(|area| {
                area.meta()
                    .sources
                    .iter()
                    .any(|bundle| bundle.source == secret)
            })
    }

    #[tokio::test]
    async fn old_get_cannot_undo_a_secret_acknowledgement() {
        let backend = ScriptedBackend::new();
        let id = AreaId(Uuid::new_v4());
        let secret = SourceId::Secret(Uuid::new_v4());
        let mut old = sample_area(id, 1, Some(SHARED_EDIT), Some("secret-v1"));
        old.sources = vec![secret_bundle(secret)];
        backend.put_area(old.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&old)]);
        let mapper = new_mapper(&backend).await;
        tick(&mapper).await;
        let calls = backend.get_count(&id);
        let gate = Arc::new(Semaphore::new(0));
        *backend.get_gate.lock() = Some(gate.clone());
        let inner = mapper.inner.clone();
        let stale = tokio::spawn(async move { refetch_area(&inner, &id, 0).await });
        wait_until(|| backend.get_count(&id) > calls).await;
        mapper.inner.pending.note_source_rev(id, secret, 2);
        gate.add_permits(1);
        assert!(!stale.await.unwrap().unwrap());
        assert_eq!(
            mapper.inner.pending.confirmed_source_rev(id, secret),
            Some(2)
        );
        assert!(shows(&mapper, &id, secret));
    }

    #[tokio::test]
    async fn old_get_cannot_erase_a_secret_published_while_it_was_in_flight() {
        let backend = ScriptedBackend::new();
        let id = AreaId(Uuid::new_v4());
        let old = sample_area(id, 1, Some(SHARED_EDIT), Some("before-secret"));
        backend.put_area(old.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&old)]);
        let mapper = new_mapper(&backend).await;
        tick(&mapper).await;
        let calls = backend.get_count(&id);
        let gate = Arc::new(Semaphore::new(0));
        *backend.get_gate.lock() = Some(gate.clone());
        let inner = mapper.inner.clone();
        let stale = tokio::spawn(async move { refetch_area(&inner, &id, 0).await });
        wait_until(|| backend.get_count(&id) > calls).await;
        let secret = SourceId::Secret(Uuid::new_v4());
        let mut fresh = sample_area(id, 1, Some(SHARED_EDIT), Some("after-secret"));
        fresh.sources = vec![secret_bundle(secret)];
        backend.put_area(fresh);
        assert!(refetch_area(&mapper.inner, &id, 0).await.unwrap());
        gate.add_permits(1);
        assert!(!stale.await.unwrap().unwrap(), "old GET must be refused");
        assert!(shows(&mapper, &id, secret), "the new Secret stays writable");
    }

    /// Regression test: a `/sync` response fetched before a Secret was
    /// created cannot carry it; it must not purge the map that now shows
    /// the Secret (which would blank the map until a refetch succeeds).
    #[tokio::test]
    async fn row_fetched_before_a_secret_was_created_keeps_the_map() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 1, Some(SHARED_EDIT), Some("h1"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);
        let mapper = new_mapper(&backend).await;

        // The next tick reads its rows now, before the Secret exists.
        let gate = Arc::new(Semaphore::new(0));
        *backend.sync_gate.lock() = Some(gate.clone());
        let calls = *backend.sync_calls.lock();
        let before = mapper.sync_status().last_sync;
        mapper.sync_now();
        wait_until(|| *backend.sync_calls.lock() > calls).await;

        // The Secret is created and the map republished with it, as
        // `create_secret` does.
        let secret = SourceId::Secret(Uuid::new_v4());
        let mut created = sample_area(area_id, 1, Some(SHARED_EDIT), Some("h2"));
        created.sources = vec![secret_bundle(secret)];
        backend.put_area(created);
        assert!(
            refetch_area(&mapper.inner, &area_id, backend.auth_generation())
                .await
                .unwrap()
        );
        assert!(shows(&mapper, &area_id, secret));

        // Any refetch the stale rows provoke fails.
        *backend.gets_offline.lock() = true;
        *backend.sync_gate.lock() = None;
        gate.add_permits(1);
        wait_until(|| mapper.sync_status().last_sync != before).await;

        assert!(
            !backend.purged(&area_id),
            "rows older than the Secret must not judge it lost"
        );
        assert!(
            shows(&mapper, &area_id, secret),
            "the map keeps showing its new Secret"
        );
    }

    /// A Secret the map showed before the rows were fetched, which the rows
    /// no longer cover, is lost: the map is purged at once.
    #[tokio::test]
    async fn secret_shown_before_the_fetch_and_missing_from_the_row_purges() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let secret = SourceId::Secret(Uuid::new_v4());
        let mut area = sample_area(area_id, 1, Some(SHARED_EDIT), Some("h1"));
        area.sources = vec![secret_bundle(secret)];
        backend.put_area(area.clone());
        let mut row = ScriptedBackend::row_for(&area);
        row.revisions.insert(secret, 1);
        backend.set_rows(vec![row.clone()]);
        let mapper = new_mapper(&backend).await;
        assert!(shows(&mapper, &area_id, secret));

        // Revoked: the row keeps its token here, but the Secret's revision
        // is gone.
        row.revisions.remove(&secret);
        backend.set_rows(vec![row]);
        *backend.gets_offline.lock() = true;
        tick(&mapper).await;

        assert!(backend.purged(&area_id));
        assert!(!shows(&mapper, &area_id, secret));
    }

    #[tokio::test]
    async fn cancelled_metadata_request_settles_before_notifying() {
        let backend = ScriptedBackend::new();
        let id = AreaId(Uuid::new_v4());
        let area = sample_area(id, 1, Some(SHARED_EDIT), Some("h1"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);
        let mapper = new_mapper(&backend).await;
        *backend.update_gate.lock() = Some(Arc::new(Semaphore::new(0)));
        let rename = {
            let mapper = mapper.clone();
            tokio::spawn(async move { mapper.rename_area(id, "cancelled").await })
        };
        wait_until(|| {
            mapper
                .inner
                .metadata_writes_by_area
                .lock()
                .contains_key(&id)
        })
        .await;
        let notify = mapper.inner.pending.projection_changes();
        // Consume startup signals before testing this request's settlement.
        while tokio::time::timeout(Duration::from_millis(1), notify.notified())
            .await
            .is_ok()
        {}
        rename.abort();
        assert!(rename.await.unwrap_err().is_cancelled());
        tokio::time::timeout(Duration::from_secs(1), notify.notified())
            .await
            .unwrap();
        assert!(
            !mapper
                .inner
                .metadata_writes_by_area
                .lock()
                .contains_key(&id)
        );
        assert!(!mapper.inner.pending_by_area.lock().contains_key(&id));
        assert_eq!(mapper.inner.sync_stats.operations_sent(), 1);
        assert_eq!(mapper.inner.sync_stats.operations_failed(), 1);
        assert_eq!(mapper.inner.sync_stats.pending_operations(), 0);
        assert_eq!(
            mapper.get_current_atlas().get_area(&id).unwrap().get_name(),
            area.area.name
        );
    }

    #[tokio::test]
    async fn metadata_write_publishes_only_after_backend_acknowledgement() {
        let backend = ScriptedBackend::new();
        let area_id = AreaId(Uuid::new_v4());
        let area = sample_area(area_id, 1, Some(SHARED_EDIT), Some("h1"));
        backend.put_area(area.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&area)]);

        let mapper = new_mapper(&backend).await;
        assert_eq!(backend.get_count(&area_id), 1);

        // Block the metadata request. Unlike content mutations, infrequent
        // management actions do not publish an optimistic cache state.
        let gate = Arc::new(Semaphore::new(0));
        *backend.update_gate.lock() = Some(gate.clone());
        let rename = {
            let mapper = mapper.clone();
            tokio::spawn(async move { mapper.rename_area(area_id, "Local Edit").await })
        };

        // The server moves on while our write is still in flight.
        let updated = sample_area(area_id, 2, Some(SHARED_EDIT), Some("h2"));
        backend.put_area(updated.clone());
        backend.set_rows(vec![ScriptedBackend::row_for(&updated)]);

        tick(&mapper).await;

        assert_eq!(
            backend.get_count(&area_id),
            1,
            "sync refetch waits for the acknowledged metadata request"
        );
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(
            cached.get_name(),
            "Area rev 1",
            "an unacknowledged rename must not masquerade as saved"
        );

        // The acknowledged request is the point at which the local cache
        // adopts the rename.
        gate.add_permits(1);
        rename.await.expect("rename task").expect("rename");
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.get_name(), "Local Edit");

        // Once the acknowledged request drains, ordinary server truth can
        // refetch again (the scripted server has an independent rev-2 edit).
        tick(&mapper).await;
        assert_eq!(backend.get_count(&area_id), 2);
        let cached = mapper.get_current_atlas().get_area(&area_id).unwrap();
        assert_eq!(cached.get_name(), "Area rev 2");
    }
}
