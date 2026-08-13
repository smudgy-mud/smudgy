import assert from "node:assert/strict";
import test from "node:test";
import type { GridPosition, LayoutEdge, LayoutNode } from "./layout.ts";
import { stackVerticalTraversals } from "./vertical-levels.ts";

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

test("leaves rooms without vertical traversals at their observed z", () => {
  const result = stackVerticalTraversals(
    [node("center", 0, 0), node("hill", 1, 0, 3)],
    [edge("center", "hill", "East")],
    new Map(),
    "center",
  );

  assert.deepEqual(relative(result, "hill"), at(1, 0, 3));
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
