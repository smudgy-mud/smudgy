import assert from "node:assert/strict";
import test from "node:test";
import {
  compactIntegralLayoutPlan,
  measureIntegralLayoutQuality,
  type GridPosition,
  type IntegralLayoutRequest,
  type LayoutEdge,
} from "./layout.ts";

// Synthetic rooms exercise repeated closure growth around a diagonal, its
// later translation and conversion to an axis-aligned segment, missing link
// endpoints, reciprocal physical links, and a separate level.
const cells: readonly [string, number, number, number][] = [
  ["anchor", 0, 0, 0], ["a", 3, 0, 0], ["b", 3, 2, 0],
  ["c", 2, -1, 0], ["d", 4, 3, 0], ["e", 6, 0, 0], ["f", 6, 2, 0],
  ["upper-c", 2, -1, 1], ["upper-d", 4, 3, 1],
];
const edges: LayoutEdge[] = [
  { from: "a", to: "b", direction: "South" },
  { from: "b", to: "a", direction: "North" },
  { from: "c", to: "d", direction: "Other" },
  { from: "d", to: "c", direction: "Other" },
  { from: "e", to: "f", direction: "South" },
  { from: "f", to: "e", direction: "North" },
  { from: "upper-c", to: "upper-d", direction: "Other" },
  { from: "missing", to: "c", direction: "Other" },
];

const goldenQuality = {
  cardinalRayViolations: 0, reciprocalRayViolations: 0, levelViolations: 0,
  routingViolations: 0, exitPortViolations: 0, reciprocalExitPortViolations: 0,
  roomObstructions: 0, linkCrossings: 0, levelSlack: 0, cardinalSlack: 0,
  footprintArea: 11, footprintPerimeter: 18,
};

for (const locked of [false, true]) {
  test(`diagonal push preparation preserves exact packing with locked=${locked}`, () => {
    const residents = cells.map(([id, x, y, level]) => ({
      id, position: { x, y, level }, movable: id !== "anchor" && !(locked && id === "c"),
    }));
    const request: IntegralLayoutRequest = {
      nodes: [], residents, edges, centerId: "anchor", allowExistingMoves: true,
    };
    const positions = new Map(residents.map((room) => [room.id, room.position]));
    const original = new Map(positions);
    const seed = {
      positions, movedExisting: new Set<string>(),
      quality: measureIntegralLayoutQuality(positions, edges),
    };
    const result = compactIntegralLayoutPlan(request, seed);
    // Pinned from the unmodified engine using this generic fixture.
    const expectedCoordinates = locked
      ? [[0, 0, 0], [2, 0, 0], [2, 1, 0], [2, -1, 0], [1, -1, 0],
        [1, 0, 0], [1, 1, 0], [4, 2, 1], [4, 3, 1]]
      : [[0, 0, 0], [1, 1, 0], [1, 2, 0], [2, 1, 0], [2, 2, 0],
        [0, 1, 0], [0, 2, 0], [1, 2, 1], [1, 3, 1]];
    const expected = new Map<string, GridPosition>(cells.map(([id], index) => {
      const [x, y, level] = expectedCoordinates[index];
      return [id, { x, y, level }];
    }));
    assert.deepEqual(result.positions, expected);
    assert.deepEqual(result.quality, goldenQuality);
    assert.deepEqual([...result.movedExisting], locked
      ? ["e", "f", "upper-c", "b", "a", "d"]
      : ["e", "f", "a", "b", "d", "upper-c", "upper-d", "c"]);
    assert.deepEqual(positions, original, "packing must preserve its immutable source snapshot");
    assert.deepEqual(result.quality, measureIntegralLayoutQuality(result.positions, edges));
    assert.equal(compactIntegralLayoutPlan(request, result), result, "the full closure is idempotent");
  });
}
