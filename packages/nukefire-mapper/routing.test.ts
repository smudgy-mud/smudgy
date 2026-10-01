import assert from "node:assert/strict";
import test from "node:test";
import type { GridPosition } from "./layout.ts";
import {
  amendedConnectionRoute,
  amendmentWaypointsBetween,
  directRoomObstructions,
  indexRouteAmendments,
  planConnectionRoute,
  routeAroundRooms,
  routeCrossesCells,
  routeEndSide,
  routeStartSide,
  routeTurnPoints,
} from "./routing.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });

test("detects rooms crossed by a long straight cardinal link", () => {
  const blocked = directRoomObstructions(at(0, 0), at(5, 0), [at(1, 0), at(3, 0), at(2, 1)]);
  assert.deepEqual(blocked, [at(1, 0), at(3, 0)]);
});

test("a room stands on a connection across a segment or on a turn, not beside it", () => {
  // A straight link from (0,0) to (4,0).
  assert.equal(routeCrossesCells(at(0, 0), at(4, 0), [], [at(2, 0)]), true, "across the segment");
  assert.equal(routeCrossesCells(at(0, 0), at(4, 0), [], [at(2, 1)]), false, "beside it");
  assert.equal(routeCrossesCells(at(0, 0), at(4, 0), [], [at(2, 0, 1)]), false, "on another level");
  assert.equal(routeCrossesCells(at(0, 0), at(4, 0), [], []), false, "no room placed");
  // A detour from (0,0) up to (0,-2), across to (4,-2) and down to (4,0).
  const detour = [{ x: 0, y: -2 }, { x: 4, y: -2 }];
  assert.equal(routeCrossesCells(at(0, 0), at(4, 0), detour, [at(4, -2)]), true, "on a turn");
  assert.equal(routeCrossesCells(at(0, 0), at(4, 0), detour, [at(2, -2)]), true, "across a leg");
  assert.equal(routeCrossesCells(at(0, 0), at(4, 0), detour, [at(2, 0)]), false, "where the detour no longer runs");
});

test("rejects a detour when the preferred endpoint ports are occupied", () => {
  const rooms = [at(1, 0), at(2, 0), at(3, 0), at(4, 0)];
  const path = routeAroundRooms(at(0, 0), at(5, 0), rooms, "East", "West");
  assert.equal(path, undefined);
  const route = planConnectionRoute(at(0, 0), at(5, 0), rooms, "East", "West");
  assert.equal(route.routing, "Automatic");
  assert.equal(route.startSide, "East");
  assert.equal(route.endSide, "West");
});

test("routes around a middle obstruction while preserving both exit walls", () => {
  const rooms = [at(0, -2)];
  const path = routeAroundRooms(at(0, 0), at(0, -4), rooms, "North", "South");
  assert.ok(path);
  assert.equal(routeStartSide(path), "North");
  assert.equal(routeEndSide(path), "South");
  assert.ok(routeTurnPoints(path).length >= 2);
});

test("keeps an unobstructed cardinal route straight", () => {
  const path = routeAroundRooms(at(0, 0), at(4, 0), [], "East", "West");
  assert.ok(path);
  assert.equal(routeTurnPoints(path).length, 0);
  assert.equal(routeStartSide(path), "East");
  assert.equal(routeEndSide(path), "West");
});

test("does not treat rooms on another level as obstacles", () => {
  assert.deepEqual(directRoomObstructions(at(0, 0), at(3, 0), [at(1, 0, 1)]), []);
});

test("stores a generated Manhattan path as diagonal-tolerant with rounded corners", () => {
  const rooms = [at(0, -2)];
  const route = planConnectionRoute(at(0, 0), at(0, -4), rooms, "North", "South");

  assert.equal(route.routing, "Manual");
  assert.equal(route.startSide, "North");
  assert.equal(route.endSide, "South");
  assert.equal(route.segmentShape, "Direct");
  assert.equal(route.corner, "Rounded");
  assert.ok(route.routePoints.length >= 2);
  for (let index = 1; index < route.routePoints.length; index += 1) {
    const previous = route.routePoints[index - 1];
    const current = route.routePoints[index];
    assert.ok(previous.x === current.x || previous.y === current.y);
  }
});

test("uses a rounded direct fallback when no detour is needed", () => {
  assert.deepEqual(planConnectionRoute(at(0, 0), at(4, 0), [], "East", "West"), {
    startSide: "East",
    endSide: "West",
    routing: "Automatic",
    segmentShape: "Direct",
    corner: "Rounded",
    routePoints: [],
  });
});

// ---------------------------------------------------------------------------
// Engine route amendments
// ---------------------------------------------------------------------------

test("indexes engine amendments by resolved room pair and drops the unresolvable", () => {
  const ids = new Map([
    ["room:11", 11],
    ["vnum:5001", 11],
    ["room:12", 12],
    ["room:13", 13],
  ]);
  const index = indexRouteAmendments([
    { from: "room:11", to: "room:12", waypoints: [{ x: 1, y: -1 }] },
    // Duplicate pair through the vnum alias of room 11: first resolution wins.
    { from: "room:12", to: "vnum:5001", waypoints: [{ x: 9, y: 9 }] },
    { from: "room:13", to: "room:99", waypoints: [{ x: 2, y: 2 }] },
    { from: "room:13", to: "room:11", waypoints: [] },
  ], ids);
  assert.ok(index);
  assert.equal(index.size, 1);
  assert.deepEqual(amendmentWaypointsBetween(index, 11, 12), [{ x: 1, y: -1 }]);
  assert.equal(amendmentWaypointsBetween(index, 12, 13), undefined);
  assert.equal(indexRouteAmendments([], ids), undefined);
  assert.equal(indexRouteAmendments(undefined, ids), undefined);
  assert.equal(
    indexRouteAmendments([{ from: "room:99", to: "room:98", waypoints: [{ x: 0, y: 0 }] }], ids),
    undefined,
  );
});

test("amendment waypoints orient from the requested endpoint and are copies", () => {
  const index = indexRouteAmendments(
    [{ from: "room:1", to: "room:2", waypoints: [{ x: 2, y: -3 }, { x: -2, y: -3 }] }],
    new Map([["room:1", 1], ["room:2", 2]]),
  );
  assert.ok(index);
  const forward = amendmentWaypointsBetween(index, 1, 2);
  const reverse = amendmentWaypointsBetween(index, 2, 1);
  assert.deepEqual(forward, [{ x: 2, y: -3 }, { x: -2, y: -3 }]);
  assert.deepEqual(reverse, [{ x: -2, y: -3 }, { x: 2, y: -3 }]);
  assert.ok(forward);
  forward[0].x = 99;
  assert.deepEqual(amendmentWaypointsBetween(index, 1, 2), [{ x: 2, y: -3 }, { x: -2, y: -3 }]);
});

test("an amended route persists the waypoints as an Automatic generated route", () => {
  const route = amendedConnectionRoute(
    at(2, 0),
    at(-2, 0),
    [{ x: 2, y: -3 }, { x: -2, y: -3 }],
    "West",
    "East",
  );
  assert.deepEqual(route, {
    // The drawn first segment leaves north and the last enters from below.
    startSide: "North",
    endSide: "North",
    routing: "Automatic",
    segmentShape: "Direct",
    corner: "Rounded",
    routePoints: [{ x: 2, y: -3 }, { x: -2, y: -3 }],
  });
});

test("a diagonal amendment segment keeps the semantic wall preference", () => {
  const route = amendedConnectionRoute(
    at(0, 0),
    at(5, 3),
    [{ x: 2, y: 1 }],
    "East",
    "West",
  );
  assert.equal(route.startSide, "East");
  assert.equal(route.endSide, "West");
  assert.equal(route.routing, "Automatic");
  assert.deepEqual(route.routePoints, [{ x: 2, y: 1 }]);
});
