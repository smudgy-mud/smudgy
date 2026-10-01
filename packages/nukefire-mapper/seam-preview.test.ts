import assert from "node:assert/strict";
import test from "node:test";
import { compareLayoutQuality } from "../map-layout/layout.ts";
import {
  improvesThroughCrossings,
  keepsSeamPins,
  SEAM_ROUND_MAX_PASSES,
  SeamPreviews,
  seamSeeds,
  type SeamPlanningBase,
  type SeamRoom,
} from "./seam-preview.ts";

const room = (roomNumber: number, ...exitsTo: number[]): SeamRoom => ({ roomNumber, exitsTo });

test("a merge's seams are the rooms at either end of an exit from a moved room to one already there", () => {
  // Rooms 1-3 were in the map; 4 and 5 arrived from one source.
  const rooms = [room(1, 2), room(2, 1, 3), room(3, 2, 4), room(4, 3, 5), room(5, 4)];
  const arrived = new Map([[4, "source-a"], [5, "source-a"]]);
  assert.deepEqual(seamSeeds(rooms, arrived), [3, 4]);
  // One exit is enough: a one-way exit is a seam as well.
  const oneWay = [room(1, 2), room(2, 1), room(7, 1)];
  assert.deepEqual(seamSeeds(oneWay, new Map([[7, "source-a"]])), [1, 7]);
});

test("rooms moved from two sources meet at a seam too", () => {
  // A new map: every room arrived, from two sections.
  const rooms = [room(1, 2), room(2, 1, 3), room(3, 2)];
  const arrived = new Map([[1, "source-a"], [2, "source-a"], [3, "source-b"]]);
  assert.deepEqual(seamSeeds(rooms, arrived), [2, 3]);
});

test("exits to rooms outside the map, exits to the same room, and merges within one origin leave no seam", () => {
  const rooms = [room(1, 1, 99), room(2, 3), room(3, 2)];
  assert.deepEqual(seamSeeds(rooms, new Map()), []);
  assert.deepEqual(seamSeeds(rooms, new Map([[2, "source-a"], [3, "source-a"]])), []);
  assert.deepEqual(seamSeeds([], new Map([[2, "source-a"]])), []);
});

test("a pass earns another only by gaining in the quality order up to crossings", () => {
  const quality = (
    cardinalRayViolations: number,
    routingViolations: number,
    linkCrossings: number,
    footprintArea = 10,
  ) => ({
    cardinalRayViolations,
    reciprocalRayViolations: 0,
    routingViolations,
    exitPortViolations: 0,
    reciprocalExitPortViolations: 0,
    roomObstructions: 0,
    linkCrossings,
    footprintArea,
  });
  const improves = (after: ReturnType<typeof quality>, before: ReturnType<typeof quality>) =>
    improvesThroughCrossings(after, before, compareLayoutQuality);
  // A directional violation is worth eight routing violations and crossings.
  assert.equal(improves(quality(4, 5, 2), quality(5, 0, 0)), true);
  assert.equal(improves(quality(4, 9, 9), quality(5, 0, 0)), false);
  assert.equal(improves(quality(6, 0, 0), quality(5, 4, 5)), true);
  assert.equal(improves(quality(5, 2, 9), quality(5, 3, 0)), false);
  assert.equal(improves(quality(5, 4, 0), quality(5, 3, 9)), true);
  assert.equal(improves(quality(5, 3, 1), quality(5, 3, 2)), true);
  // Equal scores rank fewer directional violations first.
  assert.equal(improves(quality(4, 8, 0), quality(5, 0, 0)), true);
  assert.equal(improves(quality(5, 0, 0), quality(4, 8, 0)), false);
  // Footprint and slack never earn a pass.
  assert.equal(improves(quality(5, 3, 2, 1), quality(5, 3, 2, 99)), false);
  assert.equal(improves(quality(5, 3, 2), quality(5, 3, 2)), false);
  // A quality without its reciprocal fields reads them as zero.
  assert.equal(
    improvesThroughCrossings(
      { cardinalRayViolations: 1, routingViolations: 0, exitPortViolations: 0, roomObstructions: 0, linkCrossings: 0 },
      { ...quality(1, 0, 0), reciprocalRayViolations: 1 },
      compareLayoutQuality,
    ),
    true,
  );
});

test("a round's layout keeps its pinned rooms in their cells and may move the others between levels", () => {
  const cell = (x: number, y: number, level: number) => ({ x, y, level });
  const residents = [
    { id: "pinned", position: cell(0, 0, 0), movable: false },
    { id: "seam", position: cell(1, 0, 1), movable: true },
  ];
  const layout = (pinned: ReturnType<typeof cell>, seam: ReturnType<typeof cell>) =>
    new Map([["pinned", pinned], ["seam", seam]]);
  assert.equal(keepsSeamPins(residents, layout(cell(0, 0, 0), cell(1, 0, 0))), true, "a seam room changes level");
  assert.equal(keepsSeamPins(residents, layout(cell(0, 0, 0), cell(4, -2, 3))), true, "and moves anywhere");
  assert.equal(keepsSeamPins(residents, layout(cell(1, 0, 0), cell(1, 0, 1))), false, "a pinned room keeps its cell");
  assert.equal(keepsSeamPins(residents, layout(cell(0, 0, 1), cell(1, 0, 1))), false, "and its level");
  assert.equal(keepsSeamPins(residents, new Map([["pinned", cell(0, 0, 0)]])), false, "every room is placed");
});

const geometry = (fingerprint: string): SeamPlanningBase<string> => ({
  positions: `positions of ${fingerprint}`,
  fingerprint,
});

function previews() {
  const seams = new SeamPreviews<string>();
  let regions = 0;
  const region = () => {
    regions += 1;
    return new Set(["room:1", "room:2"]);
  };
  return { seams, region, regionsComputed: () => regions };
}

test("a new round plans from the map as it is, with every pass ahead of it", () => {
  const { seams, region, regionsComputed } = previews();
  const pass = seams.begin("map", "[1,2]", geometry("merged"), region);
  assert.deepEqual(pass.base, geometry("merged"));
  assert.deepEqual(pass.round, {
    region: new Set(["room:1", "room:2"]),
    passesLeft: SEAM_ROUND_MAX_PASSES,
    resumed: false,
  });
  assert.equal(regionsComputed(), 1);
  assert.equal(SEAM_ROUND_MAX_PASSES, 4);
});

test("while the preview's writes are the only changes, the polish keeps the round's base", () => {
  const { seams, region, regionsComputed } = previews();
  seams.begin("map", "[1,2]", geometry("merged"), region);
  seams.passRan("map");
  seams.previewApplied("map", "preview-1");
  // An interrupted round resumes from the map as the preview left it.
  const resumed = seams.begin("map", "[1,2]", geometry("preview-1"), region);
  assert.deepEqual(resumed.base, geometry("merged"));
  assert.equal(resumed.round?.passesLeft, SEAM_ROUND_MAX_PASSES - 1);
  assert.equal(resumed.round?.resumed, true);
  seams.previewApplied("map", "preview-2");
  seams.roundEnded("map");
  // The round is over; later passes still plan the whole map from the merge.
  const later = seams.begin("map", "[1,2]", geometry("preview-2"), region);
  assert.deepEqual(later, { base: geometry("merged") });
  assert.equal(regionsComputed(), 1, "a round keeps the region it began with");
});

test("any other change ends the round, and the polish plans from the map as it is from then on", () => {
  const { seams, region } = previews();
  seams.begin("map", "[1,2]", geometry("merged"), region);
  seams.previewApplied("map", "preview-1");
  // The player moved a room, another writer wrote, or mapping added a room.
  assert.deepEqual(seams.begin("map", "[1,2]", geometry("edited"), region), {
    base: geometry("edited"),
  });
  // Nothing brings the base back, not even the preview's geometry again.
  assert.deepEqual(seams.begin("map", "[1,2]", geometry("preview-1"), region), {
    base: geometry("preview-1"),
  });
  // A write by the preview after that no longer moves anything.
  seams.previewApplied("map", "preview-2");
  assert.deepEqual(seams.begin("map", "[1,2]", geometry("preview-2"), region), {
    base: geometry("preview-2"),
  });
});

test("before the preview writes anything, the base is the map itself", () => {
  const { seams, region } = previews();
  seams.begin("map", "[1,2]", geometry("merged"), region);
  seams.roundEnded("map");
  assert.deepEqual(seams.begin("map", "[1,2]", geometry("merged"), region), {
    base: geometry("merged"),
  });
  assert.deepEqual(seams.begin("map", "[1,2]", geometry("elsewhere"), region), {
    base: geometry("elsewhere"),
  });
});

test("once the polish writes a layout of its own, later passes plan from the map", () => {
  const { seams, region } = previews();
  seams.begin("map", "[1,2]", geometry("merged"), region);
  seams.previewApplied("map", "preview-1");
  seams.roundEnded("map");
  seams.polishApplied("map");
  assert.deepEqual(seams.begin("map", "[1,2]", geometry("polished-1"), region), {
    base: geometry("polished-1"),
  });
});

test("passes count across interruptions, and the fourth ends the round", () => {
  const { seams, region } = previews();
  seams.begin("map", "[1,2]", geometry("merged"), region);
  seams.passRan("map");
  seams.passRan("map");
  assert.equal(seams.begin("map", "[1,2]", geometry("merged"), region).round?.passesLeft, 2);
  seams.passRan("map");
  seams.passRan("map");
  assert.deepEqual(seams.begin("map", "[1,2]", geometry("merged"), region), {
    base: geometry("merged"),
  });
});

test("new seams begin a new round; a forgotten or cleared map begins afresh", () => {
  const { seams, region, regionsComputed } = previews();
  seams.begin("map", "[1,2]", geometry("merged"), region);
  seams.previewApplied("map", "preview-1");
  seams.roundEnded("map");
  const next = seams.begin("map", "[1,2,7]", geometry("merged-again"), region);
  assert.deepEqual(next.base, geometry("merged-again"));
  assert.equal(next.round?.resumed, false);
  assert.equal(next.round?.passesLeft, SEAM_ROUND_MAX_PASSES);

  seams.forget("map");
  assert.equal(seams.begin("map", "[1,2,7]", geometry("merged-again"), region).round?.resumed, false);
  seams.begin("other", "[5]", geometry("other"), region);
  seams.clear();
  assert.equal(seams.begin("other", "[5]", geometry("other"), region).round?.resumed, false);
  assert.equal(regionsComputed(), 5);

  // Notices about a map with no round change nothing.
  seams.passRan("none");
  seams.roundEnded("none");
  seams.previewApplied("none", "x");
  seams.polishApplied("none");
  assert.equal(seams.begin("none", "[1]", geometry("n"), region).round?.passesLeft, SEAM_ROUND_MAX_PASSES);
});
