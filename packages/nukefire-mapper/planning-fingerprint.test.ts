import assert from "node:assert/strict";
import test from "node:test";
import {
  planningFingerprint,
  type PlanningFingerprintRoom,
} from "./planning-fingerprint.ts";

const rooms: PlanningFingerprintRoom[] = [
  {
    roomNumber: 2,
    vnum: 102,
    position: { x: 1, y: 0, level: 0 },
    movable: true,
    internalExits: [{ direction: "West", toRoomNumber: 1 }],
  },
  {
    roomNumber: 1,
    vnum: 101,
    position: { x: 0, y: 0, level: 0 },
    movable: false,
    internalExits: [
      { direction: "North", toRoomNumber: 3 },
      { direction: "East", toRoomNumber: 2 },
    ],
  },
];

function changed(
  roomNumber: number,
  update: (room: PlanningFingerprintRoom) => PlanningFingerprintRoom,
): PlanningFingerprintRoom[] {
  return rooms.map((room) => room.roomNumber === roomNumber ? update(room) : room);
}

test("planning fingerprint ignores room and exit enumeration order", () => {
  const reordered = [...rooms]
    .reverse()
    .map((room) => ({ ...room, internalExits: [...room.internalExits].reverse() }));

  assert.equal(planningFingerprint(reordered), planningFingerprint(rooms));
});

test("planning fingerprint detects position, identity, and movability changes", () => {
  const original = planningFingerprint(rooms);

  assert.notEqual(
    planningFingerprint(changed(1, (room) => ({
      ...room,
      position: { ...room.position, x: room.position.x + 1 },
    }))),
    original,
  );
  assert.notEqual(
    planningFingerprint(changed(1, (room) => ({ ...room, vnum: 201 }))),
    original,
  );
  assert.notEqual(
    planningFingerprint(changed(1, (room) => ({ ...room, movable: !room.movable }))),
    original,
  );
});

test("planning fingerprint detects internal-exit changes", () => {
  const original = planningFingerprint(rooms);

  assert.notEqual(
    planningFingerprint(changed(1, (room) => ({
      ...room,
      internalExits: room.internalExits.slice(1),
    }))),
    original,
  );
  assert.notEqual(
    planningFingerprint(changed(1, (room) => ({
      ...room,
      internalExits: room.internalExits.map((exit, index) =>
        index === 0 ? { ...exit, direction: "South" } : exit
      ),
    }))),
    original,
  );
});
