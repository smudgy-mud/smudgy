import assert from "node:assert/strict";
import test from "node:test";
import type { GridPosition, LayoutEdge, LayoutNode } from "./layout.ts";
import {
  restoreUnanchoredChartLevels,
  stackVerticalTraversals,
} from "./vertical-levels.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });
const node = (id: string, x: number, y: number, level = 0): LayoutNode => ({
  id,
  relative: at(x, y, level),
});
const edge = (
  from: string,
  to: string,
  direction: LayoutEdge["direction"],
): LayoutEdge => ({ from, to, direction });
const levels = (id: string, level: number): [string, number] => [id, level];
const relative = (nodes: readonly LayoutNode[], id: string): GridPosition => {
  const found = nodes.find((value) => value.id === id);
  assert.ok(found);
  return found.relative;
};

test("forces an up destination one level above its source", () => {
  const result = stackVerticalTraversals(
    [node("center", 0, 0), node("above", 1, 0)],
    [edge("center", "above", "Up"), edge("above", "center", "Down")],
    new Map(),
    "center",
  );

  assert.deepEqual(relative(result, "center"), at(0, 0));
  assert.deepEqual(relative(result, "above"), at(1, 0, 1));
});

test("forces a down destination one level below its source", () => {
  const result = stackVerticalTraversals(
    [node("center", 0, 0), node("below", 1, 0)],
    [edge("center", "below", "Down")],
    new Map(),
    "center",
  );

  assert.deepEqual(relative(result, "below"), at(1, 0, -1));
});

test("stacks a chain of vertical traversals one level per step", () => {
  const result = stackVerticalTraversals(
    [node("center", 0, 0), node("mid", 1, 0), node("top", 2, 0)],
    [edge("center", "mid", "Up"), edge("mid", "top", "Up")],
    new Map(),
    "center",
  );

  assert.deepEqual(relative(result, "center"), at(0, 0));
  assert.deepEqual(relative(result, "mid"), at(1, 0, 1));
  assert.deepEqual(relative(result, "top"), at(2, 0, 2));
});

test("seeds new rooms from an established neighbor's durable level", () => {
  const result = stackVerticalTraversals(
    [node("center", 0, 0), node("loft", 1, 0), node("new", 2, 0)],
    [edge("loft", "new", "Up")],
    new Map([levels("center", 0), levels("loft", 4)]),
    "center",
  );

  assert.deepEqual(relative(result, "center"), at(0, 0));
  assert.deepEqual(relative(result, "loft"), at(1, 0, 4));
  assert.deepEqual(relative(result, "new"), at(2, 0, 5));
});

test("seeds a new Map.Local plane from a resident absent from its chart", () => {
  const result = stackVerticalTraversals(
    [node("arrival", 0, 0), node("east", 1, 0), node("south", 0, 1)],
    [
      edge("resident", "arrival", "Up"),
      edge("arrival", "resident", "Down"),
      edge("arrival", "east", "East"),
      edge("arrival", "south", "South"),
    ],
    new Map([levels("resident", 4)]),
    "arrival",
  );

  assert.deepEqual(relative(result, "arrival"), at(0, 0, 5));
  assert.deepEqual(relative(result, "east"), at(1, 0, 5));
  assert.deepEqual(relative(result, "south"), at(0, 1, 5));
});

test("seeds a downward arrival from a resident absent from Map.Local", () => {
  const result = stackVerticalTraversals(
    [node("arrival", 0, 0)],
    [edge("resident", "arrival", "Down")],
    new Map([levels("resident", -2)]),
    "arrival",
  );

  assert.deepEqual(relative(result, "arrival"), at(0, 0, -3));
});

test("uses the server plane when a returning chart has no durable seam", () => {
  const result = stackVerticalTraversals(
    [node("arrival", 0, 0), node("east", 1, 0), node("upper", 0, -1)],
    [edge("arrival", "east", "East"), edge("arrival", "upper", "Up")],
    new Map([levels("old-area-room", 0)]),
    "arrival",
    3,
  );

  assert.deepEqual(relative(result, "arrival"), at(0, 0, 3));
  assert.deepEqual(relative(result, "east"), at(1, 0, 3));
  assert.deepEqual(relative(result, "upper"), at(0, -1, 4));
});

test("uses the server plane for an isolated returning room", () => {
  const result = stackVerticalTraversals(
    [node("arrival", 0, 0)],
    [],
    new Map([levels("old-area-room", 0)]),
    "arrival",
    -2,
  );

  assert.deepEqual(relative(result, "arrival"), at(0, 0, -2));
});

test("restores an unanchored server plane after the planner packs it at zero", () => {
  const result = restoreUnanchoredChartLevels(
    new Map([
      ["old-area-room", at(0, 0)],
      ["arrival", at(4, 0)],
      ["east", at(5, 0)],
    ]),
    [node("arrival", 0, 0, 3), node("east", 1, 0, 3)],
    [edge("arrival", "east", "East")],
    new Map([levels("old-area-room", 0)]),
    "arrival",
  );

  assert.deepEqual(result.get("arrival"), at(4, 0, 3));
  assert.deepEqual(result.get("east"), at(5, 0, 3));
  assert.deepEqual(result.get("old-area-room"), at(0, 0));
});

test("does not restore a chart component anchored through a durable seam", () => {
  const packed = new Map([
    ["resident", at(0, 0, 4)],
    ["arrival", at(0, 0, 5)],
  ]);
  const result = restoreUnanchoredChartLevels(
    packed,
    [node("arrival", 0, 0, 9)],
    [edge("resident", "arrival", "Up")],
    new Map([levels("resident", 4)]),
    "arrival",
  );

  assert.equal(result, packed);
  assert.deepEqual(result.get("arrival"), at(0, 0, 5));
});

test("mirrors durable level differences between established rooms", () => {
  const result = stackVerticalTraversals(
    [node("lower", 0, 0), node("upper", 1, 0)],
    [edge("lower", "upper", "Up")],
    new Map([levels("lower", 0), levels("upper", 1)]),
    "lower",
  );

  assert.deepEqual(relative(result, "lower"), at(0, 0));
  assert.deepEqual(relative(result, "upper"), at(1, 0, 1));
});

test("never rewrites an established room from a vertical observation", () => {
  const result = stackVerticalTraversals(
    [node("lower", 0, 0), node("beside", 1, 0)],
    [edge("lower", "beside", "Up")],
    new Map([levels("lower", 0), levels("beside", 0)]),
    "lower",
  );

  assert.deepEqual(relative(result, "beside"), at(1, 0));
});

test("ignores raw z noise across an ordinary chart edge", () => {
  const result = stackVerticalTraversals(
    [node("center", 0, 0), node("hill", 1, 0, 3)],
    [edge("center", "hill", "East")],
    new Map(),
    "center",
  );

  assert.deepEqual(relative(result, "hill"), at(1, 0));
});

test("only Up and Down split ordinary, diagonal, and special chart planes", () => {
  const result = stackVerticalTraversals(
    [
      node("center", 0, 0),
      node("east", 1, 0, 8),
      node("northeast", 2, -1, -4),
      node("portal", 3, -1, 12),
      node("above", 4, -1, -7),
      node("above-east", 5, -1, 20),
    ],
    [
      edge("center", "east", "East"),
      edge("east", "northeast", "Northeast"),
      edge("northeast", "portal", "Special"),
      edge("portal", "above", "Up"),
      edge("above", "above-east", "East"),
    ],
    new Map(),
    "center",
  );

  assert.deepEqual(relative(result, "center"), at(0, 0));
  assert.deepEqual(relative(result, "east"), at(1, 0));
  assert.deepEqual(relative(result, "northeast"), at(2, -1));
  assert.deepEqual(relative(result, "portal"), at(3, -1));
  assert.deepEqual(relative(result, "above"), at(4, -1, 1));
  assert.deepEqual(relative(result, "above-east"), at(5, -1, 1));
});

test("abandons a stack that would make two new rooms share a chart cell", () => {
  const result = stackVerticalTraversals(
    [node("center", 0, 0), node("above", 1, 0), node("occupant", 1, 0, 1)],
    [edge("center", "above", "Up")],
    new Map(),
    "center",
  );

  assert.deepEqual(relative(result, "above"), at(1, 0));
  assert.deepEqual(relative(result, "occupant"), at(1, 0, 1));
});

test("keeps the abandoned stack's durable mirror for established rooms", () => {
  const result = stackVerticalTraversals(
    [
      node("center", 0, 0),
      node("loft", 1, 0),
      node("above", 2, 0),
      node("occupant", 2, 0, 1),
    ],
    [edge("center", "above", "Up")],
    new Map([levels("center", 0), levels("loft", 2)]),
    "center",
  );

  assert.deepEqual(relative(result, "above"), at(2, 0));
  assert.deepEqual(relative(result, "loft"), at(1, 0, 2));
});

test("continues stacking through a chain observed from both ends", () => {
  const result = stackVerticalTraversals(
    [node("a", 0, 0), node("shared", 1, 0), node("c", 2, 0)],
    [edge("a", "shared", "Up"), edge("c", "shared", "Down")],
    new Map(),
  );

  assert.deepEqual(relative(result, "a"), at(0, 0));
  assert.deepEqual(relative(result, "shared"), at(1, 0, 1));
  assert.deepEqual(relative(result, "c"), at(2, 0, 2));
});

test("a room claimed by conflicting vertical paths keeps its nearest-seed level", () => {
  const result = stackVerticalTraversals(
    [node("a", 0, 0), node("b", 1, 0), node("c", 2, 0)],
    [edge("a", "b", "Up"), edge("b", "c", "Up"), edge("a", "c", "Up")],
    new Map(),
    "a",
  );

  assert.deepEqual(relative(result, "b"), at(1, 0, 1));
  assert.deepEqual(relative(result, "c"), at(2, 0, 1));
});
