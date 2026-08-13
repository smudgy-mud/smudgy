import assert from "node:assert/strict";
import test from "node:test";
import { compareLayoutQuality } from "./layout.ts";
import {
  createLayoutModel,
  createLayoutWorkspace,
  planLayoutModel,
  resolveElevationGeometry,
} from "./model.ts";

const at = (x: number, y: number, level = 0) => ({ x, y, level });

test("projects e,u,e,d as e,ne,e,se when the local path flows east", () => {
  const workspace = createLayoutWorkspace(createLayoutModel({
    rooms: [
      { id: "start", position: at(0, 0), movable: true },
      { id: "east", position: at(1, 0), movable: true },
    ],
    edges: [
      { from: "start", to: "east", direction: "East" },
      { from: "east", to: "start", direction: "West" },
    ],
  }));

  const up = workspace.plan({
    type: "add-room",
    from: "east",
    direction: "Up",
    elevation: "projected",
    temporaryId: "upper-west",
  }, { allowExistingMoves: false });
  assert.deepEqual(up.patch.placements[0]?.position, at(2, -1));
  workspace.accept(up);

  const east = workspace.plan({
    type: "add-room",
    from: "upper-west",
    direction: "East",
    temporaryId: "upper-east",
  }, { allowExistingMoves: false });
  assert.deepEqual(east.patch.placements[0]?.position, at(3, -1));
  workspace.accept(east);

  const down = workspace.plan({
    type: "add-room",
    from: "upper-east",
    direction: "Down",
    elevation: "auto",
    temporaryId: "end",
  }, { allowExistingMoves: false });
  assert.deepEqual(down.patch.placements[0]?.position, at(4, 0));
});

test("keeps an explicitly level-based up exit on the next map level", () => {
  const result = planLayoutModel(createLayoutModel({
    rooms: [{ id: "start", position: at(5, 7), movable: true }],
    edges: [],
  }), {
    type: "add-room",
    from: "start",
    direction: "Up",
    elevation: "levels",
  }, { allowExistingMoves: false });

  assert.deepEqual(result.patch.placements[0]?.position, at(5, 7, 1));
});

test("classifies same-level U/D links as a coherent locally flowing projection", () => {
  const model = resolveElevationGeometry(createLayoutModel({
    rooms: [
      { id: "west", position: at(-1, 0), movable: true },
      { id: "lower", position: at(0, 0), movable: true },
      { id: "upper", position: at(4, -2), movable: true },
      { id: "east", position: at(5, -2), movable: true },
    ],
    edges: [
      { from: "west", to: "lower", direction: "East" },
      { from: "lower", to: "west", direction: "West" },
      { from: "lower", to: "upper", direction: "Up" },
      { from: "upper", to: "lower", direction: "Down" },
      { from: "upper", to: "east", direction: "East" },
      { from: "east", to: "upper", direction: "West" },
    ],
  }));

  assert.deepEqual(
    model.edges.find((edge) => edge.from === "lower" && edge.to === "upper")?.constraintVector,
    at(1, -1),
  );
  assert.deepEqual(
    model.edges.find((edge) => edge.from === "upper" && edge.to === "lower")?.constraintVector,
    at(-1, 1),
  );
  const reflowed = planLayoutModel(model, { type: "reflow", anchor: "lower" });
  assert.deepEqual(reflowed.positions.get("upper"), at(1, -1));
  assert.deepEqual(reflowed.positions.get("east"), at(2, -1));
  assert.equal(reflowed.quality.cardinalRayViolations, 0);
});

test("leaves different-level U/D links vertical", () => {
  const model = resolveElevationGeometry(createLayoutModel({
    rooms: [
      { id: "lower", position: at(0, 0, 0), movable: true },
      { id: "upper", position: at(3, 2, 1), movable: true },
    ],
    edges: [
      { from: "lower", to: "upper", direction: "Up" },
      { from: "upper", to: "lower", direction: "Down" },
    ],
  }));

  assert.equal(model.edges.every((edge) => edge.constraintVector === undefined), true);
});

test("reflows a complete existing model without any observations", () => {
  const result = planLayoutModel(createLayoutModel({
    rooms: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(5, 2), movable: true },
    ],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "a", direction: "West" },
    ],
  }), { type: "reflow", anchor: "a" });

  assert.deepEqual(result.positions.get("a"), at(0, 0));
  assert.deepEqual(result.positions.get("b"), at(1, 0));
  assert.equal(result.patch.moves.length, 1);
});

test("moves a nonmatching occupant out of the ideal cell when adding a room", () => {
  const result = planLayoutModel(createLayoutModel({
    rooms: [
      { id: "source", position: at(0, 0), movable: true },
      { id: "occupant", position: at(1, 0), movable: true },
    ],
    edges: [],
  }), {
    type: "add-room",
    from: "source",
    direction: "East",
    temporaryId: "new-room",
  });

  assert.deepEqual(result.patch.placements[0]?.position, at(1, 0));
  assert.equal(result.patch.moves.some((move) => move.id === "occupant"), true);
});

test("reflows two existing rooms into an exact new directional connection", () => {
  const result = planLayoutModel(createLayoutModel({
    rooms: [
      { id: "source", position: at(0, 0), movable: true },
      { id: "destination", position: at(4, 2), movable: true },
      { id: "destination-east", position: at(5, 2), movable: true },
    ],
    edges: [
      { from: "destination", to: "destination-east", direction: "East" },
      { from: "destination-east", to: "destination", direction: "West" },
    ],
  }), {
    type: "connect-rooms",
    from: "source",
    to: "destination",
    direction: "East",
  });

  const source = result.positions.get("source");
  const destination = result.positions.get("destination");
  const destinationEast = result.positions.get("destination-east");
  assert.ok(source);
  assert.ok(destination);
  assert.ok(destinationEast);
  assert.deepEqual(at(destination.x - source.x, destination.y - source.y, destination.level - source.level), at(1, 0));
  assert.deepEqual(at(destinationEast.x - destination.x, destinationEast.y - destination.y, destinationEast.level - destination.level), at(1, 0));
  assert.equal(result.patch.placements.length, 0);
  assert.equal(result.quality.cardinalRayViolations, 0);
  assert.equal(result.quality.cardinalSlack, 0);
});

test("thorough reflow prioritizes violation neighborhoods and beats one anchored pass", () => {
  const model = createLayoutModel({
    rooms: [
      { id: "0", position: at(3, -2), movable: true },
      { id: "1", position: at(1, 4), movable: true },
      { id: "2", position: at(5, -1), movable: true },
      { id: "3", position: at(-6, 3), movable: true },
      { id: "4", position: at(0, -4), movable: true },
      { id: "5", position: at(-4, -2), movable: true },
      { id: "6", position: at(4, -5), movable: true },
      { id: "7", position: at(1, 3), movable: true },
      { id: "8", position: at(1, 0), movable: true },
    ],
    edges: [
      { from: "0", to: "1", direction: "North" },
      { from: "1", to: "0", direction: "South" },
      { from: "0", to: "2", direction: "West" },
      { from: "2", to: "0", direction: "East" },
      { from: "0", to: "3", direction: "East" },
      { from: "3", to: "0", direction: "West" },
      { from: "1", to: "4", direction: "East" },
      { from: "4", to: "1", direction: "West" },
      { from: "4", to: "5", direction: "West" },
      { from: "5", to: "4", direction: "East" },
      { from: "1", to: "6", direction: "North" },
      { from: "6", to: "1", direction: "South" },
      { from: "0", to: "7", direction: "West" },
      { from: "7", to: "0", direction: "East" },
      { from: "2", to: "8", direction: "East" },
      { from: "8", to: "2", direction: "West" },
      { from: "6", to: "0", direction: "West" },
      { from: "0", to: "6", direction: "East" },
      // This contradictory pair makes an exact golden embedding impossible,
      // leaving enough local structure for anchor choice to matter.
      { from: "6", to: "0", direction: "South" },
      { from: "0", to: "6", direction: "North" },
    ],
  });

  const standard = planLayoutModel(model, { type: "reflow", anchor: "8" });
  const thorough = planLayoutModel(
    model,
    { type: "reflow", anchor: "8" },
    { effort: "thorough" },
  );

  assert.ok(compareLayoutQuality(thorough.quality, standard.quality) > 0);
  assert.equal(standard.quality.cardinalRayViolations, 4);
  assert.equal(thorough.quality.cardinalRayViolations, 2);
  assert.deepEqual(thorough.search?.baselineQuality, standard.quality);
  assert.deepEqual(
    thorough.search?.anchorsTried.slice(0, 4),
    ["8", "0", "2", "6"],
    "the requested anchor is followed by endpoints of its remaining directional violations",
  );
  assert.equal(thorough.search?.selectedAnchor, null);
  assert.ok((thorough.search?.planningPasses ?? 0) > (thorough.search?.anchorsTried.length ?? 0));
  for (const move of thorough.patch.moves) {
    assert.deepEqual(move.from, model.rooms.find((room) => room.id === move.id)?.position);
    assert.deepEqual(move.to, thorough.positions.get(move.id));
  }
});

test("thorough reflow preserves locked rooms and excludes them from derived anchors", () => {
  const model = createLayoutModel({
    rooms: [
      { id: "locked", position: at(10, 10), movable: false },
      { id: "middle", position: at(0, 0), movable: true },
      { id: "east", position: at(-4, 3), movable: true },
    ],
    edges: [
      { from: "locked", to: "middle", direction: "East" },
      { from: "middle", to: "locked", direction: "West" },
      { from: "middle", to: "east", direction: "East" },
      { from: "east", to: "middle", direction: "West" },
    ],
  });

  const result = planLayoutModel(
    model,
    { type: "reflow", anchor: "middle" },
    { effort: "thorough" },
  );

  assert.deepEqual(result.positions.get("locked"), at(10, 10));
  assert.equal(result.search?.anchorsTried.includes("locked"), false);
});
