//! The Connection contract: shared visual geometry for map links.
//!
//! A [`Connection`] owns everything two reciprocal exits share on screen —
//! endpoint wall attachment (ports), routing mode and stored centerline
//! vertices, segment shape and corner treatment, dash/color/thickness — while
//! each member [`crate::Exit`] keeps directed traversal (topology, path,
//! command, weight, and the per-direction flags). One stored centerline is the
//! single source for routing, rendering, hit-testing, selection, culling, and
//! editing; the resolved output lives in [`crate::connection_geometry`].
//!
//! These types are the client half of the mirrored (not shared — see the
//! repo AGENTS.md) v2 wire contract with `smudgy-api`; their serialization
//! is the v2 area-projection shape. The kind/routing validity matrix and the
//! validation limits are part of that contract, so a change here is a
//! two-repo change.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ExitDirection;

/// Unique identifier for a [`Connection`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default,
)]
#[serde(transparent)]
pub struct ConnectionId(pub Uuid);

impl ConnectionId {
    #[must_use]
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl std::fmt::Display for ConnectionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

/// A point in absolute area coordinates. Route points are stored as these;
/// the geometry pipeline also uses them for every derived vertex.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct MapPoint {
    pub x: f32,
    pub y: f32,
}

impl MapPoint {
    #[must_use]
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// The wall of a room a Connection endpoint attaches to. Map space is
/// screen-like: +x is East, +y is South, so North is the top wall.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoomSide {
    North,
    East,
    South,
    West,
}

impl RoomSide {
    pub const ALL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    /// The outward unit normal of this wall.
    #[must_use]
    pub const fn outward(self) -> MapPoint {
        match self {
            Self::North => MapPoint::new(0.0, -1.0),
            Self::East => MapPoint::new(1.0, 0.0),
            Self::South => MapPoint::new(0.0, 1.0),
            Self::West => MapPoint::new(-1.0, 0.0),
        }
    }
}

/// Whether an endpoint's port slot is automatically maintained or was pinned
/// by hand. `AutoPinned` offsets are still stored (never recomputed at render
/// time), but layout commands may move them; `Manual` ports move only when the
/// author drags them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum PortMode {
    #[default]
    AutoPinned,
    Manual,
}

/// Server-derived topology classification of a Connection. Serialized so
/// rendering does not re-infer it; clients validate it against visible
/// topology only as a corruption check and can never set it directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionKind {
    /// Two different same-level rooms in the same area.
    Internal,
    /// Both endpoints are the same room.
    SelfLoop,
    /// One endpoint; the member exit has no destination.
    Dangling,
    /// One endpoint; the member exit leaves the area.
    External,
    /// Two same-area rooms on different levels.
    CrossLevel,
}

impl ConnectionKind {
    /// The kind × routing validity matrix. Only Internal Connections may use
    /// the routed modes; every special kind is limited to `Stub`/`Simple`.
    /// Enforced server-side, not merely disabled in the UI.
    #[must_use]
    pub const fn allows_routing(self, routing: ConnectionRouting) -> bool {
        match self {
            Self::Internal => true,
            Self::SelfLoop | Self::Dangling | Self::External | Self::CrossLevel => {
                matches!(routing, ConnectionRouting::Stub | ConnectionRouting::Simple)
            }
        }
    }
}

impl std::fmt::Display for ConnectionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// How the Connection's middle is produced. `Stub` hides the middle entirely
/// (wall stubs only); `Simple` runs straight stub-tip to stub-tip; `Manual`
/// and `Automatic` draw the stored centerline (`Automatic` marks it as
/// solver-produced). Stored route points stay dormant under `Stub`/`Simple`
/// so switching back restores them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConnectionRouting {
    Stub,
    #[default]
    Simple,
    Manual,
    Automatic,
}

impl ConnectionRouting {
    pub const ALL: [Self; 4] = [Self::Stub, Self::Simple, Self::Manual, Self::Automatic];
}

impl std::fmt::Display for ConnectionRouting {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// How a routed centerline's segments run. `Orthogonal` requires the stored
/// points to contain every elbow — the renderer never invents hidden corner
/// points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SegmentShape {
    #[default]
    Direct,
    Orthogonal,
}

impl SegmentShape {
    pub const ALL: [Self; 2] = [Self::Direct, Self::Orthogonal];
}

impl std::fmt::Display for SegmentShape {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Corner treatment at each logical centerline vertex. `Rounded` replaces the
/// corner with a quadratic fillet of [`crate::connection_geometry::CORNER_RADIUS`],
/// clamped to half the shorter adjacent leg.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CornerStyle {
    #[default]
    Sharp,
    Rounded,
}

impl CornerStyle {
    pub const ALL: [Self; 2] = [Self::Sharp, Self::Rounded];
}

impl std::fmt::Display for CornerStyle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// Dash pattern for the whole Connection stroke, continuous across the
/// centerline and stubs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ConnectionDash {
    #[default]
    Solid,
    Dashed,
    Dotted,
}

impl ConnectionDash {
    pub const ALL: [Self; 3] = [Self::Solid, Self::Dashed, Self::Dotted];
}

impl std::fmt::Display for ConnectionDash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::fmt::Display for RoomSide {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

/// The point on a room wall where a Connection attaches.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ConnectionEndpoint {
    pub room_number: crate::RoomNumber,
    pub side: RoomSide,
    /// Position along the wall, inclusive `0.0..=1.0`. Horizontal walls run
    /// west→east; vertical walls run north→south.
    pub port_offset: f32,
    pub port_mode: PortMode,
}

/// Shared visual geometry for one or two member exits. See the module docs
/// for the ownership split against [`crate::Exit`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: ConnectionId,
    pub endpoint_a: ConnectionEndpoint,
    /// Present for same-area destinations (including self-loops and
    /// cross-level links); absent for dangling and external links whose
    /// destination geometry is outside the current area. Rides the wire as
    /// an omitted field, not an explicit `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_b: Option<ConnectionEndpoint>,
    pub kind: ConnectionKind,
    pub routing: ConnectionRouting,
    pub segment_shape: SegmentShape,
    pub corner: CornerStyle,
    /// Interior centerline vertices in absolute area coordinates, between the
    /// two computed stub tips. For orthogonal routing they include every
    /// elbow.
    pub route_points: Vec<MapPoint>,
    pub dash: ConnectionDash,
    /// Canonical CSS color; [`DEFAULT_CONNECTION_COLOR`] when unset.
    pub color: String,
    /// Stroke thickness in map units, within [`THICKNESS_RANGE`].
    pub thickness: f32,
}

/// Client-authored fields for a new Connection. `kind` is deliberately
/// absent: every backend derives it from the final member-exit topology.
/// The id is minted before enqueue so exits in the same compound mutation can
/// refer to the Connection without waiting for a response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionArgs {
    pub id: ConnectionId,
    pub endpoint_a: ConnectionEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_b: Option<ConnectionEndpoint>,
    pub routing: ConnectionRouting,
    pub segment_shape: SegmentShape,
    pub corner: CornerStyle,
    #[serde(default)]
    pub route_points: Vec<MapPoint>,
    pub dash: ConnectionDash,
    pub color: String,
    pub thickness: f32,
}

impl From<&Connection> for ConnectionArgs {
    fn from(connection: &Connection) -> Self {
        Self {
            id: connection.id,
            endpoint_a: connection.endpoint_a,
            endpoint_b: connection.endpoint_b,
            routing: connection.routing,
            segment_shape: connection.segment_shape,
            corner: connection.corner,
            route_points: connection.route_points.clone(),
            dash: connection.dash,
            color: connection.color.clone(),
            thickness: connection.thickness,
        }
    }
}

/// Partial edit of a Connection's visual fields. Endpoint room identities
/// may be echoed in endpoint values but cannot be changed by this operation;
/// topology changes go through the semantic link/unlink/exit operations.
/// `route_points: Some(vec![])` explicitly clears the stored route.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ConnectionUpdates {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_a: Option<ConnectionEndpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_b: Option<ConnectionEndpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<ConnectionRouting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segment_shape: Option<SegmentShape>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corner: Option<CornerStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_points: Option<Vec<MapPoint>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dash: Option<ConnectionDash>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thickness: Option<f32>,
}

impl ConnectionUpdates {
    /// Applies this patch without changing the server-derived kind or id.
    #[must_use]
    pub fn apply(self, connection: &Connection) -> Connection {
        Connection {
            id: connection.id,
            endpoint_a: self.endpoint_a.unwrap_or(connection.endpoint_a),
            endpoint_b: self.endpoint_b.or(connection.endpoint_b),
            kind: connection.kind,
            routing: self.routing.unwrap_or(connection.routing),
            segment_shape: self.segment_shape.unwrap_or(connection.segment_shape),
            corner: self.corner.unwrap_or(connection.corner),
            route_points: self
                .route_points
                .unwrap_or_else(|| connection.route_points.clone()),
            dash: self.dash.unwrap_or(connection.dash),
            color: self.color.unwrap_or_else(|| connection.color.clone()),
            thickness: self.thickness.unwrap_or(connection.thickness),
        }
    }
}

/// The canonical default Connection color — the gray the renderer has always
/// used for exits.
pub const DEFAULT_CONNECTION_COLOR: &str = "#A4A4A4";

/// Default Connection stroke thickness in map units.
pub const DEFAULT_CONNECTION_THICKNESS: f32 = 1.0;

/// Maximum stored interior route points per Connection.
pub const MAX_ROUTE_POINTS: usize = 256;

/// Maximum operations in one compound mutation envelope.
pub const MAX_MUTATION_OPERATIONS: usize = 256;

/// Largest absolute value any map coordinate may take.
pub const MAX_COORDINATE: f32 = 1_000_000.0;

/// Accepted Connection stroke thickness, in map units.
pub const THICKNESS_RANGE: std::ops::RangeInclusive<f32> = 0.25..=8.0;

/// Longest accepted canonical color string, in bytes.
pub const MAX_COLOR_LEN: usize = 64;

/// The wall and offset an endpoint is *initialized* to from an exit
/// direction (never permanently derived — the inspector can move an endpoint
/// to any side afterwards): compass directions anchor their named wall, with
/// diagonals at the shared corner (NW = North wall at `0.0`, NE at `1.0`,
/// and likewise on the South wall).
///
/// `partner_bearing` is the unit-ish vector from this room toward the
/// partner (other room center, or outbound direction), used only by the
/// non-planar directions which have no wall of their own; without one they
/// fall back to East at `0.5`.
#[must_use]
pub fn default_anchor_for_direction(
    direction: ExitDirection,
    partner_bearing: Option<MapPoint>,
) -> (RoomSide, f32) {
    match direction {
        ExitDirection::Northwest => (RoomSide::North, 0.0),
        ExitDirection::North => (RoomSide::North, 0.5),
        ExitDirection::Northeast => (RoomSide::North, 1.0),
        ExitDirection::East => (RoomSide::East, 0.5),
        ExitDirection::Southwest => (RoomSide::South, 0.0),
        ExitDirection::South => (RoomSide::South, 0.5),
        ExitDirection::Southeast => (RoomSide::South, 1.0),
        ExitDirection::West => (RoomSide::West, 0.5),
        ExitDirection::Up
        | ExitDirection::Down
        | ExitDirection::In
        | ExitDirection::Out
        | ExitDirection::Special
        | ExitDirection::Other => (
            partner_bearing.map_or(RoomSide::East, side_nearest_bearing),
            0.5,
        ),
    }
}

/// The wall whose outward normal is nearest a bearing vector: dominant axis
/// wins, East/West on ties (a zero bearing lands on East).
#[must_use]
pub fn side_nearest_bearing(bearing: MapPoint) -> RoomSide {
    if bearing.x.abs() >= bearing.y.abs() {
        if bearing.x >= 0.0 {
            RoomSide::East
        } else {
            RoomSide::West
        }
    } else if bearing.y >= 0.0 {
        RoomSide::South
    } else {
        RoomSide::North
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_routing_matrix_matches_contract() {
        use ConnectionRouting::{Automatic, Manual, Simple, Stub};
        for routing in [Stub, Simple, Manual, Automatic] {
            assert!(ConnectionKind::Internal.allows_routing(routing));
        }
        for kind in [
            ConnectionKind::SelfLoop,
            ConnectionKind::Dangling,
            ConnectionKind::External,
            ConnectionKind::CrossLevel,
        ] {
            assert!(kind.allows_routing(Stub));
            assert!(kind.allows_routing(Simple));
            assert!(!kind.allows_routing(Manual));
            assert!(!kind.allows_routing(Automatic));
        }
    }

    #[test]
    fn cardinal_directions_anchor_their_walls() {
        assert_eq!(
            default_anchor_for_direction(ExitDirection::Northwest, None),
            (RoomSide::North, 0.0)
        );
        assert_eq!(
            default_anchor_for_direction(ExitDirection::North, None),
            (RoomSide::North, 0.5)
        );
        assert_eq!(
            default_anchor_for_direction(ExitDirection::Southeast, None),
            (RoomSide::South, 1.0)
        );
        assert_eq!(
            default_anchor_for_direction(ExitDirection::West, None),
            (RoomSide::West, 0.5)
        );
    }

    #[test]
    fn non_planar_directions_follow_partner_bearing() {
        let east = default_anchor_for_direction(ExitDirection::Up, Some(MapPoint::new(2.0, 0.5)));
        assert_eq!(east, (RoomSide::East, 0.5));
        let north = default_anchor_for_direction(ExitDirection::In, Some(MapPoint::new(0.5, -2.0)));
        assert_eq!(north, (RoomSide::North, 0.5));
        assert_eq!(
            default_anchor_for_direction(ExitDirection::Other, None),
            (RoomSide::East, 0.5)
        );
    }

    #[test]
    fn wire_shape_round_trips() {
        let connection = Connection {
            id: ConnectionId::new(),
            endpoint_a: ConnectionEndpoint {
                room_number: crate::RoomNumber(1),
                side: RoomSide::East,
                port_offset: 0.5,
                port_mode: PortMode::AutoPinned,
            },
            endpoint_b: Some(ConnectionEndpoint {
                room_number: crate::RoomNumber(2),
                side: RoomSide::West,
                port_offset: 0.25,
                port_mode: PortMode::Manual,
            }),
            kind: ConnectionKind::Internal,
            routing: ConnectionRouting::Manual,
            segment_shape: SegmentShape::Orthogonal,
            corner: CornerStyle::Rounded,
            route_points: vec![MapPoint::new(1.5, 0.0), MapPoint::new(1.5, 2.0)],
            dash: ConnectionDash::Dashed,
            color: DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: DEFAULT_CONNECTION_THICKNESS,
        };
        let json = serde_json::to_string(&connection).expect("serializes");
        let back: Connection = serde_json::from_str(&json).expect("deserializes");
        assert_eq!(back, connection);
        // Enum variants ride the wire as PascalCase strings.
        assert!(json.contains("\"Orthogonal\""));
        assert!(json.contains("\"AutoPinned\""));
    }
}
