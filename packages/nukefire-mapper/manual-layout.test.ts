import assert from "node:assert/strict";
import test from "node:test";
import { MAX_MANUAL_ROUTE_AMENDMENT_WAYPOINTS, validManualLayoutRequest, validManualRouteAmendments, type ManualLayoutRequest, type ManualRouteAmendment } from "./manual-layout.ts";
import { MAX_ROUTE_AMENDMENT_WAYPOINTS } from "../map-layout/worker-protocol.ts";

const bounded: ManualLayoutRequest = {
  requestId: "1:request", areaUuid: "map", sourceSnapshotKey: "source", plannedSnapshotKey: "planned",
  moves: [{ roomNumber: 1, x: 0, y: -1, level: 2 }],
};

test("the manual bridge accepts bounded and clone-safe perfect requests", () => {
  assert.equal(validManualLayoutRequest(bounded), true);
  assert.equal(validManualLayoutRequest({ ...bounded, policy: {
    when: "always", maxDurationMs: "infinity", maxRestarts: 1, maxLayouts: 1, maxPolishTournaments: 1,
    maxPolishPasses: 1, maxExtensionStates: 1, maxLiveSearchNodes: 1, maxMaskDiversifications: 1, maxCrossingWork: 1,
  }, report: { outcome: "no-constraints", geometricFixedPoint: false } }), true);
});

test("malformed, ambiguous or nonintegral move messages are refused before reaching the mapper", () => {
  for (const invalid of [null, {}, { ...bounded, sourceSnapshotKey: "" }, { ...bounded, moves: [null] },
    { ...bounded, moves: [undefined] }, { ...bounded, moves: [...bounded.moves, ...bounded.moves] },
    { ...bounded, moves: Array(1) },
    { ...bounded, moves: [{ roomNumber: 1, x: Number.NaN, y: 0, level: 0 }] },
    { ...bounded, moves: [{ roomNumber: 1, x: 0, y: 0, level: 0.5 }] },
    { ...bounded, report: null }, { ...bounded, policy: { maxDurationMs: Infinity } },
  ]) assert.equal(validManualLayoutRequest(invalid as unknown as ManualLayoutRequest), false);
});

test("manual route amendments are bounded, unambiguous clone-safe grid detours", () => {
  assert.equal(MAX_MANUAL_ROUTE_AMENDMENT_WAYPOINTS, MAX_ROUTE_AMENDMENT_WAYPOINTS);
  const amendment: ManualRouteAmendment = {
    fromRoomNumber: 1, toRoomNumber: 2, waypoints: [{ x: 0, y: -3, level: 0 }],
  };
  assert.equal(validManualRouteAmendments(undefined), true);
  assert.equal(validManualRouteAmendments([]), true);
  assert.equal(validManualLayoutRequest({ ...bounded, routeAmendments: structuredClone([amendment]) }), true);
  for (const invalid of [
    null, {}, [null], [undefined], Array(1),
    [{ ...amendment, fromRoomNumber: 1.5 }],
    [{ ...amendment, toRoomNumber: 2_147_483_648 }],
    [{ ...amendment, toRoomNumber: 1 }],
    [{ ...amendment, waypoints: [] }],
    [{ ...amendment, waypoints: Array(1) }],
    [{ ...amendment, waypoints: Array.from({ length: 33 }, () => ({ x: 0, y: 0, level: 0 })) }],
    [{ ...amendment, waypoints: [{ x: Number.NaN, y: 0, level: 0 }] }],
    [{ ...amendment, waypoints: [{ x: 0, y: 0, level: 0.5 }] }],
    [amendment, { ...amendment, fromRoomNumber: 2, toRoomNumber: 1 }],
  ]) assert.equal(validManualRouteAmendments(invalid as unknown as ManualRouteAmendment[]), false);
});
