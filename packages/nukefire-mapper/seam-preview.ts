/**
 * The seam preview, apart from the mapper that runs it. When merging moves
 * map sections into a map, the next polish of that map first polishes only
 * the rooms around the seams, a round of quick planner passes whose layouts
 * go on the map as they arrive, and then polishes the whole map as it would
 * have without the round: from the merged geometry, not from the preview.
 * This module holds which rooms are seams, when a pass of the round gained
 * enough to run another, and which geometry the whole-map polish plans from.
 */

/** Passes the seam round runs at most, each from the best layout so far. */
export const SEAM_ROUND_MAX_PASSES = 4;

/** One room of a map rooms were just merged into, as seams are read from it. */
export interface SeamRoom {
  readonly roomNumber: number;
  /** The rooms of the same map its exits lead to. */
  readonly exitsTo: readonly number[];
}

/**
 * The seams a merge left: the rooms at either end of an exit between rooms
 * that came from different maps. That is an exit between a room the merge
 * moved and one that was already in the destination, or between rooms moved
 * from two different sources, which meet for the first time as well.
 * `arrivedFrom` names the source of each moved room by its number in the
 * destination; exits to rooms that are not in `rooms` are ignored. The seams
 * come back in ascending order.
 */
export function seamSeeds(
  rooms: readonly SeamRoom[],
  arrivedFrom: ReadonlyMap<number, string>,
): number[] {
  const present = new Set(rooms.map((room) => room.roomNumber));
  const seeds = new Set<number>();
  for (const room of rooms) {
    const origin = arrivedFrom.get(room.roomNumber);
    for (const to of room.exitsTo) {
      if (to === room.roomNumber || !present.has(to)) continue;
      if (arrivedFrom.get(to) === origin) continue;
      seeds.add(room.roomNumber);
      seeds.add(to);
    }
  }
  return [...seeds].sort((a, b) => a - b);
}

/** The fields of a layout quality up to and including crossings. */
const THROUGH_CROSSINGS = [
  "cardinalRayViolations",
  "reciprocalRayViolations",
  "levelViolations",
  "routingViolations",
  "exitPortViolations",
  "reciprocalExitPortViolations",
  "roomObstructions",
  "linkCrossings",
] as const;

export type SeamRoundQuality = {
  readonly [Field in (typeof THROUGH_CROSSINGS)[number]]?: number;
};

/** A complete layout quality, as map-layout's `compareLayoutQuality` takes one. */
export type SeamRoundOrderedQuality = {
  readonly [Field in (typeof THROUGH_CROSSINGS)[number]]: number;
} & {
  readonly footprintArea: number;
  readonly footprintPerimeter: number;
  readonly cardinalSlack: number;
};

/** The quality with footprint and slack, the fields after crossings, at zero. */
function throughCrossings(quality: SeamRoundQuality): SeamRoundOrderedQuality {
  return {
    cardinalRayViolations: quality.cardinalRayViolations ?? 0,
    reciprocalRayViolations: quality.reciprocalRayViolations ?? 0,
    levelViolations: quality.levelViolations ?? 0,
    routingViolations: quality.routingViolations ?? 0,
    exitPortViolations: quality.exitPortViolations ?? 0,
    reciprocalExitPortViolations: quality.reciprocalExitPortViolations ?? 0,
    roomObstructions: quality.roomObstructions ?? 0,
    linkCrossings: quality.linkCrossings ?? 0,
    footprintArea: 0,
    footprintPerimeter: 0,
    cardinalSlack: 0,
  };
}

/**
 * Whether `after` ranks strictly above `before` in `compare`, map-layout's
 * `compareLayoutQuality`, on the fields up to and including crossings: a
 * pass that gains only footprint or slack does not earn the seam round
 * another pass.
 */
export function improvesThroughCrossings(
  after: SeamRoundQuality,
  before: SeamRoundQuality,
  compare: (a: SeamRoundOrderedQuality, b: SeamRoundOrderedQuality) => number,
): boolean {
  return compare(throughCrossings(after), throughCrossings(before)) > 0;
}

/** A room's cell on the map grid. */
export interface SeamCell {
  readonly x: number;
  readonly y: number;
  readonly level: number;
}

/** A room of a seam-round pass: where it is, and whether the round may move it. */
export interface SeamRoundResident {
  readonly id: string;
  readonly position: SeamCell;
  readonly movable: boolean;
}

/**
 * Whether a layout counts in the seam round: it places every room of the pass
 * and leaves each room the round pinned in its cell. A room the round may move
 * may move anywhere, to another level too.
 */
export function keepsSeamPins(
  residents: readonly SeamRoundResident[],
  positions: ReadonlyMap<string, SeamCell>,
): boolean {
  return residents.every((resident) => {
    const position = positions.get(resident.id);
    return position !== undefined && (resident.movable || (
      position.x === resident.position.x && position.y === resident.position.y &&
      position.level === resident.position.level
    ));
  });
}

/** A map's geometry as a polish plans from it. */
export interface SeamPlanningBase<Positions> {
  /** Every room's position. */
  readonly positions: Positions;
  /** The planning fingerprint of the map in that geometry. */
  readonly fingerprint: string;
}

/** The seam round's part in one pass. */
export interface SeamRoundPlan {
  /** The rooms the round may move; it pins every other room. The same for every pass of the round. */
  readonly region: ReadonlySet<string>;
  /** How many of the round's `SEAM_ROUND_MAX_PASSES` passes are left. */
  readonly passesLeft: number;
  /** Whether an earlier pass began this round and was interrupted. */
  readonly resumed: boolean;
}

/** What one polish pass over a map with seams does. */
export interface SeamPass<Positions> {
  /** The geometry the whole-map polish plans from. */
  readonly base: SeamPlanningBase<Positions>;
  /** The seam round to run first, while it has not run to its end. */
  readonly round?: SeamRoundPlan;
}

interface SeamPreviewEntry<Positions> {
  /** The map's seams, canonically, when the round began. */
  readonly seams: string;
  readonly region: ReadonlySet<string>;
  passes: number;
  roundDone: boolean;
  /** Present while only the preview's own writes have changed the map since. */
  base: SeamPlanningBase<Positions> | undefined;
  /** The fingerprint of the map as the preview's latest write left it. */
  tip: string;
}

/**
 * The seam preview of each map, across the polish passes that movement
 * interrupts and resumes. A map gets one seam round for one set of seams.
 * While the round's own writes are the only changes to the map since it
 * began, the whole-map polish plans from the geometry the round began with:
 * a later pass finds the map exactly as the preview's latest write left it,
 * which is the only proof this accepts. Any other change — the player's edit,
 * another writer, new rooms or exits, a lock — gives the map another
 * fingerprint, and the polish plans from the map as it is. So does a polish
 * that has written a layout of its own: the map then shows that polish's
 * progress rather than the preview.
 */
export class SeamPreviews<Positions> {
  readonly #areas = new Map<string, SeamPreviewEntry<Positions>>();

  /**
   * Plans one polish pass over a map with seams, given the map as it is and
   * its planning fingerprint. `region` is asked for only when a new round
   * begins.
   */
  begin(
    areaKey: string,
    seams: string,
    live: SeamPlanningBase<Positions>,
    region: () => ReadonlySet<string>,
  ): SeamPass<Positions> {
    const entry = this.#areas.get(areaKey);
    if (entry && entry.seams === seams) {
      if (entry.base && entry.tip === live.fingerprint) {
        return entry.roundDone ? { base: entry.base } : {
          base: entry.base,
          round: {
            region: entry.region,
            passesLeft: SEAM_ROUND_MAX_PASSES - entry.passes,
            resumed: true,
          },
        };
      }
      // Something besides the preview changed the map: the round is over and
      // the polish plans from the map as it is.
      entry.base = undefined;
      entry.roundDone = true;
      return { base: live };
    }
    const fresh: SeamPreviewEntry<Positions> = {
      seams,
      region: region(),
      passes: 0,
      roundDone: false,
      base: live,
      tip: live.fingerprint,
    };
    this.#areas.set(areaKey, fresh);
    return {
      base: live,
      round: { region: fresh.region, passesLeft: SEAM_ROUND_MAX_PASSES, resumed: false },
    };
  }

  /** One pass of the round finished. */
  passRan(areaKey: string): void {
    const entry = this.#areas.get(areaKey);
    if (!entry) return;
    entry.passes += 1;
    if (entry.passes >= SEAM_ROUND_MAX_PASSES) entry.roundDone = true;
  }

  /** The round stopped: it ran out of passes or gain, or could not run. */
  roundEnded(areaKey: string): void {
    const entry = this.#areas.get(areaKey);
    if (entry) entry.roundDone = true;
  }

  /** The preview wrote a layout, which left the map with `fingerprint`. */
  previewApplied(areaKey: string, fingerprint: string): void {
    const entry = this.#areas.get(areaKey);
    if (entry?.base) entry.tip = fingerprint;
  }

  /** The whole-map polish wrote a layout of its own: later passes plan from the map. */
  polishApplied(areaKey: string): void {
    const entry = this.#areas.get(areaKey);
    if (!entry) return;
    entry.base = undefined;
    entry.roundDone = true;
  }

  /** The whole-map polish completed, or the map is gone. */
  forget(areaKey: string): void {
    this.#areas.delete(areaKey);
  }

  clear(): void {
    this.#areas.clear();
  }
}
