import assert from "node:assert/strict";
import test from "node:test";
import {
  currentRoomMapViewApply,
  gpsRouteRaw,
  mapViewRoute,
  type RouteDirection,
  type RouteExit,
  type RouteRoom,
} from "./map-route.ts";

const AREA = "11111111-1111-4111-8111-111111111111";
const OTHER_AREA = "33333333-3333-4333-8333-333333333333";

function exit(
  from_direction: RouteDirection,
  to_room_number: number,
  command: string | null = null,
  to_area_id: readonly [number, number] = AREA,
): RouteExit {
  return {
    from_direction,
    to_area_id,
    to_room_number,
    command,
  };
}

function room(room_number: number, exits: RouteExit[] = []): RouteRoom {
  return { area_id: AREA, room_number, exits };
}

test("selects every exit at the current room for persistent labels", () => {
  const current = room(7, [
    exit("North", 8),
    exit("East", 9, null, OTHER_AREA),
  ]);
  assert.deepEqual(currentRoomMapViewApply(current), [{
    style: "current-room",
    exits: [
      { room: 7, direction: "North" },
      { room: 7, direction: "East" },
    ],
  }]);
  assert.deepEqual(currentRoomMapViewApply(undefined), []);
});

test("walks compact GPS directions through cardinal and vertical topology", () => {
  const rooms = new Map<number, RouteRoom>([
    [1, room(1, [exit("North", 2)])],
    [2, room(2, [exit("East", 3)])],
    [3, room(3, [exit("Up", 4)])],
    [4, room(4, [exit("Down", 5)])],
    [5, room(5)],
  ]);
  assert.deepEqual(
    mapViewRoute(rooms.get(1), "neud", (_area, number) => rooms.get(number)),
    [{
      style: "route",
      rooms: [1, 2, 3, 4, 5],
      exits: [
        { room: 1, direction: "North" },
        { room: 2, direction: "East" },
        { room: 3, direction: "Up" },
        { room: 4, direction: "Down" },
      ],
    }],
  );
});

test("prefers an explicit one-letter traversal command", () => {
  const rooms = new Map<number, RouteRoom>([
    [1, room(1, [exit("North", 2), exit("Special", 3, "n")])],
    [2, room(2)],
    [3, room(3)],
  ]);
  assert.deepEqual(
    mapViewRoute(rooms.get(1), "n", (_area, number) => rooms.get(number)),
    [{
      style: "route",
      rooms: [1, 3],
      exits: [{ room: 1, direction: "Special" }],
    }],
  );
});

test("includes the outbound connection when leaving the active MapView area", () => {
  const start = room(1, [exit("East", 9, null, OTHER_AREA)]);
  assert.deepEqual(
    mapViewRoute(start, "e", () => undefined),
    [{
      style: "route",
      rooms: [1],
      exits: [{ room: 1, direction: "East" }],
    }],
  );
});

test("yields no applications without a resolvable start room", () => {
  assert.deepEqual(mapViewRoute(undefined, "n", () => undefined), []);
});

test("supports route_raw while retaining compatibility with route", () => {
  assert.equal(gpsRouteRaw({ route_raw: "nesw", route: "old" }), "nesw");
  assert.equal(gpsRouteRaw({ route: "ud" }), "ud");
  assert.equal(gpsRouteRaw(undefined), "");
});
