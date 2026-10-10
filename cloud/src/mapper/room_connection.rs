use std::sync::Arc;

use crate::{
    AreaId, Connection, ConnectionDash, ConnectionId, ConnectionKind, ConnectionRouting,
    CornerStyle, ExitDirection, MapPoint,
    connection_geometry::{ConnectionGeometry, EndpointGeometry, GeometryInput, StubAxis},
    mapper::room_cache::RoomCache,
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
    /// The member-exit-derived stub axes the geometry was resolved with,
    /// kept so editor previews re-resolving during a drag reproduce the
    /// same stub treatment.
    pub stub_a: StubAxis,
    pub stub_b: StubAxis,
    /// The member-exit directions the stub axes were derived from, in the
    /// geometry's A/B sense. Up/Down endpoints render (and hit-test, and
    /// edit) as level triangles rather than wall ports, and only the
    /// direction — not the collapsed [`StubAxis`] — can say which is which.
    pub direction_a: ExitDirection,
    pub direction_b: Option<ExitDirection>,
    /// The Connection's parsed color (renderer-gray fallback when the stored
    /// string does not parse).
    pub color: iced::Color,
    /// Member count == 2: both traversal directions exist, so no arrow.
    pub is_bidirectional: bool,
    /// One-way arrow end in the geometry's A→B sense: `Some(true)` = the
    /// single member traverses A→B (arrow at B), `Some(false)` = B→A (arrow
    /// at A), `None` = bidirectional (no arrow).
    pub arrow_toward_b: Option<bool>,
    /// The most shut door among the members, so a closed or locked
    /// traversal shows even when its reciprocal member's door is open or
    /// absent; `None` when no member has a door.
    pub door: Option<crate::DoorState>,
    pub to: RoomConnectionEnd,
    /// Endpoint A's room (grouping/level anchor). For the far half of a
    /// cross-level Connection this is endpoint B's room instead — each half
    /// anchors on its own room.
    pub room: Arc<RoomCache>,
    pub(super) source: Connection,
    pub(super) endpoint_a_room: Arc<RoomCache>,
    pub(super) endpoint_b_room: Option<Arc<RoomCache>>,
    /// Coordinate multiplier used by view-local room-spacing presentation.
    pub layout_spacing: f32,
}

impl RoomConnection {
    /// Re-resolve this render view with multiplied room/route coordinates,
    /// keeping room glyph dimensions and port geometry unchanged.
    #[must_use]
    pub fn with_room_spacing(&self, spacing: f32) -> Self {
        if (spacing - 1.0).abs() < f32::EPSILON {
            return self.clone();
        }
        let scale = |point: MapPoint| MapPoint::new(point.x * spacing, point.y * spacing);
        let route_points: Vec<_> = self
            .source
            .route_points
            .iter()
            .copied()
            .map(scale)
            .collect();
        let endpoint_a = &self.source.endpoint_a;
        let geometry = crate::connection_geometry::resolve(&GeometryInput {
            kind: self.source.kind,
            routing: self.source.routing,
            corner: self.source.corner,
            endpoint_a: EndpointGeometry {
                room_center: MapPoint::new(
                    self.endpoint_a_room.get_x() * spacing,
                    self.endpoint_a_room.get_y() * spacing,
                ),
                side: endpoint_a.side,
                port_offset: endpoint_a.port_offset,
                stub: self.stub_a,
            },
            endpoint_b: self
                .source
                .endpoint_b
                .as_ref()
                .zip(self.endpoint_b_room.as_ref())
                .map(|(endpoint, room)| EndpointGeometry {
                    room_center: MapPoint::new(room.get_x() * spacing, room.get_y() * spacing),
                    side: endpoint.side,
                    port_offset: endpoint.port_offset,
                    stub: self.stub_b,
                }),
            route_points: &route_points,
            thickness: self.source.thickness,
        });
        Self {
            geometry: Arc::new(geometry),
            layout_spacing: spacing,
            ..self.clone()
        }
    }
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
