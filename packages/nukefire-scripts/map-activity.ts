import type { MapStyleApplication } from "smudgy:widgets";
import type { RouteDirection } from "./map-route.ts";

export const UNVISITED_STYLE = "unvisited";
export const TRACK_STYLE = "tracks";
export const VISITED_STORAGE_KEY = "nf-map-visited-v1";
/**
 * Visited rooms by the game's own room number (vnum), which a room keeps when
 * its map section is combined into another map or its room number changes.
 */
export const VISITED_VNUMS_STORAGE_KEY = "nf-map-visited-vnums-v1";

export interface MapLocation {
  areaId: string;
  roomNumber: number | null;
}

export interface MapExitRef {
  room: number;
  direction: RouteDirection;
  /** Session-local exact identity for parallel/synthetic endpoint expansion. */
  connectionKey?: string;
}

export interface TrackTraversal {
  connectionKey: string;
  room: number;
  direction: RouteDirection;
  toRoom: number | null;
  toDirection: RouteDirection | null;
}

export interface TrackConnection {
  key: string;
  endpointA: { room: number; side: "North" | "East" | "South" | "West" };
  endpointB: { room: number; side: "North" | "East" | "South" | "West" } | null;
}

export interface AreaTrack {
  rooms: number[];
  exits: MapExitRef[];
}

export interface SessionMapSnapshot {
  location?: MapLocation;
  tracks: Record<string, AreaTrack>;
}

export type VisitedRooms = Record<string, number[]>;

function validRoom(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value);
}

export function parseVisitedRooms(raw: string | null): VisitedRooms {
  if (!raw) return {};
  try {
    const value = JSON.parse(raw) as unknown;
    if (!value || typeof value !== "object" || Array.isArray(value)) return {};
    const result: VisitedRooms = {};
    for (const [areaId, rooms] of Object.entries(value)) {
      if (!Array.isArray(rooms)) continue;
      result[areaId] = [...new Set(rooms.filter(validRoom))].sort((a, b) => a - b);
    }
    return result;
  } catch {
    return {};
  }
}

export function rememberVisitedRoom(
  visited: VisitedRooms,
  location: MapLocation,
): VisitedRooms {
  if (location.roomNumber === null) return visited;
  const rooms = new Set(visited[location.areaId] ?? []);
  rooms.add(location.roomNumber);
  return {
    ...visited,
    [location.areaId]: [...rooms].sort((a, b) => a - b),
  };
}

/** Visited vnums, unique and sorted; anything that is not a room number is dropped. */
export function parseVisitedVnums(raw: string | null): number[] {
  if (!raw) return [];
  try {
    const value = JSON.parse(raw) as unknown;
    return Array.isArray(value)
      ? [...new Set(value.filter(validRoom))].sort((a, b) => a - b)
      : [];
  } catch {
    return [];
  }
}

/** The game's room number a mapper stores as a room's external id, if it is one. */
export function roomVnum(externalId: string | undefined): number | undefined {
  if (externalId === undefined || !/^\d+$/.test(externalId)) return undefined;
  const vnum = Number(externalId);
  return Number.isSafeInteger(vnum) ? vnum : undefined;
}

/**
 * The vnums of the rooms `visited` lists that `vnumOf` can still find, so
 * visits recorded by map and room number also hold once maps are combined.
 */
export function visitedRoomVnums(
  visited: VisitedRooms,
  vnumOf: (areaId: string, roomNumber: number) => number | undefined,
): number[] {
  const vnums = new Set<number>();
  for (const [areaId, rooms] of Object.entries(visited)) {
    for (const roomNumber of rooms) {
      const vnum = vnumOf(areaId, roomNumber);
      if (vnum !== undefined) vnums.add(vnum);
    }
  }
  return [...vnums].sort((a, b) => a - b);
}

export function markAreaUnvisited(visited: VisitedRooms, areaId: string): VisitedRooms {
  const next = { ...visited };
  delete next[areaId];
  return next;
}

function sameLocation(left: MapLocation | undefined, right: MapLocation): boolean {
  return left?.areaId === right.areaId && left.roomNumber === right.roomNumber;
}

function addRoom(tracks: Record<string, AreaTrack>, areaId: string, room: number): void {
  const track = tracks[areaId] ??= { rooms: [], exits: [] };
  if (!track.rooms.includes(room)) track.rooms.push(room);
}

/** Add one location transition while keeping style payloads deduplicated. */
export function advanceTrack(
  snapshot: SessionMapSnapshot | undefined,
  location: MapLocation,
  traversed?: { areaId: string; exit: MapExitRef },
): SessionMapSnapshot {
  const tracks = structuredClone(snapshot?.tracks ?? {});
  const previous = snapshot?.location;
  if (sameLocation(previous, location)) return { location, tracks };

  if (previous?.roomNumber !== null && previous?.roomNumber !== undefined) {
    addRoom(tracks, previous.areaId, previous.roomNumber);
  }
  if (location.roomNumber !== null) addRoom(tracks, location.areaId, location.roomNumber);

  if (traversed) {
    const track = tracks[traversed.areaId] ??= { rooms: [], exits: [] };
    if (!track.exits.some((candidate) =>
      candidate.room === traversed.exit.room &&
      candidate.direction === traversed.exit.direction &&
      candidate.connectionKey === traversed.exit.connectionKey
    )) {
      track.exits.push(traversed.exit);
    }
  }
  return { location, tracks };
}

const OPPOSITE: Readonly<Record<RouteDirection, RouteDirection>> = {
  North: "South",
  East: "West",
  South: "North",
  West: "East",
  Up: "Down",
  Down: "Up",
  Northeast: "Southwest",
  Northwest: "Southeast",
  Southeast: "Northwest",
  Southwest: "Northeast",
  In: "Out",
  Out: "In",
  Special: "Special",
  Other: "Other",
};

function endpointDirection(
  endpoint: TrackConnection["endpointA"],
  members: readonly TrackTraversal[],
): RouteDirection {
  const departing = members.find((member) => member.room === endpoint.room);
  if (departing) return departing.direction;
  const arriving = members.find((member) => member.toRoom === endpoint.room);
  if (arriving) return arriving.toDirection ?? OPPOSITE[arriving.direction];
  return endpoint.side;
}

/**
 * Expand traversed exits to the endpoint keys MapView actually renders.
 * One-way connections may be canonically anchored at the endpoint which has
 * no member Exit, so enumerating stored exits alone cannot style that line.
 */
export function expandTrackedExitRefs(
  refs: readonly MapExitRef[],
  traversals: readonly TrackTraversal[],
  connections: readonly TrackConnection[],
): MapExitRef[] {
  const expanded: MapExitRef[] = [];
  const add = (room: number, direction: RouteDirection): void => {
    if (!expanded.some((candidate) =>
      candidate.room === room && candidate.direction === direction
    )) expanded.push({ room, direction });
  };
  for (const ref of refs) {
    const member = ref.connectionKey
      ? traversals.find((candidate) => candidate.connectionKey === ref.connectionKey)
      : traversals.find((candidate) =>
        candidate.room === ref.room && candidate.direction === ref.direction
      );
    const key = ref.connectionKey ?? member?.connectionKey;
    const connection = connections.find((candidate) => candidate.key === key);
    if (!connection) {
      add(ref.room, ref.direction);
      continue;
    }
    const members = traversals.filter((candidate) => candidate.connectionKey === connection.key);
    add(connection.endpointA.room, endpointDirection(connection.endpointA, members));
    if (connection.endpointB) {
      add(connection.endpointB.room, endpointDirection(connection.endpointB, members));
    }
  }
  return expanded;
}

export function clearTracks(snapshot: SessionMapSnapshot | undefined): SessionMapSnapshot {
  return { location: snapshot?.location, tracks: {} };
}

export function trackApplication(
  areaId: string,
  snapshot: SessionMapSnapshot | undefined,
): MapStyleApplication | undefined {
  const track = snapshot?.tracks[areaId];
  if (!track || (track.rooms.length === 0 && track.exits.length === 0)) return undefined;
  return {
    style: TRACK_STYLE,
    area: areaId,
    rooms: [...track.rooms],
    exits: [...track.exits],
  };
}
