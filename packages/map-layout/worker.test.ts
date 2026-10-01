import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import {
  compareLayoutQuality,
  createLayoutPlanner,
  planIntegralLayout,
  planIntegralLayoutAsync,
  type GridPosition,
  type IntegralLayoutRequest,
  type LayoutDirection,
  type LayoutTraceEvent,
} from "./layout.ts";
import {
  createLayoutModel,
  createLayoutWorkspace,
  planLayoutModel,
  planLayoutModelAsync,
  type LayoutChange,
  type LayoutModel,
  type PlanLayoutOptions,
  type ReflowLayoutChange,
} from "./model.ts";
import {
  layoutSnapshotKey,
  planStableLayoutSnapshot,
  sameLayoutSnapshot,
  StaleLayoutSnapshotError,
} from "./stable-snapshot.ts";
import {
  LayoutWorkerClient,
  setLayoutWorkerFactoryForTesting,
  type LayoutWorkerLike,
} from "./worker-client.ts";
import { executeLayoutWorkerRequest } from "./worker-executor.ts";
import type { LayoutWorkerRequest } from "./worker-protocol.ts";
import {
  decodeIntegralLayoutPlan,
  encodeIntegralLayoutPlan,
  isLayoutWorkerProgress,
  isLayoutWorkerResponse,
  LAYOUT_WORKER_PROTOCOL_VERSION,
  MAX_ROUTE_AMENDMENT_WAYPOINTS,
  type IntegralLayoutWirePlan,
} from "./worker-protocol.ts";
import { layoutPlannerState, type LayoutPlannerProgress } from "./planner-state.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });

function requestToward(
  direction: "North" | "East",
  trace?: (event: LayoutTraceEvent) => void,
): IntegralLayoutRequest {
  const relative = direction === "East" ? at(1, 0) : at(0, -1);
  return {
    residents: [{ id: "start", position: at(0, 0), movable: true }],
    nodes: [
      { id: "start", relative: at(0, 0) },
      { id: "new", relative },
    ],
    edges: [{ from: "start", to: "new", direction }],
    centerId: "start",
    allowExistingMoves: false,
    trace,
  };
}

function collidingRequest(
  trace?: (event: LayoutTraceEvent) => void,
): IntegralLayoutRequest {
  return {
    residents: [
      { id: "first", position: at(0, 0), movable: false },
      { id: "second", position: at(0, 0), movable: false },
    ],
    nodes: [],
    edges: [],
    allowExistingMoves: false,
    trace,
  };
}

function constraintConflictRequest(): IntegralLayoutRequest {
  return {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(1, 0), movable: true },
      { id: "c", position: at(2, 0), movable: true },
    ],
    nodes: [],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "a", direction: "West" },
      { from: "b", to: "c", direction: "East" },
      { from: "c", to: "a", direction: "East" },
    ],
    allowExistingMoves: true,
  };
}

function twoRoomModel(eastX = 5): LayoutModel {
  return createLayoutModel({
    rooms: [
      { id: "start", roomNumber: 1, position: at(0, 0), movable: true },
      { id: "east", roomNumber: 2, position: at(eastX, 0), movable: true },
    ],
    edges: [
      { from: "start", to: "east", direction: "East" },
      { from: "east", to: "start", direction: "West" },
    ],
  });
}

class ControlledWorker implements LayoutWorkerLike {
  onmessage: LayoutWorkerLike["onmessage"] = null;
  onmessageerror: LayoutWorkerLike["onmessageerror"] = null;
  onerror: LayoutWorkerLike["onerror"] = null;
  readonly requests: LayoutWorkerRequest[] = [];
  terminated = false;

  postMessage(message: unknown): void {
    this.requests.push(structuredClone(message) as LayoutWorkerRequest);
  }

  terminate(): void {
    this.terminated = true;
  }

  respondAt(index: number): void {
    const [request] = this.requests.splice(index, 1);
    assert.ok(request, `missing fake Worker request at ${index}`);
    const response = structuredClone(executeLayoutWorkerRequest(request, (event) => {
      this.onmessage?.({ data: structuredClone({
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id: request.id,
        operation: request.operation,
        progress: true,
        event,
      }) });
    }));
    this.onmessage?.({ data: response });
  }
}

test("planner state and request progress expose work, quality, and complete layouts", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const progress: LayoutPlannerProgress[] = [];
  const seenStates: string[] = [];
  const unsubscribe = layoutPlannerState.subscribe((snapshot) => seenStates.push(snapshot.status));
  try {
    const result = await planIntegralLayoutAsync(requestToward("East"), {
      onProgress: (update) => progress.push(update as LayoutPlannerProgress),
      plannerContext: {
        source: "test",
        areaId: "area:1",
        areaName: "Test Area",
        contextKey: "coverage",
      },
    });
    const final = layoutPlannerState.value;
    assert.equal(final.status, "completed");
    assert.equal(final.terminalReason, "completed");
    assert.deepEqual(final.context, {
      source: "test",
      areaId: "area:1",
      areaName: "Test Area",
      contextKey: "coverage",
    });
    assert.ok(final.work.layoutsConsidered > 0);
    assert.deepEqual(final.currentQuality, {
      cardinalRayViolations: 0,
      reciprocalRayViolations: 0,
      levelViolations: 0,
      routingViolations: 0,
      exitPortViolations: 0,
      reciprocalExitPortViolations: 0,
      roomObstructions: 0,
      linkCrossings: 0,
      levelSlack: 0,
      cardinalSlack: 0,
      footprintArea: 1,
      footprintPerimeter: 4,
    });
    assert.deepEqual(final.bestQuality, result.quality);
    assert.ok(progress.some((update) => update.improvement?.positions instanceof Map));
    assert.ok(seenStates.includes("planning"));
    assert.ok(seenStates.includes("completed"));
  } finally {
    unsubscribe();
  }
});

test("a whole-map polish streams its preview as an improvement before the plan", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const updates: LayoutPlannerProgress[] = [];
  const request: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: at(0, 0), movable: false },
      { id: "b", position: at(5, 0), movable: true },
    ],
    nodes: [],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "a", direction: "West" },
    ],
    allowExistingMoves: true,
  };
  const plan = await planIntegralLayoutAsync(request, { onProgress: (update) => updates.push(update) });
  const previewed = updates.find((update) => update.snapshot.phase === "preview")?.improvement;
  assert.ok(previewed, "the preview arrives as an improvement");
  assert.deepEqual(previewed.positions.get("b"), at(1, 0));
  assert.ok(compareLayoutQuality(plan.quality, previewed.quality) >= 0);
});

test("planner state receives live axis-compaction candidate counts", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const progress: LayoutPlannerProgress[] = [];
  const residents = [
    { id: "anchor", position: at(0, 0), movable: false },
    { id: "second-anchor", position: at(6, 0), movable: false },
    { id: "corridor-a", position: at(0, 4), movable: true },
    { id: "corridor-b", position: at(3, 4), movable: true },
    { id: "corridor-c", position: at(6, 4), movable: true },
    { id: "blocker-1", position: at(10, 1), movable: false },
    { id: "blocker-2", position: at(10, 2), movable: false },
    { id: "blocker-3", position: at(10, 3), movable: false },
  ];
  await planIntegralLayoutAsync({
    centerId: "anchor",
    allowExistingMoves: true,
    residents,
    nodes: [],
    edges: [
      { from: "corridor-a", to: "corridor-b", direction: "East" },
      { from: "corridor-b", to: "corridor-a", direction: "West" },
      { from: "corridor-b", to: "corridor-c", direction: "East" },
      { from: "corridor-c", to: "corridor-b", direction: "West" },
      { from: "anchor", to: "corridor-a", direction: "South" },
      { from: "second-anchor", to: "corridor-c", direction: "South" },
    ],
  }, {
    onProgress: (update) => progress.push(update as LayoutPlannerProgress),
  });

  const axis = progress.filter((update) => update.snapshot.phase.startsWith("axis "));
  assert.ok(axis.some((update) => update.snapshot.phase === "axis gravity"));
  assert.ok(axis.some((update) => update.snapshot.phase === "axis spacing"));
  assert.ok(axis.every((update, index) => index === 0 ||
    update.snapshot.work.layoutsConsidered >= axis[index - 1].snapshot.work.layoutsConsidered
  ));
  assert.ok((axis.at(-1)?.snapshot.work.layoutsConsidered ?? 0) > 0);
});

test("nested progress cannot regress best quality or whole-operation elapsed time", async () => {
  const persistent = new ControlledWorker();
  let repair: ControlledWorker | undefined;
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    if (purpose === "persistent") return persistent;
    return repair = new ControlledWorker();
  });
  const request = requestToward("East");
  const standard = planIntegralLayout(request);
  const firstQuality = { ...standard.quality, cardinalRayViolations: 1 };
  const regressedQuality = { ...standard.quality, cardinalRayViolations: 2 };
  const updates: LayoutPlannerProgress[] = [];
  const pending = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
    onProgress: (update) => updates.push(update as LayoutPlannerProgress),
  });
  const posted = persistent.requests[0];
  assert.ok(posted);
  const sendPersistentProgress = (event: LayoutTraceEvent): void => {
    persistent.onmessage?.({ data: structuredClone({
      protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
      id: posted.id,
      operation: posted.operation,
      progress: true,
      event,
    }) });
  };

  sendPersistentProgress({
    type: "axis-progress",
    stage: "axis-compaction",
    phase: "gravity",
    candidatesConsidered: 1,
    complete: false,
    elapsedMs: 1,
    bestQuality: firstQuality,
  });
  assert.deepEqual(updates.at(-1)?.snapshot.bestQuality, firstQuality);
  sendPersistentProgress({
    type: "axis-progress",
    stage: "axis-compaction",
    phase: "spacing",
    candidatesConsidered: 2,
    complete: true,
    elapsedMs: 2,
    bestQuality: regressedQuality,
  });
  assert.deepEqual(updates.at(-1)?.snapshot.bestQuality, firstQuality);
  sendPersistentProgress({
    type: "axis-progress",
    stage: "axis-compaction",
    phase: "gravity",
    // This fresh pass starts above the previous pass's final count. The
    // explicit boundary must still account for all three new candidates.
    candidatesConsidered: 3,
    complete: false,
    elapsedMs: 1,
    bestQuality: regressedQuality,
  });
  assert.equal(updates.at(-1)?.snapshot.work.layoutsConsidered, 5);
  assert.deepEqual(updates.at(-1)?.snapshot.bestQuality, firstQuality);
  sendPersistentProgress({
    type: "selection",
    stage: "initial-selection",
    selected: { quality: regressedQuality, movedExisting: [] },
  });
  assert.deepEqual(updates.at(-1)?.snapshot.bestQuality, firstQuality);

  persistent.respondAt(0);
  for (let attempt = 0; attempt < 20 && !repair; attempt += 1) await Promise.resolve();
  assert.ok(repair);
  const repairRequest = repair.requests[0];
  assert.ok(repairRequest && repairRequest.operation === "constraint-repair");
  const beforeRepairElapsed = updates.at(-1)?.snapshot.elapsedMs ?? 0;
  await new Promise((resolve) => setTimeout(resolve, 5));
  repair.onmessage?.({ data: structuredClone({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: repairRequest.id,
    operation: repairRequest.operation,
    progress: true,
    event: {
      type: "constraint-progress",
      stage: "constraint-repair",
      phase: "search",
      restarts: 1,
      feasibilityChecks: 2,
      layoutsConsidered: 0,
      compactionAttempts: 0,
      elapsedMs: 0,
      bestQuality: regressedQuality,
      rawIncumbents: 0,
      softIncumbents: 0,
      distinctLayouts: 0,
      maskDiversifications: 0,
      separatorStates: 0,
      separatorBranches: 0,
      separatorCyclePrunes: 0,
    },
  }) });
  const nested = updates.at(-1)?.snapshot;
  assert.ok((nested?.elapsedMs ?? 0) >= beforeRepairElapsed);
  assert.deepEqual(nested?.bestQuality, standard.quality);

  repair.onerror?.({ error: new Error("finish fabricated repair") });
  assert.deepEqual(await pending, standard);
  assert.equal(updates.at(-1)?.snapshot.terminalReason, "degraded");
  assert.equal(updates.at(-1)?.snapshot.phase, "complete: standard fallback");
});

test("streamed crossing repair checkpoints expose work and a complete improvement", async () => {
  const worker = new ControlledWorker();
  const client = new LayoutWorkerClient(() => worker);
  const request = requestToward("East");
  const plan = planIntegralLayout(request);
  const progress: LayoutPlannerProgress[] = [];
  const pending = client.planIntegral(request, {
    onProgress: (update) => progress.push(update as LayoutPlannerProgress),
  });
  const posted = worker.requests[0];
  assert.ok(posted);

  worker.onmessage?.({ data: structuredClone({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: posted.id,
    operation: posted.operation,
    progress: true,
    event: {
      type: "crossing-progress",
      stage: "crossing-repair",
      mode: "deep",
      status: "progress",
      crossingsConsidered: 5,
      macrosConsidered: 8,
      pushClosures: 9,
      maxDepth: 1,
      visitedStates: 12,
      bestQuality: plan.quality,
    },
  }) });
  worker.onmessage?.({ data: structuredClone({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: posted.id,
    operation: posted.operation,
    progress: true,
    event: {
      type: "crossing-repair",
      stage: "crossing-repair",
      mode: "deep",
      iteration: 3,
      crossingsConsidered: 7,
      macrosConsidered: 11,
      pushClosures: 13,
      maxDepth: 2,
      visitedStates: 17,
      before: {
        quality: { ...plan.quality, linkCrossings: plan.quality.linkCrossings + 1 },
        movedExisting: [],
      },
      after: {
        quality: plan.quality,
        movedExisting: [...plan.movedExisting],
        positions: [...plan.positions].map(([id, position]) => ({ id, ...position })),
      },
    },
  }) });
  worker.onmessage?.({ data: structuredClone({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: posted.id,
    operation: posted.operation,
    progress: true,
    event: {
      type: "crossing-progress",
      stage: "crossing-repair",
      mode: "deep",
      status: "complete",
      crossingsConsidered: 8,
      macrosConsidered: 12,
      pushClosures: 14,
      maxDepth: 2,
      visitedStates: 19,
      bestQuality: plan.quality,
    },
  }) });

  const telemetry = progress.find((update) => update.snapshot.phase === "crossing deep progress");
  assert.ok(telemetry);
  assert.equal(telemetry.improvement, undefined);
  assert.equal(telemetry.snapshot.work.macrosConsidered, 8);
  const crossing = progress.find((update) => update.snapshot.phase === "crossing deep improvement");
  assert.ok(crossing);
  assert.deepEqual(crossing.snapshot.work, {
    layoutsConsidered: 0,
    compactionAttempts: 0,
    restarts: 0,
    feasibilityChecks: 0,
    rawIncumbents: 0,
    softIncumbents: 0,
    distinctLayouts: 0,
    maskDiversifications: 0,
    separatorStates: 0,
    separatorBranches: 0,
    separatorCyclePrunes: 0,
    crossingsConsidered: 7,
    macrosConsidered: 11,
    pushClosures: 13,
    maxDepth: 2,
    visitedStates: 17,
  });
  assert.deepEqual(crossing.snapshot.bestQuality, plan.quality);
  assert.deepEqual(crossing.improvement?.positions, plan.positions);
  assert.deepEqual(crossing.improvement?.movedExisting, plan.movedExisting);
  const complete = progress.find((update) => update.snapshot.phase === "crossing deep complete");
  assert.ok(complete);
  assert.equal(complete.improvement, undefined);
  assert.equal(complete.snapshot.work.visitedStates, 19);

  worker.respondAt(0);
  await pending;
});

test("the Worker boundary rejects colliding crossing candidates before publication", async () => {
  const worker = new ControlledWorker();
  const client = new LayoutWorkerClient(() => worker);
  const request = requestToward("East");
  const plan = planIntegralLayout(request);
  const progress: LayoutPlannerProgress[] = [];
  const pending = client.planIntegral(request, {
    onProgress: (update) => progress.push(update as LayoutPlannerProgress),
  });
  const rejected = assert.rejects(pending, /unexpected response/);
  const posted = worker.requests[0];
  assert.ok(posted);

  worker.onmessage?.({ data: structuredClone({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: posted.id,
    operation: posted.operation,
    progress: true,
    event: {
      type: "crossing-repair",
      stage: "crossing-repair",
      mode: "quick",
      iteration: 1,
      crossingsConsidered: 1,
      macrosConsidered: 1,
      pushClosures: 1,
      maxDepth: 1,
      visitedStates: 1,
      before: { quality: plan.quality, movedExisting: [] },
      after: {
        quality: plan.quality,
        movedExisting: [],
        positions: [
          { id: "start", ...at(0, 0) },
          { id: "new", ...at(0, 0) },
        ],
      },
    },
  }) });

  assert.equal(
    progress.some((update) => update.snapshot.phase === "crossing quick improvement"),
    false,
  );
  await rejected;
  assert.equal(worker.terminated, true);
});

test("a throwing onProgress observer stays request-local through streamed progress", async () => {
  const worker = new ControlledWorker();
  const client = new LayoutWorkerClient(() => worker);
  let observed = 0;
  const pending = client.planIntegral(requestToward("East"), {
    onProgress: () => {
      observed += 1;
      throw new Error("progress observer failed");
    },
  });
  const posted = worker.requests[0];
  assert.ok(posted);

  // A throw inside the observer must not escape the host's message dispatch.
  worker.onmessage?.({ data: structuredClone({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: posted.id,
    operation: posted.operation,
    progress: true,
    event: {
      type: "axis-progress",
      stage: "axis-compaction",
      phase: "gravity",
      candidatesConsidered: 1,
      complete: false,
      elapsedMs: 1,
    },
  }) });
  assert.ok(observed > 0);

  worker.respondAt(0);
  const result = await pending;
  assert.deepEqual(result.positions.get("new"), at(1, 0));
  assert.equal(worker.terminated, false);
});

class ExecutingWorker extends ControlledWorker {
  override postMessage(message: unknown): void {
    super.postMessage(message);
    queueMicrotask(() => {
      if (!this.terminated && this.requests.length > 0) this.respondAt(0);
    });
  }
}

test("multiple constraint incumbents publish only complete strict improvements with exact work", async () => {
  let repair: ControlledWorker | undefined;
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    if (purpose === "persistent") return new ExecutingWorker();
    return repair = new ControlledWorker();
  });
  const request = constraintConflictRequest();
  const standard = planIntegralLayout(request);
  assert.ok(standard.quality.cardinalRayViolations > 0);
  const updates: LayoutPlannerProgress[] = [];
  const pending = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
    onProgress: (update) => updates.push(update as LayoutPlannerProgress),
  });
  for (let attempt = 0; attempt < 20 && !repair; attempt += 1) await Promise.resolve();
  assert.ok(repair);
  const posted = repair.requests[0];
  assert.ok(posted && posted.operation === "constraint-repair");

  const positions = [...standard.positions].map(([id, position]) => ({ id, ...position }));
  const betterQuality = {
    ...standard.quality,
    cardinalRayViolations: standard.quality.cardinalRayViolations - 1,
  };
  const sendImprovement = (
    quality: typeof standard.quality,
    counters: {
      rawIncumbents: number;
      softIncumbents: number;
      distinctLayouts: number;
      maskDiversifications: number;
      separatorStates: number;
      separatorBranches: number;
      separatorCyclePrunes: number;
    },
  ): void => repair?.onmessage?.({ data: structuredClone({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: posted.id,
    operation: posted.operation,
    progress: true,
    event: {
      type: "constraint-improvement",
      stage: "constraint-repair",
      restarts: 4,
      feasibilityChecks: 7,
      layoutsConsidered: 2,
      compactionAttempts: 3,
      firstIncumbentMs: 12,
      ...counters,
      candidate: { quality, movedExisting: [], positions },
    },
  }) });

  sendImprovement(betterQuality, {
    rawIncumbents: 1,
    softIncumbents: 1,
    distinctLayouts: 1,
    maskDiversifications: 1,
    separatorStates: 21,
    separatorBranches: 34,
    separatorCyclePrunes: 5,
  });
  sendImprovement(betterQuality, {
    rawIncumbents: 2,
    softIncumbents: 1,
    distinctLayouts: 2,
    maskDiversifications: 2,
    separatorStates: 55,
    separatorBranches: 89,
    separatorCyclePrunes: 8,
  });
  sendImprovement(standard.quality, {
    rawIncumbents: 3,
    softIncumbents: 1,
    distinctLayouts: 3,
    maskDiversifications: 3,
    separatorStates: 76,
    separatorBranches: 123,
    separatorCyclePrunes: 13,
  });

  const improvements = updates.filter((update) =>
    update.snapshot.phase === "new best layout" && update.improvement
  );
  assert.equal(improvements.length, 1);
  assert.deepEqual(improvements[0].improvement?.quality, betterQuality);
  assert.equal(improvements[0].improvement?.positions.size, standard.positions.size);
  const latest = updates.at(-1)?.snapshot;
  assert.equal(latest?.firstIncumbentMs, 12);
  assert.deepEqual({
    rawIncumbents: latest?.work.rawIncumbents,
    softIncumbents: latest?.work.softIncumbents,
    distinctLayouts: latest?.work.distinctLayouts,
    maskDiversifications: latest?.work.maskDiversifications,
    separatorStates: latest?.work.separatorStates,
    separatorBranches: latest?.work.separatorBranches,
    separatorCyclePrunes: latest?.work.separatorCyclePrunes,
  }, {
    rawIncumbents: 3,
    softIncumbents: 1,
    distinctLayouts: 3,
    maskDiversifications: 3,
    separatorStates: 76,
    separatorBranches: 123,
    separatorCyclePrunes: 13,
  });

  repair.onerror?.({ error: new Error("finish fabricated repair") });
  const retained = await pending;
  assert.deepEqual(retained.positions, standard.positions);
  assert.deepEqual(retained.movedExisting, new Set());
  assert.deepEqual(retained.quality, betterQuality);
  assert.equal(retained.constraintRepair, undefined);
  assert.ok(compareLayoutQuality(retained.quality, standard.quality) > 0);
  assert.deepEqual(updates.at(-1)?.snapshot.bestQuality, betterQuality);
});

test("final integral responses reject malformed clone-safe plans", () => {
  const request: LayoutWorkerRequest = {
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 77,
    operation: "integral",
    collectTrace: false,
    streamProgress: false,
    request: requestToward("East"),
  };
  const response = executeLayoutWorkerRequest(request);
  assert.equal(isLayoutWorkerResponse(response), true);
  assert.equal(response.ok, true);
  if (!response.ok) return;

  const malformedQuality = structuredClone(response) as unknown as Record<string, unknown>;
  const qualityResult = malformedQuality.result as Record<string, unknown>;
  qualityResult.quality = { cardinalRayViolations: 0 };
  assert.equal(isLayoutWorkerResponse(malformedQuality), false);

  const malformedPosition = structuredClone(response) as unknown as Record<string, unknown>;
  const positionResult = malformedPosition.result as Record<string, unknown>;
  const positions = positionResult.positions as [string, GridPosition][];
  positions[0][1].x = Number.NaN;
  assert.equal(isLayoutWorkerResponse(malformedPosition), false);

  const malformedMovedId = structuredClone(response) as unknown as Record<string, unknown>;
  const movedResult = malformedMovedId.result as Record<string, unknown>;
  movedResult.movedExisting = ["not-in-the-plan"];
  assert.equal(isLayoutWorkerResponse(malformedMovedId), false);

  const duplicatePosition = structuredClone(response) as unknown as Record<string, unknown>;
  const duplicateResult = duplicatePosition.result as Record<string, unknown>;
  const duplicateEntries = duplicateResult.positions as [string, GridPosition][];
  duplicateEntries.push(structuredClone(duplicateEntries[0]));
  assert.equal(isLayoutWorkerResponse(duplicatePosition), false);

  const collidingPosition = structuredClone(response) as unknown as Record<string, unknown>;
  const collisionResult = collidingPosition.result as Record<string, unknown>;
  const collisionEntries = collisionResult.positions as [string, GridPosition][];
  collisionEntries[1][1] = structuredClone(collisionEntries[0][1]);
  assert.equal(isLayoutWorkerResponse(collidingPosition), false);

  const incompleteReport = structuredClone(response) as unknown as Record<string, unknown>;
  const reportResult = incompleteReport.result as Record<string, unknown>;
  reportResult.constraintRepair = {
    crossingRepair: {
      completed: true,
      cancelled: false,
      exhausted: false,
      elapsedMs: 1,
      crossingsConsidered: 1,
      macrosConsidered: 1,
      pushClosures: 1,
      maxDepth: 1,
      visitedStates: 1,
    },
  };
  assert.equal(isLayoutWorkerResponse(incompleteReport), false);
});

test("v11 final reports require exact work and termination fields", () => {
  const request = constraintConflictRequest();
  const response = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 81,
    operation: "constraint-repair",
    collectTrace: false,
    streamProgress: false,
    request,
    standard: encodeIntegralLayoutPlan(planIntegralLayout(request)),
    options: { when: "always", maxDurationMs: 1_000 },
  });
  assert.equal(response.ok, true);
  assert.equal(isLayoutWorkerResponse(response), true);
  if (!response.ok) return;

  for (const field of [
    "rawIncumbents",
    "softIncumbents",
    "distinctLayouts",
    "maskDiversifications",
    "separatorStates",
    "separatorBranches",
    "separatorCyclePrunes",
    "extensionSearch",
    "maskDiversification",
  ]) {
    const malformed = structuredClone(response) as unknown as Record<string, unknown>;
    const result = malformed.result as Record<string, unknown>;
    delete (result.constraintRepair as Record<string, unknown>)[field];
    assert.equal(isLayoutWorkerResponse(malformed), false, `${field} is required`);
  }

  for (const [section, fields] of [
    ["extensionSearch", ["completed", "cancelled", "exhausted"]],
    ["maskDiversification", ["completed", "exhausted"]],
  ] as const) {
    for (const field of fields) {
      const malformed = structuredClone(response) as unknown as Record<string, unknown>;
      const result = malformed.result as Record<string, unknown>;
      const report = result.constraintRepair as Record<string, unknown>;
      delete (report[section] as Record<string, unknown>)[field];
      assert.equal(isLayoutWorkerResponse(malformed), false, `${section}.${field} is required`);
    }
  }

  const contradictory = structuredClone(response) as unknown as Record<string, unknown>;
  const contradictoryResult = contradictory.result as Record<string, unknown>;
  const contradictoryReport = contradictoryResult.constraintRepair as Record<string, unknown>;
  contradictoryReport.extensionSearch = {
    completed: true,
    cancelled: true,
    exhausted: false,
  };
  assert.equal(isLayoutWorkerResponse(contradictory), false);

  for (const [field, value] of [
    ["polishAnchorsTried", (response.result.constraintRepair?.polishPasses ?? 0) + 1],
    ["polishImprovements", (response.result.constraintRepair?.polishPasses ?? 0) + 1],
    ["polishTournaments", (response.result.constraintRepair?.polishAnchorsTried ?? 0) + 1],
  ] as const) {
    const impossible = structuredClone(response) as unknown as Record<string, unknown>;
    const impossibleResult = impossible.result as Record<string, unknown>;
    const impossibleReport = impossibleResult.constraintRepair as Record<string, unknown>;
    impossibleReport[field] = value;
    assert.equal(isLayoutWorkerResponse(impossible), false, `${field} cannot exceed its work`);
  }

  const tooFewPassesForTournaments = structuredClone(response) as unknown as Record<string, unknown>;
  const tooFewPassesResult = tooFewPassesForTournaments.result as Record<string, unknown>;
  const tooFewPassesReport = tooFewPassesResult.constraintRepair as Record<string, unknown>;
  tooFewPassesReport.polishPasses = 3;
  tooFewPassesReport.polishAnchorsTried = 2;
  tooFewPassesReport.polishTournaments = 2;
  tooFewPassesReport.polishImprovements = 0;
  assert.equal(isLayoutWorkerResponse(tooFewPassesForTournaments), false);

  const falseFixedPoint = structuredClone(response) as unknown as Record<string, unknown>;
  const fixedPointResult = falseFixedPoint.result as Record<string, unknown>;
  const fixedPointReport = fixedPointResult.constraintRepair as Record<string, unknown>;
  fixedPointReport.geometricFixedPoint = true;
  fixedPointReport.extensionSearch = {
    completed: true,
    cancelled: false,
    exhausted: false,
  };
  fixedPointReport.maskDiversification = {
    completed: true,
    exhausted: false,
  };
  fixedPointReport.crossingRepair = {
    ...(fixedPointReport.crossingRepair as Record<string, unknown>),
    completed: false,
    cancelled: false,
    exhausted: true,
  };
  assert.equal(isLayoutWorkerResponse(falseFixedPoint), false);

  const interruptedFixedPoint = structuredClone(response) as unknown as Record<string, unknown>;
  const interruptedResult = interruptedFixedPoint.result as Record<string, unknown>;
  const interruptedReport = interruptedResult.constraintRepair as Record<string, unknown>;
  interruptedReport.geometricFixedPoint = true;
  interruptedReport.extensionSearch = {
    completed: true,
    cancelled: false,
    exhausted: false,
  };
  interruptedReport.maskDiversification = {
    completed: true,
    exhausted: false,
  };
  interruptedReport.crossingRepair = {
    ...(interruptedReport.crossingRepair as Record<string, unknown>),
    completed: true,
    cancelled: false,
    exhausted: false,
  };
  interruptedReport.polishCutoff = "time";
  assert.equal(isLayoutWorkerResponse(interruptedFixedPoint), false);

  const cutFixedPoint = structuredClone(response) as unknown as Record<string, unknown>;
  const cutFixedPointResult = cutFixedPoint.result as Record<string, unknown>;
  const cutFixedPointReport = cutFixedPointResult.constraintRepair as Record<string, unknown>;
  cutFixedPointReport.geometricFixedPoint = true;
  cutFixedPointReport.cutoff = "restarts";
  cutFixedPointReport.extensionSearch = {
    completed: true,
    cancelled: false,
    exhausted: false,
  };
  cutFixedPointReport.maskDiversification = {
    completed: true,
    exhausted: false,
  };
  cutFixedPointReport.crossingRepair = {
    ...(cutFixedPointReport.crossingRepair as Record<string, unknown>),
    completed: true,
    cancelled: false,
    exhausted: false,
  };
  cutFixedPointReport.polishCutoff = "fixed-point";
  assert.equal(isLayoutWorkerResponse(cutFixedPoint), false);

  const tournamentLimited = structuredClone(response) as unknown as Record<string, unknown>;
  const tournamentResult = tournamentLimited.result as Record<string, unknown>;
  const tournamentReport = tournamentResult.constraintRepair as Record<string, unknown>;
  tournamentReport.geometricFixedPoint = false;
  tournamentReport.polishCutoff = "tournaments";
  assert.equal(isLayoutWorkerResponse(tournamentLimited), true);

  const passLimited = structuredClone(response) as unknown as Record<string, unknown>;
  const passResult = passLimited.result as Record<string, unknown>;
  const passReport = passResult.constraintRepair as Record<string, unknown>;
  passReport.geometricFixedPoint = false;
  passReport.polishCutoff = "passes";
  assert.equal(isLayoutWorkerResponse(passLimited), true);

  const malformedPassCutoff = structuredClone(passLimited) as unknown as Record<string, unknown>;
  const malformedPassResult = malformedPassCutoff.result as Record<string, unknown>;
  const malformedPassReport = malformedPassResult.constraintRepair as Record<string, unknown>;
  malformedPassReport.polishCutoff = "pass";
  assert.equal(isLayoutWorkerResponse(malformedPassCutoff), false);

  passReport.geometricFixedPoint = true;
  assert.equal(isLayoutWorkerResponse(passLimited), false);

  tournamentReport.geometricFixedPoint = true;
  tournamentReport.extensionSearch = {
    completed: true,
    cancelled: false,
    exhausted: false,
  };
  tournamentReport.maskDiversification = {
    completed: true,
    exhausted: false,
  };
  tournamentReport.crossingRepair = {
    ...(tournamentReport.crossingRepair as Record<string, unknown>),
    completed: true,
    cancelled: false,
    exhausted: false,
  };
  assert.equal(isLayoutWorkerResponse(tournamentLimited), false);

  const mismatchedCutoff = structuredClone(response) as unknown as Record<string, unknown>;
  const mismatchResult = mismatchedCutoff.result as Record<string, unknown>;
  const mismatchReport = mismatchResult.constraintRepair as Record<string, unknown>;
  mismatchReport.cutoff = "extensions";
  mismatchReport.extensionSearch = {
    completed: false,
    cancelled: false,
    exhausted: false,
  };
  assert.equal(isLayoutWorkerResponse(mismatchedCutoff), false);
});

test("the v11 boundary accepts telemetry-only quick crossing completion", () => {
  const plan = planIntegralLayout(requestToward("East"));
  assert.equal(isLayoutWorkerProgress({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 78,
    operation: "integral",
    progress: true,
    event: {
      type: "crossing-progress",
      stage: "crossing-repair",
      mode: "quick",
      status: "complete",
      crossingsConsidered: 2,
      macrosConsidered: 48,
      pushClosures: 17,
      maxDepth: 1,
      visitedStates: 9,
      bestQuality: plan.quality,
    },
  }), true);
});

test("the v11 boundary requires exact constraint counters and clone-safe incumbents", () => {
  const plan = planIntegralLayout(requestToward("East"));
  const work = {
    rawIncumbents: 3,
    softIncumbents: 2,
    distinctLayouts: 5,
    maskDiversifications: 4,
    separatorStates: 91,
    separatorBranches: 144,
    separatorCyclePrunes: 17,
    firstIncumbentMs: 8.5,
  };
  const progress = {
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 79,
    operation: "constraint-repair",
    progress: true,
    event: {
      type: "constraint-progress",
      stage: "constraint-repair",
      phase: "compaction",
      restarts: 8,
      feasibilityChecks: 13,
      layoutsConsidered: 2,
      compactionAttempts: 4,
      elapsedMs: 21,
      bestQuality: plan.quality,
      ...work,
    },
  };
  assert.equal(isLayoutWorkerProgress(progress), true);

  for (const field of [
    "rawIncumbents",
    "softIncumbents",
    "distinctLayouts",
    "maskDiversifications",
    "separatorStates",
    "separatorBranches",
    "separatorCyclePrunes",
  ]) {
    const incomplete = structuredClone(progress) as Record<string, unknown>;
    delete (incomplete.event as Record<string, unknown>)[field];
    assert.equal(isLayoutWorkerProgress(incomplete), false, `${field} is required`);
  }

  const improvement = structuredClone(progress) as Record<string, unknown>;
  improvement.event = {
    type: "constraint-improvement",
    stage: "constraint-repair",
    restarts: 8,
    feasibilityChecks: 13,
    layoutsConsidered: 2,
    compactionAttempts: 4,
    ...work,
    candidate: {
      quality: plan.quality,
      movedExisting: [...plan.movedExisting],
      positions: [...plan.positions].map(([id, position]) => ({ id, ...position })),
    },
  };
  assert.equal(isLayoutWorkerProgress(improvement), true);

  const colliding = structuredClone(improvement) as Record<string, unknown>;
  const candidate = (colliding.event as Record<string, unknown>).candidate as
    Record<string, unknown>;
  const positions = candidate.positions as { id: string; x: number; y: number; level: number }[];
  positions[1] = { ...positions[1], x: positions[0].x, y: positions[0].y };
  assert.equal(isLayoutWorkerProgress(colliding), false);

  const oldProtocol = structuredClone(progress) as Record<string, unknown>;
  oldProtocol.protocol = 6;
  assert.equal(isLayoutWorkerProgress(oldProtocol), false);

  const unknownEvent = structuredClone(progress) as Record<string, unknown>;
  unknownEvent.event = { type: "constraint-future-event", stage: "constraint-repair" };
  assert.equal(isLayoutWorkerProgress(unknownEvent), false);
});

afterEach(() => {
  setLayoutWorkerFactoryForTesting();
});

test("async integral planning round-trips clone-safe DTOs and replays trace in order", async () => {
  const workers: ExecutingWorker[] = [];
  setLayoutWorkerFactoryForTesting(() => {
    const worker = new ExecutingWorker();
    workers.push(worker);
    return worker;
  });

  const expectedTrace: LayoutTraceEvent[] = [];
  const expected = planIntegralLayout(requestToward("East", (event) => expectedTrace.push(event)));
  const actualTrace: LayoutTraceEvent[] = [];
  const pending = planIntegralLayoutAsync(requestToward("East", (event) => actualTrace.push(event)));

  assert.deepEqual(actualTrace, [], "trace is replayed only after the Worker responds");
  const actual = await pending;
  assert.deepEqual(actual, expected);
  assert.deepEqual(actualTrace, expectedTrace);
  assert.ok(actual.positions instanceof Map);
  assert.ok(actual.movedExisting instanceof Set);
  assert.equal(workers.length, 1);
});

test("Worker constraint repair proves the minimum and preserves reciprocal exits", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const trace: LayoutTraceEvent[] = [];
  const request: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(1, 0), movable: true },
      { id: "c", position: at(2, 0), movable: true },
    ],
    nodes: [],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "a", direction: "West" },
      { from: "b", to: "c", direction: "East" },
      { from: "c", to: "a", direction: "East" },
    ],
    allowExistingMoves: true,
    trace: (event) => trace.push(event),
  };

  const result = await planIntegralLayoutAsync(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });

  assert.equal(result.quality.cardinalRayViolations, 1);
  assert.equal(result.quality.reciprocalRayViolations, 0);
  assert.equal(result.constraintRepair?.constraintOptimal, true);
  assert.equal(result.constraintRepair?.optimal, true);
  assert.equal(result.constraintRepair?.lowerBound, 1);
  assert.equal(result.constraintRepair?.relaxedEdges, 1);
  assert.equal(result.constraintRepair?.reciprocalRelaxedEdges, 0);
  assert.equal(result.constraintRepair?.geometricFixedPoint, true);
  assert.equal(result.constraintRepair?.polishCutoff, "fixed-point");
  assert.equal(layoutPlannerState.value.terminalReason, "fixed-point");
  assert.ok((result.constraintRepair?.polishPasses ?? 0) > 0);
  assert.equal(
    trace.filter((event) => event.type === "constraint-repair").length,
    1,
  );
});

test("constraint repair proves seventeen independent synthetic conflicts", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const request: IntegralLayoutRequest = {
    residents: [],
    nodes: [],
    edges: [],
    allowExistingMoves: true,
  };
  for (let conflict = 0; conflict < 17; conflict += 1) {
    const a = `a${conflict}`;
    const b = `b${conflict}`;
    const c = `c${conflict}`;
    const y = conflict * 2;
    request.residents = [
      ...request.residents,
      { id: a, position: at(0, y), movable: true },
      { id: b, position: at(1, y), movable: true },
      { id: c, position: at(2, y), movable: true },
    ];
    request.edges = [
      ...request.edges,
      { from: a, to: b, direction: "East" },
      { from: b, to: a, direction: "West" },
      { from: b, to: c, direction: "East" },
      { from: c, to: a, direction: "East" },
    ];
  }

  const result = await planIntegralLayoutAsync(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });

  assert.equal(result.quality.cardinalRayViolations, 17);
  assert.equal(result.quality.reciprocalRayViolations, 0);
  assert.equal(result.constraintRepair?.lowerBound, 17);
  assert.equal(result.constraintRepair?.relaxedEdges, 17);
  assert.equal(result.constraintRepair?.optimal, true);
});

test("settled-regression constraint repair stays dormant without a regression", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const request: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(1, 0), movable: true },
      { id: "c", position: at(2, 0), movable: true },
    ],
    nodes: [],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "c", direction: "East" },
      { from: "c", to: "a", direction: "East" },
    ],
    allowExistingMoves: true,
  };

  const result = await planIntegralLayoutAsync(request, {
    constraintRepair: { when: "settled-regression", maxDurationMs: 1_000 },
  });

  assert.equal(result.quality.cardinalRayViolations, 1);
  assert.equal(result.constraintRepair?.outcome, "no-regression");
  assert.equal(result.constraintRepair?.selected, false);
  assert.deepEqual(result.positions, planIntegralLayout(request).positions);
});

test("a repair that does not search reports why through the Worker, streamed and final", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const events: LayoutTraceEvent[] = [];
  const clean = await planIntegralLayoutAsync({
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
    trace: (event) => events.push(event),
  }, { constraintRepair: { when: "defects" } });
  assert.equal(clean.constraintRepair?.outcome, "clean");
  assert.ok(events.some((event) =>
    event.type === "constraint-repair" && event.report.outcome === "clean"
  ));

  // A budget that is not positive still reaches the Worker, which says so.
  const unbudgeted = await planIntegralLayoutAsync(constraintConflictRequest(), {
    constraintRepair: { when: "always", maxDurationMs: 0 },
  });
  assert.equal(unbudgeted.constraintRepair?.outcome, "no-budget");
  assert.deepEqual(unbudgeted.positions, planIntegralLayout(constraintConflictRequest()).positions);
});

test("a repair response carries a report, and one that did not search must say so consistently", () => {
  const request = constraintConflictRequest();
  const response = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 82,
    operation: "constraint-repair",
    collectTrace: false,
    streamProgress: false,
    request,
    standard: encodeIntegralLayoutPlan(planIntegralLayout(request)),
    options: { when: "settled-regression", maxDurationMs: 1_000 },
  });
  assert.equal(response.ok, true);
  if (!response.ok) return;
  assert.equal((response.result as IntegralLayoutWirePlan).constraintRepair?.outcome, "no-regression");
  assert.equal(isLayoutWorkerResponse(response), true);

  const altered = (change: (report: Record<string, unknown>) => void): unknown => {
    const malformed = structuredClone(response) as unknown as Record<string, unknown>;
    const result = malformed.result as Record<string, unknown>;
    change(result.constraintRepair as Record<string, unknown>);
    return malformed;
  };
  const alterations: [why: string, change: (report: Record<string, unknown>) => void][] = [
    ["the outcome is required", (report) => {
      delete report.outcome;
    }],
    ["the outcome is one the vocabulary names", (report) => {
      report.outcome = "skipped";
    }],
    ["a report that did not search selects nothing", (report) => {
      report.selected = true;
    }],
    ["a report that did not search proves nothing", (report) => {
      report.constraintOptimal = true;
      report.optimal = true;
    }],
    ["only a searched report has a polish cutoff", (report) => {
      report.polishCutoff = "fixed-point";
    }],
    ["a searched report has one", (report) => {
      report.outcome = "searched";
    }],
  ];
  for (const [why, change] of alterations) {
    assert.equal(isLayoutWorkerResponse(altered(change)), false, why);
  }

  const unreported = structuredClone(response) as unknown as Record<string, unknown>;
  delete (unreported.result as Record<string, unknown>).constraintRepair;
  assert.equal(isLayoutWorkerResponse(unreported), false, "every repair reports");
});

test("violation-regression repair considers newly observed topology", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const request: IntegralLayoutRequest = {
    residents: [
      { id: "a", position: at(0, 0), movable: true },
      { id: "b", position: at(1, 0), movable: true },
    ],
    nodes: [
      { id: "a", relative: at(0, 0) },
      { id: "b", relative: at(1, 0) },
      { id: "c", relative: at(2, 0) },
    ],
    edges: [
      { from: "a", to: "b", direction: "East" },
      { from: "b", to: "a", direction: "West" },
      { from: "b", to: "c", direction: "East" },
      { from: "c", to: "a", direction: "East" },
    ],
    centerId: "a",
    allowExistingMoves: true,
  };

  const result = await planIntegralLayoutAsync(request, {
    constraintRepair: { when: "violation-regression", maxDurationMs: 1_000 },
  });

  assert.equal(result.constraintRepair?.beforeViolations, 0);
  assert.equal(result.constraintRepair?.standardViolations, 1);
  assert.equal(result.constraintRepair?.optimal, true);
});

test("async model planning preserves the synchronous result without cloning callbacks", async () => {
  const worker = new ExecutingWorker();
  setLayoutWorkerFactoryForTesting(() => worker);
  const model = twoRoomModel();
  const expectedTrace: LayoutTraceEvent[] = [];
  const expected = planLayoutModel(
    model,
    { type: "reflow", anchor: "start" },
    { effort: "thorough", maxPlanningPasses: 1, trace: (event) => expectedTrace.push(event) },
  );
  const actualTrace: LayoutTraceEvent[] = [];
  const actual = await planLayoutModelAsync(
    model,
    { type: "reflow", anchor: "start" },
    { effort: "thorough", maxPlanningPasses: 1, trace: (event) => actualTrace.push(event) },
  );

  assert.deepEqual(actual, expected);
  assert.deepEqual(actualTrace, expectedTrace);
  assert.ok(actual.positions instanceof Map);
});

test("explicit model reflow can stage standard planning through constraint repair", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const result = await planLayoutModelAsync(
    twoRoomModel(),
    { type: "reflow", anchor: "start" },
    {
      constraintRepair: {
        when: "always",
        maxDurationMs: Number.POSITIVE_INFINITY,
        maxRestarts: 4,
        maxLayouts: 1,
        maxPolishTournaments: 1,
        maxPolishPasses: 1,
        maxExtensionStates: 16,
        maxMaskDiversifications: 2,
        maxCrossingWork: 8,
      },
    },
  );

  assert.ok(result.positions instanceof Map);
  assert.equal(result.constraintRepair?.trigger, "always");
  assert.equal(layoutPlannerState.value.terminalReason, "ceiling");
  for (const room of result.after.rooms) {
    assert.deepEqual(room.position, result.positions.get(room.id));
  }
});

test("v11 model responses ship no models and validate bounded-search completion", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const cases: { change: LayoutChange; options?: PlanLayoutOptions }[] = [
    { change: { type: "add-room", from: "east", direction: "Up", elevation: "projected" } },
    { change: { type: "add-room", from: "start", direction: "North", temporaryId: "$fresh" } },
    {
      change: {
        type: "connect-rooms",
        from: "start",
        to: "east",
        direction: "South",
        createReturnEdge: false,
      },
    },
    { change: { type: "reflow", anchor: "start" } },
    { change: { type: "reflow" }, options: { effort: "thorough" } },
    {
      change: { type: "reflow", anchor: "start" },
      options: { effort: "thorough", maxPlanningPasses: 1 },
    },
  ];
  for (const { change, options } of cases) {
    const expected = planLayoutModel(twoRoomModel(), change, options);
    const actual = await planLayoutModelAsync(twoRoomModel(), change, options);
    assert.deepEqual(actual.before, expected.before, `${change.type} reconstructs before`);
    assert.deepEqual(actual.after, expected.after, `${change.type} reconstructs after`);
    assert.deepEqual(actual, expected);
  }

  const wire = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 91,
    operation: "model",
    collectTrace: false,
    streamProgress: false,
    model: twoRoomModel(),
    change: { type: "reflow", anchor: "start" },
    options: {},
  });
  assert.equal(wire.ok, true);
  assert.equal(isLayoutWorkerResponse(wire), true);
  if (!wire.ok) return;
  assert.equal("before" in wire.result, false, "the requester already holds the before model");
  assert.equal("after" in wire.result, false, "after derives from the patch");

  const boundedWire = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 92,
    operation: "model",
    collectTrace: false,
    streamProgress: false,
    model: twoRoomModel(),
    change: { type: "reflow", anchor: "start" },
    options: { effort: "thorough", maxPlanningPasses: 1 },
  });
  assert.equal(boundedWire.ok, true);
  assert.equal(isLayoutWorkerResponse(boundedWire), true);
  if (!boundedWire.ok) return;
  assert.equal(boundedWire.operation, "model");
  if (boundedWire.operation !== "model") return;
  assert.equal(boundedWire.result.search?.completed, false);
  const falseCompletion = structuredClone(boundedWire) as unknown as Record<string, unknown>;
  const falseCompletionResult = falseCompletion.result as Record<string, unknown>;
  const falseCompletionSearch = falseCompletionResult.search as Record<string, unknown>;
  falseCompletionSearch.completed = true;
  assert.equal(isLayoutWorkerResponse(falseCompletion), false);
  const missingCompletion = structuredClone(boundedWire) as unknown as Record<string, unknown>;
  const missingResult = missingCompletion.result as Record<string, unknown>;
  const missingSearch = missingResult.search as Record<string, unknown>;
  delete missingSearch.completed;
  assert.equal(isLayoutWorkerResponse(missingCompletion), false);
});

test("parent-side model reconstruction is insulated from caller mutation", async () => {
  const worker = new ControlledWorker();
  setLayoutWorkerFactoryForTesting(() => worker);
  const model = twoRoomModel();
  const change: ReflowLayoutChange = { type: "reflow", anchor: "start" };
  const expected = planLayoutModel(twoRoomModel(), { type: "reflow", anchor: "start" });
  const pending = planLayoutModelAsync(model, change);

  // Mutate the caller's objects while the Worker request is in flight.
  change.anchor = "east";
  (model.rooms[0] as { position: GridPosition }).position = at(9, 9);
  worker.respondAt(0);
  const actual = await pending;

  assert.deepEqual(actual, expected);
});

test("the v11 boundary refuses progress attributed to model jobs", () => {
  const progressAs = (operation: string): unknown => ({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 5,
    operation,
    progress: true,
    event: { type: "candidate-batch" },
  });
  assert.equal(isLayoutWorkerProgress(progressAs("integral")), true);
  assert.equal(isLayoutWorkerProgress(progressAs("model")), false);
});

test("jobs without a stream consumer build and post no trace events", () => {
  const posted: LayoutTraceEvent[] = [];
  const silent = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 92,
    operation: "integral",
    collectTrace: false,
    streamProgress: false,
    request: requestToward("East"),
  }, (event) => posted.push(event));
  assert.equal(silent.ok, true);
  assert.deepEqual(posted, [], "an unrequested stream posts nothing");
  assert.deepEqual(silent.traceEvents, []);

  const streaming = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 93,
    operation: "integral",
    collectTrace: false,
    streamProgress: true,
    request: requestToward("East"),
  }, (event) => posted.push(event));
  assert.equal(streaming.ok, true);
  assert.ok(posted.length > 0, "a requested stream still posts events");
  assert.deepEqual(streaming.traceEvents, [], "streaming alone retains no response trace");

  const bounded: LayoutTraceEvent[] = [];
  const boundedResponse = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 94,
    operation: "integral",
    collectTrace: false,
    streamProgress: true,
    request: requestToward("East"),
  }, (event) => bounded.push(event), {
    now: () => 0,
    progressIntervalMs: Number.POSITIVE_INFINITY,
    maxProgressMessages: 2,
    maxProgressPositionEntries: 0,
  });
  assert.equal(boundedResponse.diagnostics.progressMessages, bounded.length);
  assert.ok(bounded.length <= 2, "the transport enforces its terminal message ceiling");
  assert.equal(boundedResponse.diagnostics.progressPositionEntries, 0);
  assert.equal(JSON.stringify(bounded).includes('"positions"'), false);
  assert.ok(boundedResponse.diagnostics.progressEventsCoalesced > 0);
  assert.ok(boundedResponse.diagnostics.estimatedProgressBytes > 0);
});

test("a preview reaches the progress observer at once, ahead of coalesced candidates", () => {
  const resident = (id: string, x: number, y: number, movable = true) => ({
    id,
    position: at(x, y),
    movable,
  });
  const edge = (from: string, to: string, direction: LayoutDirection) => ({ from, to, direction });
  const posted: LayoutTraceEvent[] = [];
  const response = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 96,
    operation: "integral",
    collectTrace: false,
    streamProgress: true,
    // A whole-map polish whose quick crossing repair publishes after its preview.
    request: {
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
    },
  }, (event) => posted.push(event), {
    // Every other candidate waits behind the first message for the whole run.
    now: () => 0,
    progressIntervalMs: Number.POSITIVE_INFINITY,
  });
  assert.equal(response.ok, true);
  assert.ok(
    posted.some((event) => event.type === "crossing-repair" && event.mode === "quick"),
    "the fixture publishes a quick crossing candidate",
  );
  const previews = posted.filter((event) => event.type === "preview");
  assert.equal(previews.length, 1, "the preview is posted, not replaced by a later candidate");
  assert.ok(previews[0].type === "preview" && previews[0].candidate.positions?.length === 8);
});

test("Worker diagnostics sample optional Deno memory without retaining layouts", () => {
  const realm = globalThis as unknown as { Deno?: unknown };
  const previous = realm.Deno;
  let samples = 0;
  realm.Deno = {
    memoryUsage: () => {
      samples += 1;
      return {
        heapUsed: samples * 100,
        heapTotal: samples * 200,
        external: samples * 10,
        rss: samples * 300,
      };
    },
  };
  try {
    const response = executeLayoutWorkerRequest({
      protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
      id: 95,
      operation: "integral",
      collectTrace: false,
      streamProgress: false,
      request: requestToward("East"),
    });
    const memory = response.diagnostics.memory;
    assert.ok(memory && memory.samples >= 2);
    assert.deepEqual(memory.start, { heapUsed: 100, heapTotal: 200, external: 10, rss: 300 });
    assert.ok(memory.end.heapUsed >= memory.start.heapUsed);
    assert.ok(memory.peak.heapTotal >= memory.end.heapTotal);
    assert.ok(memory.peak.rss !== undefined && memory.peak.rss >= (memory.end.rss ?? 0));
    assert.equal(isLayoutWorkerResponse(response), true);
  } finally {
    if (previous === undefined) delete realm.Deno;
    else realm.Deno = previous;
  }
});

test("async planner failures replay the same diagnostic prefix as synchronous failures", async () => {
  const worker = new ExecutingWorker();
  setLayoutWorkerFactoryForTesting(() => worker);
  const synchronousTrace: LayoutTraceEvent[] = [];
  assert.throws(
    () => planIntegralLayout(collidingRequest((event) => synchronousTrace.push(event))),
    /could not produce a collision-free integral layout/,
  );
  assert.ok(synchronousTrace.length > 0);

  const asynchronousTrace: LayoutTraceEvent[] = [];
  await assert.rejects(
    planIntegralLayoutAsync(collidingRequest((event) => asynchronousTrace.push(event))),
    /could not produce a collision-free integral layout/,
  );
  assert.deepEqual(asynchronousTrace, synchronousTrace);
  assert.equal(worker.terminated, false, "a planner error does not reset its healthy Worker");
});

test("a parent trace callback failure remains request-local for success and failure replies", async () => {
  const worker = new ExecutingWorker();
  setLayoutWorkerFactoryForTesting(() => worker);

  await assert.rejects(
    planIntegralLayoutAsync(requestToward("East", () => {
      throw new Error("trace sink failed");
    })),
    /trace sink failed/,
  );
  await assert.rejects(
    planIntegralLayoutAsync(collidingRequest(() => {
      throw new Error("failure trace sink failed");
    })),
    /failure trace sink failed/,
  );
  assert.equal(worker.terminated, false);
  assert.deepEqual(
    (await planIntegralLayoutAsync(requestToward("North"))).positions.get("new"),
    at(0, -1),
  );
});

test("concurrent callers retain FIFO order through the single-active scheduler", async () => {
  const worker = new ControlledWorker();
  const client = new LayoutWorkerClient(() => worker);
  const east = client.planIntegral(requestToward("East"));
  const north = client.planIntegral(requestToward("North"));
  assert.equal(worker.requests.length, 1);

  worker.respondAt(0);
  assert.equal(worker.requests.length, 1, "the next request is posted immediately after completion");
  assert.deepEqual((await east).positions.get("new"), at(1, 0));
  worker.respondAt(0);
  assert.deepEqual((await north).positions.get("new"), at(0, -1));
});

test("a planner plans on a Worker of its own beside the shared queue until it closes", async () => {
  const workers: ControlledWorker[] = [];
  setLayoutWorkerFactoryForTesting(() => {
    const worker = new ControlledWorker();
    workers.push(worker);
    return worker;
  });
  const shared = planIntegralLayoutAsync(requestToward("East"));
  const planner = createLayoutPlanner();
  assert.equal(workers.length, 1, "a planner starts no Worker before it plans");

  const own = planner.planIntegral(requestToward("North"));
  assert.equal(workers.length, 2, "the planner's request does not wait behind the shared one");
  workers[1].respondAt(0);
  assert.deepEqual((await own).positions.get("new"), at(0, -1));
  assert.equal(workers[0].requests.length, 1, "the shared request is still running");

  const unfinished = planner.planIntegral(requestToward("East"));
  planner.close();
  await assert.rejects(unfinished, /map-layout planner was closed/);
  assert.equal(workers[1].terminated, true);
  assert.equal(workers[0].terminated, false, "closing a planner leaves the shared Worker alone");
  workers[0].respondAt(0);
  assert.deepEqual((await shared).positions.get("new"), at(1, 0));
});

test("a serialized planning error rejects only its request and leaves the Worker reusable", async () => {
  const worker = new ExecutingWorker();
  let factoryCalls = 0;
  const client = new LayoutWorkerClient(() => {
    factoryCalls += 1;
    return worker;
  });

  await assert.rejects(
    client.planModel(twoRoomModel(), { type: "reflow", anchor: "missing" }),
    (error: Error) => error.name === "Error" &&
      /layout anchor room missing does not exist/.test(error.message) && !!error.stack,
  );
  const result = await client.planModel(twoRoomModel(), { type: "reflow", anchor: "start" });
  assert.deepEqual(result.positions.get("east"), at(1, 0));
  assert.equal(factoryCalls, 1);
  assert.equal(worker.terminated, false);
});

test("a fatal Worker error rejects its active request and replays queued work", async () => {
  const failed = new ControlledWorker();
  const restarted = new ControlledWorker();
  let factoryCalls = 0;
  const client = new LayoutWorkerClient(() => {
    factoryCalls += 1;
    return factoryCalls === 1 ? failed : restarted;
  });
  const first = client.planIntegral(requestToward("East"));
  const second = client.planIntegral(requestToward("North"));
  const oldErrorHandler = failed.onerror;
  let prevented = false;
  const firstRejected = assert.rejects(first, /fake Worker crashed/);

  failed.onerror?.({
    error: new Error("fake Worker crashed"),
    preventDefault: () => {
      prevented = true;
    },
  });
  await firstRejected;
  assert.equal(prevented, true);
  assert.equal(failed.terminated, true);
  assert.equal(restarted.requests.length, 1);
  oldErrorHandler?.({ error: new Error("late stale Worker error") });
  restarted.respondAt(0);
  assert.deepEqual((await second).positions.get("new"), at(0, -1));

  const recovered = client.planIntegral(requestToward("East"));
  assert.equal(restarted.requests.length, 1);
  restarted.respondAt(0);
  assert.deepEqual((await recovered).positions.get("new"), at(1, 0));
  assert.equal(factoryCalls, 2);
});

test("a synchronous postMessage failure rejects only the active request", async () => {
  class ThrowOnSecondPostWorker extends ControlledWorker {
    #posts = 0;

    override postMessage(message: unknown): void {
      this.#posts += 1;
      if (this.#posts === 2) throw new Error("structured clone failed");
      super.postMessage(message);
    }
  }

  const failed = new ThrowOnSecondPostWorker();
  const restarted = new ExecutingWorker();
  let factoryCalls = 0;
  const client = new LayoutWorkerClient(() => {
    factoryCalls += 1;
    return factoryCalls === 1 ? failed : restarted;
  });
  const first = client.planIntegral(requestToward("East"));
  const second = client.planIntegral(requestToward("North"));
  failed.respondAt(0);

  assert.deepEqual((await first).positions.get("new"), at(1, 0));
  await assert.rejects(second, /structured clone failed/);
  assert.equal(failed.terminated, true);
  assert.deepEqual(
    (await client.planIntegral(requestToward("East"))).positions.get("new"),
    at(1, 0),
  );
  assert.equal(factoryCalls, 2);
});

test("an async workspace plan cannot become pending after an intervening accept", async () => {
  const worker = new ControlledWorker();
  setLayoutWorkerFactoryForTesting(() => worker);
  const workspace = createLayoutWorkspace(twoRoomModel());
  const synchronous = workspace.plan({ type: "reflow", anchor: "start" });
  const pending = workspace.planAsync({ type: "reflow", anchor: "east" });
  workspace.accept(synchronous);

  worker.respondAt(0);
  await assert.rejects(pending, /workspace changed while Worker planning was in progress/);
  assert.deepEqual(workspace.model.rooms.find((room) => room.id === "east")?.position, at(1, 0));
});

test("snapshot comparison is order-independent and includes every planning input", () => {
  const model = twoRoomModel();
  const reordered: LayoutModel = {
    ...model,
    rooms: [...model.rooms].reverse(),
    edges: [...model.edges].reverse(),
  };
  assert.equal(sameLayoutSnapshot(model, reordered), true);
  assert.equal(layoutSnapshotKey(model), layoutSnapshotKey(reordered));
  assert.doesNotThrow(() => JSON.parse(layoutSnapshotKey(model)));
  assert.equal(sameLayoutSnapshot(model, {
    ...reordered,
    rooms: reordered.rooms.map((room) => room.id === "east"
      ? { ...room, movable: false }
      : room),
  }), false);
  assert.notEqual(layoutSnapshotKey(model), layoutSnapshotKey({
    ...reordered,
    rooms: reordered.rooms.map((room) => room.id === "east"
      ? { ...room, movable: false }
      : room),
  }));
  // The whole area id is part of the key, not just its first characters.
  assert.notEqual(
    layoutSnapshotKey({ ...model, areaId: "area:12" }),
    layoutSnapshotKey({ ...model, areaId: "area:13" }),
  );
});

test("stable snapshot planning retries once and replays only the accepted trace", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const first = twoRoomModel(5);
  const stable = twoRoomModel(4);
  const snapshots = [first, stable, stable, stable];
  let loads = 0;
  const actualTrace: LayoutTraceEvent[] = [];
  const expectedTrace: LayoutTraceEvent[] = [];
  const expected = planLayoutModel(
    stable,
    { type: "reflow", anchor: "start" },
    { effort: "thorough", maxPlanningPasses: 1, trace: (event) => expectedTrace.push(event) },
  );

  const result = await planStableLayoutSnapshot(
    () => structuredClone(snapshots[Math.min(loads++, snapshots.length - 1)]),
    { type: "reflow", anchor: "start" },
    { effort: "thorough", maxPlanningPasses: 1, trace: (event) => actualTrace.push(event) },
  );

  assert.equal(loads, 4);
  assert.deepEqual(result, expected);
  assert.deepEqual(actualTrace, expectedTrace);
});

test("stable snapshot planning is insulated from caller mutation of the change", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const first = twoRoomModel(5);
  const stable = twoRoomModel(4);
  const snapshots = [first, stable, stable, stable];
  let loads = 0;
  const expected = planLayoutModel(stable, { type: "reflow", anchor: "start" });
  const change: ReflowLayoutChange = { type: "reflow", anchor: "start" };

  const pending = planStableLayoutSnapshot(
    () => structuredClone(snapshots[Math.min(loads++, snapshots.length - 1)]),
    change,
  );
  change.anchor = "east";
  const result = await pending;

  assert.equal(loads, 4);
  assert.deepEqual(result, expected);
});

test("stable snapshot planning rejects after its bounded retry without replaying trace", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const snapshots = [twoRoomModel(5), twoRoomModel(4), twoRoomModel(4), twoRoomModel(3)];
  let loads = 0;
  const trace: LayoutTraceEvent[] = [];

  await assert.rejects(
    planStableLayoutSnapshot(
      () => structuredClone(snapshots[Math.min(loads++, snapshots.length - 1)]),
      { type: "reflow", anchor: "start" },
      { trace: (event) => trace.push(event) },
    ),
    (error: Error) => error instanceof StaleLayoutSnapshotError && error.attempts === 2,
  );
  assert.equal(loads, 4);
  assert.deepEqual(trace, []);
});

test("stable snapshot planning does not leak a failed attempt's diagnostic prefix", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const colliding = createLayoutModel({
    rooms: [
      { id: "first", position: at(0, 0), movable: false },
      { id: "second", position: at(0, 0), movable: false },
    ],
    edges: [],
  });
  const trace: LayoutTraceEvent[] = [];

  await assert.rejects(
    planStableLayoutSnapshot(
      () => structuredClone(colliding),
      { type: "reflow", anchor: "first" },
      { allowExistingMoves: false, trace: (event) => trace.push(event) },
    ),
    /could not produce a collision-free integral layout/,
  );
  assert.deepEqual(trace, []);
});

// ---------------------------------------------------------------------------
// Route amendments on the wire
// ---------------------------------------------------------------------------

function fixedCrossingRequest(): IntegralLayoutRequest {
  return {
    residents: [
      { id: "west", position: at(-2, 0), movable: false },
      { id: "east", position: at(2, 0), movable: false },
      { id: "north", position: at(0, -2), movable: false },
      { id: "south", position: at(0, 2), movable: false },
    ],
    nodes: [],
    edges: [
      { from: "west", to: "east", direction: "Other" },
      { from: "north", to: "south", direction: "Other" },
    ],
    centerId: "west",
    allowExistingMoves: true,
  };
}

test("integral responses carry route amendments through validation and decode", () => {
  const response = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 90,
    operation: "integral",
    collectTrace: false,
    streamProgress: false,
    request: fixedCrossingRequest(),
  });
  assert.equal(response.ok, true);
  assert.equal(isLayoutWorkerResponse(response), true);
  if (!response.ok || response.operation !== "integral") return;

  const wire = response.result;
  assert.ok(wire.routeAmendments);
  assert.equal(wire.routeAmendments.length, 1);
  const decoded = decodeIntegralLayoutPlan(structuredClone(wire) as IntegralLayoutWirePlan);
  assert.deepEqual(decoded.routeAmendments, wire.routeAmendments);
  assert.deepEqual(
    planIntegralLayout(fixedCrossingRequest()).routeAmendments,
    wire.routeAmendments,
    "the wire carries exactly what synchronous planning computes",
  );

  const mutate = (
    change: (amendments: {
      from: string;
      to: string;
      waypoints: { x: number; y: number; level: number }[];
    }[]) => void,
  ): boolean => {
    const malformed = structuredClone(response) as unknown as Record<string, unknown>;
    const result = malformed.result as Record<string, unknown>;
    change(result.routeAmendments as {
      from: string;
      to: string;
      waypoints: { x: number; y: number; level: number }[];
    }[]);
    return isLayoutWorkerResponse(malformed);
  };

  assert.equal(mutate((amendments) => {
    amendments[0].from = "not-in-the-plan";
  }), false, "amendment endpoints must be known plan rooms");
  assert.equal(mutate((amendments) => {
    amendments[0].to = amendments[0].from;
  }), false, "an amendment needs two distinct rooms");
  assert.equal(mutate((amendments) => {
    amendments[0].waypoints[0].x = 0.5;
  }), false, "waypoints are integral cells");
  assert.equal(mutate((amendments) => {
    amendments[0].waypoints[0].level = 7;
  }), false, "waypoints stay on the link's level");
  assert.equal(mutate((amendments) => {
    amendments.push({
      from: amendments[0].to,
      to: amendments[0].from,
      waypoints: structuredClone(amendments[0].waypoints),
    });
  }), false, "one amendment per unordered room pair");
  assert.equal(mutate((amendments) => {
    amendments[0].waypoints.length = 0;
  }), false, "an amendment without waypoints proposes nothing");
  assert.equal(mutate((amendments) => {
    const waypoint = amendments[0].waypoints[0];
    for (let index = 0; index <= MAX_ROUTE_AMENDMENT_WAYPOINTS; index += 1) {
      amendments[0].waypoints.push({ ...waypoint, x: waypoint.x + index + 1 });
    }
  }), false, "waypoint counts are bounded");
  assert.equal(mutate((amendments) => {
    amendments.length = 0;
  }), false, "an empty amendment list is encoded as an absent field");
});

test("model responses carry route amendments and reject malformed ones", () => {
  const model = createLayoutModel({
    rooms: [
      { id: "west", roomNumber: 1, position: at(-2, 0), movable: false },
      { id: "east", roomNumber: 2, position: at(2, 0), movable: false },
      { id: "north", roomNumber: 3, position: at(0, -2), movable: false },
      { id: "south", roomNumber: 4, position: at(0, 2), movable: false },
    ],
    edges: [
      { from: "west", to: "east", direction: "Other" },
      { from: "north", to: "south", direction: "Other" },
    ],
  });
  const response = executeLayoutWorkerRequest({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: 91,
    operation: "model",
    collectTrace: false,
    streamProgress: false,
    model,
    change: { type: "reflow" },
    options: {},
  });
  assert.equal(response.ok, true);
  assert.equal(isLayoutWorkerResponse(response), true);
  if (!response.ok || response.operation !== "model") return;
  assert.ok(response.result.routeAmendments);
  assert.equal(response.result.routeAmendments.length, 1);
  assert.deepEqual(
    planLayoutModel(model, { type: "reflow" }).routeAmendments,
    response.result.routeAmendments,
  );

  const malformed = structuredClone(response) as unknown as Record<string, unknown>;
  const result = malformed.result as Record<string, unknown>;
  (result.routeAmendments as { from: string }[])[0].from = "unknown";
  assert.equal(isLayoutWorkerResponse(malformed), false);
});
