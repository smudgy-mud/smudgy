// The current-location marker is per-session, but shared mapping has a single
// writer: only the owner session's NukeFireMapper calls setCurrentLocation as
// it applies snapshots. Non-owner sessions instead follow Room.Info read-only,
// resolving the reported vnum against the already-written map so their
// MapView keeps tracking the player. Nothing here writes to the map.

export interface FollowRoom {
  readonly area_id: AreaId;
  readonly room_number: RoomNumber;
  readonly externalId: string | undefined;
}

export interface FollowArea {
  readonly id: AreaId;
  readonly storage: MapStorage;
  readonly room_numbers: RoomNumber[];
  room(roomNumber: number): FollowRoom | undefined;
}

export interface FollowMapper {
  findRoomByExternalId(externalId: string): FollowRoom | undefined;
  getCurrentLocation(): { area: AreaId; room?: RoomNumber } | undefined;
  /** Throws for an unknown (e.g. just-deleted) area id. */
  getAreaById(id: AreaId): FollowArea;
  readonly areas: FollowArea[];
}

export interface FollowedLocation {
  area: AreaId;
  room: RoomNumber;
}

function sameAreaId(a: AreaId, b: AreaId): boolean {
  return a === b;
}

function roomInArea(area: FollowArea, externalId: string): FollowRoom | undefined {
  for (const number of area.room_numbers) {
    const room = area.room(number);
    if (room?.externalId === externalId) return room;
  }
  return undefined;
}

/**
 * Resolve a server room id to the map location a non-owner session should
 * show. `findRoomByExternalId` returns one of possibly several bindings (a
 * durable map adopted from another tier can coexist with a stale duplicate),
 * so an ambiguous id prefers the area the session is already viewing, then
 * any durable binding over a session one.
 */
export function resolveFollowedLocation(
  mapper: FollowMapper,
  externalId: string,
): FollowedLocation | undefined {
  const found = mapper.findRoomByExternalId(externalId);
  if (!found) return undefined;

  const current = mapper.getCurrentLocation();
  if (current && !sameAreaId(found.area_id, current.area)) {
    try {
      const stayed = roomInArea(mapper.getAreaById(current.area), externalId);
      if (stayed) return { area: stayed.area_id, room: stayed.room_number };
    } catch {
      // The viewed area disappeared; fall through to the global binding.
    }
  }

  let foundStorage: MapStorage | undefined;
  try {
    foundStorage = mapper.getAreaById(found.area_id).storage;
  } catch {
    return undefined;
  }
  if (foundStorage !== "session") {
    return { area: found.area_id, room: found.room_number };
  }
  for (const area of mapper.areas) {
    if (area.storage === "session") continue;
    const durable = roomInArea(area, externalId);
    if (durable) return { area: durable.area_id, room: durable.room_number };
  }
  return { area: found.area_id, room: found.room_number };
}
