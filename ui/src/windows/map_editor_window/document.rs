//! The document an edit reads and the place it writes.
//!
//! Each document owns its content; room addresses retain the source owning
//! their geometry. Attachments can therefore be edited without impersonating
//! rooms or translating their numbers.

use smudgy_cloud::mapper::RoomKey;
use smudgy_cloud::mapper::area_cache::{AreaCache, SourceLayer};
use smudgy_cloud::mapper::room_cache::RoomCache;
use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{
    AreaId, ConnectionId, ExitId, ExitUpdates, RoomAddress, RoomNumber, RoomUpdates, SourceId,
};
use std::sync::Arc;

use super::commands::Mutation;

/// One place's content on a map, as edits read it.
#[derive(Clone, Copy)]
pub struct Document<'a> {
    map: &'a AreaCache,
    layer: Option<&'a SourceLayer>,
}

impl<'a> Document<'a> {
    /// The map's own content.
    #[must_use]
    pub fn map(map: &'a AreaCache) -> Self {
        Self {
            map,
            layer: map.map_document_layer(),
        }
    }

    /// `source`'s content on `map`: the map itself, or a layer. `None` for
    /// a source the map has no layer for (Private additions before their
    /// first write).
    #[must_use]
    pub fn of(map: &'a AreaCache, source: SourceId) -> Option<Self> {
        if source.is_map() {
            return Some(Self::map(map));
        }
        map.source_layers()
            .iter()
            .find(|layer| layer.source() == source)
            .map(|layer| Self {
                map,
                layer: Some(layer),
            })
    }

    /// The document holding connection `id`.
    #[must_use]
    pub fn of_connection(map: &'a AreaCache, id: ConnectionId) -> Option<Self> {
        let (layer, _) = map.find_connection(id)?;
        Some(Self { map, layer })
    }

    /// The document's owned rooms, attachments, exits and connections.
    #[must_use]
    pub fn content(&self) -> &'a AreaCache {
        self.layer.map_or(self.map, SourceLayer::content)
    }

    /// The map's id: every document of a map writes under it.
    #[must_use]
    pub fn area_id(&self) -> AreaId {
        *self.map.get_id()
    }

    /// The place the document writes to.
    #[must_use]
    pub fn source(&self) -> SourceId {
        SourceLayer::source_of(self.layer)
    }

    /// Writes qualified operations to the source that owns their content.
    #[must_use]
    pub fn batch(&self, operations: Vec<AreaMutation>, description: impl Into<String>) -> Mutation {
        let area_id = self.area_id();
        let description = description.into();
        match self.layer {
            Some(layer) => Mutation::SourceBatch {
                area_id,
                source: layer.source(),
                operations,
                description,
                split_paired_exit: false,
            },
            None => Mutation::AreaBatch {
                area_id,
                operations,
                description,
            },
        }
    }

    /// One exit update in wire form. A
    /// direction edit on one end of a two-way link splits the link, on the
    /// map and in a Secret alike.
    #[must_use]
    pub fn update_exit(
        &self,
        room: RoomAddress,
        exit_id: ExitId,
        updates: ExitUpdates,
    ) -> Mutation {
        match self.layer {
            Some(layer) => Mutation::SourceBatch {
                area_id: self.area_id(),
                source: layer.source(),
                operations: vec![AreaMutation::UpdateExit {
                    exit_id,
                    body: updates,
                }],
                description: "Update exit".to_string(),
                split_paired_exit: true,
            },
            None => Mutation::UpdateExit {
                room_key: RoomKey::new(self.area_id(), room.number),
                id: exit_id,
                updates,
            },
        }
    }

    /// Brings back one of the document's own rooms with every field: a map
    /// room by upsert, as the map's room writes go; a source's own room by
    /// create, as a source's do.
    #[must_use]
    pub fn recreate_room(&self, room_number: RoomNumber, body: RoomUpdates) -> AreaMutation {
        if !self.source().is_map() {
            AreaMutation::CreateRoom {
                room_number,
                room_source: Some(self.source()),
                body,
            }
        } else {
            AreaMutation::UpsertRoom {
                room_number,
                room_source: None,
                body,
            }
        }
    }

    /// The readable room whose geometry anchors this document's content.
    #[must_use]
    pub fn shown_room(&self, address: RoomAddress) -> (RoomNumber, Option<&'a Arc<RoomCache>>) {
        let room = if address.source.is_map() {
            self.map.get_room(&address.number)
        } else {
            self.map
                .document_layer(address.source)
                .and_then(|layer| layer.own_room(address.number))
        };
        (address.number, room)
    }

    #[must_use]
    pub fn room_of(&self, address: RoomAddress) -> Option<RoomAddress> {
        self.content().get_room_at(address).map(|_| address)
    }
}
