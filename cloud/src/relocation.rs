//! High-level map copy/move operations across session, local, and cloud
//! storage.
//!
//! A storage change is not an atlas metadata update. It is a recoverable
//! copy-then-delete transaction: create all destination headers, copy all
//! rooms, remap in-set cross-area links, copy the remaining content, wait for
//! backend acknowledgement, and only then delete the sources for a move.
//! Failures before commit clean up destination objects; if cleanup or
//! same-tier member work remains, [`PartialRelocation`] identifies it and the
//! error explicitly forbids a blind retry. Failures while deleting a source
//! leave a complete destination copy and a harmless duplicate rather than
//! losing data.

use std::collections::{HashMap, HashSet};

use log::warn;

use crate::{
    AreaId, AreaWithDetails, AtlasId, CloudError, CloudResult, Connection, ConnectionArgs,
    ConnectionId, ConnectionKind, Exit, ExitArgs, ExitId, LabelArgs, LabelId, MapDestination,
    MapStorage, Mapper, RoomNumber, RoomUpdates, ShapeArgs, ShapeId, SourceBundle, SourceId,
    mapper::{AreaMutationBatch, MutationSubmission, validate_import_document},
    mutation::{AreaMutation, MAX_MUTATION_OPERATIONS},
};

/// Whether the source survives a relocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationMode {
    Copy,
    Move,
}

/// The owner's sharing review for moving a personal cloud map to local storage.
/// Supplying this review to a relocation acknowledges the displayed loss of
/// shared users' cloud-only content when `has_shares` is true.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LocalMoveReview {
    pub area_id: AreaId,
    pub sharing_token: String,
    pub has_shares: bool,
    #[serde(skip)]
    pub auth_generation: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LocalMoveGuard {
    pub expected_projection_token: String,
    pub sharing_token: String,
    pub shared_loss_confirmed: bool,
}

/// Name marker a replay-populated relocation destination carries while its
/// content is being copied. Destinations are created bearing it and shed
/// it only once fully populated, so a crash mid-copy strands areas that
/// are visibly in-progress debris instead of twins indistinguishable from
/// the originals. [`Mapper::abandoned_relocation_areas`] lists survivors
/// and startup logs them; they are never swept automatically, because on
/// the cloud tier the marker may belong to another client's relocation
/// still legitimately in flight. (Server-side clones skip the marker: the
/// server creates them complete in one transaction, leaving no
/// half-populated window.)
pub const RELOCATION_IN_PROGRESS_SUFFIX: &str = " (relocating)";

/// The result of relocating one or more areas, in source order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MapRelocation {
    pub source_ids: Vec<AreaId>,
    pub destination_ids: Vec<AreaId>,
    pub destination: MapDestination,
}

/// The result of copying or moving an atlas and all of its member areas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlasRelocation {
    pub source_atlas_id: AtlasId,
    pub destination_atlas_id: AtlasId,
    pub destination_atlas_name: String,
    pub areas: MapRelocation,
}

/// A failed relocation. When the failure struck after the destination copy
/// was fully created and acknowledged (the source-delete commit phase),
/// `completed` carries that result so callers can point the user at the
/// existing copy — retrying the whole relocation would mint a second one.
/// Work known to remain after a relocation failed before destination
/// completion. Callers must reconcile it rather than retrying blindly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartialRelocation {
    pub destination: MapDestination,
    /// Fresh destination objects which cleanup could not remove, or complete
    /// copies awaiting the remaining same-tier member work.
    pub copied_destination_ids: Vec<AreaId>,
    /// Same-tier source objects already refiled into the destination.
    pub refiled_source_ids: Vec<AreaId>,
    /// Same-tier source objects whose destination work did not finish.
    pub pending_source_ids: Vec<AreaId>,
}

#[derive(Debug)]
pub struct RelocationError<T> {
    pub error: CloudError,
    /// The fully created destination when no destination-side work remains.
    /// A source delete may still have failed, so callers reconcile the
    /// resulting duplicate instead of retrying the relocation.
    pub completed: Option<T>,
    /// Destination or same-tier member work remains but is not complete.
    /// When both outcome fields are `None`, all created destination objects
    /// were cleaned up and retrying is safe.
    pub partial: Option<PartialRelocation>,
}

impl<T> From<CloudError> for RelocationError<T> {
    fn from(error: CloudError) -> Self {
        Self {
            error,
            completed: None,
            partial: None,
        }
    }
}

impl<T> std::fmt::Display for RelocationError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.completed.is_some() {
            write!(
                f,
                "{}. A destination copy was created, but the original could not be removed. Keep the original and reconcile both copies before retrying; the original may contain newer work",
                self.error
            )
        } else if self.partial.is_some() {
            write!(
                f,
                "{}. The destination is only partially updated; inspect and reconcile it before retrying the relocation",
                self.error
            )
        } else {
            self.error.fmt(f)
        }
    }
}

impl<T: std::fmt::Debug> std::error::Error for RelocationError<T> {}

impl Mapper {
    /// Review cloud sources before offering a destructive move to local storage.
    /// Clan sources are refused even when the caller owns the clan or map.
    ///
    /// # Errors
    /// Returns an error if ownership cannot be verified or the service cannot
    /// review a guarded move under the current credential.
    pub async fn review_local_moves(
        &self,
        area_ids: &[AreaId],
    ) -> CloudResult<Vec<LocalMoveReview>> {
        self.local_move_reviews(area_ids).await
    }

    /// Copy or move a set of areas to one explicit storage/folder destination.
    /// Cross-area exits whose targets are also in `source_ids` are remapped to
    /// the corresponding destination areas. For copied members, links leaving
    /// the set become dangling, matching portable import semantics, except in
    /// a cloud→cloud copy the service makes (see `server_copy_applies`),
    /// whose links follow its copy rule; a moved member already in the
    /// destination tier keeps its id (and therefore its outside links) and is
    /// merely re-filed.
    ///
    /// A copied member carries its places as smudgy-cloudflare format-3.md
    /// §5.2 says (`keep_what_a_copy_carries`) into local and cloud storage:
    /// each Secret as an owner Secret of the copy, and the caller's Private
    /// additions as theirs there. Session storage keeps no places: a copy
    /// there arrives without them, and a move there of a map holding any is
    /// refused with [`MOVE_DROPS_PLACES`] before anything is created, so a
    /// move never deletes a place with its original.
    ///
    /// # Errors
    /// Returns authorization, storage, or synchronization errors. A failed
    /// source deletion reports any destination copies already completed.
    /// Shared cloud sources require [`Self::relocate_areas_reviewed`].
    pub async fn relocate_areas(
        &self,
        source_ids: Vec<AreaId>,
        destination: MapDestination,
        mode: RelocationMode,
    ) -> Result<MapRelocation, RelocationError<MapRelocation>> {
        self.relocate_areas_reviewed(source_ids, destination, mode, &[])
            .await
    }

    /// Relocate with the exact sharing reviews acknowledged by the caller.
    ///
    /// # Errors
    /// Has the same failures as [`Self::relocate_areas`], and refuses stale
    /// sharing reviews or source snapshots without deleting the original.
    pub async fn relocate_areas_reviewed(
        &self,
        source_ids: Vec<AreaId>,
        destination: MapDestination,
        mode: RelocationMode,
        reviews: &[LocalMoveReview],
    ) -> Result<MapRelocation, RelocationError<MapRelocation>> {
        self.relocate_areas_with_reviews(source_ids, destination, mode, reviews, &[])
            .await
    }

    /// Relocates a set after acknowledging the cloud filing previews for its cloud members.
    ///
    /// # Errors
    /// Has the relocation failures above, including stale or unauthorized filing reviews.
    pub async fn relocate_areas_with_filing_reviews(
        &self,
        source_ids: Vec<AreaId>,
        destination: MapDestination,
        reviews: &[crate::access_review::ReviewedFiling],
    ) -> Result<MapRelocation, RelocationError<MapRelocation>> {
        self.relocate_areas_with_reviews(
            source_ids,
            destination,
            RelocationMode::Move,
            &[],
            reviews,
        )
        .await
    }

    async fn refile_reviewed_relocation(
        &self,
        id: AreaId,
        atlas: Option<AtlasId>,
        reviews: &[crate::access_review::ReviewedFiling],
    ) -> CloudResult<()> {
        if let Some(review) = reviews.iter().find(|review| review.area_id == id) {
            if review.atlas_id != atlas {
                return Err(CloudError::InvalidInput(
                    "filing review names another destination".to_string(),
                ));
            }
            self.commit_reviewed_filing(review.clone()).await
        } else {
            self.move_area_to_atlas(id, atlas).await
        }
    }

    async fn relocate_areas_with_reviews(
        &self,
        source_ids: Vec<AreaId>,
        destination: MapDestination,
        mode: RelocationMode,
        reviews: &[LocalMoveReview],
        filing_reviews: &[crate::access_review::ReviewedFiling],
    ) -> Result<MapRelocation, RelocationError<MapRelocation>> {
        if destination.storage == MapStorage::Session && destination.atlas_id.is_some() {
            return Err(CloudError::InvalidInput(
                "session maps cannot be filed into atlases".to_string(),
            )
            .into());
        }
        if let Some(atlas_id) = destination.atlas_id {
            if self.atlas_storage(&atlas_id).is_none() {
                self.list_atlases().await?;
            }
            match self.atlas_storage(&atlas_id) {
                Some(storage) if storage == destination.storage => {}
                Some(_) => {
                    return Err(CloudError::InvalidInput(
                        "the destination atlas belongs to a different storage tier".to_string(),
                    )
                    .into());
                }
                None => {
                    return Err(CloudError::InvalidInput(
                        "destination atlas not found".to_string(),
                    )
                    .into());
                }
            }
        }
        if source_ids.is_empty() {
            return Ok(MapRelocation {
                source_ids,
                destination_ids: Vec::new(),
                destination,
            });
        }

        let mut seen = HashSet::with_capacity(source_ids.len());
        if source_ids.iter().any(|id| !seen.insert(*id)) {
            return Err(CloudError::InvalidInput(
                "a map relocation cannot contain the same area twice".to_string(),
            )
            .into());
        }

        let atlas = self.get_current_atlas();
        for source_id in &source_ids {
            let area = atlas
                .get_area(source_id)
                .ok_or(CloudError::AreaNotFound(*source_id))?;
            let access = area.effective_access();
            if mode == RelocationMode::Move
                && destination.storage != MapStorage::Cloud
                && area.meta().clan_id.is_some()
            {
                return Err(CloudError::InvalidInput(
                    "clan-library maps cannot be moved to local storage; use Copy".to_string(),
                )
                .into());
            }
            if !access.can_copy {
                return Err(CloudError::InvalidInput(format!(
                    "map '{}' cannot be copied with the current access",
                    area.get_name()
                ))
                .into());
            }
            if mode == RelocationMode::Move && !access.is_owner {
                return Err(CloudError::InvalidInput(format!(
                    "map '{}' is shared with you and cannot be moved",
                    area.get_name()
                ))
                .into());
            }
        }

        // Move mode partitions the set: a member already sitting in the
        // destination tier is merely re-filed — its id, content, and links
        // to areas outside the set all survive — while cross-tier members
        // are copied under fresh ids and their sources deleted. Copies mint
        // fresh ids for every member. In-set exits remap as one group
        // either way: kept members map to themselves in the id map, so a
        // copied member's link into a kept sibling holds without
        // rewriting, and a kept member's link into a copied sibling is
        // retargeted once the copy lands.
        let copied_ids: Vec<AreaId> = if mode == RelocationMode::Move {
            source_ids
                .iter()
                .copied()
                .filter(|id| self.area_storage(id) != destination.storage)
                .collect()
        } else {
            source_ids.clone()
        };
        let kept_ids: Vec<AreaId> = source_ids
            .iter()
            .copied()
            .filter(|id| !copied_ids.contains(id))
            .collect();

        // Every member already sits in the destination tier: the move is
        // merely a folder change. Preserve ids and avoid copying bytes;
        // this is the fast path the old move API exposed.
        if copied_ids.is_empty() {
            let mut refiled = Vec::with_capacity(source_ids.len());
            for (index, source_id) in source_ids.iter().enumerate() {
                if let Err(error) = self
                    .refile_reviewed_relocation(*source_id, destination.atlas_id, filing_reviews)
                    .await
                {
                    return Err(RelocationError {
                        error,
                        completed: None,
                        partial: (!refiled.is_empty()).then(|| PartialRelocation {
                            destination,
                            copied_destination_ids: Vec::new(),
                            refiled_source_ids: refiled,
                            pending_source_ids: source_ids[index..].to_vec(),
                        }),
                    });
                }
                refiled.push(*source_id);
            }
            return Ok(MapRelocation {
                source_ids: source_ids.clone(),
                destination_ids: source_ids,
                destination,
            });
        }

        let local_reviews = if mode == RelocationMode::Move
            && destination.storage != MapStorage::Cloud
        {
            let current = self.review_local_moves(&copied_ids).await?;
            for review in &current {
                match reviews.iter().find(|approved| approved.area_id == review.area_id) {
                    Some(approved) if approved != review => return Err(CloudError::InvalidInput(
                        "map sharing changed; review the move again".to_string(),
                    ).into()),
                    None if review.has_shares => return Err(CloudError::InvalidInput(
                        "confirm that moving this shared map ends sharing and deletes shared users' cloud-only content".to_string(),
                    ).into()),
                    _ => {}
                }
            }
            current
        } else {
            Vec::new()
        };

        // Only members whose sources get deleted need the move fence; kept
        // members stay editable throughout and are merely re-filed and
        // relinked at the end.
        let mut move_fences = if mode == RelocationMode::Move {
            let fences = self.begin_relocation(&copied_ids)?;
            self.wait_area_move_quiescent(&fences).await;
            Some(fences)
        } else {
            None
        };

        let (mut snapshots, confirmed_revs) = self.snapshot_relocation_sources(&copied_ids)?;
        let local_guards: HashMap<_, _> = local_reviews
            .iter()
            .map(|review| {
                let snapshot = snapshots
                    .iter()
                    .find(|snapshot| snapshot.area.id == review.area_id)
                    .ok_or(CloudError::AreaNotFound(review.area_id))?;
                let token = snapshot.area.projection_token.clone().ok_or_else(|| {
                    CloudError::InvalidInput(
                        "refresh the cloud map before moving it to local storage".to_string(),
                    )
                })?;
                Ok((
                    review.area_id,
                    (
                        LocalMoveGuard {
                            expected_projection_token: token,
                            sharing_token: review.sharing_token.clone(),
                            shared_loss_confirmed: review.has_shares,
                        },
                        review.auth_generation,
                    ),
                ))
            })
            .collect::<CloudResult<_>>()?;
        for snapshot in &mut snapshots {
            let cloud = self.area_storage(&snapshot.area.id) == MapStorage::Cloud;
            keep_what_a_copy_carries(snapshot, cloud);
            validate_import_document(snapshot)?;
        }
        // Session storage keeps no places, so a move there would delete the
        // original's Secrets and Private additions with it. A copy leaves
        // the original whole and arrives without them.
        if mode == RelocationMode::Move
            && destination.storage == MapStorage::Session
            && snapshots.iter().any(holds_places)
        {
            return Err(CloudError::StructuralConflict(MOVE_DROPS_PLACES.to_string()).into());
        }
        let members: HashSet<AreaId> = source_ids.iter().copied().collect();
        // The backend revision each move snapshot stands on: the last
        // acknowledged revision when one is known, else the cached document
        // revision (queued-but-unsent optimistic bumps ride the copy and are
        // discarded with the source, so they must not inflate the guard).
        let expected_revs: Vec<i64> = confirmed_revs
            .iter()
            .zip(&snapshots)
            .map(|(confirmed, snapshot)| confirmed.unwrap_or(snapshot.area.rev))
            .collect();
        let mut copy_destination_ids = Vec::with_capacity(snapshots.len());
        let mut server_copied = vec![false; snapshots.len()];
        for (index, snapshot) in snapshots.iter().enumerate() {
            let source_id = snapshot.area.id;
            let acknowledged = confirmed_revs[index] == Some(snapshot.area.rev)
                && snapshot.sources.iter().all(|bundle| {
                    self.confirmed_source_rev(source_id, bundle.source) == Some(bundle.rev)
                });
            let copied = if server_copy_applies(
                snapshot,
                &members,
                self.area_storage(&source_id),
                destination.storage,
                mode,
                acknowledged,
            ) {
                match self
                    .copy_cloud_area(source_id, &snapshot.area.name, destination.atlas_id)
                    .await
                {
                    Ok(copied) => copied,
                    Err(error) => {
                        let stranded = cleanup_areas(self, &copy_destination_ids).await;
                        return Err(cleanup_error(error, destination, stranded, &source_ids));
                    }
                }
            } else {
                None
            };
            if let Some(area) = copied {
                copy_destination_ids.push(area.id);
                server_copied[index] = true;
                if let Err(error) = self.adopt_cloud_copy(area.id).await {
                    let stranded = cleanup_areas(self, &copy_destination_ids).await;
                    return Err(cleanup_error(error, destination, stranded, &source_ids));
                }
                continue;
            }
            // Replay-populated destinations carry the in-progress name
            // marker from creation until fully populated (see
            // RELOCATION_IN_PROGRESS_SUFFIX).
            match self
                .create_area_at(
                    format!("{}{RELOCATION_IN_PROGRESS_SUFFIX}", snapshot.area.name),
                    destination,
                )
                .await
            {
                Ok(id) => copy_destination_ids.push(id),
                Err(error) => {
                    let stranded = cleanup_areas(self, &copy_destination_ids).await;
                    return Err(cleanup_error(error, destination, stranded, &source_ids));
                }
            }
        }

        let mut id_map: HashMap<_, _> = kept_ids.iter().map(|id| (*id, *id)).collect();
        id_map.extend(
            copied_ids
                .iter()
                .copied()
                .zip(copy_destination_ids.iter().copied()),
        );
        let destination_ids: Vec<AreaId> = source_ids.iter().map(|id| id_map[id]).collect();
        let final_names: Vec<String> = snapshots
            .iter()
            .map(|snapshot| snapshot.area.name.clone())
            .collect();
        // Server-copied members are already complete; everything else is
        // freshened and replayed. In-set links from replayed members into a
        // server-copied sibling still remap correctly through `id_map`,
        // because the server clone preserves room numbers. The places the
        // snapshot keeps (`keep_what_a_copy_carries`) ride the copy as the
        // copier's own.
        let mut documents: Vec<_> = snapshots
            .into_iter()
            .zip(server_copied.iter().copied())
            .filter(|&(_, copied)| !copied)
            .map(|(document, _)| document)
            .collect();
        freshen_documents(
            &mut documents,
            &id_map,
            &FreshenOptions {
                stamp_local_owner: false,
            },
        );

        if let Err(error) = self.populate_documents(&documents).await {
            let stranded = cleanup_areas(self, &copy_destination_ids).await;
            return Err(cleanup_error(error, destination, stranded, &source_ids));
        }

        // From here every destination copy is populated, but the relocation
        // is not complete until marker renames and any same-tier member work
        // also finish. Failures in that interval report `PartialRelocation`.
        // Fully populated destinations shed the in-progress marker. A
        // rename failure leaves a complete copy under the marker name —
        // recoverable, so it carries the completed result.
        for (index, final_name) in final_names.iter().enumerate() {
            if server_copied[index] {
                continue;
            }
            if let Err(error) = self
                .rename_area_and_wait(copy_destination_ids[index], final_name)
                .await
            {
                return Err(RelocationError {
                    error,
                    completed: None,
                    partial: Some(PartialRelocation {
                        destination,
                        copied_destination_ids: copy_destination_ids.clone(),
                        refiled_source_ids: Vec::new(),
                        pending_source_ids: kept_ids.clone(),
                    }),
                });
            }
        }

        // Kept members' links into copied members follow the fresh ids
        // before any source disappears, so no dangling window opens. The
        // live documents are read rather than the pre-copy view, so a link
        // formed while the copy ran is caught too.
        if let Err(error) = self.retarget_exits_to_copies(&kept_ids, &id_map).await {
            return Err(RelocationError {
                error,
                completed: None,
                partial: Some(PartialRelocation {
                    destination,
                    copied_destination_ids: copy_destination_ids.clone(),
                    refiled_source_ids: Vec::new(),
                    pending_source_ids: kept_ids.clone(),
                }),
            });
        }
        let mut refiled = Vec::with_capacity(kept_ids.len());
        for (index, kept_id) in kept_ids.iter().enumerate() {
            if let Err(error) = self
                .refile_reviewed_relocation(*kept_id, destination.atlas_id, filing_reviews)
                .await
            {
                return Err(RelocationError {
                    error,
                    completed: None,
                    partial: Some(PartialRelocation {
                        destination,
                        copied_destination_ids: copy_destination_ids.clone(),
                        refiled_source_ids: refiled,
                        pending_source_ids: kept_ids[index..].to_vec(),
                    }),
                });
            }
            refiled.push(*kept_id);
        }

        let completed = || MapRelocation {
            source_ids: source_ids.clone(),
            destination_ids: destination_ids.clone(),
            destination,
        };

        if let Some(fences) = move_fences.take() {
            // Destination content is fully acknowledged before the first
            // source delete. A delete failure (including the rev-drift
            // refusal) leaves complete copies on both sides — recoverable and
            // never data loss — so the error carries the completed result:
            // the remedy is pointing at the existing copy, not a retry that
            // would mint another one. Fences not yet committed are dropped
            // here, which reopens their sources for editing.
            for (fence, expected_rev) in fences.into_iter().zip(expected_revs) {
                let result = if let Some((guard, generation)) = local_guards.get(&fence.area_id()) {
                    self.commit_local_area_move(fence, guard, *generation).await
                } else {
                    self.commit_area_move(fence, Some(expected_rev)).await
                };
                if let Err(error) = result {
                    return Err(RelocationError {
                        error,
                        completed: Some(completed()),
                        partial: None,
                    });
                }
            }
        }

        Ok(MapRelocation {
            source_ids,
            destination_ids,
            destination,
        })
    }

    /// Copy or move one whole atlas. Refuses to start unless the cache holds
    /// every member reported by the authoritative inventory, so a failed area
    /// load cannot silently turn into a partial atlas move.
    ///
    /// # Errors
    /// Returns inventory, permission, or storage errors. Completed destination
    /// copies are reported if source deletion fails. Shared cloud maps require
    /// [`Self::relocate_atlas_reviewed`].
    pub async fn relocate_atlas(
        &self,
        source_atlas_id: AtlasId,
        destination_storage: MapStorage,
        mode: RelocationMode,
    ) -> Result<AtlasRelocation, RelocationError<AtlasRelocation>> {
        self.relocate_atlas_reviewed(source_atlas_id, destination_storage, mode, &[])
            .await
    }

    /// Relocate an atlas with the acknowledged sharing reviews of its maps.
    ///
    /// # Errors
    /// Has the same failures as [`Self::relocate_atlas`], and refuses stale
    /// sharing reviews or source snapshots without deleting those originals.
    pub async fn relocate_atlas_reviewed(
        &self,
        source_atlas_id: AtlasId,
        destination_storage: MapStorage,
        mode: RelocationMode,
        reviews: &[LocalMoveReview],
    ) -> Result<AtlasRelocation, RelocationError<AtlasRelocation>> {
        let auth_generation = self.local_move_generation();
        if destination_storage == MapStorage::Session {
            return Err(CloudError::InvalidInput(
                "session storage does not support atlases".to_string(),
            )
            .into());
        }
        let source = self
            .list_atlases()
            .await?
            .into_iter()
            .find(|atlas| atlas.id == source_atlas_id)
            .ok_or_else(|| CloudError::InvalidInput("atlas not found".to_string()))?;
        if mode == RelocationMode::Move
            && self.atlas_storage(&source_atlas_id) == Some(destination_storage)
        {
            return Err(CloudError::InvalidInput(
                "the atlas is already in that storage tier".to_string(),
            )
            .into());
        }
        if !source.is_owner {
            return Err(CloudError::InvalidInput(
                "a shared atlas cannot be copied or moved".to_string(),
            )
            .into());
        }
        if mode == RelocationMode::Move && source.clan_id.is_some() {
            return Err(CloudError::InvalidInput(
                "clan-library atlases cannot be moved to local storage; use Copy".to_string(),
            )
            .into());
        }
        let member_ids: Vec<_> = self
            .get_current_atlas()
            .areas()
            .filter(|area| area.meta().atlas_id == Some(source_atlas_id))
            .map(|area| *area.get_id())
            .collect();

        let listed_member_ids: HashSet<_> = self
            .list_areas()
            .await?
            .into_iter()
            .filter(|area| area.atlas_id == Some(source_atlas_id))
            .map(|area| area.id)
            .collect();
        let cached_member_ids: HashSet<_> = member_ids.iter().copied().collect();
        if listed_member_ids != cached_member_ids
            || usize::try_from(source.area_count).ok() != Some(cached_member_ids.len())
        {
            return Err(CloudError::PendingOperations(
                "not every map in this atlas is loaded; refresh maps before copying or moving the atlas"
                    .to_string(),
            )
            .into());
        }

        let local_reviews = if mode == RelocationMode::Move
            && destination_storage == MapStorage::Local
        {
            let current = self.review_local_moves(&member_ids).await?;
            for review in &current {
                match reviews.iter().find(|approved| approved.area_id == review.area_id) {
                    Some(approved) if approved != review => return Err(CloudError::InvalidInput(
                        "map sharing changed; review the move again".into(),
                    ).into()),
                    None if review.has_shares => return Err(CloudError::InvalidInput(
                        "confirm the loss of shared users' cloud-only content before moving this atlas".into(),
                    ).into()),
                    _ => {}
                }
            }
            current
        } else {
            Vec::new()
        };

        let mut move_fences = if mode == RelocationMode::Move {
            let fences = self.begin_relocation(&member_ids)?;
            self.wait_area_move_quiescent(&fences).await;
            Some(fences)
        } else {
            None
        };

        // The backend revision each member's copy stands on, captured after
        // quiescence and before the copy (see `relocate_areas`).
        let member_cache = self.get_current_atlas();
        let local_guards: HashMap<_, _> = local_reviews
            .iter()
            .map(|review| {
                let token = member_cache
                    .get_area(&review.area_id)
                    .and_then(|area| area.meta().projection_token.clone())
                    .ok_or_else(|| {
                        CloudError::InvalidInput(
                            "refresh the cloud maps before moving this atlas".into(),
                        )
                    })?;
                Ok((
                    review.area_id,
                    (
                        LocalMoveGuard {
                            expected_projection_token: token,
                            sharing_token: review.sharing_token.clone(),
                            shared_loss_confirmed: review.has_shares,
                        },
                        review.auth_generation,
                    ),
                ))
            })
            .collect::<CloudResult<_>>()?;
        let expected_revs: Vec<i64> = member_ids
            .iter()
            .map(|id| {
                self.confirmed_area_rev(*id)
                    .or_else(|| member_cache.get_area(id).map(|area| area.get_rev()))
                    .unwrap_or(1)
            })
            .collect();
        drop(member_cache);

        let destination_atlas_name = source.name;
        let destination_atlas = self
            .create_atlas_at(destination_atlas_name.clone(), destination_storage)
            .await?;
        let destination = MapDestination::in_atlas(destination_storage, destination_atlas.id);
        let areas = match self
            .relocate_areas(member_ids.clone(), destination, RelocationMode::Copy)
            .await
        {
            Ok(areas) => areas,
            Err(failure) => {
                let RelocationError { error, partial, .. } = failure;
                let cleanup_ids = partial
                    .as_ref()
                    .map_or_else(Vec::new, |partial| partial.copied_destination_ids.clone());
                let stranded = cleanup_areas(self, &cleanup_ids).await;
                if !stranded.is_empty() {
                    return Err(RelocationError {
                        error,
                        completed: None,
                        partial: Some(PartialRelocation {
                            destination,
                            copied_destination_ids: stranded,
                            refiled_source_ids: partial.as_ref().map_or_else(Vec::new, |partial| {
                                partial.refiled_source_ids.clone()
                            }),
                            pending_source_ids: partial
                                .map_or(member_ids, |partial| partial.pending_source_ids),
                        }),
                    });
                }
                if let Err(cleanup_error) = self.delete_atlas(destination_atlas.id).await {
                    warn!(
                        "failed to clean up destination atlas {} after relocation error: {cleanup_error}",
                        destination_atlas.id
                    );
                    return Err(RelocationError {
                        error,
                        completed: None,
                        partial: Some(PartialRelocation {
                            destination,
                            copied_destination_ids: Vec::new(),
                            refiled_source_ids: Vec::new(),
                            pending_source_ids: member_ids,
                        }),
                    });
                }
                return Err(error.into());
            }
        };

        if let Some(fences) = move_fences.take() {
            let completed = || AtlasRelocation {
                source_atlas_id,
                destination_atlas_id: destination_atlas.id,
                destination_atlas_name: destination_atlas_name.clone(),
                areas: areas.clone(),
            };
            for (fence, expected_rev) in fences.into_iter().zip(expected_revs) {
                let result = if let Some((guard, generation)) = local_guards.get(&fence.area_id()) {
                    self.commit_local_area_move(fence, guard, *generation).await
                } else {
                    self.commit_area_move(fence, Some(expected_rev)).await
                };
                if let Err(error) = result {
                    return Err(RelocationError {
                        error,
                        completed: Some(completed()),
                        partial: None,
                    });
                }
            }
            let removal = if destination_storage == MapStorage::Local {
                self.finish_local_atlas_move(source_atlas_id, auth_generation)
                    .await
            } else {
                self.delete_atlas(source_atlas_id).await
            };
            if let Err(error) = removal {
                return Err(RelocationError {
                    error,
                    completed: Some(completed()),
                    partial: None,
                });
            }
        }

        Ok(AtlasRelocation {
            source_atlas_id,
            destination_atlas_id: destination_atlas.id,
            destination_atlas_name,
            areas,
        })
    }

    /// Populate several already-created empty area headers in dependency
    /// order. All rooms across the set land before any cross-area exits.
    ///
    /// Local-tier destinations take the wholesale write: the freshened
    /// document already is the final content, so one atomic durable file
    /// write per area replaces thousands of per-envelope rewrite cycles.
    /// Every destination of one relocation shares a tier, so a set either
    /// bulk-writes entirely or replays envelopes entirely; in-set cross-area
    /// exits between bulk-written siblings are plain stored references the
    /// local tier never foreign-key-checks, making write order free.
    ///
    /// Cloud and session destinations replay envelopes: neither has a bulk
    /// content-upload path (eligible cloud→cloud copies take the
    /// server-side clone before reaching here), so a large relocation into
    /// those tiers remains bounded by sequential envelope application.
    async fn populate_documents(&self, documents: &[AreaWithDetails]) -> CloudResult<()> {
        let mut envelope_fed = Vec::with_capacity(documents.len());
        for document in documents {
            if !self.bulk_populate_local_area(document.clone()).await? {
                envelope_fed.push(document);
            }
        }
        let documents = envelope_fed;

        let room_batches = documents
            .iter()
            .flat_map(|document| {
                chunk_ops(
                    document.area.id,
                    document.rooms.iter().map(|room| AreaMutation::UpsertRoom {
                        room_source: None,
                        room_number: room.room_number,
                        body: RoomUpdates {
                            title: Some(room.title.clone()),
                            description: Some(room.description.clone()),
                            level: Some(room.level),
                            x: Some(room.x),
                            y: Some(room.y),
                            color: Some(room.color.clone()),
                            external_id: Some(room.external_id.clone()),
                        },
                    }),
                    "Copy map rooms",
                )
            })
            .collect();
        self.stage_and_wait(room_batches).await?;

        let metadata_batches =
            documents
                .iter()
                .flat_map(|document| {
                    let area_props = document.properties.iter().map(|property| {
                        AreaMutation::UpsertAreaProperty {
                            name: property.name.clone(),
                            value: property.value.clone(),
                        }
                    });
                    let room_props = document.rooms.iter().flat_map(|room| {
                        let properties = room.properties.iter().map(|property| {
                            AreaMutation::UpsertRoomProperty {
                                room_source: None,
                                room_number: room.room_number,
                                name: property.name.clone(),
                                value: property.value.clone(),
                            }
                        });
                        let tags = room.tags.iter().map(|tag| AreaMutation::AddRoomTag {
                            room_source: None,
                            room_number: room.room_number,
                            tag: tag.clone(),
                        });
                        properties.chain(tags)
                    });
                    chunk_ops(
                        document.area.id,
                        area_props.chain(room_props),
                        "Copy map properties",
                    )
                })
                .collect();
        self.stage_and_wait(metadata_batches).await?;

        let connection_batches = documents
            .iter()
            .flat_map(|document| connection_batches(document))
            .collect();
        self.stage_and_wait(connection_batches).await?;

        let decoration_batches = documents
            .iter()
            .flat_map(|document| {
                let labels = document
                    .labels
                    .iter()
                    .map(|label| AreaMutation::CreateLabel {
                        body: label_args(label),
                    });
                let shapes = document
                    .shapes
                    .iter()
                    .map(|shape| AreaMutation::CreateShape {
                        body: shape_args(shape),
                    });
                chunk_ops(
                    document.area.id,
                    labels.chain(shapes),
                    "Copy map decorations",
                )
            })
            .collect();
        self.stage_and_wait(decoration_batches).await?;

        for document in documents {
            if self.area_storage(&document.area.id) == MapStorage::Cloud {
                self.populate_places(document).await?;
            }
        }
        Ok(())
    }

    /// Replays a populated cloud destination's places: each Secret becomes
    /// an owner Secret of the destination under the id the service gives
    /// it, with the same name, color and content, and the Private additions
    /// become the caller's own there. Every map room is already in place,
    /// so a place's data for map rooms and its links into them land whole.
    async fn populate_places(&self, document: &AreaWithDetails) -> CloudResult<()> {
        let area_id = document.area.id;
        for bundle in document.sources.iter().filter(|bundle| holds_place(bundle)) {
            let source = match bundle.source {
                SourceId::Secret(_) => {
                    let name = bundle.name.as_deref().unwrap_or_default();
                    self.create_secret(area_id, name, bundle.color.as_deref())
                        .await?
                        .source
                }
                other => other,
            };
            let bundle = renamed_place(bundle, source);
            self.stage_and_wait(place_batches(area_id, &bundle, PlaceStage::Rooms))
                .await?;
            self.stage_and_wait(place_batches(area_id, &bundle, PlaceStage::Properties))
                .await?;
            self.stage_and_wait(place_batches(area_id, &bundle, PlaceStage::Connections))
                .await?;
            self.stage_and_wait(place_batches(area_id, &bundle, PlaceStage::Decorations))
                .await?;
        }
        Ok(())
    }

    /// Retargets, in the kept members of a mixed-tier move, every exit
    /// aimed at a copied member: same room numbers, fresh area id. Reads
    /// the live documents and stages ordinary envelope batches, chunked at
    /// the envelope cap.
    async fn retarget_exits_to_copies(
        &self,
        kept_ids: &[AreaId],
        id_map: &HashMap<AreaId, AreaId>,
    ) -> CloudResult<()> {
        let documents = self.snapshot_areas(kept_ids)?;
        let mut batches = Vec::new();
        for document in &documents {
            let retargets = document
                .rooms
                .iter()
                .flat_map(|room| room.exits.iter())
                .filter_map(|exit| {
                    let target = exit.to_area_id?;
                    let remapped = *id_map.get(&target)?;
                    (remapped != target).then_some(AreaMutation::UpdateExit {
                        exit_id: exit.id,
                        body: crate::ExitUpdates {
                            to_area_id: Some(remapped),
                            ..crate::ExitUpdates::default()
                        },
                    })
                })
                .collect::<Vec<_>>();
            batches.extend(chunk_ops(document.area.id, retargets, "Relink moved maps"));
        }
        self.stage_and_wait(batches).await
    }

    async fn stage_and_wait(&self, batches: Vec<AreaMutationBatch>) -> CloudResult<()> {
        if batches.is_empty() {
            return Ok(());
        }
        let submissions = self.mutate_batches(batches)?;
        for operation_id in submissions
            .into_iter()
            .filter_map(MutationSubmission::operation_id)
        {
            self.wait_for_mutation(operation_id).await?;
        }
        Ok(())
    }
}

/// Whether one source can take the server-side cloud clone
/// (`POST /areas/{id}/copy`) instead of freshen-and-replay. A cloud→cloud
/// copy takes it whenever it can, because the service copies what the
/// copier may copy (smudgy-cloudflare format-3.md §5.2) with every place
/// in it, where the replay rebuilds them one write at a time:
///
/// - the service copies its own state, where relocation copies the local
///   optimistic snapshot — so every source the snapshot carries must stand
///   at its backend-acknowledged revision (`acknowledged`: no queued edits
///   the service has not seen);
/// - a clone links into the original of another member of the set, which
///   only the replay's set-wide remap leads into that member's copy — so a
///   member with an exit into another member, from the map or from a
///   place, takes the replay;
/// - links into maps outside the set follow the service's copy rule: they
///   stay where the copier reads the map they lead into, and dangle
///   elsewhere;
/// - both paths mint fresh area/connection/exit identities and preserve
///   room numbers, so in-set inbound remaps hold either way.
///
/// The clone additionally records `copied_from` provenance, which the
/// replay path clears; accepted, since provenance is owner-only metadata
/// and truthful for a copy.
fn server_copy_applies(
    snapshot: &AreaWithDetails,
    members: &HashSet<AreaId>,
    source_storage: MapStorage,
    destination_storage: MapStorage,
    mode: RelocationMode,
    acknowledged: bool,
) -> bool {
    let own = snapshot.area.id;
    let place_exits = snapshot.sources.iter().flat_map(|bundle| {
        bundle
            .rooms
            .iter()
            .flat_map(|room| &room.exits)
            .chain(bundle.room_data.iter().flat_map(|data| &data.exits))
    });
    mode == RelocationMode::Copy
        && source_storage == MapStorage::Cloud
        && destination_storage == MapStorage::Cloud
        && acknowledged
        && snapshot
            .rooms
            .iter()
            .flat_map(|room| &room.exits)
            .chain(place_exits)
            .all(|exit| {
                exit.to_area_id
                    .is_none_or(|target| target == own || !members.contains(&target))
            })
}

fn chunk_ops(
    area_id: AreaId,
    operations: impl IntoIterator<Item = AreaMutation>,
    description: &str,
) -> Vec<AreaMutationBatch> {
    let mut batches = Vec::new();
    let mut current = Vec::with_capacity(MAX_MUTATION_OPERATIONS);
    for operation in operations {
        current.push(operation);
        if current.len() == MAX_MUTATION_OPERATIONS {
            batches.push(AreaMutationBatch::strict(
                area_id,
                std::mem::take(&mut current),
                description,
            ));
        }
    }
    if !current.is_empty() {
        batches.push(AreaMutationBatch::strict(area_id, current, description));
    }
    batches
}

/// Connection creation and its one/two member exits must stay in one
/// envelope; a connection without members is structurally invalid at an
/// envelope boundary.
fn connection_batches(document: &AreaWithDetails) -> Vec<AreaMutationBatch> {
    let exits = document
        .rooms
        .iter()
        .flat_map(|room| room.exits.iter().map(|exit| (None, room.room_number, exit)));
    grouped_connection_batches(
        document.area.id,
        SourceId::Map,
        &document.connections,
        exits,
    )
}

/// An exit and the room it leaves: `room_source` for one of a source's own
/// rooms, none for a map room.
type LeavingExit<'a> = (Option<SourceId>, RoomNumber, &'a Exit);

/// [`connection_batches`] for any source of a map.
fn grouped_connection_batches<'a>(
    area_id: AreaId,
    source: SourceId,
    connections: &[Connection],
    exits: impl IntoIterator<Item = LeavingExit<'a>>,
) -> Vec<AreaMutationBatch> {
    // One pass over every exit builds the member index; scanning every
    // room's exits per connection would be quadratic in area size.
    let mut members: HashMap<ConnectionId, Vec<LeavingExit<'a>>> =
        HashMap::with_capacity(connections.len());
    for (room_source, room_number, exit) in exits {
        members
            .entry(exit.connection_id)
            .or_default()
            .push((room_source, room_number, exit));
    }

    let batch = |operations| {
        AreaMutationBatch::strict(area_id, operations, "Copy map connections").in_source(source)
    };
    let mut batches = Vec::new();
    let mut current = Vec::with_capacity(MAX_MUTATION_OPERATIONS);
    for connection in connections {
        let mut group = vec![AreaMutation::CreateConnection {
            body: ConnectionArgs::from(connection),
        }];
        group.extend(
            members
                .get(&connection.id)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(
                    |&(room_source, room_number, exit)| AreaMutation::CreateExit {
                        room_source,
                        room_number,
                        body: ExitArgs {
                            to_source: exit.to_source,
                            id: Some(exit.id),
                            connection_id: Some(exit.connection_id),
                            new_connection_id: None,
                            from_direction: exit.from_direction,
                            to_area_id: exit.to_area_id,
                            to_room_number: exit.to_room_number,
                            to_direction: exit.to_direction,
                            path: Some(exit.path.clone()),
                            is_hidden: exit.is_hidden,
                            door: exit.door.clone(),
                            weight: exit.weight,
                            command: Some(exit.command.clone()),
                        },
                    },
                ),
        );
        if current.len() + group.len() > MAX_MUTATION_OPERATIONS && !current.is_empty() {
            batches.push(batch(std::mem::take(&mut current)));
        }
        current.extend(group);
    }
    if !current.is_empty() {
        batches.push(batch(current));
    }
    batches
}

/// A label's creation under its own id.
fn label_args(label: &crate::Label) -> LabelArgs {
    LabelArgs {
        id: Some(label.id),
        level: label.level,
        x: label.x,
        y: label.y,
        width: label.width,
        height: label.height,
        horizontal_alignment: label.horizontal_alignment.clone(),
        vertical_alignment: label.vertical_alignment.clone(),
        text: label.text.clone(),
        color: label.color.clone(),
        background_color: Some(label.background_color.clone()),
        font_size: label.font_size,
        font_weight: label.font_weight,
    }
}

/// A shape's creation under its own id.
fn shape_args(shape: &crate::Shape) -> ShapeArgs {
    ShapeArgs {
        id: Some(shape.id),
        level: shape.level,
        x: shape.x,
        y: shape.y,
        width: shape.width,
        height: shape.height,
        background_color: shape.background_color.clone(),
        stroke_color: shape.stroke_color.clone(),
        shape_type: shape.shape_type.clone(),
        border_radius: shape.border_radius,
        stroke_width: Some(shape.stroke_width),
    }
}

/// The refusal of a move into storage that keeps no places (session maps)
/// of a map holding Secrets or Private additions.
pub const MOVE_DROPS_PLACES: &str = "move_drops_places";

/// Whether a source of a snapshot is a place: a Secret, or the caller's
/// Private additions.
fn holds_place(bundle: &SourceBundle) -> bool {
    !bundle.source.is_map()
}

fn holds_places(document: &AreaWithDetails) -> bool {
    document.sources.iter().any(holds_place)
}

/// A freshened place under the id its destination gave it: the source and
/// every reference to its own rooms.
fn renamed_place(bundle: &SourceBundle, source: SourceId) -> SourceBundle {
    let old = bundle.source;
    let mut bundle = bundle.clone();
    bundle.source = source;
    let rename = |named: &mut Option<SourceId>| {
        if *named == Some(old) {
            *named = Some(source);
        }
    };
    for connection in &mut bundle.connections {
        rename(&mut connection.endpoint_a.source);
        if let Some(endpoint) = connection.endpoint_b.as_mut() {
            rename(&mut endpoint.source);
        }
    }
    for exit in bundle
        .rooms
        .iter_mut()
        .flat_map(|room| room.exits.iter_mut())
        .chain(
            bundle
                .room_data
                .iter_mut()
                .flat_map(|data| data.exits.iter_mut()),
        )
    {
        rename(&mut exit.to_source);
    }
    bundle
}

/// The order a place is replayed in: its rooms, then what it keeps on
/// rooms, then the links among them, then its labels and shapes.
#[derive(Clone, Copy)]
enum PlaceStage {
    Rooms,
    Properties,
    Connections,
    Decorations,
}

/// One stage of replaying `bundle` into map `area_id`, written to the
/// bundle's own source. Its own rooms are named with `room_source`, map
/// rooms without it.
fn place_batches(
    area_id: AreaId,
    bundle: &SourceBundle,
    stage: PlaceStage,
) -> Vec<AreaMutationBatch> {
    let source = bundle.source;
    let own = Some(source);
    let in_source = |batches: Vec<AreaMutationBatch>| {
        batches
            .into_iter()
            .map(|batch| batch.in_source(source))
            .collect()
    };
    match stage {
        PlaceStage::Rooms => in_source(chunk_ops(
            area_id,
            bundle.rooms.iter().map(|room| AreaMutation::UpsertRoom {
                room_source: own,
                room_number: room.room_number,
                body: RoomUpdates {
                    title: Some(room.title.clone()),
                    description: Some(room.description.clone()),
                    level: Some(room.level),
                    x: Some(room.x),
                    y: Some(room.y),
                    color: Some(room.color.clone()),
                    external_id: Some(room.external_id.clone()),
                },
            }),
            "Copy map rooms",
        )),
        PlaceStage::Properties => {
            let properties =
                bundle
                    .properties
                    .iter()
                    .map(|property| AreaMutation::UpsertAreaProperty {
                        name: property.name.clone(),
                        value: property.value.clone(),
                    });
            let own_rooms = bundle
                .rooms
                .iter()
                .map(|room| (own, room.room_number, &room.properties, &room.tags));
            let map_rooms = bundle
                .room_data
                .iter()
                .map(|data| (None, data.room_number, &data.properties, &data.tags));
            let rooms = own_rooms.chain(map_rooms).flat_map(
                |(room_source, room_number, properties, tags)| {
                    properties
                        .iter()
                        .map(move |property| AreaMutation::UpsertRoomProperty {
                            room_source,
                            room_number,
                            name: property.name.clone(),
                            value: property.value.clone(),
                        })
                        .chain(tags.iter().map(move |tag| AreaMutation::AddRoomTag {
                            room_source,
                            room_number,
                            tag: tag.clone(),
                        }))
                },
            );
            in_source(chunk_ops(
                area_id,
                properties.chain(rooms),
                "Copy map properties",
            ))
        }
        PlaceStage::Connections => {
            let exits = bundle
                .rooms
                .iter()
                .flat_map(|room| {
                    room.exits
                        .iter()
                        .map(move |exit| (own, room.room_number, exit))
                })
                .chain(bundle.room_data.iter().flat_map(|data| {
                    data.exits
                        .iter()
                        .map(move |exit| (None, data.room_number, exit))
                }));
            grouped_connection_batches(area_id, source, &bundle.connections, exits)
        }
        PlaceStage::Decorations => {
            let labels = bundle.labels.iter().map(|label| AreaMutation::CreateLabel {
                body: label_args(label),
            });
            let shapes = bundle.shapes.iter().map(|shape| AreaMutation::CreateShape {
                body: shape_args(shape),
            });
            in_source(chunk_ops(
                area_id,
                labels.chain(shapes),
                "Copy map decorations",
            ))
        }
    }
}

/// Keep sources the caller may copy. Exits belong to their owning source,
/// including exits with unreadable destinations. The freshener preserves
/// those exits and makes destinations outside the copied set dangling.
pub(crate) fn keep_what_a_copy_carries(document: &mut AreaWithDetails, cloud: bool) {
    if cloud {
        document.keep_copyable_sources();
    }
}

const OWNER_SECRET_ACTIONS: [&str; 8] = [
    "read",
    "add",
    "edit",
    "remove",
    "manage_access",
    "copy",
    "rename",
    "delete",
];

/// Freshens one of a copied map's sources as [`freshen_documents`] does the
/// map: a Secret becomes an owner Secret of the copy under a fresh id, with
/// the same name, color and content and nothing of its owners, clan or
/// grants; every exit, connection, label and shape takes a fresh id; and its
/// exits follow the map's rules: into the copied map, or a set member, they
/// lead into the copy, and into any other map they dangle.
fn freshen_source_bundle(
    bundle: &mut SourceBundle,
    id_map: &HashMap<AreaId, AreaId>,
    leaves_area: impl Fn(AreaId) -> bool,
) {
    let old = bundle.source;
    let own = match old {
        SourceId::Secret(_) => {
            bundle.ownership = Some(crate::clan_secrets::ownership::OWNER.to_string());
            bundle.clan_id = None;
            bundle.actions = OWNER_SECRET_ACTIONS
                .iter()
                .map(|action| (*action).to_string())
                .collect();
            old
        }
        other => other,
    };
    bundle.source = own;
    bundle.rev = 1;
    let renamed =
        |source: Option<SourceId>| source.map(|source| if source == old { own } else { source });
    for label in &mut bundle.labels {
        label.id = LabelId(uuid::Uuid::new_v4());
    }
    for shape in &mut bundle.shapes {
        shape.id = ShapeId(uuid::Uuid::new_v4());
    }
    let connection_map: HashMap<ConnectionId, ConnectionId> = bundle
        .connections
        .iter()
        .map(|connection| (connection.id, ConnectionId::new()))
        .collect();
    for connection in &mut bundle.connections {
        connection.id = connection_map[&connection.id];
        connection.endpoint_a.source = renamed(connection.endpoint_a.source);
        if let Some(endpoint) = connection.endpoint_b.as_mut() {
            endpoint.source = renamed(endpoint.source);
        }
    }
    let exits = bundle
        .rooms
        .iter_mut()
        .flat_map(|room| room.exits.iter_mut())
        .chain(
            bundle
                .room_data
                .iter_mut()
                .flat_map(|data| data.exits.iter_mut()),
        );
    let mut leaving: HashSet<ConnectionId> = HashSet::new();
    for exit in exits {
        exit.id = ExitId::new();
        exit.connection_id = connection_map
            .get(&exit.connection_id)
            .copied()
            .unwrap_or_else(ConnectionId::new);
        exit.to_unknown = false;
        exit.to_area_token = None;
        exit.to_source = renamed(exit.to_source);
        exit.to_area_id = match exit.to_area_id {
            Some(target) if id_map.contains_key(&target) => Some(id_map[&target]),
            Some(_) => {
                exit.to_room_number = None;
                exit.to_direction = None;
                exit.to_source = None;
                None
            }
            None => None,
        };
        if exit.to_area_id.is_some_and(&leaves_area) {
            leaving.insert(exit.connection_id);
        }
    }
    for connection in &mut bundle.connections {
        if connection.kind == ConnectionKind::External && !leaving.contains(&connection.id) {
            connection.kind = ConnectionKind::Dangling;
            connection.endpoint_b = None;
        }
    }
}

/// Allocate Secret identities for the whole copied set before rewriting any
/// reference. A reference into a source omitted from the copy must not point
/// at the original source, or fall back to an ordinary room of the same number.
fn freshen_source_references(documents: &mut [AreaWithDetails], id_map: &HashMap<AreaId, AreaId>) {
    let mut sources: HashMap<_, _> = id_map
        .keys()
        .map(|map| ((*map, SourceId::Map), SourceId::Map))
        .collect();
    for document in documents.iter() {
        sources.insert((document.area.id, SourceId::Map), SourceId::Map);
        for bundle in &document.sources {
            sources.insert(
                (document.area.id, bundle.source),
                match bundle.source {
                    SourceId::Secret(_) => SourceId::Secret(uuid::Uuid::new_v4()),
                    other => other,
                },
            );
        }
    }
    for document in documents {
        let map = document.area.id;
        let rewrite = |rooms: &mut Vec<crate::RoomWithDetails>,
                       data: &mut Vec<crate::RoomData>,
                       connections: &mut Vec<Connection>| {
            data.retain(|data| {
                sources.contains_key(&(map, data.room_source.unwrap_or(SourceId::Map)))
            });
            for data in data.iter_mut() {
                data.room_source = data.room_source.map(|source| sources[&(map, source)]);
            }
            connections.retain(|connection| {
                sources.contains_key(&(map, connection.endpoint_a.source.unwrap_or(SourceId::Map)))
            });
            for connection in connections.iter_mut() {
                connection.endpoint_a.source = connection
                    .endpoint_a
                    .source
                    .map(|source| sources[&(map, source)]);
                if let Some(endpoint) = connection.endpoint_b.as_mut() {
                    if let Some(source) =
                        sources.get(&(map, endpoint.source.unwrap_or(SourceId::Map)))
                    {
                        endpoint.source = endpoint.source.map(|_| *source);
                    } else {
                        connection.endpoint_b = None;
                    }
                }
            }
            let kept: HashSet<ConnectionId> =
                connections.iter().map(|connection| connection.id).collect();
            for exits in rooms
                .iter_mut()
                .map(|room| &mut room.exits)
                .chain(data.iter_mut().map(|data| &mut data.exits))
            {
                exits.retain(|exit| kept.contains(&exit.connection_id));
                for exit in exits {
                    if let Some(target) =
                        exit.to_area_id.filter(|target| id_map.contains_key(target))
                    {
                        if id_map[&target] == target {
                            continue;
                        }
                        if let Some(source) =
                            sources.get(&(target, exit.to_source.unwrap_or(SourceId::Map)))
                        {
                            exit.to_source = exit.to_source.map(|_| *source);
                        } else {
                            exit.to_area_id = None;
                            exit.to_room_number = None;
                            exit.to_direction = None;
                            exit.to_source = None;
                        }
                    }
                }
            }
        };
        rewrite(
            &mut document.rooms,
            &mut document.room_data,
            &mut document.connections,
        );
        for bundle in &mut document.sources {
            let source = sources[&(map, bundle.source)];
            rewrite(
                &mut bundle.rooms,
                &mut bundle.room_data,
                &mut bundle.connections,
            );
            bundle.source = source;
        }
    }
}

/// How [`freshen_documents`] treats viewer-only metadata.
pub(crate) struct FreshenOptions {
    /// Stamp locally-owned access and drop the owner's nickname — the
    /// JSON-import contract, which resets foreign metadata to a
    /// locally-owned area. Relocation keeps them: a copy of one's own map
    /// preserves the viewer's projection verbatim.
    pub stamp_local_owner: bool,
}

/// The shared identity freshener behind relocation and the §8.4 JSON
/// import. For every document: stamps the new area id from `id_map` (which
/// must cover every document in the set), resets viewer/cloud metadata to
/// that of a fresh unsynced area, mints fresh label/shape/connection/exit
/// identities (keeping exit→Connection membership consistent), remaps
/// cross-area exit targets that stay within the set, drops targets that
/// leave it, and demotes External Connections that no longer leave their
/// area to Dangling — exactly as a live edit would convert them.
pub(crate) fn freshen_documents(
    documents: &mut [AreaWithDetails],
    id_map: &HashMap<AreaId, AreaId>,
    options: &FreshenOptions,
) {
    freshen_source_references(documents, id_map);
    for document in documents {
        document.area.id = id_map[&document.area.id];
        document.area.atlas_id = None;
        document.area.atlas_name = None;
        document.area.user_id = None;
        // The source map's clan, the caller's actions there, and its
        // ownership there: a fresh area is in no clan.
        document.area.clan_id = None;
        document.area.clan_name = None;
        document.area.actions = None;
        document.area.clan_ownership = crate::clan_maps::ClanOwnership::default();
        document.area.rev = 1;
        document.area.copied_from_area_id = None;
        document.area.copied_from_rev = None;
        document.area.copied_at = None;
        document.area.family_token = None;
        document.area.projection_token = None;
        document.linked_areas.clear();
        if options.stamp_local_owner {
            document.area.access = Some(crate::AreaAccess::OWNER);
            document.area.owner_nickname = None;
        }

        for label in &mut document.labels {
            label.id = LabelId(uuid::Uuid::new_v4());
        }
        for shape in &mut document.shapes {
            shape.id = ShapeId(uuid::Uuid::new_v4());
        }
        let connection_map: HashMap<ConnectionId, ConnectionId> = document
            .connections
            .iter()
            .map(|connection| (connection.id, ConnectionId::new()))
            .collect();
        for connection in &mut document.connections {
            connection.id = connection_map[&connection.id];
        }
        let area_id = document.area.id;
        for exit in document
            .rooms
            .iter_mut()
            .flat_map(|room| &mut room.exits)
            .chain(
                document
                    .room_data
                    .iter_mut()
                    .flat_map(|data| &mut data.exits),
            )
        {
            exit.id = ExitId::new();
            exit.connection_id = connection_map[&exit.connection_id];
            exit.to_unknown = false;
            exit.to_area_token = None;
            exit.to_area_id = match exit.to_area_id {
                Some(old) if id_map.contains_key(&old) => Some(id_map[&old]),
                Some(_) => {
                    exit.to_room_number = None;
                    exit.to_direction = None;
                    exit.to_source = None;
                    None
                }
                None => None,
            };
        }
        let leaves_area: HashSet<ConnectionId> = document
            .rooms
            .iter()
            .flat_map(|room| room.exits.iter())
            .chain(document.room_data.iter().flat_map(|data| data.exits.iter()))
            .filter(|exit| exit.to_area_id.is_some_and(|target| target != area_id))
            .map(|exit| exit.connection_id)
            .collect();
        for connection in &mut document.connections {
            if connection.kind == ConnectionKind::External && !leaves_area.contains(&connection.id)
            {
                connection.kind = ConnectionKind::Dangling;
                connection.endpoint_b = None;
            }
        }
        for bundle in &mut document.sources {
            freshen_source_bundle(bundle, id_map, |target| target != area_id);
        }
    }
}

fn cleanup_error<T>(
    error: CloudError,
    destination: MapDestination,
    stranded: Vec<AreaId>,
    pending_sources: &[AreaId],
) -> RelocationError<T> {
    RelocationError {
        error,
        completed: None,
        partial: (!stranded.is_empty()).then(|| PartialRelocation {
            destination,
            copied_destination_ids: stranded,
            refiled_source_ids: Vec::new(),
            pending_source_ids: pending_sources.to_vec(),
        }),
    }
}

async fn cleanup_areas(mapper: &Mapper, area_ids: &[AreaId]) -> Vec<AreaId> {
    let mut stranded = Vec::new();
    for area_id in area_ids.iter().rev() {
        if let Err(error) = mapper.delete_area_and_wait(*area_id).await {
            warn!("failed to clean up relocated area {area_id}: {error}");
            stranded.push(*area_id);
        }
    }
    stranded.reverse();
    stranded
}

#[cfg(test)]
mod tests {
    use std::{path::PathBuf, sync::Arc};

    use super::*;
    use crate::{CompositeBackend, LocalBackend, RoomNumber, Uuid, mapper::RoomKey};

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "smudgy-relocation-{tag}-{}-{}",
            std::process::id(),
            Uuid::new_v4()
        ))
    }

    async fn mapper(tag: &str) -> (Mapper, PathBuf) {
        let root = temp_root(tag);
        let backend = CompositeBackend::new(
            Arc::new(LocalBackend::new(root.join("local"))),
            Arc::new(LocalBackend::new(root.join("cloud"))),
        );
        let mapper = Mapper::new(Arc::new(backend), root.join("cache"));
        mapper.load_all_areas().await.expect("load empty tiers");
        (mapper, root)
    }

    async fn wait(mapper: &Mapper, submission: MutationSubmission) {
        if let Some(operation_id) = submission.operation_id() {
            mapper
                .wait_for_mutation(operation_id)
                .await
                .expect("mutation acknowledged");
        }
    }

    /// A local map holding a Secret, as a local copy of a cloud map does.
    async fn local_map_with_a_secret(mapper: &Mapper) -> AreaId {
        let source = mapper
            .create_area_at(
                "Copied Keep".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create local source");
        let mut document = blank_document(source, "Copied Keep");
        document.rooms = vec![plain_room(1)];
        document.sources = vec![secret_bundle(source, Uuid::new_v4(), &["read", "copy"])];
        assert!(
            mapper
                .bulk_populate_local_area(document)
                .await
                .expect("writes")
        );
        mapper.load_all_areas().await.expect("reload");
        source
    }

    fn place_count(mapper: &Mapper, area: AreaId) -> usize {
        mapper
            .get_current_atlas()
            .get_area(&area)
            .map(|area| area.source_layers().len())
            .unwrap_or_default()
    }

    /// A move into storage that can't take a map's Secret fails before it
    /// deletes anything: the original keeps the Secret, and no copy is left.
    #[tokio::test]
    async fn a_local_maps_secret_survives_a_move_to_a_cloud_that_refuses_it() {
        let (mapper, root) = mapper("move-secret").await;
        let source = local_map_with_a_secret(&mapper).await;
        let before = place_count(&mapper, source);
        assert!(before > 0, "the source holds its Secret");
        let areas_before = mapper.get_current_atlas().areas().count();

        let refused = mapper
            .relocate_areas(
                vec![source],
                MapDestination::loose(MapStorage::Cloud),
                RelocationMode::Move,
            )
            .await
            .expect_err("this cloud tier keeps no Secrets");
        assert!(refused.completed.is_none() && refused.partial.is_none());
        assert_eq!(place_count(&mapper, source), before, "the Secret stays");
        assert_eq!(
            mapper.get_current_atlas().areas().count(),
            areas_before,
            "the failed copy is cleaned up"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// Session storage keeps no places: a move there of a map holding one
    /// is refused before anything is created, and a copy arrives without
    /// them while the original keeps them.
    #[tokio::test]
    async fn a_map_holding_places_moves_into_no_session_but_copies_there() {
        let (mapper, root) = mapper("session-places").await;
        let source = local_map_with_a_secret(&mapper).await;
        let before = place_count(&mapper, source);
        let areas_before = mapper.get_current_atlas().areas().count();

        let refused = mapper
            .relocate_areas(
                vec![source],
                MapDestination::loose(MapStorage::Session),
                RelocationMode::Move,
            )
            .await
            .expect_err("a session map keeps no places");
        assert!(
            matches!(&refused.error, CloudError::StructuralConflict(code) if code == MOVE_DROPS_PLACES),
            "{refused}"
        );
        assert!(refused.completed.is_none() && refused.partial.is_none());
        assert_eq!(mapper.get_current_atlas().areas().count(), areas_before);
        assert_eq!(place_count(&mapper, source), before);

        let copied = mapper
            .relocate_areas(
                vec![source],
                MapDestination::loose(MapStorage::Session),
                RelocationMode::Copy,
            )
            .await
            .expect("a copy leaves the original whole");
        let copy = copied.destination_ids[0];
        assert_eq!(mapper.area_storage(&copy), MapStorage::Session);
        assert_eq!(place_count(&mapper, copy), 0);
        assert_eq!(place_count(&mapper, source), before);
        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn cross_tier_move_copies_content_before_removing_source() {
        let (mapper, root) = mapper("area-move").await;
        let source = mapper
            .create_area_at(
                "Old Roads".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create local source");
        wait(
            &mapper,
            mapper
                .upsert_room(
                    RoomKey::new(source, RoomNumber(7)),
                    RoomUpdates {
                        title: Some("Seven Stones".to_string()),
                        x: Some(3.0),
                        y: Some(-2.0),
                        ..RoomUpdates::default()
                    },
                )
                .expect("enqueue room"),
        )
        .await;

        let moved = mapper
            .relocate_areas(
                vec![source],
                MapDestination::loose(MapStorage::Cloud),
                RelocationMode::Move,
            )
            .await
            .expect("move to cloud");
        let destination = moved.destination_ids[0];
        assert_ne!(source, destination, "cross-tier moves mint fresh ids");
        assert_eq!(mapper.area_storage(&destination), MapStorage::Cloud);
        let atlas = mapper.get_current_atlas();
        assert!(
            atlas.get_area(&source).is_none(),
            "source deleted after commit"
        );
        let room = atlas
            .get_room(&RoomKey::new(destination, RoomNumber(7)))
            .expect("room copied");
        assert_eq!(room.get_title(), "Seven Stones");
        assert_eq!((room.get_x(), room.get_y()), (3.0, -2.0));

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn unsupported_cloud_cannot_fall_back_to_an_unguarded_delete() {
        let (mapper, root) = mapper("unsupported-local-move").await;
        let source = mapper
            .create_area_at("Original".into(), MapDestination::loose(MapStorage::Cloud))
            .await
            .unwrap();
        let failure = mapper
            .relocate_areas(
                vec![source],
                MapDestination::loose(MapStorage::Local),
                RelocationMode::Move,
            )
            .await
            .expect_err("a backend without guarded moves must refuse");
        assert!(failure.completed.is_none() && failure.partial.is_none());
        assert!(mapper.get_current_atlas().get_area(&source).is_some());
        assert_eq!(mapper.get_current_atlas().areas().count(), 1);
        std::fs::remove_dir_all(root).ok();
    }

    /// A local destination receives the full document in one atomic write,
    /// including linked exits, decorations, properties and tags, and remains
    /// editable after the move.
    #[tokio::test]
    async fn move_to_local_bulk_writes_the_full_document() {
        let (mapper, root) = mapper("bulk-local").await;
        let source = mapper
            .create_area_at(
                "Deep Halls".to_string(),
                MapDestination::loose(MapStorage::Session),
            )
            .await
            .expect("create session source");
        for (number, title, x) in [(1, "Gate", 0.0), (2, "Vault", 4.0)] {
            wait(
                &mapper,
                mapper
                    .upsert_room(
                        RoomKey::new(source, RoomNumber(number)),
                        RoomUpdates {
                            title: Some(title.to_string()),
                            x: Some(x),
                            y: Some(0.0),
                            ..RoomUpdates::default()
                        },
                    )
                    .expect("enqueue room"),
            )
            .await;
        }
        let (_, submission) = mapper
            .create_exit_tracked(
                RoomKey::new(source, RoomNumber(1)),
                ExitArgs {
                    from_direction: crate::ExitDirection::East,
                    to_area_id: Some(source),
                    to_room_number: Some(RoomNumber(2)),
                    to_direction: Some(crate::ExitDirection::West),
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            )
            .expect("create exit");
        wait(&mapper, submission).await;
        let (_, submission) = mapper
            .create_label_tracked(
                source,
                LabelArgs {
                    text: "Armory".to_string(),
                    width: 10.0,
                    height: 4.0,
                    color: "#ffffff".to_string(),
                    font_size: 12,
                    font_weight: 400,
                    ..LabelArgs::default()
                },
            )
            .expect("create label");
        wait(&mapper, submission).await;
        let (_, submission) = mapper
            .create_shape_tracked(
                source,
                ShapeArgs {
                    width: 8.0,
                    height: 8.0,
                    ..ShapeArgs::default()
                },
            )
            .expect("create shape");
        wait(&mapper, submission).await;
        wait(
            &mapper,
            mapper
                .set_area_property(source, "climate".to_string(), "damp".to_string())
                .expect("area property"),
        )
        .await;
        wait(
            &mapper,
            mapper
                .set_room_property(
                    RoomKey::new(source, RoomNumber(1)),
                    "terrain".to_string(),
                    "stone".to_string(),
                )
                .expect("room property"),
        )
        .await;
        wait(
            &mapper,
            mapper
                .add_room_tag(RoomKey::new(source, RoomNumber(2)), "vault".to_string())
                .expect("room tag"),
        )
        .await;

        let moved = mapper
            .relocate_areas(
                vec![source],
                MapDestination::loose(MapStorage::Local),
                RelocationMode::Move,
            )
            .await
            .expect("move to local");
        let destination = moved.destination_ids[0];
        assert_eq!(mapper.area_storage(&destination), MapStorage::Local);
        assert!(mapper.get_current_atlas().get_area(&source).is_none());

        let details = mapper
            .export_area(destination)
            .await
            .expect("read the persisted destination document");
        assert_eq!(details.rooms.len(), 2);
        let gate = details
            .rooms
            .iter()
            .find(|room| room.room_number == RoomNumber(1))
            .expect("room 1 copied");
        assert_eq!(gate.title, "Gate");
        assert_eq!(
            gate.properties
                .iter()
                .find(|property| property.name == "terrain")
                .map(|property| property.value.as_str()),
            Some("stone")
        );
        let exit = gate.exits.first().expect("exit copied");
        assert_eq!(
            exit.to_area_id,
            Some(destination),
            "in-set exit target remapped to the destination id"
        );
        assert_eq!(exit.to_room_number, Some(RoomNumber(2)));
        assert!(
            details
                .connections
                .iter()
                .any(|connection| connection.id == exit.connection_id),
            "the exit's connection travelled with it"
        );
        assert!(
            details
                .rooms
                .iter()
                .find(|room| room.room_number == RoomNumber(2))
                // Tags are stored normalized to uppercase.
                .is_some_and(|room| room.tags.contains("VAULT"))
        );
        assert_eq!(details.labels.len(), 1);
        assert_eq!(details.labels[0].text, "Armory");
        assert_eq!(details.shapes.len(), 1);
        assert_eq!(
            details
                .properties
                .iter()
                .find(|property| property.name == "climate")
                .map(|property| property.value.as_str()),
            Some("damp")
        );

        // The bulk write and the CAS pipeline agree on the revision: an
        // ordinary envelope edit lands on the populated destination.
        wait(
            &mapper,
            mapper
                .upsert_room(
                    RoomKey::new(destination, RoomNumber(9)),
                    RoomUpdates {
                        title: Some("Annex".to_string()),
                        ..RoomUpdates::default()
                    },
                )
                .expect("post-move edit accepted"),
        )
        .await;
        let after = mapper
            .export_area(destination)
            .await
            .expect("re-read destination");
        assert!(
            after
                .rooms
                .iter()
                .any(|room| room.room_number == RoomNumber(9)),
            "the envelope edit persisted on top of the bulk write"
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn atlas_copy_keeps_source_and_files_members_in_new_tier() {
        let (mapper, root) = mapper("atlas-copy").await;
        let source_atlas = mapper
            .create_atlas_at("Campaign".to_string(), MapStorage::Local)
            .await
            .expect("create atlas");
        let source_area = mapper
            .create_area_at(
                "Keep".to_string(),
                MapDestination::in_atlas(MapStorage::Local, source_atlas.id),
            )
            .await
            .expect("create member");

        let copied = mapper
            .relocate_atlas(source_atlas.id, MapStorage::Cloud, RelocationMode::Copy)
            .await
            .expect("copy atlas");
        assert_ne!(copied.destination_atlas_id, source_atlas.id);
        assert_eq!(copied.areas.source_ids, vec![source_area]);
        assert_eq!(copied.areas.destination_ids.len(), 1);
        assert_eq!(
            mapper.area_storage(&copied.areas.destination_ids[0]),
            MapStorage::Cloud
        );
        let atlas = mapper.get_current_atlas();
        assert!(
            atlas.get_area(&source_area).is_some(),
            "copy retains source"
        );
        assert_eq!(
            atlas
                .get_area(&copied.areas.destination_ids[0])
                .and_then(|area| area.meta().atlas_id),
            Some(copied.destination_atlas_id)
        );

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn move_fence_rejects_late_content_and_metadata_edits() {
        let (mapper, root) = mapper("move-fence").await;
        let source = mapper
            .create_area_at(
                "Frozen while moving".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create source");

        let fences = mapper.begin_area_move(&[source]).expect("begin move");
        mapper.wait_area_move_quiescent(&fences).await;
        assert!(matches!(
            mapper.upsert_room(RoomKey::new(source, RoomNumber(1)), RoomUpdates::default()),
            Err(CloudError::PendingOperations(_))
        ));
        assert!(matches!(
            mapper.rename_area(source, "Too late").await,
            Err(CloudError::PendingOperations(_))
        ));

        drop(fences);
        let submission = mapper
            .upsert_room(RoomKey::new(source, RoomNumber(1)), RoomUpdates::default())
            .expect("dropping an uncommitted move fence reopens edits");
        wait(&mapper, submission).await;

        std::fs::remove_dir_all(root).ok();
    }

    /// C1: the source delete of a cross-tier move is guarded by the revision
    /// the move snapshot stood on. A behind-cache client whose source moved
    /// on the backend refuses the delete and fails safe into the documented
    /// harmless-duplicate outcome — and the error carries the completed
    /// destination copy so callers can point at it instead of retrying.
    #[tokio::test]
    async fn move_commit_refuses_a_source_rev_it_never_saw() {
        let root = temp_root("rev-guard");
        let make_mapper = || {
            Mapper::new(
                Arc::new(CompositeBackend::new(
                    Arc::new(LocalBackend::new(root.join("local"))),
                    Arc::new(LocalBackend::new(root.join("cloud"))),
                )),
                root.join(format!("cache-{}", Uuid::new_v4())),
            )
        };
        let stale = make_mapper();
        stale.load_all_areas().await.expect("load stale mapper");
        let source = stale
            .create_area_at(
                "Contested".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create source");
        wait(
            &stale,
            stale
                .upsert_room(
                    RoomKey::new(source, RoomNumber(1)),
                    RoomUpdates {
                        title: Some("Seen by both".to_string()),
                        ..RoomUpdates::default()
                    },
                )
                .expect("enqueue room"),
        )
        .await;

        // Keep the observer disconnected so its copied revision remains stale.
        stale.pause_local_updates_for_test();

        // Another client edits the source after this client's cache was
        // built; the backend revision moves past the snapshot's.
        let other = make_mapper();
        other.load_all_areas().await.expect("load other mapper");
        wait(
            &other,
            other
                .upsert_room(
                    RoomKey::new(source, RoomNumber(2)),
                    RoomUpdates {
                        title: Some("Unseen edit".to_string()),
                        ..RoomUpdates::default()
                    },
                )
                .expect("enqueue unseen edit"),
        )
        .await;

        let failure = stale
            .relocate_areas(
                vec![source],
                MapDestination::loose(MapStorage::Cloud),
                RelocationMode::Move,
            )
            .await
            .expect_err("the stale move must refuse the delete");
        assert!(
            matches!(failure.error, CloudError::RevisionConflict { .. }),
            "refusal names the revision drift, got {:?}",
            failure.error
        );
        let completed = failure
            .completed
            .expect("the destination copy is complete and reported");
        assert_eq!(completed.destination_ids.len(), 1);
        let destination = completed.destination_ids[0];
        assert_eq!(stale.area_storage(&destination), MapStorage::Cloud);
        assert!(
            stale.get_current_atlas().get_area(&destination).is_some(),
            "the harmless duplicate exists at the destination"
        );

        // The unseen edit survives on the backend: a fresh cache sees the
        // source area with both rooms.
        let fresh = make_mapper();
        fresh.load_all_areas().await.expect("load fresh mapper");
        let atlas = fresh.get_current_atlas();
        assert!(atlas.get_area(&source).is_some(), "source survives");
        assert!(
            atlas
                .get_room(&RoomKey::new(source, RoomNumber(2)))
                .is_some(),
            "the edit the stale client never saw survives"
        );

        // The refused move dropped its fence: the source reopens for edits
        // (a fenced area would refuse the enqueue outright).
        let _submission = stale
            .upsert_room(RoomKey::new(source, RoomNumber(3)), RoomUpdates::default())
            .expect("source reopened after the refusal");

        std::fs::remove_dir_all(root).ok();
    }

    fn blank_document(area_id: AreaId, name: &str) -> AreaWithDetails {
        AreaWithDetails {
            room_data: Vec::new(),
            sources: Vec::new(),
            area: crate::Area {
                projection_token: None,
                id: area_id,
                user_id: None,
                atlas_id: None,
                atlas_name: None,
                name: name.to_string(),
                created_at: chrono::Utc::now(),
                rev: 1,
                access: None,
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
            properties: Vec::new(),
            rooms: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
            linked_areas: Vec::new(),
        }
    }

    fn plain_room(number: i32) -> crate::RoomWithDetails {
        crate::RoomWithDetails {
            room_number: RoomNumber(number),
            title: String::new(),
            description: String::new(),
            level: 0,
            x: 0.0,
            y: 0.0,
            color: String::new(),
            properties: Vec::new(),
            exits: Vec::new(),
            tags: std::collections::BTreeSet::default(),
            external_id: None,
        }
    }

    fn member_exit(
        connection_id: ConnectionId,
        from_direction: crate::ExitDirection,
        to: Option<(AreaId, i32)>,
    ) -> Exit {
        Exit {
            to_source: None,
            id: ExitId::new(),
            from_direction,
            to_area_id: to.map(|(area, _)| area),
            to_room_number: to.map(|(_, room)| RoomNumber(room)),
            to_direction: None,
            path: String::new(),
            is_hidden: false,
            door: None,
            weight: 1.0,
            command: String::new(),
            connection_id,
            to_unknown: false,
            to_area_token: None,
        }
    }

    fn plain_connection(
        id: ConnectionId,
        a_room: i32,
        b_room: Option<i32>,
        kind: ConnectionKind,
    ) -> crate::Connection {
        let endpoint = |room: i32, side: crate::RoomSide| crate::ConnectionEndpoint {
            source: None,
            room_number: RoomNumber(room),
            side,
            port_offset: 0.5,
            port_mode: crate::PortMode::AutoPinned,
        };
        crate::Connection {
            id,
            endpoint_a: endpoint(a_room, crate::RoomSide::East),
            endpoint_b: b_room.map(|room| endpoint(room, crate::RoomSide::West)),
            kind,
            routing: crate::ConnectionRouting::Simple,
            segment_shape: crate::SegmentShape::Direct,
            corner: crate::CornerStyle::Sharp,
            route_points: Vec::new(),
            dash: crate::ConnectionDash::Solid,
            color: crate::DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: crate::DEFAULT_CONNECTION_THICKNESS,
        }
    }

    /// A Secret as a copy of `map` sees it: its door on map room 1 into its
    /// own room 2, the connection between them, and `actions`.
    fn secret_bundle(map: AreaId, secret: Uuid, actions: &[&str]) -> SourceBundle {
        let own = SourceId::Secret(secret);
        let connection = ConnectionId::new();
        let mut door = member_exit(connection, crate::ExitDirection::East, Some((map, 2)));
        door.to_source = Some(own);
        let mut back = member_exit(connection, crate::ExitDirection::West, Some((map, 1)));
        back.is_hidden = true;
        let mut link = plain_connection(connection, 1, Some(2), ConnectionKind::Internal);
        if let Some(end) = link.endpoint_b.as_mut() {
            end.source = Some(own);
        }
        let mut vault = plain_room(2);
        vault.exits.push(back);
        serde_json::from_value(serde_json::json!({
            "source": own, "name": "Bookcase", "ownership": "members",
            "clan_id": Uuid::new_v4(), "rev": 7, "actions": actions,
        }))
        .map(|mut bundle: SourceBundle| {
            bundle.rooms.push(vault);
            bundle.room_data.push(crate::RoomData {
                room_source: None,
                room_number: RoomNumber(1),
                properties: Vec::new(),
                tags: std::collections::BTreeSet::default(),
                exits: vec![door],
            });
            bundle.connections.push(link);
            bundle
        })
        .expect("the Secret parses")
    }

    /// A copy carries of a map's Secrets only those the copier holds `copy`
    /// on, each as an owner Secret of the copy under a fresh id whose
    /// references to its own rooms follow it. Remote exits retain their own
    /// content and connection while destinations outside the copy dangle.
    #[test]
    fn a_copy_carries_only_copyable_secrets_as_owner_secrets_under_fresh_ids() {
        let map = AreaId(Uuid::new_v4());
        let elsewhere = (AreaId(Uuid::new_v4()), Uuid::new_v4());
        let (copyable, readable) = (Uuid::new_v4(), Uuid::new_v4());
        let mut document = blank_document(map, "Library");
        document.area.access = Some(crate::AreaAccess {
            is_owner: false,
            can_edit: true,
            can_reshare: false,
            can_copy: true,
            can_admin: false,
            include_secrets: false,
        });
        let foreign_link = ConnectionId::new();
        let mut foreign = member_exit(
            foreign_link,
            crate::ExitDirection::North,
            Some((elsewhere.0, 4)),
        );
        foreign.to_source = Some(SourceId::Secret(elsewhere.1));
        let mut room = plain_room(1);
        room.exits.push(foreign);
        document.rooms = vec![room, plain_room(2)];
        document.connections.push(plain_connection(
            foreign_link,
            1,
            None,
            ConnectionKind::External,
        ));
        document.sources = vec![
            secret_bundle(map, copyable, &["read", "copy"]),
            secret_bundle(map, readable, &["read", "add", "edit", "remove"]),
        ];

        keep_what_a_copy_carries(&mut document, true);
        let copy = AreaId(Uuid::new_v4());
        freshen_documents(
            std::slice::from_mut(&mut document),
            &HashMap::from([(map, copy)]),
            &FreshenOptions {
                stamp_local_owner: false,
            },
        );

        let exit = &document.rooms[0].exits[0];
        assert_eq!(exit.to_area_id, None);
        assert_eq!(exit.to_room_number, None);
        assert_eq!(exit.to_source, None);
        assert_eq!(document.connections.len(), 1);
        assert_eq!(document.connections[0].id, exit.connection_id);
        assert_eq!(document.connections[0].kind, ConnectionKind::Dangling);
        assert_eq!(
            document.sources.len(),
            1,
            "only the copyable Secret comes along"
        );
        let bundle = &document.sources[0];
        let SourceId::Secret(id) = bundle.source else {
            panic!("a Secret: {:?}", bundle.source);
        };
        assert!(id != copyable && id != readable, "a fresh id");
        assert_eq!(bundle.ownership.as_deref(), Some("owner"));
        assert_eq!(bundle.clan_id, None);
        assert_eq!(bundle.rev, 1);
        assert!(bundle.can("copy") && bundle.can("delete") && bundle.can("manage_access"));
        assert!(!bundle.can("manage_ownership"));
        let door = &bundle.room_data[0].exits[0];
        assert_eq!(door.to_area_id, Some(copy));
        assert_eq!(door.to_source, Some(bundle.source));
        let back = &bundle.rooms[0].exits[0];
        assert_eq!(back.to_area_id, Some(copy));
        assert_eq!(back.to_source, None);
        let link = &bundle.connections[0];
        assert_eq!(door.connection_id, link.id);
        assert_eq!(back.connection_id, link.id);
        assert_eq!(
            link.endpoint_b.as_ref().and_then(|end| end.source),
            Some(bundle.source)
        );
    }

    #[test]
    fn copying_maps_remaps_secret_references_and_dangles_targets_omitted_from_the_copy() {
        let map = AreaId(Uuid::new_v4());
        let target = AreaId(Uuid::new_v4());
        let (kept, omitted) = (Uuid::new_v4(), Uuid::new_v4());
        let mut document = blank_document(map, "Origin");
        let mut origin = plain_room(1);
        for source in [kept, omitted] {
            let id = ConnectionId::new();
            let mut exit = member_exit(id, crate::ExitDirection::East, Some((target, 2)));
            exit.to_source = Some(SourceId::Secret(source));
            exit.command = "enter".into();
            origin.exits.push(exit);
            document
                .connections
                .push(plain_connection(id, 1, None, ConnectionKind::External));
        }
        document.rooms.push(origin);
        let mut destination = blank_document(target, "Destination");
        destination.rooms.push(plain_room(2));
        destination
            .sources
            .push(secret_bundle(target, kept, &["read", "copy"]));
        let mut documents = vec![document, destination];
        let copied_origin = AreaId(Uuid::new_v4());
        let copied_target = AreaId(Uuid::new_v4());
        freshen_documents(
            &mut documents,
            &HashMap::from([(map, copied_origin), (target, copied_target)]),
            &FreshenOptions {
                stamp_local_owner: true,
            },
        );
        let copied_source = documents[1].sources[0].source;
        assert_ne!(copied_source, SourceId::Secret(kept));
        let exits = &documents[0].rooms[0].exits;
        assert_eq!(exits[0].to_area_id, Some(copied_target));
        assert_eq!(exits[0].to_source, Some(copied_source));
        assert_eq!(exits[0].to_room_number, Some(RoomNumber(2)));
        assert_eq!(exits[1].to_area_id, None);
        assert_eq!(exits[1].to_source, None);
        assert_eq!(exits[1].to_room_number, None);
        assert_eq!(exits[1].command, "enter");
        assert_eq!(documents[0].connections.len(), 2);
    }

    /// C6: the shared freshener remaps in-set cross-area targets across the
    /// whole document set, drops out-of-set targets, demotes their External
    /// Connections to Dangling exactly as a live edit would, and treats
    /// access per caller contract — preserved for relocation, stamped
    /// locally owned for import.
    #[test]
    fn freshener_remaps_in_set_links_and_demotes_the_rest() {
        let a = AreaId(Uuid::new_v4());
        let b = AreaId(Uuid::new_v4());
        let outside = AreaId(Uuid::new_v4());
        let build = || {
            let to_b = ConnectionId::new();
            let to_outside = ConnectionId::new();
            let mut doc_a = blank_document(a, "A");
            let mut room = plain_room(1);
            room.exits
                .push(member_exit(to_b, crate::ExitDirection::East, Some((b, 5))));
            room.exits.push(member_exit(
                to_outside,
                crate::ExitDirection::West,
                Some((outside, 9)),
            ));
            doc_a.rooms.push(room);
            doc_a
                .connections
                .push(plain_connection(to_b, 1, None, ConnectionKind::External));
            doc_a.connections.push(plain_connection(
                to_outside,
                1,
                None,
                ConnectionKind::External,
            ));
            let mut doc_b = blank_document(b, "B");
            doc_b.rooms.push(plain_room(5));
            vec![doc_a, doc_b]
        };
        let id_map: HashMap<AreaId, AreaId> =
            [(a, AreaId(Uuid::new_v4())), (b, AreaId(Uuid::new_v4()))]
                .into_iter()
                .collect();

        let mut preserved = build();
        freshen_documents(
            &mut preserved,
            &id_map,
            &FreshenOptions {
                stamp_local_owner: false,
            },
        );
        assert_eq!(preserved[0].area.id, id_map[&a]);
        assert_eq!(preserved[1].area.id, id_map[&b]);
        let room = &preserved[0].rooms[0];
        let in_set = &room.exits[0];
        assert_eq!(
            in_set.to_area_id,
            Some(id_map[&b]),
            "in-set targets remap to the destination sibling"
        );
        assert_eq!(in_set.to_room_number, Some(RoomNumber(5)));
        let kept_external = preserved[0]
            .connections
            .iter()
            .find(|connection| connection.id == in_set.connection_id)
            .expect("in-set connection survives");
        assert_eq!(
            kept_external.kind,
            ConnectionKind::External,
            "a remapped link still leaves its area"
        );
        let dangled = &room.exits[1];
        assert_eq!(dangled.to_area_id, None, "out-of-set targets are dropped");
        assert_eq!(dangled.to_room_number, None);
        let demoted = preserved[0]
            .connections
            .iter()
            .find(|connection| connection.id == dangled.connection_id)
            .expect("demoted connection survives");
        assert_eq!(demoted.kind, ConnectionKind::Dangling);
        assert_eq!(demoted.endpoint_b, None);
        assert!(preserved[0].area.access.is_none(), "access left untouched");

        let mut scrubbed = build();
        freshen_documents(
            &mut scrubbed,
            &id_map,
            &FreshenOptions {
                stamp_local_owner: true,
            },
        );
        assert_eq!(
            scrubbed[0].area.access,
            Some(crate::AreaAccess::OWNER),
            "import stamps locally-owned access"
        );
    }

    /// A copy of a clan's map, imported or relocated, is in no clan: it
    /// keeps neither the clan, the caller's actions there, nor its ownership.
    #[test]
    fn freshened_copies_of_a_clans_map_are_in_no_clan() {
        let source = AreaId(Uuid::new_v4());
        let copy = AreaId(Uuid::new_v4());
        let clan = Uuid::new_v4();
        for stamp_local_owner in [false, true] {
            let mut document = blank_document(source, "Solace");
            document.area.clan_id = Some(clan);
            document.area.clan_name = Some("Lantern Company".to_string());
            document.area.actions = Some(["area.read".to_string(), "area.copy".to_string()].into());
            document.area.clan_ownership = crate::clan_maps::ClanOwnership {
                ownership: Some(crate::clan_maps::MapOwnership::Members),
                owned_by_me: true,
                frozen: false,
            };
            let mut documents = vec![document];
            freshen_documents(
                &mut documents,
                &HashMap::from([(source, copy)]),
                &FreshenOptions { stamp_local_owner },
            );
            let area = &documents[0].area;
            assert_eq!(area.clan_id, None);
            assert_eq!(area.clan_name, None);
            assert_eq!(area.actions, None);
            assert_eq!(
                area.clan_ownership,
                crate::clan_maps::ClanOwnership::default()
            );
        }
    }

    /// C6: connection groups (one CreateConnection plus its member exits)
    /// never straddle a 256-operation envelope boundary — a connection
    /// without members is structurally invalid at any boundary the server
    /// could observe.
    #[test]
    fn connection_groups_never_split_across_envelopes() {
        let area_id = AreaId(Uuid::new_v4());
        let mut document = blank_document(area_id, "Chunked");
        // 100 paired connections at 3 operations per group: 300 operations,
        // which cannot pack evenly into 256-op envelopes.
        for index in 0..100 {
            let connection_id = ConnectionId::new();
            let a_room = index * 2 + 1;
            let b_room = index * 2 + 2;
            let mut room_a = plain_room(a_room);
            room_a.exits.push(member_exit(
                connection_id,
                crate::ExitDirection::East,
                Some((area_id, b_room)),
            ));
            let mut room_b = plain_room(b_room);
            room_b.exits.push(member_exit(
                connection_id,
                crate::ExitDirection::West,
                Some((area_id, a_room)),
            ));
            document.rooms.push(room_a);
            document.rooms.push(room_b);
            document.connections.push(plain_connection(
                connection_id,
                a_room,
                Some(b_room),
                ConnectionKind::Internal,
            ));
        }

        let batches = connection_batches(&document);
        assert!(batches.len() > 1, "the set must overflow one envelope");
        let mut total_ops = 0;
        for batch in &batches {
            let operations = batch.operations();
            assert!(operations.len() <= MAX_MUTATION_OPERATIONS);
            total_ops += operations.len();
            let mut created: HashSet<ConnectionId> = HashSet::new();
            for operation in operations {
                match operation {
                    AreaMutation::CreateConnection { body } => {
                        created.insert(body.id);
                    }
                    AreaMutation::CreateExit { body, .. } => {
                        let member_of = body
                            .connection_id
                            .expect("copied exits carry explicit membership");
                        assert!(
                            created.contains(&member_of),
                            "an exit landed in a different envelope than its connection"
                        );
                    }
                    other => panic!("unexpected operation in a connection batch: {other:?}"),
                }
            }
        }
        assert_eq!(total_ops, 300, "every operation lands exactly once");
    }

    /// C4: relocation destinations are visibly marked while in flight and
    /// shed the marker on completion, so a crash mid-copy strands
    /// reviewable debris rather than an indistinguishable twin. The
    /// reconcile surface lists exactly the still-marked areas.
    #[tokio::test]
    async fn in_flight_marker_is_shed_on_completion_and_surfaced_when_stranded() {
        let (mapper, root) = mapper("marker").await;
        let source = mapper
            .create_area_at(
                "Catacombs".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create source");
        wait(
            &mapper,
            mapper
                .upsert_room(RoomKey::new(source, RoomNumber(1)), RoomUpdates::default())
                .expect("enqueue room"),
        )
        .await;

        let copied = mapper
            .relocate_areas(
                vec![source],
                MapDestination::loose(MapStorage::Cloud),
                RelocationMode::Copy,
            )
            .await
            .expect("copy");
        let atlas = mapper.get_current_atlas();
        let destination_name = atlas
            .get_area(&copied.destination_ids[0])
            .expect("destination present")
            .get_name()
            .to_string();
        assert_eq!(
            destination_name, "Catacombs",
            "a finished destination bears the final name, not the marker"
        );
        assert!(
            mapper.abandoned_relocation_areas().is_empty(),
            "a completed relocation leaves no marked debris"
        );

        // A destination whose populate never finished keeps the marker —
        // exactly what a crash mid-copy strands — and the reconcile
        // surface reports it.
        let stranded = mapper
            .create_area_at(
                format!("Catacombs{RELOCATION_IN_PROGRESS_SUFFIX}"),
                MapDestination::loose(MapStorage::Cloud),
            )
            .await
            .expect("create stranded twin");
        let abandoned = mapper.abandoned_relocation_areas();
        assert_eq!(abandoned.len(), 1);
        assert_eq!(abandoned[0].0, stranded);
        assert!(abandoned[0].1.ends_with(RELOCATION_IN_PROGRESS_SUFFIX));

        std::fs::remove_dir_all(root).ok();
    }

    /// C5: a mixed-tier move copies only the cross-tier members. Same-tier
    /// members keep their ids (bookmarks, scripts, and outside links stay
    /// valid), and links between the two groups survive in both
    /// directions: the copied member's exit remaps onto the kept member's
    /// unchanged id, and the kept member's exit is retargeted onto the
    /// copied member's fresh id.
    #[tokio::test]
    async fn mixed_tier_move_keeps_same_tier_ids_and_relinks_the_set() {
        let (mapper, root) = mapper("mixed-move").await;
        let local_member = mapper
            .create_area_at(
                "Sewers".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create local member");
        let cloud_member = mapper
            .create_area_at(
                "Spires".to_string(),
                MapDestination::loose(MapStorage::Cloud),
            )
            .await
            .expect("create cloud member");
        for (area, number) in [(local_member, 1), (cloud_member, 2)] {
            wait(
                &mapper,
                mapper
                    .upsert_room(
                        RoomKey::new(area, RoomNumber(number)),
                        RoomUpdates::default(),
                    )
                    .expect("enqueue room"),
            )
            .await;
        }
        // A link in each direction across the tier boundary.
        let (_, submission) = mapper
            .create_exit_tracked(
                RoomKey::new(local_member, RoomNumber(1)),
                ExitArgs {
                    from_direction: crate::ExitDirection::East,
                    to_area_id: Some(cloud_member),
                    to_room_number: Some(RoomNumber(2)),
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            )
            .expect("link local to cloud");
        wait(&mapper, submission).await;
        let (_, submission) = mapper
            .create_exit_tracked(
                RoomKey::new(cloud_member, RoomNumber(2)),
                ExitArgs {
                    from_direction: crate::ExitDirection::West,
                    to_area_id: Some(local_member),
                    to_room_number: Some(RoomNumber(1)),
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            )
            .expect("link cloud to local");
        wait(&mapper, submission).await;

        let moved = mapper
            .relocate_areas(
                vec![local_member, cloud_member],
                MapDestination::loose(MapStorage::Cloud),
                RelocationMode::Move,
            )
            .await
            .expect("mixed-tier move");
        assert_eq!(
            moved.destination_ids[1], cloud_member,
            "the same-tier member keeps its id"
        );
        let local_copy = moved.destination_ids[0];
        assert_ne!(
            local_copy, local_member,
            "cross-tier members mint fresh ids"
        );
        assert_eq!(mapper.area_storage(&local_copy), MapStorage::Cloud);

        let atlas = mapper.get_current_atlas();
        assert!(
            atlas.get_area(&local_member).is_none(),
            "only the cross-tier source is deleted"
        );
        assert!(atlas.get_area(&cloud_member).is_some());

        let copied_exit = atlas
            .get_room(&RoomKey::new(local_copy, RoomNumber(1)))
            .expect("copied room")
            .to_details()
            .exits
            .first()
            .cloned()
            .expect("copied exit");
        assert_eq!(
            copied_exit.to_area_id,
            Some(cloud_member),
            "the copied member's link lands on the kept member's unchanged id"
        );
        let kept_exit = atlas
            .get_room(&RoomKey::new(cloud_member, RoomNumber(2)))
            .expect("kept room")
            .to_details()
            .exits
            .first()
            .cloned()
            .expect("kept exit");
        assert_eq!(
            kept_exit.to_area_id,
            Some(local_copy),
            "the kept member's link is retargeted onto the fresh id"
        );
        assert_eq!(kept_exit.to_room_number, Some(RoomNumber(1)));

        std::fs::remove_dir_all(root).ok();
    }

    /// C6: destination cleanup after a failed relocation deletes every
    /// partially created area, newest first.
    #[tokio::test]
    async fn cleanup_deletes_partially_created_destinations() {
        let (mapper, root) = mapper("cleanup").await;
        let first = mapper
            .create_area_at(
                "Half copied".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create first");
        let second = mapper
            .create_area_at(
                "Never populated".to_string(),
                MapDestination::loose(MapStorage::Cloud),
            )
            .await
            .expect("create second");

        assert!(cleanup_areas(&mapper, &[first, second]).await.is_empty());
        let atlas = mapper.get_current_atlas();
        assert!(atlas.get_area(&first).is_none());
        assert!(atlas.get_area(&second).is_none());

        std::fs::remove_dir_all(root).ok();
    }

    /// C6: copying a set with cross-area links between members keeps those
    /// links, remapped onto the destination siblings, while the sources
    /// stay linked to each other.
    #[tokio::test]
    async fn copy_set_remaps_cross_area_links_between_members() {
        let (mapper, root) = mapper("cross-remap").await;
        let a = mapper
            .create_area_at(
                "Docks".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create A");
        let b = mapper
            .create_area_at(
                "Warrens".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create B");
        for (area, number) in [(a, 1), (b, 2)] {
            wait(
                &mapper,
                mapper
                    .upsert_room(
                        RoomKey::new(area, RoomNumber(number)),
                        RoomUpdates::default(),
                    )
                    .expect("enqueue room"),
            )
            .await;
        }
        let (_, submission) = mapper
            .create_exit_tracked(
                RoomKey::new(a, RoomNumber(1)),
                ExitArgs {
                    from_direction: crate::ExitDirection::East,
                    to_area_id: Some(b),
                    to_room_number: Some(RoomNumber(2)),
                    weight: 1.0,
                    ..ExitArgs::default()
                },
            )
            .expect("create cross-area exit");
        wait(&mapper, submission).await;

        let copied = mapper
            .relocate_areas(
                vec![a, b],
                MapDestination::loose(MapStorage::Cloud),
                RelocationMode::Copy,
            )
            .await
            .expect("copy the linked set");
        let a_copy = copied.destination_ids[0];
        let b_copy = copied.destination_ids[1];

        let atlas = mapper.get_current_atlas();
        let copied_exit = atlas
            .get_room(&RoomKey::new(a_copy, RoomNumber(1)))
            .expect("copied room")
            .to_details()
            .exits
            .first()
            .cloned()
            .expect("copied exit");
        assert_eq!(
            copied_exit.to_area_id,
            Some(b_copy),
            "the in-set link re-anchors onto the copied sibling"
        );
        assert_eq!(copied_exit.to_room_number, Some(RoomNumber(2)));

        let source_exit = atlas
            .get_room(&RoomKey::new(a, RoomNumber(1)))
            .expect("source room survives a copy")
            .to_details()
            .exits
            .first()
            .cloned()
            .expect("source exit");
        assert_eq!(
            source_exit.to_area_id,
            Some(b),
            "the source set keeps its own linkage"
        );

        std::fs::remove_dir_all(root).ok();
    }

    /// The server-side cloud clone applies to every fully acknowledged
    /// cloud→cloud copy whose links lead into no other member of the set;
    /// every other combination takes the freshen-and-replay path.
    #[test]
    fn server_copy_gate_takes_every_acknowledged_cloud_copy_without_in_set_links() {
        let area_id = AreaId(Uuid::new_v4());
        let snapshot = |target: AreaId, to_unknown: bool| {
            let connection_id = ConnectionId::new();
            let exit = Exit {
                to_source: None,
                id: ExitId::new(),
                from_direction: crate::ExitDirection::North,
                to_area_id: Some(target),
                to_room_number: Some(RoomNumber(2)),
                to_direction: None,
                path: String::new(),
                is_hidden: false,
                door: None,
                weight: 1.0,
                command: String::new(),
                connection_id,
                to_unknown,
                to_area_token: None,
            };
            AreaWithDetails {
                room_data: Vec::new(),
                sources: Vec::new(),
                area: crate::Area {
                    projection_token: None,
                    id: area_id,
                    user_id: None,
                    atlas_id: None,
                    atlas_name: None,
                    name: "Gated".to_string(),
                    created_at: chrono::Utc::now(),
                    rev: 4,
                    access: None,
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
                properties: Vec::new(),
                rooms: vec![crate::RoomWithDetails {
                    room_number: RoomNumber(1),
                    title: String::new(),
                    description: String::new(),
                    level: 0,
                    x: 0.0,
                    y: 0.0,
                    color: String::new(),
                    properties: Vec::new(),
                    exits: vec![exit],
                    tags: std::collections::BTreeSet::default(),
                    external_id: None,
                }],
                labels: Vec::new(),
                shapes: Vec::new(),
                connections: Vec::new(),
                linked_areas: Vec::new(),
            }
        };

        let sibling = AreaId(Uuid::new_v4());
        let outside = AreaId(Uuid::new_v4());
        let members: HashSet<AreaId> = [area_id, sibling].into_iter().collect();
        let gate = |snapshot: &AreaWithDetails,
                    from: MapStorage,
                    to: MapStorage,
                    mode: RelocationMode,
                    acknowledged: bool| {
            server_copy_applies(snapshot, &members, from, to, mode, acknowledged)
        };
        let (cloud, local) = (MapStorage::Cloud, MapStorage::Local);
        let copy = RelocationMode::Copy;

        let eligible = snapshot(area_id, false);
        assert!(gate(&eligible, cloud, cloud, copy, true));
        assert!(
            gate(&snapshot(outside, false), cloud, cloud, copy, true),
            "a link out of the set follows the service's copy rule"
        );
        assert!(
            gate(&snapshot(outside, true), cloud, cloud, copy, true),
            "a link into a map the copier can't read dangles in the service's copy"
        );

        // Any single condition failing forces the replay path.
        assert!(
            !gate(&eligible, cloud, cloud, RelocationMode::Move, true),
            "moves never take the server clone"
        );
        assert!(
            !gate(&eligible, local, cloud, copy, true),
            "only a cloud source has a server-side copy"
        );
        assert!(
            !gate(&eligible, cloud, local, copy, true),
            "a cross-tier destination needs the freshen contract"
        );
        assert!(
            !gate(&eligible, cloud, cloud, copy, false),
            "queued unacknowledged edits would be missing from a server clone"
        );
        assert!(
            !gate(&snapshot(sibling, false), cloud, cloud, copy, true),
            "a link into another member leads into its copy only through the replay"
        );
        let mut place_into_sibling = eligible.clone();
        place_into_sibling.sources =
            vec![secret_bundle(area_id, Uuid::new_v4(), &["read", "copy"])];
        place_into_sibling.sources[0].room_data[0].exits[0].to_area_id = Some(sibling);
        place_into_sibling.sources[0].room_data[0].exits[0].to_source = None;
        assert!(
            !gate(&place_into_sibling, cloud, cloud, copy, true),
            "a place's link into another member counts as the map's own"
        );
    }

    #[tokio::test]
    async fn relocation_rejects_duplicate_source_ids() {
        let (mapper, root) = mapper("duplicate-source").await;
        let source = mapper
            .create_area_at(
                "Only once".to_string(),
                MapDestination::loose(MapStorage::Local),
            )
            .await
            .expect("create source");

        let result = mapper
            .relocate_areas(
                vec![source, source],
                MapDestination::loose(MapStorage::Cloud),
                RelocationMode::Copy,
            )
            .await;
        assert!(matches!(
            result,
            Err(RelocationError {
                error: CloudError::InvalidInput(_),
                completed: None,
                partial: None,
            })
        ));

        std::fs::remove_dir_all(root).ok();
    }

    #[tokio::test]
    async fn atlas_relocation_refuses_an_incomplete_cache() {
        let root = temp_root("atlas-incomplete");
        let make_mapper = || {
            Mapper::new(
                Arc::new(CompositeBackend::new(
                    Arc::new(LocalBackend::new(root.join("local"))),
                    Arc::new(LocalBackend::new(root.join("cloud"))),
                )),
                root.join(format!("cache-{}", Uuid::new_v4())),
            )
        };
        let first = make_mapper();
        first.load_all_areas().await.expect("load first mapper");
        let atlas = first
            .create_atlas_at("Changing folder".to_string(), MapStorage::Local)
            .await
            .expect("create atlas");
        first
            .create_area_at(
                "Loaded member".to_string(),
                MapDestination::in_atlas(MapStorage::Local, atlas.id),
            )
            .await
            .expect("create loaded member");

        // Deliberately keep this session behind while another adds a member.
        // Live same-process adoption would otherwise race the refusal check.
        first.pause_local_updates_for_test();
        let second = make_mapper();
        second.load_all_areas().await.expect("load second mapper");
        second
            .create_area_at(
                "Late member".to_string(),
                MapDestination::in_atlas(MapStorage::Local, atlas.id),
            )
            .await
            .expect("create late member");

        let result = first
            .relocate_atlas(atlas.id, MapStorage::Cloud, RelocationMode::Move)
            .await;
        assert!(matches!(
            result,
            Err(RelocationError {
                error: CloudError::PendingOperations(_),
                completed: None,
                partial: None,
            })
        ));
        assert!(
            first
                .list_atlases()
                .await
                .expect("list source atlases")
                .iter()
                .any(|item| item.id == atlas.id),
            "refusal must leave the source atlas intact"
        );

        std::fs::remove_dir_all(root).ok();
    }
}
