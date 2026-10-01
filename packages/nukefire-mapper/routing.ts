import type { GridPosition } from "./layout.ts";

export type RouteSide = "North" | "East" | "South" | "West";

/**
 * Stored geometry for a generated connection route. Generated paths prefer
 * orthogonal turn points, but `Direct` keeps that preference from becoming a
 * persistence invariant when endpoints move or no Manhattan path is found.
 */
export interface PlannedConnectionRoute {
  startSide: RouteSide;
  endSide: RouteSide;
  routing: "Manual" | "Automatic";
  segmentShape: "Direct";
  corner: "Rounded";
  routePoints: { x: number; y: number }[];
}

interface Step {
  x: number;
  y: number;
  side: RouteSide;
}

interface SearchState {
  x: number;
  y: number;
  direction: number;
  cost: number;
  priority: number;
  previous?: string;
}

const STEPS: readonly Step[] = [
  { x: 0, y: -1, side: "North" },
  { x: 1, y: 0, side: "East" },
  { x: 0, y: 1, side: "South" },
  { x: -1, y: 0, side: "West" },
];

const OPPOSITE_SIDE: Record<RouteSide, RouteSide> = {
  North: "South",
  East: "West",
  South: "North",
  West: "East",
};

function pointKey(x: number, y: number): string {
  return `${x}:${y}`;
}

function stateKey(x: number, y: number, direction: number): string {
  return `${x}:${y}:${direction}`;
}

function sameCell(a: GridPosition, b: GridPosition): boolean {
  return Math.round(a.x) === Math.round(b.x) && Math.round(a.y) === Math.round(b.y);
}

function segmentIntersectsRoom(
  from: GridPosition,
  to: GridPosition,
  room: GridPosition,
): boolean {
  if (from.level !== room.level || to.level !== room.level) return false;
  if (sameCell(from, room) || sameCell(to, room)) return false;

  // Smudgy rooms occupy roughly half of one grid interval. Treat a slightly
  // larger central square as blocked so diagonal connections do not graze it.
  const half = 0.32;
  const minX = room.x - half;
  const maxX = room.x + half;
  const minY = room.y - half;
  const maxY = room.y + half;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  let enter = 0;
  let leave = 1;
  const clips: readonly [number, number][] = [
    [-dx, from.x - minX],
    [dx, maxX - from.x],
    [-dy, from.y - minY],
    [dy, maxY - from.y],
  ];
  for (const [p, q] of clips) {
    if (p === 0) {
      if (q < 0) return false;
      continue;
    }
    const ratio = q / p;
    if (p < 0) {
      enter = Math.max(enter, ratio);
    } else {
      leave = Math.min(leave, ratio);
    }
    if (enter > leave) return false;
  }
  return true;
}

/** Rooms whose rendered cell is crossed by a straight connection. */
export function directRoomObstructions(
  from: GridPosition,
  to: GridPosition,
  rooms: readonly GridPosition[],
): GridPosition[] {
  return rooms.filter((room) => segmentIntersectsRoom(from, to, room));
}

/**
 * Whether a room on one of `cells` would stand on a connection drawn from
 * `from` through `routePoints` to `to`: on a turn of its route, or across one
 * of its segments. Cells on another level never do.
 */
export function routeCrossesCells(
  from: GridPosition,
  to: GridPosition,
  routePoints: readonly { x: number; y: number }[],
  cells: readonly GridPosition[],
): boolean {
  const level = from.level;
  const onLevel = cells.filter((cell) => cell.level === level);
  if (onLevel.length === 0) return false;
  const turns = routePoints.map((point) => ({ x: Math.round(point.x), y: Math.round(point.y), level }));
  if (onLevel.some((cell) => turns.some((turn) => turn.x === cell.x && turn.y === cell.y))) return true;
  const path = [from, ...turns, to];
  for (let index = 1; index < path.length; index += 1) {
    if (directRoomObstructions(path[index - 1], path[index], onLevel).length > 0) return true;
  }
  return false;
}

class MinHeap {
  readonly #values: SearchState[] = [];

  get size(): number {
    return this.#values.length;
  }

  push(value: SearchState): void {
    this.#values.push(value);
    let index = this.#values.length - 1;
    while (index > 0) {
      const parent = Math.floor((index - 1) / 2);
      if (this.#values[parent].priority <= value.priority) break;
      this.#values[index] = this.#values[parent];
      index = parent;
    }
    this.#values[index] = value;
  }

  pop(): SearchState | undefined {
    const first = this.#values[0];
    const last = this.#values.pop();
    if (!first || !last || this.#values.length === 0) return first;
    let index = 0;
    while (true) {
      const left = index * 2 + 1;
      const right = left + 1;
      if (left >= this.#values.length) break;
      const child = right < this.#values.length &&
          this.#values[right].priority < this.#values[left].priority
        ? right
        : left;
      if (this.#values[child].priority >= last.priority) break;
      this.#values[index] = this.#values[child];
      index = child;
    }
    this.#values[index] = last;
    return first;
  }
}

function reconstruct(
  goal: SearchState,
  states: ReadonlyMap<string, SearchState>,
  level: number,
): GridPosition[] {
  const path: GridPosition[] = [];
  let current: SearchState | undefined = goal;
  while (current) {
    path.push({ x: current.x, y: current.y, level });
    current = current.previous ? states.get(current.previous) : undefined;
  }
  return path.reverse();
}

function search(
  from: GridPosition,
  to: GridPosition,
  blocked: ReadonlySet<string>,
  preferredStart: RouteSide | undefined,
  preferredEnd: RouteSide | undefined,
  padding: number,
): GridPosition[] | undefined {
  const startX = Math.round(from.x);
  const startY = Math.round(from.y);
  const endX = Math.round(to.x);
  const endY = Math.round(to.y);
  const minX = Math.min(startX, endX) - padding;
  const maxX = Math.max(startX, endX) + padding;
  const minY = Math.min(startY, endY) - padding;
  const maxY = Math.max(startY, endY) + padding;
  const frontier = new MinHeap();
  const states = new Map<string, SearchState>();
  const bestCost = new Map<string, number>();
  const start: SearchState = {
    x: startX,
    y: startY,
    direction: -1,
    cost: 0,
    priority: Math.abs(endX - startX) * 10 + Math.abs(endY - startY) * 10,
  };
  frontier.push(start);
  states.set(stateKey(startX, startY, -1), start);
  bestCost.set(stateKey(startX, startY, -1), 0);

  while (frontier.size > 0) {
    const current = frontier.pop() as SearchState;
    if (current.x === endX && current.y === endY) return reconstruct(current, states, from.level);
    const currentKey = stateKey(current.x, current.y, current.direction);
    if (current.cost !== bestCost.get(currentKey)) continue;

    for (let direction = 0; direction < STEPS.length; direction += 1) {
      const step = STEPS[direction];
      // A generated detour must preserve the semantic exit wall. Soft costs
      // allowed a North exit to leave through South when that shortened the
      // path, making a layout obstruction look like a valid routed link.
      if (current.direction < 0 && preferredStart && step.side !== preferredStart) continue;
      const x = current.x + step.x;
      const y = current.y + step.y;
      if (x < minX || x > maxX || y < minY || y > maxY) continue;
      const isGoal = x === endX && y === endY;
      if (isGoal && preferredEnd && step.side !== OPPOSITE_SIDE[preferredEnd]) continue;
      if (!isGoal && blocked.has(pointKey(x, y))) continue;

      let cost = current.cost + 10;
      if (current.direction >= 0 && current.direction !== direction) cost += 3;
      const key = stateKey(x, y, direction);
      if ((bestCost.get(key) ?? Number.POSITIVE_INFINITY) <= cost) continue;
      const next: SearchState = {
        x,
        y,
        direction,
        cost,
        priority: cost + (Math.abs(endX - x) + Math.abs(endY - y)) * 10,
        previous: currentKey,
      };
      bestCost.set(key, cost);
      states.set(key, next);
      frontier.push(next);
    }
  }
  return undefined;
}

/**
 * Find an orthogonal centerline through unoccupied integral cells. The endpoint
 * rooms are allowed; every interior room cell is an obstacle.
 */
export function routeAroundRooms(
  from: GridPosition,
  to: GridPosition,
  rooms: readonly GridPosition[],
  preferredStart?: RouteSide,
  preferredEnd?: RouteSide,
): GridPosition[] | undefined {
  if (from.level !== to.level) return undefined;
  const blocked = new Set(
    rooms
      .filter((room) => room.level === from.level && !sameCell(room, from) && !sameCell(room, to))
      .map((room) => pointKey(Math.round(room.x), Math.round(room.y))),
  );
  for (const padding of [3, 6, 12, 24, 48]) {
    const path = search(from, to, blocked, preferredStart, preferredEnd, padding);
    if (path) return path;
  }
  return undefined;
}

/** Keep only orthogonal turn vertices; connection endpoints are not route points. */
export function routeTurnPoints(path: readonly GridPosition[]): { x: number; y: number }[] {
  const result: { x: number; y: number }[] = [];
  for (let index = 1; index < path.length - 1; index += 1) {
    const before = path[index - 1];
    const current = path[index];
    const after = path[index + 1];
    const beforeDirection = `${current.x - before.x}:${current.y - before.y}`;
    const afterDirection = `${after.x - current.x}:${after.y - current.y}`;
    if (beforeDirection !== afterDirection) result.push({ x: current.x, y: current.y });
  }
  return result;
}

export function routeStartSide(path: readonly GridPosition[]): RouteSide | undefined {
  if (path.length < 2) return undefined;
  const dx = path[1].x - path[0].x;
  const dy = path[1].y - path[0].y;
  return STEPS.find((step) => step.x === dx && step.y === dy)?.side;
}

export function routeEndSide(path: readonly GridPosition[]): RouteSide | undefined {
  if (path.length < 2) return undefined;
  const before = path[path.length - 2];
  const end = path[path.length - 1];
  const dx = before.x - end.x;
  const dy = before.y - end.y;
  return STEPS.find((step) => step.x === dx && step.y === dy)?.side;
}

/** One engine route amendment, translated into this area's room numbers. */
export interface ResolvedRouteAmendment {
  fromRoomNumber: number;
  toRoomNumber: number;
  /** Elbow cells ordered from `fromRoomNumber` toward `toRoomNumber`. */
  waypoints: readonly { x: number; y: number }[];
}

function amendmentPairKey(a: number, b: number): string {
  return a <= b ? `${a}|${b}` : `${b}|${a}`;
}

/**
 * Translate a plan's engine route amendments into room-number space, keyed by
 * their unordered room pair. Amendments whose endpoints do not resolve to
 * known rooms are dropped — amendments are advisory, and a plan can describe
 * rooms this area has not committed yet. Returns undefined when nothing
 * resolves so callers skip matching entirely.
 */
export function indexRouteAmendments(
  amendments:
    | readonly {
      from: string;
      to: string;
      waypoints: readonly { x: number; y: number }[];
    }[]
    | undefined,
  roomNumbersByLayoutId: ReadonlyMap<string, number>,
): ReadonlyMap<string, ResolvedRouteAmendment> | undefined {
  if (!amendments || amendments.length === 0) return undefined;
  const result = new Map<string, ResolvedRouteAmendment>();
  for (const amendment of amendments) {
    const fromRoomNumber = roomNumbersByLayoutId.get(amendment.from);
    const toRoomNumber = roomNumbersByLayoutId.get(amendment.to);
    if (fromRoomNumber === undefined || toRoomNumber === undefined ||
      fromRoomNumber === toRoomNumber || amendment.waypoints.length === 0) continue;
    const key = amendmentPairKey(fromRoomNumber, toRoomNumber);
    if (result.has(key)) continue;
    result.set(key, {
      fromRoomNumber,
      toRoomNumber,
      waypoints: amendment.waypoints.map((point) => ({ x: point.x, y: point.y })),
    });
  }
  return result.size > 0 ? result : undefined;
}

/**
 * The amendment waypoints for the connection joining two rooms, oriented to
 * run from `fromRoomNumber` toward `toRoomNumber`; undefined when no
 * amendment names that pair.
 */
export function amendmentWaypointsBetween(
  index: ReadonlyMap<string, ResolvedRouteAmendment> | undefined,
  fromRoomNumber: number,
  toRoomNumber: number,
): { x: number; y: number }[] | undefined {
  const amendment = index?.get(amendmentPairKey(fromRoomNumber, toRoomNumber));
  if (!amendment) return undefined;
  const oriented = amendment.fromRoomNumber === fromRoomNumber
    ? [...amendment.waypoints]
    : [...amendment.waypoints].reverse();
  return oriented.map((point) => ({ x: point.x, y: point.y }));
}

/** The wall a possibly multi-cell axis-aligned segment leaves through. */
function segmentSide(dx: number, dy: number): RouteSide | undefined {
  if (dx === 0 && dy < 0) return "North";
  if (dx > 0 && dy === 0) return "East";
  if (dx === 0 && dy > 0) return "South";
  if (dx < 0 && dy === 0) return "West";
  return undefined;
}

/**
 * Build the stored route for an engine amendment. The drawn centerline runs
 * `from` → each waypoint → `to`, so the endpoint walls follow the first and
 * last drawn segments when those are axis-aligned and keep the semantic
 * preference otherwise. Amendment routes persist as `Automatic` exactly like
 * every other generated route; `Manual` remains the user-ownership marker.
 */
export function amendedConnectionRoute(
  from: GridPosition,
  to: GridPosition,
  waypoints: readonly { x: number; y: number }[],
  preferredStart: RouteSide,
  preferredEnd: RouteSide,
): PlannedConnectionRoute {
  const first = waypoints[0];
  const last = waypoints[waypoints.length - 1];
  return {
    startSide: first
      ? segmentSide(first.x - Math.round(from.x), first.y - Math.round(from.y)) ?? preferredStart
      : preferredStart,
    endSide: last
      ? segmentSide(last.x - Math.round(to.x), last.y - Math.round(to.y)) ?? preferredEnd
      : preferredEnd,
    routing: "Automatic",
    segmentShape: "Direct",
    corner: "Rounded",
    routePoints: waypoints.map((point) => ({ x: point.x, y: point.y })),
  };
}

/**
 * Prefer a Manhattan route around occupied rooms without requiring stored
 * segments to remain perfectly axis-aligned. The renderer still follows the
 * generated turn points; `Direct` permits a diagonal fallback or harmless
 * endpoint drift, while `Rounded` fillets every resulting turn.
 */
export function planConnectionRoute(
  from: GridPosition,
  to: GridPosition,
  rooms: readonly GridPosition[],
  preferredStart: RouteSide,
  preferredEnd: RouteSide,
  knownObstructed?: boolean,
): PlannedConnectionRoute {
  const obstructed = knownObstructed ?? directRoomObstructions(from, to, rooms).length > 0;
  if (obstructed) {
    const path = routeAroundRooms(from, to, rooms, preferredStart, preferredEnd);
    if (path) {
      return {
        startSide: routeStartSide(path) ?? preferredStart,
        endSide: routeEndSide(path) ?? preferredEnd,
        routing: "Manual",
        segmentShape: "Direct",
        corner: "Rounded",
        routePoints: routeTurnPoints(path),
      };
    }
  }
  // Falling back to automatic geometry is preferable to persisting a manual
  // route on the wrong room walls. The layout quality reports the blocked
  // ports and gives the quiet reflow a chance to move the offending rooms.
  return {
    startSide: preferredStart,
    endSide: preferredEnd,
    routing: "Automatic",
    segmentShape: "Direct",
    corner: "Rounded",
    routePoints: [],
  };
}
