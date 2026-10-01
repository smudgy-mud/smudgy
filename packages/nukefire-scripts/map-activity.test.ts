import assert from "node:assert/strict";
import test from "node:test";
import {
  advanceTrack,
  clearTracks,
  expandTrackedExitRefs,
  markAreaUnvisited,
  parseVisitedRooms,
  rememberVisitedRoom,
  trackApplication,
} from "./map-activity.ts";

test("visited rooms survive malformed entries and remain unique", () => {
  const parsed = parseVisitedRooms('{"area-a":[7,3,7,"bad"],"bad":"rooms"}');
  assert.deepEqual(parsed, { "area-a": [3, 7] });
  const remembered = rememberVisitedRoom(parsed, { areaId: "area-a", roomNumber: 5 });
  assert.deepEqual(remembered["area-a"], [3, 5, 7]);
  assert.deepEqual(markAreaUnvisited(remembered, "area-a"), {});
});

test("tracks retain rooms and exact traversed exits without duplicates", () => {
  const first = advanceTrack(undefined, { areaId: "area-a", roomNumber: 1 });
  const second = advanceTrack(
    first,
    { areaId: "area-a", roomNumber: 2 },
    { areaId: "area-a", exit: { room: 1, direction: "East" } },
  );
  const replayed = advanceTrack(
    second,
    { areaId: "area-a", roomNumber: 2 },
    { areaId: "area-a", exit: { room: 1, direction: "East" } },
  );

  assert.deepEqual(trackApplication("area-a", replayed), {
    style: "tracks",
    area: "area-a",
    rooms: [1, 2],
    exits: [{ room: 1, direction: "East" }],
  });
});

test("clearing tracks keeps the current location as the next path origin", () => {
  const atTwo = advanceTrack(undefined, { areaId: "area-a", roomNumber: 2 });
  const cleared = clearTracks(atTwo);
  assert.equal(trackApplication("area-a", cleared), undefined);

  const moved = advanceTrack(
    cleared,
    { areaId: "area-a", roomNumber: 3 },
    { areaId: "area-a", exit: { room: 2, direction: "South" } },
  );
  assert.deepEqual(moved.tracks["area-a"].rooms, [2, 3]);
});

test("one-way off-axis tracks include a synthetic canonical endpoint", () => {
  assert.deepEqual(
    expandTrackedExitRefs(
      [{ room: 2, direction: "Special", connectionKey: "connection-7" }],
      [{
        connectionKey: "connection-7",
        room: 2,
        direction: "Special",
        toRoom: 1,
        toDirection: null,
      }],
      [{
        key: "connection-7",
        endpointA: { room: 1, side: "West" },
        endpointB: { room: 2, side: "East" },
      }],
    ),
    [
      { room: 1, direction: "Special" },
      { room: 2, direction: "Special" },
    ],
  );
});
