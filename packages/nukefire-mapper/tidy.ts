/**
 * The parts of tidying every NukeFire map at once (`nfmap tidy`) that read no
 * live state: which name a zone settles under when the player is not standing
 * in it to hear the game's, and how a polish reports its progress.
 */

import type { LayoutPlannerSnapshot, LayoutQuality } from "smudgy://kapusniak/map-layout";
import type { MapFacts } from "./zone-plan.ts";

/** The map holding most of the zone, then the largest, then the lowest id. */
function mostOfZone(maps: readonly MapFacts[]): MapFacts | undefined {
  return [...maps].sort((a, b) =>
    b.zoneRooms.length - a.zoneRooms.length ||
    b.roomCount - a.roomCount ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  )[0];
}

/**
 * The name `zone` settles under when no visit supplies the one the game shows,
 * read from the mapper's maps: the map made for the zone, else a map holding
 * only that zone's rooms, else the map for an area name holding most of them.
 * Each was named after what the game showed, or after the map its rules put
 * that name in. A zone that only placeholder maps hold has no name until the
 * player visits it.
 */
export function zoneNameFromMaps(zone: number, maps: readonly MapFacts[]): string | undefined {
  const named = maps.filter((map) => map.managed && !map.placeholder);
  return mostOfZone(named.filter((map) => map.madeFor === zone))?.name ??
    mostOfZone(named.filter((map) => map.zoneRooms.length > 0 && map.zoneRooms.length === map.roomCount))
      ?.name ??
    mostOfZone(named.filter((map) => map.key !== undefined && map.zoneRooms.length > 0))?.name;
}

/**
 * A layout quality as wrong exits / blocked routes / crossings, and how many
 * of the wrong exits join rooms on the wrong levels when any do.
 */
export function qualitySummary(quality: Readonly<LayoutQuality> | undefined): string {
  if (!quality) return "?";
  const summary = `${quality.cardinalRayViolations}/${quality.routingViolations}/${quality.linkCrossings}`;
  const level = quality.levelViolations ?? 0;
  return level > 0 ? `${summary} (${level} on the wrong level)` : summary;
}

/** Elapsed time as minutes and seconds. */
export function elapsedClock(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  return `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
}

/** `count` of `noun`, which takes an s for any count but one. */
export function counted(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

/** The planner's first stages, which build the layouts it starts from. */
const STARTING_STAGES = new Set(["stable", "golden", "exact-new", "chart-reflow", "all-candidates"]);

/** What the planner is doing, in words: its own, but one phrase for its starting layouts. */
function plannerStage(snapshot: Readonly<LayoutPlannerSnapshot>): string {
  const stage = snapshot.phase || snapshot.status;
  return STARTING_STAGES.has(stage) ? "starting layouts" : stage.replaceAll("-", " ");
}

/**
 * One line of a running polish: its map and time, what the planner is doing,
 * the best layout found so far against the map as it started, the work behind
 * it, and how many layouts are on the map.
 */
export function polishProgressLine(
  map: string,
  elapsedMs: number,
  snapshot: Readonly<LayoutPlannerSnapshot> | undefined,
  applied: number,
): string {
  const parts = [`${map} ${elapsedClock(elapsedMs)}`];
  if (!snapshot) {
    parts.push("waiting for the planner");
  } else {
    parts.push(plannerStage(snapshot));
    const best = snapshot.bestQuality ?? snapshot.standardQuality;
    parts.push(best
      ? `best ${qualitySummary(best)} from ${qualitySummary(snapshot.currentQuality)}`
      : `now ${qualitySummary(snapshot.currentQuality)}`);
    const { work } = snapshot;
    const counts = [
      work.layoutsConsidered > 0 ? counted(work.layoutsConsidered, "layout") : "",
      work.feasibilityChecks > 0 ? counted(work.feasibilityChecks, "check") : "",
      work.restarts > 0 ? counted(work.restarts, "restart") : "",
      work.crossingsConsidered > 0 ? `${counted(work.crossingsConsidered, "crossing")} tried` : "",
    ].filter((count) => count !== "");
    if (counts.length > 0) parts.push(counts.join(", "));
  }
  if (applied > 0) parts.push(`${applied} on the map`);
  return parts.join(" | ");
}
