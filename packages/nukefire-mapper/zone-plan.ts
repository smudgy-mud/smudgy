/**
 * Where a zone's rooms belong, decided from what the maps hold now.
 *
 * Every room records its zone. Once the player has stood in a zone, all of
 * the zone's rooms belong in the one map for that zone's area name. This
 * module only decides; the caller reads the facts from the mapper and carries
 * the plan out, so an interrupted plan is simply computed again next visit.
 */

import type { MapName } from "./area-names.ts";

/** One map, as settling a zone sees it. */
export interface MapFacts {
  readonly id: AreaId;
  readonly name: string;
  /** Its key, when it is the map for an area name. */
  readonly key: string | undefined;
  /** The key its current name resolves to under the area-name rules. */
  readonly nameKey: string | undefined;
  /** Whether the mapper made it. Every other map is the player's, and never touched. */
  readonly managed: boolean;
  /** Whether its name is a placeholder waiting for its zone to be named. */
  readonly placeholder: boolean;
  /** The zone a placeholder or an older version's per-zone map was made for. */
  readonly madeFor: number | undefined;
  readonly roomCount: number;
  /** Its rooms in this zone. */
  readonly zoneRooms: readonly RoomNumber[];
}

export type Destination =
  | { readonly kind: "existing"; readonly id: AreaId }
  | { readonly kind: "adopt"; readonly id: AreaId; readonly rename: boolean }
  | { readonly kind: "create" };

/** A map to take from: all of it, after which it is deleted, or only the listed rooms. */
export interface Source {
  readonly id: AreaId;
  readonly rooms?: readonly RoomNumber[];
}

export type ZonePlan =
  | { readonly kind: "settle"; readonly destination: Destination; readonly sources: readonly Source[] }
  /** Most of the zone is in a map the player made: it stays as they arranged it. */
  | { readonly kind: "player"; readonly id: AreaId };

const rooms = (map: MapFacts) => map.roomCount;
const zoneRooms = (map: MapFacts) => map.zoneRooms.length;
const onlyThisZone = (map: MapFacts) => map.zoneRooms.length > 0 && map.zoneRooms.length === map.roomCount;

/** The highest-scoring map; ties go to the lowest id, so plans are deterministic. */
function best(maps: readonly MapFacts[], ...scores: ((map: MapFacts) => number)[]): MapFacts | undefined {
  return [...maps].sort((a, b) => {
    for (const score of scores) {
      const difference = score(b) - score(a);
      if (difference !== 0) return difference;
    }
    return a.id < b.id ? -1 : a.id > b.id ? 1 : 0;
  })[0];
}

/** Where the rooms of `zone`, which the game calls `name`, go, and which maps give them up. */
export function planZone(zone: number, name: MapName, maps: readonly MapFacts[]): ZonePlan {
  const holders = maps.filter((map) => map.zoneRooms.length > 0);
  const mostOfZone = best(holders, zoneRooms, rooms);
  if (mostOfZone && !mostOfZone.managed) return { kind: "player", id: mostOfZone.id };

  const managed = maps.filter((map) => map.managed);
  const claimant = best(managed.filter((map) => map.key === name.key), rooms);
  // A map an older version made for another zone stays that zone's map,
  // whatever rooms of this zone strayed into it.
  const anotherZones = (map: MapFacts) => map.madeFor !== undefined && map.madeFor !== zone;
  // Without a map for the name yet, prefer the family's largest map, so the
  // map that moves the least survives; then whichever map holds the zone.
  const adoptee = claimant ? undefined :
    best(managed.filter((map) => !map.key && map.nameKey === name.key), rooms) ??
    best(
      holders.filter((map) => map.managed && !anotherZones(map) && (!map.key || onlyThisZone(map))),
      zoneRooms,
      rooms,
    ) ??
    best(managed.filter((map) => map.madeFor === zone && map.roomCount === 0));
  const into = claimant ?? adoptee;

  const destination: Destination = claimant
    ? { kind: "existing", id: claimant.id }
    : adoptee
    ? {
      kind: "adopt",
      id: adoptee.id,
      // A placeholder or a spelling of this area takes the map's title; any other name is the player's.
      rename: adoptee.name !== name.display && (adoptee.placeholder || adoptee.nameKey === name.key),
    }
    : { kind: "create" };

  const sources = managed.filter((map) => map !== into).flatMap((map): Source[] => {
    if (map.key === name.key || onlyThisZone(map)) return [{ id: map.id }];
    if (map.zoneRooms.length > 0) return [{ id: map.id, rooms: map.zoneRooms }];
    if (map.madeFor === zone && map.roomCount === 0) return [{ id: map.id }];
    return [];
  });
  return { kind: "settle", destination, sources };
}

/** The map new rooms of a zone the player has only glimpsed join, or undefined when it needs a placeholder. */
export function mapForGlimpsedZone(zone: number, maps: readonly MapFacts[]): AreaId | undefined {
  const holder = best(maps.filter((map) => map.zoneRooms.length > 0), zoneRooms, rooms);
  return (holder ?? best(maps.filter((map) => map.managed && map.madeFor === zone), rooms))?.id;
}
