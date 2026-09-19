//! Session recovery decisions. The store and pending queue retain ownership
//! of their durable records; this module owns completion after publication.

use super::{AreaId, AreaMoveFence, CloudResult, Inner, OperationId, RoomNumberHold};
use crate::backends::local::LocalSnapshot;
use std::{collections::HashSet, sync::atomic::Ordering};

#[derive(Default)]
pub(super) struct SessionRecovery {
    pub merges: Vec<MergeRecovery>,
    pub mutations: Vec<MutationRecovery>,
}

pub(super) struct MutationRecovery {
    pub generation: u64,
    pub area: AreaId,
    pub operation: OperationId,
}

pub(super) struct MergeRecovery {
    pub generation: u64,
    pub deleted: Vec<AreaId>,
    pub fences: Vec<AreaMoveFence>,
    pub _hold: RoomNumberHold,
}

impl MergeRecovery {
    /// Called only after the committed map is published. Dropping an
    /// unfinished completion instead releases its pre-commit guards.
    pub fn finish(self, inner: &Inner) {
        for fence in self.fences {
            let id = fence.area_id();
            if self.deleted.contains(&id) {
                match fence.commit_deleted() {
                    Ok(discarded) => inner.account_deleted_pending(id, &discarded),
                    Err(error) => log::warn!(
                        "Could not retire merged area {id}; keeping its delete intent: {error}"
                    ),
                }
            } else {
                fence.release();
            }
        }
    }
}

impl SessionRecovery {
    pub fn retain_mutation(
        &mut self,
        inner: &Inner,
        generation: u64,
        area: AreaId,
        operation: OperationId,
    ) {
        if inner.pending.hold_journaled(area, operation)
            && !self
                .mutations
                .iter()
                .any(|entry| entry.operation == operation)
        {
            self.mutations.push(MutationRecovery {
                generation,
                area,
                operation,
            });
        }
    }

    pub fn ready_fences(&self, generation: u64) -> HashSet<AreaId> {
        self.merges
            .iter()
            .filter(|merge| generation > merge.generation)
            .flat_map(|merge| merge.fences.iter().map(AreaMoveFence::area_id))
            // A delete can be waiting for this durable head to settle.
            // Publication acknowledges the head without releasing that fence.
            .chain(
                self.mutations
                    .iter()
                    .filter(|mutation| generation > mutation.generation)
                    .map(|mutation| mutation.area),
            )
            .collect()
    }

    pub fn finish_published(&mut self, inner: &Inner, snapshot: &LocalSnapshot) {
        if snapshot.recovery_error.is_some() {
            return;
        }
        let generation = snapshot.generation;
        for merge in std::mem::take(&mut self.merges) {
            if generation > merge.generation {
                merge.finish(inner);
            } else {
                self.merges.push(merge);
            }
        }
        for mutation in std::mem::take(&mut self.mutations) {
            if generation <= mutation.generation {
                self.mutations.push(mutation);
                continue;
            }
            // A healthy newer generation proves the store finished every
            // earlier decided transaction. Keep the head non-discardable until
            // this publication; it cannot be retried or discarded meanwhile.
            let rev = snapshot.area(mutation.area).ok().map(|area| area.area.rev);
            if inner
                .pending
                .acknowledge(mutation.area, mutation.operation, rev)
            {
                inner
                    .sync_stats
                    .operations_succeeded
                    .fetch_add(1, Ordering::Relaxed);
                inner.settle_pending(mutation.area, 1);
            }
        }
    }
}

impl Inner {
    /// A missing local document or authorized cloud refusal settles a delete
    /// intent; other saved edits remain recoverable and exportable. Transport
    /// failures and unreadable local files must never enter this path.
    pub(super) fn recover_unavailable_area(&self, id: AreaId) -> CloudResult<()> {
        if self.pending.has_delete_intent(id) {
            let discarded = self.pending.commit_recovered_delete(id)?;
            self.account_deleted_pending(id, &discarded);
        } else {
            let message = format!(
                "saved edits for area {id} cannot be restored because the area is unavailable; access may have changed or the area may have been deleted"
            );
            if self.pending.recovery_base_unavailable(id, message) {
                self.sync_stats
                    .operations_failed
                    .fetch_add(1, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    pub(super) fn reconcile_local_deletes(
        &self,
        known: &HashSet<AreaId>,
        snapshot: &LocalSnapshot,
    ) {
        for id in known.iter().copied() {
            if !self.pending.delete_needs_recovery(id) {
                continue;
            }
            let result = if snapshot.area(id).is_ok() {
                self.pending.abort_recovered_delete(id)
            } else if !snapshot.contains_area(id) {
                self.recover_unavailable_area(id)
            } else {
                continue;
            };
            if let Err(error) = result {
                log::warn!("Could not reconcile local delete {id}: {error}");
            }
        }
    }
}
