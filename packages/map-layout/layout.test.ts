import assert from "node:assert/strict";
import test from "node:test";
import {
  compactIntegralLayoutPlan,
  compareLayoutQuality,
  computeIntegralRouteAmendments,
  measureIntegralLayoutQuality,
  measureLayoutRoutingQuality,
  packsAxisGroups,
  planIntegralLayout,
  repairIntegralLayoutCrossingsDeep,
  safePushRepairs,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
  type LayoutEdge,
  type LayoutNode,
  type LayoutResident,
  type LayoutTraceEvent,
} from "./layout.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });
const node = (id: string, x: number, y: number, level = 0): LayoutNode => ({
  id,
  relative: at(x, y, level),
});
const resident = (
  id: string,
  x: number,
  y: number,
  movable = true,
  level = 0,
): LayoutResident => ({ id, position: at(x, y, level), movable });
const edge = (
  from: string,
  to: string,
  direction: LayoutEdge["direction"],
  constraintVector?: GridPosition,
): LayoutEdge => ({
  from,
  to,
  direction,
  constraintVector,
});

function plan(request: Omit<IntegralLayoutRequest, "allowExistingMoves">) {
  return planIntegralLayout({ ...request, allowExistingMoves: true });
}

test("places an ordinary cardinal neighbor one integral cell away", () => {
  const result = plan({
    centerId: "a",
    residents: [resident("a", 0, 0)],
    nodes: [node("a", 0, 0), node("b", 1, 0)],
    edges: [edge("a", "b", "East")],
  });

  assert.deepEqual(result.positions.get("b"), at(1, 0));
  assert.deepEqual(result.positions.get("a"), at(0, 0));
});

test("places an up exit on the next level without an x/y offset", () => {
  const result = plan({
    centerId: "a",
    residents: [resident("a", 4, 7)],
    // Map.Local coordinates are player-relative and may display a vertical
    // destination beside the player; topology is authoritative here.
    nodes: [node("a", 0, 0), node("upper", 3, -2)],
    edges: [edge("a", "upper", "Up")],
  });

  assert.deepEqual(result.positions.get("a"), at(4, 7));
  assert.deepEqual(result.positions.get("upper"), at(4, 7, 1));
});

test("anchors a new chart from a vertical resident omitted by the source", () => {
  const result = planIntegralLayout({
    centerId: "upper",
    allowExistingMoves: false,
    residents: [resident("lower", 4, 7, false, 3)],
    nodes: [node("upper", 0, 0, 4), node("east", 1, 0, 4)],
    edges: [
      edge("lower", "upper", "Up"),
      edge("upper", "lower", "Down"),
      edge("upper", "east", "East"),
    ],
  });

  assert.deepEqual(result.positions.get("lower"), at(4, 7, 3));
  assert.deepEqual(result.positions.get("upper"), at(4, 7, 4));
  assert.deepEqual(result.positions.get("east"), at(5, 7, 4));
  assert.equal(result.quality.cardinalRayViolations, 0);
});

test("counts an exit as mis-levelled when its rooms' levels disagree with its own vector", () => {
  const measure = (from: GridPosition, to: GridPosition, direction: LayoutEdge["direction"], vector?: GridPosition) => {
    const quality = measureIntegralLayoutQuality(
      new Map([["a", from], ["b", to]]),
      [edge("a", "b", direction, vector)],
    );
    return [quality.cardinalRayViolations, quality.levelViolations];
  };
  // A compass exit belongs on one level.
  assert.deepEqual(measure(at(0, 0), at(1, 0), "East"), [0, 0]);
  assert.deepEqual(measure(at(0, 0), at(1, 0, 1), "East"), [1, 1]);
  assert.deepEqual(measure(at(0, 0), at(-1, 0), "East"), [1, 0]);
  // Up leads higher and Down lower; level 2 is above level 1.
  assert.deepEqual(measure(at(0, 0, 1), at(0, 0, 2), "Up"), [0, 0]);
  assert.deepEqual(measure(at(0, 0, 1), at(0, 0, 1), "Up"), [1, 1]);
  assert.deepEqual(measure(at(0, 0, 1), at(0, 0, 0), "Up"), [1, 1]);
  assert.deepEqual(measure(at(0, 0, 1), at(0, 0, 2), "Down"), [1, 1]);
  // Higher but beside the room is drawn wrong on the plane only.
  assert.deepEqual(measure(at(0, 0, 1), at(1, 0, 2), "Up"), [1, 0]);
  // A projected Up exit is drawn on one level, so its vector decides.
  assert.deepEqual(measure(at(0, 0), at(1, -1), "Up", at(1, -1)), [0, 0]);
  assert.deepEqual(measure(at(0, 0), at(1, -1, 1), "Up", at(1, -1)), [1, 1]);
  // Exits without a direction to keep are never mis-levelled.
  assert.deepEqual(measure(at(0, 0), at(1, -1, 1), "Northeast"), [0, 0]);
  assert.deepEqual(measure(at(0, 0), at(3, 3, 2), "In"), [0, 0]);
  // No layout draws a self-loop on another level.
  const loop = measureIntegralLayoutQuality(new Map([["a", at(0, 0)]]), [edge("a", "a", "Up")]);
  assert.deepEqual([loop.cardinalRayViolations, loop.levelViolations], [1, 0]);
});

test("counts the extra levels exits drawn right climb or descend as level slack", () => {
  const slack = (to: GridPosition, direction: LayoutEdge["direction"], vector?: GridPosition) => {
    const quality = measureIntegralLayoutQuality(
      new Map([["a", at(0, 0)], ["b", to]]),
      [edge("a", "b", direction, vector)],
    );
    return [quality.cardinalRayViolations, quality.levelSlack, quality.cardinalSlack];
  };
  assert.deepEqual(slack(at(0, 0, 1), "Up"), [0, 0, 0]);
  assert.deepEqual(slack(at(0, 0, 3), "Up"), [0, 2, 2]);
  assert.deepEqual(slack(at(0, 0, -2), "Down"), [0, 1, 1]);
  // Only climbing or descending counts: a long flat exit, a projected Up
  // exit on one level, and an exit drawn wrong span no extra level.
  assert.deepEqual(slack(at(3, 0), "East"), [0, 0, 2]);
  assert.deepEqual(slack(at(2, -2), "Up", at(1, -1)), [0, 0, 1]);
  assert.deepEqual(slack(at(1, 0, 3), "Up"), [1, 0, 0]);
});

test("treats a projected up/down pair as an authoritative diagonal", () => {
  const result = plan({
    centerId: "lower",
    residents: [
      resident("lower", 0, 0, false),
      resident("upper", 4, -2),
    ],
    nodes: [],
    edges: [
      edge("lower", "upper", "Up", at(1, -1)),
      edge("upper", "lower", "Down", at(-1, 1)),
    ],
  });

  assert.deepEqual(result.positions.get("lower"), at(0, 0));
  assert.deepEqual(result.positions.get("upper"), at(1, -1));
  assert.equal(result.quality.cardinalRayViolations, 0);
  assert.equal(result.quality.cardinalSlack, 0);
});

test("reflows a down-exit destination onto the level below its source", () => {
  const result = plan({
    centerId: "a",
    residents: [resident("a", 2, 3), resident("lower", 6, 3)],
    nodes: [node("a", 0, 0), node("lower", 4, 0)],
    edges: [edge("a", "lower", "Down")],
  });

  assert.deepEqual(result.positions.get("a"), at(2, 3));
  assert.deepEqual(result.positions.get("lower"), at(2, 3, -1));
  assert.equal(result.movedExisting.has("lower"), true);
});

test("keeps a same-level vertical flow the chart requested when moves are disallowed", () => {
  // Level policy belongs to callers: a chart may deliberately flow an up/down
  // destination on its source's plane, and stable placement preserves that.
  const result = planIntegralLayout({
    centerId: "a",
    allowExistingMoves: false,
    residents: [resident("a", 0, 0)],
    nodes: [node("a", 0, 0), node("b", 1, 0)],
    edges: [edge("a", "b", "Up")],
  });

  assert.deepEqual(result.positions.get("b"), at(1, 0));
});

test("satisfies a cross-level chart's vertical ray without reflowing existing rooms", () => {
  const result = planIntegralLayout({
    centerId: "a",
    allowExistingMoves: false,
    residents: [resident("a", 2, 3, true, 5)],
    nodes: [node("a", 0, 0), node("b", 1, 0, 1)],
    edges: [edge("a", "b", "Up")],
  });

  assert.deepEqual(result.positions.get("b"), at(2, 3, 6));
  assert.equal(result.quality.cardinalRayViolations, 0);
});

test("moves an isolated blocker when that preserves exact cardinal adjacency", () => {
  const result = plan({
    centerId: "a",
    residents: [resident("a", 0, 0), resident("blocker", 1, 0)],
    nodes: [node("a", 0, 0), node("b", 1, 0)],
    edges: [edge("a", "b", "East")],
  });

  assert.deepEqual(result.positions.get("b"), at(1, 0));
  assert.notDeepEqual(result.positions.get("blocker"), at(1, 0));
  assert.equal(result.movedExisting.has("blocker"), true);
});

test("keeps a fixed blocker but moves the surrounding patch to retain a golden exit", () => {
  const result = planIntegralLayout({
    centerId: "a",
    allowExistingMoves: true,
    residents: [resident("a", 0, 0), resident("blocker", 1, 0, false)],
    nodes: [node("a", 0, 0), node("b", 1, 0)],
    edges: [edge("a", "b", "East")],
  });

  const placed = result.positions.get("b");
  const source = result.positions.get("a");
  assert.ok(placed);
  assert.ok(source);
  assert.equal(placed.x - source.x, 1);
  assert.equal(placed.y, source.y);
  assert.deepEqual(result.positions.get("blocker"), at(1, 0));
});

test("stable placement searches past a colliding zero-offset", () => {
  const result = planIntegralLayout({
    centerId: "a",
    allowExistingMoves: false,
    residents: [
      resident("a", 0, 0, false),
      resident("blocker", 1, 0, false),
    ],
    nodes: [node("a", 0, 0), node("new", 1, 0)],
    edges: [edge("a", "new", "East")],
  });

  assert.deepEqual(result.positions.get("a"), at(0, 0));
  assert.deepEqual(result.positions.get("blocker"), at(1, 0));
  assert.deepEqual(result.positions.get("new"), at(2, 0));
  assert.deepEqual(result.movedExisting, new Set());
});

test("moves a coherent block out of a late closet's ideal cell without distorting it", () => {
  const result = plan({
    centerId: "a",
    residents: [resident("a", 0, 0), resident("x", 1, 0), resident("y", 2, 0)],
    nodes: [node("a", 0, 0), node("b", 1, 0)],
    edges: [edge("a", "b", "East"), edge("x", "y", "East")],
  });

  const x = result.positions.get("x");
  const y = result.positions.get("y");
  assert.ok(x && y);
  assert.deepEqual(result.positions.get("b"), at(1, 0));
  assert.equal(y.x - x.x, 1);
  assert.equal(y.y, x.y);
});

test("reflows the known area around a late closet when a golden representation exists", () => {
  const result = plan({
    centerId: "a",
    residents: [resident("a", 0, 0), resident("misplaced", 1, 0)],
    nodes: [node("a", 0, 0), node("closet", 1, 0)],
    edges: [edge("a", "misplaced", "North"), edge("a", "closet", "East")],
  });

  const a = result.positions.get("a");
  const closet = result.positions.get("closet");
  const misplaced = result.positions.get("misplaced");
  assert.ok(a && closet && misplaced);
  assert.deepEqual(closet, at(a.x + 1, a.y, a.level));
  assert.deepEqual(misplaced, at(a.x, a.y - 1, a.level));
});

test("can clear more than one blocking existing room for one local chart", () => {
  const result = plan({
    centerId: "a",
    residents: [
      resident("a", 0, 0),
      resident("east-blocker", 1, 0),
      resident("south-blocker", 0, 1),
    ],
    nodes: [node("a", 0, 0), node("b", 1, 0), node("c", 0, 1)],
    edges: [edge("a", "b", "East"), edge("a", "c", "South")],
  });

  assert.deepEqual(result.positions.get("b"), at(1, 0));
  assert.deepEqual(result.positions.get("c"), at(0, 1));
  assert.equal(result.movedExisting.has("east-blocker"), true);
  assert.equal(result.movedExisting.has("south-blocker"), true);
});

test("reflows existing rooms when a substantially better cardinal layout is known", () => {
  const result = plan({
    centerId: "a",
    residents: [resident("a", 0, 0), resident("b", 5, 0)],
    nodes: [node("a", 0, 0), node("b", 1, 0)],
    edges: [edge("a", "b", "East")],
  });

  assert.deepEqual(result.positions.get("a"), at(0, 0));
  assert.deepEqual(result.positions.get("b"), at(1, 0));
  assert.equal(result.movedExisting.has("b"), true);
});

test("straightens a late cardinal link that was previously drawn at an angle", () => {
  const request = {
    centerId: "a",
    residents: [resident("a", 0, 0), resident("b", 4, 2)],
    nodes: [node("a", 0, 0), node("b", 1, 0)],
    edges: [edge("a", "b", "East")],
  };
  const run = () => {
    const trace: LayoutTraceEvent[] = [];
    const result = plan({ ...request, trace: (event) => trace.push(event) });
    return { result, trace };
  };
  const first = run();
  const second = run();
  assert.deepEqual(second, first);

  const result = first.result;
  const a = result.positions.get("a");
  const b = result.positions.get("b");
  assert.ok(a && b);
  assert.equal(b.x - a.x, 1);
  assert.equal(b.y, a.y);
  assert.deepEqual(
    first.trace
      .filter((event) => event.type === "improvement" && event.stage === "greedy-cardinal-repair")
      .map((event) => event.iteration),
    [0],
  );
});

test("aligns an angled bridge by repeatedly pushing only its local cardinal branch", () => {
  const result = planIntegralLayout({
    centerId: "anchor",
    allowExistingMoves: true,
    residents: [
      resident("anchor", 3, 1, false),
      resident("junction", 3, 0),
      resident("north", 3, -1),
      resident("east", 4, 0),
      resident("west", 2, 0),
      resident("far", 6, -2, false),
    ],
    nodes: [],
    edges: [
      edge("anchor", "junction", "North"),
      edge("junction", "anchor", "South"),
      edge("junction", "north", "North"),
      edge("north", "junction", "South"),
      edge("junction", "east", "East"),
      edge("east", "junction", "West"),
      edge("junction", "west", "West"),
      edge("west", "junction", "East"),
      edge("east", "far", "East"),
      edge("far", "east", "West"),
    ],
  });

  assert.deepEqual(result.positions.get("anchor"), at(3, 1));
  assert.deepEqual(result.positions.get("far"), at(6, -2));
  assert.deepEqual(result.positions.get("junction"), at(3, -2));
  assert.deepEqual(result.positions.get("north"), at(3, -3));
  assert.deepEqual(result.positions.get("east"), at(4, -2));
  assert.deepEqual(result.positions.get("west"), at(2, -2));
  assert.equal(result.quality.cardinalRayViolations, 0);
});

test("keeps impossible non-Euclidean constraints collision-free and integral", () => {
  const result = plan({
    centerId: "a",
    residents: [
      resident("a", 0, 0),
      resident("b", 0, -1),
      resident("c", 1, -1),
      resident("d", 2, -1),
    ],
    nodes: [node("a", 0, 0), node("b", 0, -1), node("c", 1, -1), node("d", 2, -1)],
    edges: [
      edge("a", "b", "North"),
      edge("b", "c", "East"),
      edge("c", "d", "East"),
      edge("d", "a", "South"),
      edge("a", "d", "West"),
    ],
  });

  const cells = new Set<string>();
  for (const position of result.positions.values()) {
    assert.equal(Number.isInteger(position.x), true);
    assert.equal(Number.isInteger(position.y), true);
    assert.equal(Number.isInteger(position.level), true);
    cells.add(`${position.level}:${position.x}:${position.y}`);
  }
  assert.equal(cells.size, result.positions.size);
});

test("greedy fallback leaves only the unavoidable stretched edge in an inconsistent loop", () => {
  const result = plan({
    centerId: "a",
    residents: [
      resident("a", 0, 0),
      resident("b", 0, -1),
      resident("c", 1, -1),
      resident("d", 2, -1),
      resident("e", 2, 0),
    ],
    nodes: [
      node("a", 0, 0),
      node("b", 0, -1),
      node("c", 1, -1),
      node("d", 2, -1),
      node("e", 2, 0),
    ],
    edges: [
      edge("a", "b", "North"),
      edge("b", "c", "East"),
      edge("c", "d", "East"),
      edge("d", "e", "South"),
      edge("e", "a", "West"),
    ],
  });

  const positions = result.positions;
  const exact = [
    ["a", "b", 0, -1],
    ["b", "c", 1, 0],
    ["c", "d", 1, 0],
    ["d", "e", 0, 1],
    ["e", "a", -1, 0],
  ].filter(([from, to, dx, dy]) => {
    const a = positions.get(from as string);
    const b = positions.get(to as string);
    return a && b && b.x - a.x === dx && b.y - a.y === dy;
  }).length;
  assert.equal(exact, 4);
});

test("prefers every cardinal exit on its proper ray over many exact shortcuts", () => {
  const result = plan({
    centerId: "a",
    residents: [resident("a", 0, 0), resident("b", 2, 0), resident("c", 1, 0)],
    nodes: [node("a", 0, 0), node("b", 1, 0), node("c", 2, 0)],
    edges: [
      edge("a", "b", "East"),
      edge("b", "c", "East"),
      ...Array.from({ length: 32 }, () => edge("a", "c", "East")),
    ],
  });

  const a = result.positions.get("a");
  const b = result.positions.get("b");
  const c = result.positions.get("c");
  assert.ok(a && b && c);
  assert.equal(a.y, b.y);
  assert.equal(b.y, c.y);
  assert.equal(a.x < b.x, true);
  assert.equal(b.x < c.x, true);
  assert.equal(result.quality.cardinalRayViolations, 0);
});

test("counts a strict cardinal link crossing with the sweep-line scorer", () => {
  const result = planIntegralLayout({
    allowExistingMoves: false,
    residents: [
      resident("west", -2, 0),
      resident("east", 2, 0),
      resident("north", 0, -2),
      resident("south", 0, 2),
    ],
    nodes: [],
    edges: [
      edge("west", "east", "East"),
      edge("east", "west", "West"),
      edge("north", "south", "South"),
      edge("south", "north", "North"),
    ],
  });

  assert.equal(result.quality.linkCrossings, 1);
});

test("ignores a zero-length physical link in the crossing sweep", () => {
  const quality = measureIntegralLayoutQuality(
    new Map([
      ["collapsed-a", at(0, 0)],
      ["collapsed-b", at(0, 0)],
      ["north", at(0, -2)],
      ["south", at(0, 2)],
    ]),
    [
      edge("collapsed-a", "collapsed-b", "Other"),
      edge("north", "south", "South"),
    ],
  );

  assert.equal(quality.linkCrossings, 0);
});

test("keeps greedy-cardinal scores exact and public-quality monotonic", () => {
  const residents = [
    resident("0", -1, 1, false),
    resident("1", 1, 3, true, 1),
    resident("2", 0, 4),
    resident("3", 2, -1, false),
    resident("4", -1, 0, true, 1),
    resident("5", 2, -3),
    resident("6", -1, 3),
  ];
  const edges = [
    edge("2", "6", "North"),
    edge("6", "2", "South"),
    edge("3", "5", "West"),
    edge("5", "3", "East"),
    edge("2", "0", "North"),
    edge("0", "2", "South"),
    edge("3", "6", "West"),
    edge("6", "3", "East"),
    edge("1", "6", "North"),
    edge("6", "1", "South"),
    edge("2", "3", "South"),
    edge("3", "2", "North"),
    edge("4", "5", "East"),
    edge("1", "2", "South"),
    edge("2", "1", "North"),
  ];
  const run = () => {
    const trace: LayoutTraceEvent[] = [];
    const result = planIntegralLayout({
      residents,
      nodes: [],
      edges,
      centerId: "0",
      allowExistingMoves: true,
      trace: (event) => trace.push(event),
    });
    return { result, trace };
  };

  const first = run();
  const second = run();
  assert.deepEqual(second.result, first.result);
  const improvements = first.trace.filter((event) =>
    event.type === "improvement" && event.stage === "greedy-cardinal-repair"
  );
  assert.deepEqual(
    second.trace.filter((event) =>
      event.type === "improvement" && event.stage === "greedy-cardinal-repair"
    ),
    improvements,
  );
  assert.ok(improvements.length > 0);
  for (const improvement of improvements) {
    assert.ok(improvement.after.positions);
    const positions = new Map(improvement.after.positions.map(({ id, ...position }) => [id, position]));
    assert.deepEqual(improvement.after.quality, measureIntegralLayoutQuality(positions, edges));
    assert.ok(improvement.after.quality.linkCrossings >= 0);
    const publicComparison = compareLayoutQuality(
      improvement.after.quality,
      improvement.before.quality,
    );
    assert.ok(publicComparison > 0 ||
      (publicComparison === 0 &&
        improvement.after.movedExisting.length < improvement.before.movedExisting.length));
  }
  assert.deepEqual(
    first.result.quality,
    measureIntegralLayoutQuality(first.result.positions, edges),
  );
});

test("retains exact crossing detection for diagonal links", () => {
  const result = planIntegralLayout({
    allowExistingMoves: false,
    residents: [
      resident("northwest", -2, -2),
      resident("southeast", 2, 2),
      resident("northeast", 2, -2),
      resident("southwest", -2, 2),
    ],
    nodes: [],
    edges: [
      edge("northwest", "southeast", "Other"),
      edge("northeast", "southwest", "Other"),
    ],
  });

  assert.equal(result.quality.linkCrossings, 1);
});

test("routing quality counts only same-level rooms intersected by a diagonal link", () => {
  const positions = new Map([
    ["from", at(-2, -2)],
    ["to", at(2, 2)],
    ["blocker", at(0, 0)],
    ["miss", at(0, 1)],
    ["other-level", at(0, 0, 1)],
  ]);

  assert.deepEqual(measureLayoutRoutingQuality(positions, [edge("from", "to", "Other")]), {
    routingViolations: 1,
    exitPortViolations: 0,
    reciprocalExitPortViolations: 0,
    roomObstructions: 1,
  });
});

test("exit-port quality ignores endpoint occupants and detects scalar and collided blockers", () => {
  const east = [edge("from", "to", "East")];
  const clear = new Map([
    ["from", at(0, 0)],
    ["to", at(1, 0)],
  ]);
  const scalarBlocker = new Map([
    ["from", at(0, 0)],
    ["to", at(2, 0)],
    ["blocker", at(1, 0)],
  ]);
  const collidedBlocker = new Map([
    ["from", at(0, 0)],
    ["to", at(1, 0)],
    ["blocker", at(1, 0)],
  ]);

  assert.equal(measureLayoutRoutingQuality(clear, east).exitPortViolations, 0);
  assert.equal(measureLayoutRoutingQuality(scalarBlocker, east).exitPortViolations, 1);
  assert.equal(measureLayoutRoutingQuality(collidedBlocker, east).exitPortViolations, 1);
});

test("vacuums an empty gap without sacrificing cardinal rays", () => {
  const trace: LayoutTraceEvent[] = [];
  const result = plan({
    centerId: "a",
    residents: [
      resident("a", 0, 0),
      resident("near", 1, 0, false),
      resident("far", 5, 0),
    ],
    nodes: [node("a", 0, 0)],
    edges: [
      edge("a", "near", "East"),
      // Prevent golden re-embedding while leaving `far` outside the link graph,
      // so only the global empty-column vacuum can compact it.
      edge("a", "a", "North"),
    ],
    trace: (event) => trace.push(event),
  });

  assert.deepEqual(result.positions.get("a"), at(0, 0));
  assert.deepEqual(result.positions.get("near"), at(1, 0));
  assert.deepEqual(result.positions.get("far"), at(2, 0));
  assert.equal(result.quality.cardinalSlack, 0);
  assert.equal(result.quality.footprintArea, 3);
  assert.equal(trace.some((event) =>
    event.type === "candidate-batch" && event.stage === "all-candidates"
  ), true);
  assert.equal(trace.some((event) =>
    event.type === "vacuum" && event.axis === "x" && event.distance === -3 && event.moved.includes("far")
  ), true);
  assert.deepEqual(trace.at(-1), {
    type: "selection",
    stage: "final-selection",
    selected: {
      quality: result.quality,
      movedExisting: ["far"],
      positions: [
        { id: "a", ...at(0, 0) },
        { id: "far", ...at(2, 0) },
        { id: "near", ...at(1, 0) },
      ],
    },
  });
});

test("a vacuum footprint gain never buys a new routing violation into the plan", () => {
  // Closing the whole empty-column gap would drop `blocker` onto a's East
  // exit port; the single occupied row leaves no perpendicular escape. The
  // published plan may compact only as far as the port allows, and its public
  // quality must never fall below the seed's.
  const request: IntegralLayoutRequest = {
    centerId: "a",
    allowExistingMoves: true,
    nodes: [],
    residents: [
      resident("a", 0, 0, false),
      resident("b", -2, 0, false),
      resident("blocker", 4, 0),
    ],
    edges: [edge("a", "b", "East")],
  };
  const positions = new Map(request.residents.map((room) => [room.id, room.position]));
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
  };
  assert.equal(seed.quality.routingViolations, 0);

  const compacted = compactIntegralLayoutPlan(request, seed);
  const planned = planIntegralLayout(request);
  for (const result of [compacted, planned]) {
    assert.deepEqual(result.positions.get("blocker"), at(2, 0));
    assert.equal(result.quality.routingViolations, 0);
    assert.equal(result.quality.exitPortViolations, 0);
    assert.ok(compareLayoutQuality(result.quality, seed.quality) > 0);
  }
});

test("candidate admission keeps a valid vacuum alternative when the private best is invalid", () => {
  const trace: LayoutTraceEvent[] = [];
  const acceptsPositions = (positions: ReadonlyMap<string, GridPosition>): boolean =>
    positions.get("locked-a")?.x === 0 && positions.get("locked-b")?.x === 1 &&
    (positions.get("b")?.x ?? 0) - (positions.get("a")?.x ?? 0) >= 5;
  const result = planIntegralLayout({
    residents: [
      resident("locked-a", 0, 0, false),
      resident("locked-b", 1, 0, false),
      resident("a", 5, 0),
      resident("b", 10, 0),
    ],
    nodes: [],
    edges: [],
    allowExistingMoves: true,
    trace: (event) => trace.push(event),
  }, { acceptsPositions });

  assert.deepEqual(result.positions.get("a"), at(2, 0));
  assert.deepEqual(result.positions.get("b"), at(7, 0));
  assert.equal(acceptsPositions(result.positions), true);
  const vacuums = trace.filter((event) => event.type === "vacuum");
  assert.equal(vacuums.length, 1);
  for (const event of vacuums) {
    const positions = new Map(event.after.positions?.map(({ id, x, y, level }) => [
      id,
      { x, y, level },
    ]));
    assert.equal(acceptsPositions(positions), true, "no rejected vacuum candidate is traced");
  }
});

test("repeatedly slides bridge-connected lobes without requiring empty columns", () => {
  const trace: LayoutTraceEvent[] = [];
  const result = plan({
    centerId: "anchor",
    residents: [
      resident("anchor", 3, 0, false),
      resident("locked-stop", 2, 1, false),
      resident("lobe-end", -3, 0),
      resident("lobe-tail", -3, 1),
      resident("filler-a", -2, 2, false),
      resident("filler-b", -1, 2, false),
      resident("filler-c", 0, 2, false),
      resident("filler-d", 1, 2, false),
      resident("second-anchor", 3, 4, false),
      resident("second-stop", 2, 5, false),
      resident("second-end", 0, 4),
      resident("second-tail", 0, 5),
    ],
    nodes: [node("anchor", 0, 0)],
    edges: [
      edge("lobe-end", "anchor", "East"),
      edge("lobe-end", "lobe-tail", "South"),
      edge("second-end", "second-anchor", "East"),
      edge("second-end", "second-tail", "South"),
    ],
    trace: (event) => trace.push(event),
  });

  assert.deepEqual(result.positions.get("anchor"), at(3, 0));
  assert.deepEqual(result.positions.get("lobe-end"), at(1, 0));
  assert.deepEqual(result.positions.get("lobe-tail"), at(1, 1));
  assert.deepEqual(result.positions.get("second-end"), at(1, 4));
  assert.deepEqual(result.positions.get("second-tail"), at(1, 5));
  assert.equal(result.quality.cardinalRayViolations, 0);
  assert.equal(result.quality.roomObstructions, 0);
  assert.equal(result.quality.linkCrossings, 0);
  assert.equal(trace.some((event) =>
    event.type === "bridge-vacuum" &&
    event.movingEndpoint === "lobe-end" &&
    event.offset.x === 4 &&
    event.moved.includes("lobe-tail")
  ), true);
  assert.equal(trace.some((event) =>
    event.type === "bridge-vacuum" &&
    event.movingEndpoint === "second-end" &&
    event.offset.x === 1 &&
    event.moved.includes("second-tail")
  ), true);
});

test("packs a multiply attached corridor row around occupied global gaps", () => {
  const residents = [
    resident("anchor", 0, 0, false),
    resident("second-anchor", 6, 0, false),
    resident("corridor-a", 0, 4),
    resident("corridor-b", 3, 4),
    resident("corridor-c", 6, 4),
    // These make every intervening row globally occupied, so whole-row
    // vacuuming cannot close the corridor's gap.
    resident("blocker-1", 10, 1, false),
    resident("blocker-2", 10, 2, false),
    resident("blocker-3", 10, 3, false),
  ];
  const edges = [
    edge("corridor-a", "corridor-b", "East"),
    edge("corridor-b", "corridor-a", "West"),
    edge("corridor-b", "corridor-c", "East"),
    edge("corridor-c", "corridor-b", "West"),
    edge("anchor", "corridor-a", "South"),
    edge("second-anchor", "corridor-c", "South"),
  ];
  const before = measureIntegralLayoutQuality(
    new Map(residents.map((room) => [room.id, room.position])),
    edges,
  );
  const trace: LayoutTraceEvent[] = [];
  const result = planIntegralLayout({
    centerId: "anchor",
    allowExistingMoves: true,
    residents,
    nodes: [],
    edges,
    trace: (event) => trace.push(event),
  });

  assert.deepEqual(result.positions.get("corridor-a"), at(0, 1));
  assert.deepEqual(result.positions.get("corridor-b"), at(3, 1));
  assert.deepEqual(result.positions.get("corridor-c"), at(6, 1));
  assert.equal(result.quality.cardinalRayViolations, 0);
  assert.equal(result.quality.routingViolations, 0);
  assert.equal(result.quality.linkCrossings, 0);
  assert.ok(result.quality.footprintArea < before.footprintArea);
  assert.ok(result.quality.cardinalSlack < before.cardinalSlack);
  assert.ok(trace.some((event) =>
    event.type === "axis-progress" && event.stage === "axis-compaction" &&
    event.candidatesConsidered > 0
  ));
});

function recursiveGravityChain(axis: "x" | "y", lockedChainRoom?: string): IntegralLayoutRequest {
  const position = (along: number, across: number): GridPosition => axis === "x"
    ? at(along, across)
    : at(across, along);
  const forward: LayoutEdge["direction"] = axis === "x" ? "East" : "South";
  const backward: LayoutEdge["direction"] = axis === "x" ? "West" : "North";
  return {
    centerId: "wall",
    allowExistingMoves: true,
    residents: [
      { id: "a", position: position(0, 0), movable: lockedChainRoom !== "a" },
      { id: "b", position: position(1, 0), movable: lockedChainRoom !== "b" },
      { id: "c", position: position(2, 0), movable: lockedChainRoom !== "c" },
      { id: "wall", position: position(4, 0), movable: false },
      // Occupy the otherwise empty global row/column so vacuumLayout cannot
      // solve the local gap before recursive axis gravity sees it.
      { id: "gap-mask", position: position(3, 1), movable: false },
    ],
    nodes: [],
    edges: [
      edge("a", "b", forward),
      edge("b", "a", backward),
      edge("b", "c", forward),
      edge("c", "b", backward),
      edge("c", "wall", forward),
      edge("wall", "c", backward),
      // Close the physical-link cycle so bridgeLobeVacuum cannot move a cut
      // lobe and accidentally mask the axis-gravity regression.
      edge("a", "wall", "Other"),
    ],
  };
}

function compactRequestSeed(request: IntegralLayoutRequest) {
  const positions = new Map(request.residents.map((room) => [room.id, room.position]));
  return compactIntegralLayoutPlan(request, {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
  });
}

test("recursively packs a horizontal cardinal chain through a neutral plateau", () => {
  const request = recursiveGravityChain("x");
  const before = measureIntegralLayoutQuality(
    new Map(request.residents.map((room) => [room.id, room.position])),
    request.edges,
  );
  const first = compactRequestSeed(request);
  const repeated = compactRequestSeed(recursiveGravityChain("x"));

  assert.deepEqual(first.positions.get("a"), at(1, 0));
  assert.deepEqual(first.positions.get("b"), at(2, 0));
  assert.deepEqual(first.positions.get("c"), at(3, 0));
  assert.equal(compareLayoutQuality(first.quality, before) > 0, true);
  assert.equal(first.quality.footprintArea, 8);
  assert.equal(first.quality.cardinalSlack, 0);
  assert.deepEqual([...repeated.positions].sort(), [...first.positions].sort());
});

test("recursively packs the vertical transpose of a cardinal chain", () => {
  const result = compactRequestSeed(recursiveGravityChain("y"));

  assert.deepEqual(result.positions.get("a"), at(0, 1));
  assert.deepEqual(result.positions.get("b"), at(0, 2));
  assert.deepEqual(result.positions.get("c"), at(0, 3));
  assert.equal(result.quality.footprintArea, 8);
  assert.equal(result.quality.cardinalSlack, 0);
});

test("recursive gravity respects fixed rooms, the center, and candidate admission", () => {
  const original = new Map(recursiveGravityChain("x").residents.map((room) => [
    room.id,
    room.position,
  ]));
  const fixed = compactRequestSeed(recursiveGravityChain("x", "c"));
  const centeredRequest = recursiveGravityChain("x");
  centeredRequest.centerId = "b";
  const centered = compactRequestSeed(centeredRequest);
  const admittedRequest = recursiveGravityChain("x");
  const admittedPositions = new Map(admittedRequest.residents.map((room) => [
    room.id,
    room.position,
  ]));
  const admitted = compactIntegralLayoutPlan(admittedRequest, {
    positions: admittedPositions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(admittedPositions, admittedRequest.edges),
  }, {
    acceptsPositions: (positions) => positions.get("a")?.x === 0,
  });

  assert.deepEqual(fixed.positions, original);
  assert.deepEqual(centered.positions, original);
  assert.deepEqual(admitted.positions, original);
});

function unevenSeriesRequest(
  axis: "x" | "y",
  coordinates: readonly number[],
): IntegralLayoutRequest {
  const position = (along: number, across: number): GridPosition => axis === "x"
    ? at(along, across)
    : at(across, along);
  const forward: LayoutEdge["direction"] = axis === "x" ? "East" : "South";
  const backward: LayoutEdge["direction"] = axis === "x" ? "West" : "North";
  const ids = coordinates.map((_, index) => `series-${index}`);
  const occupied = new Set<number>(coordinates);
  const minimum = Math.min(...coordinates);
  const maximum = Math.max(...coordinates);
  const masks: LayoutResident[] = [];
  for (let coordinate = minimum; coordinate <= maximum; coordinate += 1) {
    if (!occupied.has(coordinate)) {
      // These fixed off-axis rooms occupy every global row/column, proving the
      // result comes from series spacing rather than the first vacuum pass.
      masks.push({ id: `mask-${coordinate}`, position: position(coordinate, 2), movable: false });
    }
  }
  const edges: LayoutEdge[] = [];
  for (let index = 1; index < ids.length; index += 1) {
    edges.push(edge(ids[index - 1], ids[index], forward));
    edges.push(edge(ids[index], ids[index - 1], backward));
  }
  return {
    centerId: ids[0],
    allowExistingMoves: true,
    residents: [
      ...ids.map((id, index) => ({
        id,
        position: position(coordinates[index], 0),
        movable: index > 0 && index + 1 < ids.length,
      })),
      ...masks,
    ],
    nodes: [],
    edges,
  };
}

test("evenly spaces a horizontal reciprocal series without changing public quality", () => {
  const request = unevenSeriesRequest("x", [0, 1, 5, 9]);
  const before = new Map(request.residents.map((room) => [room.id, room.position]));
  const seed = {
    positions: before,
    movedExisting: new Set<string>(),
    quality: measureIntegralLayoutQuality(before, request.edges),
  };
  const result = compactIntegralLayoutPlan(request, seed);

  assert.deepEqual(result.positions.get("series-0"), at(0, 0));
  assert.deepEqual(result.positions.get("series-1"), at(3, 0));
  assert.deepEqual(result.positions.get("series-2"), at(6, 0));
  assert.deepEqual(result.positions.get("series-3"), at(9, 0));
  assert.deepEqual(result.quality, seed.quality);
  assert.deepEqual([...result.movedExisting].sort(), ["series-1", "series-2"]);
  assert.equal(compactIntegralLayoutPlan(request, result), result);
});

test("a compacted plan preserves its originating constraint report", () => {
  const request = unevenSeriesRequest("x", [0, 1, 5, 9]);
  const positions = new Map(request.residents.map((room) => [room.id, room.position]));
  const report = { source: "synthetic" } as unknown as NonNullable<
    IntegralLayoutPlan["constraintRepair"]
  >;
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
    constraintRepair: report,
  };

  const result = compactIntegralLayoutPlan(request, seed);
  assert.notEqual(result, seed);
  assert.equal(result.constraintRepair, report);
});

test("even spacing handles the vertical transpose and a nondivisible span deterministically", () => {
  const vertical = compactRequestSeed(unevenSeriesRequest("y", [0, 1, 5, 9]));
  assert.deepEqual(vertical.positions.get("series-1"), at(0, 3));
  assert.deepEqual(vertical.positions.get("series-2"), at(0, 6));

  const request = unevenSeriesRequest("x", [0, 1, 5, 10]);
  const shuffled: IntegralLayoutRequest = {
    ...request,
    residents: [...request.residents].reverse(),
    edges: [...request.edges].reverse(),
  };
  const first = compactRequestSeed(request);
  const second = compactRequestSeed(shuffled);
  assert.deepEqual(first.positions.get("series-1"), at(3, 0));
  assert.deepEqual(first.positions.get("series-2"), at(7, 0));
  assert.deepEqual([...second.positions].sort(), [...first.positions].sort());
});

test("ordinary settled reflow keeps an equal-quality series-spacing improvement", () => {
  const request = unevenSeriesRequest("x", [0, 1, 5, 9]);
  const trace: LayoutTraceEvent[] = [];
  const result = planIntegralLayout({ ...request, trace: (event) => trace.push(event) });

  assert.deepEqual(result.positions.get("series-1"), at(3, 0));
  assert.deepEqual(result.positions.get("series-2"), at(6, 0));
  const final = trace.findLast((event) => event.type === "selection" &&
    event.stage === "final-selection");
  assert.equal(final?.type, "selection");
  assert.deepEqual(final?.selected.positions?.find(({ id }) => id === "series-1"), {
    id: "series-1",
    ...at(3, 0),
  });
});

test("a late public fallback receives the complete compaction fixed point", () => {
  const request: IntegralLayoutRequest = {
    centerId: "r0",
    allowExistingMoves: true,
    nodes: [],
    residents: [
      resident("r0", -4, 0),
      resident("r1", -2, 3),
      resident("r2", 1, -3),
      resident("r3", 1, 0, false),
      resident("r4", 2, 3),
    ],
    edges: [
      edge("r0", "r2", "East"), edge("r2", "r0", "West"),
      edge("r0", "r4", "East"),
      edge("r1", "r2", "North"), edge("r2", "r1", "South"),
      edge("r1", "r3", "North"), edge("r3", "r1", "South"),
    ],
  };

  const result = planIntegralLayout(request);

  assert.deepEqual(result.positions.get("r2"), at(1, -2));
  assert.deepEqual(result.positions.get("r4"), at(-2, 0));
  assert.equal(result.quality.routingViolations, 2);
  assert.equal(compactIntegralLayoutPlan(request, result), result);
});

test("settled reflow closes every axis-compaction basin before publication", () => {
  const request: IntegralLayoutRequest = {
    centerId: "r0",
    allowExistingMoves: true,
    nodes: [],
    residents: [
      resident("r0", -1, 9, false),
      resident("r1", -4, -8),
      resident("r2", 2, -2, false),
      resident("r4", -1, -5),
      resident("r5", 1, -8),
      resident("r6", 8, -8),
      resident("r7", 6, -8),
    ],
    edges: [
      edge("r0", "r1", "North"),
      edge("r1", "r2", "West"),
      edge("r1", "r5", "North"),
      edge("r4", "r6", "North"),
      edge("r1", "r7", "East"),
    ],
  };

  const result = planIntegralLayout(request);

  // The first axis basin has footprint 56 and cardinal slack 12. A neutral
  // vacuum used to stop the closure there even though another axis pass could
  // reach this visibly tighter basin.
  assert.equal(result.quality.footprintArea, 48);
  assert.equal(result.quality.footprintPerimeter, 32);
  assert.equal(result.quality.cardinalSlack, 1);
  assert.equal(compactIntegralLayoutPlan(request, result), result);
});

test("quick crossing selections receive a final compaction closure", () => {
  const trace: LayoutTraceEvent[] = [];
  const request: IntegralLayoutRequest = {
    centerId: "r0",
    allowExistingMoves: true,
    nodes: [],
    residents: [
      resident("r0", -6, 7, false),
      resident("r1", -4, 5, false),
      resident("r2", -10, -6),
      resident("r3", -6, 6),
      resident("r4", 5, 6),
      resident("r5", -3, 6),
      resident("r6", 6, 1),
      resident("r7", 9, 0),
    ],
    edges: [
      edge("r0", "r1", "Other"),
      edge("r0", "r2", "North"),
      edge("r2", "r3", "East"), edge("r3", "r2", "West"),
      edge("r0", "r4", "North"), edge("r4", "r0", "South"),
      edge("r4", "r5", "North"), edge("r5", "r4", "South"),
      edge("r4", "r6", "North"), edge("r6", "r4", "South"),
      edge("r4", "r7", "East"), edge("r7", "r4", "West"),
    ],
    trace: (event) => trace.push(event),
  };

  const result = planIntegralLayout(request);

  assert.ok(trace.some((event) => event.type === "crossing-repair" && event.mode === "quick"));
  assert.deepEqual(result.positions.get("r7"), at(-4, 6));
  assert.equal(result.quality.routingViolations, 4);
  assert.equal(compactIntegralLayoutPlan(request, result), result);
});

test("an in-flight new-room request stays on the topology-first path", () => {
  const request = unevenSeriesRequest("x", [0, 1, 5, 9]);
  const trace: LayoutTraceEvent[] = [];
  planIntegralLayout({
    ...request,
    nodes: [node("series-0", 0, 0), node("new-room", 0, -1)],
    edges: [...request.edges, edge("series-0", "new-room", "North")],
    trace: (event) => trace.push(event),
  });

  assert.equal(trace.some((event) => event.type === "axis-progress"), false);
});

test("series spacing extracts an unbranched arm from a cardinal junction", () => {
  const residents = [
    resident("left", -9, 0, false),
    resident("middle", -1, 0),
    resident("junction", 0, 0, false),
    resident("east-near", 1, 0, false),
    resident("east-far", 2, 0, false),
  ];
  const edges = [
    edge("left", "middle", "East"), edge("middle", "left", "West"),
    edge("middle", "junction", "East"), edge("junction", "middle", "West"),
    edge("junction", "east-near", "East"), edge("east-near", "junction", "West"),
    edge("junction", "east-far", "East"), edge("east-far", "junction", "West"),
  ];
  const request: IntegralLayoutRequest = {
    centerId: "junction",
    allowExistingMoves: true,
    residents,
    nodes: [],
    edges,
  };
  const result = compactRequestSeed(request);

  assert.deepEqual(result.positions.get("middle"), at(-4, 0));
  assert.equal(result.quality.linkCrossings, 0);
});

test("even spacing cannot trade one reciprocal series against another", () => {
  const coordinates = [0, 2, 4, 6, 9, 10, 19] as const;
  const canonical = [0, 3, 6, 10, 13, 16, 19] as const;
  const ids = coordinates.map((_, index) => String.fromCharCode("a".charCodeAt(0) + index));
  const occupied = new Set<number>(coordinates);
  const residents: LayoutResident[] = [
    ...ids.map((id, index) => resident(
      id,
      coordinates[index],
      0,
      index > 0 && index + 1 < ids.length,
    )),
  ];
  for (let x = 0; x <= 19; x += 1) {
    if (!occupied.has(x)) residents.push(resident(`mask-${x}`, x, 2, false));
  }
  const edges = [
    edge("a", "b", "East"), edge("b", "a", "West"),
    edge("b", "c", "East"), edge("c", "b", "West"),
    edge("c", "d", "East"), edge("d", "c", "West"),
    edge("d", "e", "East"),
    edge("e", "f", "East"), edge("f", "e", "West"),
    edge("f", "g", "East"), edge("g", "f", "West"),
  ];
  const request: IntegralLayoutRequest = {
    centerId: "a",
    allowExistingMoves: true,
    residents,
    nodes: [],
    edges,
  };
  const positions = new Map(residents.map((room) => [room.id, room.position]));
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  const permitted = (candidate: ReadonlyMap<string, GridPosition>): boolean => {
    const values = ids.map((id) => candidate.get(id)?.x);
    return values.every((value, index) => value === coordinates[index]) ||
      values.every((value, index) => value === canonical[index]);
  };

  const result = compactIntegralLayoutPlan(request, seed, { acceptsPositions: permitted });
  assert.equal(result, seed);
});

test("series spacing respects its center, fixed groups, admission, and entry cancellation", () => {
  const base = unevenSeriesRequest("x", [0, 1, 5, 9]);
  const positions = new Map(base.residents.map((room) => [room.id, room.position]));
  const seed = {
    positions,
    movedExisting: new Set<string>(),
    quality: measureIntegralLayoutQuality(positions, base.edges),
  };
  const centered: IntegralLayoutRequest = { ...base, centerId: "series-1" };
  const fixed: IntegralLayoutRequest = {
    ...base,
    residents: base.residents.map((room) => room.id === "series-1"
      ? { ...room, movable: false }
      : room),
  };
  const rejected = compactIntegralLayoutPlan(base, seed, {
    acceptsPositions: (candidate) => candidate.get("series-1")?.x === 1,
  });

  assert.deepEqual(compactIntegralLayoutPlan(centered, seed).positions, positions);
  assert.deepEqual(compactIntegralLayoutPlan(fixed, seed).positions, positions);
  assert.deepEqual(rejected.positions, positions);
  assert.equal(compactIntegralLayoutPlan(base, seed, { shouldCancel: () => true }), seed);
});

test("cancelling during compaction returns the exact seed transaction", () => {
  const request = recursiveGravityChain("x");
  const positions = new Map(request.residents.map((room) => [room.id, room.position]));
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
  };
  let checks = 0;

  const result = compactIntegralLayoutPlan(request, seed, {
    shouldCancel: () => ++checks >= 10,
  });

  assert.ok(checks >= 10);
  assert.equal(result, seed);
});

test("recursive gravity retains the compact basin from the legacy axis sweep", () => {
  const result = planIntegralLayout({
    allowExistingMoves: true,
    residents: [
      resident("r0", -2, 0),
      resident("r1", 3, -2),
      resident("r2", -1, 2, false),
    ],
    nodes: [],
    edges: [
      edge("r0", "r2", "West"),
      edge("r2", "r0", "North"),
    ],
  });

  assert.deepEqual(result.positions.get("r0"), at(0, 2));
  assert.deepEqual(result.positions.get("r1"), at(1, 2));
  assert.deepEqual(result.positions.get("r2"), at(-1, 2));
  assert.equal(result.quality.footprintArea, 3);
  assert.equal(result.quality.footprintPerimeter, 8);
});

test("pulls a disconnected boundary room into unused interior space", () => {
  const residents = [
    resident("anchor", 1, 0, false),
    resident("northeast", 4, 0, false),
    resident("southwest", 1, 2, false),
    resident("southeast", 4, 2, false),
    resident("detached", 0, 1),
  ];
  const before = measureIntegralLayoutQuality(
    new Map(residents.map((room) => [room.id, room.position])),
    [],
  );
  const result = planIntegralLayout({
    centerId: "anchor",
    allowExistingMoves: true,
    residents,
    nodes: [],
    edges: [],
  });

  assert.equal(result.quality.cardinalRayViolations, 0);
  assert.equal(result.quality.routingViolations, 0);
  assert.equal(result.quality.linkCrossings, 0);
  assert.ok(result.quality.footprintArea < before.footprintArea);
  assert.ok(result.quality.footprintPerimeter < before.footprintPerimeter);
  assert.notDeepEqual(result.positions.get("detached"), at(0, 1));
});

test("reflows an occupied region to make space for a late cardinal room", () => {
  const upperResidents: LayoutResident[] = [];
  const upperEdges: LayoutEdge[] = [];
  for (let row = 0; row < 3; row += 1) {
    for (let column = 0; column < 3; column += 1) {
      const id = `upper-${column}-${row}`;
      upperResidents.push(resident(id, column, row - 2));
      if (column > 0) upperEdges.push(edge(`upper-${column - 1}-${row}`, id, "East"));
      if (row > 0) upperEdges.push(edge(`upper-${column}-${row - 1}`, id, "South"));
    }
  }

  const result = plan({
    centerId: "a",
    residents: [resident("a", 1, 1), ...upperResidents],
    nodes: [node("a", 0, 0), node("late", 0, -1)],
    edges: [
      ...upperEdges,
      edge("a", "late", "North"),
      // Prevent the unrelated area-wide golden solver from deciding the test.
      edge("a", "a", "North"),
    ],
  });

  const a = result.positions.get("a");
  const late = result.positions.get("late");
  assert.ok(a && late);
  assert.deepEqual(late, at(a.x, a.y - 1));
  for (let row = 0; row < 3; row += 1) {
    for (let column = 0; column < 3; column += 1) {
      const placed = result.positions.get(`upper-${column}-${row}`);
      assert.ok(placed);
      if (column > 0) {
        const west = result.positions.get(`upper-${column - 1}-${row}`);
        assert.ok(west);
        assert.deepEqual(placed, at(west.x + 1, west.y));
      }
      if (row > 0) {
        const north = result.positions.get(`upper-${column}-${row - 1}`);
        assert.ok(north);
        assert.deepEqual(placed, at(north.x, north.y + 1));
      }
    }
  }
});

test("preserves a cleaner established outer region over one conflicting exit", () => {
  const northernResidents: LayoutResident[] = [];
  const northernEdges: LayoutEdge[] = [];
  const trace: LayoutTraceEvent[] = [];
  for (let row = 0; row < 3; row += 1) {
    for (let column = 0; column < 3; column += 1) {
      const id = `north-${column}-${row}`;
      northernResidents.push(resident(id, column + 2, row - 3));
      if (column > 0) northernEdges.push(edge(`north-${column - 1}-${row}`, id, "East"));
      if (row > 0) northernEdges.push(edge(`north-${column}-${row - 1}`, id, "South"));
    }
  }

  const result = plan({
    centerId: "a",
    residents: [resident("a", 0, 0), resident("b", 1, 0), resident("c", 2, 0), ...northernResidents],
    nodes: [node("a", 0, 0), node("north-0-2", 0, -1)],
    edges: [
      edge("a", "b", "East"),
      edge("b", "c", "East"),
      edge("c", "north-0-2", "North"),
      ...northernEdges,
      // This late observation conflicts with the already exact path above.
      edge("a", "north-0-2", "North"),
    ],
    trace: (event) => trace.push(event),
  });

  assert.deepEqual(result.positions.get("a"), at(0, 0));
  assert.deepEqual(result.positions.get("north-0-2"), at(2, -1));
  for (let row = 0; row < 3; row += 1) {
    for (let column = 0; column < 3; column += 1) {
      assert.deepEqual(result.positions.get(`north-${column}-${row}`), at(column + 2, row - 3));
    }
  }
  assert.equal(result.quality.routingViolations, 0);
  const initial = trace.find((event) =>
    event.type === "selection" && event.stage === "initial-selection"
  );
  assert.ok(initial && initial.type === "selection");
  assert.ok(compareLayoutQuality(result.quality, initial.selected.quality) >= 0);
  for (const improvement of trace.filter((event) =>
    event.type === "improvement" && event.stage === "greedy-cardinal-repair"
  )) {
    const comparison = compareLayoutQuality(improvement.after.quality, improvement.before.quality);
    assert.ok(comparison > 0 ||
      (comparison === 0 &&
        improvement.after.movedExisting.length < improvement.before.movedExisting.length));
  }
});

test("places a new component at its established seam before an unanchored room", () => {
  const result = planIntegralLayout({
    centerId: "player",
    allowExistingMoves: false,
    residents: [resident("player", 0, 0), resident("anchor", 10, 0)],
    nodes: [
      node("player", 0, 0),
      node("anchor", 5, 0),
      // If placed first from the player chart, this room claims the seam cell.
      node("a-unanchored", 11, 0),
      node("z-seam", 6, 0),
    ],
    edges: [edge("anchor", "z-seam", "East")],
  });

  assert.deepEqual(result.positions.get("anchor"), at(10, 0));
  assert.deepEqual(result.positions.get("z-seam"), at(11, 0));
  assert.notDeepEqual(result.positions.get("a-unanchored"), at(11, 0));
});

test("an axis push carries a perpendicular band and keeps distant parallel geometry clean", () => {
  const result = plan({
    centerId: "a",
    residents: [
      resident("a", 0, 1),
      resident("blocker", 0, 0),
      resident("east", 1, 0),
      resident("far-north", 0, -3),
      resident("non-euclidean", 10, 10),
    ],
    nodes: [node("a", 0, 0), node("late", 0, -1)],
    edges: [
      edge("a", "late", "North"),
      edge("blocker", "east", "East"),
      edge("blocker", "far-north", "North"),
      edge("non-euclidean", "non-euclidean", "North"),
    ],
  });

  assert.deepEqual(result.positions.get("late"), at(0, 0));
  const blocker = result.positions.get("blocker");
  const east = result.positions.get("east");
  assert.ok(blocker && east);
  assert.equal(blocker.y < 0, true);
  assert.deepEqual(east, at(blocker.x + 1, blocker.y));
  const farNorth = result.positions.get("far-north");
  assert.ok(farNorth);
  assert.equal(farNorth.x, blocker.x);
  assert.equal(farNorth.y < blocker.y, true);
});

test("an axis push carries endpoints of a link crossed by its swept path", () => {
  const residentList = [
    resident("a", -1, 0),
    resident("blocker", 0, 0),
    resident("east-goal", 2, 0),
    resident("west-tail", -2, 0),
    resident("cross-a", 0, -1),
    resident("cross-b", 1, 1),
  ];
  const positions = new Map(residentList.map((room) => [room.id, room.position]));
  positions.set("late", at(0, 0));
  const repairs = safePushRepairs(
    positions,
    new Set(["a", "late"]),
    new Map(residentList.map((room) => [room.id, room])),
    [
      edge("a", "late", "East"),
      edge("blocker", "east-goal", "East"),
      edge("west-tail", "blocker", "Other"),
      edge("cross-a", "cross-b", "Other"),
    ],
  );
  const eastPush = repairs.find((repair) => repair.get("blocker")?.x === 1);
  assert.ok(eastPush);
  assert.deepEqual(eastPush.get("cross-a"), at(1, -1));
  assert.deepEqual(eastPush.get("cross-b"), at(2, 1));
});

test("a vertical axis push carries endpoints of a diagonal swept crossing", () => {
  const residentList = [
    resident("a", 0, 1),
    resident("blocker", 0, 0),
    resident("cross-a", -1, -1),
    resident("cross-b", 1, 0),
    resident("remote-a", -1, -1, true, 1),
    resident("remote-b", 1, 0, true, 1),
  ];
  const positions = new Map(residentList.map((room) => [room.id, room.position]));
  positions.set("late", at(0, 0));
  const repairs = safePushRepairs(
    positions,
    new Set(["a", "late"]),
    new Map(residentList.map((room) => [room.id, room])),
    [
      edge("cross-a", "cross-b", "Other"),
      edge("remote-a", "remote-b", "Other"),
    ],
  );
  const northPush = repairs.find((repair) => repair.get("blocker")?.y === -1);
  assert.ok(northPush);
  assert.deepEqual(northPush.get("cross-a"), at(-1, -2));
  assert.deepEqual(northPush.get("cross-b"), at(1, -1));
  assert.deepEqual(northPush.get("remote-a"), at(-1, -1, 1));
  assert.deepEqual(northPush.get("remote-b"), at(1, 0, 1));
});

test("an axis push recursively carries destination occupants and their perpendicular band", () => {
  const residentList = [
    resident("a", 0, 1),
    resident("blocker", 0, 0),
    resident("ahead", 0, -1),
    resident("side", 1, -1),
  ];
  const positions = new Map(residentList.map((room) => [room.id, room.position]));
  positions.set("late", at(0, 0));
  const repairs = safePushRepairs(
    positions,
    new Set(["a", "late"]),
    new Map(residentList.map((room) => [room.id, room])),
    [edge("ahead", "side", "East")],
  );
  const northPush = repairs.find((repair) => repair.get("blocker")?.y === -1);
  assert.ok(northPush);
  assert.deepEqual(northPush.get("ahead"), at(0, -2));
  assert.deepEqual(northPush.get("side"), at(1, -2));
});

test("measures enough push distance to carry a trailing branch across an obstructed link", () => {
  const trace: LayoutTraceEvent[] = [];
  const result = plan({
    centerId: "east",
    residents: [
      resident("west", -3, 0, false),
      resident("east", 3, 0, false),
      resident("blocker", -1, 0),
      resident("tail", -1, 1),
      resident("side", 0, 0),
      resident("side-tail", 0, 1),
    ],
    nodes: [node("east", 0, 0)],
    edges: [
      edge("west", "east", "East"),
      edge("blocker", "tail", "South"),
      edge("blocker", "side", "East"),
      edge("tail", "side-tail", "East"),
      edge("side", "side-tail", "South"),
    ],
    trace: (event) => trace.push(event),
  });

  assert.equal(result.quality.roomObstructions, 0);
  assert.equal(result.quality.linkCrossings, 0);
  assert.deepEqual(result.positions.get("blocker"), at(-1, -2));
  assert.deepEqual(result.positions.get("tail"), at(-1, -1));
  const accepted = trace.find((event) =>
    event.type === "obstruction-repair" && event.offset.y === -2
  );
  assert.ok(accepted && accepted.type === "obstruction-repair");
  assert.deepEqual(accepted.obstructing, ["blocker", "side"]);
});

test("streams a stable top-eight obstruction frontier without changing trace order", () => {
  const residents: LayoutResident[] = [];
  const edges: LayoutEdge[] = [];
  for (let row = 0; row < 10; row += 1) {
    residents.push(
      resident(`left-${row}`, 0, row * 2, false),
      resident(`blocker-${row}`, 2, row * 2),
      resident(`right-${row}`, 4, row * 2, false),
    );
    edges.push(edge(`left-${row}`, `right-${row}`, "Other"));
  }
  // Keep the disconnected-room golden packing from bypassing the repair pass.
  edges.push(edge("left-0", "left-0", "North"));
  const run = () => {
    const trace: LayoutTraceEvent[] = [];
    const result = plan({
      centerId: "left-0",
      residents,
      nodes: [node("left-0", 0, 0)],
      edges,
      trace: (event) => trace.push(event),
    });
    // Axis-compaction telemetry is throttled by wall-clock time; every other
    // event is deterministic.
    return { result, trace: trace.filter((event) => event.type !== "axis-progress") };
  };

  const first = run();
  const second = run();
  assert.deepEqual(second, first);
  const repairs = first.trace.filter(
    (event): event is Extract<LayoutTraceEvent, { type: "obstruction-repair" }> =>
      event.type === "obstruction-repair",
  );
  assert.equal(repairs.length, 8);
  assert.equal(repairs.at(-1)?.after.quality.roomObstructions, 2);
  // The request sends a chart, so it ends at the cheap compaction fixed point
  // without packing axis groups: the two obstructions the eight-wide frontier
  // leaves stay.
  assert.equal(first.result.quality.roomObstructions, 2);
  assert.deepEqual(
    first.trace
      .filter((event) => event.type === "obstruction-candidates")
      .map((event) => event.candidates.length),
    [8, 8, 8, 8, 8, 8, 8, 6],
  );
});

test("a locked room invalidates every push closure that reaches it", () => {
  const residentList = [
    resident("a", 0, 1),
    resident("blocker", 0, 0),
    resident("locked-east", 1, 0, false),
  ];
  const positions = new Map(residentList.map((room) => [room.id, room.position]));
  positions.set("late", at(0, 0));
  const repairs = safePushRepairs(
    positions,
    new Set(["a", "late"]),
    new Map(residentList.map((room) => [room.id, room])),
    [edge("blocker", "locked-east", "East")],
  );

  assert.equal(repairs.length > 0, true);
  assert.equal(repairs.every((repair) => (repair.get("blocker")?.x ?? 0) < 0), true);
  assert.equal(repairs.every((repair) =>
    JSON.stringify(repair.get("locked-east")) === JSON.stringify(at(1, 0))
  ), true);
});

// --- Incremental-scoring equivalence and occupancy-key packing bounds ---
//
// The engine scores most candidates through delta paths (rigid-translation
// contexts, changed-id edge deltas, incremental exit ports) that must agree
// bit-for-bit with a from-scratch measurement of the same geometry. These
// tests drive whole optimization passes over seeded areas and require the
// reported winner's quality tuple to equal an independent full rescore of
// the winning positions — any drift in a delta path on the winning chain
// surfaces as a tuple mismatch here.

function equivalenceXorshift(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
}

const EQUIVALENCE_REVERSE: Partial<Record<LayoutEdge["direction"], LayoutEdge["direction"]>> = {
  North: "South",
  East: "West",
  South: "North",
  West: "East",
  Up: "Down",
  Down: "Up",
};

function equivalenceArea(roomCount: number, seed: number): {
  residents: LayoutResident[];
  edges: LayoutEdge[];
  centerId: string;
} {
  const random = equivalenceXorshift(seed);
  const id = (index: number) => `eq${String(index).padStart(3, "0")}`;
  const width = Math.ceil(Math.sqrt(roomCount));
  const edges: LayoutEdge[] = [];
  const residents: LayoutResident[] = [];
  for (let index = 0; index < roomCount; index += 1) {
    const column = index % width;
    const row = Math.floor(index / width);
    // Deliberate slack and a few misplaced rooms give every compaction phase
    // real work; one immovable room exercises the movability guards.
    const slack = random() < 0.3 ? 1 : 0;
    const level = random() < 0.06 ? 1 : 0;
    residents.push({
      id: id(index),
      position: at(column * 2 + slack, row * 2, level),
      movable: index !== 1,
    });
    if (column > 0 && random() < 0.85) {
      const direction: LayoutEdge["direction"] = random() < 0.1 ? "Other" : "East";
      edges.push(edge(id(index - 1), id(index), direction));
      const reverse = EQUIVALENCE_REVERSE[direction];
      if (reverse && random() < 0.8) edges.push(edge(id(index), id(index - 1), reverse));
    }
    if (row > 0 && random() < 0.7) {
      edges.push(edge(id(index - width), id(index), "South"));
      if (random() < 0.8) edges.push(edge(id(index), id(index - width), "North"));
    }
  }
  return { residents, edges, centerId: id(0) };
}

test("room-cell corner contact is invariant under rigid translation", () => {
  const positions = new Map<string, GridPosition>([
    ["from", at(7, -4)],
    ["to", at(-14, 0)],
    ["corner", at(5, -4)],
  ]);
  const edges = [edge("from", "to", "Other")];
  const translated = new Map([...positions].map(([id, position]) => [id, {
    ...position,
    y: position.y - 1,
  }]));

  const before = measureIntegralLayoutQuality(positions, edges);
  const after = measureIntegralLayoutQuality(translated, edges);
  assert.equal(before.roomObstructions, 1, "the segment touches the room-cell corner");
  assert.equal(after.roomObstructions, before.roomObstructions);
});

/** Synthetic seed/index 100 which exposed stale rigid-translation scoring. */
function rigidScoringRegressionArea(): IntegralLayoutRequest {
  const random = equivalenceXorshift(0xa5a5c3c3);
  const directions: readonly LayoutEdge["direction"][] = [
    "North",
    "East",
    "South",
    "West",
    "Other",
  ];
  let request: IntegralLayoutRequest | undefined;
  for (let example = 0; example <= 100; example += 1) {
    const roomCount = 8 + Math.floor(random() * 28);
    const residents: LayoutResident[] = [];
    const occupied = new Set<string>();
    for (let index = 0; index < roomCount; index += 1) {
      let position: GridPosition;
      let key: string;
      do {
        position = at(
          Math.floor(random() * 31) - 15,
          Math.floor(random() * 31) - 15,
          random() < 0.08 ? 1 : 0,
        );
        key = `${position.x},${position.y},${position.level}`;
      } while (occupied.has(key));
      occupied.add(key);
      residents.push({ id: `r${index}`, position, movable: random() > 0.08 });
    }

    const edges: LayoutEdge[] = [];
    for (let index = 1; index < roomCount; index += 1) {
      const parent = Math.floor(random() * index);
      const direction = directions[Math.floor(random() * directions.length)];
      edges.push(edge(`r${parent}`, `r${index}`, direction));
      const reverse = EQUIVALENCE_REVERSE[direction];
      if (reverse && random() < 0.7) edges.push(edge(`r${index}`, `r${parent}`, reverse));
    }
    for (let index = 0; index < roomCount; index += 1) {
      const from = Math.floor(random() * roomCount);
      let to = Math.floor(random() * (roomCount - 1));
      if (to >= from) to += 1;
      edges.push(edge(
        `r${from}`,
        `r${to}`,
        directions[Math.floor(random() * directions.length)],
      ));
    }
    request = {
      centerId: "r0",
      residents,
      nodes: [],
      edges,
      allowExistingMoves: true,
    };
  }
  assert.ok(request);
  assert.equal(request.residents.length, 34, "regression fixture drifted");
  return request;
}

test("rigid-translation winner and final trace use a fresh quality tuple", () => {
  const request = rigidScoringRegressionArea();
  const trace: LayoutTraceEvent[] = [];
  const result = planIntegralLayout({ ...request, trace: (event) => trace.push(event) });
  const quality = measureIntegralLayoutQuality(result.positions, request.edges);
  assert.deepEqual(result.quality, quality);

  const final = trace.findLast((event) =>
    event.type === "selection" && event.stage === "final-selection"
  );
  assert.ok(final && final.type === "selection");
  assert.deepEqual(final.selected.quality, quality);
});

test("compaction winners rescore to their reported quality tuple", () => {
  for (const seed of [0x51ab1e01, 0x51ab1e02, 0x51ab1e03]) {
    const area = equivalenceArea(48, seed);
    const request: IntegralLayoutRequest = {
      residents: area.residents,
      nodes: [],
      edges: area.edges,
      centerId: area.centerId,
      allowExistingMoves: true,
    };
    const positions = new Map(area.residents.map((room) => [room.id, { ...room.position }]));
    const seedPlan: IntegralLayoutPlan = {
      positions,
      movedExisting: new Set(),
      quality: measureIntegralLayoutQuality(positions, area.edges),
    };
    const compacted = compactIntegralLayoutPlan(request, seedPlan);
    assert.deepEqual(
      { ...compacted.quality },
      measureIntegralLayoutQuality(compacted.positions, area.edges),
      `seed ${seed}: compaction quality drifted from a full rescore`,
    );
  }
});

test("reflow winners rescore to their reported quality tuple", () => {
  for (const seed of [0x0eef1a01, 0x0eef1a02]) {
    const area = equivalenceArea(40, seed);
    const plan = planIntegralLayout({
      residents: area.residents,
      nodes: [],
      edges: area.edges,
      centerId: area.centerId,
      allowExistingMoves: true,
    });
    assert.deepEqual(
      { ...plan.quality },
      measureIntegralLayoutQuality(plan.positions, area.edges),
      `seed ${seed}: reflow quality drifted from a full rescore`,
    );
  }
});

/**
 * A nested bridge tree laid out with every ray on its exit and no route
 * blocked, leaving three crossings the deep repair clears in several steps.
 */
function progressiveCrossingRegressionArea(): IntegralLayoutRequest {
  const cells = [
    [-8, 11],
    [-10, 11],
    [-10, 12],
    [-11, 11],
    [-8, 12],
    [-9, 12],
    [-8, 10],
    [-11, 8],
    [-10, 9],
    [-7, 7],
    [-11, 9],
    [-7, 9],
  ] as const;
  const parents = [0, 1, 1, 0, 4, 0, 5, 7, 2, 3, 0] as const;
  const directions = [
    "West",
    "South",
    "West",
    "South",
    "West",
    "North",
    "Other",
    "Other",
    "Other",
    "Other",
    "Other",
  ] as const satisfies readonly LayoutEdge["direction"][];
  const residents = cells.map(([x, y], index) =>
    resident(`progress-${index}`, x, y, index !== 0)
  );
  const edges: LayoutEdge[] = parents.flatMap((parent, offset) => {
    const child = offset + 1;
    const direction = directions[offset];
    const reverse = EQUIVALENCE_REVERSE[direction] ?? "Other";
    return [
      edge(`progress-${parent}`, `progress-${child}`, direction),
      edge(`progress-${child}`, `progress-${parent}`, reverse),
    ];
  });
  return {
    residents,
    nodes: [],
    edges,
    centerId: "progress-0",
    allowExistingMoves: true,
  };
}

function rawCrossingPublicationRegressionArea(): IntegralLayoutRequest {
  const cells = [
    [-7, -4], [-8, -4], [-2, 2], [-8, -2], [4, 3], [8, -5], [3, 3],
    [-2, 7], [2, -6], [-6, 8], [3, 5], [5, 7], [8, -8],
  ] as const;
  const parents = [0, 0, 0, 1, 1, 2, 4, 1, 6, 8, 5, 2] as const;
  const residents = cells.map(([x, y], index) => resident(`raw-${index}`, x, y, index !== 0));
  const edges = parents.flatMap((parent, offset) => [
    edge(`raw-${parent}`, `raw-${offset + 1}`, "Other"),
    edge(`raw-${offset + 1}`, `raw-${parent}`, "Other"),
  ]);
  return {
    residents,
    nodes: [],
    edges,
    centerId: "raw-0",
    allowExistingMoves: true,
  };
}

test("deep crossing progress is freshly scored and globally monotonic", () => {
  const request = progressiveCrossingRegressionArea();
  const seed = planIntegralLayout(request);
  let previous = seed.quality;
  let improvements = 0;
  const result = repairIntegralLayoutCrossingsDeep(request, seed, {
    maximumWork: 500,
    onProgress: (progress) => {
      if (progress.kind !== "improvement") return;
      const tracedPositions = progress.candidate.positions;
      assert.ok(tracedPositions, "a progressive geometry includes its complete position map");
      const positions = new Map(tracedPositions.map(({ id, x, y, level }) => [
        id,
        { x, y, level },
      ]));
      const measured = measureIntegralLayoutQuality(positions, request.edges);
      assert.deepEqual(progress.candidate.quality, measured, "published tuple was not fresh");
      assert.deepEqual(progress.bestQuality, measured, "published candidate was not the global best");
      const checkpoint: IntegralLayoutPlan = {
        positions,
        movedExisting: new Set(),
        quality: measured,
      };
      assert.equal(
        compactIntegralLayoutPlan(request, checkpoint),
        checkpoint,
        "every deep crossing publication must already be compaction-idempotent",
      );
      assert.ok(
        compareLayoutQuality(measured, previous) > 0,
        "progressive geometry regressed or tied the preceding publication",
      );
      previous = measured;
      improvements += 1;
    },
  });

  assert.ok(improvements >= 1, "fixture must exercise a progressive improvement");
  assert.ok(compareLayoutQuality(result.plan.quality, previous) >= 0);
  assert.equal(compactIntegralLayoutPlan(request, result.plan), result.plan);
});

test("deep crossing progress compacts raw bridge passages before publication", () => {
  const residentOnly = rawCrossingPublicationRegressionArea();
  const promoted = residentOnly.residents.at(-1)!;
  for (const withNewNode of [false, true]) {
    // A new node disables crossing DFS's internal axis polish. The public
    // settlement boundary must still compact the raw transaction in both
    // modes rather than exposing a giant bridge passage: fully for a reflow,
    // at the cheap fixed point for a request that sends a chart.
    const request: IntegralLayoutRequest = withNewNode
      ? {
        ...residentOnly,
        residents: residentOnly.residents.slice(0, -1),
        nodes: [{ id: promoted.id, relative: promoted.position }],
      }
      : residentOnly;
    const positions = new Map(
      residentOnly.residents.map((room) => [room.id, room.position]),
    );
    const seed: IntegralLayoutPlan = {
      positions,
      movedExisting: new Set(),
      quality: measureIntegralLayoutQuality(positions, request.edges),
    };
    const compaction = packsAxisGroups(request) ? {} : { axisGroupCompaction: false };
    let previous = seed.quality;
    let improvements = 0;
    const result = repairIntegralLayoutCrossingsDeep(request, seed, {
      maximumWork: 20,
      onProgress: (progress) => {
        if (progress.kind !== "improvement") return;
        const published = new Map(progress.candidate.positions?.map(({ id, x, y, level }) => [
          id,
          { x, y, level },
        ]));
        assert.equal(published.size, positions.size);
        const checkpoint: IntegralLayoutPlan = {
          positions: published,
          movedExisting: new Set(),
          quality: progress.candidate.quality,
        };
        assert.equal(
          compactIntegralLayoutPlan(request, checkpoint, compaction),
          checkpoint,
          `a progressive crossing checkpoint withNewNode=${withNewNode} must be compacted`,
        );
        assert.ok(compareLayoutQuality(checkpoint.quality, previous) > 0);
        previous = checkpoint.quality;
        improvements += 1;
      },
    });

    assert.ok(improvements > 0);
    assert.equal(result.plan.quality.routingViolations, 0);
    if (!withNewNode) {
      assert.equal(result.plan.quality.linkCrossings, 0);
      assert.equal(result.plan.quality.footprintArea, 15);
    }
    assert.equal(compactIntegralLayoutPlan(request, result.plan, compaction), result.plan);
  }
});

test("settled deep publications do not truncate the raw search budget", () => {
  const request = rawCrossingPublicationRegressionArea();
  const positions = new Map(request.residents.map((room) => [room.id, room.position]));
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
  };
  let improvements = 0;
  const result = repairIntegralLayoutCrossingsDeep(request, seed, {
    maximumWork: 80,
    onProgress: (progress) => {
      if (progress.kind !== "improvement") return;
      const checkpointPositions = new Map(
        progress.candidate.positions?.map(({ id, x, y, level }) => [id, { x, y, level }]),
      );
      const checkpoint: IntegralLayoutPlan = {
        positions: checkpointPositions,
        movedExisting: new Set(),
        quality: progress.candidate.quality,
      };
      assert.equal(compactIntegralLayoutPlan(request, checkpoint), checkpoint);
      improvements += 1;
    },
  });

  // The settled checkpoint has zero crossings within 20 macros, but it must
  // not stop raw DFS from spending the caller's larger bounded budget.
  assert.equal(result.stats.macrosConsidered, 80);
  assert.ok(improvements >= 1);
  assert.equal(result.plan.quality.footprintArea, 15);
  assert.equal(result.plan.quality.footprintPerimeter, 16);
  assert.equal(compactIntegralLayoutPlan(request, result.plan), result.plan);
});

test("deep crossing repair winners rescore to their reported quality tuple", () => {
  const area = equivalenceArea(36, 0xdeeb0001);
  const positions = new Map(area.residents.map((room) => [room.id, { ...room.position }]));
  const seedPlan: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, area.edges),
  };
  const result = repairIntegralLayoutCrossingsDeep(
    {
      residents: area.residents,
      nodes: [],
      edges: area.edges,
      centerId: area.centerId,
      allowExistingMoves: true,
    },
    seedPlan,
    { maximumWork: 400 },
  );
  assert.deepEqual(
    { ...result.plan.quality },
    measureIntegralLayoutQuality(result.plan.positions, area.edges),
    "deep repair quality drifted from a full rescore",
  );
});

test("occupancy scoring is identical outside the packed-key envelope", () => {
  // The packed cell keys assume |x|, |y| < 2^20 and |level| < 2^9; beyond
  // that (or off the integral grid) probes fall back to string keys. The
  // same small scene measured near the origin, far outside the envelope, and
  // at fractional coordinates must produce identical tuples.
  const scene = (dx: number, dy: number, level: number) => {
    const positions = new Map<string, GridPosition>([
      ["a", at(dx, dy, level)],
      ["b", at(dx + 2, dy, level)],
      ["blocker", at(dx + 1, dy, level)],
      ["stack-1", at(dx + 4, dy + 1, level)],
      ["stack-2", at(dx + 4, dy + 1, level)],
    ]);
    const edges = [
      edge("a", "b", "East"),
      edge("b", "a", "West"),
      edge("stack-1", "stack-2", "North"),
    ];
    return { positions, edges };
  };
  const near = scene(0, 0, 0);
  const nearQuality = measureIntegralLayoutQuality(near.positions, near.edges);
  assert.equal(nearQuality.exitPortViolations, 2);
  assert.equal(nearQuality.roomObstructions, 1);
  for (const [dx, dy, level] of [
    [1 << 21, -(1 << 21), 0],
    [5_000_000, 5_000_000, 700],
    [0.5, -0.25, 0],
  ] as const) {
    const far = scene(dx, dy, level);
    assert.deepEqual(
      measureIntegralLayoutQuality(far.positions, far.edges),
      nearQuality,
      `offset ${dx},${dy},${level}: quality changed outside the packed envelope`,
    );
    assert.deepEqual(
      measureLayoutRoutingQuality(far.positions, far.edges),
      measureLayoutRoutingQuality(near.positions, near.edges),
    );
  }
});

// ---------------------------------------------------------------------------
// Route amendments: declarative detours for permanent fixed-room defects
// ---------------------------------------------------------------------------

function fixedCrossResidents(southMovable = false): LayoutResident[] {
  return [
    resident("west", -2, 0, false),
    resident("east", 2, 0, false),
    resident("north", 0, -2, false),
    resident("south", 0, 2, southMovable),
  ];
}

const FIXED_CROSS_EDGES: LayoutEdge[] = [
  edge("west", "east", "Other"),
  edge("north", "south", "Other"),
];

test("a permanent fixed-endpoint crossing rides the plan as a route amendment", () => {
  const trace: LayoutTraceEvent[] = [];
  const request: IntegralLayoutRequest = {
    residents: fixedCrossResidents(),
    nodes: [],
    edges: FIXED_CROSS_EDGES,
    centerId: "west",
    allowExistingMoves: true,
    trace: (event) => trace.push(event),
  };
  const result = planIntegralLayout(request);

  // No participant can move, so the geometry and its metrics are untouched:
  // the amendment is presentation-layer truth, never a metric adjustment.
  assert.deepEqual(result.positions.get("west"), at(-2, 0));
  assert.deepEqual(result.positions.get("south"), at(0, 2));
  assert.equal(result.quality.linkCrossings, 1);

  const amendments = result.routeAmendments;
  assert.ok(amendments);
  assert.equal(amendments.length, 1);
  const [amendment] = amendments;
  const pair = [amendment.from, amendment.to].sort().join("|");
  assert.ok(pair === "east|west" || pair === "north|south");
  assert.ok(amendment.waypoints.length >= 1);
  for (const waypoint of amendment.waypoints) {
    assert.ok(Number.isInteger(waypoint.x) && Number.isInteger(waypoint.y));
    assert.equal(waypoint.level, 0);
  }

  const finalSelection = trace.find((event) =>
    event.type === "selection" && event.stage === "final-selection"
  );
  assert.ok(finalSelection && finalSelection.type === "selection");
  assert.deepEqual(finalSelection.routeAmendments, amendments);

  const again = planIntegralLayout({ ...request, trace: undefined });
  assert.deepEqual(again.routeAmendments, amendments);
});

test("an amended obstruction detour honors the link's declared cardinal walls", () => {
  const request: IntegralLayoutRequest = {
    residents: [
      resident("a", 0, 0, false),
      resident("b", 4, 0, false),
      resident("c", 2, 0, false),
    ],
    nodes: [],
    edges: [edge("a", "b", "East"), edge("b", "a", "West")],
    centerId: "a",
    allowExistingMoves: true,
  };
  const result = planIntegralLayout(request);
  assert.ok(result.quality.roomObstructions >= 1);

  const amendments = result.routeAmendments;
  assert.ok(amendments);
  assert.equal(amendments.length, 1);
  const [amendment] = amendments;
  assert.equal(amendment.from, "a");
  assert.equal(amendment.to, "b");
  const first = amendment.waypoints[0];
  const last = amendment.waypoints[amendment.waypoints.length - 1];
  // The route leaves `a` through its declared East wall and enters `b`
  // through the West wall its reciprocal traversal declares.
  assert.ok(first.y === 0 && first.x > 0, "detour leaves through the East wall");
  assert.ok(last.y === 0 && last.x < 4, "detour arrives through the West wall");
  assert.ok(
    amendment.waypoints.every((waypoint) => waypoint.x !== 2 || waypoint.y !== 0),
    "the detour dodges the obstructing room",
  );
});

test("a movable participant keeps the defect in the movement engine's hands", () => {
  const result = planIntegralLayout({
    residents: fixedCrossResidents(true),
    nodes: [],
    edges: FIXED_CROSS_EDGES,
    centerId: "west",
    allowExistingMoves: true,
  });
  assert.equal(result.routeAmendments, undefined);
});

test("the prompt lane never proposes route amendments", () => {
  const result = planIntegralLayout({
    residents: fixedCrossResidents(),
    nodes: [],
    edges: FIXED_CROSS_EDGES,
    centerId: "west",
    allowExistingMoves: false,
  });
  assert.equal(result.quality.linkCrossings, 1);
  assert.equal(result.routeAmendments, undefined);
});

test("the exported amendment computation matches what plans attach", () => {
  const request: IntegralLayoutRequest = {
    residents: fixedCrossResidents(),
    nodes: [],
    edges: FIXED_CROSS_EDGES,
    centerId: "west",
    allowExistingMoves: true,
  };
  const result = planIntegralLayout(request);
  assert.ok(result.routeAmendments);
  assert.deepEqual(computeIntegralRouteAmendments(request, result), result.routeAmendments);
  assert.equal(
    computeIntegralRouteAmendments({ ...request, allowExistingMoves: false }, result),
    undefined,
  );
});
