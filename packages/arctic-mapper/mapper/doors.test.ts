import assert from "node:assert/strict";
import test from "node:test";

import { describeDoor, doorIsClosed, doorUpdate, nextDoorState, openCommandFor } from "./doors.ts";

const door = (state: DoorState, name: string | null = null, opensWith: string | null = null): Door =>
    ({ state, name, opensWith });

test("a closed or locked door is closed; an open door and no door are not", () => {
    assert.equal(doorIsClosed({ door: door("closed") }), true);
    assert.equal(doorIsClosed({ door: door("locked") }), true);
    assert.equal(doorIsClosed({ door: door("open") }), false);
    assert.equal(doorIsClosed({ door: null }), false);
});

test("the door's own open command wins over the default", () => {
    assert.equal(openCommandFor({ door: door("closed", null, "pull lever") }, "open n"), "pull lever");
    assert.equal(openCommandFor({ door: door("open", null, "part brush") }, "open n"), "part brush");
    assert.equal(openCommandFor({ door: door("locked") }, "open n"), "open n");
    assert.equal(openCommandFor({ door: door("closed") }, "open n"), "open n");
});

test("the default applies only to a closed door or one the prompt shows closed", () => {
    assert.equal(openCommandFor({ door: door("open") }, "open n"), null);
    assert.equal(openCommandFor({ door: null }, "open n"), null);
    assert.equal(openCommandFor({ door: null }, "open door n", true), "open door n");
    assert.equal(openCommandFor({ door: door("open") }, "open door n", true), "open door n");
});

test("a door update keeps the name and open command it does not change", () => {
    const gate = door("closed", "gate", "unlock gate;open gate");
    assert.deepEqual(doorUpdate(gate, { state: "locked" }), {
        state: "locked",
        name: "gate",
        opensWith: "unlock gate;open gate",
    });
    assert.deepEqual(doorUpdate(gate, { opensWith: null }), {
        state: "closed",
        name: "gate",
        opensWith: null,
    });
    assert.deepEqual(doorUpdate(door("open"), { opensWith: "push wall" }), {
        state: "open",
        name: null,
        opensWith: "push wall",
    });
});

test("an open command on an exit without a door gives it a closed door", () => {
    assert.deepEqual(doorUpdate(null, { opensWith: "pull lever" }), {
        state: "closed",
        name: null,
        opensWith: "pull lever",
    });
});

test("closing and locking follow locked-implies-closed", () => {
    assert.equal(nextDoorState(null, "closed", true), "closed");
    assert.equal(nextDoorState("open", "closed", true), "closed");
    assert.equal(nextDoorState("locked", "closed", true), "locked");
    assert.equal(nextDoorState("closed", "closed", false), "open");
    assert.equal(nextDoorState("locked", "closed", false), "open");
    assert.equal(nextDoorState(null, "closed", false), null);

    assert.equal(nextDoorState(null, "locked", true), "locked");
    assert.equal(nextDoorState("open", "locked", true), "locked");
    assert.equal(nextDoorState("locked", "locked", false), "closed");
    assert.equal(nextDoorState("closed", "locked", false), "closed");
    assert.equal(nextDoorState("open", "locked", false), "open");
    assert.equal(nextDoorState(null, "locked", false), null);
});

test("doors describe their state and name", () => {
    assert.equal(describeDoor(door("closed")), "closed");
    assert.equal(describeDoor(door("locked", "gate")), 'locked "gate"');
});
