//! Multi-area merge orchestration. The backend owns the atomic commit; this
//! module owns validation, bounded draining, fences, and session publication.

use super::{
    AreaId, AreaMergeCommit, AreaMergePlan, AreaMergeSource, AreaMoveFence, AtlasCache, CloudError,
    CloudResult, CommittedChange, Inner, MapStorage, MapperEvent, MergeRecovery, RoomNumber,
    RoomNumberHold, local_projection,
};
use std::{collections::HashSet, sync::Arc, time::Duration};
use uuid::Uuid;

/// How many times a merge reopens its touched areas to let edits that
/// arrived between the drain and the fence flush before it reports the
/// areas busy. Each round drains a queue fully, so only an area under
/// continuous editing exhausts this.
const MERGE_FENCE_ATTEMPTS: u32 = 8;
/// Bound both queue draining and an already-sent request after fencing.
pub(super) const MERGE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

impl Inner {
    /// Drain and fence every touched area before preparing the merge.
    /// Completion owns those fences and the destination's room-number hold:
    /// failures before commit drop them; ambiguous local commits retain them.
    /// Successful local commits finish after whole-generation adoption, and
    /// session-only merges finish after direct publication of their result.
    pub(super) async fn merge_areas(
        self: &Arc<Self>,
        into: AreaId,
        sources: Vec<AreaMergeSource>,
    ) -> CloudResult<AreaMergeCommit> {
        let (tier, inbound) = self.merge_third_parties(into, &sources)?;
        let deleted: Vec<AreaId> = sources
            .iter()
            .filter(|source| !source.is_partial())
            .map(|source| source.id)
            .collect();
        let touched: Vec<AreaId> = std::iter::once(into)
            .chain(sources.iter().map(|source| source.id))
            .chain(inbound.iter().copied())
            .collect();
        let fences = tokio::time::timeout(MERGE_DRAIN_TIMEOUT, self.fence_drained_areas(&touched))
            .await
            .map_err(|_| Self::merge_refusal("merge_areas_busy"))??;
        let hold = Uuid::new_v4();
        let hold_guard = RoomNumberHold {
            inner: Arc::downgrade(self),
            area: into,
            token: hold,
        };
        let mut recovery = MergeRecovery {
            generation: 0,
            deleted,
            fences,
            _hold: hold_guard,
        };
        let plan = self.merge_plan(into, sources, inbound, &touched, hold)?;
        let commit = match self.backend.merge_areas(&plan).await {
            Ok(commit) => commit,
            Err(error) => {
                if let CloudError::LocalCommitPending { generation, .. } = &error {
                    recovery.generation = *generation;
                    self.recovery.lock().merges.push(recovery);
                    self.pending.projection_changed();
                    return Err(error);
                }
                return Err(match error {
                    CloudError::RevisionConflict { .. } => {
                        Self::merge_refusal("merge_areas_source_changed")
                    }
                    other => other,
                });
            }
        };
        let event = MapperEvent::AreasMerged {
            into,
            deleted: recovery.deleted.clone(),
            rooms: commit.outcome.rooms.clone(),
        };
        if tier == MapStorage::Local {
            // Completion stays owned until the full generation passes the
            // session's publication fences, even when the ACK arrived first.
            self.recovery.lock().merges.push(recovery);
            local_projection::adopt_and_wait(self).await;
        } else {
            self.publish_committed(
                CommittedChange::Documents(&commit.documents, &recovery.deleted),
                false,
            )
            .await;
            recovery.finish(self);
        }
        self.pending.emit(event);
        Ok(commit)
    }

    /// Raises an area's allocation floor to at least `floor` under `token`
    /// until [`Self::release_room_reservations`] releases it. A merge holds
    /// its destination this way while the backend commits: the plan's floor
    /// was sampled once, and a draft opened during the commit would
    /// otherwise reserve inside the band the merge is filling. Numbers
    /// below `floor` already held by others stay held.
    fn hold_room_number_floor(&self, area_id: AreaId, token: Uuid, floor: i64) {
        let mut reservations = self.room_reservations.lock();
        let state = reservations.entry(area_id).or_default();
        state.floor = state.floor.max(floor);
        *state.holders.entry(token).or_insert(0) += 1;
    }

    /// Releases every reservation held under `token` for an area.
    /// Idempotent; when the last holder releases, allocation falls back to
    /// the cache maximum.
    pub(super) fn release_room_reservations(&self, area_id: &AreaId, token: Uuid) {
        let mut reservations = self.room_reservations.lock();
        if let Some(state) = reservations.get_mut(area_id) {
            state.holders.remove(&token);
            if state.holders.is_empty() {
                reservations.remove(area_id);
            }
        }
    }

    fn merge_refusal(code: &str) -> CloudError {
        CloudError::StructuralConflict(code.to_string())
    }

    /// Checks a merge's arguments against the live cache and discovers its
    /// third parties: the sources are non-empty, distinct and not the
    /// destination; a partial source lists at least one room and only rooms
    /// it holds; every touched area is loaded, fully projected, and in the
    /// destination's storage tier; that tier is one a backend can transact
    /// in. Returns the tier and third parties in id order.
    fn merge_third_parties(
        &self,
        into: AreaId,
        sources: &[AreaMergeSource],
    ) -> CloudResult<(MapStorage, Vec<AreaId>)> {
        if sources.is_empty() {
            return Err(Self::merge_refusal("merge_areas_no_sources"));
        }
        let mut source_set = HashSet::with_capacity(sources.len());
        for source in sources {
            if source.id == into || !source_set.insert(source.id) {
                return Err(Self::merge_refusal("merge_areas_same_area"));
            }
        }

        let local = self.backend.local_area_ids();
        let ephemeral = self.backend.ephemeral_area_ids();
        let tier_of = |id: &AreaId| {
            if ephemeral.contains(id) {
                MapStorage::Session
            } else if local.contains(id) {
                MapStorage::Local
            } else {
                MapStorage::Cloud
            }
        };
        let tier = tier_of(&into);
        let cache = self.atlas_cache.load_full();
        let check_touched = |id: AreaId| -> CloudResult<()> {
            let area = cache.get_area(&id).ok_or(CloudError::AreaNotFound(id))?;
            if !area.effective_access().is_cleared_for_secrets() {
                return Err(Self::merge_refusal("merge_requires_full_projection"));
            }
            if tier_of(&id) != tier {
                return Err(Self::merge_refusal("merge_areas_mixed_tiers"));
            }
            Ok(())
        };
        for id in std::iter::once(into).chain(source_set.iter().copied()) {
            check_touched(id)?;
        }
        for source in sources {
            let Some(listed) = &source.rooms else {
                continue;
            };
            if listed.is_empty() {
                return Err(Self::merge_refusal("merge_areas_no_rooms"));
            }
            let area = cache
                .get_area(&source.id)
                .ok_or(CloudError::AreaNotFound(source.id))?;
            if listed.iter().any(|number| area.get_room(number).is_none()) {
                return Err(Self::merge_refusal("merge_areas_room_not_found"));
            }
        }
        if tier == MapStorage::Cloud {
            return Err(Self::merge_refusal("merge_areas_unsupported_storage"));
        }
        let inbound = Self::merge_inbound_areas(&cache, into, &source_set);
        for id in &inbound {
            check_touched(*id)?;
        }
        Ok((tier, inbound))
    }

    /// Fences every area in `touched` with its queue empty and nothing on
    /// the wire. Drain first, fence second: a delete fence holds unsent
    /// followers rather than flushing them, and an edit held behind the
    /// fence would be missing from the snapshot a merge publishes. Edits that
    /// slip in between the drain and the fence reopen the areas for another
    /// round; a stream of them that never lets the fence close is a busy
    /// area, reported as such rather than waited on forever. A queue that
    /// cannot drain on its own (parked for review, mid-rename) is refused by
    /// the fence itself, before anything is frozen.
    async fn fence_drained_areas(&self, touched: &[AreaId]) -> CloudResult<Vec<AreaMoveFence>> {
        let mut attempts = 0;
        loop {
            for id in touched {
                while self.pending.is_draining(*id) {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
            let fences = self.begin_area_move(touched).map_err(|error| match error {
                CloudError::PendingOperations(_) => Self::merge_refusal("merge_areas_busy"),
                other => other,
            })?;
            for fence in &fences {
                self.pending
                    .wait_until_delete_quiescent(fence.area_id())
                    .await;
            }
            if touched.iter().all(|id| self.pending.queued_len(*id) == 0) {
                return Ok(fences);
            }
            Self::release_fences(fences);
            attempts += 1;
            if attempts == MERGE_FENCE_ATTEMPTS {
                return Err(Self::merge_refusal("merge_areas_busy"));
            }
        }
    }

    fn release_fences(fences: Vec<AreaMoveFence>) {
        for fence in fences {
            fence.release();
        }
    }

    /// The plan for a merge whose areas are fenced and quiescent: the
    /// third parties re-derived on the settled atlas (draining may have
    /// landed an exit into a source from an area the fences do not cover;
    /// the caller re-derives rather than the merge chasing a moving target),
    /// the revision each document stands on at the backend (the last
    /// acknowledged one when known, else the cached document's, which a
    /// drained queue leaves equal), and the destination's allocation floor
    /// including numbers reserved by open drafts.
    ///
    /// Building the plan also holds the destination's floor under `hold`
    /// past every number the merge can place: the floor plus one per source
    /// room bounds what allocation hands out, and a source number at or
    /// above the floor is kept as is, so the hold is the larger of those
    /// two bounds. This is where the merged rooms would push the floor
    /// anyway once they are in the cache; the hold only makes it true
    /// during the commit. The caller releases `hold` on every exit.
    fn merge_plan(
        &self,
        into: AreaId,
        sources: Vec<AreaMergeSource>,
        inbound: Vec<AreaId>,
        touched: &[AreaId],
        hold: Uuid,
    ) -> CloudResult<AreaMergePlan> {
        let cache = self.atlas_cache.load_full();
        let source_set: HashSet<AreaId> = sources.iter().map(|source| source.id).collect();
        let inbound_now = Self::merge_inbound_areas(&cache, into, &source_set);
        if inbound_now.iter().copied().collect::<HashSet<_>>()
            != inbound.iter().copied().collect::<HashSet<_>>()
        {
            return Err(Self::merge_refusal("merge_areas_source_changed"));
        }
        let mut expected = Vec::with_capacity(touched.len());
        for id in touched {
            let area = cache.get_area(id).ok_or(CloudError::AreaNotFound(*id))?;
            let rev = self
                .pending
                .confirmed_rev(*id)
                .0
                .unwrap_or_else(|| area.get_rev());
            expected.push((*id, rev));
        }
        let base = cache
            .get_area(&into)
            .map_or(1, |area| area.room_number_floor());
        // Only the rooms that move count toward the hold: every room of a
        // whole source, the listed rooms of a partial one.
        let (mut source_rooms, mut source_max) = (0_i64, 0_i32);
        for source in &sources {
            let Some(area) = cache.get_area(&source.id) else {
                continue;
            };
            let listed: Option<HashSet<RoomNumber>> = source
                .rooms
                .as_ref()
                .map(|rooms| rooms.iter().copied().collect());
            for room in area.get_rooms() {
                let number = room.get_room_number();
                if listed
                    .as_ref()
                    .is_some_and(|listed| !listed.contains(&number))
                {
                    continue;
                }
                source_rooms += 1;
                source_max = source_max.max(number.0);
            }
        }
        let floor = {
            let reservations = self.room_reservations.lock();
            reservations
                .get(&into)
                .map_or(base, |state| base.max(state.floor))
        };
        // A full destination can still accept rooms in unoccupied gaps.
        // An exhausted draft reservation above a smaller destination cannot
        // be represented by the plan's i32 floor, so refuse it explicitly.
        if floor > i64::from(i32::MAX) && floor > base {
            return Err(Self::merge_refusal("merge_areas_room_numbers_exhausted"));
        }
        let number_floor = RoomNumber(i32::try_from(floor).unwrap_or(i32::MAX));
        self.hold_room_number_floor(
            into,
            hold,
            (floor + source_rooms).max(i64::from(source_max) + 1),
        );
        Ok(AreaMergePlan {
            into,
            sources,
            inbound,
            expected,
            number_floor,
        })
    }

    /// The third parties of a merge: every loaded area other than the
    /// destination and the sources holding at least one exit into a source,
    /// in id order. Disabled and scope-excluded areas count; their exits are
    /// real and would dangle otherwise.
    fn merge_inbound_areas(
        cache: &AtlasCache,
        into: AreaId,
        sources: &HashSet<AreaId>,
    ) -> Vec<AreaId> {
        let mut inbound: Vec<AreaId> = cache
            .areas()
            .filter(|area| *area.get_id() != into && !sources.contains(area.get_id()))
            .filter(|area| {
                area.get_rooms().iter().any(|room| {
                    room.get_exits().iter().any(|exit| {
                        exit.to_area_id
                            .is_some_and(|target| sources.contains(&target))
                    })
                })
            })
            .map(|area| *area.get_id())
            .collect();
        inbound.sort_by_key(|id| id.0);
        inbound
    }
}
