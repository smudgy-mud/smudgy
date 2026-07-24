use std::sync::Arc;

use crate::{
    AreaId, ConnectionDash, ConnectionId, ConnectionKind, ConnectionRouting, CornerStyle,
    ExitDirection, connection_geometry::ConnectionGeometry, mapper::room_cache::RoomCache,
};

/// A resolved render view over one stored [`crate::Connection`] half: the
/// Connection's appearance fields, its geometry resolved exactly once at
/// cache build, and the special-kind facts the renderer needs, anchored on
/// endpoint A's room. A cross-level Connection contributes two halves — one
/// per endpoint level — that share a single geometry [`Arc`].
///
/// Nothing here is derived at render time: the exits' pairing, the endpoint
/// resolution, and the color parse all happened when the area cache was
/// built from the stored `connections` array.
#[derive(Debug, Clone)]
pub struct RoomConnection {
    pub connection_id: ConnectionId,
    /// The level this half renders on.
    pub from_level: i32,
    /// The shared geometry, resolved once at cache build.
    pub geometry: Arc<ConnectionGeometry>,
    pub kind: ConnectionKind,
    pub routing: ConnectionRouting,
    pub dash: ConnectionDash,
    pub corner: CornerStyle,
    pub thickness: f32,
    /// The Connection's parsed color (renderer-gray fallback when the stored
    /// string does not parse).
    pub color: iced::Color,
    /// Member count == 2: both traversal directions exist, so no arrow.
    pub is_bidirectional: bool,
    /// One-way arrow end in the geometry's A→B sense: `Some(true)` = the
    /// single member traverses A→B (arrow at B), `Some(false)` = B→A (arrow
    /// at A), `None` = bidirectional (no arrow).
    pub arrow_toward_b: Option<bool>,
    /// True when any member exit or either endpoint room is secret-marked
    /// (cleared views only ever see this); renderers draw these distinctly.
    pub is_secret: bool,
    pub to: RoomConnectionEnd,
    /// Endpoint A's room (grouping/level anchor). For the far half of a
    /// cross-level Connection this is endpoint B's room instead — each half
    /// anchors on its own room.
    pub room: Arc<RoomCache>,
}

#[derive(Debug, Clone)]
pub enum RoomConnectionEnd {
    None,
    /// Both Connection endpoints are the same room (a self-loop). A `Normal`
    /// end here would place the destination on top of the source and collapse
    /// to a bare directional stub, indistinguishable from a dangling `None`;
    /// renderers instead draw the geometry's loop arc.
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
        /// The compass direction this half's level marker anchors on:
        /// derived from the member exit's direction at this half's room,
        /// falling back to the endpoint's wall side.
        direction: ExitDirection,
        x: f32,
        y: f32,
        room: Arc<RoomCache>,
    },
    Normal {
        /// The wall direction at the destination: the member exit's
        /// `to_direction`, or the opposite of its `from_direction` when
        /// absent.
        direction: ExitDirection,
        x: f32,
        y: f32,
        room: Arc<RoomCache>,
    },
}
