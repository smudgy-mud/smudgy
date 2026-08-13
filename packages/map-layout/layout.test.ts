import assert from "node:assert/strict";
import test from "node:test";
import {
  planIntegralLayout,
  safePushRepairs,
  type GridPosition,
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
  const result = plan({
    centerId: "a",
    residents: [resident("a", 0, 0), resident("b", 4, 2)],
    nodes: [node("a", 0, 0), node("b", 1, 0)],
    edges: [edge("a", "b", "East")],
  });

  const a = result.positions.get("a");
  const b = result.positions.get("b");
  assert.ok(a && b);
  assert.equal(b.x - a.x, 1);
  assert.equal(b.y, a.y);
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

test("shifts a whole outer region to repair the current room's cardinal exit", () => {
  const northernResidents: LayoutResident[] = [];
  const northernEdges: LayoutEdge[] = [];
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
  });

  assert.deepEqual(result.positions.get("a"), at(0, 0));
  assert.deepEqual(result.positions.get("north-0-2"), at(0, -1));
  for (let row = 0; row < 3; row += 1) {
    for (let column = 0; column < 3; column += 1) {
      assert.deepEqual(result.positions.get(`north-${column}-${row}`), at(column, row - 3));
    }
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
  assert.equal(trace.some((event) =>
    event.type === "obstruction-repair" && event.offset.y === -2
  ), true);
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
