//! Session publication of confirmed changes. Storage and recovery stay with
//! their existing owners; every publication preserves the atlas's filters.

use super::{
    AreaCache, AreaId, AreaWithDetails, AtlasCache, AtlasId, Inner, ReplayMode, local_projection,
};
use std::sync::{Arc, atomic::Ordering};

pub(super) enum CommittedChange<'a> {
    Documents(&'a [AreaWithDetails], &'a [AreaId]),
    Metadata(AreaId, MetadataChange<'a>),
    DeleteAtlas(AtlasId),
}

pub(super) enum MetadataChange<'a> {
    Rename(&'a str),
    Move(Option<AtlasId>),
}

impl CommittedChange<'_> {
    fn visit_areas(&self, mut visit: impl FnMut(AreaId)) {
        match self {
            Self::Documents(documents, deleted) => {
                documents.iter().for_each(|details| visit(details.area.id));
                deleted.iter().copied().for_each(visit);
            }
            Self::Metadata(id, _) => visit(*id),
            Self::DeleteAtlas(_) => {}
        }
    }
}

impl Inner {
    /// Local responses are commit acknowledgements, not publishable snapshots:
    /// a newer generation may already exist. Register even already-deleted ids
    /// so a delayed response cannot resurrect them through the cloud path.
    pub(super) async fn publish_committed(&self, change: CommittedChange<'_>, local: bool) {
        let generation = {
            let _gate = self.mutation_gate.lock();
            self.publish_committed_locked(change, local)
        };
        if let Some(generation) = generation {
            local_projection::wait_until_published(self, generation).await;
        }
    }

    pub(super) fn publish_committed_locked(
        &self,
        change: CommittedChange<'_>,
        mut local: bool,
    ) -> Option<u64> {
        if !local {
            change.visit_areas(|id| local |= self.is_local_projection(id));
        }
        if let Some(snapshot) = self.backend.local_snapshot().filter(|_| local) {
            {
                let mut projection = self.local_projection.lock();
                change.visit_areas(|id| {
                    projection.known.insert(id);
                });
            }
            local_projection::adopt_locked(self);
            return Some(snapshot.generation);
        }
        match change {
            CommittedChange::Documents(documents, deleted) => {
                let updates: Vec<_> = documents
                    .iter()
                    .map(|details| {
                        self.pending.note_confirmed_rev(
                            details.area.id,
                            details.area.rev,
                            details.area.access.map(|access| access.fingerprint()),
                        );
                        (
                            details.area.id,
                            Arc::new(AreaCache::new_with_area(details.clone())),
                        )
                    })
                    .collect();
                self.publish_areas(&updates, deleted);
            }
            CommittedChange::Metadata(id, change) => self.publish_cache(|cache| {
                cache.get_area(&id).map_or_else(
                    || cache.clone(),
                    |area| {
                        let updated = match change {
                            MetadataChange::Rename(name) => area.rename(name),
                            MetadataChange::Move(atlas_id) => area.with_atlas(atlas_id),
                        };
                        Arc::new(cache.insert_area(id, Arc::new(updated)))
                    },
                )
            }),
            CommittedChange::DeleteAtlas(id) => self.publish_cache(|cache| {
                let updates = cache
                    .areas()
                    .filter(|area| area.meta().atlas_id == Some(id))
                    .map(|area| (*area.get_id(), Arc::new(area.with_atlas(None))));
                Arc::new(cache.with_areas_updated(updates))
            }),
        }
        None
    }

    /// Fold one confirmed document under the mutation gate. Callers record
    /// revisions according to their stale-response policy; receipts suppress
    /// local edits committed before their acknowledgement arrived.
    pub(super) fn project_confirmed(
        &self,
        details: &AreaWithDetails,
        mode: ReplayMode,
        local: Option<&crate::backends::local::LocalSnapshot>,
    ) -> (Arc<AreaCache>, Option<uuid::Uuid>) {
        let id = details.area.id;
        let (details, failed) = self.fold_pending_with_receipts(id, details, mode, local);
        if mode == ReplayMode::StopAtFailure {
            self.pending.record_replay_result(id, failed);
        }
        (Arc::new(AreaCache::new_with_area(details)), failed)
    }

    pub(super) fn publish_areas(&self, updates: &[(AreaId, Arc<AreaCache>)], deleted: &[AreaId]) {
        self.publish_cache(|cache| {
            let mut next = cache.with_areas_updated(updates.iter().cloned());
            for id in deleted {
                next = next.delete_area(*id);
            }
            Arc::new(next)
        });
    }

    /// The only confirmed cache swap. Callers hold the mutation gate; RCU also
    /// preserves concurrent filter changes without blocking snapshot readers.
    pub(super) fn publish_cache(&self, update: impl FnMut(&Arc<AtlasCache>) -> Arc<AtlasCache>) {
        self.atlas_cache.rcu(update);
        self.sync_revision.fetch_add(1, Ordering::AcqRel);
    }
}
