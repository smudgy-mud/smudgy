import assert from "node:assert/strict";
import test from "node:test";
import type { LayoutPlannerSnapshot } from "smudgy://kapusniak/map-layout";
import { elapsedClock, polishProgressLine, qualitySummary, zoneNameFromMaps } from "./tidy.ts";
import type { MapFacts } from "./zone-plan.ts";

function map(
  id: string,
  name: string,
  { zone = 0, other = 0, key, managed = true, madeFor }: {
    zone?: number;
    other?: number;
    key?: string;
    managed?: boolean;
    madeFor?: number;
  } = {},
): MapFacts {
  return {
    id: id as AreaId,
    name,
    key,
    nameKey: undefined,
    managed,
    placeholder: /^NukeFire Zone \d+$/.test(name),
    madeFor,
    roomCount: zone + other,
    zoneRooms: Array.from({ length: zone }, (_, index) => index + 1),
  };
}

test("a zone takes the name of the map made for it", () => {
  const maps = [
    map("a", "Eastern Forest", { zone: 30, other: 120, key: "eastern forest" }),
    map("b", "More Thick Forest", { zone: 7, madeFor: 47 }),
  ];
  assert.equal(zoneNameFromMaps(47, maps), "More Thick Forest");
});

test("without one, a map holding only the zone names it, then the map for an area name holding most of it", () => {
  assert.equal(
    zoneNameFromMaps(47, [
      map("a", "Eastern Forest", { zone: 30, other: 120, key: "eastern forest" }),
      map("b", "Thick Woods", { zone: 7, key: "thick woods" }),
    ]),
    "Thick Woods",
  );
  assert.equal(
    zoneNameFromMaps(47, [
      map("a", "Eastern Forest", { zone: 30, other: 120, key: "eastern forest" }),
      map("b", "Rusty Gulch", { zone: 3, other: 40, key: "rusty gulch" }),
    ]),
    "Eastern Forest",
  );
});

test("placeholders, maps the player made and maps without the zone name nothing", () => {
  assert.equal(
    zoneNameFromMaps(47, [
      map("a", "NukeFire Zone 47", { zone: 9, madeFor: 47 }),
      map("b", "My Forest", { zone: 4, managed: false }),
      map("c", "Rusty Gulch", { other: 40, key: "rusty gulch" }),
    ]),
    undefined,
  );
});

test("a polish reports its time, phase, best layout so far and work", () => {
  const snapshot = {
    sequence: 1,
    status: "repairing",
    operation: "constraint-repair",
    phase: "constraint search",
    elapsedMs: 12_000,
    nodes: 0,
    residents: 537,
    edges: 1_400,
    work: {
      layoutsConsidered: 14,
      compactionAttempts: 0,
      restarts: 0,
      feasibilityChecks: 52_310,
      rawIncumbents: 0,
      softIncumbents: 0,
      distinctLayouts: 0,
      maskDiversifications: 0,
      separatorStates: 0,
      separatorBranches: 0,
      separatorCyclePrunes: 0,
      crossingsConsidered: 0,
      macrosConsidered: 0,
      pushClosures: 0,
      maxDepth: 0,
      visitedStates: 0,
    },
    currentQuality: { cardinalRayViolations: 95, routingViolations: 376, linkCrossings: 233 },
    bestQuality: { cardinalRayViolations: 41, routingViolations: 289, linkCrossings: 71 },
  } as unknown as LayoutPlannerSnapshot;
  assert.equal(
    polishProgressLine("Eastern Forest", 72_500, snapshot, 2),
    "Eastern Forest 1:12 | constraint search | best 41/289/71 from 95/376/233 | " +
      "14 layouts, 52310 checks | 2 on the map",
  );
  assert.equal(
    polishProgressLine("Eastern Forest", 400, undefined, 0),
    "Eastern Forest 0:00 | waiting for the planner",
  );
  assert.equal(qualitySummary(undefined), "?");
  assert.equal(
    qualitySummary({ ...snapshot.bestQuality!, levelViolations: 3 }),
    "41/289/71 (3 on the wrong level)",
  );
  assert.equal(qualitySummary({ ...snapshot.bestQuality!, levelViolations: 0 }), "41/289/71");
  assert.equal(elapsedClock(3_599_999), "59:59");
});

test("a polish names its starting layouts in words and counts one of anything as one", () => {
  const starting = {
    sequence: 1,
    status: "planning",
    operation: "integral",
    phase: "stable",
    elapsedMs: 900,
    nodes: 0,
    residents: 400,
    edges: 1_520,
    work: {
      layoutsConsidered: 1,
      compactionAttempts: 0,
      restarts: 1,
      feasibilityChecks: 0,
      rawIncumbents: 0,
      softIncumbents: 0,
      distinctLayouts: 0,
      maskDiversifications: 0,
      separatorStates: 0,
      separatorBranches: 0,
      separatorCyclePrunes: 0,
      crossingsConsidered: 1,
      macrosConsidered: 0,
      pushClosures: 0,
      maxDepth: 0,
      visitedStates: 0,
    },
    currentQuality: { cardinalRayViolations: 480, routingViolations: 7038, linkCrossings: 25746 },
  } as unknown as LayoutPlannerSnapshot;
  assert.equal(
    polishProgressLine("Shuffled Grid", 1_000, starting, 0),
    "Shuffled Grid 0:01 | starting layouts | now 480/7038/25746 | 1 layout, 1 restart, 1 crossing tried",
  );
  assert.equal(
    polishProgressLine("Shuffled Grid", 1_000, { ...starting, phase: "greedy-cardinal-repair" }, 0),
    "Shuffled Grid 0:01 | greedy cardinal repair | now 480/7038/25746 | 1 layout, 1 restart, 1 crossing tried",
  );
});
