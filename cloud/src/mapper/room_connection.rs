use std::sync::Arc;

use crate::{AreaId, ExitDirection, ExitStyle, mapper::room_cache::RoomCache};

#[derive(Debug, Clone)]
pub struct RoomConnection {
    pub from_level: i32,
    pub from_x: f32,
    pub from_y: f32,
    pub from_direction: ExitDirection,
    pub room: Arc<RoomCache>,
    pub to: RoomConnectionEnd,
    pub is_bidirectional: bool,
    /// True when the originating exit (or its paired reverse exit) or either
    /// endpoint room is secret-marked; renderers draw these distinctly.
    pub is_secret: bool,
    /// Drawing style of the originating exit; for a bidirectional pair each
    /// half carries its own direction's style.
    pub style: ExitStyle,
}

#[derive(Debug, Clone)]
pub enum RoomConnectionEnd {
    None,
    /// The exit's destination is its own origin room (a self-loop). A `Normal`
    /// end here would place the destination on top of the source and collapse
    /// to a bare directional stub, indistinguishable from a dangling `None`;
    /// renderers instead draw a small loop arc off the room's wall. The origin
    /// room is the enclosing [`RoomConnection::room`], and the wall side is its
    /// [`RoomConnection::from_direction`].
    SelfLoop,
    External {
        area_id: AreaId,
    },
    /// The destination exists but is not visible to the viewer (`to_unknown`
    /// on the projected exit). Renderers must show the literal "Unknown map"
    /// — never a name or id. `token` is the per-viewer `to_area_token`;
    /// exits sharing one token converge on the same hidden destination.
    Unknown {
        token: String,
    },
    ToLevel {
        level: i32,
        direction: ExitDirection,
        x: f32,
        y: f32,
        room: Arc<RoomCache>,
    },
    Normal {
        direction: ExitDirection,
        x: f32,
        y: f32,
        room: Arc<RoomCache>,
    },
}
