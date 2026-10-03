import assert from "node:assert/strict";
import test from "node:test";
import {
  compactIntegralLayoutPlan,
  measureIntegralLayoutQuality,
  type IntegralLayoutRequest,
  type LayoutEdge,
} from "./layout.ts";

function fixture(locked: boolean) {
  const residents = [
    ["anchor", 0, 0, 0], ["a", 3, 0, 0], ["b", 3, 2, 0],
    ["c", 2, -1, 0], ["d", 4, 3, 0], ["e", 6, 0, 0], ["f", 6, 2, 0],
    ["upper-c", 2, -1, 1], ["upper-d", 4, 3, 1],
  ].map(([id, x, y, level]) => ({
    id: String(id),
    position: Object.freeze({ x: Number(x), y: Number(y), level: Number(level) }),
    movable: id !== "anchor" && !(locked && id === "c"),
  }));
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "South" },
    { from: "b", to: "a", direction: "North" },
    { from: "c", to: "d", direction: "Other" },
    { from: "e", to: "f", direction: "South" },
    { from: "f", to: "e", direction: "North" },
    { from: "upper-c", to: "upper-d", direction: "Other" },
  ];
  const request: IntegralLayoutRequest = {
    nodes: [], residents, edges, centerId: "anchor", allowExistingMoves: true,
  };
  const positions = new Map(residents.map((room) => [room.id, room.position]));
  const seed = {
    positions, movedExisting: new Set<string>(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  return { request, seed };
}

for (const locked of [false, true]) {
  test(`cancellation inside squeeze stops further trials with locked=${locked}`, () => {
    const { request, seed } = fixture(locked);
    const originalPositions = structuredClone(seed.positions);
    const originalQuality = { ...seed.quality };
    let requested = false;
    let cancellationPulse = false;
    let observed = 0;
    let admissionsAfterRequest = 0;
    // Progress is time-throttled. A deterministic clock exposes the first
    // spacing trial without relying on execution speed or a timer race.
    const ownNow = Object.getOwnPropertyDescriptor(performance, "now");
    let clock = 0;
    Object.defineProperty(performance, "now", {
      configurable: true, value: () => clock += 31,
    });
    try {
      request.trace = (event) => {
        if (event.type === "axis-progress" && event.phase === "spacing" && !requested) {
          assert.equal(event.complete, false, "request cancellation during active squeeze");
          requested = true;
          cancellationPulse = true;
        }
      };
      const result = compactIntegralLayoutPlan(request, seed, {
        acceptsPositions: () => {
          if (requested) admissionsAfterRequest += 1;
          return true;
        },
        shouldCancel: () => {
          if (!cancellationPulse) return false;
          cancellationPulse = false;
          observed += 1;
          return true;
        },
      });
      assert.equal(requested, true, "the fixture reaches the prior unpolled squeeze path");
      assert.equal(observed, 1, "a momentary request is observed once and remains latched");
      assert.equal(admissionsAfterRequest, 0, "stop before checking another squeeze candidate");
      assert.equal(result, seed, "no partial compaction transaction can escape");
      assert.deepEqual(seed.positions, originalPositions);
      assert.deepEqual(seed.quality, originalQuality);
      assert.deepEqual([...seed.movedExisting], []);
      assert.deepEqual(seed.positions.get("anchor"), { x: 0, y: 0, level: 0 });
      if (locked) assert.deepEqual(seed.positions.get("c"), { x: 2, y: -1, level: 0 });
    } finally {
      if (ownNow) Object.defineProperty(performance, "now", ownNow);
      else Reflect.deleteProperty(performance, "now");
    }
  });
}
