use crate::{AreaId, ConnectionId, Exit, ExitDirection, ExitId, RoomNumber};

#[derive(Debug, Clone)]
pub struct ExitCache {
    pub id: ExitId,
    pub from_direction: ExitDirection,
    pub to_area_id: Option<AreaId>,
    pub to_room_number: Option<RoomNumber>,
    pub to_direction: Option<ExitDirection>,
    pub path: Option<String>,
    pub is_hidden: bool,
    pub is_closed: bool,
    pub is_locked: bool,
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
    pub is_secret: bool,
}

impl From<Exit> for ExitCache {
    fn from(exit: Exit) -> Self {
        Self {
            id: exit.id,
            from_direction: exit.from_direction,
            to_area_id: exit.to_area_id,
            to_room_number: exit.to_room_number,
            to_direction: exit.to_direction,
            path: (!exit.path.is_empty()).then_some(exit.path),
            is_hidden: exit.is_hidden,
            is_closed: exit.is_closed,
            is_locked: exit.is_locked,
            weight: exit.weight,
            command: (!exit.command.is_empty()).then_some(exit.command),
            connection_id: exit.connection_id,
            to_unknown: exit.to_unknown,
            to_area_token: exit.to_area_token,
            is_secret: exit.is_secret,
        }
    }
}

impl ExitCache {
    #[must_use]
    pub(crate) fn to_exit(&self) -> Exit {
        Exit {
            id: self.id,
            from_direction: self.from_direction,
            to_area_id: self.to_area_id,
            to_room_number: self.to_room_number,
            to_direction: self.to_direction,
            path: self.path.clone().unwrap_or_default(),
            is_hidden: self.is_hidden,
            is_closed: self.is_closed,
            is_locked: self.is_locked,
            weight: self.weight,
            command: self.command.clone().unwrap_or_default(),
            connection_id: self.connection_id,
            to_unknown: self.to_unknown,
            to_area_token: self.to_area_token.clone(),
            is_secret: self.is_secret,
        }
    }
}
