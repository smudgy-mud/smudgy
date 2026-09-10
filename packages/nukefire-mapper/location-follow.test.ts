import assert from "node:assert/strict";
import test from "node:test";
import {
  resolveFollowedLocation,
  type FollowArea,
  type FollowMapper,
  type FollowRoom,
} from "./location-follow.ts";

function makeArea(
  id: AreaId,
  storage: MapStorage,
  rooms: { number: RoomNumber; externalId?: string }[],
): FollowArea {
  const byNumber = new Map<RoomNumber, FollowRoom>(rooms.map((room) => [room.number, {
    area_id: id,
    room_number: room.number,
    externalId: room.externalId,
  }]));
  return {
    id,
    storage,
    room_numbers: rooms.map((room) => room.number),
    room: (number) => byNumber.get(number),
  };
}

function makeMapper(
  areas: FollowArea[],
  found: FollowRoom | undefined,
  current?: { area: AreaId; room?: RoomNumber },
): FollowMapper {
  return {
    findRoomByExternalId: () => found,
    getCurrentLocation: () => current,
    getAreaById(id: AreaId): FollowArea {
      const area = areas.find((candidate) => candidate.id === id);
      if (!area) throw new Error("unknown area");
      return area;
    },
    areas,
  };
}

const CLOUD_ID: AreaId = "11111111-1111-4111-8111-111111111111";
const LOCAL_ID: AreaId = "22222222-2222-4222-8222-222222222222";
const SESSION_ID: AreaId = "33333333-3333-4333-8333-333333333333";

test("follows the sole binding of an unambiguous room id", () => {
  const cloud = makeArea(CLOUD_ID, "cloud", [{ number: 7, externalId: "100" }]);
  const mapper = makeMapper([cloud], cloud.room(7));

  assert.deepEqual(resolveFollowedLocation(mapper, "100"), { area: CLOUD_ID, room: 7 });
});

test("a duplicated room id prefers the area already being viewed", () => {
  const cloud = makeArea(CLOUD_ID, "cloud", [{ number: 7, externalId: "100" }]);
  const local = makeArea(LOCAL_ID, "local", [{ number: 9, externalId: "100" }]);
  const mapper = makeMapper([cloud, local], local.room(9), { area: CLOUD_ID, room: 3 });

  assert.deepEqual(resolveFollowedLocation(mapper, "100"), { area: CLOUD_ID, room: 7 });
});

test("a session binding yields to a durable copy of the same room id", () => {
  const session = makeArea(SESSION_ID, "session", [{ number: 4, externalId: "100" }]);
  const local = makeArea(LOCAL_ID, "local", [{ number: 9, externalId: "100" }]);
  const mapper = makeMapper([session, local], session.room(4));

  assert.deepEqual(resolveFollowedLocation(mapper, "100"), { area: LOCAL_ID, room: 9 });
});

test("an unmapped room id resolves to nothing", () => {
  const mapper = makeMapper([], undefined);

  assert.equal(resolveFollowedLocation(mapper, "100"), undefined);
});
