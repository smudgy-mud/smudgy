/** Stored point maintenance for one complete layout, before batch splitting. */
export interface AuthoredPoint {
  readonly x: number;
  readonly y: number;
}

export interface AuthoredEndpoint {
  readonly side: "North" | "East" | "South" | "West";
  readonly port_offset: number;
}

export interface AuthoredRouteMove {
  readonly points: readonly AuthoredPoint[];
  readonly endpointA: AuthoredEndpoint;
  readonly endpointB: AuthoredEndpoint;
  readonly beforeA: AuthoredPoint;
  readonly beforeB: AuthoredPoint;
  readonly afterA: AuthoredPoint;
  readonly afterB: AuthoredPoint;
  readonly segmentShape: "Direct" | "Orthogonal";
  /** True for a final active Manual route; false for dormant Simple/Stub points. */
  readonly authored: boolean;
}

// Public cloud connection_geometry.rs constants. Keep operations in f32 so
// the host's strict orthogonality check sees exactly the restored stub axes.
const f32 = Math.fround;
const ROOM_SIZE = f32(0.5);
const STUB_LENGTH = f32(0.15);
const CORNER_INSET = f32(0.2);
const FLOAT_EPSILON = 2 ** -23;

function near(left: number, right: number): boolean {
  return Math.abs(f32(left - right)) <= FLOAT_EPSILON;
}

/** Public cloud port_position + wall-normal stub_tip, including rounded corners. */
export function authoredEndpointTip(position: AuthoredPoint, endpoint: AuthoredEndpoint): { x: number; y: number } {
  const half = f32(ROOM_SIZE / 2);
  const radius = f32(CORNER_INSET * ROOM_SIZE);
  const flat = f32(half - radius);
  const offset = Math.min(1, Math.max(0, f32(endpoint.port_offset)));
  const along = f32(f32(offset - 0.5) * ROOM_SIZE);
  let across = along;
  let outward = half;
  if (Math.abs(along) > flat) {
    const angle = f32(f32(f32(Math.abs(along) - flat) / radius) * f32(Math.PI / 4));
    across = f32(Math.sign(along) * f32(flat + f32(radius * f32(Math.sin(angle)))));
    outward = f32(flat + f32(radius * f32(Math.cos(angle))));
  }
  const x = f32(position.x);
  const y = f32(position.y);
  switch (endpoint.side) {
    case "North": return { x: f32(x + across), y: f32(f32(y - outward) - STUB_LENGTH) };
    case "South": return { x: f32(x + across), y: f32(f32(y + outward) + STUB_LENGTH) };
    case "East": return { x: f32(f32(x + outward) + STUB_LENGTH), y: f32(y + across) };
    case "West": return { x: f32(f32(x - outward) - STUB_LENGTH), y: f32(y + across) };
  }
}

/**
 * Mirrors cloud area_edits::maintain_routes_after_room_moves for a complete
 * final move. Shared endpoint deltas translate all stored points. An active
 * Orthogonal author route otherwise adjusts only endpoint legs and inserts
 * the minimum elbows; dormant/direct interiors remain untouched.
 */
export function maintainAuthoredRoutePoints(move: AuthoredRouteMove): { x: number; y: number }[] {
  const points = move.points.map(({ x, y }) => ({ x: f32(x), y: f32(y) }));
  const deltaA = { x: f32(f32(move.afterA.x) - f32(move.beforeA.x)), y: f32(f32(move.afterA.y) - f32(move.beforeA.y)) };
  const deltaB = { x: f32(f32(move.afterB.x) - f32(move.beforeB.x)), y: f32(f32(move.afterB.y) - f32(move.beforeB.y)) };
  if (deltaA.x === 0 && deltaA.y === 0 && deltaB.x === 0 && deltaB.y === 0) return points;
  if (deltaA.x === deltaB.x && deltaA.y === deltaB.y) {
    return points.map(({ x, y }) => ({ x: f32(x + deltaA.x), y: f32(y + deltaA.y) }));
  }
  if (!move.authored || move.segmentShape !== "Orthogonal") return points;

  const oldTipA = authoredEndpointTip(move.beforeA, move.endpointA);
  const oldTipB = authoredEndpointTip(move.beforeB, move.endpointB);
  const newTipA = authoredEndpointTip(move.afterA, move.endpointA);
  const newTipB = authoredEndpointTip(move.afterB, move.endpointB);
  if (points.length === 0) {
    if (!near(newTipA.x, newTipB.x) && !near(newTipA.y, newTipB.y)) {
      points.push({ x: newTipB.x, y: newTipA.y });
    }
  } else {
    if (near(points[0].y, oldTipA.y)) points[0].y = newTipA.y;
    else points[0].x = newTipA.x;
    const last = points.length - 1;
    if (near(points[last].y, oldTipB.y)) points[last].y = newTipB.y;
    else points[last].x = newTipB.x;
    const first = points[0];
    if (!near(first.x, newTipA.x) && !near(first.y, newTipA.y)) {
      points.unshift({ x: first.x, y: newTipA.y });
    }
    const end = points[points.length - 1];
    if (!near(end.x, newTipB.x) && !near(end.y, newTipB.y)) {
      points.push({ x: newTipB.x, y: end.y });
    }
  }
  return points.filter((point, index) => index === 0 ||
    point.x !== points[index - 1].x || point.y !== points[index - 1].y);
}
