//! The one geometry pipeline for Connections: consumes a
//! [`crate::Connection`]'s fields plus its endpoint room centers and emits
//! every derived product — port/stub segments, the logical centerline,
//! flattened polylines for hit-testing and collision, rounded-corner path
//! primitives, end tangents for arrowheads, editing handles, and full
//! expanded bounds.
//!
//! Rendering, hit-testing, selection, drag previews, the spatial index,
//! export previews, and both canvas backends consume this one result; nothing
//! downstream re-derives geometry. The module is deliberately iced-free so
//! the server can mirror its validation half (orthogonality, normalization)
//! byte-for-byte.
//!
//! Coordinates are map units in a screen-like frame: +x East, +y South.
//! Rooms are uniform [`ROOM_SIZE`] squares centered on their `(x, y)`.
//! Inputs must be finite — non-finite coordinates are rejected at the wire,
//! file-load, and mutation boundaries before they can reach this module —
//! and resolution stays total/panic-free even on garbage so a corrupt value
//! can never take the renderer down.

use crate::connection::{ConnectionKind, ConnectionRouting, CornerStyle, MapPoint, RoomSide};

/// Room square edge length, in map units: the room-geometry authority every
/// consumer derives from (`map_widget` re-exports it as its own room size).
pub const ROOM_SIZE: f32 = 0.5;

/// Distance from a port (on the wall) to its stub tip, outward along the
/// wall normal. `0.25` half-room + `0.15` puts stub tips `0.4` from the room
/// center, the reach exit stubs have always had on screen.
pub const STUB_LENGTH: f32 = 0.15;

/// Fillet radius for rounded corners, clamped to half the shorter adjacent
/// leg so short legs never overlap their fillets.
pub const CORNER_RADIUS: f32 = 0.2;

/// Arrowhead leg length for one-way Connections, in map units.
pub const ARROW_SIZE: f32 = 0.1;

/// Radius of the self-loop arc — the small circle tangent to the room wall.
pub const SELF_LOOP_RADIUS: f32 = 0.15;

/// Extra tail a dangling Connection's `Simple` rendering extends beyond the
/// stub tip; total reach from the wall matches a plain stub-tip-to-stub-tip
/// line's visual weight.
pub const DANGLING_TAIL_LENGTH: f32 = 0.25;

/// Bounds reservation past each stub tip for the marker glyphs the renderer
/// draws on External/CrossLevel Connections (level triangles, area dots).
/// Deliberately its own constant: marker footprint and the dangling tail are
/// unrelated quantities that merely start equal.
pub const MARKER_RESERVE: f32 = 0.25;

/// Map-space stroke width of one thickness unit. `thickness: 1.0` matches
/// the familiar 1-px stroke at the default zoom (40 px per map unit);
/// strokes scale with the map while selection tolerance stays screen-space.
pub const BASE_STROKE_WIDTH: f32 = 0.025;

/// Flat map-space padding added to bounds beyond stroke and arrowhead reach,
/// covering selection tolerance at any supported zoom.
pub const BOUNDS_PAD: f32 = 0.1;

/// Line segments each rounded-corner fillet flattens into for hit-testing.
const FLATTEN_STEPS: usize = 8;

/// Two coordinates closer than this are the same; keeps degenerate legs out
/// of fillet math and orthogonality checks.
pub const EPSILON: f32 = 1e-4;

impl std::ops::Add for MapPoint {
    type Output = MapPoint;
    fn add(self, other: MapPoint) -> MapPoint {
        MapPoint::new(self.x + other.x, self.y + other.y)
    }
}

impl std::ops::Sub for MapPoint {
    type Output = MapPoint;
    fn sub(self, other: MapPoint) -> MapPoint {
        MapPoint::new(self.x - other.x, self.y - other.y)
    }
}

impl MapPoint {
    #[must_use]
    pub fn scale(self, factor: f32) -> MapPoint {
        MapPoint::new(self.x * factor, self.y * factor)
    }

    #[must_use]
    pub fn distance(self, other: MapPoint) -> f32 {
        (other.x - self.x).hypot(other.y - self.y)
    }

    /// The point `t` of the way from `self` toward `to`.
    #[must_use]
    pub fn lerp(self, to: MapPoint, t: f32) -> MapPoint {
        MapPoint::new(self.x + (to.x - self.x) * t, self.y + (to.y - self.y) * t)
    }

    /// Unit vector toward `to`; `None` when the points coincide.
    #[must_use]
    pub fn direction_to(self, to: MapPoint) -> Option<MapPoint> {
        let len = self.distance(to);
        if len < EPSILON {
            None
        } else {
            Some((to - self).scale(1.0 / len))
        }
    }

    #[must_use]
    pub fn nearly_equals(self, other: MapPoint) -> bool {
        (self.x - other.x).abs() < EPSILON && (self.y - other.y).abs() < EPSILON
    }

    #[must_use]
    pub fn is_finite(self) -> bool {
        self.x.is_finite() && self.y.is_finite()
    }
}

/// One endpoint's geometric inputs: the room's center plus the stored wall
/// attachment.
#[derive(Debug, Clone, Copy)]
pub struct EndpointGeometry {
    pub room_center: MapPoint,
    pub side: RoomSide,
    /// `0.0..=1.0` along the wall; horizontal walls run west→east, vertical
    /// walls north→south.
    pub port_offset: f32,
}

/// Everything resolution needs; a distilled borrow of a [`crate::Connection`]
/// plus resolved room centers so callers without a full `Connection`
/// (previews, migrations) can still resolve geometry. The Connection's
/// `segment_shape` is deliberately absent: stored route points already
/// contain every elbow, so shape affects editing/validation
/// ([`orthogonal_violation`]) but never resolution.
#[derive(Debug, Clone, Copy)]
pub struct GeometryInput<'a> {
    pub kind: ConnectionKind,
    pub routing: ConnectionRouting,
    pub corner: CornerStyle,
    pub endpoint_a: EndpointGeometry,
    pub endpoint_b: Option<EndpointGeometry>,
    pub route_points: &'a [MapPoint],
    /// Contract thickness units (`1.0` = standard stroke).
    pub thickness: f32,
}

/// Drawing operations describing the visible stroke. Renderers translate
/// these 1:1 into canvas paths; a `MoveTo` starts each disjoint subpath.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PathPrimitive {
    MoveTo(MapPoint),
    LineTo(MapPoint),
    /// Quadratic fillet toward `to` with the logical corner as control point.
    QuadTo {
        control: MapPoint,
        to: MapPoint,
    },
    /// A full circle (self-loop arc), stroked separately from polylines.
    Circle {
        center: MapPoint,
        radius: f32,
    },
}

/// A draggable editor handle with its map position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Handle {
    /// Endpoint A's port.
    PortA(MapPoint),
    /// Endpoint B's port.
    PortB(MapPoint),
    /// A stored interior route vertex, by index into `route_points`.
    Waypoint(usize, MapPoint),
}

impl Handle {
    #[must_use]
    pub fn position(self) -> MapPoint {
        match self {
            Handle::PortA(p) | Handle::PortB(p) | Handle::Waypoint(_, p) => p,
        }
    }
}

/// Axis-aligned bounds in map units. The empty value is the inverted-infinity
/// sentinel (`min > max`); [`Bounds::is_empty`] names that convention for
/// consumers converting to spatial-index envelopes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl Bounds {
    const EMPTY: Bounds = Bounds {
        min_x: f32::INFINITY,
        min_y: f32::INFINITY,
        max_x: f32::NEG_INFINITY,
        max_y: f32::NEG_INFINITY,
    };

    fn include(&mut self, p: MapPoint) {
        self.min_x = self.min_x.min(p.x);
        self.min_y = self.min_y.min(p.y);
        self.max_x = self.max_x.max(p.x);
        self.max_y = self.max_y.max(p.y);
    }

    fn expand(&mut self, margin: f32) {
        self.min_x -= margin;
        self.min_y -= margin;
        self.max_x += margin;
        self.max_y += margin;
    }

    /// Whether nothing was ever included (or every candidate was non-finite —
    /// `f32::min`/`max` discard NaN, so garbage inputs land here rather than
    /// poisoning the spatial index).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.min_x > self.max_x || self.min_y > self.max_y
    }

    #[must_use]
    pub fn contains(&self, p: MapPoint) -> bool {
        p.x >= self.min_x && p.x <= self.max_x && p.y >= self.min_y && p.y <= self.max_y
    }
}

/// The resolved geometry every consumer shares.
#[derive(Debug, Clone)]
pub struct ConnectionGeometry {
    /// Endpoint A's port on its room wall.
    pub port_a: MapPoint,
    /// Endpoint A's stub tip, [`STUB_LENGTH`] outward from the port.
    pub stub_tip_a: MapPoint,
    pub port_b: Option<MapPoint>,
    pub stub_tip_b: Option<MapPoint>,
    /// The logical polyline the stroke follows when the Connection has one
    /// continuous line — `port A → stub tip A → stored vertices → stub tip B
    /// → port B` for routed kinds, `port → tip → tail` for dangling. Empty
    /// when there is no single line (Stub mode, self-loops, marker kinds).
    pub centerline: Vec<MapPoint>,
    /// Drawing operations (fillets applied) for the visible stroke.
    pub primitives: Vec<PathPrimitive>,
    /// Flattened polylines (fillets subdivided) for hit-testing, collision
    /// checks, and bounds. Disjoint subpaths (e.g. two bare stubs) are
    /// separate entries; circles are carried in [`Self::circles`], not here.
    pub flattened: Vec<Vec<MapPoint>>,
    /// Stroked circles (the self-loop arc), hit-tested and bounded
    /// analytically rather than via a polygon approximation.
    pub circles: Vec<(MapPoint, f32)>,
    /// Unit tangent leaving port A along the path (A→B sense).
    pub start_tangent: MapPoint,
    /// Unit tangent arriving at the far end along the path (A→B sense). For
    /// stub-only and marker kinds this is the direction the line *would*
    /// arrive from — inward through B's wall — not the outward stub stroke.
    pub end_tangent: MapPoint,
    /// Editing handles: ports plus one handle per stored route vertex that
    /// the resolved centerline actually consumed.
    pub handles: Vec<Handle>,
    /// Full expanded bounds: every stroked point and circle, widened by
    /// stroke width, arrowhead reach, and [`BOUNDS_PAD`].
    pub bounds: Bounds,
}

impl ConnectionGeometry {
    /// Distance from `point` to the nearest stroked segment or circle, for
    /// hit-testing. `f32::INFINITY` when there is nothing stroked.
    #[must_use]
    pub fn distance_to(&self, point: MapPoint) -> f32 {
        let mut best = f32::INFINITY;
        for polyline in &self.flattened {
            for pair in polyline.windows(2) {
                best = best.min(distance_to_segment(point, pair[0], pair[1]));
            }
        }
        for &(center, radius) in &self.circles {
            best = best.min((point.distance(center) - radius).abs());
        }
        best
    }

    /// Whether `point` falls within `tolerance` of the stroke.
    #[must_use]
    pub fn hit_test(&self, point: MapPoint, tolerance: f32) -> bool {
        self.bounds.contains(point) && self.distance_to(point) <= tolerance
    }
}

/// The port position for a wall attachment. Horizontal walls run west→east
/// with `offset`, vertical walls north→south.
#[must_use]
pub fn port_position(room_center: MapPoint, side: RoomSide, offset: f32) -> MapPoint {
    let half = ROOM_SIZE / 2.0;
    let along = (offset.clamp(0.0, 1.0) - 0.5) * ROOM_SIZE;
    match side {
        RoomSide::North => MapPoint::new(room_center.x + along, room_center.y - half),
        RoomSide::South => MapPoint::new(room_center.x + along, room_center.y + half),
        RoomSide::East => MapPoint::new(room_center.x + half, room_center.y + along),
        RoomSide::West => MapPoint::new(room_center.x - half, room_center.y + along),
    }
}

/// The stub tip for a port: [`STUB_LENGTH`] outward along the wall normal.
#[must_use]
pub fn stub_tip(port: MapPoint, side: RoomSide) -> MapPoint {
    port + side.outward().scale(STUB_LENGTH)
}

/// Resolves the full geometry for one Connection. Total: every kind/routing
/// combination yields a well-defined (possibly stub-only) result, so callers
/// never branch on validity here — invalid combinations were rejected at
/// mutation time and degrade to their `Simple` look if encountered.
#[must_use]
pub fn resolve(input: &GeometryInput<'_>) -> ConnectionGeometry {
    debug_assert!(
        input.endpoint_a.room_center.is_finite()
            && input.endpoint_a.port_offset.is_finite()
            && input
                .endpoint_b
                .is_none_or(|b| { b.room_center.is_finite() && b.port_offset.is_finite() })
            && input.route_points.iter().all(|p| p.is_finite()),
        "non-finite geometry input escaped boundary validation"
    );

    let port_a = port_position(
        input.endpoint_a.room_center,
        input.endpoint_a.side,
        input.endpoint_a.port_offset,
    );
    let tip_a = stub_tip(port_a, input.endpoint_a.side);
    let (port_b, tip_b) = match input.endpoint_b {
        Some(b) => {
            let port = port_position(b.room_center, b.side, b.port_offset);
            (Some(port), Some(stub_tip(port, b.side)))
        }
        None => (None, None),
    };

    let mut geometry = ConnectionGeometry {
        port_a,
        stub_tip_a: tip_a,
        port_b,
        stub_tip_b: tip_b,
        centerline: Vec::new(),
        primitives: Vec::new(),
        flattened: Vec::new(),
        circles: Vec::new(),
        start_tangent: input.endpoint_a.side.outward(),
        end_tangent: input
            .endpoint_b
            .map_or(input.endpoint_a.side.outward(), |b| {
                b.side.outward().scale(-1.0)
            }),
        handles: Vec::new(),
        bounds: Bounds::EMPTY,
    };

    resolve_stroke(&mut geometry, input, port_a, tip_a, port_b, tip_b);

    geometry.handles.push(Handle::PortA(port_a));
    if let Some(port) = port_b {
        geometry.handles.push(Handle::PortB(port));
    }
    // Waypoint handles exist exactly for the vertices the centerline
    // consumed; a routed Connection whose endpoint B is gone draws only a
    // stub, so its dormant stored points get no floating handles.
    if !geometry.centerline.is_empty()
        && matches!(
            input.routing,
            ConnectionRouting::Manual | ConnectionRouting::Automatic
        )
    {
        for (index, &p) in input.route_points.iter().enumerate() {
            geometry.handles.push(Handle::Waypoint(index, p));
        }
    }

    finalize_tangents(&mut geometry);
    finalize_bounds(&mut geometry, input, port_a, tip_a, tip_b);
    geometry
}

/// The orthogonality rule for stored routes: with `Orthogonal` segment
/// shape, every consecutive centerline pair — stub tip → first vertex
/// through last vertex → stub tip — must be axis-aligned. Returns the index
/// of the first offending segment (0 = `tip_a → points[0]`), or `None` when
/// valid. Enforced at mutation time on both sides of the wire; the renderer
/// applies no correction.
#[must_use]
pub fn orthogonal_violation(
    tip_a: MapPoint,
    points: &[MapPoint],
    tip_b: MapPoint,
) -> Option<usize> {
    let mut prev = tip_a;
    for (index, &p) in points.iter().chain(std::iter::once(&tip_b)).enumerate() {
        let dx = (p.x - prev.x).abs();
        let dy = (p.y - prev.y).abs();
        if dx >= EPSILON && dy >= EPSILON {
            return Some(index);
        }
        prev = p;
    }
    None
}

/// Drops consecutive (near-)duplicate points — the stored-route
/// normalization both sides of the wire apply before validation.
#[must_use]
pub fn dedup(points: &[MapPoint]) -> Vec<MapPoint> {
    let mut out: Vec<MapPoint> = Vec::with_capacity(points.len());
    for &p in points {
        if out.last().is_none_or(|&prev| !prev.nearly_equals(p)) {
            out.push(p);
        }
    }
    out
}

/// Converts an arbitrary stored route into an explicit orthogonal route that
/// visits the authored points in order. Every diagonal leg receives a
/// deterministic horizontal-then-vertical elbow, and redundant collinear
/// vertices are removed. The returned points remain interior (the two stub
/// tips are never stored).
///
/// Returns `None` if normalization would exceed the wire-format route-point
/// limit. Callers can then leave the Direct route untouched instead of
/// submitting a mutation the backend must reject.
#[must_use]
pub fn orthogonalize_route(
    tip_a: MapPoint,
    points: &[MapPoint],
    tip_b: MapPoint,
) -> Option<Vec<MapPoint>> {
    fn push_compacted(path: &mut Vec<MapPoint>, point: MapPoint) {
        if path.last().is_some_and(|last| last.nearly_equals(point)) {
            return;
        }
        if path.len() >= 2 {
            let a = path[path.len() - 2];
            let b = path[path.len() - 1];
            let same_x = (a.x - b.x).abs() < EPSILON && (b.x - point.x).abs() < EPSILON;
            let same_y = (a.y - b.y).abs() < EPSILON && (b.y - point.y).abs() < EPSILON;
            if same_x || same_y {
                if let Some(last) = path.last_mut() {
                    *last = point;
                }
                return;
            }
        }
        path.push(point);
    }

    let mut path = Vec::with_capacity(points.len().saturating_mul(2).saturating_add(2));
    path.push(tip_a);
    for target in points.iter().copied().chain(std::iter::once(tip_b)) {
        let &current = path.last()?;
        if (current.x - target.x).abs() >= EPSILON && (current.y - target.y).abs() >= EPSILON {
            push_compacted(&mut path, MapPoint::new(target.x, current.y));
        }
        push_compacted(&mut path, target);
        if path.len().saturating_sub(2) > crate::MAX_ROUTE_POINTS {
            return None;
        }
    }

    if path.last().is_none_or(|last| !last.nearly_equals(tip_b)) {
        return None;
    }
    path.remove(0);
    path.pop();
    (path.len() <= crate::MAX_ROUTE_POINTS).then_some(path)
}

/// Distance from `point` to the segment `a..b`.
#[must_use]
pub fn distance_to_segment(point: MapPoint, a: MapPoint, b: MapPoint) -> f32 {
    let ab = b - a;
    let len_sq = ab.x * ab.x + ab.y * ab.y;
    if len_sq < EPSILON * EPSILON {
        return point.distance(a);
    }
    let t = ((point.x - a.x) * ab.x + (point.y - a.y) * ab.y) / len_sq;
    point.distance(a + ab.scale(t.clamp(0.0, 1.0)))
}

/// Fills primitives/flattened/circles/centerline for the kind × routing
/// combination.
fn resolve_stroke(
    geometry: &mut ConnectionGeometry,
    input: &GeometryInput<'_>,
    port_a: MapPoint,
    tip_a: MapPoint,
    port_b: Option<MapPoint>,
    tip_b: Option<MapPoint>,
) {
    match (input.kind, input.routing) {
        // Bare wall stubs, middle hidden — Stub mode for every kind, and the
        // stub half of the marker kinds whose middle glyph the renderer owns
        // (markers anchor on the exposed stub tips; nothing re-derives them).
        (_, ConnectionRouting::Stub)
        | (ConnectionKind::External | ConnectionKind::CrossLevel, _) => {
            push_polyline(geometry, &[port_a, tip_a]);
            if let (Some(port), Some(tip)) = (port_b, tip_b) {
                push_polyline(geometry, &[port, tip]);
            }
        }
        (ConnectionKind::SelfLoop, _) => {
            resolve_self_loop(geometry, input, port_a, port_b);
        }
        (ConnectionKind::Dangling, _) => {
            let tail = tip_a + input.endpoint_a.side.outward().scale(DANGLING_TAIL_LENGTH);
            let line = dedup(&[port_a, tip_a, tail]);
            push_polyline(geometry, &line);
            geometry.centerline = line;
        }
        (ConnectionKind::Internal, ConnectionRouting::Simple) => {
            if let (Some(port), Some(tip)) = (port_b, tip_b) {
                let line = dedup(&[port_a, tip_a, tip, port]);
                push_path(geometry, &line, CornerStyle::Sharp);
                geometry.centerline = line;
            } else {
                push_polyline(geometry, &[port_a, tip_a]);
            }
        }
        (ConnectionKind::Internal, ConnectionRouting::Manual | ConnectionRouting::Automatic) => {
            if let (Some(port), Some(tip)) = (port_b, tip_b) {
                let mut line = Vec::with_capacity(input.route_points.len() + 4);
                line.push(port_a);
                line.push(tip_a);
                line.extend_from_slice(input.route_points);
                line.push(tip);
                line.push(port);
                let line = dedup(&line);
                push_path(geometry, &line, input.corner);
                geometry.centerline = line;
            } else {
                push_polyline(geometry, &[port_a, tip_a]);
            }
        }
    }
}

/// Reads the A→B tangents off the centerline's first/last legs. Only a
/// continuous centerline overrides the endpoint-derived defaults: disjoint
/// stub subpaths and self-loop circles have no meaningful along-path
/// direction, and their B-side strokes run outward — the opposite of the
/// documented "arriving at the far end" sense.
fn finalize_tangents(geometry: &mut ConnectionGeometry) {
    if geometry.centerline.len() < 2 {
        return;
    }
    let line = &geometry.centerline;
    if let Some(t) = line[0].direction_to(line[1]) {
        geometry.start_tangent = t;
    }
    if let Some(t) = line[line.len() - 2].direction_to(line[line.len() - 1]) {
        geometry.end_tangent = t;
    }
}

/// Folds every stroked point and circle into bounds, reserves marker glyph
/// space past both stub tips for the marker kinds, and expands by stroke,
/// arrowhead, and selection padding.
fn finalize_bounds(
    geometry: &mut ConnectionGeometry,
    input: &GeometryInput<'_>,
    port_a: MapPoint,
    tip_a: MapPoint,
    tip_b: Option<MapPoint>,
) {
    for polyline in &geometry.flattened {
        for &p in polyline {
            geometry.bounds.include(p);
        }
    }
    for &(center, radius) in &geometry.circles {
        geometry
            .bounds
            .include(MapPoint::new(center.x - radius, center.y - radius));
        geometry
            .bounds
            .include(MapPoint::new(center.x + radius, center.y + radius));
    }
    if matches!(
        input.kind,
        ConnectionKind::External | ConnectionKind::CrossLevel
    ) {
        geometry
            .bounds
            .include(tip_a + input.endpoint_a.side.outward().scale(MARKER_RESERVE));
        if let (Some(tip), Some(b)) = (tip_b, input.endpoint_b) {
            geometry
                .bounds
                .include(tip + b.side.outward().scale(MARKER_RESERVE));
        }
    }
    // Reachable only when every candidate point was non-finite (NaN falls
    // out of f32::min/max): anchor on the port so the entry stays findable.
    if geometry.bounds.is_empty() {
        geometry.bounds.include(port_a);
    }
    geometry
        .bounds
        .expand(input.thickness.max(0.0) * BASE_STROKE_WIDTH / 2.0 + ARROW_SIZE + BOUNDS_PAD);
}

/// Appends a sharp polyline as primitives + one flattened subpath.
fn push_polyline(geometry: &mut ConnectionGeometry, points: &[MapPoint]) {
    let points = dedup(points);
    if points.len() < 2 {
        return;
    }
    geometry.primitives.push(PathPrimitive::MoveTo(points[0]));
    for &p in &points[1..] {
        geometry.primitives.push(PathPrimitive::LineTo(p));
    }
    geometry.flattened.push(points);
}

/// Appends a full path with the corner treatment: `Sharp` emits the polyline
/// as-is; `Rounded` replaces each interior corner with a quadratic fillet of
/// [`CORNER_RADIUS`] clamped to half the shorter adjacent leg, and flattens
/// each fillet into [`FLATTEN_STEPS`] segments for hit-testing.
fn push_path(geometry: &mut ConnectionGeometry, points: &[MapPoint], corner: CornerStyle) {
    if points.len() < 2 {
        return;
    }
    if corner == CornerStyle::Sharp || points.len() == 2 {
        push_polyline(geometry, points);
        return;
    }

    let mut flattened: Vec<MapPoint> = vec![points[0]];
    geometry.primitives.push(PathPrimitive::MoveTo(points[0]));
    for i in 1..points.len() - 1 {
        let (a, corner_pt, b) = (points[i - 1], points[i], points[i + 1]);
        let len_in = a.distance(corner_pt);
        let len_out = corner_pt.distance(b);
        let radius = CORNER_RADIUS.min(len_in / 2.0).min(len_out / 2.0);
        if radius < EPSILON || len_in < EPSILON || len_out < EPSILON {
            geometry.primitives.push(PathPrimitive::LineTo(corner_pt));
            flattened.push(corner_pt);
            continue;
        }
        let entry = corner_pt.lerp(a, radius / len_in);
        let exit = corner_pt.lerp(b, radius / len_out);
        geometry.primitives.push(PathPrimitive::LineTo(entry));
        geometry.primitives.push(PathPrimitive::QuadTo {
            control: corner_pt,
            to: exit,
        });
        flattened.push(entry);
        #[allow(clippy::cast_precision_loss)]
        for step in 1..=FLATTEN_STEPS {
            let t = step as f32 / FLATTEN_STEPS as f32;
            // De Casteljau on (entry, corner, exit).
            let q0 = entry.lerp(corner_pt, t);
            let q1 = corner_pt.lerp(exit, t);
            flattened.push(q0.lerp(q1, t));
        }
    }
    let last = points[points.len() - 1];
    geometry.primitives.push(PathPrimitive::LineTo(last));
    flattened.push(last);
    geometry.flattened.push(dedup(&flattened));
}

/// The self-loop arc: a circle bulging outward, centered off the midpoint of
/// the loop's port(s) along the averaged outward normals. `Stub` routing is
/// handled by the caller; both `Simple` faces land here.
fn resolve_self_loop(
    geometry: &mut ConnectionGeometry,
    input: &GeometryInput<'_>,
    port_a: MapPoint,
    port_b: Option<MapPoint>,
) {
    let normal_a = input.endpoint_a.side.outward();
    let (anchor, bulge) = match (port_b, input.endpoint_b) {
        (Some(port), Some(b)) => {
            let combined = normal_a + b.side.outward();
            let len = (combined.x * combined.x + combined.y * combined.y).sqrt();
            let bulge = if len < EPSILON {
                // Opposite walls cancel; bulge along A's normal.
                normal_a
            } else {
                combined.scale(1.0 / len)
            };
            (port_a.lerp(port, 0.5), bulge)
        }
        _ => (port_a, normal_a),
    };
    let center = anchor + bulge.scale(SELF_LOOP_RADIUS);
    geometry.primitives.push(PathPrimitive::Circle {
        center,
        radius: SELF_LOOP_RADIUS,
    });
    geometry.circles.push((center, SELF_LOOP_RADIUS));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(x: f32, y: f32, side: RoomSide, offset: f32) -> EndpointGeometry {
        EndpointGeometry {
            room_center: MapPoint::new(x, y),
            side,
            port_offset: offset,
        }
    }

    fn internal_input(
        routing: ConnectionRouting,
        corner: CornerStyle,
        route_points: &[MapPoint],
    ) -> GeometryInput<'_> {
        GeometryInput {
            kind: ConnectionKind::Internal,
            routing,
            corner,
            endpoint_a: endpoint(0.0, 0.0, RoomSide::East, 0.5),
            endpoint_b: Some(endpoint(4.0, 0.0, RoomSide::West, 0.5)),
            route_points,
            thickness: 1.0,
        }
    }

    #[test]
    fn ports_land_on_walls_with_directed_offsets() {
        let center = MapPoint::new(1.0, 1.0);
        // North wall runs west→east: offset 0 is the west corner.
        assert!(
            port_position(center, RoomSide::North, 0.0).nearly_equals(MapPoint::new(0.75, 0.75))
        );
        assert!(
            port_position(center, RoomSide::North, 1.0).nearly_equals(MapPoint::new(1.25, 0.75))
        );
        assert!(
            port_position(center, RoomSide::South, 0.5).nearly_equals(MapPoint::new(1.0, 1.25))
        );
        // East wall runs north→south.
        assert!(
            port_position(center, RoomSide::East, 0.0).nearly_equals(MapPoint::new(1.25, 0.75))
        );
        assert!(
            port_position(center, RoomSide::West, 1.0).nearly_equals(MapPoint::new(0.75, 1.25))
        );
    }

    #[test]
    fn stub_tips_reach_the_standard_distance_from_center() {
        // 0.25 half-room + 0.15 stub = 0.4 from room center.
        let center = MapPoint::new(0.0, 0.0);
        let port = port_position(center, RoomSide::North, 0.5);
        let tip = stub_tip(port, RoomSide::North);
        assert!(tip.nearly_equals(MapPoint::new(0.0, -0.4)));
    }

    #[test]
    fn simple_runs_port_stub_stub_port() {
        let input = internal_input(ConnectionRouting::Simple, CornerStyle::Sharp, &[]);
        let g = resolve(&input);
        assert_eq!(g.flattened.len(), 1);
        let line = &g.flattened[0];
        assert!(line[0].nearly_equals(MapPoint::new(0.25, 0.0)));
        assert!(line[1].nearly_equals(MapPoint::new(0.4, 0.0)));
        assert!(line[line.len() - 2].nearly_equals(MapPoint::new(3.6, 0.0)));
        assert!(line[line.len() - 1].nearly_equals(MapPoint::new(3.75, 0.0)));
        assert!(g.start_tangent.nearly_equals(MapPoint::new(1.0, 0.0)));
        assert!(g.end_tangent.nearly_equals(MapPoint::new(1.0, 0.0)));
    }

    #[test]
    fn stub_mode_emits_two_disjoint_stubs_and_keeps_arrival_tangent() {
        let input = internal_input(ConnectionRouting::Stub, CornerStyle::Sharp, &[]);
        let g = resolve(&input);
        assert_eq!(g.flattened.len(), 2);
        assert!(g.centerline.is_empty());
        assert_eq!(g.handles.len(), 2);
        // The end tangent stays the "arriving through B's wall" direction
        // (+x toward a West wall), not the outward B-stub stroke direction.
        assert!(g.end_tangent.nearly_equals(MapPoint::new(1.0, 0.0)));
    }

    #[test]
    fn manual_route_stores_every_vertex_and_handles_them() {
        let route = [
            MapPoint::new(1.0, 0.0),
            MapPoint::new(1.0, 2.0),
            MapPoint::new(3.0, 2.0),
        ];
        let input = internal_input(ConnectionRouting::Manual, CornerStyle::Sharp, &route);
        let g = resolve(&input);
        // port, tip, 3 stored, tip, port
        assert_eq!(g.centerline.len(), 7);
        let waypoints: Vec<_> = g
            .handles
            .iter()
            .filter(|h| matches!(h, Handle::Waypoint(..)))
            .collect();
        assert_eq!(waypoints.len(), 3);
    }

    #[test]
    fn routed_connection_without_endpoint_b_gets_no_floating_handles() {
        let route = [MapPoint::new(1.0, 0.0), MapPoint::new(1.0, 2.0)];
        let mut input = internal_input(ConnectionRouting::Manual, CornerStyle::Sharp, &route);
        input.endpoint_b = None;
        let g = resolve(&input);
        assert!(g.centerline.is_empty());
        assert!(
            g.handles.iter().all(|h| !matches!(h, Handle::Waypoint(..))),
            "dormant stored points must not grow draggable handles"
        );
    }

    #[test]
    fn fillet_radius_clamps_to_half_the_shorter_leg() {
        // A corner with a 0.2 leg out: the fillet must use radius 0.1 (half
        // the short leg), so the fillet entry sits exactly 0.1 back along
        // the incoming leg and the exit 0.1 along the outgoing leg.
        let route = [
            MapPoint::new(2.0, 0.0),
            MapPoint::new(2.0, 0.2),
            MapPoint::new(3.0, 0.2),
        ];
        let input = internal_input(ConnectionRouting::Manual, CornerStyle::Rounded, &route);
        let g = resolve(&input);
        let quads: Vec<_> = g
            .primitives
            .iter()
            .filter_map(|p| match p {
                PathPrimitive::QuadTo { control, to } => Some((*control, *to)),
                _ => None,
            })
            .collect();
        // The corner at (2.0, 0.2) has legs 0.2 (in) and 1.0 (out).
        let (control, exit) = quads
            .iter()
            .find(|(c, _)| c.nearly_equals(MapPoint::new(2.0, 0.2)))
            .copied()
            .expect("short-leg corner produces a fillet");
        assert!(control.nearly_equals(MapPoint::new(2.0, 0.2)));
        assert!(
            exit.nearly_equals(MapPoint::new(2.1, 0.2)),
            "exit must sit exactly radius=0.1 along the outgoing leg, got {exit:?}"
        );
    }

    #[test]
    fn rounded_corners_flatten_and_stay_near_the_logical_corner() {
        let route = [MapPoint::new(2.0, 0.0), MapPoint::new(2.0, 2.0)];
        let input = internal_input(ConnectionRouting::Manual, CornerStyle::Rounded, &route);
        let g = resolve(&input);
        let sharp_count = g.centerline.len();
        let flat = &g.flattened[0];
        assert!(flat.len() > sharp_count, "fillets subdivide the corner");
        let corner = MapPoint::new(2.0, 0.0);
        let near_corner: Vec<_> = flat
            .iter()
            .filter(|p| p.distance(corner) <= CORNER_RADIUS + EPSILON)
            .collect();
        assert!(!near_corner.is_empty());
    }

    #[test]
    fn bounds_cover_a_route_far_from_both_rooms() {
        // Rooms at y=0, route dips to y=6: bounds must include the middle
        // even though both endpoint rooms are near the origin, so a viewport
        // crossing only the middle still finds the Connection.
        let route = [MapPoint::new(1.0, 6.0), MapPoint::new(3.0, 6.0)];
        let input = internal_input(ConnectionRouting::Manual, CornerStyle::Sharp, &route);
        let g = resolve(&input);
        assert!(g.bounds.max_y >= 6.0);
        assert!(g.hit_test(MapPoint::new(2.0, 6.0), 0.05));
        assert!(!g.hit_test(MapPoint::new(2.0, 3.0), 0.05));
    }

    #[test]
    fn orthogonality_flags_the_offending_segment() {
        let tip_a = MapPoint::new(0.4, 0.0);
        let tip_b = MapPoint::new(3.6, 2.0);
        assert_eq!(
            orthogonal_violation(
                tip_a,
                &[MapPoint::new(2.0, 0.0), MapPoint::new(2.0, 2.0)],
                tip_b
            ),
            None
        );
        assert_eq!(
            orthogonal_violation(tip_a, &[MapPoint::new(2.0, 1.0)], tip_b),
            Some(0)
        );
    }

    #[test]
    fn direct_points_normalize_to_explicit_orthogonal_elbows() {
        let tip_a = MapPoint::new(0.0, 0.0);
        let tip_b = MapPoint::new(4.0, 3.0);
        let points = [MapPoint::new(1.0, 2.0), MapPoint::new(3.0, 1.0)];
        let normalized = orthogonalize_route(tip_a, &points, tip_b).expect("within limit");

        assert_eq!(orthogonal_violation(tip_a, &normalized, tip_b), None);
        assert!(normalized.contains(&points[0]));
        assert!(normalized.contains(&points[1]));
        assert!(normalized.len() <= points.len() * 2 + 1);
    }

    #[test]
    fn dangling_gets_a_directional_tail_with_centerline() {
        let input = GeometryInput {
            kind: ConnectionKind::Dangling,
            routing: ConnectionRouting::Simple,
            corner: CornerStyle::Sharp,
            endpoint_a: endpoint(0.0, 0.0, RoomSide::North, 0.5),
            endpoint_b: None,
            route_points: &[],
            thickness: 1.0,
        };
        let g = resolve(&input);
        let line = &g.flattened[0];
        // Port → tip → tail end, 0.65 above center.
        assert!(line[line.len() - 1].nearly_equals(MapPoint::new(0.0, -0.65)));
        assert!(g.end_tangent.nearly_equals(MapPoint::new(0.0, -1.0)));
        // The single continuous stroke is a real centerline.
        assert_eq!(g.centerline.len(), 3);
    }

    #[test]
    fn self_loop_resolves_analytically_with_wall_normal_tangents() {
        let input = GeometryInput {
            kind: ConnectionKind::SelfLoop,
            routing: ConnectionRouting::Simple,
            corner: CornerStyle::Sharp,
            endpoint_a: endpoint(0.0, 0.0, RoomSide::North, 0.4),
            endpoint_b: Some(endpoint(0.0, 0.0, RoomSide::North, 0.6)),
            route_points: &[],
            thickness: 1.0,
        };
        let g = resolve(&input);
        assert!(matches!(g.primitives[0], PathPrimitive::Circle { .. }));
        assert_eq!(g.circles.len(), 1);
        assert!(g.bounds.min_y < -0.25, "loop bulges outside the wall");
        // Tangents keep their wall-normal defaults; the circle has no
        // along-path direction to override them with.
        assert!(g.start_tangent.nearly_equals(MapPoint::new(0.0, -1.0)));
        // Analytic ring hit-test: a point on the circle hits, its center
        // does not.
        let (center, radius) = g.circles[0];
        assert!(g.hit_test(MapPoint::new(center.x, center.y - radius), 0.02));
        assert!(!g.hit_test(center, 0.02));
    }

    #[test]
    fn cross_level_reserves_marker_space_at_both_tips() {
        // B attaches on its room's North wall: tip_b = (3.0, -0.4), so its
        // marker reservation extends to y = -0.65 — outside the hull of every
        // stroked point (the deepest stroke reaches only y = -0.4). Without
        // the B-side reservation, bounds stop at -0.4 - expansion = -0.6.
        let input = GeometryInput {
            kind: ConnectionKind::CrossLevel,
            routing: ConnectionRouting::Simple,
            corner: CornerStyle::Sharp,
            endpoint_a: endpoint(0.0, 0.0, RoomSide::East, 0.5),
            endpoint_b: Some(endpoint(3.0, 0.0, RoomSide::North, 0.5)),
            route_points: &[],
            thickness: 1.0,
        };
        let g = resolve(&input);
        assert!(
            g.bounds.min_y <= -0.65,
            "B-side marker reservation missing: min_y = {}",
            g.bounds.min_y
        );
        // Port B at (3.0, -0.25) is covered, and the A-side reservation
        // (tip_a = (0.4, 0), East outward to x = 0.65) is present.
        assert!(g.bounds.max_x >= 3.0 - EPSILON, "port B covered");
        assert!(g.bounds.max_x >= 0.65, "A-side marker reservation present");
    }

    #[test]
    fn degenerate_duplicate_points_are_dropped() {
        let p = MapPoint::new(1.0, 1.0);
        assert_eq!(dedup(&[p, p, p]).len(), 1);
        let route = [MapPoint::new(1.0, 0.0), MapPoint::new(1.0, 0.0)];
        let input = internal_input(ConnectionRouting::Manual, CornerStyle::Rounded, &route);
        // Must not panic on zero-length legs.
        let _ = resolve(&input);
    }
}
