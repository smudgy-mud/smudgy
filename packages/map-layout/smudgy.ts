import { mapper } from "smudgy:core";
import type {
  LayoutDirection,
  LayoutEdge,
  LayoutWorkerControlOptions,
} from "./layout.ts";
import {
  createLayoutModel,
  resolveElevationGeometry,
  type LayoutChange,
  type LayoutModel,
  type PlanLayoutAsyncOptions,
  type PlannedLayout,
} from "./model.ts";
import {
  layoutSnapshotKey,
  planStableLayoutSnapshot,
  StaleLayoutSnapshotError,
} from "./stable-snapshot.ts";

export { layoutSnapshotKey, StaleLayoutSnapshotError };

export interface LoadLayoutModelOptions {
  isRoomMovable?: (room: Room) => boolean;
}

export interface PlanAreaChangeOptions
  extends PlanLayoutAsyncOptions, LoadLayoutModelOptions, LayoutWorkerControlOptions {
  /** Return exact source/planned model keys for a later cross-realm commit guard. */
  includeSnapshotKeys?: boolean;
}

/** Stateless façade result: the temporary layout model stays private. */
export interface AreaChangePlan extends Pick<
  PlannedLayout,
  "patch" | "positions" | "quality" | "search" | "constraintRepair"
> {
  /** Exact stable source model accepted after Worker planning. */
  sourceSnapshotKey?: string;
  /** Exact source model with the returned moves applied. */
  plannedSnapshotKey?: string;
}

function idsMatch(a: AreaId | null, b: AreaId | null): boolean {
  return !!a && !!b && a === b;
}

function resolveArea(area: Area | AreaId): Area {
  // An AreaId is a string primitive, so `in` would throw here -- discriminate
  // on the type, not on a property probe.
  return typeof area === "string" ? mapper.getAreaById(area) : area;
}

function defaultRoomMovable(room: Room): boolean {
  return !room.hasTag("LAYOUT_LOCKED") && room.data("layoutLocked") !== "true";
}

/**
 * Copy one Smudgy area into a host-independent layout model. No live Area,
 * Room, or Exit wrappers are retained after this function returns.
 */
export function loadLayoutModel(
  source: Area | AreaId,
  options: LoadLayoutModelOptions = {},
): LayoutModel {
  const area = resolveArea(source);
  const areaId: AreaId = area.id;
  const movable = options.isRoomMovable ?? defaultRoomMovable;
  // Materialize every host wrapper exactly once. All work below this point is
  // performed against these primitives rather than repeated atomic reads.
  const rooms = area.room_numbers.flatMap((number) => {
    const room = area.room(number);
    if (!room) return [];
    return [{
      roomNumber: room.room_number,
      position: { x: room.x, y: room.y, level: room.level },
      movable: movable(room),
      exits: room.exits.map((exit) => ({
        fromDirection: exit.from_direction,
        toAreaId: exit.to_area_id ?? null,
        toRoomNumber: exit.to_room_number,
      })),
    }];
  });
  const known = new Set(rooms.map((room) => room.roomNumber));
  const edges: LayoutEdge[] = [];
  const seen = new Set<string>();

  for (const room of rooms) {
    for (const exit of room.exits) {
      if (!exit.toAreaId || exit.toRoomNumber === null ||
        !idsMatch(exit.toAreaId, areaId) || !known.has(exit.toRoomNumber)) continue;
      const id = `${room.roomNumber}>${exit.toRoomNumber}:${exit.fromDirection}`;
      if (seen.has(id)) continue;
      seen.add(id);
      edges.push({
        from: String(room.roomNumber),
        to: String(exit.toRoomNumber),
        direction: exit.fromDirection as LayoutDirection,
      });
    }
  }

  return resolveElevationGeometry(createLayoutModel({
    areaId,
    rooms: rooms.map((room) => ({
      id: String(room.roomNumber),
      roomNumber: room.roomNumber,
      position: room.position,
      movable: room.movable,
    })),
    edges,
  }));
}

/**
 * The model `loadLayoutModel` reads back once `planned` is applied: its rooms
 * where the plan puts them, with Up/Down geometry resolved from those levels
 * rather than from the levels the plan started at.
 */
function reloadedLayoutModel(planned: LayoutModel): LayoutModel {
  return resolveElevationGeometry(createLayoutModel({
    ...planned,
    edges: planned.edges.map(({ constraintVector: _resolved, ...edge }) => edge),
  }));
}

/**
 * Stateless Smudgy façade. It snapshots immediately before the expensive
 * operation, plans entirely in V8-owned data, and returns a declarative patch.
 */
export async function planAreaChange(
  area: Area | AreaId,
  change: LayoutChange,
  options: PlanAreaChangeOptions = {},
): Promise<AreaChangePlan> {
  // Always re-resolve a host wrapper by ID. An Area supplied by the caller may
  // itself be an immutable snapshot and cannot validate a later Worker result.
  const liveArea: AreaId = typeof area === "string" ? area : area.id;
  const planOptions: PlanLayoutAsyncOptions = {
    allowExistingMoves: options.allowExistingMoves,
    fixedRooms: options.fixedRooms,
    defaultElevation: options.defaultElevation,
    effort: options.effort,
    maxPlanningPasses: options.maxPlanningPasses,
    constraintRepair: options.constraintRepair,
    trace: options.trace,
  };
  const result = await planStableLayoutSnapshot(
    () => loadLayoutModel(liveArea, { isRoomMovable: options.isRoomMovable }),
    change,
    planOptions,
    { signal: options.signal, timeoutMs: options.timeoutMs },
  );
  const { before, after, patch, positions, quality, search, constraintRepair } = result;
  return {
    patch,
    positions,
    quality,
    search,
    ...(options.includeSnapshotKeys
      ? {
        sourceSnapshotKey: layoutSnapshotKey(before),
        plannedSnapshotKey: layoutSnapshotKey(reloadedLayoutModel(after)),
      }
      : {}),
    ...(constraintRepair ? { constraintRepair } : {}),
  };
}
