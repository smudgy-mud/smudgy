import {
  compareLayoutQuality,
  directionalViolationEdges,
  planIntegralLayout,
  type GridPosition,
  type LayoutDirection,
  type LayoutEdge,
  type LayoutNode,
  type LayoutQuality,
  type LayoutResident,
  type LayoutTraceEvent,
} from "./layout.ts";

export type LayoutRoomKey = string | number;
export type ElevationPreference = "auto" | "levels" | "projected";
export type LayoutSearchEffort = "standard" | "thorough";

export interface LayoutModelRoom {
  id: string;
  position: GridPosition;
  movable: boolean;
  /** Present when the room came from a Smudgy area. */
  roomNumber?: number;
}

/** A complete, host-independent layout snapshot. */
export interface LayoutModel {
  areaId?: readonly [number, number];
  rooms: readonly LayoutModelRoom[];
  edges: readonly LayoutEdge[];
}

export interface AddRoomLayoutChange {
  type: "add-room";
  from: LayoutRoomKey;
  direction: LayoutDirection;
  temporaryId?: string;
  elevation?: ElevationPreference;
  createReturnEdge?: boolean;
}

export interface ConnectRoomsLayoutChange {
  type: "connect-rooms";
  from: LayoutRoomKey;
  to: LayoutRoomKey;
  direction: LayoutDirection;
  elevation?: ElevationPreference;
  createReturnEdge?: boolean;
}

export interface ReflowLayoutChange {
  type: "reflow";
  anchor?: LayoutRoomKey;
}

export type LayoutChange = AddRoomLayoutChange | ConnectRoomsLayoutChange | ReflowLayoutChange;

export interface PlanLayoutOptions {
  allowExistingMoves?: boolean;
  fixedRooms?: readonly LayoutRoomKey[];
  defaultElevation?: Exclude<ElevationPreference, "auto">;
  /** `thorough` runs a bounded multi-anchor, fixed-point tournament for reflows. */
  effort?: LayoutSearchEffort;
  trace?: (event: LayoutTraceEvent) => void;
}

export interface LayoutMove {
  id: string;
  roomNumber?: number;
  from: GridPosition;
  to: GridPosition;
}

export interface ProposedRoomPlacement {
  id: string;
  position: GridPosition;
}

export interface LayoutPatch {
  moves: readonly LayoutMove[];
  placements: readonly ProposedRoomPlacement[];
}

export interface PlannedLayout {
  before: LayoutModel;
  after: LayoutModel;
  patch: LayoutPatch;
  positions: ReadonlyMap<string, GridPosition>;
  quality: Readonly<LayoutQuality>;
  /** Present when an opt-in thorough reflow tournament was run. */
  search?: Readonly<{
    effort: "thorough";
    anchorsTried: readonly (string | null)[];
    planningPasses: number;
    selectedAnchor: string | null;
    /** Quality from the ordinary single pass at the requested anchor. */
    baselineQuality: Readonly<LayoutQuality>;
  }>;
}

const THOROUGH_ANCHOR_LIMIT = 8;
const THOROUGH_FIXED_POINT_PASSES = 4;

const DIRECTION_VECTORS: Partial<Record<LayoutDirection, GridPosition>> = {
  North: { x: 0, y: -1, level: 0 },
  East: { x: 1, y: 0, level: 0 },
  South: { x: 0, y: 1, level: 0 },
  West: { x: -1, y: 0, level: 0 },
  Northeast: { x: 1, y: -1, level: 0 },
  Northwest: { x: -1, y: -1, level: 0 },
  Southeast: { x: 1, y: 1, level: 0 },
  Southwest: { x: -1, y: 1, level: 0 },
  Up: { x: 0, y: 0, level: 1 },
  Down: { x: 0, y: 0, level: -1 },
};

const OPPOSITE: Partial<Record<LayoutDirection, LayoutDirection>> = {
  North: "South",
  East: "West",
  South: "North",
  West: "East",
  Northeast: "Southwest",
  Northwest: "Southeast",
  Southeast: "Northwest",
  Southwest: "Northeast",
  Up: "Down",
  Down: "Up",
  In: "Out",
  Out: "In",
};

function key(value: LayoutRoomKey): string {
  return String(value);
}

function position(value: GridPosition): GridPosition {
  return {
    x: Math.round(value.x),
    y: Math.round(value.y),
    level: Math.round(value.level),
  };
}

function negate(value: GridPosition): GridPosition {
  return {
    x: -value.x || 0,
    y: -value.y || 0,
    level: -value.level || 0,
  };
}

function samePosition(a: GridPosition, b: GridPosition): boolean {
  return a.x === b.x && a.y === b.y && a.level === b.level;
}

/** Clone arbitrary room/link input into a stable library-owned value. */
export function createLayoutModel(input: LayoutModel): LayoutModel {
  return {
    areaId: input.areaId ? [input.areaId[0], input.areaId[1]] : undefined,
    rooms: input.rooms.map((room) => ({
      ...room,
      id: key(room.id),
      position: position(room.position),
    })),
    edges: input.edges.map((edge) => ({
      ...edge,
      from: key(edge.from),
      to: key(edge.to),
      constraintVector: edge.constraintVector
        ? position(edge.constraintVector)
        : undefined,
    })),
  };
}

function isElevationDirection(direction: LayoutDirection): direction is "Up" | "Down" {
  return direction === "Up" || direction === "Down";
}

function elevationStyle(edge: LayoutEdge): Exclude<ElevationPreference, "auto"> | undefined {
  if (!isElevationDirection(edge.direction)) return undefined;
  const vector = edge.constraintVector ?? DIRECTION_VECTORS[edge.direction];
  return vector && vector.level === 0 ? "projected" : "levels";
}

function adjacency(model: LayoutModel): Map<string, Set<string>> {
  const result = new Map<string, Set<string>>();
  for (const room of model.rooms) result.set(room.id, new Set());
  for (const edge of model.edges) {
    if (!result.has(edge.from) || !result.has(edge.to) || edge.from === edge.to) continue;
    result.get(edge.from)?.add(edge.to);
    result.get(edge.to)?.add(edge.from);
  }
  return result;
}

/** Infer the elevation convention used by the nearest mapped section. */
export function inferElevationPreference(
  model: LayoutModel,
  from: LayoutRoomKey,
  fallback: Exclude<ElevationPreference, "auto"> = "levels",
): Exclude<ElevationPreference, "auto"> {
  const start = key(from);
  const graph = adjacency(model);
  const visited = new Set<string>([start]);
  let frontier = [start];
  while (frontier.length > 0) {
    let levels = 0;
    let projected = 0;
    for (const id of frontier) {
      for (const edge of model.edges) {
        if (edge.from !== id && edge.to !== id) continue;
        const style = elevationStyle(edge);
        if (style === "levels") levels += 1;
        if (style === "projected") projected += 1;
      }
    }
    if (levels !== projected) return projected > levels ? "projected" : "levels";
    if (levels > 0) return fallback;
    const next: string[] = [];
    for (const id of frontier) {
      for (const neighbor of graph.get(id) ?? []) {
        if (visited.has(neighbor)) continue;
        visited.add(neighbor);
        next.push(neighbor);
      }
    }
    frontier = next;
  }
  return fallback;
}

function horizontalNeighbors(
  model: LayoutModel,
  endpoint: string,
  exclude?: string,
): number[] {
  const rooms = new Map(model.rooms.map((room) => [room.id, room.position]));
  const here = rooms.get(endpoint);
  if (!here) return [];
  const result: number[] = [];
  const seen = new Set<string>();
  for (const edge of model.edges) {
    const other = edge.from === endpoint
      ? edge.to
      : edge.to === endpoint
        ? edge.from
        : undefined;
    if (!other || other === exclude || seen.has(other)) continue;
    const there = rooms.get(other);
    if (!there || there.level !== here.level || there.y !== here.y || there.x === here.x) continue;
    seen.add(other);
    result.push(Math.sign(there.x - here.x));
  }
  return result;
}

/**
 * Choose a projected U/D bearing from local path continuity. At the source a
 * lone horizontal stem should continue through the elevation edge; at an
 * existing target the edge should arrive from the side opposite its outgoing
 * stem. Current geometry is retained when those signals tie.
 */
export function chooseProjectedConstraint(
  model: LayoutModel,
  from: LayoutRoomKey,
  direction: "Up" | "Down",
  to?: LayoutRoomKey,
): GridPosition {
  const fromId = key(from);
  const toId = to === undefined ? undefined : key(to);
  const vertical = direction === "Up" ? -1 : 1;
  const sourceNeighbors = horizontalNeighbors(model, fromId, toId);
  const targetNeighbors = toId ? horizontalNeighbors(model, toId, fromId) : [];
  const currentRooms = new Map(model.rooms.map((room) => [room.id, room.position]));
  const currentFrom = currentRooms.get(fromId);
  const currentTo = toId ? currentRooms.get(toId) : undefined;
  const currentLateral = currentFrom && currentTo && currentFrom.level === currentTo.level &&
      currentFrom.x !== currentTo.x
    ? Math.sign(currentTo.x - currentFrom.x)
    : 0;

  const candidates = [-1, 1].map((lateral) => {
    let continuity = 0;
    if (sourceNeighbors.length === 1) continuity -= sourceNeighbors[0] * lateral;
    if (targetNeighbors.length === 1) continuity += targetNeighbors[0] * lateral;
    return {
      lateral,
      continuity,
      preservesCurrent: lateral === currentLateral ? 1 : 0,
      // Match Arctic's historical NE/SW projection only after geometry ties.
      historicalDefault: lateral === (direction === "Up" ? 1 : -1) ? 1 : 0,
    };
  });
  candidates.sort((a, b) =>
    b.continuity - a.continuity ||
    b.preservesCurrent - a.preservesCurrent ||
    b.historicalDefault - a.historicalDefault
  );
  return { x: candidates[0].lateral, y: vertical, level: 0 };
}

/**
 * Classify every existing U/D physical connection from its endpoint levels.
 * Different-level links retain normal vertical semantics. Same-level links
 * receive one coherent projected vector and its exact inverse.
 */
export function resolveElevationGeometry(input: LayoutModel): LayoutModel {
  const model = createLayoutModel(input);
  const rooms = new Map(model.rooms.map((room) => [room.id, room]));
  const edges = model.edges.map((edge) => ({ ...edge }));
  const byLink = new Map<string, number[]>();
  for (let index = 0; index < edges.length; index += 1) {
    const edge = edges[index];
    if (edge.from === edge.to || !isElevationDirection(edge.direction)) continue;
    const pair = edge.from.localeCompare(edge.to) <= 0
      ? `${edge.from}|${edge.to}`
      : `${edge.to}|${edge.from}`;
    const indexes = byLink.get(pair) ?? [];
    indexes.push(index);
    byLink.set(pair, indexes);
  }

  const unresolved = createLayoutModel({ ...model, edges });
  for (const indexes of byLink.values()) {
    const representative = edges[indexes[0]];
    const from = rooms.get(representative.from);
    const to = rooms.get(representative.to);
    if (!from || !to || from.position.level !== to.position.level) continue;
    const vector = chooseProjectedConstraint(
      unresolved,
      representative.from,
      representative.direction as "Up" | "Down",
      representative.to,
    );
    for (const index of indexes) {
      const edge = edges[index];
      edge.constraintVector = edge.from === representative.from && edge.to === representative.to
        ? vector
        : negate(vector);
    }
  }
  return createLayoutModel({ ...model, edges });
}

function residentsForPlan(
  model: LayoutModel,
  anchor: string | undefined,
  options: PlanLayoutOptions,
): LayoutResident[] {
  const fixed = new Set((options.fixedRooms ?? []).map(key));
  if (anchor) fixed.add(anchor);
  return model.rooms.map((room) => ({
    id: room.id,
    position: room.position,
    movable: room.movable && !fixed.has(room.id),
  }));
}

function edgesForDirectionalConnection(
  model: LayoutModel,
  change: AddRoomLayoutChange | ConnectRoomsLayoutChange,
  from: string,
  to: string,
  options: PlanLayoutOptions,
): { edges: LayoutEdge[]; relative: GridPosition } {
  let relative = DIRECTION_VECTORS[change.direction];
  let constraintVector: GridPosition | undefined;
  if (isElevationDirection(change.direction)) {
    const destination = model.rooms.find((room) => room.id === to);
    const source = model.rooms.find((room) => room.id === from);
    const inferred = destination && source
      ? (destination.position.level === source.position.level ? "projected" : "levels")
      : inferElevationPreference(model, from, options.defaultElevation ?? "levels");
    const preference = change.elevation === "auto" || change.elevation === undefined
      ? inferred
      : change.elevation;
    if (preference === "projected") {
      relative = chooseProjectedConstraint(model, from, change.direction);
      constraintVector = relative;
    }
  }
  if (!relative) relative = { x: 1, y: 0, level: 0 };

  const edges: LayoutEdge[] = [
    ...model.edges,
    { from, to, direction: change.direction, constraintVector },
  ];
  const opposite = OPPOSITE[change.direction];
  if (change.createReturnEdge !== false && opposite) {
    edges.push({
      from: to,
      to: from,
      direction: opposite,
      constraintVector: constraintVector ? negate(constraintVector) : undefined,
    });
  }
  return { edges, relative };
}

/** Run the existing deterministic planner once. */
function planLayoutModelOnce(
  input: LayoutModel,
  change: LayoutChange,
  options: PlanLayoutOptions = {},
): PlannedLayout {
  const model = createLayoutModel(input);
  const existing = new Map(model.rooms.map((room) => [room.id, room]));
  let anchor: string | undefined;
  let nodes: LayoutNode[] = [];
  let edges = [...model.edges];
  const proposedIds = new Set<string>();

  if (change.type === "add-room") {
    const from = key(change.from);
    if (!existing.has(from)) throw new Error(`layout source room ${from} does not exist`);
    const temporaryId = change.temporaryId ?? "$new";
    if (existing.has(temporaryId)) throw new Error(`layout id ${temporaryId} already exists`);
    anchor = from;
    const added = edgesForDirectionalConnection(model, change, from, temporaryId, options);
    edges = added.edges;
    nodes = [
      { id: from, relative: { x: 0, y: 0, level: 0 } },
      { id: temporaryId, relative: added.relative },
    ];
    proposedIds.add(temporaryId);
  } else if (change.type === "connect-rooms") {
    const from = key(change.from);
    const to = key(change.to);
    if (!existing.has(from)) throw new Error(`layout source room ${from} does not exist`);
    if (!existing.has(to)) throw new Error(`layout destination room ${to} does not exist`);
    if (from === to) throw new Error("cannot connect a layout room to itself");
    anchor = from;
    const added = edgesForDirectionalConnection(model, change, from, to, options);
    edges = added.edges;
    nodes = [
      { id: from, relative: { x: 0, y: 0, level: 0 } },
      { id: to, relative: added.relative },
    ];
  } else {
    anchor = change.anchor === undefined ? undefined : key(change.anchor);
    if (anchor && !existing.has(anchor)) throw new Error(`layout anchor room ${anchor} does not exist`);
  }

  const plan = planIntegralLayout({
    residents: residentsForPlan(model, anchor, options),
    nodes,
    edges,
    centerId: anchor,
    allowExistingMoves: options.allowExistingMoves !== false,
    trace: options.trace,
  });

  const moves: LayoutMove[] = [];
  const afterRooms: LayoutModelRoom[] = [];
  for (const room of model.rooms) {
    const next = plan.positions.get(room.id) ?? room.position;
    afterRooms.push({ ...room, position: next });
    if (!samePosition(room.position, next)) {
      moves.push({
        id: room.id,
        roomNumber: room.roomNumber,
        from: room.position,
        to: next,
      });
    }
  }
  const placements: ProposedRoomPlacement[] = [];
  for (const id of proposedIds) {
    const next = plan.positions.get(id);
    if (!next) throw new Error(`layout did not place proposed room ${id}`);
    placements.push({ id, position: next });
    afterRooms.push({ id, position: next, movable: true });
  }
  const after = createLayoutModel({
    areaId: model.areaId,
    rooms: afterRooms,
    edges,
  });
  return {
    before: model,
    after,
    patch: { moves, placements },
    positions: plan.positions,
    quality: plan.quality,
  };
}

function changedRoomCount(
  original: LayoutModel,
  positions: ReadonlyMap<string, GridPosition>,
): number {
  let result = 0;
  for (const room of original.rooms) {
    const next = positions.get(room.id);
    if (next && !samePosition(room.position, next)) result += 1;
  }
  return result;
}

/** Positive means `a` is the better tournament result. */
function comparePlannedLayouts(a: PlannedLayout, b: PlannedLayout, original: LayoutModel): number {
  const quality = compareLayoutQuality(a.quality, b.quality);
  if (quality !== 0) return quality;
  return changedRoomCount(original, b.positions) - changedRoomCount(original, a.positions);
}

/** Rebase a later fixed-point pass into one declarative patch from the live snapshot. */
function rebaseReflowPlan(original: LayoutModel, plan: PlannedLayout): PlannedLayout {
  const moves: LayoutMove[] = [];
  const rooms = original.rooms.map((room) => {
    const next = plan.positions.get(room.id) ?? room.position;
    if (!samePosition(room.position, next)) {
      moves.push({
        id: room.id,
        roomNumber: room.roomNumber,
        from: room.position,
        to: next,
      });
    }
    return { ...room, position: next };
  });
  const after = createLayoutModel({
    areaId: original.areaId,
    rooms,
    edges: original.edges,
  });
  return {
    before: original,
    after,
    patch: { moves, placements: [] },
    positions: new Map(after.rooms.map((room) => [room.id, room.position])),
    quality: plan.quality,
  };
}

function thoroughReflowAnchors(
  model: LayoutModel,
  requestedAnchor: string | undefined,
  baseline: PlannedLayout,
  options: PlanLayoutOptions,
): (string | undefined)[] {
  const adjacency = new Map(model.rooms.map((room) => [room.id, new Set<string>()]));
  for (const edge of model.edges) {
    if (edge.from === edge.to || !adjacency.has(edge.from) || !adjacency.has(edge.to)) continue;
    adjacency.get(edge.from)?.add(edge.to);
    adjacency.get(edge.to)?.add(edge.from);
  }

  const directViolations = new Map<string, number>();
  const nearbyViolations = new Map<string, number>();
  for (const edge of directionalViolationEdges(baseline.positions, model.edges)) {
    for (const endpoint of [edge.from, edge.to]) {
      directViolations.set(endpoint, (directViolations.get(endpoint) ?? 0) + 1);
      for (const neighbor of adjacency.get(endpoint) ?? []) {
        if (neighbor !== edge.from && neighbor !== edge.to) {
          nearbyViolations.set(neighbor, (nearbyViolations.get(neighbor) ?? 0) + 1);
        }
      }
    }
  }

  const explicitlyFixed = new Set((options.fixedRooms ?? []).map(key));
  const ranked = model.rooms
    .filter((room) => room.movable && !explicitlyFixed.has(room.id))
    .map((room) => ({
      id: room.id,
      direct: directViolations.get(room.id) ?? 0,
      nearby: nearbyViolations.get(room.id) ?? 0,
      degree: adjacency.get(room.id)?.size ?? 0,
    }))
    .sort((a, b) =>
      b.direct - a.direct ||
      b.nearby - a.nearby ||
      b.degree - a.degree ||
      a.id.localeCompare(b.id)
  );

  const result: (string | undefined)[] = [];
  const seen = new Set<string | undefined>();
  const add = (anchor: string | undefined): void => {
    if (seen.has(anchor) || result.length >= THOROUGH_ANCHOR_LIMIT) return;
    seen.add(anchor);
    result.push(anchor);
  };

  // Preserve the user's point of reference first. Rooms incident to unresolved
  // constraints, then their immediate neighbors, consume the scarce anchor
  // slots before generic high-degree structural anchors.
  add(requestedAnchor);
  for (const candidate of ranked) {
    if (candidate.direct === 0 && candidate.nearby === 0) break;
    // Always reserve one slot for the unrestricted whole-area candidate.
    if (result.length >= THOROUGH_ANCHOR_LIMIT - 1) break;
    add(candidate.id);
  }
  add(undefined);
  for (const candidate of ranked) {
    if (candidate.degree === 0) continue;
    add(candidate.id);
  }
  return result;
}

function reflowToFixedPoint(
  original: LayoutModel,
  anchor: string | undefined,
  options: PlanLayoutOptions,
): { plan: PlannedLayout; passes: number; initialQuality: Readonly<LayoutQuality> } {
  const change: ReflowLayoutChange = { type: "reflow", anchor };
  let current = planLayoutModelOnce(original, change, options);
  const initialQuality = { ...current.quality };
  let best = rebaseReflowPlan(original, current);
  let passes = 1;

  for (let pass = 1; pass < THOROUGH_FIXED_POINT_PASSES; pass += 1) {
    const next = planLayoutModelOnce(current.after, change, options);
    passes += 1;
    // The unchanged layout is always a candidate, so a strict geometry gain
    // is the only useful reason to continue. This also prevents equal-quality
    // coordinate drift and makes the bounded search deterministic.
    if (compareLayoutQuality(next.quality, current.quality) <= 0) break;
    current = next;
    const rebased = rebaseReflowPlan(original, current);
    if (comparePlannedLayouts(rebased, best, original) > 0) best = rebased;
  }
  return { plan: best, passes, initialQuality };
}

function planThoroughReflow(
  input: LayoutModel,
  change: ReflowLayoutChange,
  options: PlanLayoutOptions,
): PlannedLayout {
  const original = createLayoutModel(input);
  const requestedAnchor = change.anchor === undefined ? undefined : key(change.anchor);
  if (requestedAnchor && !original.rooms.some((room) => room.id === requestedAnchor)) {
    throw new Error(`layout anchor room ${requestedAnchor} does not exist`);
  }

  const baseline = reflowToFixedPoint(original, requestedAnchor, options);
  const anchors = thoroughReflowAnchors(original, requestedAnchor, baseline.plan, options);
  let winner = baseline.plan;
  let selectedAnchor = requestedAnchor;
  let planningPasses = baseline.passes;

  for (const anchor of anchors.slice(1)) {
    const candidate = reflowToFixedPoint(original, anchor, options);
    planningPasses += candidate.passes;
    if (comparePlannedLayouts(candidate.plan, winner, original) > 0) {
      winner = candidate.plan;
      selectedAnchor = anchor;
    }
  }

  return {
    ...winner,
    search: {
      effort: "thorough",
      anchorsTried: anchors.map((anchor) => anchor ?? null),
      planningPasses,
      selectedAnchor: selectedAnchor ?? null,
      baselineQuality: baseline.initialQuality,
    },
  };
}

/** Plan one change against an already materialized layout snapshot. */
export function planLayoutModel(
  input: LayoutModel,
  change: LayoutChange,
  options: PlanLayoutOptions = {},
): PlannedLayout {
  if (options.effort === "thorough" && change.type === "reflow" &&
    options.allowExistingMoves !== false) {
    return planThoroughReflow(input, change, options);
  }
  return planLayoutModelOnce(input, change, options);
}

/** Optional retained-snapshot API for consumers which map continuously. */
export class LayoutWorkspace {
  #model: LayoutModel;
  #pending = new WeakSet<PlannedLayout>();

  constructor(model: LayoutModel) {
    this.#model = createLayoutModel(model);
  }

  get model(): LayoutModel {
    return this.#model;
  }

  plan(change: LayoutChange, options?: PlanLayoutOptions): PlannedLayout {
    const result = planLayoutModel(this.#model, change, options);
    this.#pending.add(result);
    return result;
  }

  accept(result: PlannedLayout): void {
    if (!this.#pending.has(result)) {
      throw new Error("cannot accept a layout planned from a different snapshot");
    }
    this.#model = result.after;
    this.#pending = new WeakSet();
  }
}

export function createLayoutWorkspace(model: LayoutModel): LayoutWorkspace {
  return new LayoutWorkspace(model);
}
