import assert from "node:assert/strict";
import test from "node:test";
import {
  advanceTrack,
  clearTracks,
  expandTrackedExitRefs,
  isRoomVisited,
  markAreaUnvisited,
  migrateVisitedRooms,
  parseVisitedRooms,
  parseVisitedVnums,
  rememberVisitedRoom,
  remapTracks,
  remapVisitedRooms,
  roomVnum,
  trackApplication,
} from "./map-activity.ts";

test("visited rooms survive malformed entries and remain unique", () => {
  const parsed = parseVisitedRooms('{"area-a":[7,3,7,"bad"],"bad":"rooms"}');
  assert.deepEqual(parsed, { "area-a": [3, 7] });
  const remembered = rememberVisitedRoom(parsed, { areaId: "area-a", roomNumber: 5 });
  assert.deepEqual(remembered["area-a"], [3, 5, 7]);
  assert.deepEqual(markAreaUnvisited(remembered, "area-a"), {});
});

test("a whole merge preserves the trail and next traversal without inventing movement", () => {
  const source = advanceTrack(
    advanceTrack(undefined, { areaId: "source", roomNumber: 1 }),
    { areaId: "source", roomNumber: 2 },
    { areaId: "source", exit: { room: 1, direction: "East", connectionKey: "link" } },
  );
  const migrated = remapTracks(source, {
    into: "destination",
    rooms: [{ from: { area: "source", room: 1 }, to: 3 }, { from: { area: "source", room: 2 }, to: 4 }],
  });
  assert.deepEqual(migrated.location, { areaId: "destination", roomNumber: 4 });
  assert.equal(trackApplication("source", migrated), undefined);
  assert.deepEqual(trackApplication("destination", migrated), {
    style: "tracks", area: "destination", rooms: [3, 4],
    exits: [{ room: 3, direction: "East", connectionKey: "link" }],
  });
  assert.deepEqual(advanceTrack(migrated, migrated.location!), migrated);
  const moved = advanceTrack(migrated, { areaId: "destination", roomNumber: 5 }, {
    areaId: "destination", exit: { room: 4, direction: "South" },
  });
  assert.deepEqual(moved.tracks.destination.rooms, [3, 4, 5]);
  assert.deepEqual(moved.tracks.destination.exits[1], { room: 4, direction: "South" });
  assert.deepEqual(source.tracks.source.rooms, [1, 2], "input snapshot is retained");
});

test("partial merges preserve stationary trails and area-rule splits can move them back", () => {
  const snapshot = {
    location: { areaId: "elsewhere", roomNumber: 9 },
    tracks: {
      source: { rooms: [1, 2], exits: [{ room: 1, direction: "East" as const }, { room: 2, direction: "North" as const }] },
      destination: { rooms: [3], exits: [] },
    },
  };
  const joined = remapTracks(snapshot, {
    into: "destination", rooms: [{ from: { area: "source", room: 1 }, to: 3 }],
  });
  assert.deepEqual(joined.location, snapshot.location, "stationary player remains elsewhere");
  assert.deepEqual(joined.tracks.source, { rooms: [2], exits: [{ room: 2, direction: "North" }] });
  assert.deepEqual(joined.tracks.destination, { rooms: [3], exits: [{ room: 3, direction: "East" }] });
  const split = remapTracks(joined, {
    into: "new-section", rooms: [{ from: { area: "destination", room: 3 }, to: 1 }],
  });
  assert.equal(trackApplication("destination", split), undefined);
  assert.deepEqual(split.tracks["new-section"], { rooms: [1], exits: [{ room: 1, direction: "East" }] });
});

test("duplicate-room joins reconcile old connection identities against committed topology", () => {
  const migrated = remapTracks({
    location: { areaId: "source", roomNumber: 2 },
    tracks: {
      source: { rooms: [1, 2], exits: [{ room: 2, direction: "Special", connectionKey: "removed-link" }] },
      destination: { rooms: [3], exits: [] },
    },
  }, {
    into: "destination", rooms: [{ from: { area: "source", room: 1 }, to: 3 }, { from: { area: "source", room: 2 }, to: 3 }],
  });
  assert.deepEqual(migrated.tracks.destination.rooms, [3]);
  assert.deepEqual(expandTrackedExitRefs(migrated.tracks.destination.exits, [{
    connectionKey: "surviving-link", room: 3, direction: "Special", toRoom: 4, toDirection: null,
  }], [{
    key: "surviving-link", endpointA: { room: 4, side: "West" }, endpointB: { room: 3, side: "East" },
  }]), [{ room: 4, direction: "Special" }, { room: 3, direction: "Special" }]);
});

test("same-area joins remap the retained peer snapshot without losing its trail", () => {
  const peer = remapTracks({
    location: { areaId: "area", roomNumber: 9 },
    tracks: { area: { rooms: [1, 9], exits: [{ room: 9, direction: "West" }] } },
  }, { into: "area", rooms: [{ from: { area: "area", room: 9 }, to: 2 }] });
  const directedCopy = JSON.parse(JSON.stringify(peer));
  assert.deepEqual(directedCopy.location, { areaId: "area", roomNumber: 2 });
  assert.deepEqual(trackApplication("area", directedCopy), {
    style: "tracks", area: "area", rooms: [1, 2], exits: [{ room: 2, direction: "West" }],
  });
});

test("a surviving exact connection still distinguishes parallel links", () => {
  const traversals = [
    { connectionKey: "first", room: 1, direction: "Special" as const, toRoom: 2, toDirection: null },
    { connectionKey: "second", room: 1, direction: "Special" as const, toRoom: 3, toDirection: null },
  ];
  const connections = [
    { key: "first", endpointA: { room: 1, side: "East" as const }, endpointB: { room: 2, side: "West" as const } },
    { key: "second", endpointA: { room: 1, side: "East" as const }, endpointB: { room: 3, side: "West" as const } },
  ];
  assert.deepEqual(expandTrackedExitRefs([{ room: 1, direction: "Special", connectionKey: "second" }], traversals, connections), [
    { room: 1, direction: "Special" }, { room: 3, direction: "Special" },
  ]);
});

test("legacy visits migrate once and reused numbers cannot create false VNUM visits", () => {
  const legacy = { source: [1, 2, 8] };
  const original = (area: string, room: number): number | undefined =>
    area === "source" && room < 3 ? 2000 + room : undefined;
  const first = migrateVisitedRooms(legacy, new Set([9000]), original, true);
  assert.deepEqual([...first.vnums].sort((a, b) => a - b), [2001, 2002, 9000]);
  assert.deepEqual(first.visited, { source: [8] });
  // A dual-store upgrade can still contain stale numeric marks. Its VNUM store
  // is already present, so reading the new occupant does not certify a visit.
  const reused = migrateVisitedRooms(legacy, first.vnums, (_area, room) => room < 3 ? 3000 + room : undefined, false);
  assert.equal(reused.vnums.has(3001), false);
  assert.equal(isRoomVisited(legacy, reused.vnums, "source", 1, 3001), false);
  assert.equal(isRoomVisited(legacy, reused.vnums, "source", 1, 2001), true);
  assert.equal(isRoomVisited(reused.visited, reused.vnums, "source", 8, undefined), true);
  const restart = migrateVisitedRooms(reused.visited, reused.vnums, () => 4001, false);
  assert.equal(restart.vnums.has(4001), false, "unresolved marks are not backfilled on later loads");
  assert.equal(restart.vnums.has(2001), true, "previous VNUM history survives");
});

test("visit resets survive partial remaps and a subsequent reload", () => {
  const merge = { into: "destination", rooms: [{ from: { area: "source", room: 1 }, to: 3 }] };
  assert.deepEqual(remapVisitedRooms({ source: [1, 2], destination: [3] }, merge), { destination: [3], source: [2] });
  const reset = markAreaUnvisited({ destination: [3], source: [2] }, "destination");
  const vnums = new Set([2001, 2002]);
  vnums.delete(2001);
  const reloaded = migrateVisitedRooms(reset, vnums, () => undefined, false);
  assert.equal(isRoomVisited(reloaded.visited, reloaded.vnums, "destination", 3, 2001), false);
  assert.equal(isRoomVisited(reloaded.visited, reloaded.vnums, "source", 2, 2002), true);
});

test("queued merge addresses compose before legacy identity conversion", () => {
  const first = remapVisitedRooms({ source: [1, 2] }, {
    into: "middle", rooms: [{ from: { area: "source", room: 1 }, to: 3 }, { from: { area: "source", room: 2 }, to: 4 }],
  });
  const last = remapVisitedRooms(first, {
    into: "final", rooms: [{ from: { area: "middle", room: 3 }, to: 7 }, { from: { area: "middle", room: 4 }, to: 8 }],
  });
  const migrated = migrateVisitedRooms(last, new Set(), (_area, room) => 2000 + room, true);
  assert.deepEqual([...migrated.vnums], [2007, 2008]);
  assert.deepEqual(migrated.visited, {});
});

test("visited vnums survive malformed entries and remain unique", () => {
  assert.deepEqual(parseVisitedVnums("[307, 12, 307, \"bad\", 1.5, null]"), [12, 307]);
  assert.deepEqual(parseVisitedVnums("{\"rooms\":[1]}"), []);
  assert.deepEqual(parseVisitedVnums("not json"), []);
  assert.deepEqual(parseVisitedVnums(null), []);
  assert.equal(roomVnum("4012"), 4012);
  assert.equal(roomVnum(undefined), undefined);
  assert.equal(roomVnum("room-7"), undefined);
  assert.equal(roomVnum(" 7"), undefined);
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
