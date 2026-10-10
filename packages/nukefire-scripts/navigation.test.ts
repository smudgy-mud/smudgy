import assert from "node:assert/strict";
import test from "node:test";
import {
  buildNavigationRoute,
  formatRoute,
  splitArrivalCommands,
  takeCommandArgument,
  type NavigationAreaId,
  type NavigationDoor,
  type NavigationRoom,
} from "./navigation.ts";

const AREA = "11111111-1111-4111-8111-111111111111";
const OTHER = "33333333-3333-4333-8333-333333333333";

function room(
  room_number: number,
  exits: NavigationRoom["exits"] = [],
  area_id: NavigationAreaId = AREA,
): NavigationRoom {
  return { area_id, room_number, exits };
}

test("turns weighted mapper paths into explicit and directional commands", () => {
  const rooms = new Map<string, NavigationRoom>([
    [`${AREA}:10`, room(10, [{
      from_direction: "North",
      to_area_id: AREA,
      to_room_number: 11,
      command: "north",
    }])],
    [`${AREA}:11`, room(11, [{
      from_direction: "Special",
      to_area_id: OTHER,
      to_room_number: 20,
      command: "enter portal",
      door: { state: "closed", name: null, opensWith: null },
    }])],
    [`${OTHER}:20`, room(20, [], OTHER)],
  ]);
  const route = buildNavigationRoute(
    [[AREA, 10], [AREA, 11], [OTHER, 20]],
    (area, number) => rooms.get(`${area}:${number}`),
  );

  assert.equal(route.error, undefined);
  assert.deepEqual(route.steps.map(({ command, closed }) => ({ command, closed })), [
    { command: "north", closed: false },
    { command: "enter portal", closed: true },
  ]);
  assert.equal(formatRoute(route.steps), "north ; enter portal");
});

test("reads each hop's door: locked is closed too, and an open door is neither", () => {
  const exit = (to: number, door: NavigationDoor | null) => ({
    from_direction: "East",
    to_area_id: AREA,
    to_room_number: to,
    command: null,
    door,
  });
  const rooms = new Map<number, NavigationRoom>([
    [1, room(1, [exit(2, { state: "locked", name: "gate", opensWith: "unlock gate;open gate" })])],
    [2, room(2, [exit(3, { state: "open", name: null, opensWith: null })])],
    [3, room(3, [exit(4, null)])],
    [4, room(4)],
  ]);
  const route = buildNavigationRoute(
    [[AREA, 1], [AREA, 2], [AREA, 3], [AREA, 4]],
    (_area, number) => rooms.get(number),
  );

  assert.equal(route.error, undefined);
  assert.deepEqual(route.steps.map(({ closed, locked }) => ({ closed, locked })), [
    { closed: true, locked: true },
    { closed: false, locked: false },
    { closed: false, locked: false },
  ]);
});

test("falls back to the canonical short command when an exit has none", () => {
  const route = buildNavigationRoute(
    [[AREA, 1], [AREA, 2]],
    (_area, number) => number === 1
      ? room(1, [{
        from_direction: "Northeast",
        to_area_id: AREA,
        to_room_number: 2,
        command: null,
      }])
      : room(2),
  );
  assert.equal(route.error, undefined);
  assert.equal(route.steps[0]?.command, "ne");
});

test("reports a path whose durable exit no longer exists", () => {
  const route = buildNavigationRoute(
    [[AREA, 1], [AREA, 2]],
    (_area, number) => room(number),
  );
  assert.match(route.error ?? "", /no exit/);
  assert.deepEqual(route.steps, []);
});

test("parses quoted room names without consuming arrival commands", () => {
  assert.deepEqual(takeCommandArgument('"The Temple of Technology" look north;look south'), {
    value: "The Temple of Technology",
    rest: "look north;look south",
  });
  assert.deepEqual(takeCommandArgument("3001 look"), { value: "3001", rest: "look" });
  assert.deepEqual(splitArrivalCommands(" look north; ;look south "), ["look north", "look south"]);
});
