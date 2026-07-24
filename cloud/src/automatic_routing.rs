//! Deterministic, bounded Automatic Connection routing.
//!
//! The solver owns no map/cache state. Callers provide the public room
//! projection as obstacles and an immutable endpoint snapshot, then run
//! [`solve`] off their UI thread. Accepted points are stored by the caller;
//! this module is never used as a render-time layout engine.

use std::{
    cmp::Ordering,
    collections::{BinaryHeap, HashMap},
    sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
    time::{Duration, Instant},
};

use rstar::{AABB, RTree, RTreeObject};

use crate::{
    ConnectionKind, ConnectionRouting, CornerStyle, MAX_COORDINATE, MAX_ROUTE_POINTS, MapPoint,
    RoomNumber, RoomSide,
    connection_geometry::{
        ARROW_SIZE, BASE_STROKE_WIDTH, EndpointGeometry, GeometryInput, ROOM_SIZE,
        orthogonal_violation, port_position, resolve, stub_tip,
    },
};

/// One lane step is half a map cell.
pub const LANE_STEP: f64 = 0.5;
/// Cost of advancing one half-cell lane.
pub const STEP_COST: u32 = 10;
/// Cost added whenever the path bends.
pub const TURN_COST: u32 = 40;
/// Cost for each cardinally adjacent blocked lane.
pub const OBSTACLE_ADJACENCY_COST: u32 = 1;
/// Public room rectangles are inflated by this route clearance in addition
/// to half the visible stroke width.
pub const ROUTE_PADDING: f64 = 0.25;
/// Successive endpoint-bounds expansion attempts, in map units.
pub const SEARCH_MARGINS: [f64; 3] = [8.0, 16.0, 32.0];
/// Deterministic work cap shared across all expansion attempts.
pub const MAX_VISITED_STATES: usize = 100_000;
/// Responsiveness guard for a production solve.
pub const WALL_TIME_LIMIT: Duration = Duration::from_millis(100);

/// An axis-aligned map-space rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteRect {
    pub min_x: f64,
    pub min_y: f64,
    pub max_x: f64,
    pub max_y: f64,
}

impl RouteRect {
    #[must_use]
    pub fn from_center(center: MapPoint, half_width: f64, half_height: f64) -> Self {
        Self {
            min_x: f64::from(center.x) - half_width,
            min_y: f64::from(center.y) - half_height,
            max_x: f64::from(center.x) + half_width,
            max_y: f64::from(center.y) + half_height,
        }
    }

    fn inflate(self, amount: f64) -> Self {
        Self {
            min_x: self.min_x - amount,
            min_y: self.min_y - amount,
            max_x: self.max_x + amount,
            max_y: self.max_y + amount,
        }
    }

    fn contains(self, point: GridPoint) -> bool {
        let (x, y) = point.map_coordinates();
        x >= self.min_x && x <= self.max_x && y >= self.min_y && y <= self.max_y
    }

    fn intersects_segment(self, a: MapPoint, b: MapPoint) -> bool {
        let (ax, ay) = (f64::from(a.x), f64::from(a.y));
        let (bx, by) = (f64::from(b.x), f64::from(b.y));
        let mut enter = 0.0_f64;
        let mut exit = 1.0_f64;
        for (start, delta, min, max) in [
            (ax, bx - ax, self.min_x, self.max_x),
            (ay, by - ay, self.min_y, self.max_y),
        ] {
            if delta.abs() <= f64::EPSILON {
                if start < min || start > max {
                    return false;
                }
                continue;
            }
            let mut axis_enter = (min - start) / delta;
            let mut axis_exit = (max - start) / delta;
            if axis_enter > axis_exit {
                std::mem::swap(&mut axis_enter, &mut axis_exit);
            }
            enter = enter.max(axis_enter);
            exit = exit.min(axis_exit);
            if enter > exit {
                return false;
            }
        }
        true
    }
}

/// One room in the public obstacle projection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteObstacle {
    pub room_number: RoomNumber,
    pub bounds: RouteRect,
}

/// One endpoint's immutable solve geometry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RouteEndpoint {
    pub room_number: RoomNumber,
    pub room_center: MapPoint,
    pub side: RoomSide,
    pub port_offset: f32,
}

impl RouteEndpoint {
    #[must_use]
    pub fn tip(self) -> MapPoint {
        stub_tip(
            port_position(self.room_center, self.side, self.port_offset),
            self.side,
        )
    }

    fn room_bounds(self) -> RouteRect {
        RouteRect::from_center(
            self.room_center,
            f64::from(ROOM_SIZE) / 2.0,
            f64::from(ROOM_SIZE) / 2.0,
        )
    }

    const fn geometry(self) -> EndpointGeometry {
        EndpointGeometry {
            room_center: self.room_center,
            side: self.side,
            port_offset: self.port_offset,
        }
    }
}

/// Owned input suitable for `spawn_blocking`.
#[derive(Debug, Clone)]
pub struct AutoRouteRequest {
    pub endpoint_a: RouteEndpoint,
    pub endpoint_b: RouteEndpoint,
    /// Public rooms on the Connection's level. Endpoint rooms must be present
    /// even when secret so only their explicit escape lanes are usable.
    pub obstacles: Vec<RouteObstacle>,
    /// Dimensionless Connection thickness multiplier (`1.0` is the standard
    /// visible stroke width).
    pub thickness: f32,
    /// The accepted route is resolved with this corner treatment before the
    /// preview is offered, so collision validation uses visible geometry.
    pub corner: CornerStyle,
}

/// Terminal outcome. Failure never carries partial route points.
#[derive(Debug, Clone, PartialEq)]
pub enum AutoRouteResult {
    Solved {
        route_points: Vec<MapPoint>,
        visited_states: usize,
    },
    NoRoute,
    LimitReached,
    Cancelled,
}

/// Validation result for a stored or preview route against the same public
/// obstacle snapshot used by [`solve`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteValidation {
    Valid,
    Invalid,
    Collision,
}

#[derive(Debug, Clone, Copy)]
struct SolveLimits {
    max_states: usize,
    wall_time: Option<Duration>,
}

impl Default for SolveLimits {
    fn default() -> Self {
        Self {
            max_states: MAX_VISITED_STATES,
            wall_time: Some(WALL_TIME_LIMIT),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
enum Direction {
    North,
    East,
    South,
    West,
}

impl Direction {
    const ALL: [Self; 4] = [Self::North, Self::East, Self::South, Self::West];

    const fn delta(self) -> (i32, i32) {
        match self {
            Self::North => (0, -1),
            Self::East => (1, 0),
            Self::South => (0, 1),
            Self::West => (-1, 0),
        }
    }

    const fn from_side(side: RoomSide) -> Self {
        match side {
            RoomSide::North => Self::North,
            RoomSide::East => Self::East,
            RoomSide::South => Self::South,
            RoomSide::West => Self::West,
        }
    }

    fn between(a: MapPoint, b: MapPoint) -> Option<Self> {
        let dx = f64::from(b.x) - f64::from(a.x);
        let dy = f64::from(b.y) - f64::from(a.y);
        if dx.abs() > f64::EPSILON && dy.abs() <= f64::EPSILON {
            Some(if dx > 0.0 { Self::East } else { Self::West })
        } else if dy.abs() > f64::EPSILON && dx.abs() <= f64::EPSILON {
            Some(if dy > 0.0 { Self::South } else { Self::North })
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct GridPoint {
    x: i32,
    y: i32,
}

impl GridPoint {
    const fn moved(self, direction: Direction) -> Self {
        let (dx, dy) = direction.delta();
        Self {
            x: self.x + dx,
            y: self.y + dy,
        }
    }

    fn map_coordinates(self) -> (f64, f64) {
        (f64::from(self.x) * LANE_STEP, f64::from(self.y) * LANE_STEP)
    }

    #[allow(clippy::cast_possible_truncation)]
    fn from_map(point: MapPoint) -> Self {
        Self {
            x: (f64::from(point.x) / LANE_STEP).round() as i32,
            y: (f64::from(point.y) / LANE_STEP).round() as i32,
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn to_map(self) -> MapPoint {
        MapPoint::new(
            (f64::from(self.x) * LANE_STEP) as f32,
            (f64::from(self.y) * LANE_STEP) as f32,
        )
    }

    fn manhattan(self, other: Self) -> u32 {
        self.x.abs_diff(other.x) + self.y.abs_diff(other.y)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct State {
    point: GridPoint,
    entry: Direction,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HeapEntry {
    state: State,
    f_cost: u32,
    g_cost: u32,
    bend_count: u32,
}

impl Ord for HeapEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        // BinaryHeap is a max-heap; reverse the contract's ascending tie key.
        (
            other.f_cost,
            other.bend_count,
            other.state.point.y,
            other.state.point.x,
            other.state.entry,
        )
            .cmp(&(
                self.f_cost,
                self.bend_count,
                self.state.point.y,
                self.state.point.x,
                self.state.entry,
            ))
    }
}

impl PartialOrd for HeapEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone)]
struct IndexedObstacle {
    obstacle: RouteObstacle,
    inflated: RouteRect,
    envelope: AABB<[f64; 2]>,
}

impl RTreeObject for IndexedObstacle {
    type Envelope = AABB<[f64; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.envelope
    }
}

#[derive(Debug)]
enum SearchOutcome {
    Found(Vec<GridPoint>),
    Exhausted,
    Limited,
    Cancelled,
}

/// Solves with the production limits. Check `cancel` whenever a newer edit or
/// solve supersedes this immutable request.
#[must_use]
pub fn solve(request: &AutoRouteRequest, cancel: &AtomicBool) -> AutoRouteResult {
    solve_with_limits(request, cancel, SolveLimits::default())
}

fn solve_with_limits(
    request: &AutoRouteRequest,
    cancel: &AtomicBool,
    limits: SolveLimits,
) -> AutoRouteResult {
    if !valid_request(request) {
        return AutoRouteResult::NoRoute;
    }
    let started = Instant::now();
    let clearance = f64::from(request.thickness.max(0.0) * BASE_STROKE_WIDTH / 2.0) + ROUTE_PADDING;
    let indexed = request
        .obstacles
        .iter()
        .copied()
        .map(|obstacle| {
            let inflated = obstacle.bounds.inflate(clearance);
            IndexedObstacle {
                obstacle,
                inflated,
                envelope: AABB::from_corners(
                    [inflated.min_x, inflated.min_y],
                    [inflated.max_x, inflated.max_y],
                ),
            }
        })
        .collect();
    let obstacle_index = RTree::bulk_load(indexed);
    let connector_a = escape_connector(request.endpoint_a, clearance);
    let connector_b = escape_connector(request.endpoint_b, clearance);
    let start = GridPoint::from_map(*connector_a.last().expect("connector has a lane"));
    let goal = GridPoint::from_map(*connector_b.last().expect("connector has a lane"));
    let start_entry = connector_a
        .windows(2)
        .last()
        .and_then(|pair| Direction::between(pair[0], pair[1]))
        .unwrap_or_else(|| Direction::from_side(request.endpoint_a.side));
    let goal_exit = connector_b
        .windows(2)
        .last()
        .and_then(|pair| Direction::between(pair[1], pair[0]))
        .unwrap_or_else(|| Direction::from_side(request.endpoint_b.side));

    let mut visited_states = 0;
    let mut route_point_limit_hit = false;
    for margin in SEARCH_MARGINS {
        if cancelled_or_timed_out(cancel, started, limits.wall_time) {
            return if cancel.load(AtomicOrdering::Relaxed) {
                AutoRouteResult::Cancelled
            } else {
                AutoRouteResult::LimitReached
            };
        }
        let bounds = search_bounds(request.endpoint_a.tip(), request.endpoint_b.tip(), margin);
        let envelope =
            AABB::from_corners([bounds.min_x, bounds.min_y], [bounds.max_x, bounds.max_y]);
        let obstacles: Vec<_> = obstacle_index
            .locate_in_envelope_intersecting(&envelope)
            .cloned()
            .collect();
        match search_attempt(
            start,
            goal,
            start_entry,
            goal_exit,
            bounds,
            &obstacles,
            &mut visited_states,
            limits,
            started,
            cancel,
        ) {
            SearchOutcome::Found(grid_path) => {
                let points = assemble_route(&connector_a, &grid_path, &connector_b);
                if points.len() > MAX_ROUTE_POINTS {
                    route_point_limit_hit = true;
                    continue;
                }
                if validate_route_with_index(request, &points, &obstacle_index)
                    != RouteValidation::Valid
                {
                    continue;
                }
                return AutoRouteResult::Solved {
                    route_points: points,
                    visited_states,
                };
            }
            SearchOutcome::Exhausted => {}
            SearchOutcome::Limited => return AutoRouteResult::LimitReached,
            SearchOutcome::Cancelled => return AutoRouteResult::Cancelled,
        }
    }
    if route_point_limit_hit {
        AutoRouteResult::LimitReached
    } else {
        AutoRouteResult::NoRoute
    }
}

fn valid_request(request: &AutoRouteRequest) -> bool {
    fn valid_endpoint(endpoint: RouteEndpoint) -> bool {
        endpoint.room_center.is_finite()
            && endpoint.room_center.x.abs() <= MAX_COORDINATE
            && endpoint.room_center.y.abs() <= MAX_COORDINATE
            && endpoint.port_offset.is_finite()
            && (0.0..=1.0).contains(&endpoint.port_offset)
    }

    fn valid_rect(rect: RouteRect) -> bool {
        [rect.min_x, rect.min_y, rect.max_x, rect.max_y]
            .into_iter()
            .all(f64::is_finite)
            && rect.min_x <= rect.max_x
            && rect.min_y <= rect.max_y
            && rect.min_x.abs() <= f64::from(MAX_COORDINATE)
            && rect.min_y.abs() <= f64::from(MAX_COORDINATE)
            && rect.max_x.abs() <= f64::from(MAX_COORDINATE)
            && rect.max_y.abs() <= f64::from(MAX_COORDINATE)
    }

    let endpoints_valid = valid_endpoint(request.endpoint_a)
        && valid_endpoint(request.endpoint_b)
        && request.endpoint_a.room_number != request.endpoint_b.room_number;
    let stroke_valid = request.thickness.is_finite()
        && request.thickness >= 0.0
        && ROUTE_PADDING >= f64::from(ARROW_SIZE);
    let obstacles_valid = request
        .obstacles
        .iter()
        .all(|obstacle| valid_rect(obstacle.bounds));
    let endpoints_present = [request.endpoint_a, request.endpoint_b]
        .into_iter()
        .all(|endpoint| {
            request.obstacles.iter().any(|obstacle| {
                obstacle.room_number == endpoint.room_number
                    && same_rect(obstacle.bounds, endpoint.room_bounds())
            })
        });
    let mut obstacle_rooms: Vec<_> = request
        .obstacles
        .iter()
        .map(|obstacle| obstacle.room_number)
        .collect();
    obstacle_rooms.sort_unstable();
    let rooms_unique = obstacle_rooms.windows(2).all(|pair| pair[0] != pair[1]);

    endpoints_valid && stroke_valid && obstacles_valid && endpoints_present && rooms_unique
}

fn search_bounds(a: MapPoint, b: MapPoint, margin: f64) -> RouteRect {
    RouteRect {
        min_x: f64::from(a.x.min(b.x)) - margin,
        min_y: f64::from(a.y.min(b.y)) - margin,
        max_x: f64::from(a.x.max(b.x)) + margin,
        max_y: f64::from(a.y.max(b.y)) + margin,
    }
}

#[allow(clippy::cast_possible_truncation)]
fn escape_connector(endpoint: RouteEndpoint, clearance: f64) -> Vec<MapPoint> {
    let blocked = endpoint.room_bounds().inflate(clearance);
    let tip = endpoint.tip();
    let mut lane = GridPoint::from_map(tip);
    match endpoint.side {
        RoomSide::North => lane.y = (blocked.min_y / LANE_STEP).floor() as i32 - 1,
        RoomSide::East => lane.x = (blocked.max_x / LANE_STEP).ceil() as i32 + 1,
        RoomSide::South => lane.y = (blocked.max_y / LANE_STEP).ceil() as i32 + 1,
        RoomSide::West => lane.x = (blocked.min_x / LANE_STEP).floor() as i32 - 1,
    }
    let lane_point = lane.to_map();
    let bend = match endpoint.side {
        RoomSide::North | RoomSide::South => MapPoint::new(tip.x, lane_point.y),
        RoomSide::East | RoomSide::West => MapPoint::new(lane_point.x, tip.y),
    };
    simplify_collinear(vec![tip, bend, lane_point])
}

#[allow(clippy::too_many_arguments)]
fn search_attempt(
    start: GridPoint,
    goal: GridPoint,
    start_entry: Direction,
    goal_exit: Direction,
    bounds: RouteRect,
    obstacles: &[IndexedObstacle],
    visited_states: &mut usize,
    limits: SolveLimits,
    started: Instant,
    cancel: &AtomicBool,
) -> SearchOutcome {
    let initial = State {
        point: start,
        entry: start_entry,
    };
    let mut open = BinaryHeap::new();
    open.push(HeapEntry {
        state: initial,
        f_cost: start.manhattan(goal) * STEP_COST,
        g_cost: 0,
        bend_count: 0,
    });
    let mut scores = HashMap::new();
    scores.insert(initial, (0_u32, 0_u32));
    let mut parents = HashMap::new();
    let mut best_goal: Option<(u32, u32, State)> = None;

    while let Some(current) = open.pop() {
        if cancelled_or_timed_out(cancel, started, limits.wall_time) {
            return if cancel.load(AtomicOrdering::Relaxed) {
                SearchOutcome::Cancelled
            } else {
                SearchOutcome::Limited
            };
        }
        if *visited_states >= limits.max_states {
            return SearchOutcome::Limited;
        }
        if scores.get(&current.state).copied() != Some((current.g_cost, current.bend_count)) {
            continue;
        }
        if best_goal.is_some_and(|(cost, _, _)| current.f_cost > cost) {
            break;
        }
        *visited_states += 1;
        if current.state.point == goal {
            let final_bend = u32::from(current.state.entry != goal_exit);
            let candidate = (
                current.g_cost + final_bend * TURN_COST,
                current.bend_count + final_bend,
                current.state,
            );
            if best_goal.is_none_or(|best| candidate < best) {
                best_goal = Some(candidate);
            }
            continue;
        }

        for direction in Direction::ALL {
            let point = current.state.point.moved(direction);
            if !grid_in_bounds(point, bounds)
                || point_blocked(point, obstacles)
                || segment_blocked(current.state.point, point, obstacles)
            {
                continue;
            }
            let bend = u32::from(direction != current.state.entry);
            let adjacency = adjacent_obstacle_count(point, obstacles);
            let g_cost =
                current.g_cost + STEP_COST + bend * TURN_COST + adjacency * OBSTACLE_ADJACENCY_COST;
            let bend_count = current.bend_count + bend;
            let state = State {
                point,
                entry: direction,
            };
            if scores
                .get(&state)
                .is_some_and(|&(old_cost, old_bends)| (old_cost, old_bends) <= (g_cost, bend_count))
            {
                continue;
            }
            scores.insert(state, (g_cost, bend_count));
            parents.insert(state, current.state);
            open.push(HeapEntry {
                state,
                f_cost: g_cost + point.manhattan(goal) * STEP_COST,
                g_cost,
                bend_count,
            });
        }
    }

    let Some((_, _, goal_state)) = best_goal else {
        return SearchOutcome::Exhausted;
    };
    let mut path = vec![goal_state.point];
    let mut cursor = goal_state;
    while cursor != initial {
        let Some(parent) = parents.get(&cursor).copied() else {
            return SearchOutcome::Exhausted;
        };
        path.push(parent.point);
        cursor = parent;
    }
    path.reverse();
    SearchOutcome::Found(path)
}

fn grid_in_bounds(point: GridPoint, bounds: RouteRect) -> bool {
    let (x, y) = point.map_coordinates();
    x >= bounds.min_x && x <= bounds.max_x && y >= bounds.min_y && y <= bounds.max_y
}

fn point_blocked(point: GridPoint, obstacles: &[IndexedObstacle]) -> bool {
    obstacles.iter().any(|entry| entry.inflated.contains(point))
}

fn segment_blocked(a: GridPoint, b: GridPoint, obstacles: &[IndexedObstacle]) -> bool {
    let a = a.to_map();
    let b = b.to_map();
    obstacles
        .iter()
        .any(|entry| entry.inflated.intersects_segment(a, b))
}

fn adjacent_obstacle_count(point: GridPoint, obstacles: &[IndexedObstacle]) -> u32 {
    Direction::ALL
        .into_iter()
        .filter(|&direction| point_blocked(point.moved(direction), obstacles))
        .count()
        .try_into()
        .expect("four directions fit u32")
}

fn assemble_route(
    connector_a: &[MapPoint],
    grid_path: &[GridPoint],
    connector_b: &[MapPoint],
) -> Vec<MapPoint> {
    let mut full = connector_a.to_vec();
    full.extend(grid_path.iter().copied().map(GridPoint::to_map));
    full.extend(connector_b.iter().rev().copied());
    let mut full = simplify_collinear(full);
    if !full.is_empty() {
        full.remove(0);
    }
    full.pop();
    full
}

fn simplify_collinear(points: Vec<MapPoint>) -> Vec<MapPoint> {
    let mut simplified: Vec<MapPoint> = Vec::with_capacity(points.len());
    for point in points {
        if simplified
            .last()
            .is_some_and(|last| same_point(*last, point))
        {
            continue;
        }
        while simplified.len() >= 2 {
            let a = simplified[simplified.len() - 2];
            let b = simplified[simplified.len() - 1];
            if (nearly_equal(a.x, b.x) && nearly_equal(b.x, point.x))
                || (nearly_equal(a.y, b.y) && nearly_equal(b.y, point.y))
            {
                simplified.pop();
            } else {
                break;
            }
        }
        simplified.push(point);
    }
    simplified
}

/// Checks a candidate without mutating it. This is also the route-warning
/// authority used by the editor after rooms or appearance change.
#[must_use]
pub fn validate_route(request: &AutoRouteRequest, route_points: &[MapPoint]) -> RouteValidation {
    if !valid_request(request) {
        return RouteValidation::Invalid;
    }
    let clearance = f64::from(request.thickness.max(0.0) * BASE_STROKE_WIDTH / 2.0) + ROUTE_PADDING;
    let obstacle_index = RTree::bulk_load(
        request
            .obstacles
            .iter()
            .copied()
            .map(|obstacle| {
                let inflated = obstacle.bounds.inflate(clearance);
                IndexedObstacle {
                    obstacle,
                    inflated,
                    envelope: AABB::from_corners(
                        [inflated.min_x, inflated.min_y],
                        [inflated.max_x, inflated.max_y],
                    ),
                }
            })
            .collect(),
    );
    validate_route_with_index(request, route_points, &obstacle_index)
}

/// Resolves a candidate through the shared renderer geometry pipeline only
/// when it is valid and collision-free for this snapshot. Callers use this
/// for a view-only preview; accepting the route remains a separate CAS write.
#[must_use]
pub fn validated_geometry(
    request: &AutoRouteRequest,
    route_points: &[MapPoint],
) -> Option<crate::connection_geometry::ConnectionGeometry> {
    (validate_route(request, route_points) == RouteValidation::Valid)
        .then(|| resolve_route_geometry(request, route_points))
}

fn validate_route_with_index(
    request: &AutoRouteRequest,
    route_points: &[MapPoint],
    obstacles: &RTree<IndexedObstacle>,
) -> RouteValidation {
    if route_points.len() > MAX_ROUTE_POINTS
        || route_points.iter().any(|point| {
            !point.is_finite() || point.x.abs() > MAX_COORDINATE || point.y.abs() > MAX_COORDINATE
        })
        || orthogonal_violation(
            request.endpoint_a.tip(),
            route_points,
            request.endpoint_b.tip(),
        )
        .is_some()
    {
        return RouteValidation::Invalid;
    }
    let geometry = resolve_route_geometry(request, route_points);
    let collision = geometry.flattened.iter().any(|polyline| {
        polyline.windows(2).any(|segment| {
            obstacles.iter().any(|entry| {
                entry.obstacle.room_number != request.endpoint_a.room_number
                    && entry.obstacle.room_number != request.endpoint_b.room_number
                    && entry.inflated.intersects_segment(segment[0], segment[1])
            })
        })
    });
    if collision {
        RouteValidation::Collision
    } else {
        RouteValidation::Valid
    }
}

fn resolve_route_geometry(
    request: &AutoRouteRequest,
    route_points: &[MapPoint],
) -> crate::connection_geometry::ConnectionGeometry {
    resolve(&GeometryInput {
        kind: ConnectionKind::Internal,
        routing: ConnectionRouting::Automatic,
        corner: request.corner,
        endpoint_a: request.endpoint_a.geometry(),
        endpoint_b: Some(request.endpoint_b.geometry()),
        route_points,
        thickness: request.thickness,
    })
}

fn same_rect(a: RouteRect, b: RouteRect) -> bool {
    nearly_equal_f64(a.min_x, b.min_x)
        && nearly_equal_f64(a.min_y, b.min_y)
        && nearly_equal_f64(a.max_x, b.max_x)
        && nearly_equal_f64(a.max_y, b.max_y)
}

fn cancelled_or_timed_out(
    cancel: &AtomicBool,
    started: Instant,
    wall_time: Option<Duration>,
) -> bool {
    cancel.load(AtomicOrdering::Relaxed)
        || wall_time.is_some_and(|limit| started.elapsed() >= limit)
}

fn same_point(a: MapPoint, b: MapPoint) -> bool {
    nearly_equal(a.x, b.x) && nearly_equal(a.y, b.y)
}

fn nearly_equal(a: f32, b: f32) -> bool {
    (a - b).abs() <= 1e-4
}

fn nearly_equal_f64(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;

    const HALF_ROOM: f64 = 0.25;

    fn endpoint(room: i32, center: MapPoint, tip: MapPoint, side: RoomSide) -> RouteEndpoint {
        RouteEndpoint {
            room_number: RoomNumber(room),
            room_center: center,
            side,
            port_offset: match side {
                RoomSide::North | RoomSide::South => (tip.x - center.x) / ROOM_SIZE + 0.5,
                RoomSide::East | RoomSide::West => (tip.y - center.y) / ROOM_SIZE + 0.5,
            },
        }
    }

    fn obstacle(room: i32, center: MapPoint) -> RouteObstacle {
        RouteObstacle {
            room_number: RoomNumber(room),
            bounds: RouteRect::from_center(center, HALF_ROOM, HALF_ROOM),
        }
    }

    fn request(obstacles: Vec<RouteObstacle>) -> AutoRouteRequest {
        AutoRouteRequest {
            endpoint_a: endpoint(
                1,
                MapPoint::new(0.0, 0.0),
                MapPoint::new(0.4, 0.0),
                RoomSide::East,
            ),
            endpoint_b: endpoint(
                2,
                MapPoint::new(6.0, 0.0),
                MapPoint::new(5.6, 0.0),
                RoomSide::West,
            ),
            obstacles,
            thickness: 1.0,
            corner: CornerStyle::Rounded,
        }
    }

    fn solve_unbounded(request: &AutoRouteRequest) -> AutoRouteResult {
        solve_with_limits(
            request,
            &AtomicBool::new(false),
            SolveLimits {
                max_states: MAX_VISITED_STATES,
                wall_time: None,
            },
        )
    }

    #[test]
    fn straight_route_is_minimal_and_orthogonal() {
        let request = request(vec![
            obstacle(1, MapPoint::new(0.0, 0.0)),
            obstacle(2, MapPoint::new(6.0, 0.0)),
        ]);
        let AutoRouteResult::Solved { route_points, .. } = solve_unbounded(&request) else {
            panic!("straight route should solve");
        };
        assert_eq!(
            validate_route(&request, &route_points),
            RouteValidation::Valid
        );
        assert!(route_points.is_empty(), "a straight route needs no elbows");
    }

    #[test]
    fn obstacle_detour_is_deterministic_under_heap_ties() {
        let request = request(vec![
            obstacle(1, MapPoint::new(0.0, 0.0)),
            obstacle(2, MapPoint::new(6.0, 0.0)),
            obstacle(3, MapPoint::new(3.0, 0.0)),
        ]);
        let first = solve_unbounded(&request);
        let second = solve_unbounded(&request);
        assert_eq!(first, second);
        let AutoRouteResult::Solved { route_points, .. } = first else {
            panic!("detour should solve: {first:?}");
        };
        assert!(route_points.iter().any(|point| point.y.abs() > 0.5));
        assert_eq!(
            validate_route(&request, &route_points),
            RouteValidation::Valid
        );
    }

    #[test]
    fn state_cap_reports_limit_without_partial_points() {
        let request = request(vec![
            obstacle(1, MapPoint::new(0.0, 0.0)),
            obstacle(2, MapPoint::new(6.0, 0.0)),
            obstacle(3, MapPoint::new(3.0, 0.0)),
        ]);
        let result = solve_with_limits(
            &request,
            &AtomicBool::new(false),
            SolveLimits {
                max_states: 1,
                wall_time: None,
            },
        );
        assert_eq!(result, AutoRouteResult::LimitReached);
    }

    #[test]
    fn cancellation_is_immediate_and_distinct() {
        let result = solve(
            &request(vec![
                obstacle(1, MapPoint::new(0.0, 0.0)),
                obstacle(2, MapPoint::new(6.0, 0.0)),
            ]),
            &AtomicBool::new(true),
        );
        assert_eq!(result, AutoRouteResult::Cancelled);
    }

    #[test]
    fn unrelated_room_on_escape_lane_rejects_the_final_route() {
        let request = request(vec![
            obstacle(1, MapPoint::new(0.0, 0.0)),
            obstacle(2, MapPoint::new(6.0, 0.0)),
            obstacle(3, MapPoint::new(0.75, 0.0)),
        ]);
        assert_eq!(solve_unbounded(&request), AutoRouteResult::NoRoute);
    }

    #[test]
    fn malformed_or_secret_omitting_obstacle_snapshots_are_rejected() {
        let missing_endpoint = request(vec![obstacle(1, MapPoint::new(0.0, 0.0))]);
        assert_eq!(solve_unbounded(&missing_endpoint), AutoRouteResult::NoRoute);

        let mut non_finite = request(vec![
            obstacle(1, MapPoint::new(0.0, 0.0)),
            obstacle(2, MapPoint::new(6.0, 0.0)),
        ]);
        non_finite.obstacles.push(RouteObstacle {
            room_number: RoomNumber(3),
            bounds: RouteRect {
                min_x: f64::NAN,
                min_y: 0.0,
                max_x: 1.0,
                max_y: 1.0,
            },
        });
        assert_eq!(solve_unbounded(&non_finite), AutoRouteResult::NoRoute);
    }

    #[test]
    fn validation_checks_endpoint_legs_and_visible_rounded_geometry() {
        let request = request(vec![
            obstacle(1, MapPoint::new(0.0, 0.0)),
            obstacle(2, MapPoint::new(6.0, 0.0)),
            obstacle(3, MapPoint::new(3.0, 0.0)),
        ]);
        assert_eq!(
            validate_route(&request, &[MapPoint::new(3.0, 1.0)]),
            RouteValidation::Invalid
        );
        assert_eq!(validate_route(&request, &[]), RouteValidation::Collision);
    }
}
