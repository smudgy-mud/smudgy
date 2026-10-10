use crate::{AreaId, ConnectionId, Door, Exit, ExitDirection, ExitId, RoomNumber, SourceId};

/// An exit as the area cache holds it. Its destination reads as the atlas
/// reads areas: an exit into a room of another map's Secret names the
/// Secret's own area in `to_area_id` (the Secret's id) and that map in
/// [`Self::to_secret_map`], so lookups, routing and scripts follow it like
/// any exit into another area.
#[derive(Debug, Clone)]
pub struct ExitCache {
    pub id: ExitId,
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    /// For an exit into a room of another map's Secret, that map; the
    /// Secret's own area is `to_area_id`. `None` for every other exit.
    pub to_secret_map: Option<AreaId>,
    /// Parent map of a destination in this viewer's Private source.
    pub to_private_map: Option<AreaId>,
    pub path: Option<String>,
    pub is_hidden: bool,
    /// The exit's door; `None` for an exit without one.
    pub door: Option<Door>,
    pub weight: f32,
    pub command: Option<String>,
    /// The stored [`crate::Connection`] this exit is a member of; all visual
    /// appearance (routing, dash, color, thickness) lives there.
    pub connection_id: ConnectionId,
    /// Destination exists but is invisible to the viewer ("Unknown map").
    pub to_unknown: bool,
    /// Stable per-viewer token for the hidden destination; converging exits
    /// share one token.
    pub to_area_token: Option<String>,
}

impl From<Exit> for ExitCache {
    /// The cache's form of a document's exit. A document keeps `to_source`
    /// only on exits into another map's Secret rooms (a source's references
    /// to its own rooms are read into its numbering first), so a Secret
    /// named there is always another map's.
    fn from(exit: Exit) -> Self {
        let (to_area_id, to_secret_map) = match exit.foreign_secret() {
            Some((map, secret)) => (Some(AreaId(secret)), Some(map)),
            None if exit.to_source == Some(SourceId::Private) => (None, None),
            None => (exit.to_area_id, None),
        };
        Self {
            id: exit.id,
            from_direction: exit.from_direction,
            to_area_id,
            to_room_number: exit.to_room_number,
            to_direction: exit.to_direction,
            to_secret_map,
            to_private_map: (exit.to_source == Some(SourceId::Private))
                .then_some(exit.to_area_id)
                .flatten(),
            path: (!exit.path.is_empty()).then_some(exit.path),
            is_hidden: exit.is_hidden,
            door: exit.door,
            weight: exit.weight,
            command: (!exit.command.is_empty()).then_some(exit.command),
            connection_id: exit.connection_id,
            to_unknown: exit.to_unknown,
            to_area_token: exit.to_area_token,
        }
    }
}

impl ExitCache {
    /// The actual destination address, independent of the compatibility area
    /// IDs used by legacy routing and script handles.
    #[must_use]
    pub fn destination_address(&self) -> Option<crate::MapRoomAddress> {
        if self.to_unknown {
            return None;
        }
        Some(crate::MapRoomAddress {
            map: self.wire_to_area_id()?,
            room: crate::RoomAddress::new(
                self.to_private_map
                    .map(|_| SourceId::Private)
                    .or_else(|| self.foreign_secret().map(|(_, id)| SourceId::Secret(id)))
                    .unwrap_or_default(),
                self.to_room_number?,
            ),
        })
    }
    /// Resolve Private addressing with the authenticated viewer. Until then
    /// its destination cannot be mistaken for an ordinary map room.
    pub(crate) fn with_viewer(mut self, viewer: Option<uuid::Uuid>) -> Self {
        if let Some(map) = self.to_private_map {
            self.to_area_id = super::area_cache::source_area_id(map, SourceId::Private, viewer);
        }
        self
    }
    /// The exit in document (wire) form: an exit into another map's Secret
    /// room names that map, with the Secret as `to_source`.
    #[must_use]
    pub fn to_exit(&self) -> Exit {
        Exit {
            to_source: self.to_private_map.map(|_| SourceId::Private).or_else(|| {
                self.foreign_secret()
                    .map(|(_, secret)| SourceId::Secret(secret))
            }),
            id: self.id,
            from_direction: self.from_direction,
            to_area_id: self.wire_to_area_id(),
            to_room_number: self.to_room_number,
            to_direction: self.to_direction,
            path: self.path.clone().unwrap_or_default(),
            is_hidden: self.is_hidden,
            door: self.door.clone(),
            weight: self.weight,
            command: self.command.clone().unwrap_or_default(),
            connection_id: self.connection_id,
            to_unknown: self.to_unknown,
            to_area_token: self.to_area_token.clone(),
        }
    }

    /// The map the destination is in, as the wire names it: for an exit
    /// into another map's Secret room, that map rather than the Secret's
    /// own area.
    #[must_use]
    pub fn wire_to_area_id(&self) -> Option<AreaId> {
        self.to_private_map
            .or(self.to_secret_map)
            .or(self.to_area_id)
    }

    /// For an exit into a room of another map's Secret: that map and the
    /// Secret's id.
    #[must_use]
    pub fn foreign_secret(&self) -> Option<(AreaId, uuid::Uuid)> {
        Some((self.to_secret_map?, self.to_area_id?.0))
    }
}
