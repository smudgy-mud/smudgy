import { mapper } from "smudgy:core";
import type { LayoutDirection, LayoutEdge } from "./layout.ts";
import {
  createLayoutModel,
  planLayoutModel,
  resolveElevationGeometry,
  type LayoutChange,
  type LayoutModel,
  type PlanLayoutOptions,
  type PlannedLayout,
} from "./model.ts";

export interface LoadLayoutModelOptions {
  isRoomMovable?: (room: Room) => boolean;
}

/** Stateless façade result: the temporary layout model stays private. */
export type AreaChangePlan = Pick<PlannedLayout, "patch" | "positions" | "quality" | "search">;

function idsMatch(a: AreaId | null, b: AreaId | null): boolean {
  return !!a && !!b && a[0] === b[0] && a[1] === b[1];
}

function resolveArea(area: Area | AreaId): Area {
  return "room_numbers" in area ? area : mapper.getAreaById(area);
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
  const areaId: AreaId = [area.id[0], area.id[1]];
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
        toAreaId: exit.to_area_id
          ? [exit.to_area_id[0], exit.to_area_id[1]] as AreaId
          : null,
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
 * Stateless Smudgy façade. It snapshots immediately before the expensive
 * operation, plans entirely in V8-owned data, and returns a declarative patch.
 */
export async function planAreaChange(
  area: Area | AreaId,
  change: LayoutChange,
  options: PlanLayoutOptions & LoadLayoutModelOptions = {},
): Promise<AreaChangePlan> {
  const model = loadLayoutModel(area, options);
  const { patch, positions, quality, search } = planLayoutModel(model, change, options);
  return { patch, positions, quality, search };
}
