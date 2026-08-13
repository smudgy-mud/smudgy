export const NUKEFIRE_AREA_ID_PROPERTY = "nukefire.zone";

export interface NukeFireAreaCandidate {
  readonly name: string;
  readonly storage: MapStorage;
  data(key: string): string | undefined;
}

/** Whether an existing area belongs to the mapper's configured storage tier. */
export function isAdoptableStorage(candidate: MapStorage, configured: MapStorage): boolean {
  return candidate === configured;
}

/**
 * Find the area already tagged with NukeFire's area identity in the configured
 * storage tier. Moving a NukeFire area between tiers is an explicit user
 * action; automatic mapping must never adopt or write through another tier.
 */
export function findAreaByNukeFireId<T extends NukeFireAreaCandidate>(
  areas: readonly T[],
  storage: MapStorage,
  areaId: number | string,
): T | undefined {
  const expected = String(areaId);
  const matches = areas.filter((candidate) =>
    isAdoptableStorage(candidate.storage, storage) &&
    candidate.data(NUKEFIRE_AREA_ID_PROPERTY) === expected
  );
  return matches[0];
}

/**
 * Fall back to a display-name match only when it is unclaimed or already
 * carries the requested NukeFire area identity. Only the configured storage
 * tier participates in this fallback.
 */
export function findCompatibleAreaByName<T extends NukeFireAreaCandidate>(
  areas: readonly T[],
  storage: MapStorage,
  areaId: number | string,
  name: string,
): T | undefined {
  const expected = String(areaId);
  const matches = areas.filter((candidate) => {
    if (!isAdoptableStorage(candidate.storage, storage)) return false;
    const candidateAreaId = candidate.data(NUKEFIRE_AREA_ID_PROPERTY);
    return (candidateAreaId === undefined || candidateAreaId === expected) &&
      candidate.name.localeCompare(name, undefined, { sensitivity: "accent" }) === 0;
  });
  return matches[0];
}

/**
 * The area one observed room is written into. A vnum the map already contains
 * keeps its room's area even when the snapshot's zone resolves to a different
 * one — border rooms are reported by both neighboring zones, and re-creating a
 * known vnum in the zone's area would duplicate it under the same externalId.
 * Only unknown vnums land in the zone's area.
 */
export function areaForObservedRoom<T>(
  zoneArea: T | undefined,
  knownRoomArea: T | undefined,
): T | undefined {
  return knownRoomArea ?? zoneArea;
}
