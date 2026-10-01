import assert from "node:assert/strict";
import test from "node:test";
import {
  compareLayoutQuality,
  planIntegralLayout,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
} from "./layout.ts";
import {
  layoutWorkerDiagnostics,
  LayoutWorkerClient,
  type LayoutWorkerLike,
  type LayoutWorkerPurpose,
} from "./worker-client.ts";
import { executeLayoutWorkerRequest } from "./worker-executor.ts";
import {
  LAYOUT_WORKER_PROTOCOL_VERSION,
  type LayoutWorkerRequest,
} from "./worker-protocol.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });

function requestToward(direction: "North" | "East"): IntegralLayoutRequest {
  return {
    residents: [{ id: "start", position: at(0, 0), movable: true }],
    nodes: [
      { id: "start", relative: at(0, 0) },
      { id: "new", relative: direction === "East" ? at(1, 0) : at(0, -1) },
    ],
    edges: [{ from: "start", to: "new", direction }],
    centerId: "start",
    allowExistingMoves: false,
  };
}

function conflictingRequest(): IntegralLayoutRequest {
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

class ManualWorker implements LayoutWorkerLike {
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

  respond(inspectedStates?: number): void {
    const request = this.requests.shift();
    assert.ok(request, "missing fake Worker request");
    const response = executeLayoutWorkerRequest(request);
    if (inspectedStates !== undefined) response.diagnostics.inspectedStates = inspectedStates;
    this.onmessage?.({ data: structuredClone(response) });
  }
}

class AutoWorker extends ManualWorker {
  override postMessage(message: unknown): void {
    super.postMessage(message);
    queueMicrotask(() => {
      if (!this.terminated && this.requests.length > 0) this.respond();
    });
  }
}

async function until(predicate: () => boolean): Promise<void> {
  for (let attempt = 0; attempt < 20 && !predicate(); attempt += 1) {
    await Promise.resolve();
  }
  assert.equal(predicate(), true, "condition did not become true");
}

function streamStrictImprovement(
  worker: ManualWorker,
  request: LayoutWorkerRequest,
  standard: IntegralLayoutPlan,
): IntegralLayoutPlan {
  if (request.operation !== "constraint-repair") {
    throw new Error("expected a constraint-repair request");
  }
  const quality = {
    ...standard.quality,
    cardinalRayViolations: standard.quality.cardinalRayViolations - 1,
  };
  const entries = [...standard.positions].map(([id, position]) => [id, {
    x: position.x + 10,
    y: position.y,
    level: position.level,
  }] as const);
  const positions = entries.map(([id, position]) => ({ id, ...position }));
  const movedExisting = positions.map(({ id }) => id);
  worker.onmessage?.({ data: structuredClone({
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: request.id,
    operation: request.operation,
    progress: true,
    event: {
      type: "constraint-improvement",
      stage: "constraint-repair",
      restarts: 0,
      feasibilityChecks: 1,
      layoutsConsidered: 0,
      compactionAttempts: 1,
      rawIncumbents: 1,
      softIncumbents: 1,
      distinctLayouts: 1,
      maskDiversifications: 1,
      separatorStates: 1,
      separatorBranches: 1,
      separatorCyclePrunes: 0,
      firstIncumbentMs: 1,
      candidate: { quality, movedExisting, positions },
    },
  }) });
  return {
    positions: new Map(entries),
    movedExisting: new Set(movedExisting),
    quality,
  };
}

test("a queued request can abort without disturbing active or later FIFO work", async () => {
  const worker = new ManualWorker();
  const client = new LayoutWorkerClient(() => worker);
  const queuedAbort = new AbortController();

  const first = client.planIntegral(requestToward("East"), {
    plannerContext: { source: "test", areaId: "area:1" },
  });
  const aborted = client.planIntegral(requestToward("North"), {
    signal: queuedAbort.signal,
    timeoutMs: 1_000,
  });
  const abortedRejection = assert.rejects(
    aborted,
    (error: Error) => error.name === "AbortError",
  );
  const third = client.planIntegral(requestToward("North"));

  assert.equal(worker.requests.length, 1);
  assert.equal("signal" in (worker.requests[0] as unknown as Record<string, unknown>), false);
  assert.equal("timeoutMs" in (worker.requests[0] as unknown as Record<string, unknown>), false);
  assert.equal(
    "plannerContext" in (worker.requests[0] as unknown as Record<string, unknown>),
    false,
    "telemetry attribution stays in the parent realm",
  );
  assert.equal(
    worker.requests[0].streamProgress,
    false,
    "an unobserved integral job requests no event stream",
  );
  queuedAbort.abort();
  await abortedRejection;
  assert.equal(worker.terminated, false);

  worker.respond();
  assert.equal(worker.requests.length, 1);
  assert.deepEqual((await first).positions.get("new"), at(1, 0));
  worker.respond();
  assert.deepEqual((await third).positions.get("new"), at(0, -1));
});

test("a non-positive timeout rejects without creating or posting to a Worker", async () => {
  let factoryCalls = 0;
  const client = new LayoutWorkerClient(() => {
    factoryCalls += 1;
    return new ManualWorker();
  });

  await assert.rejects(
    client.planIntegral(requestToward("East"), { timeoutMs: 0 }),
    (error: Error) => error.name === "TimeoutError",
  );
  assert.equal(factoryCalls, 0);
});

test("aborting the active request replaces its Worker and preserves queued B/C", async () => {
  const workers: ManualWorker[] = [];
  const client = new LayoutWorkerClient(() => {
    const worker = new ManualWorker();
    workers.push(worker);
    return worker;
  });
  const activeAbort = new AbortController();
  const completed: string[] = [];

  const first = client.planIntegral(requestToward("East"), { signal: activeAbort.signal });
  const firstRejection = assert.rejects(first, (error: Error) => error.name === "AbortError");
  const second = client.planIntegral(requestToward("North")).then((result) => {
    completed.push("B");
    return result;
  });
  const third = client.planIntegral(requestToward("East")).then((result) => {
    completed.push("C");
    return result;
  });

  activeAbort.abort();
  await firstRejection;
  assert.equal(workers[0].terminated, true);
  assert.equal(workers.length, 2);
  assert.equal(workers[1].requests.length, 1);

  workers[1].respond();
  assert.equal(workers[1].requests.length, 1);
  workers[1].respond();
  await Promise.all([second, third]);
  assert.deepEqual(completed, ["B", "C"]);
});

test("handlers retained from an aborted Worker cannot corrupt its replacement", async () => {
  const workers: ManualWorker[] = [];
  const client = new LayoutWorkerClient(() => {
    const worker = new ManualWorker();
    workers.push(worker);
    return worker;
  });
  const activeAbort = new AbortController();
  const staleProgress: string[] = [];
  const first = client.planIntegral(requestToward("East"), {
    signal: activeAbort.signal,
    onProgress: ({ snapshot }) => staleProgress.push(snapshot.phase),
  });
  const firstRejection = assert.rejects(first);
  const staleMessage = workers[0].onmessage;
  const staleError = workers[0].onerror;
  const staleRequest = workers[0].requests[0];
  const second = client.planIntegral(requestToward("North"));

  activeAbort.abort();
  await firstRejection;
  assert.equal(workers.length, 2);
  const progressCount = staleProgress.length;
  staleMessage?.({ data: {
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: staleRequest.id,
    operation: staleRequest.operation,
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
      before: { quality: {}, movedExisting: [] },
      after: { quality: {}, movedExisting: [], positions: [] },
    },
  } });
  assert.equal(staleProgress.length, progressCount);
  staleMessage?.({ data: executeLayoutWorkerRequest(staleRequest) });
  let prevented = false;
  staleError?.({
    error: new Error("stale failure"),
    preventDefault: () => {
      prevented = true;
    },
  });
  assert.equal(prevented, false);
  assert.equal(workers[1].terminated, false);
  assert.equal(workers[1].requests.length, 1);

  workers[1].respond();
  assert.deepEqual((await second).positions.get("new"), at(0, -1));
});

test("the parent backstop reclaims a silent repair Worker only after the grace window", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const request = conflictingRequest();
  const standard = planIntegralLayout(request);
  const phases: string[] = [];
  const streamed: IntegralLayoutPlan[] = [];

  let settled = false;
  const planning = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 10 },
    onProgress: (progress) => {
      phases.push(progress.snapshot.phase);
      if (progress.improvement && progress.snapshot.status === "repairing") {
        streamed.push(progress.improvement);
      }
    },
  });
  planning.then(() => {
    settled = true;
  }, () => {
    settled = true;
  });
  await until(() => workers.length === 2);
  const staleRepairMessage = workers[1].worker.onmessage;
  const repairRequest = workers[1].worker.requests[0];
  assert.ok(repairRequest);
  const retained = streamStrictImprovement(workers[1].worker, repairRequest, standard);
  assert.equal(streamed.length, 1);
  // The Worker budget alone must not fire the backstop: a full-budget repair
  // is still allowed its startup and final-polish grace to deliver a report.
  context.mock.timers.tick(10);
  for (let attempt = 0; attempt < 5; attempt += 1) await Promise.resolve();
  assert.equal(settled, false);
  context.mock.timers.tick(2_000);
  const actual = await planning;

  assert.deepEqual(actual, retained);
  assert.deepEqual(actual, streamed[0]);
  assert.equal(actual.constraintRepair, undefined);
  assert.ok(compareLayoutQuality(actual.quality, standard.quality) > 0);
  assert.ok(streamed.every((candidate) =>
    compareLayoutQuality(actual.quality, candidate.quality) >= 0
  ));
  assert.deepEqual(workers.map(({ purpose }) => purpose), ["persistent", "constraint-repair"]);
  assert.equal(workers[0].worker.terminated, false);
  assert.equal(workers[1].worker.terminated, true);
  const phaseCount = phases.length;
  staleRepairMessage?.({ data: {
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: repairRequest.id,
    operation: repairRequest.operation,
    progress: true,
    event: {
      type: "crossing-progress",
      stage: "crossing-repair",
      mode: "deep",
      status: "complete",
      crossingsConsidered: 1,
      macrosConsidered: 1,
      pushClosures: 1,
      maxDepth: 1,
      visitedStates: 1,
      bestQuality: standard.quality,
    },
  } });
  assert.equal(phases.length, phaseCount);
});

test("a full-budget repair completing within the grace window returns its report", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const request = conflictingRequest();
  const standard = planIntegralLayout(request);

  // The in-process repair measures its budget on the real clock, so the budget
  // must be generous enough that a slow machine still finishes the repair
  // inside it; only the parent's timers are mocked.
  const budgetMs = 60_000;
  let settled = false;
  const planning = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: budgetMs },
  });
  planning.then(() => {
    settled = true;
  }, () => {
    settled = true;
  });
  await until(() => workers.length === 2);
  context.mock.timers.tick(budgetMs);
  for (let attempt = 0; attempt < 5; attempt += 1) await Promise.resolve();
  assert.equal(settled, false, "the parent no longer terminates at exactly the Worker budget");
  workers[1].worker.respond();
  const actual = await planning;

  assert.ok(actual.constraintRepair, "a full-budget repair delivers its report inside grace");
  assert.ok(compareLayoutQuality(actual.quality, standard.quality) >= 0);
  assert.equal(
    workers[1].worker.terminated,
    false,
    "a consumed report leaves the repair Worker warm",
  );
});

test("a caller timeout during optional repair resolves with the retained plan", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const request = conflictingRequest();
  const standard = planIntegralLayout(request);
  let terminalReason: string | undefined;
  let terminalPhase: string | undefined;

  const planning = client.planIntegral(request, {
    timeoutMs: 5_000,
    constraintRepair: { when: "always", maxDurationMs: Number.POSITIVE_INFINITY },
    onProgress: ({ snapshot }) => {
      terminalReason = snapshot.terminalReason;
      terminalPhase = snapshot.phase;
    },
  });
  await until(() => workers.length === 2);
  const repairWorker = workers[1].worker;
  const repairRequest = repairWorker.requests[0];
  assert.ok(repairRequest);
  const retained = streamStrictImprovement(repairWorker, repairRequest, standard);
  context.mock.timers.tick(5_000);
  const actual = await planning;

  assert.deepEqual(actual, retained);
  assert.equal(actual.constraintRepair, undefined);
  assert.ok(compareLayoutQuality(actual.quality, standard.quality) > 0);
  assert.equal(repairWorker.terminated, true);
  assert.equal(terminalReason, "timeout");
  assert.equal(terminalPhase, "complete: repair timeout");
});

test("a caller timeout during ordinary planning still rejects", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const worker = new ManualWorker();
  const client = new LayoutWorkerClient(() => worker);

  const planning = client.planIntegral(requestToward("East"), {
    timeoutMs: 50,
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });
  const rejection = assert.rejects(planning, (error: Error) => error.name === "TimeoutError");
  assert.equal(worker.requests.length, 1);
  context.mock.timers.tick(50);
  await rejection;
  assert.equal(worker.terminated, true);
});

test("an infinite repair budget installs no implicit parent-side timeout", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const expected = planIntegralLayout(conflictingRequest());
  const originalSetTimeout = globalThis.setTimeout;
  const timerDelays: number[] = [];
  let planning: Promise<Awaited<ReturnType<LayoutWorkerClient["planIntegral"]>>> | undefined;

  globalThis.setTimeout = ((handler: () => void, timeout?: number) => {
    timerDelays.push(Number(timeout));
    return originalSetTimeout(handler, timeout);
  }) as typeof setTimeout;
  try {
    planning = client.planIntegral(conflictingRequest(), {
      constraintRepair: { when: "always", maxDurationMs: Number.POSITIVE_INFINITY },
    });
    await until(() => workers.length === 2);
    assert.deepEqual(timerDelays, []);
  } finally {
    globalThis.setTimeout = originalSetTimeout;
  }

  const repair = workers[1]?.worker;
  assert.ok(repair);
  repair.onerror?.({ error: new Error("stop the unbounded fake repair") });
  assert.deepEqual(await planning, expected);
  assert.equal(repair.terminated, true);
});

test("large repair limits cross the v8 boundary and aborted repair progress stays isolated", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const controller = new AbortController();
  const updates: { phase: string; separatorStates: number }[] = [];
  const options = {
    when: "always" as const,
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 250_000,
    maxLayouts: 256,
    maxPolishTournaments: 1_024,
    maxPolishPasses: 2_048,
    maxExtensionStates: Number.POSITIVE_INFINITY,
    maxMaskDiversifications: Number.POSITIVE_INFINITY,
    maxCrossingWork: Number.POSITIVE_INFINITY,
  };
  const planning = client.planIntegral(conflictingRequest(), {
    signal: controller.signal,
    constraintRepair: options,
    onProgress: ({ snapshot }) => updates.push({
      phase: snapshot.phase,
      separatorStates: snapshot.work.separatorStates,
    }),
  });
  const rejected = assert.rejects(planning, (error: Error) => error.name === "AbortError");
  await until(() => workers.length === 2);

  const repair = workers[1].worker;
  const request = repair.requests[0];
  assert.ok(request && request.operation === "constraint-repair");
  assert.deepEqual(request.options, options);
  assert.equal(
    request.streamProgress,
    true,
    "repair jobs always stream their anytime improvements",
  );
  const staleMessage = repair.onmessage;
  repair.onmessage?.({ data: {
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: request.id,
    operation: request.operation,
    progress: true,
    event: {
      type: "constraint-progress",
      stage: "constraint-repair",
      phase: "compaction",
      restarts: 5,
      feasibilityChecks: 8,
      layoutsConsidered: 2,
      compactionAttempts: 3,
      elapsedMs: 13,
      rawIncumbents: 2,
      softIncumbents: 1,
      distinctLayouts: 2,
      maskDiversifications: 3,
      separatorStates: 144,
      separatorBranches: 233,
      separatorCyclePrunes: 21,
      firstIncumbentMs: 9,
    },
  } });
  assert.equal(updates.at(-1)?.separatorStates, 144);

  controller.abort();
  await rejected;
  assert.equal(repair.terminated, true);
  const updateCount = updates.length;
  staleMessage?.({ data: {
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: request.id,
    operation: request.operation,
    progress: true,
    event: {
      type: "constraint-progress",
      stage: "constraint-repair",
      phase: "compaction",
      restarts: 6,
      feasibilityChecks: 9,
      layoutsConsidered: 3,
      compactionAttempts: 4,
      elapsedMs: 14,
      rawIncumbents: 3,
      softIncumbents: 2,
      distinctLayouts: 3,
      maskDiversifications: 4,
      separatorStates: 145,
      separatorBranches: 234,
      separatorCyclePrunes: 22,
      firstIncumbentMs: 9,
    },
  } });
  assert.equal(updates.length, updateCount);
});

test("successful constraint repair retains its dedicated Worker for the next repair", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: AutoWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = new AutoWorker();
    workers.push({ purpose, worker });
    return worker;
  });

  const result = await client.planIntegral(conflictingRequest(), {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });

  assert.equal(result.constraintRepair?.optimal, true);
  assert.deepEqual(workers.map(({ purpose }) => purpose), ["persistent", "constraint-repair"]);
  assert.equal(workers[0].worker.terminated, false);
  assert.equal(workers[1].worker.terminated, false);
});

test("sequential successful repairs reuse one warm repair Worker", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: AutoWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = new AutoWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const options = { constraintRepair: { when: "always" as const, maxDurationMs: 1_000 } };

  const first = await client.planIntegral(conflictingRequest(), options);
  const second = await client.planIntegral(conflictingRequest(), options);

  assert.ok(first.constraintRepair && second.constraintRepair);
  assert.deepEqual(
    workers.map(({ purpose }) => purpose),
    ["persistent", "constraint-repair"],
    "the second repair spawns no new Worker",
  );
  assert.equal(workers[1].worker.terminated, false);
});

test("a successful perfect repair retires its Worker and queued work gets a fresh one", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const request = conflictingRequest();
  const first = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: Number.POSITIVE_INFINITY },
  });
  const second = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });

  await until(() => workers.length === 2 && workers[1].worker.requests.length === 1);
  await Promise.resolve();
  const perfectWorker = workers[1].worker;
  perfectWorker.respond();
  await until(() => workers.length === 3 && workers[2].worker.requests.length === 1);
  assert.equal(perfectWorker.terminated, true);
  assert.equal(workers[2].purpose, "constraint-repair");

  workers[2].worker.respond();
  await Promise.all([first, second]);
  assert.equal(workers[2].worker.terminated, false, "the small queued repair remains reusable");
});

test("a successful repair crossing the state threshold retires its Worker", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  }, { heavyRepairStateThreshold: 100 });

  const planning = client.planIntegral(conflictingRequest(), {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });
  await until(() => workers.length === 2);
  workers[1].worker.respond(100);
  await planning;

  assert.equal(workers[1].worker.terminated, true);
  assert.equal(client.lastDiagnostics?.inspectedStates, 100);
  assert.equal(client.lastDiagnostics?.workerDisposition, "retired");
});

test("public terminal diagnostics expose production worker accounting", async () => {
  const observed: unknown[] = [];
  const unsubscribe = layoutWorkerDiagnostics.subscribe((value) => observed.push(value));
  try {
    const client = new LayoutWorkerClient(() => new AutoWorker());
    await client.planIntegral(requestToward("East"));
    const latest = observed.at(-1);
    assert.deepEqual(latest, client.lastDiagnostics);
    assert.doesNotThrow(() => structuredClone(latest));
  } finally {
    unsubscribe();
  }
});

test("an unbounded live-node policy retires its Worker", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });

  const planning = client.planIntegral(conflictingRequest(), {
    constraintRepair: {
      when: "always",
      maxDurationMs: 1_000,
      maxLiveSearchNodes: Number.POSITIVE_INFINITY,
    },
  });
  await until(() => workers.length === 2);
  workers[1].worker.respond();
  await planning;

  assert.equal(workers[1].worker.terminated, true);
  assert.equal(client.lastDiagnostics?.workerDisposition, "retired");
});

test("an idle repair Worker retires after the configured short TTL", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  }, { repairIdleTimeoutMs: 25 });

  const planning = client.planIntegral(conflictingRequest(), {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });
  await until(() => workers.length === 2);
  workers[1].worker.respond();
  await planning;
  context.mock.timers.tick(24);
  assert.equal(workers[1].worker.terminated, false);
  context.mock.timers.tick(1);
  assert.equal(workers[1].worker.terminated, true);
});

test("shutdown rejects active and queued work, retires both lanes, and stays closed", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  let factoryCalls = 0;
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    factoryCalls += 1;
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const first = client.planIntegral(conflictingRequest(), {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });
  await until(() => workers.length === 2);
  const second = client.planIntegral(requestToward("East"));
  const reason = new Error("package stopped");
  client.shutdown(reason);

  await assert.rejects(first, (error) => error === reason);
  await assert.rejects(second, (error) => error === reason);
  assert.ok(workers.every(({ worker }) => worker.terminated));
  await assert.rejects(client.planIntegral(requestToward("North")), /shut down/);
  assert.equal(factoryCalls, 2, "closed clients never spawn replacement Workers");
});

test("a backstopped repair Worker is reclaimed and the next repair starts fresh", async (context) => {
  context.mock.timers.enable({ apis: ["setTimeout"] });
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const request = conflictingRequest();

  const first = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 10 },
  });
  await until(() => workers.length === 2);
  context.mock.timers.tick(2_010);
  const degraded = await first;
  assert.equal(degraded.constraintRepair, undefined);
  assert.equal(workers[1].worker.terminated, true);

  const second = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });
  await until(() => workers.length === 3);
  assert.equal(workers[2].purpose, "constraint-repair");
  workers[2].worker.respond();
  const repaired = await second;
  assert.ok(repaired.constraintRepair, "the next repair runs on a fresh Worker");
  assert.equal(workers[2].worker.terminated, false);
});

test("aborting active repair work reclaims the Worker and the next repair starts fresh", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const controller = new AbortController();

  const first = client.planIntegral(conflictingRequest(), {
    signal: controller.signal,
    constraintRepair: { when: "always", maxDurationMs: Number.POSITIVE_INFINITY },
  });
  const rejected = assert.rejects(first, (error: Error) => error.name === "AbortError");
  await until(() => workers.length === 2);
  controller.abort();
  await rejected;
  assert.equal(workers[1].worker.terminated, true);

  const second = client.planIntegral(conflictingRequest(), {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });
  await until(() => workers.length === 3);
  assert.equal(workers[2].purpose, "constraint-repair");
  workers[2].worker.respond();
  assert.ok((await second).constraintRepair, "the next repair runs on a fresh Worker");
});

test("a serialized repair failure degrades to the retained plan and keeps the Worker", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const request = conflictingRequest();
  const standard = planIntegralLayout(request);

  const planning = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });
  await until(() => workers.length === 2);
  const repairWorker = workers[1].worker;
  const posted = repairWorker.requests[0];
  assert.ok(posted);
  repairWorker.onmessage?.({ data: {
    protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
    id: posted.id,
    operation: "constraint-repair",
    ok: false,
    error: { name: "Error", message: "repair failed inside the Worker" },
    traceEvents: [],
    diagnostics: {
      ...executeLayoutWorkerRequest(posted).diagnostics,
      terminalReason: "failed",
    },
  } });
  // The fabricated response consumed the job; drop its request from the fake.
  repairWorker.requests.shift();
  const actual = await planning;

  assert.deepEqual(actual, standard);
  assert.equal(actual.constraintRepair, undefined);
  assert.equal(repairWorker.terminated, false, "a caught repair error keeps the healthy Worker");

  const second = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  });
  await until(() => repairWorker.requests.length === 1);
  assert.equal(workers.length, 2, "the next repair reuses the retained Worker");
  repairWorker.respond();
  assert.ok((await second).constraintRepair);
});

test("unattributable traffic from an idle repair Worker retires it", async () => {
  const workers: { purpose: LayoutWorkerPurpose; worker: ManualWorker }[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = purpose === "persistent" ? new AutoWorker() : new ManualWorker();
    workers.push({ purpose, worker });
    return worker;
  });
  const options = { constraintRepair: { when: "always" as const, maxDurationMs: 1_000 } };

  const first = client.planIntegral(conflictingRequest(), options);
  await until(() => workers.length === 2);
  const repairWorker = workers[1].worker;
  repairWorker.respond();
  assert.ok((await first).constraintRepair);
  assert.equal(repairWorker.terminated, false);

  repairWorker.onmessage?.({ data: { garbage: true } });
  assert.equal(repairWorker.terminated, true, "an idle lane cannot attribute the message");

  const second = client.planIntegral(conflictingRequest(), options);
  await until(() => workers.length === 3);
  assert.equal(workers[2].purpose, "constraint-repair");
  workers[2].worker.respond();
  assert.ok((await second).constraintRepair);
});

test("deep repair never blocks FIFO work and failure retains streamed geometry", async () => {
  let persistent: ManualWorker | undefined;
  let repair: ManualWorker | undefined;
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = new ManualWorker();
    if (purpose === "persistent") persistent = worker;
    else repair = worker;
    return worker;
  });
  const request = conflictingRequest();
  const expected = planIntegralLayout(request);
  const completed: string[] = [];

  const deep = client.planIntegral(request, {
    constraintRepair: { when: "always", maxDurationMs: 1_000 },
  }).then((result) => {
    completed.push("repair");
    return result;
  });
  const second = client.planIntegral(requestToward("North")).then((result) => {
    completed.push("B");
    return result;
  });
  const third = client.planIntegral(requestToward("East")).then((result) => {
    completed.push("C");
    return result;
  });

  assert.ok(persistent);
  persistent.respond();
  assert.equal(persistent.requests.length, 1, "B starts before the repair continuation runs");
  persistent.respond();
  assert.equal(persistent.requests.length, 1, "C starts immediately after B");
  persistent.respond();
  await Promise.all([second, third]);
  assert.deepEqual(completed, ["B", "C"]);

  await until(() => repair !== undefined);
  const repairRequest = repair?.requests[0];
  assert.ok(repair && repairRequest);
  const retained = streamStrictImprovement(repair, repairRequest, expected);
  repair?.onerror?.({ error: new Error("optional repair failed") });
  const actual = await deep;
  assert.deepEqual(actual, retained);
  assert.equal(actual.constraintRepair, undefined);
  assert.deepEqual(completed, ["B", "C", "repair"]);
  assert.equal(repair?.terminated, true);
});

test("deep repairs use a separate single-active FIFO while standards keep flowing", async () => {
  let persistent: ManualWorker | undefined;
  const repairs: ManualWorker[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = new ManualWorker();
    if (purpose === "persistent") persistent = worker;
    else repairs.push(worker);
    return worker;
  });
  const completed: string[] = [];
  const options = { constraintRepair: { when: "always" as const, maxDurationMs: 1_000 } };
  const plans = ["A", "B", "C"].map((label) =>
    client.planIntegral(conflictingRequest(), options).then((result) => {
      completed.push(label);
      return result;
    })
  );

  assert.ok(persistent);
  persistent.respond();
  await until(() => repairs.length === 1);
  assert.equal(persistent.requests.length, 1, "B's standard starts during A's deep repair");
  persistent.respond();
  await Promise.resolve();
  assert.equal(persistent.requests.length, 1, "C's standard starts while A remains active");
  assert.equal(repairs[0].requests.length, 1, "B's deep repair queues behind A");
  persistent.respond();
  await Promise.resolve();
  assert.equal(repairs[0].requests.length, 1, "C's deep repair also queues behind A");

  repairs[0].respond();
  await until(() => repairs[0].requests.length === 1);
  assert.equal(repairs.length, 1, "B's repair reuses A's warm Worker");
  assert.equal(repairs[0].terminated, false);
  repairs[0].respond();
  await until(() => repairs[0].requests.length === 1);
  assert.equal(repairs.length, 1, "C's repair reuses the same warm Worker");
  repairs[0].respond();

  await Promise.all(plans);
  assert.deepEqual(completed, ["A", "B", "C"]);
  assert.equal(repairs.length, 1, "the serialized repair lane spawned exactly one Worker");
  assert.equal(repairs[0].terminated, false);
});

test("aborting a queued deep repair preserves the active and later repair", async () => {
  let persistent: ManualWorker | undefined;
  const repairs: ManualWorker[] = [];
  const client = new LayoutWorkerClient((purpose = "persistent") => {
    const worker = new ManualWorker();
    if (purpose === "persistent") persistent = worker;
    else repairs.push(worker);
    return worker;
  });
  const abortB = new AbortController();
  const options = { constraintRepair: { when: "always" as const, maxDurationMs: 1_000 } };
  const first = client.planIntegral(conflictingRequest(), options);
  const second = client.planIntegral(conflictingRequest(), {
    ...options,
    signal: abortB.signal,
  });
  const secondRejection = assert.rejects(second, (error: Error) => error.name === "AbortError");
  const third = client.planIntegral(conflictingRequest(), options);

  assert.ok(persistent);
  persistent.respond();
  await until(() => repairs.length === 1);
  persistent.respond();
  persistent.respond();
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(repairs.length, 1);

  abortB.abort();
  await secondRejection;
  assert.equal(repairs.length, 1);
  assert.equal(repairs[0].terminated, false, "aborting queued work never touches the Worker");
  repairs[0].respond();
  await until(() => repairs[0].requests.length === 1);
  assert.equal(repairs.length, 1, "C's repair reuses the Worker A finished on");
  repairs[0].respond();

  await Promise.all([first, third]);
  assert.equal(repairs[0].terminated, false);
});

test("default throughput remains one continuously occupied persistent FIFO Worker", async () => {
  const worker = new ManualWorker();
  const client = new LayoutWorkerClient(() => worker);
  const results = [
    client.planIntegral(requestToward("East")),
    client.planIntegral(requestToward("North")),
    client.planIntegral(requestToward("East")),
  ];

  assert.equal(worker.requests.length, 1);
  worker.respond();
  assert.equal(worker.requests.length, 1);
  worker.respond();
  assert.equal(worker.requests.length, 1);
  worker.respond();
  assert.deepEqual(
    (await Promise.all(results)).map((result) => result.positions.get("new")),
    [at(1, 0), at(0, -1), at(1, 0)],
  );
  assert.equal(worker.terminated, false);
});
