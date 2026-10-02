import assert from "node:assert/strict";
import test from "node:test";
import { serializeManualRouteAmendments, waitForManualLayoutCommit, type ManualLayoutReply } from "./manual-layout.ts";
import { createLayoutModel, planLayoutModel } from "../map-layout/model.ts";
import { validManualLayoutRequest } from "../nukefire-mapper/manual-layout.ts";
import { amendmentWaypointsBetween, indexRouteAmendments } from "../nukefire-mapper/routing.ts";

function replies() {
  const handlers = new Set<(reply: Readonly<ManualLayoutReply>) => void>();
  return {
    subscribe(receive: (reply: Readonly<ManualLayoutReply>) => void) {
      handlers.add(receive);
      return { off: () => handlers.delete(receive) };
    },
    emit(reply: ManualLayoutReply) { for (const receive of handlers) receive(reply); },
    count: () => handlers.size,
  };
}

test("a fast commit receipt is observed, and concurrent callers cannot consume each other's reply", async () => {
  const bus = replies();
  let completed = false;
  const pending = waitForManualLayoutCommit("one", 7, bus.subscribe, () => {
    bus.emit({ requestId: "two", sessionId: 7, accepted: true });
    bus.emit({ requestId: "one", sessionId: 8, accepted: true });
  }).then(() => { completed = true; });
  await Promise.resolve();
  assert.equal(completed, false);
  bus.emit({ requestId: "one", sessionId: 7, accepted: true });
  await pending;
  assert.equal(bus.count(), 0);
  await waitForManualLayoutCommit("instant", 7, bus.subscribe, () =>
    bus.emit({ requestId: "instant", sessionId: 7, accepted: true }));
  assert.equal(bus.count(), 0);
});

test("a rejected commit rejects the command and releases its subscription", async () => {
  const bus = replies();
  await assert.rejects(waitForManualLayoutCommit("old", 7, bus.subscribe, () =>
    bus.emit({ requestId: "old", sessionId: 7, accepted: false, error: "Map changed" })), /Map changed/);
  assert.equal(bus.count(), 0);
});

test("lost receipts report an unknown result, and failed sends leave no waiter", async () => {
  const bus = replies();
  await assert.rejects(waitForManualLayoutCommit("lost", 7, bus.subscribe, () => {}, 1), /result is unknown/);
  assert.equal(bus.count(), 0);
  await assert.rejects(waitForManualLayoutCommit("send", 7, bus.subscribe, () => {
    throw new Error("owner disappeared");
  }), /owner disappeared/);
  assert.equal(bus.count(), 0);
});

test("a zero-move engine detour survives the clone-safe manual bridge", () => {
  const model = createLayoutModel({
    rooms: [
      { id: "west", roomNumber: 1, position: { x: -2, y: 0, level: 0 }, movable: false },
      { id: "east", roomNumber: 2, position: { x: 2, y: 0, level: 0 }, movable: false },
      { id: "north", roomNumber: 3, position: { x: 0, y: -2, level: 0 }, movable: false },
      { id: "south", roomNumber: 4, position: { x: 0, y: 2, level: 0 }, movable: false },
    ],
    edges: [{ from: "west", to: "east", direction: "Other" }, { from: "north", to: "south", direction: "Other" }],
  });
  const plan = planLayoutModel(model, { type: "reflow" });
  assert.equal(plan.patch.moves.length, 0);
  assert.ok(plan.routeAmendments?.length);
  const routeAmendments = structuredClone(serializeManualRouteAmendments(plan.routeAmendments, plan.before.rooms));
  assert.ok(routeAmendments?.length);
  assert.equal(validManualLayoutRequest({ requestId: "detour", areaUuid: "area", sourceSnapshotKey: "source",
    plannedSnapshotKey: "planned", moves: [], routeAmendments }), true);
  const mapperIds = new Map(model.rooms.map((room) => [`room:${room.roomNumber}`, room.roomNumber!]));
  const converted = routeAmendments.map((amendment) => ({
    from: `room:${amendment.fromRoomNumber}`, to: `room:${amendment.toRoomNumber}`, waypoints: amendment.waypoints,
  }));
  const index = indexRouteAmendments(converted, mapperIds);
  for (const amendment of routeAmendments) {
    assert.deepEqual(amendmentWaypointsBetween(index, amendment.fromRoomNumber, amendment.toRoomNumber),
      amendment.waypoints.map(({ x, y }) => ({ x, y })));
  }
  // Serialization owns its arrays; a later caller mutation cannot alter the plan.
  (routeAmendments[0].waypoints as { x: number; y: number; level: number }[])[0].x += 10;
  assert.notEqual(routeAmendments[0].waypoints[0].x, plan.routeAmendments[0].waypoints[0].x);
  assert.equal(serializeManualRouteAmendments(undefined, model.rooms), undefined);
  assert.throws(() => serializeManualRouteAmendments([{ from: "missing", to: "east", waypoints: [{ x: 0, y: 0, level: 0 }] }], model.rooms), /unmapped room/);
});
