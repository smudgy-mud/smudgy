import assert from "node:assert/strict";
import test from "node:test";
import { copyDoor, doorIsClosed, reportedDoor, sameDoor } from "./doors.ts";

const door = (state: DoorState, name: string | null = null, opensWith: string | null = null): Door =>
  ({ state, name, opensWith });

test("a new exit gets the reported door, or none", () => {
  assert.deepEqual(reportedDoor(true, true, null), { state: "locked", name: null, opensWith: null });
  assert.deepEqual(reportedDoor(true, false, null), { state: "closed", name: null, opensWith: null });
  assert.deepEqual(reportedDoor(false, true, null), { state: "locked", name: null, opensWith: null });
  assert.equal(reportedDoor(false, false, null), null);
});

test("a report keeps the door's name and opening command", () => {
  const gate = door("open", "gate", "unlock gate;open gate");
  assert.deepEqual(reportedDoor(true, false, gate), {
    state: "closed",
    name: "gate",
    opensWith: "unlock gate;open gate",
  });
  assert.deepEqual(reportedDoor(true, true, gate), {
    state: "locked",
    name: "gate",
    opensWith: "unlock gate;open gate",
  });
});

test("a door the report no longer shows closed stays, open", () => {
  assert.deepEqual(reportedDoor(false, false, door("closed", null, "pull lever")), {
    state: "open",
    name: null,
    opensWith: "pull lever",
  });
  assert.deepEqual(reportedDoor(false, false, door("locked")), {
    state: "open",
    name: null,
    opensWith: null,
  });
});

test("an unchanged report matches the exit's door", () => {
  const current = door("closed", "hatch", null);
  assert.equal(sameDoor(current, copyDoor(reportedDoor(true, false, current))), true);
  assert.equal(sameDoor(door("open"), copyDoor(reportedDoor(false, false, door("open")))), true);
  assert.equal(sameDoor(null, copyDoor(reportedDoor(false, false, null))), true);
  assert.equal(sameDoor(current, copyDoor(reportedDoor(false, false, current))), false);
  assert.equal(sameDoor(null, copyDoor(reportedDoor(true, false, null))), false);
});

test("copies fill a missing name and opening command with null", () => {
  assert.deepEqual(copyDoor({ state: "closed" }), door("closed"));
  assert.equal(copyDoor(null), null);
  assert.equal(copyDoor(undefined), null);
});

test("closed and locked doors are closed", () => {
  assert.equal(doorIsClosed(door("closed")), true);
  assert.equal(doorIsClosed(door("locked")), true);
  assert.equal(doorIsClosed(door("open")), false);
  assert.equal(doorIsClosed(null), false);
});
