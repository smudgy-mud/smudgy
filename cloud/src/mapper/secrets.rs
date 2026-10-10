//! Secrets and moves on cloud maps. The server owns both; this module orders
//! them against the map's queued edits and republishes the map once the
//! server has accepted them.

use std::collections::BTreeMap;
use std::time::Duration;

use log::warn;
use uuid::Uuid;

use super::{
    Inner, MapStorage, MapperEvent, RoomKey, area_cache::source_area_id,
    merge::MERGE_DRAIN_TIMEOUT, sync_engine,
};
use crate::{
    AreaId, CloudError, CloudResult, ConnectionId, LabelId, RoomNumber, RoomRemap, ShapeId,
    SourceId,
    access_review::ReviewedMove,
    clan_secrets::NewSecret,
    cloud_api::{SecretChange, SecretGrant, SecretSummary},
    mutation::{MoveRequest, MoveResult, MovedRoom, Precondition, ResourceKind},
};

/// What a move carries from one source of a map to another. A room takes
/// the number it asks for in the destination (its own, unless `asked` names
/// another) unless the destination already uses it or an earlier room took
/// it; then it takes the destination's next free number, in the order
/// `rooms` lists them. Every exit and connection touching a moved room
/// travels with it. A connection named in `connections` moves with its own
/// exits. Same-map room references remain source-qualified when their room
/// stays behind; foreign destinations retain their independent Read gate.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MovedContent {
    pub rooms: Vec<RoomNumber>,
    pub connections: Vec<ConnectionId>,
    pub labels: Vec<LabelId>,
    pub shapes: Vec<ShapeId>,
    pub properties: Vec<crate::mutation::PropertyAddress>,
    pub property_resolutions: Vec<crate::mutation::PropertyResolution>,
    /// The number a room asks for in the destination, by its number in the
    /// source, for each room asking for a number other than its own.
    pub asked: BTreeMap<RoomNumber, RoomNumber>,
}

impl MovedContent {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rooms.is_empty()
            && self.connections.is_empty()
            && self.labels.is_empty()
            && self.shapes.is_empty()
            && self.properties.is_empty()
    }

    /// The rooms as a move request lists them, each with the number it asks
    /// for.
    #[must_use]
    pub fn moved_rooms(&self) -> Vec<MovedRoom> {
        self.rooms
            .iter()
            .map(|number| MovedRoom {
                room_number: *number,
                asks: self.asked.get(number).copied(),
            })
            .collect()
    }
}

impl Inner {
    fn move_request(
        &self,
        area_id: AreaId,
        from: SourceId,
        to: SourceId,
        content: &MovedContent,
    ) -> CloudResult<MoveRequest> {
        if from == to || content.is_empty() {
            return Err(CloudError::InvalidInput(
                "a move needs content and two different sources".to_string(),
            ));
        }
        Ok(MoveRequest {
            operation_id: Uuid::new_v4(),
            preconditions: vec![
                Precondition::source(area_id.0, from, self.source_revision(area_id, &from)?),
                Precondition::source(area_id.0, to, self.source_revision(area_id, &to)?),
            ],
            from,
            to,
            rooms: content.moved_rooms(),
            connections: content.connections.clone(),
            labels: content.labels.clone(),
            shapes: content.shapes.clone(),
            access_review: None,
            properties: content.properties.clone(),
            property_resolutions: content.property_resolutions.clone(),
        })
    }

    pub(super) async fn review_move_content(
        &self,
        area_id: AreaId,
        from: SourceId,
        to: SourceId,
        content: MovedContent,
    ) -> CloudResult<ReviewedMove> {
        let generation = self.source_write_generation(area_id)?;
        let fences = tokio::time::timeout(
            MERGE_DRAIN_TIMEOUT,
            self.fence_drained_areas(&[area_id], "move_busy"),
        )
        .await
        .map_err(|_| CloudError::StructuralConflict("move_busy".to_string()))??;
        let result = async {
            let request = self.move_request(area_id, from, to, &content)?;
            let review = self
                .backend
                .review_move_content(&area_id, &request, generation)
                .await?;
            Ok(ReviewedMove {
                review,
                request,
                area_id,
                content,
                generation,
            })
        }
        .await;
        Self::release_fences(fences);
        result
    }

    pub(super) async fn create_secret(
        &self,
        area_id: AreaId,
        secret: &NewSecret,
    ) -> CloudResult<SecretSummary> {
        let generation = self.source_write_generation(area_id)?;
        let secret = self
            .backend
            .create_secret_as(&area_id, secret, generation)
            .await?;
        self.republish_after_source_write(area_id, generation).await;
        Ok(secret)
    }

    pub(super) async fn update_secret(
        &self,
        area_id: AreaId,
        secret: &SourceId,
        change: &SecretChange,
    ) -> CloudResult<SecretSummary> {
        let generation = self.source_write_generation(area_id)?;
        let updated = self
            .backend
            .update_secret(&area_id, secret, change, generation)
            .await?;
        self.republish_after_source_write(area_id, generation).await;
        Ok(updated)
    }

    pub(super) async fn rename_secret(
        &self,
        area_id: AreaId,
        secret: &SourceId,
        name: &str,
    ) -> CloudResult<SecretSummary> {
        let generation = self.source_write_generation(area_id)?;
        let renamed = self
            .backend
            .rename_secret(&area_id, secret, name, generation)
            .await?;
        self.republish_after_source_write(area_id, generation).await;
        Ok(renamed)
    }

    pub(super) async fn recolor_secret(
        &self,
        area_id: AreaId,
        secret: &SourceId,
        color: Option<&str>,
    ) -> CloudResult<SecretSummary> {
        let generation = self.source_write_generation(area_id)?;
        let recolored = self
            .backend
            .recolor_secret(&area_id, secret, color, generation)
            .await?;
        self.republish_after_source_write(area_id, generation).await;
        Ok(recolored)
    }

    pub(super) async fn delete_secret(
        &self,
        area_id: AreaId,
        secret: &SourceId,
    ) -> CloudResult<()> {
        let generation = self.source_write_generation(area_id)?;
        self.backend
            .delete_secret(&area_id, secret, generation)
            .await?;
        self.republish_after_source_write(area_id, generation).await;
        Ok(())
    }

    // A Secret's grants change who else reads it, never what the caller
    // reads, so these calls republish nothing.

    pub(super) async fn secret_grants(
        &self,
        area_id: AreaId,
        secret: &SourceId,
    ) -> CloudResult<Vec<SecretGrant>> {
        let generation = self.source_write_generation(area_id)?;
        self.backend
            .secret_grants(&area_id, secret, generation)
            .await
    }

    pub(super) async fn grant_secret(
        &self,
        area_id: AreaId,
        secret: &SourceId,
        grantee_id: Uuid,
        actions: &[&str],
    ) -> CloudResult<SecretGrant> {
        let generation = self.source_write_generation(area_id)?;
        self.backend
            .grant_secret(&area_id, secret, grantee_id, actions, generation)
            .await
    }

    pub(super) async fn update_secret_grant(
        &self,
        area_id: AreaId,
        secret: &SourceId,
        grant_id: Uuid,
        actions: &[&str],
    ) -> CloudResult<SecretGrant> {
        let generation = self.source_write_generation(area_id)?;
        self.backend
            .update_secret_grant(&area_id, secret, grant_id, actions, generation)
            .await
    }

    pub(super) async fn revoke_secret_grant(
        &self,
        area_id: AreaId,
        secret: &SourceId,
        grant_id: Uuid,
    ) -> CloudResult<()> {
        let generation = self.source_write_generation(area_id)?;
        self.backend
            .revoke_secret_grant(&area_id, secret, grant_id, generation)
            .await
    }

    /// Moves content between two sources of a map. The map's queued edits
    /// land first and new ones are fenced off until the map is republished,
    /// so both preconditions describe what the server holds.
    pub(super) async fn move_content(
        &self,
        area_id: AreaId,
        from: SourceId,
        to: SourceId,
        content: MovedContent,
        reviewed: Option<ReviewedMove>,
    ) -> CloudResult<MoveResult> {
        if from == to {
            return Err(CloudError::InvalidInput(
                "a move needs two different sources".to_string(),
            ));
        }
        if content.is_empty() {
            return Err(CloudError::InvalidInput("nothing to move".to_string()));
        }
        // A clan's Secret on a map filed by link trades content with none of
        // the map's own sources (the server refuses such a move).
        if self
            .atlas_cache
            .load()
            .get_area(&area_id)
            .is_some_and(|area| {
                let (from_clan, to_clan) = (area.linked_clan_of(from), area.linked_clan_of(to));
                (from_clan.is_some() || to_clan.is_some()) && from_clan != to_clan
            })
        {
            return Err(CloudError::InvalidInput(
                "secret_linked_map_rooms".to_string(),
            ));
        }
        let generation = self.source_write_generation(area_id)?;
        if reviewed
            .as_ref()
            .is_some_and(|review| review.generation != generation)
        {
            return Err(CloudError::CredentialChanged);
        }
        let moved_rooms = content.rooms.clone();
        let fences = tokio::time::timeout(
            MERGE_DRAIN_TIMEOUT,
            self.fence_drained_areas(&[area_id], "move_busy"),
        )
        .await
        .map_err(|_| CloudError::StructuralConflict("move_busy".to_string()))??;
        let request = match reviewed.map_or_else(
            || self.move_request(area_id, from, to, &content),
            |review| {
                let mut request = review.request;
                request.access_review = Some(review.review.token);
                Ok(request)
            },
        ) {
            Ok(request) => request,
            Err(error) => {
                Self::release_fences(fences);
                return Err(error);
            }
        };
        let result = self
            .backend
            .move_content(&area_id, &request, generation)
            .await;
        match &result {
            Ok(result) => {
                // Both sources and every other one the move changed stand
                // on these revisions now, whatever the republish below
                // manages to read.
                let mut elsewhere = false;
                for version in &result.versions {
                    if version.resource == ResourceKind::Source {
                        self.pending.note_source_rev(
                            AreaId(version.id),
                            version.source,
                            version.rev,
                        );
                        elsewhere |= version.id != area_id.0;
                    }
                }
                self.republish_after_source_write(area_id, generation).await;
                self.announce_move(area_id, from, to, &moved_rooms, result);
                if elsewhere {
                    self.request_sync();
                }
            }
            Err(CloudError::RevisionConflict { .. }) => {
                // A source moved on elsewhere. Show the map as the server
                // now holds it, so the caller sees that change and a move
                // asked again stands on it.
                self.backend.purge_area(&area_id).await;
                self.republish_after_source_write(area_id, generation).await;
            }
            Err(_) => {}
        }
        Self::release_fences(fences);
        result
    }

    /// Tells every holder of room handles (location markers, editor
    /// selections, scripts) that `rooms` moved from `from` to `to` of map
    /// `area_id`, each under the number `moved` gave it there. Each source
    /// reads as an area of its own, so a move is a merge of rooms from one
    /// area into another that deletes neither.
    fn announce_move(
        &self,
        area_id: AreaId,
        from: SourceId,
        to: SourceId,
        rooms: &[RoomNumber],
        moved: &MoveResult,
    ) {
        if rooms.is_empty() {
            return;
        }
        let viewer = self.pending.active_viewer().map(|(viewer, _)| viewer);
        let area_of = |source: SourceId| source_area_id(area_id, source, viewer).unwrap_or(area_id);
        let from_area = area_of(from);
        self.pending.emit(MapperEvent::AreasMerged {
            into: area_of(to),
            deleted: Vec::new(),
            rooms: rooms
                .iter()
                .map(|number| RoomRemap {
                    from: RoomKey::new(from_area, *number),
                    to: moved.number_in_destination(*number),
                })
                .collect(),
        });
    }

    /// The credential generation a Secret write or move runs under, once the
    /// map is known to be a loaded cloud map that no delete or move holds.
    fn source_write_generation(&self, area_id: AreaId) -> CloudResult<u64> {
        if self.atlas_cache.load().get_area(&area_id).is_none() {
            return Err(CloudError::AreaNotFound(area_id));
        }
        if self.area_storage(area_id) != MapStorage::Cloud {
            return Err(CloudError::InvalidInput(
                "only cloud maps have Secrets".to_string(),
            ));
        }
        if self.pending.is_delete_fenced(area_id) {
            return Err(CloudError::PendingOperations(
                "this map is being moved or deleted".to_string(),
            ));
        }
        Ok(self.backend.auth_generation())
    }

    /// The revision a source of a loaded map stands on at the server: the
    /// newer of the last acknowledged one and the cached one. The published
    /// map keeps each source's revision as it was read, while every
    /// acknowledged edit, move and sync row moves the acknowledged one; a
    /// read can also bring a later edit from elsewhere. The caller's Private
    /// additions start at revision 0 until the first write creates them.
    fn source_revision(&self, area_id: AreaId, source: &SourceId) -> CloudResult<i64> {
        let cache = self.atlas_cache.load();
        let area = cache
            .get_area(&area_id)
            .ok_or(CloudError::AreaNotFound(area_id))?;
        let cached = if source.is_map() {
            area.get_rev()
        } else {
            match area
                .meta()
                .sources
                .iter()
                .find(|bundle| &bundle.source == source)
            {
                Some(bundle) => bundle.rev,
                None if *source == SourceId::Private => 0,
                None => {
                    return Err(CloudError::InvalidInput(format!(
                        "this map has no source {source}"
                    )));
                }
            }
        };
        // Revisions only rise, so whichever is newer is where the server
        // stands: a read that lands after an acknowledgement can carry a
        // later edit from elsewhere.
        Ok(self
            .pending
            .confirmed_source_rev(area_id, *source)
            .map_or(cached, |confirmed| confirmed.max(cached)))
    }

    /// Republishes a map after the server accepted a write to its sources:
    /// the server's projection plus whatever edits remain queued. Queued
    /// edits get a bounded chance to land first, so the read is not folded
    /// under an edit already on the wire. A failed read is left to the sync
    /// engine, which the write already nudged.
    async fn republish_after_source_write(&self, area_id: AreaId, auth_generation: u64) {
        let drained = tokio::time::timeout(MERGE_DRAIN_TIMEOUT, async {
            while self.pending.queued_len(area_id) > 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        if drained.is_err() {
            warn!("map {area_id} still has queued edits; republishing over them");
        }
        for _ in 0..3 {
            match sync_engine::refetch_area(self, &area_id, auth_generation).await {
                Ok(true) => return,
                Ok(false) => {}
                Err(error) => {
                    warn!("could not refetch map {area_id} after a write: {error}");
                    return;
                }
            }
        }
        self.request_sync();
        warn!("the refetch of map {area_id} remained stale; the sync engine retries");
    }
}
