//! High-level map copy/move operations across session, local, and cloud
//! storage.
//!
//! A storage change is not an atlas metadata update. It is a recoverable
//! copy-then-delete transaction: create all destination headers, copy all
//! rooms, remap in-set cross-area links, copy the remaining content, wait for
//! backend acknowledgement, and only then delete the sources for a move.
//! Failures before commit clean up destination objects best-effort; failures
//! while deleting a source leave a complete destination copy and a harmless
//! duplicate rather than losing data.

use std::collections::{HashMap, HashSet};

use log::warn;

use crate::{
    AreaId, AreaWithDetails, AtlasId, CloudError, CloudResult, ConnectionArgs, ConnectionId,
    ConnectionKind, Exit, ExitArgs, ExitId, LabelArgs, LabelId, MapDestination, MapStorage, Mapper,
    RoomNumber, RoomUpdates, ShapeArgs, ShapeId,
    mapper::{AreaMutationBatch, MutationSubmission, validate_import_document},
    mutation::{AreaMutation, MAX_MUTATION_OPERATIONS},
};

/// Whether the source survives a relocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelocationMode {
    Copy,
    Move,
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
#[derive(Debug)]
pub struct RelocationError<T> {
    pub error: CloudError,
    /// The fully created destination, or `None` when the failure preceded
    /// destination completion (partially created objects are cleaned up
    /// best-effort and a retry is safe).
    pub completed: Option<T>,
}

impl<T> From<CloudError> for RelocationError<T> {
    fn from(error: CloudError) -> Self {
        Self {
            error,
            completed: None,
        }
    }
}

impl<T> std::fmt::Display for RelocationError<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.completed.is_some() {
            write!(
                f,
                "{}. The copy at the destination is complete; the source was left in place — remove it there instead of retrying the move",
                self.error
            )
        } else {
            self.error.fmt(f)
        }
    }
}

impl<T: std::fmt::Debug> std::error::Error for RelocationError<T> {}

impl Mapper {
    /// Copy or move a set of areas to one explicit storage/folder destination.
    /// Cross-area exits whose targets are also in `source_ids` are remapped to
    /// the corresponding destination areas. For copied members, links leaving
    /// the set become dangling, matching portable import semantics; a moved
    /// member already in the destination tier keeps its id (and therefore
    /// its outside links) and is merely re-filed.
    pub async fn relocate_areas(
        &self,
        source_ids: Vec<AreaId>,
        destination: MapDestination,
        mode: RelocationMode,
    ) -> Result<MapRelocation, RelocationError<MapRelocation>> {
        if destination.storage == MapStorage::Session && destination.atlas_id.is_some() {
            return Err(CloudError::InvalidInput(
                "session maps cannot be filed into atlases".to_string(),
            )
            .into());
        }
        if let Some(atlas_id) = destination.atlas_id
            && self.atlas_storage(&atlas_id) != destination.storage
        {
            return Err(CloudError::InvalidInput(
                "the destination atlas belongs to a different storage tier".to_string(),
            )
            .into());
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
            for source_id in &source_ids {
                self.move_area_to_atlas(*source_id, destination.atlas_id)
                    .await?;
            }
            return Ok(MapRelocation {
                source_ids: source_ids.clone(),
                destination_ids: source_ids,
                destination,
            });
        }

        // Only members whose sources get deleted need the move fence; kept
        // members stay editable throughout and are merely re-filed and
        // relinked at the end.
        let mut move_fences = if mode == RelocationMode::Move {
            let fences = self.begin_area_move(&copied_ids)?;
            self.wait_area_move_quiescent(&fences).await;
            Some(fences)
        } else {
            None
        };

        let snapshots = self.snapshot_areas(&copied_ids)?;
        for snapshot in &snapshots {
            validate_import_document(snapshot)?;
        }
        // The backend revision each move snapshot stands on: the last
        // acknowledged revision when one is known, else the cached document
        // revision (queued-but-unsent optimistic bumps ride the copy and are
        // discarded with the source, so they must not inflate the guard).
        let expected_revs: Vec<i64> = copied_ids
            .iter()
            .zip(&snapshots)
            .map(|(id, snapshot)| self.confirmed_area_rev(*id).unwrap_or(snapshot.area.rev))
            .collect();
        let mut copy_destination_ids = Vec::with_capacity(snapshots.len());
        let mut server_copied = vec![false; snapshots.len()];
        for (index, snapshot) in snapshots.iter().enumerate() {
            let source_id = snapshot.area.id;
            let copied = if server_copy_applies(
                snapshot,
                self.area_storage(&source_id),
                destination.storage,
                mode,
                self.confirmed_area_rev(source_id),
            ) {
                match self
                    .copy_cloud_area(source_id, &snapshot.area.name, destination.atlas_id)
                    .await
                {
                    Ok(copied) => copied,
                    Err(error) => {
                        cleanup_areas(self, &copy_destination_ids).await;
                        return Err(error.into());
                    }
                }
            } else {
                None
            };
            if let Some(area) = copied {
                copy_destination_ids.push(area.id);
                server_copied[index] = true;
                if let Err(error) = self.adopt_cloud_copy(area.id).await {
                    cleanup_areas(self, &copy_destination_ids).await;
                    return Err(error.into());
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
                    cleanup_areas(self, &copy_destination_ids).await;
                    return Err(error.into());
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
        // because the server clone preserves room numbers. Secrecy markings
        // ride the copy untouched — the snapshot already is the viewer's
        // projection.
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
                scrub_secrets: false,
            },
        );

        if let Err(error) = self.populate_documents(&documents).await {
            cleanup_areas(self, &copy_destination_ids).await;
            return Err(error.into());
        }

        // From here every destination copy is complete: later failures carry
        // the completed result so callers point at the existing copies
        // instead of retrying into duplicates.
        let completed = || MapRelocation {
            source_ids: source_ids.clone(),
            destination_ids: destination_ids.clone(),
            destination,
        };

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
                    completed: Some(completed()),
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
                completed: Some(completed()),
            });
        }
        for kept_id in &kept_ids {
            if let Err(error) = self
                .move_area_to_atlas(*kept_id, destination.atlas_id)
                .await
            {
                return Err(RelocationError {
                    error,
                    completed: Some(completed()),
                });
            }
        }

        if mode == RelocationMode::Move {
            // Destination content is fully acknowledged before the first
            // source delete. A delete failure (including the rev-drift
            // refusal) leaves complete copies on both sides — recoverable and
            // never data loss — so the error carries the completed result:
            // the remedy is pointing at the existing copy, not a retry that
            // would mint another one. Fences not yet committed are dropped
            // here, which reopens their sources for editing.
            let fences = move_fences.take().expect("move mode creates source fences");
            for (fence, expected_rev) in fences.into_iter().zip(expected_revs) {
                if let Err(error) = self.commit_area_move(fence, Some(expected_rev)).await {
                    return Err(RelocationError {
                        error,
                        completed: Some(completed()),
                    });
                }
            }
        }
        drop(completed);

        Ok(MapRelocation {
            source_ids,
            destination_ids,
            destination,
        })
    }

    /// Copy or move one whole atlas. Refuses to start unless the cache holds
    /// every member reported by the authoritative inventory, so a failed area
    /// load cannot silently turn into a partial atlas move.
    pub async fn relocate_atlas(
        &self,
        source_atlas_id: AtlasId,
        destination_storage: MapStorage,
        mode: RelocationMode,
    ) -> Result<AtlasRelocation, RelocationError<AtlasRelocation>> {
        if destination_storage == MapStorage::Session {
            return Err(CloudError::InvalidInput(
                "session storage does not support atlases".to_string(),
            )
            .into());
        }
        if mode == RelocationMode::Move
            && self.atlas_storage(&source_atlas_id) == destination_storage
        {
            return Err(CloudError::InvalidInput(
                "the atlas is already in that storage tier".to_string(),
            )
            .into());
        }

        let source = self
            .list_atlases()
            .await?
            .into_iter()
            .find(|atlas| atlas.id == source_atlas_id)
            .ok_or_else(|| CloudError::InvalidInput("atlas not found".to_string()))?;
        if !source.is_owner {
            return Err(CloudError::InvalidInput(
                "a shared atlas cannot be copied or moved".to_string(),
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

        let mut move_fences = if mode == RelocationMode::Move {
            let fences = self.begin_area_move(&member_ids)?;
            self.wait_area_move_quiescent(&fences).await;
            Some(fences)
        } else {
            None
        };

        // The backend revision each member's copy stands on, captured after
        // quiescence and before the copy (see `relocate_areas`).
        let member_cache = self.get_current_atlas();
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
            .relocate_areas(member_ids, destination, RelocationMode::Copy)
            .await
        {
            Ok(areas) => areas,
            Err(failure) => {
                if let Err(cleanup_error) = self.delete_atlas(destination_atlas.id).await {
                    warn!(
                        "failed to clean up destination atlas {} after relocation error: {cleanup_error}",
                        destination_atlas.id
                    );
                }
                // Copy mode never reaches the source-delete phase, so no
                // completed destination survives the cleanup above.
                return Err(failure.error.into());
            }
        };

        if mode == RelocationMode::Move {
            let completed = || AtlasRelocation {
                source_atlas_id,
                destination_atlas_id: destination_atlas.id,
                destination_atlas_name: destination_atlas_name.clone(),
                areas: areas.clone(),
            };
            let fences = move_fences
                .take()
                .expect("atlas move mode creates source fences");
            for (fence, expected_rev) in fences.into_iter().zip(expected_revs) {
                if let Err(error) = self.commit_area_move(fence, Some(expected_rev)).await {
                    return Err(RelocationError {
                        error,
                        completed: Some(completed()),
                    });
                }
            }
            if let Err(error) = self.delete_atlas(source_atlas_id).await {
                return Err(RelocationError {
                    error,
                    completed: Some(completed()),
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
                        room_number: room.room_number,
                        body: RoomUpdates {
                            title: Some(room.title.clone()),
                            description: Some(room.description.clone()),
                            level: Some(room.level),
                            x: Some(room.x),
                            y: Some(room.y),
                            color: Some(room.color.clone()),
                            is_secret: Some(room.is_secret),
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
                            is_secret: Some(property.is_secret),
                        }
                    });
                    let room_props = document.rooms.iter().flat_map(|room| {
                        let properties = room.properties.iter().map(|property| {
                            AreaMutation::UpsertRoomProperty {
                                room_number: room.room_number,
                                name: property.name.clone(),
                                value: property.value.clone(),
                                is_secret: Some(property.is_secret),
                            }
                        });
                        let tags = room.tags.iter().map(|tag| AreaMutation::AddRoomTag {
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
                        body: LabelArgs {
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
                            is_secret: Some(label.is_secret),
                        },
                    });
                let shapes = document
                    .shapes
                    .iter()
                    .map(|shape| AreaMutation::CreateShape {
                        body: ShapeArgs {
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
                            is_secret: Some(shape.is_secret),
                        },
                    });
                chunk_ops(
                    document.area.id,
                    labels.chain(shapes),
                    "Copy map decorations",
                )
            })
            .collect();
        self.stage_and_wait(decoration_batches).await
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
/// (`POST /areas/{id}/copy`) instead of freshen-and-replay. The gate is
/// deliberately narrow because the server clone's semantics diverge from
/// the freshen contract outside it:
///
/// - the server preserves live outbound cross-area links to visible areas,
///   where relocation demotes every link leaving the set to dangling — so
///   only an area with **no cross-area exits at all** (in-set links from
///   siblings are inbound and unaffected) is eligible;
/// - the server copies its own state, where relocation copies the local
///   optimistic snapshot — so eligibility requires the backend-acknowledged
///   revision to match the snapshot (no queued edits the server has not
///   seen);
/// - both paths mint fresh area/connection/exit identities and preserve
///   room numbers, so in-set inbound remaps hold either way.
///
/// The clone additionally records `copied_from` provenance, which the
/// replay path clears; accepted, since provenance is owner-only metadata
/// and truthful for a copy.
fn server_copy_applies(
    snapshot: &AreaWithDetails,
    source_storage: MapStorage,
    destination_storage: MapStorage,
    mode: RelocationMode,
    confirmed_rev: Option<i64>,
) -> bool {
    mode == RelocationMode::Copy
        && source_storage == MapStorage::Cloud
        && destination_storage == MapStorage::Cloud
        && confirmed_rev == Some(snapshot.area.rev)
        && snapshot
            .rooms
            .iter()
            .flat_map(|room| &room.exits)
            .all(|exit| {
                !exit.to_unknown
                    && exit
                        .to_area_id
                        .is_none_or(|target| target == snapshot.area.id)
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
    // One pass over every exit builds the member index; scanning every
    // room's exits per connection would be quadratic in area size.
    let mut members: HashMap<ConnectionId, Vec<(RoomNumber, &Exit)>> =
        HashMap::with_capacity(document.connections.len());
    for room in &document.rooms {
        for exit in &room.exits {
            members
                .entry(exit.connection_id)
                .or_default()
                .push((room.room_number, exit));
        }
    }

    let mut batches = Vec::new();
    let mut current = Vec::with_capacity(MAX_MUTATION_OPERATIONS);
    for connection in &document.connections {
        let mut group = vec![AreaMutation::CreateConnection {
            body: ConnectionArgs::from(connection),
        }];
        group.extend(
            members
                .get(&connection.id)
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .map(|&(room_number, exit)| AreaMutation::CreateExit {
                    room_number,
                    body: ExitArgs {
                        id: Some(exit.id),
                        connection_id: Some(exit.connection_id),
                        new_connection_id: None,
                        from_direction: exit.from_direction,
                        to_area_id: exit.to_area_id,
                        to_room_number: exit.to_room_number,
                        to_direction: exit.to_direction,
                        path: Some(exit.path.clone()),
                        is_hidden: exit.is_hidden,
                        is_closed: exit.is_closed,
                        is_locked: exit.is_locked,
                        weight: exit.weight,
                        command: Some(exit.command.clone()),
                        is_secret: Some(exit.is_secret),
                    },
                }),
        );
        if current.len() + group.len() > MAX_MUTATION_OPERATIONS && !current.is_empty() {
            batches.push(AreaMutationBatch::strict(
                document.area.id,
                std::mem::take(&mut current),
                "Copy map connections",
            ));
        }
        current.extend(group);
    }
    if !current.is_empty() {
        batches.push(AreaMutationBatch::strict(
            document.area.id,
            current,
            "Copy map connections",
        ));
    }
    batches
}

/// How [`freshen_documents`] treats viewer-only markings.
pub(crate) struct FreshenOptions {
    /// Strip every `is_secret` marking and stamp locally-owned access — the
    /// JSON-import contract, which resets foreign metadata to a
    /// locally-owned area. Relocation keeps markings: a copy of one's own
    /// map preserves the viewer's projection verbatim.
    pub scrub_secrets: bool,
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
    for document in documents {
        document.area.id = id_map[&document.area.id];
        document.area.atlas_id = None;
        document.area.atlas_name = None;
        document.area.user_id = None;
        document.area.rev = 1;
        document.area.copied_from_area_id = None;
        document.area.copied_from_rev = None;
        document.area.copied_at = None;
        document.area.family_token = None;
        document.content_hash = None;
        document.linked_areas.clear();
        if options.scrub_secrets {
            document.area.access = Some(crate::AreaAccess::OWNER);
            document.area.owner_nickname = None;
        }

        for label in &mut document.labels {
            label.id = LabelId(uuid::Uuid::new_v4());
            if options.scrub_secrets {
                label.is_secret = false;
            }
        }
        for shape in &mut document.shapes {
            shape.id = ShapeId(uuid::Uuid::new_v4());
            if options.scrub_secrets {
                shape.is_secret = false;
            }
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
        for room in &mut document.rooms {
            if options.scrub_secrets {
                room.is_secret = false;
            }
            for exit in &mut room.exits {
                exit.id = ExitId::new();
                exit.connection_id = connection_map[&exit.connection_id];
                if options.scrub_secrets {
                    exit.is_secret = false;
                }
                exit.to_unknown = false;
                exit.to_area_token = None;
                exit.to_area_id = match exit.to_area_id {
                    Some(old) if id_map.contains_key(&old) => Some(id_map[&old]),
                    Some(_) => {
                        exit.to_room_number = None;
                        exit.to_direction = None;
                        None
                    }
                    None => None,
                };
            }
        }
        let leaves_area: HashSet<ConnectionId> = document
            .rooms
            .iter()
            .flat_map(|room| room.exits.iter())
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
    }
}

async fn cleanup_areas(mapper: &Mapper, area_ids: &[AreaId]) {
    for area_id in area_ids.iter().rev() {
        if let Err(error) = mapper.delete_area_and_wait(*area_id).await {
            warn!("failed to clean up relocated area {area_id}: {error}");
        }
    }
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

    /// C3: a local-tier destination is populated by the wholesale write
    /// path (one atomic file write), not envelope replay. The copy must
    /// carry the full document — rooms, linked exits and their connection,
    /// decorations, properties, tags — persist it durably (read back
    /// through the backend, not the cache), and the destination must accept
    /// ordinary envelope edits afterward.
    #[tokio::test]
    async fn move_to_local_bulk_writes_the_full_document() {
        let (mapper, root) = mapper("bulk-local").await;
        let source = mapper
            .create_area_at(
                "Deep Halls".to_string(),
                MapDestination::loose(MapStorage::Cloud),
            )
            .await
            .expect("create cloud source");
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
            area: crate::Area {
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
            },
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
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
            is_secret: false,
            external_id: None,
        }
    }

    fn member_exit(
        connection_id: ConnectionId,
        from_direction: crate::ExitDirection,
        to: Option<(AreaId, i32)>,
    ) -> Exit {
        Exit {
            id: ExitId::new(),
            from_direction,
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
        }
    }

    fn plain_connection(
        id: ConnectionId,
        a_room: i32,
        b_room: Option<i32>,
        kind: ConnectionKind,
    ) -> crate::Connection {
        let endpoint = |room: i32, side: crate::RoomSide| crate::ConnectionEndpoint {
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

    /// C6: the shared freshener remaps in-set cross-area targets across the
    /// whole document set, drops out-of-set targets, demotes their External
    /// Connections to Dangling exactly as a live edit would, and treats
    /// secrecy per caller contract — preserved for relocation, scrubbed to
    /// a locally-owned area for import.
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
            room.is_secret = true;
            let mut in_set = member_exit(to_b, crate::ExitDirection::East, Some((b, 5)));
            in_set.is_secret = true;
            room.exits.push(in_set);
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
        let id_map: HashMap<AreaId, AreaId> = [
            (a, AreaId(Uuid::new_v4())),
            (b, AreaId(Uuid::new_v4())),
        ]
        .into_iter()
        .collect();

        let mut preserved = build();
        freshen_documents(
            &mut preserved,
            &id_map,
            &FreshenOptions {
                scrub_secrets: false,
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
        assert!(
            room.is_secret && room.exits[0].is_secret,
            "relocation preserves the viewer's secrecy markings"
        );
        assert!(preserved[0].area.access.is_none(), "access left untouched");

        let mut scrubbed = build();
        freshen_documents(
            &mut scrubbed,
            &id_map,
            &FreshenOptions {
                scrub_secrets: true,
            },
        );
        let room = &scrubbed[0].rooms[0];
        assert!(
            !room.is_secret && room.exits.iter().all(|exit| !exit.is_secret),
            "import scrubs secrecy markings"
        );
        assert_eq!(
            scrubbed[0].area.access,
            Some(crate::AreaAccess::OWNER),
            "import stamps locally-owned access"
        );
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
        assert_ne!(local_copy, local_member, "cross-tier members mint fresh ids");
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

        cleanup_areas(&mapper, &[first, second]).await;
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
                    .upsert_room(RoomKey::new(area, RoomNumber(number)), RoomUpdates::default())
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

    /// C3: the server-side cloud clone applies only to a self-contained,
    /// fully acknowledged cloud→cloud copy — every other combination must
    /// take the freshen-and-replay path whose semantics the relocation
    /// contract documents.
    #[test]
    fn server_copy_gate_is_narrow() {
        let area_id = AreaId(Uuid::new_v4());
        let snapshot = |cross_area: bool, to_unknown: bool| {
            let connection_id = ConnectionId::new();
            let exit = Exit {
                id: ExitId::new(),
                from_direction: crate::ExitDirection::North,
                to_area_id: if cross_area {
                    Some(AreaId(Uuid::new_v4()))
                } else {
                    Some(area_id)
                },
                to_room_number: Some(RoomNumber(2)),
                to_direction: None,
                path: String::new(),
                is_hidden: false,
                is_closed: false,
                is_locked: false,
                weight: 1.0,
                command: String::new(),
                connection_id,
                to_unknown,
                to_area_token: None,
                is_secret: false,
            };
            AreaWithDetails {
                area: crate::Area {
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
                },
                format_version: crate::AREA_FORMAT_VERSION,
                content_hash: None,
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
                    is_secret: false,
                    external_id: None,
                }],
                labels: Vec::new(),
                shapes: Vec::new(),
                connections: Vec::new(),
                linked_areas: Vec::new(),
            }
        };

        let eligible = snapshot(false, false);
        assert!(server_copy_applies(
            &eligible,
            MapStorage::Cloud,
            MapStorage::Cloud,
            RelocationMode::Copy,
            Some(4),
        ));

        // Any single condition failing must force the replay path.
        assert!(
            !server_copy_applies(
                &eligible,
                MapStorage::Cloud,
                MapStorage::Cloud,
                RelocationMode::Move,
                Some(4),
            ),
            "moves never take the server clone"
        );
        assert!(
            !server_copy_applies(
                &eligible,
                MapStorage::Local,
                MapStorage::Cloud,
                RelocationMode::Copy,
                Some(4),
            ),
            "only a cloud source has a server-side copy"
        );
        assert!(
            !server_copy_applies(
                &eligible,
                MapStorage::Cloud,
                MapStorage::Local,
                RelocationMode::Copy,
                Some(4),
            ),
            "a cross-tier destination needs the freshen contract"
        );
        assert!(
            !server_copy_applies(
                &eligible,
                MapStorage::Cloud,
                MapStorage::Cloud,
                RelocationMode::Copy,
                Some(3),
            ),
            "queued unacknowledged edits would be missing from a server clone"
        );
        assert!(
            !server_copy_applies(
                &eligible,
                MapStorage::Cloud,
                MapStorage::Cloud,
                RelocationMode::Copy,
                None,
            ),
            "an unknown acknowledged revision is not proof of quiescence"
        );
        assert!(
            !server_copy_applies(
                &snapshot(true, false),
                MapStorage::Cloud,
                MapStorage::Cloud,
                RelocationMode::Copy,
                Some(4),
            ),
            "outbound cross-area links would survive a server clone but must dangle"
        );
        assert!(
            !server_copy_applies(
                &snapshot(false, true),
                MapStorage::Cloud,
                MapStorage::Cloud,
                RelocationMode::Copy,
                Some(4),
            ),
            "redacted destinations mark links that leave the area"
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

        // A second process adds a member after the first mapper's cache was
        // built. The inventory sees it; the first cache deliberately does not.
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
