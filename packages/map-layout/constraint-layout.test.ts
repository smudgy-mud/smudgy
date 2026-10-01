import assert from "node:assert/strict";
import test from "node:test";
import {
  constraintLayoutInternalsForTesting as internals,
  repairIntegralLayoutConstraints,
} from "./constraint-layout.ts";
import {
  compactIntegralLayoutPlan,
  compareLayoutQuality,
  directionalViolationEdges,
  measureIntegralLayoutQuality,
  measureLayoutRoutingQuality,
  planIntegralLayout,
  type ConstraintRepairOptions,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
  type LayoutDirection,
  type LayoutEdge,
  type LayoutTraceEvent,
} from "./layout.ts";
import { isLayoutWorkerProgress, LAYOUT_WORKER_PROTOCOL_VERSION } from "./worker-protocol.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });

function chain(size: number, cycle: boolean): {
  positions: Map<string, GridPosition>;
  edges: LayoutEdge[];
} {
  const id = (index: number) => `r${String(index).padStart(5, "0")}`;
  const positions = new Map<string, GridPosition>();
  const edges: LayoutEdge[] = [];
  for (let index = 0; index < size; index += 1) {
    positions.set(id(index), at(index, 0));
    if (index > 0) edges.push({ from: id(index - 1), to: id(index), direction: "East" });
  }
  if (cycle) edges.push({ from: id(size - 1), to: id(0), direction: "East" });
  return { positions, edges };
}

function repairFixture(): {
  request: IntegralLayoutRequest;
  standard: IntegralLayoutPlan;
} {
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(1, 0)],
    ["c", at(2, 0)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "East" },
    { from: "b", to: "c", direction: "East" },
    { from: "c", to: "a", direction: "East" },
  ];
  return {
    request: {
      nodes: [],
      residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
      edges,
      allowExistingMoves: true,
    },
    standard: {
      positions,
      movedExisting: new Set(),
      quality: {
        cardinalRayViolations: 1,
        reciprocalRayViolations: 0,
        routingViolations: 0,
        exitPortViolations: 0,
        reciprocalExitPortViolations: 0,
        roomObstructions: 0,
        linkCrossings: 0,
        cardinalSlack: 0,
        footprintArea: 3,
        footprintPerimeter: 8,
      },
    },
  };
}

function softSeparatorFixture(): {
  request: IntegralLayoutRequest;
  standard: IntegralLayoutPlan;
} {
  const residents = [
    { id: "from", position: at(0, 0), movable: true },
    { id: "to", position: at(0, -3), movable: true },
    { id: "block-a", position: at(0, -1), movable: true },
    { id: "block-b", position: at(0, -2), movable: true },
  ];
  const edges: LayoutEdge[] = [
    { from: "from", to: "to", direction: "North" },
    { from: "to", to: "from", direction: "South" },
    { from: "block-a", to: "block-b", direction: "North" },
    { from: "block-b", to: "block-a", direction: "South" },
  ];
  const request: IntegralLayoutRequest = {
    residents,
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  return {
    request,
    standard: planIntegralLayout({ ...request, allowExistingMoves: false }),
  };
}

function multiAnchorPolishFixture(centerId = "8"): {
  request: IntegralLayoutRequest;
  seed: IntegralLayoutPlan;
} {
  const residents = [
    { id: "0", position: at(3, -2), movable: true },
    { id: "1", position: at(1, 4), movable: true },
    { id: "2", position: at(5, -1), movable: true },
    { id: "3", position: at(-6, 3), movable: true },
    { id: "4", position: at(0, -4), movable: true },
    { id: "5", position: at(-4, -2), movable: true },
    { id: "6", position: at(4, -5), movable: true },
    { id: "7", position: at(1, 3), movable: true },
    { id: "8", position: at(1, 0), movable: true },
  ];
  const edges: LayoutEdge[] = [
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
    { from: "6", to: "0", direction: "South" },
    { from: "0", to: "6", direction: "North" },
  ];
  const request: IntegralLayoutRequest = {
    residents,
    nodes: [],
    edges,
    centerId,
    allowExistingMoves: true,
  };
  const seed = planIntegralLayout({
    ...request,
    residents: residents.map((resident) => ({
      ...resident,
      movable: resident.id !== request.centerId,
    })),
  });
  return { request, seed };
}

function unfinishedCrossingFixture(): IntegralLayoutRequest {
  const cells = [
    [-8, 11],
    [-5, 5],
    [10, 2],
    [-2, -3],
    [-7, -6],
    [9, -4],
    [-8, -3],
    [-8, 5],
    [8, 4],
    [1, -6],
    [5, -10],
    [-6, 0],
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
  ] as const;
  const reverses = {
    North: "South",
    East: "West",
    South: "North",
    West: "East",
    Other: "Other",
  } as const;
  const residents = cells.map(([x, y], index) => ({
    id: `r${index}`,
    position: at(x, y),
    movable: index !== 0,
  }));
  const edges = parents.flatMap((parent, offset) => {
    const child = offset + 1;
    const forward = directions[offset];
    return [
      { from: `r${parent}`, to: `r${child}`, direction: forward },
      { from: `r${child}`, to: `r${parent}`, direction: reverses[forward] },
    ];
  });
  return {
    residents,
    nodes: [],
    edges,
    centerId: "r0",
    allowExistingMoves: true,
  };
}

/**
 * Whether the constraints of `edges` without `removed` hold together. A flat
 * edge in `relaxedLevels` keeps neither its shared level nor its ray.
 */
function referenceConstraintFeasibility(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  removed: ReadonlySet<number>,
  relaxedLevels: ReadonlySet<number> = new Set(),
): boolean {
  const ids = [...positions.keys()].sort();
  const indexes = new Map(ids.map((id, index) => [id, index]));
  const parent = [0, 1, 2].map(() => Int32Array.from(ids, (_, index) => index));
  const find = (axis: number, value: number): number => {
    let root = value;
    while (parent[axis][root] !== root) root = parent[axis][root];
    while (parent[axis][value] !== value) {
      const next = parent[axis][value];
      parent[axis][value] = root;
      value = next;
    }
    return root;
  };
  const union = (axis: number, first: number, second: number): void => {
    const a = find(axis, first);
    const b = find(axis, second);
    if (a !== b) parent[axis][b] = a;
  };
  const relations: { axis: number; low: number; high: number }[] = [];
  for (let index = 0; index < edges.length; index += 1) {
    const edge = edges[index];
    const from = indexes.get(edge.from);
    const to = indexes.get(edge.to);
    if (from === undefined || to === undefined) continue;
    const vector = edge.direction === "East" ? [1, 0, 0]
      : edge.direction === "West" ? [-1, 0, 0]
      : edge.direction === "South" ? [0, 1, 0]
      : edge.direction === "North" ? [0, -1, 0]
      : edge.direction === "Up" ? [0, 0, 1]
      : edge.direction === "Down" ? [0, 0, -1]
      : undefined;
    if (!vector) continue;
    const axis = vector.findIndex((value) => value !== 0);
    if (relaxedLevels.has(index)) continue;
    if (axis !== 2) union(2, from, to);
    if (removed.has(index)) continue;
    for (let other = 0; other < 3; other += 1) {
      if (other !== axis) union(other, from, to);
    }
    relations.push({
      axis,
      low: vector[axis] > 0 ? from : to,
      high: vector[axis] > 0 ? to : from,
    });
  }

  for (const relation of relations) {
    if (find(relation.axis, relation.low) === find(relation.axis, relation.high)) return false;
  }
  for (let axis = 0; axis < 3; axis += 1) {
    const outgoing = new Map<number, number[]>();
    const indegree = new Map<number, number>();
    for (let node = 0; node < ids.length; node += 1) {
      const root = find(axis, node);
      if (!outgoing.has(root)) outgoing.set(root, []);
      indegree.set(root, 0);
    }
    for (const relation of relations) {
      if (relation.axis !== axis) continue;
      const from = find(axis, relation.low);
      const to = find(axis, relation.high);
      outgoing.get(from)?.push(to);
      indegree.set(to, (indegree.get(to) ?? 0) + 1);
    }
    const ready = [...indegree].filter(([, degree]) => degree === 0).map(([root]) => root);
    let visited = 0;
    while (ready.length > 0) {
      const root = ready.pop() as number;
      visited += 1;
      for (const target of outgoing.get(root) ?? []) {
        const degree = (indegree.get(target) as number) - 1;
        indegree.set(target, degree);
        if (degree === 0) ready.push(target);
      }
    }
    if (visited !== indegree.size) return false;
  }

  const triples = new Set<string>();
  for (let node = 0; node < ids.length; node += 1) {
    const triple = `${find(0, node)}:${find(1, node)}:${find(2, node)}`;
    if (triples.has(triple)) return false;
    triples.add(triple);
  }
  return true;
}

/**
 * The master objective's exhaustive minimum, [directed source edges given up,
 * those of them with a reciprocal, level relations given up], over every set
 * of source edges and of the graph's level relations whose remaining
 * constraints hold together. A level relation gives up its flat edges too.
 */
function exhaustiveRelaxationScore(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  reciprocal: readonly boolean[],
): [number, number, number] | undefined {
  const { levelRelations } = internals.relations(positions, edges);
  let best: [number, number, number] | undefined;
  for (let levelMask = 0; levelMask < 1 << levelRelations.length; levelMask += 1) {
    const relaxedLevels = new Set<number>();
    let levels = 0;
    for (let relation = 0; relation < levelRelations.length; relation += 1) {
      if (!(levelMask & (1 << relation))) continue;
      levels += 1;
      for (const sourceIndex of levelRelations[relation]) relaxedLevels.add(sourceIndex);
    }
    for (let mask = 0; mask < 1 << edges.length; mask += 1) {
      const removed = new Set(relaxedLevels);
      for (let edge = 0; edge < edges.length; edge += 1) {
        if (mask & (1 << edge)) removed.add(edge);
      }
      if (!referenceConstraintFeasibility(positions, edges, removed, relaxedLevels)) continue;
      const score: [number, number, number] = [
        removed.size,
        [...removed].filter((edge) => reciprocal[edge]).length,
        levels,
      ];
      if (!best || score[0] < best[0] || score[0] === best[0] &&
        (score[1] < best[1] || score[1] === best[1] && score[2] < best[2])) best = score;
    }
  }
  return best;
}

test("iterative constraint traversal handles 6,000- and 15,000-node chains", () => {
  const acyclic = chain(6_000, false);
  const acyclicResult = internals.analyze(acyclic.positions, acyclic.edges);
  assert.equal(acyclicResult.ok, true);
  assert.equal(acyclicResult.ok && acyclicResult.feasible, true);

  const cyclic = chain(15_000, true);
  const cyclicResult = internals.analyze(cyclic.positions, cyclic.edges);
  assert.equal(cyclicResult.ok, true);
  assert.equal(cyclicResult.ok && cyclicResult.feasible, false);
  assert.equal(cyclicResult.ok && cyclicResult.conflictSourceIndexes?.length, 15_000);
});

test("hard validity rejects fractional coordinates instead of rounding them", () => {
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(1, 0)],
  ]);
  const edges: LayoutEdge[] = [{ from: "a", to: "b", direction: "East" }];
  assert.equal(internals.hardValid(positions, edges, positions), true);
  assert.equal(internals.hardValid(positions, edges, new Map([
    ["a", at(0, 0)],
    ["b", at(1.25, 0)],
  ])), false);
  assert.equal(internals.hardValid(positions, edges, new Map([
    ["a", at(0, 0)],
    ["b", at(Number.MAX_SAFE_INTEGER + 1, 0)],
  ])), false);
});

test("relaxing a planar ray never relaxes its hard same-level topology", () => {
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(1, 0)],
  ]);
  const edges: LayoutEdge[] = [{ from: "a", to: "b", direction: "East" }];
  const movedOffRay = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(-3, 4)],
  ]);
  const movedOffLevel = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(-3, 4, 1)],
  ]);

  assert.equal(internals.hardValid(positions, edges, movedOffRay, [0]), true);
  assert.equal(internals.hardValid(positions, edges, movedOffLevel, [0]), false);
});

test("hard validity freezes absolute levels outside level-crossing components", () => {
  const positions = new Map<string, GridPosition>([
    ["plane-a", at(0, 0, 7)],
    ["plane-b", at(1, 0, 7)],
    ["isolated", at(5, 5, -3)],
  ]);
  const edges: LayoutEdge[] = [{ from: "plane-a", to: "plane-b", direction: "East" }];
  assert.equal(internals.hardValid(positions, edges, new Map<string, GridPosition>([
    ["plane-a", at(0, 0, 8)],
    ["plane-b", at(1, 0, 8)],
    ["isolated", at(5, 5, -3)],
  ]), [0]), false, "a whole planar component cannot drift to another level");
  assert.equal(internals.hardValid(positions, edges, new Map<string, GridPosition>([
    ["plane-a", at(0, 0, 7)],
    ["plane-b", at(1, 0, 7)],
    ["isolated", at(5, 5, -2)],
  ]), [0]), false, "an isolated room cannot drift to another level");

  const verticalPositions = new Map<string, GridPosition>([
    ["lower", at(0, 0)],
    ["upper", at(0, 0, 1)],
    ["upper-east", at(1, 0, 1)],
  ]);
  const verticalEdges: LayoutEdge[] = [
    { from: "lower", to: "upper", direction: "Up" },
    { from: "upper", to: "upper-east", direction: "East" },
  ];
  assert.equal(internals.hardValid(
    verticalPositions,
    verticalEdges,
    new Map<string, GridPosition>([
      ["lower", at(0, 0, 4)],
      ["upper", at(0, 0, 5)],
      ["upper-east", at(1, 0, 5)],
    ]),
  ), true, "a planar wing inherits its component's real level-crossing reachability");
});

test("soft separator selection skips an unrepairable first defect", () => {
  const selected = internals.firstAdmissibleSeparator(
    3,
    [{ axis: 0, from: 0, to: 1 }],
    [
      {
        kind: "cycle-or-no-op",
        alternatives: [
          { arcs: [{ axis: 0, from: 1, to: 0 }] },
          { arcs: [{ axis: 0, from: 0, to: 1 }] },
        ],
      },
      {
        kind: "later-repairable",
        alternatives: [{ arcs: [{ axis: 0, from: 1, to: 2 }] }],
      },
    ],
  );
  assert.equal(selected, 1);
});

test("a legal supplied layout survives incompatible compact shifts for multiple fixed anchors", () => {
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(5, 0)],
  ]);
  const edges: LayoutEdge[] = [{ from: "a", to: "b", direction: "East" }];
  const request: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: at(0, 0), movable: false },
      { id: "b", position: at(5, 0), movable: false },
    ],
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const standard: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  const trace: LayoutTraceEvent[] = [];
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 1,
    maxLayouts: 1,
    maxExtensionStates: 0,
    maxMaskDiversifications: 1,
    maxCrossingWork: 0,
  });

  assert.deepEqual(repaired.positions, positions);
  assert.equal(repaired.constraintRepair?.rawIncumbents, 1);
  assert.equal(repaired.constraintRepair?.distinctLayouts, 1);
  assert.equal(repaired.constraintRepair?.separatorStates, 0);
  assert.equal(repaired.constraintRepair?.cutoff, "extensions");
  assert.deepEqual(repaired.constraintRepair?.extensionSearch, {
    completed: false,
    cancelled: false,
    exhausted: true,
  });
  assert.equal(repaired.constraintRepair?.geometricFixedPoint, false);
});

test("expected search and compaction failures return the supplied standard plan", () => {
  const { request, standard } = repairFixture();
  for (const [failure, outcome] of [
    ["search", "search-failed:analysis"],
    ["compaction", "no-layout"],
  ] as const) {
    const trace: LayoutTraceEvent[] = [];
    const result = internals.repairWithFailure(
      request,
      standard,
      { when: "always", maxDurationMs: 1_000 },
      failure,
      (event) => trace.push(event),
    );
    const { constraintRepair: report, ...plan } = result;
    assert.deepEqual(plan, standard);
    assert.equal(report?.outcome, outcome);
    assert.equal(report?.cutoff, "none", "neither failure is a deadline or a ceiling");
    assert.deepEqual(trace, [{ type: "constraint-repair", stage: "constraint-repair", report }]);
  }
});

/** Two rooms joined both ways and drawn exactly: nothing to repair. */
function cleanPairRequest(): IntegralLayoutRequest {
  return {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(1, 0), movable: true },
    ],
    nodes: [],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "a", direction: "West" },
    ],
    allowExistingMoves: true,
  };
}

test("a repair that does not search says why, on its plan and to the trace", () => {
  const { request, standard } = repairFixture();
  const clean = cleanPairRequest();
  const cleanSeed = storedPlan(clean);
  // An Up exit projected along a diagonal is misdrawn, and the constraint
  // graph cannot express it.
  const projected: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "loft", position: at(3, 2), movable: true },
    ],
    nodes: [],
    edges: [{ from: "a", to: "loft", direction: "Up", constraintVector: at(1, -1) }],
    allowExistingMoves: true,
  };
  const projectedSeed = storedPlan(projected);
  assert.equal(projectedSeed.quality.cardinalRayViolations, 1);
  // A clock that advances a second on every reading spends a one-millisecond
  // budget before the search's first check.
  let tick = 0;
  const jumpingClock = (): number => (tick += 1_000);
  const cases: [
    outcome: string,
    seed: IntegralLayoutPlan,
    repair: (trace: (event: LayoutTraceEvent) => void) => IntegralLayoutPlan,
  ][] = [
    ["locked", standard, (trace) =>
      repairIntegralLayoutConstraints(
        { ...request, allowExistingMoves: false },
        standard,
        { when: "always" },
        trace,
      )],
    ["no-regression", standard, (trace) =>
      repairIntegralLayoutConstraints(request, standard, { when: "settled-regression" }, trace)],
    ["no-regression", standard, (trace) =>
      repairIntegralLayoutConstraints(request, standard, { when: "violation-regression" }, trace)],
    ["clean", cleanSeed, (trace) =>
      repairIntegralLayoutConstraints(clean, cleanSeed, { when: "defects" }, trace)],
    ["no-constraints", projectedSeed, (trace) =>
      repairIntegralLayoutConstraints(projected, projectedSeed, { when: "defects" }, trace)],
    ["no-budget", standard, (trace) =>
      repairIntegralLayoutConstraints(request, standard, { when: "always", maxDurationMs: 0 }, trace)],
    ["search-failed:time", standard, (trace) =>
      internals.repairWithClock(request, standard, { when: "always", maxDurationMs: 1 }, jumpingClock, trace)],
  ];
  for (const [outcome, seed, repair] of cases) {
    const trace: LayoutTraceEvent[] = [];
    const { constraintRepair: report, ...plan } = repair((event) => trace.push(event));
    assert.deepEqual(plan, seed, `${outcome} returns its seed`);
    assert.ok(report, `${outcome} reports`);
    assert.equal(report.outcome, outcome);
    assert.equal(report.selected, false, outcome);
    assert.equal(report.geometricFixedPoint, false, outcome);
    assert.equal(report.polishCutoff, "none", outcome);
    assert.equal(report.cutoff, outcome === "search-failed:time" ? "time" : "none", outcome);
    assert.equal(report.finalViolations, seed.quality.cardinalRayViolations, outcome);
    const reported = trace.filter((event) => event.type === "constraint-repair");
    assert.deepEqual(reported, [{ type: "constraint-repair", stage: "constraint-repair", report }], outcome);
    assert.equal(isLayoutWorkerProgress({
      protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
      id: 1,
      operation: "constraint-repair",
      progress: true,
      event: reported[0],
    }), true, `${outcome} crosses the Worker boundary`);
  }
});

test("the regression triggers weigh a directional violation against routing violations as the quality order does", () => {
  // Before, `a` reaches `b` East past a row of blockers: every blocker
  // obstructs the link and the first also blocks `a`'s East port. The plan
  // moves `b` north: one directional violation for all but the port.
  const plans = (blockers: number) => {
    const positions = new Map<string, GridPosition>([["a", at(0, 0)], ["b", at(blockers + 1, 0)]]);
    for (let index = 1; index <= blockers; index += 1) positions.set(`blocker-${index}`, at(index, 0));
    const edges: LayoutEdge[] = [{ from: "a", to: "b", direction: "East" }];
    const request: IntegralLayoutRequest = {
      nodes: [],
      residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
      edges,
      allowExistingMoves: true,
    };
    const moved = new Map(positions).set("b", at(0, -2));
    const standard: IntegralLayoutPlan = {
      positions: moved,
      movedExisting: new Set(["b"]),
      quality: measureIntegralLayoutQuality(moved, edges),
    };
    return { request, standard, before: measureIntegralLayoutQuality(positions, edges) };
  };
  for (const when of ["settled-regression", "violation-regression"] as const) {
    for (const [blockers, outcome] of [[7, "no-budget"], [8, "no-budget"], [9, "no-regression"]] as const) {
      const { request, standard, before } = plans(blockers);
      assert.equal(standard.quality.cardinalRayViolations, before.cardinalRayViolations + 1);
      assert.equal(standard.quality.routingViolations, before.routingViolations - blockers);
      // A budget of zero stops a repair the trigger lets through before it searches.
      const repaired = repairIntegralLayoutConstraints(request, standard, { when, maxDurationMs: 0 });
      assert.equal(repaired.constraintRepair?.outcome, outcome, `${when} with ${blockers} blockers`);
    }
  }
});

test("the defects mode repairs a defective layout as always does and returns a clean one as it is", () => {
  const clean = cleanPairRequest();
  const cleanSeed = storedPlan(clean);
  const skipped = repairIntegralLayoutConstraints(clean, cleanSeed, { when: "defects" });
  assert.equal(skipped.constraintRepair?.outcome, "clean");
  assert.equal(skipped.constraintRepair?.trigger, "defects");
  assert.deepEqual(skipped.positions, cleanSeed.positions);
  const polished = repairIntegralLayoutConstraints(clean, cleanSeed, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
  });
  assert.equal(polished.constraintRepair?.outcome, "searched", "always still polishes a clean layout");

  const { request, standard } = repairFixture();
  assert.ok(standard.quality.cardinalRayViolations > 0);
  const options = { maxDurationMs: Number.POSITIVE_INFINITY, maxLayouts: 5 };
  const repaired = repairIntegralLayoutConstraints(request, standard, { ...options, when: "defects" });
  const always = repairIntegralLayoutConstraints(request, standard, { ...options, when: "always" });
  assert.equal(repaired.constraintRepair?.outcome, "searched");
  assert.equal(repaired.constraintRepair?.trigger, "defects");
  assert.deepEqual(repaired.positions, always.positions);
  assert.deepEqual(repaired.quality, always.quality);
  assert.equal(repaired.constraintRepair?.selected, always.constraintRepair?.selected);
});

test("deep repair polishes only distinct complete layouts within its configured bound", () => {
  const { request, standard } = repairFixture();
  const trace: LayoutTraceEvent[] = [];
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: Number.POSITIVE_INFINITY,
    maxLayouts: 5,
  }, (event) => trace.push(event));

  const report = repaired.constraintRepair;
  assert.ok(report && report.layoutsConsidered >= 1 && report.layoutsConsidered <= 5);
  assert.ok(report && report.distinctLayouts >= report.layoutsConsidered);
  assert.ok(report && report.rawIncumbents >= report.distinctLayouts);
  assert.equal(repaired.constraintRepair?.cutoff, "none");
  assert.equal(report?.compactionAttempts, report?.maskDiversifications);
  assert.ok((report?.maskDiversifications ?? 0) >= 1);
  assert.ok(trace.some((event) =>
    event.type === "constraint-repair" && event.report.layoutsConsidered === report?.layoutsConsidered
  ));
  assert.equal(repaired.constraintRepair?.constraintOptimal, repaired.constraintRepair?.optimal);
  assert.equal(repaired.constraintRepair?.geometricFixedPoint, true);
  assert.equal(repaired.constraintRepair?.polishCutoff, "fixed-point");
  assert.ok((repaired.constraintRepair?.polishPasses ?? 0) > 0);
});

test("layout cutoff requires an actually unprocessed polish entry", () => {
  const { request, standard } = repairFixture();
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: Number.POSITIVE_INFINITY,
    maxLayouts: 1,
  });

  assert.equal(repaired.constraintRepair?.distinctLayouts, 1);
  assert.equal(repaired.constraintRepair?.layoutsConsidered, 1);
  assert.equal(repaired.constraintRepair?.cutoff, "none");
});

test("geometric fixed point requires crossing repair to finish", () => {
  const request = unfinishedCrossingFixture();
  const standard = planIntegralLayout({ ...request, allowExistingMoves: false });
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 1,
    maxLayouts: 16,
    maxCrossingWork: 0,
  });

  const report = repaired.constraintRepair;
  assert.ok(report);
  assert.equal(report.layoutsConsidered, report.distinctLayouts);
  assert.equal(report.cutoff, "none", "the complete layout frontier was not capped");
  assert.deepEqual({
    completed: report.crossingRepair.completed,
    cancelled: report.crossingRepair.cancelled,
    exhausted: report.crossingRepair.exhausted,
  }, { completed: false, cancelled: false, exhausted: true });
  assert.equal(report.polishCutoff, "fixed-point");
  assert.equal(report.extensionSearch.completed, true);
  assert.equal(report.maskDiversification.completed, true);
  assert.equal(report.geometricFixedPoint, false);

  // The tournament cap keeps the truncated winner imperfect: an unbounded
  // fixed-point polish on this fixture reaches a perfect layout from the one
  // retained frontier entry, and a perfect winner truthfully reports "none"
  // rather than the truncation this variant exists to pin.
  const truncated = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 1,
    maxLayouts: 1,
    maxPolishTournaments: 0,
    maxCrossingWork: 0,
  });
  assert.ok((truncated.constraintRepair?.distinctLayouts ?? 0) > 1);
  assert.equal(truncated.constraintRepair?.layoutsConsidered, 1);
  assert.equal(truncated.constraintRepair?.cutoff, "layouts");
  assert.equal(truncated.constraintRepair?.geometricFixedPoint, false);
});

test("post-constraint polish reaches the multi-anchor reflow fixed point", () => {
  const { request, seed } = multiAnchorPolishFixture();
  const trace: LayoutTraceEvent[] = [];
  const polished = internals.polish(request, seed, (event) => trace.push(event));

  assert.ok(compareLayoutQuality(polished.plan.quality, seed.quality) > 0);
  assert.equal(polished.fixedPoint, true);
  assert.equal(polished.cutoff, "fixed-point");
  assert.ok(polished.tournaments >= 2, "one improving tournament plus a fixed-point proof");
  assert.ok(polished.passes > polished.tournaments);
  assert.ok(polished.improvements > 0);
  assert.ok(trace.some((event) => event.type === "constraint-improvement"));
  assert.equal(
    trace.filter((event) => event.type === "constraint-progress" && event.phase === "polish").length,
    polished.passes + 1,
  );

  const repeated = internals.polish(request, polished.plan);
  assert.equal(compareLayoutQuality(repeated.plan.quality, polished.plan.quality), 0);
  assert.equal(repeated.tournaments, 1);
  assert.equal(repeated.improvements, 0);
});

test("a one-pass anchored preview publishes before MaxHS without starving it", () => {
  const { request, seed } = multiAnchorPolishFixture("0");
  const trace: LayoutTraceEvent[] = [];
  const repaired = repairIntegralLayoutConstraints(request, seed, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 1,
    maxLayouts: 1,
    maxPolishTournaments: 2,
    maxPolishPasses: 2,
    maxExtensionStates: 0,
    maxMaskDiversifications: 1,
    maxCrossingWork: 0,
  }, (event) => trace.push(event));

  const first = trace.find((event) => event.type === "constraint-improvement");
  assert.ok(first && first.type === "constraint-improvement");
  assert.equal(first.feasibilityChecks, 0);
  assert.equal(first.compactionAttempts, 0);
  assert.equal(first.separatorStates, 0);
  assert.equal(first.layoutsConsidered, 1, "the complete preview pass is charged before publication");
  assert.ok(compareLayoutQuality(first.candidate.quality, seed.quality) > 0);
  assert.ok(compareLayoutQuality(repaired.quality, first.candidate.quality) >= 0);
  assert.ok(repaired.constraintRepair);
  assert.ok(repaired.constraintRepair.feasibilityChecks > 0, "MaxHS began after the preview");
  assert.equal(repaired.constraintRepair.polishPasses, 2);
  assert.equal(repaired.constraintRepair.polishAnchorsTried, 2);
  assert.equal(repaired.constraintRepair.polishTournaments, 0);
  assert.equal(repaired.constraintRepair.polishCutoff, "passes");
  assert.equal(repaired.constraintRepair.geometricFixedPoint, false);
  const streamed = trace.filter((event): event is Extract<LayoutTraceEvent, {
    type: "constraint-progress" | "constraint-improvement";
  }> => event.type === "constraint-progress" || event.type === "constraint-improvement");
  let layoutsConsidered = 0;
  let bestQuality = seed.quality;
  for (const event of streamed) {
    assert.ok(event.layoutsConsidered >= layoutsConsidered);
    layoutsConsidered = event.layoutsConsidered;
    const quality = event.type === "constraint-improvement" ? event.candidate.quality : event.bestQuality;
    if (!quality) continue;
    assert.ok(compareLayoutQuality(quality, bestQuality) >= 0);
    bestQuality = quality;
  }
});

test("a deterministic polish tournament ceiling retains the last complete winner", () => {
  const { request, seed } = multiAnchorPolishFixture();

  const skipped = internals.polish(request, seed, undefined, { maximumTournaments: 0 });
  assert.strictEqual(skipped.plan, seed);
  assert.equal(skipped.tournaments, 0);
  assert.equal(skipped.passes, 0);
  assert.equal(skipped.improvements, 0);
  assert.equal(skipped.fixedPoint, false);
  assert.equal(skipped.cutoff, "tournaments");

  const bounded = internals.polish(request, seed, undefined, { maximumTournaments: 1 });
  assert.equal(bounded.tournaments, 1, "tournament two was never started");
  assert.ok(bounded.passes > 1);
  assert.ok(bounded.improvements > 0);
  assert.ok(compareLayoutQuality(bounded.plan.quality, seed.quality) > 0);
  assert.equal(bounded.fixedPoint, false);
  assert.equal(bounded.cutoff, "tournaments");

  const passBounded = internals.polish(request, seed, undefined, { maximumPasses: 1 });
  assert.equal(passBounded.tournaments, 0, "an incomplete tournament is not charged as complete");
  assert.equal(passBounded.passes, 1);
  assert.equal(passBounded.anchorsTried, 1);
  assert.equal(passBounded.fixedPoint, false);
  assert.equal(passBounded.cutoff, "passes");
});

test("the public repair report truthfully identifies a polish tournament cutoff", () => {
  const { request, standard } = repairFixture();
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 1,
    maxLayouts: 1,
    maxPolishTournaments: 0,
    maxCrossingWork: 0,
  });

  const report = repaired.constraintRepair;
  assert.ok(report);
  assert.equal(report.polishTournaments, 0);
  assert.equal(report.polishPasses, 0);
  assert.equal(report.polishImprovements, 0);
  assert.equal(report.polishCutoff, "tournaments");
  assert.equal(report.geometricFixedPoint, false);
});

test("finite polish skips an expired budget and keeps the last complete adopted plan", () => {
  const { request, seed } = multiAnchorPolishFixture();
  const preExpiredTrace: LayoutTraceEvent[] = [];
  const preExpired = internals.polish(
    request,
    seed,
    (event) => preExpiredTrace.push(event),
    { now: () => 5, deadline: 5 },
  );
  assert.strictEqual(preExpired.plan, seed);
  assert.equal(preExpired.cutoff, "time");
  assert.equal(preExpired.fixedPoint, false);
  assert.equal(preExpired.tournaments, 0);
  assert.equal(preExpired.passes, 0);
  assert.equal(preExpired.improvements, 0);
  assert.equal(preExpiredTrace.length, 1);
  assert.equal(preExpiredTrace[0].type, "constraint-progress");

  let expired = false;
  const cutoffTrace: LayoutTraceEvent[] = [];
  const cutoff = internals.polish(
    request,
    seed,
    (event) => {
      cutoffTrace.push(event);
      // Expire immediately after the first complete improvement callback. The
      // nested planner may still emit synchronous events, so the sentinel must
      // be checked before event filtering and the adopted winner must survive.
      if (event.type === "constraint-improvement") expired = true;
    },
    { now: () => expired ? 1 : 0, deadline: 1 },
  );
  assert.equal(cutoff.cutoff, "time");
  assert.equal(cutoff.fixedPoint, false);
  assert.equal(cutoff.tournaments, 0, "the interrupted tournament never completed");
  assert.ok(cutoff.improvements > 0);
  assert.ok(compareLayoutQuality(cutoff.plan.quality, seed.quality) > 0);
  assert.ok(cutoffTrace.some((event) => event.type === "constraint-improvement"));
});

test("infinite polish keeps the fixed-point path without cooperative cutoff", () => {
  const { request, seed } = multiAnchorPolishFixture();
  const polished = internals.polish(request, seed, undefined, {
    now: () => 0,
    deadline: Number.POSITIVE_INFINITY,
  });
  assert.equal(polished.cutoff, "fixed-point");
  assert.equal(polished.fixedPoint, true);
  assert.ok(polished.tournaments >= 2);
  assert.ok(polished.improvements > 0);
});

test("an already clean layout retains its gain but reports a truncated polish frontier", () => {
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(10, 0)],
    ["c", at(20, 0)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "East" },
    { from: "b", to: "a", direction: "West" },
    { from: "b", to: "c", direction: "East" },
    { from: "c", to: "b", direction: "West" },
  ];
  const request: IntegralLayoutRequest = {
    residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
    nodes: [],
    edges,
    centerId: "b",
    allowExistingMoves: true,
  };
  const standard: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  assert.equal(standard.quality.cardinalRayViolations, 0);
  assert.equal(standard.quality.routingViolations, 0);
  assert.equal(standard.quality.linkCrossings, 0);

  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxLayouts: 1,
  });
  assert.ok(compareLayoutQuality(repaired.quality, standard.quality) > 0);
  assert.equal(repaired.quality.cardinalSlack, 0);
  assert.ok(
    (repaired.constraintRepair?.distinctLayouts ?? 0) >
      (repaired.constraintRepair?.layoutsConsidered ?? 0),
  );
  assert.equal(repaired.constraintRepair?.cutoff, "layouts");
  assert.equal(repaired.constraintRepair?.geometricFixedPoint, false);
  assert.ok((repaired.constraintRepair?.polishPasses ?? 0) > 0);
});

test("deep repair anchors locked residents instead of skipping the area", () => {
  const residents = [
    { id: "a", position: at(0, 0), movable: true },
    { id: "b", position: at(1, 0), movable: true },
    { id: "c", position: at(2, 0), movable: true },
    { id: "locked", position: at(10, 7), movable: false },
  ];
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "East" },
    { from: "b", to: "c", direction: "East" },
    { from: "c", to: "a", direction: "East" },
  ];
  const request: IntegralLayoutRequest = {
    nodes: [],
    residents,
    edges,
    allowExistingMoves: true,
  };
  const standard = planIntegralLayout({ ...request, allowExistingMoves: false });
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: 1_000,
    maxLayouts: 2,
  });

  assert.ok(repaired.constraintRepair, "the constraint stage ran despite the lock");
  assert.deepEqual(repaired.positions.get("locked"), at(10, 7));
  assert.equal(repaired.movedExisting.has("locked"), false);
});

test("deep repair separates two otherwise clean crossing corridors", () => {
  const residents = [
    { id: "west", position: at(-2, 0), movable: true },
    { id: "east", position: at(2, 0), movable: true },
    { id: "north", position: at(0, -2), movable: true },
    { id: "south", position: at(0, 2), movable: true },
  ];
  const edges: LayoutEdge[] = [
    { from: "west", to: "east", direction: "East" },
    { from: "east", to: "west", direction: "West" },
    { from: "north", to: "south", direction: "South" },
    { from: "south", to: "north", direction: "North" },
  ];
  const request: IntegralLayoutRequest = { residents, nodes: [], edges, allowExistingMoves: true };
  const standard = planIntegralLayout({ ...request, allowExistingMoves: false });
  assert.equal(standard.quality.linkCrossings, 1);

  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxLayouts: 8,
  });
  assert.equal(repaired.quality.cardinalRayViolations, 0);
  assert.equal(repaired.quality.routingViolations, 0);
  assert.equal(repaired.quality.linkCrossings, 0);
  assert.equal(repaired.constraintRepair?.selected, true);
});

test("constraint compaction separates unrelated rooms from a protected corridor", () => {
  const residents = [
    { id: "from", position: at(0, 0), movable: true },
    { id: "to", position: at(0, -3), movable: true },
    { id: "block-a", position: at(0, -1), movable: true },
    { id: "block-b", position: at(0, -2), movable: true },
  ];
  const edges: LayoutEdge[] = [
    { from: "from", to: "to", direction: "North" },
    { from: "to", to: "from", direction: "South" },
    { from: "block-a", to: "block-b", direction: "North" },
    { from: "block-b", to: "block-a", direction: "South" },
  ];
  const request: IntegralLayoutRequest = {
    residents,
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const standard = planIntegralLayout({ ...request, allowExistingMoves: false });
  assert.deepEqual(measureLayoutRoutingQuality(standard.positions, edges), {
    routingViolations: 4,
    exitPortViolations: 2,
    reciprocalExitPortViolations: 2,
    roomObstructions: 2,
  });

  const repaired = repairIntegralLayoutConstraints(
    request,
    standard,
    { when: "always", maxDurationMs: 1_000 },
  );
  assert.equal(repaired.constraintRepair?.selected, true);
  assert.equal(repaired.quality.cardinalRayViolations, 0);
  assert.equal(repaired.quality.routingViolations, 0);
  assert.equal(repaired.quality.exitPortViolations, 0);
  assert.equal(repaired.quality.roomObstructions, 0);
});

test("compaction pulls a dangling source flush against its retained relation", () => {
  // Longest-path ranks alone would leave `u` at rank zero of row one — its
  // East relation into `v` carrying two cells of slack — because `v`'s column
  // rank is driven by the unrelated `p` chain in row zero. The per-axis raise
  // pass moves `u` to its tightest outgoing bound instead, and row one is
  // empty there, so the very first built state is already slack-free.
  const positions = new Map<string, GridPosition>([
    ["p0", at(0, 0)],
    ["p1", at(1, 0)],
    ["p2", at(2, 0)],
    ["w", at(3, 0)],
    ["v", at(3, 1)],
    ["u", at(0, 1)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "p0", to: "p1", direction: "East" },
    { from: "p1", to: "p2", direction: "East" },
    { from: "p2", to: "w", direction: "East" },
    { from: "w", to: "v", direction: "South" },
    { from: "u", to: "v", direction: "East" },
  ];
  const compacted = internals.compact(positions, edges);
  assert.equal(compacted.ok, true);
  assert.ok(compacted.ok);
  assert.deepEqual(compacted.status, { completed: true, cancelled: false, exhausted: false });
  assert.equal(compacted.workStats.separatorStates, 1, "the first state needs no separator");
  assert.equal(compacted.quality.cardinalSlack, 0);
  const u = compacted.positions.get("u") as GridPosition;
  const v = compacted.positions.get("v") as GridPosition;
  assert.equal(u.x, v.x - 1, "u sits flush against its only retained relation");
  assert.equal(u.y, v.y);
});

test("packed constraint occupancy keys are collision-equivalent to coordinate triples", () => {
  let state = 0xc0111de;
  const random = (): number => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
  const samples: [number, number, number][] = [
    [0, 0, 0],
    [-(1 << 20), -(1 << 20), -(1 << 9)],
    [(1 << 20) - 1, (1 << 20) - 1, (1 << 9) - 1],
    [1 << 20, 0, 0],
    [0, -(1 << 20) - 1, 0],
    [0, 0, 1 << 9],
    [0.5, 0, 0],
  ];
  for (let index = 0; index < 5_000; index += 1) {
    const existing = samples[Math.floor(random() * samples.length)];
    samples.push(random() < 0.2
      ? [existing[0], existing[1], existing[2]]
      : [
        Math.floor(random() * (1 << 23)) - (1 << 22),
        Math.floor(random() * (1 << 23)) - (1 << 22),
        Math.floor(random() * (1 << 12)) - (1 << 11),
      ]);
  }
  const seen = new Map<number | string, readonly [number, number, number]>();
  for (const sample of samples) {
    const key = internals.packedCellKey(sample[0], sample[1], sample[2]);
    const previous = seen.get(key);
    if (previous) assert.deepEqual(sample, previous);
    else seen.set(key, sample);
  }
});

test("randomized spatial defect indexes preserve brute-force ordering", () => {
  let state = 0x5a71a1;
  const random = (): number => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
  for (let example = 0; example < 12; example += 1) {
    const positions = new Map<string, GridPosition>();
    const edges: LayoutEdge[] = [];
    const horizontalY = [-7, -3, 2, 6];
    const verticalX = [-6, -1, 3, 7];
    for (let index = 0; index < horizontalY.length; index += 1) {
      const left = `h${index}-left`;
      const right = `h${index}-right`;
      positions.set(left, at(-11 - example, horizontalY[index]));
      positions.set(right, at(11 + example, horizontalY[index]));
      edges.push({ from: left, to: right, direction: "East" });
    }
    for (let index = 0; index < verticalX.length; index += 1) {
      const top = `v${index}-top`;
      const bottom = `v${index}-bottom`;
      positions.set(top, at(verticalX[index], -12 - example));
      positions.set(bottom, at(verticalX[index], 12 + example));
      edges.push({ from: top, to: bottom, direction: "South" });
    }
    // Isolated rooms exercise row/column range queries without changing the
    // protected graph. Keep cells unique while varying port/interior order.
    const occupied = new Set([...positions.values()].map(({ x, y, level }) => `${x}:${y}:${level}`));
    for (let blocker = 0; blocker < 10; blocker += 1) {
      const onHorizontal = blocker % 2 === 0;
      let x: number;
      let y: number;
      do {
        x = onHorizontal
          ? Math.floor(random() * 19) - 9
          : verticalX[Math.floor(random() * verticalX.length)];
        y = onHorizontal
          ? horizontalY[Math.floor(random() * horizontalY.length)]
          : Math.floor(random() * 21) - 10;
      } while (occupied.has(`${x}:${y}:0`));
      occupied.add(`${x}:${y}:0`);
      positions.set(`block-${blocker}`, at(x, y));
    }
    const compacted = internals.compact(positions, edges, {
      maximumStates: 32,
      verifySpatialIndexes: true,
    });
    assert.equal(compacted.ok, true, `example ${example}`);
    if (compacted.ok) {
      const verified = compacted.spatialVerification;
      assert.ok(verified && verified.obstructionQueries > 0, `example ${example}: obstruction`);
      assert.ok(verified.crossingStates > 0, `example ${example}: crossing state`);
      assert.ok(
        (compacted.workStats.candidateMaterializations ?? 0) <=
          compacted.incumbents.length + 1,
      );
    }

    const offsetX = Math.floor(random() * 21) - 10;
    const offsetY = Math.floor(random() * 21) - 10;
    const horizontal = example % 2 === 0;
    const corridor = new Map<string, GridPosition>(horizontal
      ? [
        ["from", at(offsetX, offsetY)],
        ["to", at(offsetX + 3, offsetY)],
        ["block-a", at(offsetX + 1, offsetY)],
        ["block-b", at(offsetX + 2, offsetY)],
      ]
      : [
        ["from", at(offsetX, offsetY)],
        ["to", at(offsetX, offsetY - 3)],
        ["block-a", at(offsetX, offsetY - 1)],
        ["block-b", at(offsetX, offsetY - 2)],
      ]);
    const corridorEdges: LayoutEdge[] = horizontal
      ? [
        { from: "from", to: "to", direction: "East" },
        { from: "to", to: "from", direction: "West" },
        { from: "block-a", to: "block-b", direction: "East" },
        { from: "block-b", to: "block-a", direction: "West" },
      ]
      : [
        { from: "from", to: "to", direction: "North" },
        { from: "to", to: "from", direction: "South" },
        { from: "block-a", to: "block-b", direction: "North" },
        { from: "block-b", to: "block-a", direction: "South" },
      ];
    const obstructed = internals.compact(corridor, corridorEdges, {
      maximumStates: 64,
      verifySpatialIndexes: true,
    });
    assert.equal(obstructed.ok, true, `example ${example}: corridor`);
    assert.ok(
      obstructed.spatialVerification && obstructed.spatialVerification.obstructionHits > 0,
      `example ${example}: non-empty obstruction oracle`,
    );
  }
});

test("crossing sweep matches brute force at random interiors and tied endpoints", () => {
  let state = 0xc20551;
  const random = (): number => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
  let crossings = 0;
  for (let example = 0; example < 40; example += 1) {
    const positions: [number, number, number][] = [
      [-5, 0, 0], [5, 0, 0], [0, -5, 0], [0, 5, 0],
      [-5, -4, 0], [-5, 4, 0], [-4, 0, 0], [-4, 4, 0],
    ];
    const edges: { from: number; to: number; axis: 0 | 1 }[] = [
      { from: 0, to: 1, axis: 0 },
      { from: 2, to: 3, axis: 1 }, // strict interior crossing
      { from: 4, to: 5, axis: 1 }, // coordinate tie at horizontal endpoint
      { from: 6, to: 7, axis: 1 }, // vertical endpoint on horizontal interior
      { from: 0, to: 5, axis: 1 }, // shared endpoint is never a crossing
    ];
    for (let edge = 0; edge < 35; edge += 1) {
      const axis = random() < 0.5 ? 0 : 1;
      const level = Math.floor(random() * 3) - 1;
      const fixed = Math.floor(random() * 17) - 8;
      let first = Math.floor(random() * 19) - 9;
      let second = Math.floor(random() * 19) - 9;
      if (first === second) second += 1;
      const from = positions.length;
      positions.push(axis === 0 ? [first, fixed, level] : [fixed, first, level]);
      positions.push(axis === 0 ? [second, fixed, level] : [fixed, second, level]);
      edges.push({ from, to: from + 1, axis });
    }
    const expected = Array.from({ length: edges.length }, () => [] as number[]);
    for (let first = 0; first < edges.length; first += 1) {
      for (let second = first + 1; second < edges.length; second += 1) {
        const a = edges[first];
        const b = edges[second];
        if (a.axis === b.axis || a.from === b.from || a.from === b.to ||
          a.to === b.from || a.to === b.to) continue;
        const horizontal = a.axis === 0 ? a : b;
        const vertical = a.axis === 1 ? a : b;
        const hFrom = positions[horizontal.from];
        const hTo = positions[horizontal.to];
        const vFrom = positions[vertical.from];
        const vTo = positions[vertical.to];
        if (hFrom[2] !== hTo[2] || hFrom[2] !== vFrom[2] || hFrom[2] !== vTo[2]) continue;
        if (vFrom[0] > Math.min(hFrom[0], hTo[0]) &&
          vFrom[0] < Math.max(hFrom[0], hTo[0]) &&
          hFrom[1] > Math.min(vFrom[1], vTo[1]) &&
          hFrom[1] < Math.max(vFrom[1], vTo[1])) expected[first].push(second);
      }
    }
    const actual = internals.crossingPairs(positions, edges);
    assert.deepEqual(actual, expected, `example ${example}`);
    crossings += actual.reduce((total, values) => total + values.length, 0);
  }
  assert.ok(crossings > 40, "the oracle exercised non-empty crossing sets");
});

test("compaction cancelled on any inspected state never reports a completed traversal", () => {
  const positions = new Map<string, GridPosition>([
    ["from", at(0, 0)],
    ["to", at(0, -3)],
    ["block-a", at(0, -1)],
    ["block-b", at(0, -2)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "from", to: "to", direction: "North" },
    { from: "to", to: "from", direction: "South" },
    { from: "block-a", to: "block-b", direction: "North" },
    { from: "block-b", to: "block-a", direction: "South" },
  ];
  const reference = internals.compact(positions, edges);
  assert.equal(reference.ok, true);
  assert.deepEqual(reference.status, { completed: true, cancelled: false, exhausted: false });
  const total = reference.incumbents.length;
  assert.ok(total >= 3, "the corridor traversal publishes several incumbents");

  // The first incumbent precedes the root frame (supplied geometry); the
  // second is published while inspecting the first candidate state; the last
  // while inspecting the final candidate state of the deterministic traversal.
  for (const cancelAtIncumbent of [1, 2, total]) {
    const cancelled = internals.compact(positions, edges, { cancelAtIncumbent });
    assert.deepEqual(
      cancelled.status,
      { completed: false, cancelled: true, exhausted: false },
      `cancellation latched at incumbent ${cancelAtIncumbent}`,
    );
    assert.equal(cancelled.incumbents.length, cancelAtIncumbent);
  }
});

test("every cancellation observation point terminates the compaction as cancelled", () => {
  const { request } = softSeparatorFixture();
  const positions = new Map(request.residents.map(({ id, position }) => [id, position]));
  let observations = 0;
  const reference = internals.compact(positions, request.edges, {
    shouldCancel: () => {
      observations += 1;
      return false;
    },
  });
  assert.equal(reference.ok, true);
  assert.deepEqual(reference.status, { completed: true, cancelled: false, exhausted: false });
  assert.ok(observations > 0);

  // A single-observation spike models the worst cooperative-cancellation
  // timing. Whichever sampled check consumes it — coordinate construction,
  // separator admission, a defect scan, or a core frame boundary — the run
  // must terminate as cancelled instead of completing around the cut.
  for (let target = 1; target <= observations; target += 1) {
    let calls = 0;
    const spiked = internals.compact(positions, request.edges, {
      shouldCancel: () => ++calls === target,
    });
    assert.deepEqual(
      spiked.status,
      { completed: false, cancelled: true, exhausted: false },
      `observation ${target} of ${observations}`,
    );
    assert.ok(spiked.incumbents.length <= reference.incumbents.length);
  }
});

test("a repair deadline cutting the extension traversal reports cancellation, never proof", () => {
  const { request, standard } = softSeparatorFixture();
  const options = {
    when: "always" as const,
    maxRestarts: 4,
    maxLayouts: 2,
    maxExtensionStates: 64,
    maxMaskDiversifications: 4,
    maxCrossingWork: 0,
  };
  // A tick-per-call clock lands the deadline on a different deterministic
  // observation point for every budget, covering cuts inside coordinate
  // construction, separator admission, and the first and final DFS frames.
  let cancelledTraversals = 0;
  for (let budget = 2; budget <= 120; budget += 1) {
    let tick = 0;
    const repaired = internals.repairWithClock(
      request,
      standard,
      { ...options, maxDurationMs: budget },
      () => tick++,
    );
    const report = repaired.constraintRepair;
    if (!report) continue;
    if (report.extensionSearch.cancelled) {
      cancelledTraversals += 1;
      assert.equal(report.extensionSearch.completed, false, `budget ${budget}`);
      assert.equal(report.maskDiversification.completed, false, `budget ${budget}`);
      assert.equal(report.cutoff, "time", `budget ${budget}`);
    }
    if (report.cutoff === "time" || report.extensionSearch.cancelled) {
      assert.equal(report.geometricFixedPoint, false, `budget ${budget}`);
    }
  }
  assert.ok(cancelledTraversals > 0, "some budget cut the extension traversal itself");
});

test("protocol validation rejects a fixed point claimed over a cut traversal", () => {
  const { request, standard } = repairFixture();
  const trace: LayoutTraceEvent[] = [];
  repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: Number.POSITIVE_INFINITY,
    maxLayouts: 5,
  }, (event) => trace.push(event));
  const reportEvent = trace.find(
    (event): event is Extract<LayoutTraceEvent, { type: "constraint-repair" }> =>
      event.type === "constraint-repair",
  );
  assert.ok(reportEvent);
  assert.equal(reportEvent.report.geometricFixedPoint, true);

  const message = (report: unknown): unknown => ({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 1,
    operation: "constraint-repair",
    progress: true,
    event: { type: "constraint-repair", stage: "constraint-repair", report },
  });
  assert.equal(isLayoutWorkerProgress(message(reportEvent.report)), true);
  assert.equal(isLayoutWorkerProgress(message({
    ...reportEvent.report,
    extensionSearch: { completed: false, cancelled: true, exhausted: false },
  })), false, "a cancelled extension frontier cannot carry a fixed point");
  assert.equal(isLayoutWorkerProgress(message({
    ...reportEvent.report,
    extensionSearch: { completed: true, cancelled: true, exhausted: false },
  })), false, "a completed-yet-cancelled frontier is contradictory");
  assert.equal(isLayoutWorkerProgress(message({
    ...reportEvent.report,
    cutoff: "time",
  })), false, "a deadline-cut run cannot carry a fixed point");
});

test("a bounded soft-separator search publishes and returns its hard-valid incumbent", () => {
  const { request, standard } = softSeparatorFixture();
  const { residents, edges } = request;
  const trace: LayoutTraceEvent[] = [];
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxLayouts: 1,
    maxPolishTournaments: 0,
    maxExtensionStates: 2,
    maxMaskDiversifications: 1,
    maxCrossingWork: 0,
  }, (event) => trace.push(event));

  const report = repaired.constraintRepair;
  assert.ok(report, "a hard-valid state survives the exhausted soft search");
  assert.equal(report.separatorStates, 2);
  assert.equal(report.rawIncumbents, 2, "supplied and first compacted hard-valid states are scored");
  assert.equal(report.distinctLayouts, 2);
  assert.equal(report.softIncumbents, 1);
  assert.equal(report.cutoff, "extensions");
  assert.deepEqual(report.extensionSearch, {
    completed: false,
    cancelled: false,
    exhausted: true,
  });
  assert.equal(report.geometricFixedPoint, false);
  assert.ok(Number.isFinite(report.firstIncumbentMs));
  assert.ok(compareLayoutQuality(repaired.quality, standard.quality) > 0);

  const improvements = trace.filter((event) => event.type === "constraint-improvement");
  assert.ok(improvements.length >= 1);
  assert.deepEqual({
    states: improvements[0].separatorStates,
    branches: improvements[0].separatorBranches,
    cyclePrunes: improvements[0].separatorCyclePrunes,
  }, { states: 2, branches: 1, cyclePrunes: 0 });
  let frontier = standard.quality;
  for (const event of improvements) {
    assert.ok(compareLayoutQuality(event.candidate.quality, frontier) > 0);
    const positions = new Map(event.candidate.positions?.map(({ id, x, y, level }) => [
      id,
      { x, y, level },
    ]));
    assert.equal(positions.size, residents.length);
    assert.equal(new Set([...positions.values()].map((position) =>
      `${position.level}:${position.x}:${position.y}`
    )).size, residents.length);
    assert.equal(directionalViolationEdges(positions, edges).length, 0);
    assert.deepEqual(measureIntegralLayoutQuality(positions, edges), event.candidate.quality);
    frontier = event.candidate.quality;
  }
  assert.ok(
    compareLayoutQuality(repaired.quality, frontier) >= 0,
    "the final plan cannot retract a published hard-valid incumbent",
  );
});

test("an exhausted first geometry mask cannot starve MaxHS certification", () => {
  const soft = softSeparatorFixture();
  const positions = new Map(soft.standard.positions);
  positions.set("cycle-a", at(10, 0));
  positions.set("cycle-b", at(11, 0));
  positions.set("cycle-c", at(12, 0));
  const edges: LayoutEdge[] = [
    ...soft.request.edges,
    { from: "cycle-a", to: "cycle-b", direction: "East" },
    { from: "cycle-b", to: "cycle-c", direction: "East" },
    { from: "cycle-c", to: "cycle-a", direction: "East" },
  ];
  const request: IntegralLayoutRequest = {
    residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const standard: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 8,
    maxLayouts: 1,
    maxPolishTournaments: 0,
    maxExtensionStates: 2,
    maxMaskDiversifications: 4,
    maxCrossingWork: 0,
  });

  const report = repaired.constraintRepair;
  assert.ok(report);
  assert.ok(report.feasibilityChecks > 1);
  assert.equal(report.lowerBound, 1);
  assert.equal(report.constraintOptimal, true);
  assert.equal(report.cutoff, "extensions");
  assert.equal(report.separatorStates, 2);
});

test("unrestricted polish closes its derived mask before reporting no cutoff", () => {
  const trace: LayoutTraceEvent[] = [];
  const positions = new Map<string, GridPosition>([
    ["r0", at(1, -1)],
    ["r1", at(-2, -5)],
    ["r2", at(1, -5)],
    ["r3", at(-2, 5)],
    ["r4", at(0, 0)],
    ["r5", at(2, 0)],
    ["r6", at(1, -3)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "r0", to: "r1", direction: "East" },
    { from: "r1", to: "r0", direction: "West" },
    { from: "r1", to: "r2", direction: "North" },
    { from: "r0", to: "r3", direction: "East" },
    { from: "r3", to: "r0", direction: "West" },
    { from: "r1", to: "r4", direction: "North" },
    { from: "r4", to: "r1", direction: "South" },
    { from: "r2", to: "r5", direction: "South" },
    { from: "r5", to: "r2", direction: "North" },
    { from: "r4", to: "r6", direction: "East" },
    { from: "r4", to: "r0", direction: "East" },
    { from: "r0", to: "r5", direction: "North" },
    { from: "r5", to: "r6", direction: "North" },
  ];
  const request: IntegralLayoutRequest = {
    residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const standard: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  const repaired = repairIntegralLayoutConstraints(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 8,
    maxLayouts: Number.POSITIVE_INFINITY,
    maxPolishTournaments: 3,
    maxExtensionStates: 500,
    maxMaskDiversifications: 32,
    maxCrossingWork: 100,
  }, (event) => trace.push(event));

  const report = repaired.constraintRepair;
  assert.ok(report);
  assert.ok(
    report.cutoff !== "none" || report.extensionSearch.completed,
    "a no-cutoff report cannot leave the polish-derived mask unprocessed",
  );
  assert.ok(
    report.extensionSearch.completed || report.extensionSearch.cancelled ||
      report.extensionSearch.exhausted || report.cutoff !== "none",
  );
  assert.equal(
    report.layoutsConsidered,
    report.distinctLayouts,
    "an unbounded layout frontier polishes layouts discovered during mask closure",
  );
  const progressCounts = trace.flatMap((event) =>
    event.type === "constraint-progress" || event.type === "constraint-improvement"
      ? [event.layoutsConsidered]
      : []
  );
  for (let index = 1; index < progressCounts.length; index += 1) {
    assert.ok(
      progressCounts[index] >= progressCounts[index - 1],
      `operation-wide layout work regressed ${progressCounts[index - 1]} -> ${progressCounts[index]}`,
    );
  }
});

test("planar constraint repair never invents levels in bounded progressive or final candidates", () => {
  const positions = new Map<string, GridPosition>([
    ["r0", at(0, 2)],
    ["r1", at(0, 3)],
    ["r2", at(1, 0)],
    ["r3", at(1, 2)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "r2", to: "r1", direction: "East" },
    { from: "r0", to: "r1", direction: "East" },
    { from: "r1", to: "r2", direction: "North" },
  ];
  const request: IntegralLayoutRequest = {
    residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const standard: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };

  for (const maxExtensionStates of [60, 128]) {
    const trace: LayoutTraceEvent[] = [];
    const repaired = repairIntegralLayoutConstraints(request, standard, {
      when: "always",
      maxDurationMs: Number.POSITIVE_INFINITY,
      maxRestarts: 8,
      maxLayouts: 1,
      maxPolishTournaments: 0,
      maxExtensionStates,
      maxMaskDiversifications: 8,
      maxCrossingWork: 0,
    }, (event) => trace.push(event));
    const improvements = trace.filter(
      (event): event is Extract<LayoutTraceEvent, { type: "constraint-improvement" }> =>
        event.type === "constraint-improvement",
    );
    assert.ok(improvements.length > 0, `${maxExtensionStates}: expected progressive candidates`);
    for (const [index, improvement] of improvements.entries()) {
      assert.ok(
        improvement.candidate.positions?.every((position) => position.level === 0),
        `${maxExtensionStates}: progressive candidate ${index} left the only authoritative level`,
      );
    }
    assert.ok(
      [...repaired.positions.values()].every((position) => position.level === 0),
      `${maxExtensionStates}: final candidate left the only authoritative level`,
    );
  }
});

test("mixed-level planar-only repairs preserve every input level", () => {
  for (let example = 0; example < 12; example += 1) {
    const activeLevel = example % 5 - 2;
    const quietLevel = 7 - example % 3;
    const isolatedLevel = example - 9;
    const positions = new Map<string, GridPosition>([
      ["from", at(0, 0, activeLevel)],
      ["to", at(0, -3, activeLevel)],
      ["block-a", at(0, -1, activeLevel)],
      ["block-b", at(0, -2, activeLevel)],
      ["quiet-a", at(10, 0, quietLevel)],
      ["quiet-b", at(11, 0, quietLevel)],
      ["isolated", at(20, 20, isolatedLevel)],
    ]);
    const edges: LayoutEdge[] = [
      { from: "from", to: "to", direction: "North" },
      { from: "to", to: "from", direction: "South" },
      { from: "block-a", to: "block-b", direction: "North" },
      { from: "block-b", to: "block-a", direction: "South" },
      { from: "quiet-a", to: "quiet-b", direction: "East" },
    ];
    const request: IntegralLayoutRequest = {
      residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
      nodes: [],
      edges,
      allowExistingMoves: true,
    };
    const trace: LayoutTraceEvent[] = [];
    const repaired = repairIntegralLayoutConstraints(request, {
      positions,
      movedExisting: new Set(),
      quality: measureIntegralLayoutQuality(positions, edges),
    }, {
      when: "always",
      maxDurationMs: Number.POSITIVE_INFINITY,
      maxRestarts: 8,
      maxLayouts: 1,
      maxPolishTournaments: 0,
      maxExtensionStates: example % 2 === 0 ? 60 : 128,
      maxMaskDiversifications: 8,
      maxCrossingWork: 0,
    }, (event) => trace.push(event));
    const expected = new Map([...positions].map(([id, position]) => [id, position.level]));
    const assertInputLevels = (
      actual: Iterable<readonly [string, GridPosition]>,
      label: string,
    ): void => {
      for (const [id, position] of actual) {
        assert.equal(position.level, expected.get(id), `${example}: ${label} changed ${id}`);
      }
    };
    const improvements = trace.filter(
      (event): event is Extract<LayoutTraceEvent, { type: "constraint-improvement" }> =>
        event.type === "constraint-improvement",
    );
    assert.ok(improvements.length > 0, `${example}: expected a progressive candidate`);
    for (const [index, improvement] of improvements.entries()) {
      assertInputLevels(
        improvement.candidate.positions?.map(({ id, x, y, level }) =>
          [id, { x, y, level }] as const
        ) ?? [],
        `progressive candidate ${index}`,
      );
    }
    assertInputLevels(repaired.positions, "final candidate");
  }
});

test("a planar edge across input levels is repaired onto one level", () => {
  // No Up/Down exit holds the two floors apart, so the slope's level relation,
  // given up by the plan, is restored: the rooms join one level and the East
  // exit is drawn right.
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(1, 0, 3)],
  ]);
  const edges: LayoutEdge[] = [{ from: "a", to: "b", direction: "East" }];
  const request: IntegralLayoutRequest = {
    residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const repaired = repairIntegralLayoutConstraints(request, {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  }, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 8,
    maxLayouts: 1,
    maxPolishTournaments: 0,
    maxExtensionStates: 128,
    maxMaskDiversifications: 8,
    maxCrossingWork: 0,
  });

  assert.equal(repaired.constraintRepair?.outcome, "searched");
  assert.equal(repaired.constraintRepair?.relaxedLevelRelations, 0);
  assert.equal(repaired.quality.cardinalRayViolations, 0);
  const a = repaired.positions.get("a") as GridPosition;
  const b = repaired.positions.get("b") as GridPosition;
  assert.equal(a.level, b.level);
  assert.ok(b.x > a.x && b.y === a.y, "b lies east of a");
});

test("constraint repair preserves legitimate Up/Down stacks and their planar components", () => {
  const positions = new Map<string, GridPosition>([
    ["lower", at(0, 0)],
    ["lower-east", at(1, 0)],
    ["upper", at(0, 0, 1)],
    ["upper-east", at(1, 0, 1)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "lower", to: "upper", direction: "Up" },
    { from: "upper", to: "lower", direction: "Down" },
    { from: "lower", to: "lower-east", direction: "East" },
    { from: "lower-east", to: "lower", direction: "West" },
    { from: "upper", to: "upper-east", direction: "East" },
    { from: "upper-east", to: "upper", direction: "West" },
  ];
  const request: IntegralLayoutRequest = {
    residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const repaired = repairIntegralLayoutConstraints(request, {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  }, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 8,
    maxLayouts: 1,
    maxPolishTournaments: 0,
    maxExtensionStates: 64,
    maxMaskDiversifications: 8,
    maxCrossingWork: 0,
  });

  const lower = repaired.positions.get("lower") as GridPosition;
  const lowerEast = repaired.positions.get("lower-east") as GridPosition;
  const upper = repaired.positions.get("upper") as GridPosition;
  const upperEast = repaired.positions.get("upper-east") as GridPosition;
  assert.equal(upper.level, lower.level + 1);
  assert.equal(lowerEast.level, lower.level);
  assert.equal(upperEast.level, upper.level);
});

test("gravity compacts each strict raw improvement, and a rejected one changes nothing", () => {
  const { request, standard } = softSeparatorFixture();
  const options = {
    when: "always" as const,
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 1,
    maxLayouts: 1,
    maxPolishTournaments: 0,
    maxExtensionStates: 2,
    maxMaskDiversifications: 1,
    maxCrossingWork: 0,
  };
  const run = (mode: "identity" | "invalid" | "throw") => {
    let calls = 0;
    const trace: LayoutTraceEvent[] = [];
    const result = internals.repairWithGravity(request, standard, options, (
      _request,
      seed,
      control,
    ) => {
      calls += 1;
      assert.equal(control?.acceptsPositions?.(seed.positions), true);
      if (mode === "throw") throw new Error("synthetic gravity failure");
      if (mode === "invalid") {
        return {
          ...seed,
          positions: new Map([["from", at(0, 0)]]),
        };
      }
      return seed;
    }, (event) => trace.push(event), () => 0);
    return { result, calls, trace };
  };

  const identity = run("identity");
  assert.ok(identity.result.constraintRepair);
  assert.ok(identity.result.constraintRepair.rawIncumbents > identity.calls);
  assert.equal(identity.calls, identity.result.constraintRepair.softIncumbents);
  assert.ok(identity.calls > 0);

  for (const mode of ["invalid", "throw"] as const) {
    const fallback = run(mode);
    assert.equal(fallback.calls, identity.calls);
    assert.deepEqual(fallback.result.positions, identity.result.positions);
    assert.deepEqual(fallback.result.quality, identity.result.quality);
  }
});

test("a raw incumbent whose compaction fails is still published finished", () => {
  // The soft-separator rooms stored far apart, with a branch to one side, so
  // the first hard-valid incumbent spans empty rows a vacuum removes.
  const residents = [
    { id: "from", position: at(0, 0), movable: true },
    { id: "to", position: at(0, -9), movable: true },
    { id: "block-a", position: at(0, -3), movable: true },
    { id: "block-b", position: at(0, -6), movable: true },
    { id: "side", position: at(6, -3), movable: true },
  ];
  const edges: LayoutEdge[] = [
    { from: "from", to: "to", direction: "North" },
    { from: "to", to: "from", direction: "South" },
    { from: "block-a", to: "block-b", direction: "North" },
    { from: "block-b", to: "block-a", direction: "South" },
    { from: "block-a", to: "side", direction: "East" },
  ];
  const request: IntegralLayoutRequest = { residents, nodes: [], edges, allowExistingMoves: true };
  const standard = planIntegralLayout({ ...request, allowExistingMoves: false });
  const trace: LayoutTraceEvent[] = [];
  const repaired = internals.repairWithGravity(request, standard, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 1,
    maxLayouts: 1,
    maxPolishTournaments: 0,
    maxExtensionStates: 2,
    maxMaskDiversifications: 1,
    maxCrossingWork: 0,
  }, () => {
    throw new Error("synthetic gravity failure");
  }, (event) => trace.push(event));

  const improvements = trace.filter(
    (event): event is Extract<LayoutTraceEvent, { type: "constraint-improvement" }> =>
      event.type === "constraint-improvement",
  );
  assert.ok(improvements.length > 0);
  for (const [index, improvement] of improvements.entries()) {
    const positions = new Map(improvement.candidate.positions?.map(({ id, x, y, level }) => [
      id,
      { x, y, level },
    ]));
    const plan: IntegralLayoutPlan = {
      positions,
      movedExisting: new Set(),
      quality: measureIntegralLayoutQuality(positions, edges),
    };
    assert.equal(
      compactIntegralLayoutPlan(request, plan, { axisGroupCompaction: false }),
      plan,
      `improvement ${index} is not at the cheap compaction fixed point`,
    );
    assert.ok(compareLayoutQuality(repaired.quality, plan.quality) >= 0);
  }
});

test("soft defects enqueue deterministic equal-primary canonical mask swaps", () => {
  const positions = new Map<string, GridPosition>([
    ["a", at(-2, 0)],
    ["b", at(0, 0)],
    ["c", at(2, 0)],
    ["d", at(0, -2)],
    ["e", at(0, 1)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "East" },
    { from: "b", to: "c", direction: "East" },
    { from: "c", to: "a", direction: "East" },
    { from: "d", to: "e", direction: "South" },
  ];
  const request: IntegralLayoutRequest = {
    residents: [...positions].map(([id, position]) => ({ id, position, movable: true })),
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const standard: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  const searched = internals.search(positions, edges, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: Number.POSITIVE_INFINITY,
    maxMaskDiversifications: 12,
  });
  assert.equal(searched.ok, true);
  assert.equal(searched.ok && searched.masks.length, 1, "master search has only one seed mask");

  const run = (maxMaskDiversifications = 12) => repairIntegralLayoutConstraints(request, standard, {
    when: "always" as const,
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: Number.POSITIVE_INFINITY,
    maxLayouts: 1,
    maxExtensionStates: 100,
    maxMaskDiversifications,
    maxCrossingWork: 0,
  });
  const first = run();
  const second = run();
  const capped = run(2);
  assert.ok((first.constraintRepair?.maskDiversifications ?? 0) > 1);
  assert.equal(capped.constraintRepair?.maskDiversifications, 2);
  assert.equal(capped.constraintRepair?.cutoff, "masks");
  assert.deepEqual(capped.constraintRepair?.maskDiversification, {
    completed: false,
    exhausted: true,
  });
  assert.equal(capped.constraintRepair?.geometricFixedPoint, false);
  assert.equal(
    first.constraintRepair?.maskDiversifications,
    first.constraintRepair?.compactionAttempts,
  );
  assert.equal(first.constraintRepair?.relaxedEdges, 1, "every queued swap keeps primary weight");
  assert.equal(first.constraintRepair?.reciprocalRelaxedEdges, 0);
  const deterministicStats = (plan: IntegralLayoutPlan) => {
    const report = plan.constraintRepair;
    return report && {
      rawIncumbents: report.rawIncumbents,
      softIncumbents: report.softIncumbents,
      distinctLayouts: report.distinctLayouts,
      maskDiversifications: report.maskDiversifications,
      separatorStates: report.separatorStates,
      separatorBranches: report.separatorBranches,
      separatorCyclePrunes: report.separatorCyclePrunes,
      compactionAttempts: report.compactionAttempts,
      relaxedEdges: report.relaxedEdges,
    };
  };
  assert.deepEqual(deterministicStats(first), deterministicStats(second));
  assert.deepEqual(first.quality, second.quality);
  assert.deepEqual([...first.positions], [...second.positions]);
});

test("forced hash collisions retain exact soft defects and conservatively withhold proof", () => {
  const { request, standard } = softSeparatorFixture();
  const options: ConstraintRepairOptions = {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 64,
    maxLayouts: Number.POSITIVE_INFINITY,
    maxPolishTournaments: Number.POSITIVE_INFINITY,
    maxPolishPasses: Number.POSITIVE_INFINITY,
    maxExtensionStates: Number.POSITIVE_INFINITY,
    maxMaskDiversifications: Number.POSITIVE_INFINITY,
    maxCrossingWork: Number.POSITIVE_INFINITY,
  };
  const ordinary = repairIntegralLayoutConstraints(request, standard, options);
  const collided = internals.repairWithHashCollisions(request, standard, options);

  assert.equal(ordinary.constraintRepair?.geometricFixedPoint, true);
  assert.equal(ordinary.constraintRepair?.cutoff, "none");
  assert.deepEqual(collided.positions, ordinary.positions);
  assert.deepEqual(collided.quality, ordinary.quality);
  assert.equal(collided.constraintRepair?.extensionSearch.completed, true);
  assert.equal(collided.constraintRepair?.geometricFixedPoint, false);
  assert.equal(collided.constraintRepair?.cutoff, "layouts");
});

test("re-encountered geometries are suppressed without duplicating published improvements", () => {
  const { request, standard } = softSeparatorFixture();
  const run = () => {
    const trace: LayoutTraceEvent[] = [];
    const plan = repairIntegralLayoutConstraints(request, standard, {
      when: "always",
      maxDurationMs: Number.POSITIVE_INFINITY,
      maxRestarts: Number.POSITIVE_INFINITY,
      maxLayouts: 8,
      maxMaskDiversifications: 12,
      maxCrossingWork: 0,
    }, (event) => trace.push(event));
    const improvements = trace
      .filter((event): event is Extract<LayoutTraceEvent, { type: "constraint-improvement" }> =>
        event.type === "constraint-improvement")
      .map((event) => JSON.stringify(event.candidate));
    return { plan, improvements };
  };
  const first = run();
  const second = run();
  const report = first.plan.constraintRepair;
  assert.ok(report);
  assert.ok(
    report.rawIncumbents > report.distinctLayouts,
    "at least one re-encountered geometry was suppressed by the signature window",
  );
  assert.ok(first.improvements.length > 0);
  assert.equal(
    new Set(first.improvements).size,
    first.improvements.length,
    "no improvement is published twice",
  );
  assert.deepEqual(second.improvements, first.improvements);
  const counters = (plan: IntegralLayoutPlan) => {
    const value = plan.constraintRepair;
    return value && {
      rawIncumbents: value.rawIncumbents,
      softIncumbents: value.softIncumbents,
      distinctLayouts: value.distinctLayouts,
      layoutsConsidered: value.layoutsConsidered,
    };
  };
  assert.deepEqual(counters(second.plan), counters(first.plan));
  assert.deepEqual(second.plan.quality, first.plan.quality);
  assert.deepEqual([...second.plan.positions], [...first.plan.positions]);
});

test("deadline and deterministic work cutoffs interrupt lower-bound work", () => {
  const { standard, request } = repairFixture();
  let tick = 0;
  const timed = internals.search(
    standard.positions,
    request.edges,
    { when: "always", maxDurationMs: 2, maxRestarts: 10 },
    { now: () => tick++ },
  );
  assert.equal(timed.ok, true);
  assert.equal(timed.ok && timed.cutoff, "time");
  assert.equal(timed.ok && timed.feasibilityChecks, 1);
  assert.equal(timed.ok && timed.optimal, false);

  const workBounded = internals.search(
    standard.positions,
    request.edges,
    { when: "always", maxDurationMs: 1_000, maxRestarts: 10 },
    { now: () => 0, maximumFeasibilityChecks: 1 },
  );
  assert.equal(workBounded.ok, true);
  assert.equal(workBounded.ok && workBounded.cutoff, "restarts");
  assert.equal(workBounded.ok && workBounded.feasibilityChecks, 1);
  assert.equal(workBounded.ok && workBounded.optimal, false);
});

test("constraint search streams its first feasible mask before lower-bound or restart work", () => {
  const { standard, request } = repairFixture();
  const streamed: { restarts: number; feasibilityChecks: number; elapsedMs: number }[] = [];
  const searched = internals.search(
    standard.positions,
    request.edges,
    {
      when: "always",
      maxDurationMs: Number.POSITIVE_INFINITY,
      maxRestarts: 250_000,
    },
    {
      mask: (_mask, progress) => {
        streamed.push(progress);
        return false;
      },
    },
  );

  assert.equal(searched.ok, true);
  assert.equal(streamed.length, 1);
  assert.equal(streamed[0].restarts, 0);
  assert.equal(streamed[0].feasibilityChecks, 1);
  assert.ok(streamed[0].elapsedMs >= 0);
  assert.equal(searched.ok && searched.restarts, 0);
  assert.equal(searched.ok && searched.feasibilityChecks, 1);
  assert.equal(searched.ok && searched.optimal, false);
});

test("constraint search publishes work progress about every thirty milliseconds", () => {
  const positions = new Map<string, GridPosition>();
  const edges: LayoutEdge[] = [];
  for (let cycle = 0; cycle < 20; cycle += 1) {
    for (let node = 0; node < 3; node += 1) {
      positions.set(`${cycle}-${node}`, at(0, cycle));
    }
    edges.push(
      { from: `${cycle}-0`, to: `${cycle}-1`, direction: "East" },
      { from: `${cycle}-1`, to: `${cycle}-2`, direction: "East" },
      { from: `${cycle}-2`, to: `${cycle}-0`, direction: "East" },
    );
  }
  let clock = 0;
  const progress: { feasibilityChecks: number; elapsedMs: number }[] = [];
  const searched = internals.search(
    positions,
    edges,
    { when: "always", maxDurationMs: Number.POSITIVE_INFINITY, maxRestarts: 1 },
    {
      now: () => clock++,
      progress: (event) => progress.push(event),
    },
  );

  assert.equal(searched.ok, true);
  assert.ok(progress.length >= 3);
  assert.equal(progress[0].feasibilityChecks, 32);
  assert.ok(progress[0].elapsedMs >= 30);
  assert.ok(progress[1].elapsedMs - progress[0].elapsedMs < 75);
});

const AXIS_CASES: readonly {
  positive: LayoutDirection;
  negative: LayoutDirection;
  position: (value: number) => GridPosition;
}[] = [
  { positive: "East", negative: "West", position: (value) => at(value, 0) },
  { positive: "South", negative: "North", position: (value) => at(0, value) },
  { positive: "Up", negative: "Down", position: (value) => at(0, 0, value) },
];
const PERMUTATIONS = [
  [0, 1, 2],
  [0, 2, 1],
  [1, 0, 2],
  [1, 2, 0],
  [2, 0, 1],
  [2, 1, 0],
] as const;

test("190 deterministic cases preserve the minimum reciprocal-aware objective", () => {
  for (let example = 0; example < 190; example += 1) {
    const axis = AXIS_CASES[example % AXIS_CASES.length];
    const order = PERMUTATIONS[(example * 5 + Math.floor(example / 7)) % PERMUTATIONS.length];
    const ids = [
      `a${String(example).padStart(3, "0")}`,
      `b${String(example).padStart(3, "0")}`,
      `c${String(example).padStart(3, "0")}`,
    ];
    const positions = new Map(ids.map((id, index) => [id, axis.position(order[index])]));
    const edges: LayoutEdge[] = [
      { from: ids[0], to: ids[1], direction: axis.positive },
      { from: ids[1], to: ids[0], direction: axis.negative },
      { from: ids[1], to: ids[2], direction: axis.positive },
      { from: ids[2], to: ids[0], direction: axis.positive },
    ];

    // Every room lies on one level or one vertical line, so no level relation
    // exists and the third weight stays 0.
    let bruteScore: readonly [number, number, number] | undefined;
    for (let mask = 0; mask < 1 << edges.length; mask += 1) {
      const removed: number[] = [];
      for (let edge = 0; edge < edges.length; edge += 1) {
        if (mask & (1 << edge)) removed.push(edge);
      }
      const analyzed = internals.analyze(positions, edges, removed);
      assert.equal(analyzed.ok, true);
      if (!analyzed.ok || !analyzed.feasible) continue;
      const score: readonly [number, number, number] = [
        removed.length,
        removed.filter((edge) => edge === 0 || edge === 1).length,
        0,
      ];
      if (!bruteScore || score[0] < bruteScore[0] ||
        (score[0] === bruteScore[0] && score[1] < bruteScore[1])) bruteScore = score;
    }

    const searched = internals.search(
      positions,
      edges,
      { when: "always", maxDurationMs: 1_000, maxRestarts: 64 },
    );
    assert.equal(searched.ok, true, `case ${example}`);
    assert.deepEqual(searched.ok && searched.score, bruteScore, `case ${example}`);
    assert.equal(searched.ok && searched.optimal, true, `case ${example}`);
    assert.equal(searched.ok && searched.score[1], 0, `case ${example}`);

    const repeated = internals.search(
      positions,
      edges,
      { when: "always", maxDurationMs: 1_000, maxRestarts: 64 },
    );
    assert.deepEqual(
      repeated.ok && repeated.removedSourceIndexes,
      searched.ok && searched.removedSourceIndexes,
      `case ${example}`,
    );
  }
});

test("grouped reciprocal constraints preserve exhaustive small-graph feasibility", () => {
  const ids = ["a", "b", "c", "d"];
  const positions = new Map(ids.map((id, index) => [id, at(index, index % 2, Math.floor(index / 2))]));
  let levelCases = 0;
  const pairs = [["a", "b"], ["b", "c"], ["c", "d"], ["a", "c"], ["b", "d"]] as const;
  const directions: LayoutDirection[] = ["East", "West", "South", "North", "Up", "Down"];
  let state = 0x51ced123;
  const random = (): number => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };

  for (let example = 0; example < 48; example += 1) {
    const edges: LayoutEdge[] = [];
    for (let index = 0; index < 7; index += 1) {
      const pair = pairs[Math.floor(random() * pairs.length)];
      edges.push({
        from: pair[0],
        to: pair[1],
        direction: directions[Math.floor(random() * directions.length)],
      });
    }
    // Level relations are given up in turn: each mask also runs with the
    // subset of level relations its low bits select.
    const { levelRelations } = internals.relations(positions, edges);
    for (let mask = 0; mask < 1 << edges.length; mask += 1) {
      const removed = new Set<number>();
      for (let edge = 0; edge < edges.length; edge += 1) {
        if (mask & (1 << edge)) removed.add(edge);
      }
      const expected = referenceConstraintFeasibility(positions, edges, removed);
      const analyzed = internals.analyze(positions, edges, [...removed]);
      assert.equal(analyzed.ok, true, `case ${example}, mask ${mask}`);
      assert.equal(
        analyzed.ok && analyzed.feasible,
        expected,
        `case ${example}, mask ${mask}`,
      );
      if (levelRelations.length === 0) continue;
      const levelMask = mask % (1 << levelRelations.length);
      const relaxedLevels = new Set(levelRelations.flatMap((sourceIndexes, relation) =>
        levelMask & (1 << relation) ? sourceIndexes : []
      ));
      const levelExpected = referenceConstraintFeasibility(
        positions,
        edges,
        new Set([...removed, ...relaxedLevels]),
        relaxedLevels,
      );
      const levelAnalyzed = internals.analyze(positions, edges, [...removed], {
        relaxedLevelSourceIndexes: [...relaxedLevels],
      });
      assert.equal(
        levelAnalyzed.ok && levelAnalyzed.feasible,
        levelExpected,
        `case ${example}, mask ${mask}, level relations ${levelMask}`,
      );
      if (levelMask !== 0) levelCases += 1;
    }
  }
  assert.ok(levelCases >= 500, `only ${levelCases} masks gave up a level relation`);
});

test("grouped source edges still expand to a feasible public removal set", () => {
  const ids = ["a", "b", "c", "d"];
  const positions = new Map(ids.map((id, index) => [id, at(index, index % 2, Math.floor(index / 2))]));
  const pairs = [["a", "b"], ["b", "c"], ["c", "d"], ["a", "c"]] as const;
  const directions = ["East", "West", "South", "North", "Up", "Down"] as const;
  const opposite: Record<(typeof directions)[number], (typeof directions)[number]> = {
    East: "West", West: "East", South: "North", North: "South", Up: "Down", Down: "Up",
  };

  for (let example = 0; example < 24; example += 1) {
    const edges: LayoutEdge[] = [];
    for (let index = 0; index < 6; index += 1) {
      const pair = pairs[(index * 3 + example) % pairs.length];
      edges.push({
        from: pair[0],
        to: pair[1],
        direction: directions[(index * 5 + example * 2) % directions.length],
      });
    }
    const reciprocal = edges.map((edge) => edges.some((other) =>
      other.from === edge.to && other.to === edge.from &&
      other.direction === opposite[edge.direction as keyof typeof opposite]
    ));
    const expected = exhaustiveRelaxationScore(positions, edges, reciprocal);
    const searched = internals.search(
      positions,
      edges,
      { when: "always", maxDurationMs: 1_000, maxRestarts: 64 },
    );
    assert.equal(searched.ok, true, `case ${example}`);
    // The hitting-set master strategy certifies every one of these instances,
    // so the exhaustive objective is asserted unconditionally.
    assert.equal(searched.ok && searched.optimal, true, `case ${example} certified`);
    assert.ok(expected, `case ${example} has an exhaustive objective`);
    assert.deepEqual(searched.ok && searched.score, expected, `case ${example} exact objective`);
    const analyzed = searched.ok
      ? internals.analyze(positions, edges, searched.removedSourceIndexes, {
        relaxedLevelSourceIndexes: searched.relaxedLevelSourceIndexes,
      })
      : undefined;
    assert.equal(analyzed?.ok && analyzed.feasible, true, `case ${example} feasible expansion`);
    assert.equal(
      searched.ok && searched.removedSourceIndexes.length,
      searched.ok && searched.score[0],
      `case ${example} public expansion`,
    );
  }
});

test("a reciprocal-positive optimum is certified instead of burning the restart budget", () => {
  // Contradictory reciprocal pairs: a<->b protected both East and West. Every
  // feasible mask removes one whole canonical group, and both groups carry
  // two reciprocal source edges, so the exhaustive optimum is [2, 2] — an
  // objective no zero-reciprocal certificate can ever reach.
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(1, 0)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "East" },
    { from: "b", to: "a", direction: "West" },
    { from: "a", to: "b", direction: "West" },
    { from: "b", to: "a", direction: "East" },
  ];
  let expected: readonly [number, number] | undefined;
  for (let mask = 1; mask < 1 << edges.length; mask += 1) {
    const removed: number[] = [];
    for (let edge = 0; edge < edges.length; edge += 1) {
      if (mask & (1 << edge)) removed.push(edge);
    }
    const analyzed = internals.analyze(positions, edges, removed);
    assert.equal(analyzed.ok, true);
    if (!analyzed.ok || !analyzed.feasible) continue;
    const score: readonly [number, number] = [removed.length, removed.length];
    if (!expected || score[0] < expected[0]) expected = score;
  }
  assert.deepEqual(expected, [2, 2]);

  const searched = internals.search(
    positions,
    edges,
    { when: "always", maxDurationMs: Number.POSITIVE_INFINITY, maxRestarts: 4 },
  );
  assert.equal(searched.ok, true);
  assert.equal(searched.ok && searched.optimal, true);
  assert.deepEqual(searched.ok && searched.score, [2, 2, 0]);
  assert.equal(searched.ok && searched.cutoff, "none");
  assert.equal(searched.ok && searched.restarts, 0, "no restart was needed for the proof");
  assert.equal(searched.ok && searched.lowerBound, 2);
});

test("an exhausted hitting-set budget falls back to restarts and reports honestly", () => {
  // One of the four-room instances whose optimum the disjoint-conflict bound
  // alone cannot certify. The exact solver certifies it; with the solver's
  // deterministic node budget forced to zero the search must degrade to the
  // seeded randomized restarts, keep a feasible (possibly weaker) result, and
  // never claim optimality it did not prove.
  const ids = ["a", "b", "c", "d"];
  const positions = new Map(ids.map((id, index) => [id, at(index, index % 2, Math.floor(index / 2))]));
  const pairs = [["a", "b"], ["b", "c"], ["c", "d"], ["a", "c"]] as const;
  const directions = ["East", "West", "South", "North", "Up", "Down"] as const;
  const edges: LayoutEdge[] = [];
  for (let index = 0; index < 6; index += 1) {
    const pair = pairs[(index * 3) % pairs.length];
    edges.push({
      from: pair[0],
      to: pair[1],
      direction: directions[(index * 5) % directions.length],
    });
  }
  const options = { when: "always" as const, maxDurationMs: 1_000, maxRestarts: 64 };
  const certified = internals.search(positions, edges, options);
  assert.equal(certified.ok, true);
  assert.equal(certified.ok && certified.optimal, true);
  assert.equal(certified.ok && certified.restarts, 0);

  const fallback = internals.search(positions, edges, options, { maximumHittingSetNodes: 0 });
  assert.equal(fallback.ok, true);
  assert.ok(fallback.ok && fallback.restarts > 0, "the restart fallback actually ran");
  if (certified.ok && fallback.ok) {
    assert.ok(
      fallback.score[0] > certified.score[0] ||
        (fallback.score[0] === certified.score[0] && fallback.score[1] >= certified.score[1]),
      "the fallback cannot beat the certified optimum",
    );
    if (!fallback.optimal) assert.equal(fallback.cutoff, "restarts");
    const expanded = internals.analyze(positions, edges, fallback.removedSourceIndexes, {
      relaxedLevelSourceIndexes: fallback.relaxedLevelSourceIndexes,
    });
    assert.equal(expanded.ok && expanded.feasible, true, "the fallback mask stays feasible");
  }
});

/** How many components cores form when cores sharing a group join one. */
function coreComponentCount(cores: readonly (readonly number[])[]): number {
  const component = cores.map((_core, index) => index);
  for (let a = 0; a < cores.length; a += 1) {
    for (let b = a + 1; b < cores.length; b += 1) {
      if (component[a] === component[b]) continue;
      if (!cores[a].some((group) => cores[b].includes(group))) continue;
      const joined = component[b];
      for (let index = 0; index < cores.length; index += 1) {
        if (component[index] === joined) component[index] = component[a];
      }
    }
  }
  return new Set(component).size;
}

test("the exact hitting-set solve matches brute force on seeded random instances", () => {
  // 3–13 relation groups with primary weights 1–3, reciprocal weights up to
  // the primary and a level weight of 0 or 1 (about a third of them level
  // relations), 1–6 cores of 1–5 groups, and an incumbent drawn from every
  // hitting set, the optimum a fifth of the time. The solve must complete with
  // the exact minimum, and return a set only when it strictly beats the
  // incumbent, hits every core and weighs what it reports.
  let state = 0x4817_5e7a;
  const random = (): number => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
  let multipleComponents = 0;
  let improved = 0;
  for (let trial = 0; trial < 4_000; trial += 1) {
    const groupCount = 3 + Math.floor(random() * 11);
    const groupSourceCount = Int32Array.from(
      { length: groupCount },
      () => 1 + Math.floor(random() * 3),
    );
    const groupReciprocalCount = Int32Array.from(
      groupSourceCount,
      (weight) => Math.floor(random() * (weight + 1)),
    );
    const groupLevelCount = Int32Array.from(groupSourceCount, () => random() < 0.3 ? 1 : 0);
    const cores: number[][] = [];
    const coreCount = 1 + Math.floor(random() * 6);
    for (let index = 0; index < coreCount; index += 1) {
      const width = 1 + Math.floor(random() * Math.min(groupCount, 5));
      const core = new Set<number>();
      while (core.size < width) core.add(Math.floor(random() * groupCount));
      cores.push([...core]);
    }
    const weigh = (includes: (group: number) => boolean): [number, number, number] => {
      const score: [number, number, number] = [0, 0, 0];
      for (let group = 0; group < groupCount; group += 1) {
        if (!includes(group)) continue;
        score[0] += groupSourceCount[group];
        score[1] += groupReciprocalCount[group];
        score[2] += groupLevelCount[group];
      }
      return score;
    };
    const better = (a: readonly number[], b: readonly number[]): boolean =>
      a[0] < b[0] || a[0] === b[0] && (a[1] < b[1] || a[1] === b[1] && a[2] < b[2]);
    const hittingSets: number[] = [];
    let optimum: [number, number, number] | undefined;
    let optimalSet = 0;
    for (let set = 0; set < 1 << groupCount; set += 1) {
      if (!cores.every((core) => core.some((group) => set & (1 << group)))) continue;
      hittingSets.push(set);
      const score = weigh((group) => (set & (1 << group)) !== 0);
      if (!optimum || better(score, optimum)) {
        optimum = score;
        optimalSet = set;
      }
    }
    const incumbent = random() < 0.2
      ? optimalSet
      : hittingSets[Math.floor(random() * hittingSets.length)];
    const incumbentMask = Uint8Array.from(
      { length: groupCount },
      (_value, group) => (incumbent >> group) & 1,
    );
    const solved = internals.hittingSet(
      { groupSourceCount, groupReciprocalCount, groupLevelCount },
      cores,
      incumbentMask,
    );
    const label = `trial ${trial}: ${JSON.stringify({ cores, incumbent })}`;
    assert.equal(solved.complete, true, label);
    assert.deepEqual(solved.score ?? solved.incumbent, optimum, label);
    if (solved.mask) {
      const mask = solved.mask;
      improved += 1;
      assert.ok(
        better(optimum!, solved.incumbent),
        `${label}: a returned set strictly beats the incumbent`,
      );
      assert.ok(cores.every((core) => core.some((group) => mask[group])), `${label}: hits every core`);
      assert.deepEqual(weigh((group) => mask[group] === 1), solved.score, `${label}: weighs its score`);
    }
    if (coreComponentCount(cores) > 1) multipleComponents += 1;
  }
  assert.ok(multipleComponents >= 500, `${multipleComponents} instances with several components`);
  assert.ok(improved >= 1_000, `${improved} instances with a better set than the incumbent`);
});

test("wide disjoint conflict cores certify within a sliver of the node budget", () => {
  // The first exact solve on a large map: the pairwise-disjoint cores the
  // lower bound collects, cycles of about fifty relations each, all of one
  // primary weight but mixed reciprocal weights, so no core offers a relation
  // free of reciprocal cost. The incumbent relaxes a fully reciprocal relation
  // in every core and one relation in no core. Branch and bound over the
  // whole cores spends the default budget of 262,144 nodes here without
  // finishing; every core instead keeps only its cheapest relation, and each
  // core, a component of its own, solves in two nodes.
  const widths = [50, 52, 48, 51, 49, 53, 47];
  const cores: number[][] = [];
  let groupCount = 0;
  for (const width of widths) {
    cores.push(Array.from({ length: width }, () => groupCount++));
  }
  const outside = groupCount++;
  const groupSourceCount = new Int32Array(groupCount).fill(2);
  const groupReciprocalCount = Int32Array.from(
    { length: groupCount },
    (_value, group) => group % 3 === 1 ? 1 : 2,
  );
  const incumbentMask = new Uint8Array(groupCount);
  for (const core of cores) {
    incumbentMask[core.find((group) => groupReciprocalCount[group] === 2) as number] = 1;
  }
  incumbentMask[outside] = 1;
  const weights = { groupSourceCount, groupReciprocalCount };

  const solved = internals.hittingSet(weights, cores, incumbentMask);
  assert.deepEqual(solved.incumbent, [16, 16, 0]);
  assert.equal(solved.complete, true);
  assert.deepEqual(solved.score, [14, 7, 0]);
  const minimum = solved.mask as Uint8Array;
  assert.ok(cores.every((core) => core.some((group) => minimum[group])), "the minimum hits every core");
  assert.ok(solved.nodes <= 2 * cores.length, `${solved.nodes} nodes`);
  // The components draw on one budget: a node short of the whole solve, the
  // last component runs out and the solve is incomplete.
  assert.equal(internals.hittingSet(weights, cores, incumbentMask, solved.nodes - 1).complete, false);

  // Offered back as the incumbent, the minimum is certified just as fast.
  const certified = internals.hittingSet(weights, cores, minimum);
  assert.equal(certified.complete, true);
  assert.equal(certified.mask, undefined);
  assert.ok(certified.nodes <= 2 * cores.length, `${certified.nodes} nodes`);
});

test("canonical relation groups preserve duplicate and reciprocal objective weights exactly", () => {
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(1, 0)],
    ["c", at(2, 0)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "East" },
    { from: "a", to: "b", direction: "East" },
    { from: "b", to: "a", direction: "West" },
    { from: "b", to: "c", direction: "East" },
    { from: "c", to: "a", direction: "East" },
  ];

  assert.equal(internals.analyze(positions, edges).ok, true);
  assert.equal(internals.analyze(positions, edges).feasible, false);
  assert.equal(
    internals.analyze(positions, edges, [0]).feasible,
    false,
    "removing only part of a canonical duplicate group changes no relation",
  );
  assert.equal(internals.analyze(positions, edges, [0, 1, 2]).feasible, true);
  assert.equal(internals.analyze(positions, edges, [3]).feasible, true);
  assert.equal(internals.analyze(positions, edges, [4]).feasible, true);

  const searched = internals.search(positions, edges, {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: Number.POSITIVE_INFINITY,
    maxMaskDiversifications: Number.POSITIVE_INFINITY,
  });
  assert.equal(searched.ok, true);
  assert.equal(searched.ok && searched.optimal, true);
  assert.deepEqual(searched.ok && searched.score, [1, 0, 0]);
  assert.deepEqual(searched.ok && searched.removedSourceIndexes, [4]);
  assert.equal(
    searched.ok && searched.removed.reduce((total, value) => total + value, 0),
    1,
  );
});

test("consecutive full repairs stay bit-identical across scratch-arena reuse", () => {
  // Feasibility analysis and coordinate construction draw on per-instance
  // scratch arenas. Three back-to-back repairs in one process must agree on
  // every position and every deterministic counter: any state leaking across
  // checks, states, or repair instances would surface here.
  const { request, standard } = softSeparatorFixture();
  const repair = () =>
    repairIntegralLayoutConstraints(request, standard, {
      when: "always",
      maxDurationMs: Number.POSITIVE_INFINITY,
      maxRestarts: 64,
      maxExtensionStates: 256,
      maxLayouts: 2,
      maxPolishTournaments: 1,
    });
  const stable = (plan: IntegralLayoutPlan) => ({
    positions: [...plan.positions].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
    movedExisting: [...plan.movedExisting].sort(),
    quality: plan.quality,
    report: plan.constraintRepair && {
      ...plan.constraintRepair,
      firstIncumbentMs: 0,
      searchMs: 0,
      compactionMs: 0,
      polishMs: 0,
      crossingRepair: { ...plan.constraintRepair.crossingRepair, elapsedMs: 0 },
    },
  });
  const first = stable(repair());
  assert.equal(first.report?.selected, true);
  assert.ok((first.report?.feasibilityChecks ?? 0) >= 1, "expected feasibility analysis to run");
  assert.ok((first.report?.separatorStates ?? 0) > 1, "expected repeated separator states");
  assert.deepEqual(stable(repair()), first);
  assert.deepEqual(stable(repair()), first);
});

test("fixed-anchor compaction incumbents repeat exactly under scratch reuse", () => {
  // The coordinate scratch carries fixed-shift and visitation flags across
  // separator states; an incomplete reset would corrupt a later state's
  // anchored component. Two identical runs must publish identical incumbent
  // sequences, work stats, and final geometry.
  const positions = new Map<string, GridPosition>([
    ["from", at(0, 0)],
    ["to", at(0, -3)],
    ["block-a", at(0, -1)],
    ["block-b", at(0, -2)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "from", to: "to", direction: "North" },
    { from: "to", to: "from", direction: "South" },
    { from: "block-a", to: "block-b", direction: "North" },
    { from: "block-b", to: "block-a", direction: "South" },
  ];
  const run = () =>
    internals.compact(positions, edges, { fixedIds: ["from"], maximumStates: 64 });
  const first = run();
  assert.equal(first.ok, true);
  assert.equal(first.status.completed, true);
  assert.ok(first.workStats.separatorStates > 1, "expected a multi-state extension search");
  assert.ok((first.workStats.peakLiveSearchNodes ?? 0) > 0, "expected live-node telemetry");
  assert.ok(first.incumbents.length >= 1, "expected at least one published incumbent");
  assert.deepEqual(run(), first);
  assert.deepEqual(
    internals.compact(positions, edges, {
      fixedIds: ["from"],
      maximumStates: 64,
      forceSoftDefectHashCollision: true,
    }),
    first,
    "exact witnesses distinguish every forced soft-defect hash collision",
  );

  const memoryBounded = internals.compact(positions, edges, {
    fixedIds: ["from"],
    maximumStates: 64,
    maximumLiveSearchNodes: 1,
  });
  assert.equal(memoryBounded.ok, true);
  assert.equal(memoryBounded.workStats.peakLiveSearchNodes, 1);
  assert.equal(memoryBounded.status.exhausted, true);
});

// ---------------------------------------------------------------------------
// Levels
// ---------------------------------------------------------------------------

const LEVEL_REPAIR_OPTIONS: ConstraintRepairOptions = {
  when: "always",
  maxDurationMs: Number.POSITIVE_INFINITY,
  maxLayouts: 2,
  maxExtensionStates: 4_096,
  maxMaskDiversifications: 16,
  maxPolishTournaments: 2,
  maxCrossingWork: 64,
};

function storedPlan(request: IntegralLayoutRequest): IntegralLayoutPlan {
  const positions = new Map(request.residents.map((room) => [room.id, room.position]));
  return {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
  };
}

test("the planner and the repair flatten a row that climbs a level by a flat exit", () => {
  // A row on level 0 whose east end climbs onto level 1 by a flat East exit,
  // with a North exit drawn beside its room.
  const request: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(1, 0), movable: true },
      { id: "c", position: at(2, 0, 1), movable: true },
      { id: "d", position: at(3, 0, 1), movable: true },
      { id: "n", position: at(1, 0, 1), movable: true },
    ],
    nodes: [],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "a", direction: "West" },
      { from: "b", to: "c", direction: "East" },
      { from: "c", to: "b", direction: "West" },
      { from: "c", to: "d", direction: "East" },
      { from: "d", to: "c", direction: "West" },
      { from: "d", to: "n", direction: "North" },
      { from: "n", to: "d", direction: "South" },
    ],
    allowExistingMoves: true,
  };

  const standard = planIntegralLayout(request);
  const repaired = repairIntegralLayoutConstraints(request, standard, LEVEL_REPAIR_OPTIONS);
  assert.deepEqual(["c", "d", "n"].map((id) => repaired.positions.get(id)?.level), [0, 0, 0]);
  // The flattened plan contradicts no level, so its repair has no level
  // relations.
  assert.deepEqual(internals.relations(standard.positions, request.edges).levelRelations, []);
  assert.equal(repaired.constraintRepair?.relaxedLevelRelations, 0);
  // Seeded with the drawing as stored, where the East exit climbs a level,
  // the repair flattens the row itself instead of finding no layout.
  const fromStored = repairIntegralLayoutConstraints(request, storedPlan(request), LEVEL_REPAIR_OPTIONS);
  assert.equal(fromStored.constraintRepair?.outcome, "searched");
  assert.equal(fromStored.quality.cardinalRayViolations, 0);
  assert.equal(new Set([...fromStored.positions.values()].map((position) => position.level)).size, 1);
});

/**
 * A room of a grid leads West by a flat exit to the room stacked one level
 * above it, whose way back is Down. The grid has two misplaced rooms, so a
 * repair has work to do beyond that pair.
 */
function slopeWithDownRequest(): IntegralLayoutRequest {
  const residents: IntegralLayoutRequest["residents"][number][] = [];
  const edges: LayoutEdge[] = [];
  const both = (from: string, to: string, forward: LayoutDirection, back: LayoutDirection): void => {
    edges.push({ from, to, direction: forward }, { from: to, to: from, direction: back });
  };
  const grid = (x: number, y: number): string => `g${x}${y}`;
  const misplaced = new Map([["g33", at(6, 5)], ["g21", at(4, -2)]]);
  for (let y = 0; y < 4; y += 1) {
    for (let x = 0; x < 4; x += 1) {
      residents.push({
        id: grid(x, y),
        position: misplaced.get(grid(x, y)) ?? at(x, y),
        movable: true,
      });
      if (x > 0) both(grid(x - 1, y), grid(x, y), "East", "West");
      if (y > 0) both(grid(x, y - 1), grid(x, y), "South", "North");
    }
  }
  for (let x = 0; x < 3; x += 1) {
    residents.push({ id: `u${x}`, position: at(x, 0, 1), movable: true });
    if (x > 0) both(`u${x - 1}`, `u${x}`, "East", "West");
  }
  edges.push(
    { from: "g00", to: "u0", direction: "West" },
    { from: "u0", to: "g00", direction: "Down" },
  );
  return { residents, nodes: [], edges, allowExistingMoves: true };
}

test("a slope whose way back is Down is an ordinary conflict", () => {
  const request = slopeWithDownRequest();
  const stored = storedPlan(request);
  // The flat West exit's shared level is a level relation the search may give
  // up, so the pair is an ordinary conflict: give up the Down exit, or the
  // West exit's level. Each is one one-way exit, and of equal costs the
  // search's mask keeps the rooms on one level.
  const searched = internals.search(stored.positions, request.edges, LEVEL_REPAIR_OPTIONS);
  assert.ok(searched.ok, "the search's first check passes");
  assert.equal(searched.optimal, true);
  assert.deepEqual(searched.score, [1, 0, 0]);
  assert.deepEqual(
    searched.removedSourceIndexes.map((index) => request.edges[index].direction),
    ["Down"],
  );
  assert.deepEqual(searched.relaxedLevelSourceIndexes, []);
  const repaired = repairIntegralLayoutConstraints(request, stored, LEVEL_REPAIR_OPTIONS);
  const report = repaired.constraintRepair;
  assert.ok(report, "the repair runs and reports");
  assert.equal(report.outcome, "searched");
  assert.ok(compareLayoutQuality(repaired.quality, stored.quality) > 0, "the repair improves its seed");
  assert.ok(stored.quality.cardinalRayViolations > 1, "the seed draws more exits wrong");
  assert.equal(repaired.quality.cardinalRayViolations, 1, "only the contradicting pair stays unmet");
  // Pulling the upper row onto the grid's level would crowd it, so the layout
  // that gives up the West exit's level instead draws the map better. It
  // gives up as few exits, so it is still optimal.
  assert.equal(report.relaxedEdges, 1);
  assert.equal(report.relaxedLevelRelations, 1);
  assert.equal(report.constraintOptimal, true);
  assert.deepEqual(repaired.positions.get("u0"), at(0, 0, 1), "u0 stays stacked above g00");
  assert.deepEqual(repaired.positions.get("g00"), at(0, 0));
});

/**
 * Room `b` stacked above `a`, joined by `a` West `b` and `b` Down `a`: the flat
 * exit puts them on one level, the vertical one a level apart. `reciprocal`
 * says which of the two carries its exact reverse.
 */
function stackedContradiction(reciprocal: "flat" | "vertical"): IntegralLayoutRequest {
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "West" },
    { from: "b", to: "a", direction: "Down" },
  ];
  if (reciprocal === "flat") edges.push({ from: "b", to: "a", direction: "East" });
  else edges.push({ from: "a", to: "b", direction: "Up" });
  return {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(0, 0, 1), movable: true },
    ],
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
}

test("a flat and a vertical exit that contradict give up the cheaper one", () => {
  // The West/East pair costs two exits and the Down exit one: the search
  // gives up Down, and the rooms join one level with the pair drawn right.
  const cheapVertical = stackedContradiction("flat");
  const verticalSeed = storedPlan(cheapVertical);
  const verticalSearch = internals.search(
    verticalSeed.positions,
    cheapVertical.edges,
    LEVEL_REPAIR_OPTIONS,
  );
  assert.ok(verticalSearch.ok);
  assert.equal(verticalSearch.optimal, true);
  assert.deepEqual(verticalSearch.score, [1, 0, 0]);
  assert.deepEqual(verticalSearch.removedSourceIndexes, [1], "the Down exit");
  assert.deepEqual(verticalSearch.relaxedLevelSourceIndexes, []);
  const flattened = repairIntegralLayoutConstraints(cheapVertical, verticalSeed, LEVEL_REPAIR_OPTIONS);
  assert.equal(flattened.constraintRepair?.outcome, "searched");
  assert.equal(flattened.constraintRepair?.relaxedLevelRelations, 0);
  assert.equal(flattened.quality.cardinalRayViolations, 1);
  const a = flattened.positions.get("a") as GridPosition;
  const b = flattened.positions.get("b") as GridPosition;
  assert.ok(a.level === b.level && b.x < a.x && b.y === a.y, "b lies west of a on its level");

  // The Up/Down pair costs two exits and the West exit's level one: the search
  // gives up that level, and b stays stacked above a.
  const cheapLevel = stackedContradiction("vertical");
  const levelSeed = storedPlan(cheapLevel);
  const levelSearch = internals.search(levelSeed.positions, cheapLevel.edges, LEVEL_REPAIR_OPTIONS);
  assert.ok(levelSearch.ok);
  assert.equal(levelSearch.optimal, true);
  assert.deepEqual(levelSearch.score, [1, 0, 1]);
  assert.deepEqual(levelSearch.removedSourceIndexes, [0], "the West exit");
  assert.deepEqual(levelSearch.relaxedLevelSourceIndexes, [0], "through its level");
  const stacked = repairIntegralLayoutConstraints(cheapLevel, levelSeed, LEVEL_REPAIR_OPTIONS);
  const report = stacked.constraintRepair;
  assert.ok(report);
  assert.equal(report.outcome, "searched");
  assert.equal(report.relaxedEdges, 1);
  assert.equal(report.relaxedLevelRelations, 1);
  assert.equal(report.constraintOptimal, true);
  assert.equal(stacked.quality.cardinalRayViolations, 1);
  assert.deepEqual(stacked.positions.get("b"), at(0, 0, 1), "b stays stacked above a");
  assert.deepEqual(stacked.positions.get("a"), at(0, 0));

  // The report crosses the Worker boundary with its level relations, which
  // can never outnumber the exits given up.
  const message = (value: unknown): unknown => ({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 1,
    operation: "constraint-repair",
    progress: true,
    event: { type: "constraint-repair", stage: "constraint-repair", report: value },
  });
  assert.equal(isLayoutWorkerProgress(message(report)), true);
  assert.equal(isLayoutWorkerProgress(message({ ...report, relaxedLevelRelations: 2 })), false);
  const { relaxedLevelRelations: _omitted, ...withoutLevels } = report;
  assert.equal(isLayoutWorkerProgress(message(withoutLevels)), false);
});

test("random two-floor contradictions reach the exhaustive optimum over levels and rays", () => {
  // Two stacked pairs of rooms with random flat and Up/Down exits, about half
  // of them with their reverse: the search must certify the exhaustive minimum
  // over every set of rays and level relations, and a good share of those
  // minima give up a level.
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(1, 0)],
    ["c", at(0, 0, 1)],
    ["d", at(1, 0, 1)],
  ]);
  const pairs = [["a", "b"], ["c", "d"], ["a", "c"], ["b", "d"], ["a", "d"], ["b", "c"]] as const;
  const flat: LayoutDirection[] = ["East", "West", "South", "North"];
  const opposite: Partial<Record<LayoutDirection, LayoutDirection>> = {
    East: "West", West: "East", South: "North", North: "South", Up: "Down", Down: "Up",
  };
  const random = xorshift32(0x1e7e1);
  let levelOptima = 0;
  for (let example = 0; example < 60; example += 1) {
    const edges: LayoutEdge[] = [];
    while (edges.length < 5) {
      const [from, to] = pairs[Math.floor(random() * pairs.length)];
      const direction: LayoutDirection = random() < 0.4
        ? random() < 0.5 ? "Up" : "Down"
        : flat[Math.floor(random() * flat.length)];
      edges.push({ from, to, direction });
      if (random() < 0.5 && edges.length < 6) {
        edges.push({ from: to, to: from, direction: opposite[direction] as LayoutDirection });
      }
    }
    const reciprocal = edges.map((edge) => edges.some((other) =>
      other.from === edge.to && other.to === edge.from && other.direction === opposite[edge.direction]
    ));
    const expected = exhaustiveRelaxationScore(positions, edges, reciprocal);
    const searched = internals.search(positions, edges, { when: "always", maxDurationMs: 1_000 });
    const label = `case ${example}: ${JSON.stringify(edges)}`;
    assert.ok(searched.ok, label);
    assert.equal(searched.optimal, true, label);
    assert.deepEqual(searched.score, expected, label);
    const expanded = internals.analyze(positions, edges, searched.removedSourceIndexes, {
      relaxedLevelSourceIndexes: searched.relaxedLevelSourceIndexes,
    });
    assert.equal(expanded.ok && expanded.feasible, true, `${label}: the chosen relaxation is feasible`);
    if (expected && expected[2] > 0) levelOptima += 1;
  }
  assert.ok(levelOptima >= 5, `only ${levelOptima} minima give up a level`);
});

test("a slope between locked rooms no longer leaves the repair without a layout", () => {
  // The plan keeps a flat exit between two locked rooms a level apart, which
  // no layout can draw right, beside a room drawn off its North exit. The
  // search starts with the slope's level relation given up, so it keeps the
  // slope and still repairs the North exit.
  const positions = new Map<string, GridPosition>([
    ["a", at(0, 0)],
    ["b", at(1, 0, 1)],
    ["c", at(3, 2)],
  ]);
  const edges: LayoutEdge[] = [
    { from: "a", to: "b", direction: "East" },
    { from: "b", to: "a", direction: "West" },
    { from: "a", to: "c", direction: "North" },
  ];
  const request: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: positions.get("a") as GridPosition, movable: false },
      { id: "b", position: positions.get("b") as GridPosition, movable: false },
      { id: "c", position: positions.get("c") as GridPosition, movable: true },
    ],
    nodes: [],
    edges,
    allowExistingMoves: true,
  };
  const seed = storedPlan(request);
  assert.equal(seed.quality.cardinalRayViolations, 3);
  const repaired = repairIntegralLayoutConstraints(request, seed, LEVEL_REPAIR_OPTIONS);
  const report = repaired.constraintRepair;
  assert.ok(report);
  assert.equal(report.outcome, "searched", "the repair searches instead of finding no layout");
  assert.equal(report.relaxedLevelRelations, 1);
  assert.equal(repaired.quality.cardinalRayViolations, 2, "only the slope's pair stays unmet");
  assert.deepEqual(repaired.positions.get("a"), at(0, 0));
  assert.deepEqual(repaired.positions.get("b"), at(1, 0, 1));
  const c = repaired.positions.get("c") as GridPosition;
  assert.ok(c.x === 0 && c.y < 0 && c.level === 0, "c lies north of a");
});

test("only rooms whose levels the exits or the plan contradict carry level relations", () => {
  const relations = (
    rooms: readonly (readonly [string, GridPosition])[],
    edges: readonly LayoutEdge[],
  ): number[][] => internals.relations(new Map(rooms), edges).levelRelations;
  const both = (from: string, to: string, forward: LayoutDirection, back: LayoutDirection): LayoutEdge[] => [
    { from, to, direction: forward },
    { from: to, to: from, direction: back },
  ];

  // Two floors, each a row, joined by a stair: every level holds.
  const floors = [
    ["a", at(0, 0)],
    ["b", at(1, 0)],
    ["c", at(0, 0, 1)],
    ["d", at(1, 0, 1)],
  ] as const;
  const stair = [
    ...both("a", "b", "East", "West"),
    ...both("c", "d", "East", "West"),
    ...both("a", "c", "Up", "Down"),
  ];
  assert.deepEqual(relations(floors, stair), []);

  // An Up exit inside a row: that row's flat exits get level relations, the
  // other floor's do not. An Up exit from a room to itself contests nothing.
  const inside = [...stair, { from: "a", to: "b", direction: "Up" } as LayoutEdge];
  assert.deepEqual(relations(floors, inside), [[0, 1]]);
  assert.deepEqual(relations(floors, [...stair, { from: "a", to: "a", direction: "Up" }]), []);

  // Up/Down exits that climb from each floor to the other close a cycle
  // through both.
  const cycle = [...stair, { from: "d", to: "b", direction: "Up" } as LayoutEdge];
  assert.deepEqual(relations(floors, cycle), [[0, 1], [2, 3]]);

  // A row the plan draws across two levels, with no Up/Down exit anywhere,
  // beside a row on one level.
  assert.deepEqual(
    relations([["a", at(0, 0)], ["b", at(1, 0, 2)], ["c", at(5, 5)], ["d", at(6, 5)]], [
      ...both("a", "b", "East", "West"),
      { from: "c", to: "d", direction: "East" },
    ]),
    [[0, 1]],
  );
});

test("an edge the constraint graph cannot express no longer disables the repair", () => {
  // A misdrawn one-way cycle beside a projected Up/Down pair, whose diagonal
  // constraint vectors move along two axes at once.
  const request: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(1, 0), movable: true },
      { id: "c", position: at(2, 0), movable: true },
      { id: "loft", position: at(4, -3), movable: true },
    ],
    nodes: [],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "c", direction: "East" },
      { from: "c", to: "a", direction: "East" },
      { from: "a", to: "loft", direction: "Up", constraintVector: at(1, -1) },
      { from: "loft", to: "a", direction: "Down", constraintVector: at(-1, 1) },
    ],
    allowExistingMoves: true,
  };
  const seed = storedPlan(request);
  const searched = internals.search(seed.positions, request.edges, LEVEL_REPAIR_OPTIONS);
  assert.ok(searched.ok);
  assert.deepEqual(searched.removedSourceIndexes, [2]);

  const repaired = repairIntegralLayoutConstraints(request, seed, LEVEL_REPAIR_OPTIONS);
  assert.ok(repaired.constraintRepair, "the repair runs and reports");
  assert.equal(repaired.constraintRepair.relaxedEdges, 1);
  // The diagonal pair is still measured, and the polish draws it exactly.
  assert.equal(repaired.quality.cardinalRayViolations, 1);
  assert.deepEqual(repaired.positions.get("loft"), at(1, -1));
});

function xorshift32(seed: number): () => number {
  let state = seed >>> 0 || 1;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
}

const REVERSE_DIRECTION: Partial<Record<LayoutDirection, LayoutDirection>> = {
  North: "South",
  East: "West",
  South: "North",
  West: "East",
  Up: "Down",
  Down: "Up",
};

const PLANAR_STEP: Partial<Record<LayoutDirection, GridPosition>> = {
  North: at(0, -1),
  East: at(1, 0),
  South: at(0, 1),
  West: at(-1, 0),
};

const COMPASS_DIRECTIONS = ["North", "East", "South", "West"] as const;

/**
 * A random mixed-level request: slopes, stacked and contradicted Up/Down
 * exits, projected verticals, locked rooms, misdrawn rooms, and a chart whose
 * new rooms and charted residents sit a few levels off the stored map.
 */
function mixedLevelRequest(seed: number): IntegralLayoutRequest {
  const random = xorshift32(seed);
  const integer = (low: number, high: number): number =>
    low + Math.floor(random() * (high - low + 1));
  const rooms: { id: string; position: GridPosition }[] = [{ id: "r00", position: at(0, 0) }];
  const cells = new Set(["0:0:0"]);
  const edges: LayoutEdge[] = [];
  const roomCount = integer(8, 22);
  for (let index = 1; index < roomCount; index += 1) {
    const parent = rooms[integer(0, rooms.length - 1)];
    const id = `r${String(index).padStart(2, "0")}`;
    let { x, y, level } = parent.position;
    let direction: LayoutDirection;
    let constraintVector: GridPosition | undefined;
    const roll = random();
    if (roll < 0.05) {
      direction = random() < 0.5 ? "Up" : "Down";
      constraintVector = at(random() < 0.5 ? 1 : -1, direction === "Up" ? -1 : 1);
      x += constraintVector.x;
      y += constraintVector.y;
    } else if (roll < 0.15) {
      direction = random() < 0.5 ? "Up" : "Down";
      level += direction === "Up" ? 1 : -1;
    } else {
      direction = COMPASS_DIRECTIONS[integer(0, 3)];
      x += (PLANAR_STEP[direction] as GridPosition).x;
      y += (PLANAR_STEP[direction] as GridPosition).y;
      if (roll < 0.3) level += random() < 0.5 ? 1 : -1;
    }
    if (random() < 0.25) {
      x += integer(-2, 2);
      y += integer(-2, 2);
    }
    if (random() < 0.05) level += 1;
    while (cells.has(`${x}:${y}:${level}`)) x += 1;
    cells.add(`${x}:${y}:${level}`);
    rooms.push({ id, position: at(x, y, level) });
    edges.push({ from: parent.id, to: id, direction, ...(constraintVector ? { constraintVector } : {}) });
    if (random() < 0.8) {
      edges.push({
        from: id,
        to: parent.id,
        direction: REVERSE_DIRECTION[direction] as LayoutDirection,
        ...(constraintVector ? { constraintVector: at(-constraintVector.x, -constraintVector.y) } : {}),
      });
    }
  }
  for (let chord = 0; chord < Math.floor(roomCount / 5); chord += 1) {
    const a = rooms[integer(0, rooms.length - 1)];
    const b = rooms[integer(0, rooms.length - 1)];
    if (a !== b) {
      edges.push({ from: a.id, to: b.id, direction: COMPASS_DIRECTIONS[integer(0, 3)] });
    }
  }
  const chartShift = integer(-2, 2);
  const residents: IntegralLayoutRequest["residents"][number][] = [];
  const nodes: IntegralLayoutRequest["nodes"][number][] = [];
  for (const room of rooms) {
    const fresh = (room.id !== "r00" || seed % 5 === 0) && random() < 0.25;
    if (!fresh) residents.push({ id: room.id, position: room.position, movable: random() > 0.1 });
    if (fresh || random() < 0.4) {
      const { x, y, level } = room.position;
      nodes.push({ id: room.id, relative: at(x, y, level + (fresh ? 0 : chartShift)) });
    }
  }
  return { residents, nodes, edges, centerId: "r00", allowExistingMoves: true };
}

/**
 * A random tree of rooms whose flat exits stay on one level, with stacked
 * Up/Down exits, same-level chords and pinned rooms, plus the edges the
 * constraint graph reads specially: a self-loop, a duplicate, an exit to an
 * unknown room, constraint vectors that round to one step (one of them with a
 * negative zero), a longer vector and a diagonal.
 */
function admissionFixture(seed: number): {
  positions: Map<string, GridPosition>;
  edges: LayoutEdge[];
  fixedIds: string[];
} {
  const random = xorshift32(seed);
  const integer = (low: number, high: number): number =>
    low + Math.floor(random() * (high - low + 1));
  const rooms: { id: string; position: GridPosition }[] = [{ id: "a00", position: at(0, 0) }];
  const cells = new Set(["0:0:0"]);
  const edges: LayoutEdge[] = [];
  const count = integer(6, 18);
  for (let index = 1; index < count; index += 1) {
    const parent = rooms[integer(0, rooms.length - 1)];
    const id = `a${String(index).padStart(2, "0")}`;
    let { x, y, level } = parent.position;
    let direction: LayoutDirection;
    if (random() < 0.2) {
      direction = random() < 0.5 ? "Up" : "Down";
      level += direction === "Up" ? 1 : -1;
    } else {
      direction = COMPASS_DIRECTIONS[integer(0, 3)];
      const cellsAlong = integer(1, 2);
      x += (PLANAR_STEP[direction] as GridPosition).x * cellsAlong;
      y += (PLANAR_STEP[direction] as GridPosition).y * cellsAlong;
    }
    if (random() < 0.15) {
      x += integer(-1, 1);
      y += integer(-1, 1);
    }
    while (cells.has(`${x}:${y}:${level}`)) x += 1;
    cells.add(`${x}:${y}:${level}`);
    rooms.push({ id, position: at(x, y, level) });
    edges.push({ from: parent.id, to: id, direction });
    if (random() < 0.8) {
      edges.push({ from: id, to: parent.id, direction: REVERSE_DIRECTION[direction] as LayoutDirection });
    }
  }
  for (let chord = 0; chord < Math.floor(count / 4); chord += 1) {
    const a = rooms[integer(0, rooms.length - 1)];
    const sameLevel = rooms.filter((room) => room !== a && room.position.level === a.position.level);
    if (sameLevel.length === 0) continue;
    const b = sameLevel[integer(0, sameLevel.length - 1)];
    edges.push({ from: a.id, to: b.id, direction: COMPASS_DIRECTIONS[integer(0, 3)] });
  }
  const [first, second, third, fourth] = rooms;
  edges.push(
    { from: first.id, to: first.id, direction: "East" },
    { ...edges[0] },
    { from: second.id, to: "ghost", direction: "North" },
    { from: second.id, to: third.id, direction: "In", constraintVector: { x: -0.4, y: 0.2, level: 0.6 } },
    { from: third.id, to: second.id, direction: "Other", constraintVector: { x: 0.6, y: 0, level: 0 } },
    { from: third.id, to: fourth.id, direction: "Other", constraintVector: at(2, 0) },
    { from: fourth.id, to: second.id, direction: "Northeast" },
  );
  return {
    positions: new Map(rooms.map((room) => [room.id, room.position])),
    edges,
    fixedIds: rooms.filter(() => random() < 0.1).map((room) => room.id),
  };
}

/** Every room of a mixed-level request where its chart or the map puts it. */
function mixedLevelPositions(request: IntegralLayoutRequest): Map<string, GridPosition> {
  const positions = new Map(request.residents.map((resident) => [resident.id, resident.position]));
  for (const node of request.nodes) if (!positions.has(node.id)) positions.set(node.id, node.relative);
  return positions;
}

/**
 * Candidates around a layout: itself, moved, stretched, shuffled, collided,
 * moved between levels, with a pinned room moved, with a room missing or
 * extra, off the integral grid, and translated beyond the numeric cell keys'
 * range, partly or wholly.
 */
function admissionCandidates(
  positions: ReadonlyMap<string, GridPosition>,
  fixedIds: readonly string[],
  random: () => number,
): Map<string, GridPosition>[] {
  const integer = (low: number, high: number): number =>
    low + Math.floor(random() * (high - low + 1));
  const ids = [...positions.keys()];
  const pinned = new Set(fixedIds);
  const movable = ids.filter((id) => !pinned.has(id));
  const pick = (from: readonly string[] = ids): string => from[integer(0, from.length - 1)];
  const cellOf = (position: GridPosition): string => `${position.x}:${position.y}:${position.level}`;
  const mapped = (move: (position: GridPosition) => GridPosition): Map<string, GridPosition> =>
    new Map([...positions].map(([id, position]) => [id, move(position)]));
  const copy = (): Map<string, GridPosition> => mapped((position) => ({ ...position }));
  const withRoom = (id: string, position: GridPosition): Map<string, GridPosition> =>
    copy().set(id, position);
  const moved = (count: number, planar: boolean): Map<string, GridPosition> => {
    const candidate = copy();
    const occupied = new Set([...candidate.values()].map(cellOf));
    for (let move = 0; move < count && movable.length > 0; move += 1) {
      const id = pick(movable);
      const from = candidate.get(id) as GridPosition;
      const to = at(
        from.x + integer(-2, 2),
        from.y + integer(-2, 2),
        from.level + (planar ? 0 : integer(-1, 1)),
      );
      if (occupied.has(cellOf(to))) continue;
      occupied.delete(cellOf(from));
      occupied.add(cellOf(to));
      candidate.set(id, to);
    }
    return candidate;
  };
  const translated = (dx: number, dy: number, dl: number): Map<string, GridPosition> =>
    mapped((position) => at(position.x + dx, position.y + dy, position.level + dl));
  const shuffled = (keepLevels: boolean): Map<string, GridPosition> => {
    const candidate = copy();
    const classes = new Map<number | string, string[]>();
    for (const [id, position] of positions) {
      const key = keepLevels ? position.level : "all";
      classes.set(key, [...(classes.get(key) ?? []), id]);
    }
    for (const members of classes.values()) {
      const cells = members.map((id) => positions.get(id) as GridPosition);
      for (let index = cells.length - 1; index > 0; index -= 1) {
        const other = integer(0, index);
        [cells[index], cells[other]] = [cells[other], cells[index]];
      }
      members.forEach((id, index) => candidate.set(id, { ...cells[index] }));
    }
    return candidate;
  };
  const a = pick();
  const b = pick(ids.filter((id) => id !== a));
  const origin = positions.get(a) as GridPosition;
  const limit = 2 ** 19;
  const beyond = translated(limit + 5, -limit - 5, 0);
  beyond.set(b, { ...(beyond.get(a) as GridPosition) });
  const missing = copy();
  missing.delete(a);
  const replaced = copy();
  replaced.delete(a);
  replaced.set("ghost", { ...origin });
  const candidates = [
    copy(),
    moved(1, true),
    moved(3, true),
    moved(ids.length, true),
    moved(3, false),
    mapped((position) => at(position.x * 2, position.y * 2, position.level)),
    translated(3, -2, 0),
    translated(0, 0, 1),
    shuffled(false),
    shuffled(true),
    withRoom(b, { ...origin }),
    withRoom(a, at(origin.x, origin.y, origin.level + 1)),
    missing,
    copy().set("ghost", at(99, 99)),
    replaced,
    withRoom(a, at(origin.x + 0.5, origin.y, origin.level)),
    withRoom(a, at(Number.NaN, origin.y, origin.level)),
    withRoom(a, at(origin.x, Number.POSITIVE_INFINITY, origin.level)),
    withRoom(a, at(origin.x, origin.y, Number.NaN)),
    mapped((position) => at(position.x || -0, position.y || -0, position.level || -0)),
    translated(limit, 0, 0),
    translated(0, -limit, 0),
    translated(-limit + 1, 0, 0),
    translated(0, 0, 2 ** 10),
    translated(limit - 1 - origin.x, 0, 0),
    beyond,
    translated(2 ** 52, 0, 0),
    translated(Number.MAX_SAFE_INTEGER, 0, 0),
  ];
  const pinnedRoom = fixedIds.find((id) => positions.has(id));
  if (pinnedRoom) {
    const position = positions.get(pinnedRoom) as GridPosition;
    candidates.push(withRoom(pinnedRoom, at(position.x + 1, position.y, position.level)));
  }
  return candidates;
}

test("allocation-free candidate admission returns the reference verdicts", () => {
  const random = xorshift32(0x5eed_a11c);
  const bases: {
    label: string;
    positions: Map<string, GridPosition>;
    edges: LayoutEdge[];
    fixedIds: string[];
  }[] = [];
  for (let seed = 1; seed <= 80; seed += 1) {
    bases.push({ label: `tree ${seed}`, ...admissionFixture(seed * 104_729) });
    const request = mixedLevelRequest(seed * 7_919);
    bases.push({
      label: `mixed ${seed}`,
      positions: mixedLevelPositions(request),
      edges: [...request.edges],
      fixedIds: request.residents.filter((resident) => !resident.movable).map((resident) => resident.id),
    });
  }
  let admittedAny = 0;
  let admittedWithMask = 0;
  let checks = 0;
  let graphsWithLevelRelations = 0;
  let levelChecks = 0;
  let levelAdmitted = 0;
  for (const base of bases) {
    const admission = internals.admission(base.positions, base.edges, base.fixedIds);
    const { groupCount, relationCount } = admission;
    if (relationCount > groupCount) graphsWithLevelRelations += 1;
    const candidates = admissionCandidates(base.positions, base.fixedIds, random);
    for (const [index, candidate] of candidates.entries()) {
      const label = `${base.label}, candidate ${index}`;
      const implied = admission.referenceRemovedGroups(candidate);
      assert.deepEqual(admission.removedGroups(candidate), implied, `${label}: implied mask`);
      const acceptsAny = admission.referenceHardValid(candidate, implied);
      assert.equal(admission.acceptsAny(candidate), acceptsAny, `${label}: any mask`);
      if (acceptsAny) admittedAny += 1;
      const masks = [
        implied,
        new Uint8Array(relationCount),
        new Uint8Array(relationCount).fill(1),
        Uint8Array.from(implied, () => random() < 0.3 ? 1 : 0),
        // Every level relation given up, and every directional group kept.
        Uint8Array.from({ length: relationCount }, (_value, relation) => relation < groupCount ? 0 : 1),
      ];
      for (const [maskIndex, mask] of masks.entries()) {
        const expected = admission.referenceHardValid(candidate, mask);
        assert.equal(admission.hardValid(candidate, mask), expected, `${label}: mask ${maskIndex}`);
        if (expected) admittedWithMask += 1;
        checks += 1;
        if (mask.subarray(groupCount).some((relaxed) => relaxed !== 0)) {
          levelChecks += 1;
          if (expected) levelAdmitted += 1;
        }
      }
    }
  }
  // The comparison means something only while both verdicts are common.
  assert.ok(admittedAny >= 500, `only ${admittedAny} candidates admitted under their own mask`);
  assert.ok(admittedWithMask >= 1_000, `only ${admittedWithMask} of ${checks} mask checks admitted`);
  assert.ok(checks - admittedWithMask >= 10_000, `only ${checks - admittedWithMask} mask checks rejected`);
  // Level relations, too, are given up in admitted and rejected candidates.
  assert.ok(graphsWithLevelRelations >= 20, `only ${graphsWithLevelRelations} graphs have level relations`);
  assert.ok(levelAdmitted >= 200, `only ${levelAdmitted} of ${levelChecks} level-relaxing checks admitted`);
  assert.ok(levelChecks - levelAdmitted >= 1_000, `only ${levelChecks - levelAdmitted} rejected`);
});
