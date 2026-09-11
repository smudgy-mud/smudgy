import type {
  LayoutEdge,
  LayoutTraceEvent,
  LayoutWorkerControlOptions,
} from "./layout.ts";
import {
  planLayoutModelAsync,
  type LayoutChange,
  type LayoutModel,
  type LayoutModelRoom,
  type PlannedLayout,
  type PlanLayoutOptions,
} from "./model.ts";

const STALE_SNAPSHOT_RETRIES = 1;

function edgeKey(edge: LayoutEdge): string {
  return JSON.stringify([
    edge.from,
    edge.to,
    edge.direction,
    edge.constraintVector?.x ?? null,
    edge.constraintVector?.y ?? null,
    edge.constraintVector?.level ?? null,
  ]);
}

function roomKey(room: LayoutModelRoom): string {
  return JSON.stringify([
    room.id,
    room.roomNumber ?? null,
    room.position.x,
    room.position.y,
    room.position.level,
    room.movable,
  ]);
}

/** Compare the inputs that can affect a plan, independent of host enumeration order. */
export function sameLayoutSnapshot(a: LayoutModel, b: LayoutModel): boolean {
  if (a.areaId !== b.areaId ||
    a.rooms.length !== b.rooms.length || a.edges.length !== b.edges.length) return false;

  const aRooms = a.rooms.map(roomKey).sort();
  const bRooms = b.rooms.map(roomKey).sort();
  if (!aRooms.every((room, index) => room === bRooms[index])) return false;
  const aEdges = a.edges.map(edgeKey).sort();
  const bEdges = b.edges.map(edgeKey).sort();
  return aEdges.every((edge, index) => edge === bEdges[index]);
}

export class StaleLayoutSnapshotError extends Error {
  readonly attempts: number;

  constructor(attempts: number) {
    super(`layout area changed during ${attempts} Worker planning attempts`);
    this.name = "StaleLayoutSnapshotError";
    this.attempts = attempts;
  }
}

/**
 * Plan from a snapshot, then verify that the live source still matches it.
 * One stale result is discarded and retried. Diagnostic events are buffered so
 * callers only observe the trace belonging to the accepted result.
 */
export async function planStableLayoutSnapshot(
  loadSnapshot: () => LayoutModel,
  change: LayoutChange,
  options: PlanLayoutOptions = {},
  control: LayoutWorkerControlOptions = {},
): Promise<PlannedLayout> {
  const startedAt = performance.now();
  const trace = options.trace;
  // Attempts re-plan across awaits; a deep copy insulates them from caller
  // mutation of the change, including any nested members the union may carry.
  const stableChange = structuredClone(change);
  const stableOptions: PlanLayoutOptions = {
    allowExistingMoves: options.allowExistingMoves,
    fixedRooms: options.fixedRooms ? [...options.fixedRooms] : undefined,
    defaultElevation: options.defaultElevation,
    effort: options.effort,
    maxPlanningPasses: options.maxPlanningPasses,
  };
  const attempts = STALE_SNAPSHOT_RETRIES + 1;
  for (let attempt = 0; attempt < attempts; attempt += 1) {
    const snapshot = loadSnapshot();
    const traceEvents: LayoutTraceEvent[] | undefined = trace ? [] : undefined;
    const timeoutMs = control.timeoutMs === undefined
      ? undefined
      : Math.max(0, control.timeoutMs - (performance.now() - startedAt));
    const result = await planLayoutModelAsync(
      snapshot,
      stableChange,
      {
        ...stableOptions,
        trace: traceEvents ? (event) => traceEvents.push(event) : undefined,
      },
      { signal: control.signal, timeoutMs },
    );
    if (!sameLayoutSnapshot(snapshot, loadSnapshot())) continue;

    if (trace && traceEvents) {
      for (const event of traceEvents) trace(event);
    }
    return result;
  }
  throw new StaleLayoutSnapshotError(attempts);
}
