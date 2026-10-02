import assert from "node:assert/strict";
import test from "node:test";
import { authoredEndpointTip, maintainAuthoredRoutePoints, type AuthoredRouteMove } from "./authored-route-maintenance.ts";

const east = { side: "East", port_offset: 0.5 } as const;
const west = { side: "West", port_offset: 0.5 } as const;
const beforeA = { x: 0, y: 0 };
const beforeB = { x: 6, y: 0 };
const baseline: AuthoredRouteMove = {
  points: [{ x: 1, y: 0 }, { x: 1, y: 2 }, { x: 5, y: 2 }, { x: 5, y: 0 }],
  endpointA: east, endpointB: west, beforeA, beforeB, afterA: beforeA, afterB: beforeB,
  segmentShape: "Orthogonal", authored: true,
};

function assertOrthogonal(move: AuthoredRouteMove, points: readonly { x: number; y: number }[]): void {
  const path = [authoredEndpointTip(move.afterA, move.endpointA), ...points,
    authoredEndpointTip(move.afterB, move.endpointB)];
  for (let i = 1; i < path.length; i += 1) {
    assert.ok(Math.abs(Math.fround(path[i].x - path[i - 1].x)) <= 2 ** -23 ||
      Math.abs(Math.fround(path[i].y - path[i - 1].y)) <= 2 ** -23,
    `diagonal segment ${JSON.stringify([path[i - 1], path[i]])}`);
  }
}

test("a complete shared translation preserves active and dormant stored interiors", () => {
  for (const authored of [true, false]) for (const segmentShape of ["Direct", "Orthogonal"] as const) {
    const move = { ...baseline, authored, segmentShape, afterA: { x: 3, y: -2 }, afterB: { x: 9, y: -2 } };
    assert.deepEqual(maintainAuthoredRoutePoints(move),
      baseline.points.map(({ x, y }) => ({ x: x + 3, y: y - 2 })));
  }
  assert.deepEqual(maintainAuthoredRoutePoints(baseline), baseline.points);
  assert.notEqual(maintainAuthoredRoutePoints(baseline)[0], baseline.points[0]);
});

test("unequal endpoint moves maintain authored Orthogonal legs and preserve fixed interior bends", () => {
  const move = { ...baseline, afterA: { x: 0, y: 1 }, afterB: { x: 7, y: -1 } };
  const original = structuredClone(move);
  const points = maintainAuthoredRoutePoints(move);
  assert.deepEqual(points, [{ x: 1, y: 1 }, { x: 1, y: 2 }, { x: 5, y: 2 }, { x: 5, y: -1 }]);
  assertOrthogonal(move, points);
  assert.deepEqual(move, original);
  for (const changes of [{ authored: false }, { segmentShape: "Direct" as const }]) {
    assert.deepEqual(maintainAuthoredRoutePoints({ ...move, ...changes }), baseline.points);
  }
});

test("vertical endpoint legs use final stub axes even when only one endpoint moves", () => {
  const tipA = authoredEndpointTip(beforeA, east);
  const tipB = authoredEndpointTip(beforeB, west);
  const move = { ...baseline, points: [{ x: tipA.x, y: 2 }, { x: tipB.x, y: 2 }], afterA: { x: 1, y: 1 } };
  const points = maintainAuthoredRoutePoints(move);
  assert.deepEqual(points, [{ x: authoredEndpointTip(move.afterA, east).x, y: 2 }, { x: tipB.x, y: 2 }]);
  assertOrthogonal(move, points);
});

test("empty and single-point authored paths receive only the necessary endpoint elbows", () => {
  const empty = { ...baseline, points: [], afterB: { x: 6, y: 2 } };
  const end = authoredEndpointTip(empty.afterB, west);
  assert.deepEqual(maintainAuthoredRoutePoints(empty), [{ x: end.x, y: 0 }]);
  assertOrthogonal(empty, maintainAuthoredRoutePoints(empty));
  const oldTipA = authoredEndpointTip(beforeA, east);
  const one = { ...baseline, points: [oldTipA], afterA: { x: 0, y: 1 }, afterB: { x: 6, y: 2 } };
  // The host updates the first then the last point; one shared point is touched twice.
  const points = maintainAuthoredRoutePoints(one);
  assert.deepEqual(points, [{ x: authoredEndpointTip(one.afterB, west).x, y: 1 }]);
  assertOrthogonal(one, points);
});

test("rounded corner port tips follow public f32 geometry on all four walls", () => {
  const north = authoredEndpointTip({ x: 0, y: 0 }, { side: "North", port_offset: 0 });
  const south = authoredEndpointTip({ x: 0, y: 0 }, { side: "South", port_offset: 1 });
  const eastTip = authoredEndpointTip({ x: 0, y: 0 }, { side: "East", port_offset: 0 });
  const westTip = authoredEndpointTip({ x: 0, y: 0 }, { side: "West", port_offset: 1 });
  assert.deepEqual(south, { x: -north.x, y: -north.y });
  assert.deepEqual(eastTip, { x: -north.y, y: north.x });
  assert.deepEqual(westTip, { x: north.y, y: -north.x });
  assert.ok(Math.abs(north.x + 0.22071068) < 1e-7);
  assert.ok(Math.abs(north.y + 0.37071068) < 1e-7);
  for (const side of ["North", "East", "South", "West"] as const) {
    const move = { ...baseline, endpointA: { side, port_offset: 0 }, endpointB: { side, port_offset: 1 },
      points: [], afterA: { x: 1, y: -1 }, afterB: { x: 8, y: 2 } };
    assertOrthogonal(move, maintainAuthoredRoutePoints(move));
  }
});
