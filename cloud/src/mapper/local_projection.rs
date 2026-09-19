//! Adopt one committed local generation per session, independently of cloud sync.

use std::{collections::HashSet, sync::Arc};

use super::{Inner, ReplayMode, area_cache::AreaCache};
use crate::{AreaId, AreaWithDetails, backends::local::LocalSnapshot};

#[derive(Default)]
pub(super) struct LocalProjection {
    snapshot: Option<Arc<LocalSnapshot>>,
    /// Includes formerly local ids so an older cloud-sync response cannot
    /// refetch or individually prune pieces of a local transaction.
    pub known: HashSet<AreaId>,
}

pub(super) fn spawn(inner: &Arc<Inner>) {
    let weak = Arc::downgrade(inner);
    let backend = inner.backend.clone();
    let notify = inner.pending.projection_changes();
    let task = tokio::spawn(async move {
        let mut changed = loop {
            match backend.subscribe_local().await {
                Ok(Some(changed)) => break changed,
                Ok(None) => return,
                Err(error) => log::warn!("Could not initialize local map subscription: {error}"),
            }
            notify.notified().await;
        };
        let mut attempted_generation = None;
        loop {
            // A watch channel coalesces commits. Snapshot loading follows
            // subscription, so startup and a burst of writes cannot lose the
            // latest generation. Deferred adoption retries on queue settlement.
            changed.borrow_and_update();
            {
                let Some(inner) = weak.upgrade() else {
                    return;
                };
                adopt(&inner);
                let recovering = {
                    let recovery = inner.recovery.lock();
                    !recovery.mutations.is_empty() || !recovery.merges.is_empty()
                };
                let needs_recovery = recovering
                    || inner
                        .local_projection
                        .lock()
                        .known
                        .iter()
                        .any(|id| inner.pending.delete_needs_recovery(*id));
                let generation = backend.local_snapshot().map(|snapshot| snapshot.generation);
                if !needs_recovery {
                    attempted_generation = None;
                } else if generation != attempted_generation {
                    // Queue activity retries adoption, not disk recovery. One
                    // attempt per store generation; explicit refresh always runs.
                    attempted_generation = generation;
                    match backend.refresh_local().await {
                        Ok(()) => {
                            // Consume our own refresh before loading the snapshot.
                            // A failed WAL reconciliation must not spin on it.
                            changed.borrow_and_update();
                            attempted_generation = backend
                                .local_snapshot()
                                .filter(|snapshot| snapshot.recovery_error.is_none())
                                .map(|snapshot| snapshot.generation);
                            adopt_refreshed(&inner);
                            // A writer may have journaled another change after
                            // refresh released its lock. Recheck before sleeping.
                            continue;
                        }
                        Err(error) => log::warn!("Could not recover local map changes: {error}"),
                    }
                }
            }
            tokio::select! {
                result = changed.changed() => if result.is_err() { return; },
                () = notify.notified() => {},
            }
        }
    });
    inner.background_tasks.lock().push(task.abort_handle());
}

/// Completion promises wait for session visibility without holding any
/// synchronous guard. The watch value also covers publication before subscribe.
pub(super) async fn wait_until_published(inner: &Inner, generation: u64) {
    let mut published = inner.local_published.subscribe();
    while *published.borrow_and_update() < generation {
        if published.changed().await.is_err() {
            return;
        }
    }
}

pub(super) async fn adopt_and_wait(inner: &Inner) {
    if let Some(snapshot) = inner.backend.local_snapshot() {
        adopt(inner);
        wait_until_published(inner, snapshot.generation).await;
    }
}

pub(super) fn adopt(inner: &Inner) {
    let _gate = inner.mutation_gate.lock();
    adopt_locked(inner);
}

pub(super) fn adopt_locked(inner: &Inner) {
    reconcile_locked(inner, None, false);
}

pub(super) fn adopt_refreshed(inner: &Inner) {
    let _gate = inner.mutation_gate.lock();
    reconcile_locked(inner, None, true);
}

pub(super) fn replay_locked(inner: &Inner, area: AreaId, mode: ReplayMode) -> Option<uuid::Uuid> {
    reconcile_locked(inner, Some((area, mode)), false)
}

fn reconcile_locked(
    inner: &Inner,
    replay: Option<(AreaId, ReplayMode)>,
    refreshed: bool,
) -> Option<uuid::Uuid> {
    // Pin after both guards: a merge registered before this adoption must
    // be covered by its snapshot. Registration releases recovery before
    // requesting adoption, so the lock order is always projection, recovery.
    let mut projection = inner.local_projection.lock();
    let mut recovery = inner.recovery.lock();
    let snapshot = inner.backend.local_snapshot()?;
    let unchanged = replay.is_none()
        && projection
            .snapshot
            .as_ref()
            .is_some_and(|previous| previous.generation == snapshot.generation);
    if !unchanged {
        for &(area, operation) in snapshot.journaled_operations() {
            recovery.retain_mutation(inner, snapshot.generation, area, operation);
        }
        projection
            .known
            .extend(snapshot.areas().map(|details| details.area.id));
    }
    if refreshed && snapshot.recovery_error.is_none() {
        inner.reconcile_local_deletes(&projection.known, &snapshot);
    }
    if unchanged {
        recovery.finish_published(inner, &snapshot);
        return None;
    }
    let previous = projection.snapshot.as_ref();
    let changed: Vec<_> = snapshot
        .areas()
        .filter(|details| {
            replay.is_some_and(|(id, _)| id == details.area.id)
                || previous.is_none_or(|previous| !snapshot.shares_area(previous, details.area.id))
        })
        .collect();
    // Initial load can populate local ids before this task's first adoption.
    // Use the session's known local inventory as the deletion baseline too.
    let removed: Vec<_> = projection
        .known
        .iter()
        .copied()
        // An unreadable file is not a deletion. Preserve the last readable
        // view while the store refuses edits to that document.
        .filter(|id| {
            !snapshot.contains_area(*id) && inner.atlas_cache.load().get_area(id).is_some()
        })
        .collect();
    let recovered_fences = recovery.ready_fences(snapshot.generation);
    let touched = changed
        .iter()
        .map(|details| details.area.id)
        .chain(removed.iter().copied());
    // Degraded startup must expose its scanned survivors without settling
    // delete intents. Only a healthy recovery may release those write fences.
    if touched.clone().any(|id| {
        (snapshot.recovery_error.is_none()
            && inner.pending.blocks_publication(id)
            && !recovered_fences.contains(&id))
            || inner
                .metadata_writes_by_area
                .lock()
                .get(&id)
                .copied()
                .unwrap_or(0)
                > 0
    }) {
        // The caller can still classify its conflict, but must not publish
        // one area while the full generation is waiting behind another fence.
        return replay_deferred(inner, &snapshot, replay);
    }
    let mut updates = Vec::with_capacity(changed.len());
    let mut target_failure = None;
    for details in changed {
        let id = details.area.id;
        let cached = inner.atlas_cache.load().get_area(&id);
        if replay.is_none()
            && !refreshed
            && cached.is_some()
            && projection.contains_optimistic_commit(inner, &snapshot, details)
        {
            confirm_revision(inner, details);
            continue;
        }
        let metadata_only = replay.is_none()
            && previous.is_some_and(|previous| snapshot.shares_content(previous, id));
        confirm_revision(inner, details);
        let mode = replay
            .filter(|(target, _)| *target == id)
            .map_or(ReplayMode::StopAtFailure, |(_, mode)| mode);
        // Receipts distinguish an already committed local head whose ACK has
        // not arrived from an unsent optimistic edit. Never apply it twice.
        let (details, failed) =
            project_area(inner, details, mode, &snapshot, cached, metadata_only);
        if replay.is_some_and(|(target, _)| target == id) {
            target_failure = failed;
        }
        if snapshot.recovery_error.is_none() {
            inner.pending.recovery_base_loaded(id);
        }
        updates.push((id, details));
    }
    if !updates.is_empty() || !removed.is_empty() {
        inner.publish_areas(&updates, &removed);
    }
    recovery.finish_published(inner, &snapshot);
    let generation = snapshot.generation;
    projection.snapshot = Some(snapshot);
    inner.local_published.send_replace(generation);
    target_failure
}

impl LocalProjection {
    /// An ACK or stored receipt proves the cache already includes this commit.
    /// A newer foreign revision still requires replay over the new base.
    fn contains_optimistic_commit(
        &self,
        inner: &Inner,
        snapshot: &LocalSnapshot,
        details: &AreaWithDetails,
    ) -> bool {
        let id = details.area.id;
        if !self.snapshot.as_ref().is_some_and(|previous| {
            previous
                .area(id)
                .is_ok_and(|old| old.area.rev < details.area.rev)
        }) {
            return false;
        }
        let Some(confirmed) = inner.pending.confirmed_rev(id).0 else {
            return false;
        };
        let committed_here = inner
            .pending
            .pending_for(id)
            .iter()
            .filter(|envelope| snapshot.applied_after(id, envelope.operation_id, confirmed))
            .count();
        i64::try_from(committed_here)
            .ok()
            .and_then(|count| confirmed.checked_add(count))
            == Some(details.area.rev)
    }
}

fn project_area(
    inner: &Inner,
    details: &AreaWithDetails,
    mode: ReplayMode,
    snapshot: &LocalSnapshot,
    cached: Option<Arc<AreaCache>>,
    metadata_only: bool,
) -> (Arc<AreaCache>, Option<uuid::Uuid>) {
    if let Some(cached) = cached.as_ref().filter(|_| metadata_only) {
        return (Arc::new(cached.with_local_metadata(&details.area)), None);
    }
    let (mut area, failed) = inner.project_confirmed(details, mode, Some(snapshot));
    if let Some(cached) = cached {
        Arc::make_mut(&mut area).advance_revision(cached.get_rev());
    }
    (area, failed)
}

fn replay_deferred(
    inner: &Inner,
    snapshot: &LocalSnapshot,
    replay: Option<(AreaId, ReplayMode)>,
) -> Option<uuid::Uuid> {
    replay.and_then(|(id, mode)| {
        snapshot.area(id).ok().and_then(|details| {
            confirm_revision(inner, details);
            let failed = inner
                .fold_pending_with_receipts(id, details, mode, Some(snapshot))
                .1;
            if mode == ReplayMode::StopAtFailure {
                inner.pending.record_replay_result(id, failed);
            }
            failed
        })
    })
}

fn confirm_revision(inner: &Inner, details: &AreaWithDetails) {
    inner.pending.adopt_confirmed_rev(
        details.area.id,
        details.area.rev,
        details.area.access.map(|access| access.fingerprint()),
    );
}
