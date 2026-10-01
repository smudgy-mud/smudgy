import assert from "node:assert/strict";
import test from "node:test";
import {
  compareLayoutQuality,
  measureIntegralLayoutQuality,
  planIntegralLayout,
  repairIntegralLayoutCrossingsDeep,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
  type LayoutEdge,
  type LayoutQuality,
  type LayoutTraceCandidate,
  type LayoutTraceEvent,
} from "./layout.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });
type CrossingRepairTraceEvent = Extract<LayoutTraceEvent, { type: "crossing-repair" }>;
type CrossingProgressTraceEvent = Extract<LayoutTraceEvent, { type: "crossing-progress" }>;

function isCrossingRepairEvent(event: LayoutTraceEvent): event is CrossingRepairTraceEvent {
  return event.type === "crossing-repair";
}

function isCrossingProgressEvent(event: LayoutTraceEvent): event is CrossingProgressTraceEvent {
  return event.type === "crossing-progress";
}

function positionsFromTrace(candidate: LayoutTraceCandidate): Map<string, GridPosition> {
  assert.ok(candidate.positions, "crossing repair candidates always include positions");
  return new Map(candidate.positions.map(({ id, x, y, level }) => [id, { x, y, level }]));
}

function planFingerprint(plan: IntegralLayoutPlan): string {
  return JSON.stringify({
    positions: [...plan.positions].sort(([left], [right]) => left.localeCompare(right)),
    movedExisting: [...plan.movedExisting].sort(),
    quality: plan.quality,
  });
}

function sixCrossingCluster(): IntegralLayoutRequest {
  // A synthetic six-crossing nested bridge tree: several one-room leaves hang
  // from a deeper movable lobe.
  const cells = [
    [4, 4],
    [5, -2],
    [4, 1],
    [-3, -3],
    [4, 0],
    [3, -2],
    [3, 2],
    [1, 0],
    [6, -5],
    [-2, 2],
    [4, -6],
    [0, -6],
    [-1, -3],
    [-1, 4],
  ] as const;
  const parents = [0, 1, 0, 3, 4, 5, 2, 1, 4, 3, 9, 2, 5] as const;
  const id = (index: number): string => `cluster-${index}`;
  const residents = cells.map(([x, y], index) => ({
    id: id(index),
    position: at(x, y),
    movable: index !== 0,
  }));
  const edges: LayoutEdge[] = parents.flatMap((parent, offset) => {
    const child = offset + 1;
    return [
      { from: id(parent), to: id(child), direction: "Other" },
      { from: id(child), to: id(parent), direction: "Other" },
    ];
  });
  return {
    residents,
    nodes: [],
    edges,
    centerId: id(0),
    allowExistingMoves: true,
  };
}

/**
 * A nested bridge tree laid out with every ray on its exit and no route
 * blocked, leaving three crossings that only nested transactions clear.
 */
function nestedDeepFixture(): IntegralLayoutRequest {
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
  ] as const;
  const reverses = {
    North: "South",
    East: "West",
    South: "North",
    West: "East",
    Other: "Other",
  } as const;
  const residents = cells.map(([x, y], index) => ({
    id: `nested-${index}`,
    position: at(x, y),
    movable: index !== 0,
  }));
  const edges: LayoutEdge[] = parents.flatMap((parent, offset) => {
    const child = offset + 1;
    const forward = directions[offset];
    return [
      { from: `nested-${parent}`, to: `nested-${child}`, direction: forward },
      { from: `nested-${child}`, to: `nested-${parent}`, direction: reverses[forward] },
    ];
  });
  return {
    residents,
    nodes: [],
    edges,
    centerId: "nested-0",
    allowExistingMoves: true,
  };
}

function budgetMonotonicFixture(): IntegralLayoutRequest {
  const cells = [
    [-7, -4],
    [-8, -4],
    [-2, 2],
    [-8, -2],
    [4, 3],
    [8, -5],
    [3, 3],
    [-2, 7],
    [2, -6],
    [-6, 8],
    [3, 5],
    [5, 7],
    [8, -8],
  ] as const;
  const parents = [0, 0, 0, 1, 1, 2, 4, 1, 6, 8, 5, 2] as const;
  const residents = cells.map(([x, y], index) => ({
    id: `budget-${index}`,
    position: at(x, y),
    movable: index !== 0,
  }));
  const edges: LayoutEdge[] = parents.flatMap((parent, offset) => {
    const child = offset + 1;
    return [
      { from: `budget-${parent}`, to: `budget-${child}`, direction: "Other" },
      { from: `budget-${child}`, to: `budget-${parent}`, direction: "Other" },
    ];
  });
  return {
    residents,
    nodes: [],
    edges,
    centerId: "budget-0",
    allowExistingMoves: true,
  };
}

test("quick repair accepts the six-crossing nested-lobe transaction and reports its bounded work", () => {
  const events: LayoutTraceEvent[] = [];
  const request = sixCrossingCluster();
  const result = planIntegralLayout({ ...request, trace: (event) => events.push(event) });
  const repairs = events.filter(isCrossingRepairEvent).filter((event) => event.mode === "quick");
  assert.ok(repairs.length > 0);
  const first = repairs[0];
  assert.equal(first.before.quality.linkCrossings, 6);
  assert.equal(first.after.quality.linkCrossings, 1);
  assert.ok(compareLayoutQuality(first.after.quality, first.before.quality) > 0);
  assert.equal(first.after.positions?.length, 14);
  assert.ok(result.quality.linkCrossings <= first.after.quality.linkCrossings);
  assert.deepEqual(result.positions.get("cluster-0"), at(4, 4), "the fixed center never moves");

  const complete = events.filter(isCrossingProgressEvent).find((event) =>
    event.mode === "quick" && event.status === "complete"
  );
  assert.ok(complete);
  assert.equal(complete.macrosConsidered, 48);
  assert.ok(complete.crossingsConsidered >= 6);
  assert.ok(complete.pushClosures > 0);

  // A direct slide can erase all crossings by lining up the tree, but doing so
  // routes many links through intervening rooms. The public tuple must reject
  // that tempting 6 -> 0 false solution before considering crossings.
  const cleanSeed = positionsFromTrace(first.before);
  const naive = new Map(
    [...cleanSeed.keys()].sort().map((id, index) => [id, at(index, 0)]),
  );
  const naiveQuality = measureIntegralLayoutQuality(naive, request.edges);
  assert.equal(naiveQuality.linkCrossings, 0);
  assert.ok(naiveQuality.routingViolations > first.before.quality.routingViolations);
  assert.ok(naiveQuality.roomObstructions > first.before.quality.roomObstructions);
  assert.ok(compareLayoutQuality(naiveQuality, first.before.quality) < 0);
});

test("deep repair composes strict nested transactions and publishes accepted complete maps only", () => {
  const request = nestedDeepFixture();
  const seed = planIntegralLayout(request);
  assert.ok(seed.quality.linkCrossings > 0);
  const trace: LayoutTraceEvent[] = [];
  const progressKinds: string[] = [];
  const first = repairIntegralLayoutCrossingsDeep(
    { ...request, trace: (event) => trace.push(event) },
    seed,
    {
      maximumWork: 500,
      onProgress: (progress) => progressKinds.push(progress.kind),
    },
  );
  assert.equal(first.plan.quality.linkCrossings, 0);
  assert.equal(first.completed, true);
  assert.equal(first.cancelled, false);
  assert.equal(first.exhausted, false);
  assert.ok(first.stats.maxDepth >= 2);
  assert.ok(compareLayoutQuality(first.plan.quality, seed.quality) > 0);
  assert.deepEqual(first.plan.positions.get("nested-0"), seed.positions.get("nested-0"));

  const accepted = trace.filter(isCrossingRepairEvent).filter((event) => event.mode === "deep");
  assert.ok(accepted.length > 0);
  for (const event of accepted) {
    assert.ok(compareLayoutQuality(event.after.quality, event.before.quality) > 0);
    const positions = positionsFromTrace(event.after);
    assert.equal(positions.size, 12);
    assert.equal(
      new Set([...positions.values()].map(({ x, y, level }) => `${x},${y},${level}`)).size,
      positions.size,
      "an emitted transaction must be collision-free",
    );
  }
  assert.ok(compareLayoutQuality(first.plan.quality, accepted.at(-1)?.after.quality ?? seed.quality) >= 0);
  assert.equal(progressKinds.filter((kind) => kind === "improvement").length, accepted.length);
  const acceptedIndex = trace.findIndex((event) => event.type === "crossing-repair");
  const completeIndex = trace.map((event) =>
    event.type === "crossing-progress" && event.mode === "deep" && event.status === "complete"
  ).lastIndexOf(true);
  assert.ok(acceptedIndex >= 0 && completeIndex > acceptedIndex, "improvement arrives before completion");

  const second = repairIntegralLayoutCrossingsDeep(request, seed, { maximumWork: 500 });
  assert.equal(planFingerprint(second.plan), planFingerprint(first.plan));
  assert.deepEqual(second.stats, first.stats);
});

test("deep repair explored twice never publishes the same complete map twice", () => {
  const request = nestedDeepFixture();
  const seed = planIntegralLayout(request);
  const publications = (): string[] => {
    const events: LayoutTraceEvent[] = [];
    repairIntegralLayoutCrossingsDeep(
      { ...request, trace: (event) => events.push(event) },
      seed,
      { maximumWork: 500 },
    );
    return events.filter(isCrossingRepairEvent).map((event) =>
      JSON.stringify(
        [...positionsFromTrace(event.after)].sort(([left], [right]) => left.localeCompare(right)),
      )
    );
  };
  const first = publications();
  const second = publications();
  assert.ok(first.length > 0);
  assert.equal(new Set(first).size, first.length, "each published complete map is distinct");
  assert.deepEqual(second, first);
});

test("deep repair rejects candidates before frontier admission or progressive publication", () => {
  const request = nestedDeepFixture();
  const seed = planIntegralLayout(request);
  const seedKey = JSON.stringify(
    [...seed.positions].sort(([left], [right]) => left.localeCompare(right)),
  );
  const trace: LayoutTraceEvent[] = [];
  const progressKinds: string[] = [];
  const repaired = repairIntegralLayoutCrossingsDeep(
    { ...request, trace: (event) => trace.push(event) },
    seed,
    {
      maximumWork: 100,
      acceptsPositions: (positions) => JSON.stringify(
        [...positions].sort(([left], [right]) => left.localeCompare(right)),
      ) === seedKey,
      onProgress: (progress) => progressKinds.push(progress.kind),
    },
  );

  assert.strictEqual(repaired.plan, seed);
  assert.equal(trace.some(isCrossingRepairEvent), false);
  assert.equal(progressKinds.includes("improvement"), false);
  assert.ok(repaired.stats.macrosConsidered > 0, "the filter guards admission, not search setup");
});

test("zero-crossing seeds return before resident, graph, cancellation, or search allocation", () => {
  let iterated = false;
  const forbidden = new Proxy([], {
    get(target, property, receiver) {
      if (property === Symbol.iterator) {
        iterated = true;
        throw new Error("zero-crossing gate iterated request data");
      }
      return Reflect.get(target, property, receiver);
    },
  });
  const quality: LayoutQuality = {
    cardinalRayViolations: 0,
    reciprocalRayViolations: 0,
    routingViolations: 0,
    exitPortViolations: 0,
    reciprocalExitPortViolations: 0,
    roomObstructions: 0,
    linkCrossings: 0,
    cardinalSlack: 0,
    footprintArea: 0,
    footprintPerimeter: 0,
  };
  const seed: IntegralLayoutPlan = { positions: new Map(), movedExisting: new Set(), quality };
  const trace: LayoutTraceEvent[] = [];
  let cancellationChecks = 0;
  const result = repairIntegralLayoutCrossingsDeep(
    {
      residents: forbidden as IntegralLayoutRequest["residents"],
      nodes: forbidden as IntegralLayoutRequest["nodes"],
      edges: forbidden as IntegralLayoutRequest["edges"],
      allowExistingMoves: true,
      trace: (event) => trace.push(event),
    },
    seed,
    { shouldCancel: () => (cancellationChecks += 1) > 0 },
  );
  assert.equal(result.plan, seed);
  assert.equal(iterated, false);
  assert.equal(cancellationChecks, 0);
  assert.deepEqual(result.stats, {
    crossingsConsidered: 0,
    macrosConsidered: 0,
    pushClosures: 0,
    maxDepth: 0,
    visitedStates: 0,
  });
  assert.deepEqual(trace.map((event) => event.type), ["crossing-progress"]);
});

test("work cutoff and cancellation are deterministic and request-local", () => {
  const request = nestedDeepFixture();
  const seed = planIntegralLayout(request);
  const limitedA = repairIntegralLayoutCrossingsDeep(request, seed, { maximumWork: 1 });
  const limitedB = repairIntegralLayoutCrossingsDeep(request, seed, { maximumWork: 1 });
  assert.equal(limitedA.plan, seed);
  assert.equal(limitedB.plan, seed);
  assert.equal(limitedA.exhausted, true);
  assert.equal(limitedA.stats.macrosConsidered, 1);
  assert.deepEqual(limitedB.stats, limitedA.stats);

  let checks = 0;
  const cancelled = repairIntegralLayoutCrossingsDeep(request, seed, {
    maximumWork: Number.POSITIVE_INFINITY,
    shouldCancel: () => (checks += 1) > 5,
  });
  assert.equal(cancelled.cancelled, true);
  assert.equal(cancelled.plan, seed);
  assert.ok(cancelled.stats.macrosConsidered <= 5);
});

test("larger deep work budgets retain every better complete prefix result", () => {
  const request = budgetMonotonicFixture();
  const positions = new Map(request.residents.map((resident) => [resident.id, resident.position]));
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
  };
  assert.equal(seed.quality.linkCrossings, 10);
  let previous = seed;
  for (const maximumWork of [1, 5, 10, 20, 40, 80]) {
    const result = repairIntegralLayoutCrossingsDeep(request, seed, { maximumWork });
    assert.ok(
      compareLayoutQuality(result.plan.quality, previous.quality) >= 0,
      `work=${maximumWork} lost an earlier complete plan`,
    );
    previous = result.plan;
  }
  const small = repairIntegralLayoutCrossingsDeep(request, seed, { maximumWork: 20 });
  const large = repairIntegralLayoutCrossingsDeep(request, seed, { maximumWork: 80 });
  assert.equal(small.plan.quality.linkCrossings, 0);
  assert.ok(compareLayoutQuality(large.plan.quality, small.plan.quality) >= 0);
});

test("deep repair publishes a settled improvement before its search transaction finishes", () => {
  const request = budgetMonotonicFixture();
  const positions = new Map(request.residents.map((resident) => [resident.id, resident.position]));
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
  };
  const improvementWork: number[] = [];
  const result = repairIntegralLayoutCrossingsDeep(request, seed, {
    maximumWork: 40,
    onProgress: (progress) => {
      if (progress.kind === "improvement") improvementWork.push(progress.macrosConsidered);
    },
  });

  assert.equal(result.completed, true);
  assert.equal(result.exhausted, false);
  assert.ok(improvementWork.length > 0);
  assert.ok(improvementWork[0] < result.stats.macrosConsidered);
});

/** Strict proper-crossing test for two same-level segments. */
function segmentsCrossStrictly(
  a: GridPosition,
  b: GridPosition,
  c: GridPosition,
  d: GridPosition,
): boolean {
  const cross = (p: GridPosition, q: GridPosition, r: GridPosition): number =>
    (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
  const abC = cross(a, b, c);
  const abD = cross(a, b, d);
  const cdA = cross(c, d, a);
  const cdB = cross(c, d, b);
  return ((abC < 0 && abD > 0) || (abC > 0 && abD < 0)) &&
    ((cdA < 0 && cdB > 0) || (cdA > 0 && cdB < 0));
}

/** Every integral cell an axis-aligned polyline passes through, in order. */
function polylineCells(points: readonly GridPosition[]): GridPosition[] {
  const cells: GridPosition[] = [];
  for (let index = 1; index < points.length; index += 1) {
    const from = points[index - 1];
    const to = points[index];
    assert.ok(from.x === to.x || from.y === to.y, "detour segments are axis-aligned");
    const stepX = Math.sign(to.x - from.x);
    const stepY = Math.sign(to.y - from.y);
    let cursor = from;
    while (cursor.x !== to.x || cursor.y !== to.y) {
      cells.push(cursor);
      cursor = { x: cursor.x + stepX, y: cursor.y + stepY, level: cursor.level };
    }
  }
  cells.push(points[points.length - 1]);
  return cells;
}

function fixedBridgeFixture(): {
  residents: IntegralLayoutRequest["residents"];
  edges: LayoutEdge[];
  seed: IntegralLayoutPlan;
} {
  const residents = [
    { id: "west", position: at(-2, 0), movable: false },
    { id: "east", position: at(2, 0), movable: false },
    { id: "north", position: at(0, -2), movable: false },
    { id: "south", position: at(0, 2), movable: false },
  ];
  const edges: LayoutEdge[] = [
    { from: "west", to: "east", direction: "Other" },
    { from: "north", to: "south", direction: "Other" },
  ];
  const positions = new Map(residents.map((resident) => [resident.id, resident.position]));
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  return { residents, edges, seed };
}

test("fixed bridge sides force exact seed fallback even with an infinite budget", () => {
  const { residents, edges, seed } = fixedBridgeFixture();
  assert.equal(seed.quality.linkCrossings, 1);
  const result = repairIntegralLayoutCrossingsDeep(
    { residents, nodes: [], edges, centerId: "west", allowExistingMoves: true },
    seed,
    { maximumWork: Number.POSITIVE_INFINITY },
  );
  assert.equal(result.plan, seed);
  assert.equal(result.completed, true);
  assert.equal(result.exhausted, false);
  assert.equal(result.stats.macrosConsidered, 0);
  assert.deepEqual(result.plan.positions.get("west"), at(-2, 0));

  // Movement is provably out of options here, so the engine proposes a
  // declarative route amendment instead: one link of the permanent crossing
  // gets an orthogonal detour that dodges both the other link and every room.
  const amendments = result.routeAmendments;
  assert.ok(amendments, "a permanent fixed-endpoint crossing proposes a detour");
  assert.equal(amendments.length, 1);
  const [amendment] = amendments;
  const pair = [amendment.from, amendment.to].sort().join("|");
  assert.ok(pair === "east|west" || pair === "north|south");
  assert.ok(amendment.waypoints.length >= 1);
  const rooms = new Map(residents.map((resident) => [resident.id, resident.position]));
  const route = [
    rooms.get(amendment.from) as GridPosition,
    ...amendment.waypoints,
    rooms.get(amendment.to) as GridPosition,
  ];
  const otherIds = pair === "east|west" ? ["north", "south"] : ["west", "east"];
  const otherFrom = rooms.get(otherIds[0]) as GridPosition;
  const otherTo = rooms.get(otherIds[1]) as GridPosition;
  for (let index = 1; index < route.length; index += 1) {
    assert.equal(
      segmentsCrossStrictly(route[index - 1], route[index], otherFrom, otherTo),
      false,
      "the drawn detour never crosses the surviving straight link",
    );
  }
  const occupiedCells = new Set(
    residents.map((resident) => `${resident.position.x},${resident.position.y}`),
  );
  const cells = polylineCells(route);
  for (const cell of cells.slice(1, -1)) {
    assert.equal(cell.level, 0);
    assert.ok(Number.isInteger(cell.x) && Number.isInteger(cell.y));
    assert.equal(
      occupiedCells.has(`${cell.x},${cell.y}`),
      false,
      `detour cell ${cell.x},${cell.y} passes through a room`,
    );
  }

  // The amendment is presentation-layer truth only: the metrics still count
  // the geometric crossing of the straight segments.
  assert.equal(result.plan.quality.linkCrossings, 1);

  const again = repairIntegralLayoutCrossingsDeep(
    { residents, nodes: [], edges, centerId: "west", allowExistingMoves: true },
    seed,
    { maximumWork: Number.POSITIVE_INFINITY },
  );
  assert.deepEqual(again.routeAmendments, amendments);
});

test("a walled-in fixed crossing emits no amendment and keeps today's exact behavior", () => {
  const { residents: core, edges } = fixedBridgeFixture();
  const ring: Array<IntegralLayoutRequest["residents"][number]> = [];
  for (let x = -3; x <= 3; x += 1) {
    for (let y = -3; y <= 3; y += 1) {
      if (Math.abs(x) === 3 || Math.abs(y) === 3) {
        ring.push({ id: `ring-${x}-${y}`, position: at(x, y), movable: false });
      }
    }
  }
  const residents = [...core, ...ring];
  const positions = new Map(residents.map((resident) => [resident.id, resident.position]));
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, edges),
  };
  assert.equal(seed.quality.linkCrossings, 1);
  const result = repairIntegralLayoutCrossingsDeep(
    { residents, nodes: [], edges, centerId: "west", allowExistingMoves: true },
    seed,
    { maximumWork: Number.POSITIVE_INFINITY },
  );
  assert.equal(result.plan, seed);
  assert.equal(result.stats.macrosConsidered, 0);
  assert.equal(result.routeAmendments, undefined);
});

test("iterative bridge decomposition handles a 15,000-room chain with a crossing", { timeout: 10_000 }, () => {
  const count = 15_000;
  const leading = [[-2, 0], [2, 0], [3, 0], [3, -2], [0, -2], [0, 2]] as const;
  const positions = new Map<string, GridPosition>();
  const residents: Array<IntegralLayoutRequest["residents"][number]> = [];
  for (let index = 0; index < count; index += 1) {
    const [x, y] = index < leading.length ? leading[index] : [index - 2, 2];
    const position = at(x, y);
    positions.set(`chain-${index}`, position);
    residents.push({ id: `chain-${index}`, position, movable: index !== 0 });
  }
  const edges: LayoutEdge[] = [];
  for (let index = 1; index < count; index += 1) {
    edges.push({ from: `chain-${index - 1}`, to: `chain-${index}`, direction: "Other" });
  }
  const seed: IntegralLayoutPlan = {
    positions,
    movedExisting: new Set(),
    // The work=0 test deliberately trusts the already-measured seed tuple; it
    // still enumerates the actual crossing and decomposes every bridge.
    quality: {
      cardinalRayViolations: 0,
      reciprocalRayViolations: 0,
      routingViolations: 0,
      exitPortViolations: 0,
      reciprocalExitPortViolations: 0,
      roomObstructions: 0,
      linkCrossings: 1,
      cardinalSlack: 0,
      footprintArea: count,
      footprintPerimeter: count,
    },
  };
  const result = repairIntegralLayoutCrossingsDeep(
    { residents, nodes: [], edges, centerId: "chain-0", allowExistingMoves: true },
    seed,
    { maximumWork: 0 },
  );
  assert.equal(result.plan, seed);
  assert.equal(result.exhausted, true);
  assert.equal(result.stats.crossingsConsidered, 1);
  assert.equal(result.stats.macrosConsidered, 0);
});
