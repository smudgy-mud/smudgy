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
    /// the corresponding destination areas. Links leaving the set become
    /// dangling, matching portable import semantics.
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

        // A same-tier move is merely a folder change. Preserve ids and avoid
        // copying bytes; this is the fast path the old move API exposed.
        if mode == RelocationMode::Move
            && source_ids
                .iter()
                .all(|id| self.area_storage(id) == destination.storage)
        {
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

        let mut move_fences = if mode == RelocationMode::Move {
            let fences = self.begin_area_move(&source_ids)?;
            self.wait_area_move_quiescent(&fences).await;
            Some(fences)
        } else {
            None
        };

        let snapshots = self.snapshot_areas(&source_ids)?;
        for snapshot in &snapshots {
            validate_import_document(snapshot)?;
        }
        // The backend revision each move snapshot stands on: the last
        // acknowledged revision when one is known, else the cached document
        // revision (queued-but-unsent optimistic bumps ride the copy and are
        // discarded with the source, so they must not inflate the guard).
        let expected_revs: Vec<i64> = source_ids
            .iter()
            .zip(&snapshots)
            .map(|(id, snapshot)| self.confirmed_area_rev(*id).unwrap_or(snapshot.area.rev))
            .collect();
        let mut destination_ids = Vec::with_capacity(snapshots.len());
        for snapshot in &snapshots {
            match self
                .create_area_at(snapshot.area.name.clone(), destination)
                .await
            {
                Ok(id) => destination_ids.push(id),
                Err(error) => {
                    cleanup_areas(self, &destination_ids).await;
                    return Err(error.into());
                }
            }
        }

        let id_map: HashMap<_, _> = source_ids
            .iter()
            .copied()
            .zip(destination_ids.iter().copied())
            .collect();
        let documents: Vec<_> = snapshots
            .into_iter()
            .zip(destination_ids.iter().copied())
            .map(|(document, destination_id)| freshen(document, destination_id, &id_map))
            .collect();

        if let Err(error) = self.populate_documents(&documents).await {
            cleanup_areas(self, &destination_ids).await;
            return Err(error.into());
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
                        completed: Some(MapRelocation {
                            source_ids: source_ids.clone(),
                            destination_ids: destination_ids.clone(),
                            destination,
                        }),
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
    async fn populate_documents(&self, documents: &[AreaWithDetails]) -> CloudResult<()> {
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

        let connection_batches = documents.iter().flat_map(connection_batches).collect();
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

fn freshen(
    mut document: AreaWithDetails,
    destination_id: AreaId,
    id_map: &HashMap<AreaId, AreaId>,
) -> AreaWithDetails {
    document.area.id = destination_id;
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
    for room in &mut document.rooms {
        for exit in &mut room.exits {
            exit.id = ExitId::new();
            exit.connection_id = connection_map[&exit.connection_id];
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
        .filter(|exit| exit.to_area_id.is_some_and(|id| id != destination_id))
        .map(|exit| exit.connection_id)
        .collect();
    for connection in &mut document.connections {
        if connection.kind == ConnectionKind::External && !leaves_area.contains(&connection.id) {
            connection.kind = ConnectionKind::Dangling;
            connection.endpoint_b = None;
        }
    }
    document
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
