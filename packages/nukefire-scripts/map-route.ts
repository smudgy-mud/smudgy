export type RouteDirection =
  | "North"
  | "East"
  | "South"
  | "West"
  | "Up"
  | "Down"
  | "Northeast"
  | "Northwest"
  | "Southeast"
  | "Southwest"
  | "In"
  | "Out"
  | "Special"
  | "Other";

/** An area id as the mapper spells it: a canonical UUID string. */
export type MapRouteAreaId = string;

export interface RouteExit {
  from_direction: RouteDirection;
  to_area_id: MapRouteAreaId | null;
  to_room_number: number | null;
  command: string | null;
}

export interface RouteRoom {
  area_id: MapRouteAreaId;
  room_number: number;
  exits: readonly RouteExit[];
}

export type RouteRoomLookup = (
  areaId: MapRouteAreaId,
  roomNumber: number,
) => RouteRoom | undefined;

/** The name this module's output expects in the MapView styles palette. */
export const ROUTE_STYLE = "route";
/** The style which keeps cross-area labels at the current room visible. */
export const CURRENT_ROOM_STYLE = "current-room";

/** One resolved application of {@link ROUTE_STYLE}, in MapView `apply` shape. */
export interface RouteStyleApplication {
  style: typeof ROUTE_STYLE;
  rooms: number[];
  exits: { room: number; direction: RouteDirection }[];
}

export interface CurrentRoomStyleApplication {
  style: typeof CURRENT_ROOM_STYLE;
  exits: { room: number; direction: RouteDirection }[];
}

const DIRECTIONS: Readonly<Record<string, string>> = {
  n: "north",
  e: "east",
  s: "south",
  w: "west",
  u: "up",
  d: "down",
};

function sameArea(
  left: MapRouteAreaId,
  right: MapRouteAreaId,
): boolean {
  return left === right;
}

function normalizedCommand(command: string | null): string {
  return (command ?? "").trim().toLowerCase();
}

function exitForStep(room: RouteRoom, step: string): RouteExit | undefined {
  const direction = DIRECTIONS[step];
  if (!direction) return undefined;
  // Explicit traversal commands win. This matters when a room has a Special
  // exit alongside an ordinary direction which happens to share its command.
  return room.exits.find((exit) => normalizedCommand(exit.command) === step) ??
    room.exits.find((exit) => exit.from_direction.toLowerCase() === direction);
}

/** Select every exit anchored at the current room. The associated style only
 * changes cross-area label visibility, so ordinary exits are harmless here
 * and redacted/dangling destinations do not need to be distinguished. */
export function currentRoomMapViewApply(
  room: RouteRoom | undefined,
): CurrentRoomStyleApplication[] {
  if (!room || room.exits.length === 0) return [];
  return [{
    style: CURRENT_ROOM_STYLE,
    exits: room.exits.map((exit) => ({
      room: room.room_number,
      direction: exit.from_direction,
    })),
  }];
}

/**
 * Resolve NukeFire's compact GPS route against the durable Smudgy topology,
 * in MapView `apply` shape: one {@link ROUTE_STYLE} application carrying the
 * traversed rooms and exact exits (empty when there is no resolvable start).
 *
 * A MapView displays one area at a time, so resolution stops when the route
 * leaves the starting area or reaches topology the mapper has not learned yet.
 * The listed rooms drive room accents; the exact source room + direction for
 * each step selects only the traversed Connection, including Up/Down and the
 * visible end of an outbound cross-area route.
 */
export function mapViewRoute(
  start: RouteRoom | undefined,
  routeRaw: string,
  lookup: RouteRoomLookup,
): RouteStyleApplication[] {
  if (!start) return [];
  const areaId = start.area_id;
  const rooms = [start.room_number];
  const exits: { room: number; direction: RouteDirection }[] = [];
  let room = start;

  for (const step of routeRaw.trim().toLowerCase()) {
    const exit = exitForStep(room, step);
    if (!exit) break;
    exits.push({ room: room.room_number, direction: exit.from_direction });
    if (!exit.to_area_id || exit.to_room_number === null) break;
    if (!sameArea(areaId, exit.to_area_id)) break;
    const next = lookup(exit.to_area_id, exit.to_room_number);
    if (!next) break;
    rooms.push(next.room_number);
    room = next;
  }
  return [{ style: ROUTE_STYLE, rooms, exits }];
}

/** Accept the live `route_raw` spelling and the older GMCP package's `route`. */
export function gpsRouteRaw(gps: { route_raw?: unknown; route?: unknown } | undefined): string {
  if (typeof gps?.route_raw === "string") return gps.route_raw;
  return typeof gps?.route === "string" ? gps.route : "";
}
