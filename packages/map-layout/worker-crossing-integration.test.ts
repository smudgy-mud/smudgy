import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import {
  compactIntegralLayoutPlan,
  compareLayoutQuality,
  measureIntegralLayoutQuality,
  planIntegralLayoutAsync,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
} from "./layout.ts";
import {
  setLayoutWorkerFactoryForTesting,
  type LayoutWorkerLike,
} from "./worker-client.ts";
import { executeLayoutWorkerRequest } from "./worker-executor.ts";
import {
  LAYOUT_WORKER_PROTOCOL_VERSION,
  type LayoutWorkerRequest,
} from "./worker-protocol.ts";
import type { LayoutPlannerProgress } from "./planner-state.ts";

const at = (x: number, y: number): GridPosition => ({ x, y, level: 0 });

/** Execute the real protocol/executor in a microtask while retaining clone boundaries. */
class ExecutingWorker implements LayoutWorkerLike {
  onmessage: LayoutWorkerLike["onmessage"] = null;
  onmessageerror: LayoutWorkerLike["onmessageerror"] = null;
  onerror: LayoutWorkerLike["onerror"] = null;
  #terminated = false;

  postMessage(message: unknown): void {
    const request = structuredClone(message) as LayoutWorkerRequest;
    queueMicrotask(() => {
      if (this.#terminated) return;
      const response = structuredClone(executeLayoutWorkerRequest(request, (event) => {
        if (this.#terminated) return;
        this.onmessage?.({ data: structuredClone({
          protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
          id: request.id,
          operation: request.operation,
          progress: true,
          event,
        }) });
      }));
      if (!this.#terminated) this.onmessage?.({ data: response });
    });
  }

  terminate(): void {
    this.#terminated = true;
  }
}

afterEach(() => setLayoutWorkerFactoryForTesting());

function deepCrossingFixture(): IntegralLayoutRequest {
  // Every ray holds and no route is blocked, but three links cross. The
  // synchronous quick pass and ordinary constraint polish keep them; deep
  // search needs nested lobe transactions to clear them, making this a useful
  // boundary fixture rather than another fabricated progress message.
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
    id: `r${index}`,
    position: at(x, y),
    movable: index !== 0,
  }));
  const edges = parents.flatMap((parent, offset) => {
    const child = offset + 1;
    const forward = directions[offset];
    const reverse = reverses[forward];
    return [
      { from: `r${parent}`, to: `r${child}`, direction: forward },
      { from: `r${child}`, to: `r${parent}`, direction: reverse },
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

function deterministicFingerprint(plan: IntegralLayoutPlan): string {
  const crossing = plan.constraintRepair?.crossingRepair;
  return JSON.stringify({
    positions: [...plan.positions].sort(([left], [right]) => left.localeCompare(right)),
    movedExisting: [...plan.movedExisting].sort(),
    quality: plan.quality,
    crossing: crossing && {
      completed: crossing.completed,
      cancelled: crossing.cancelled,
      exhausted: crossing.exhausted,
      crossingsConsidered: crossing.crossingsConsidered,
      macrosConsidered: crossing.macrosConsidered,
      pushClosures: crossing.pushClosures,
      maxDepth: crossing.maxDepth,
      visitedStates: crossing.visitedStates,
    },
  });
}

async function runDeepCrossingFixture(): Promise<{
  result: IntegralLayoutPlan;
  progress: LayoutPlannerProgress[];
}> {
  const progress: LayoutPlannerProgress[] = [];
  const result = await planIntegralLayoutAsync(deepCrossingFixture(), {
    constraintRepair: {
      when: "always",
      maxDurationMs: 10_000,
      maxRestarts: 1,
      maxLayouts: 1,
      maxCrossingWork: 200,
    },
    onProgress: (update) => progress.push(update as LayoutPlannerProgress),
  });
  return { result, progress };
}

test("a real Worker request streams and returns an accepted deep crossing repair", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const { result, progress } = await runDeepCrossingFixture();

  const standard = progress.find((update) =>
    update.snapshot.phase === "deterministic layout complete"
  );
  assert.ok((standard?.snapshot.standardQuality?.linkCrossings ?? 0) > 0);

  // Deep search may publish several strict improvements. Its last accepted
  // complete candidate is the durable result returned across the clone boundary.
  const accepted = progress.filter((update) =>
    update.snapshot.phase === "crossing deep improvement" && update.improvement
  ).at(-1);
  assert.ok(accepted?.improvement);
  assert.equal(accepted.improvement.quality.linkCrossings, 0);
  assert.deepEqual(accepted.improvement.positions, result.positions);
  assert.deepEqual(accepted.improvement.movedExisting, result.movedExisting);
  assert.equal(accepted.improvement.positions.size, 12);
  assert.equal(
    new Set([...accepted.improvement.positions.values()].map(({ x, y, level }) =>
      `${x},${y},${level}`
    )).size,
    12,
    "a streamed complete candidate cannot contain room collisions",
  );
  for (const position of accepted.improvement.positions.values()) {
    assert.ok(Number.isSafeInteger(position.x));
    assert.ok(Number.isSafeInteger(position.y));
    assert.ok(Number.isSafeInteger(position.level));
  }

  const work = accepted.snapshot.work;
  assert.ok(work.crossingsConsidered >= 1);
  assert.ok(work.macrosConsidered >= 1);
  assert.ok(work.pushClosures >= 1);
  assert.ok(work.maxDepth >= 2);
  assert.ok(work.visitedStates >= 1);
  const complete = progress.find((update) =>
    update.snapshot.phase === "crossing deep complete"
  );
  assert.ok(complete);
  assert.ok(complete.snapshot.work.macrosConsidered >= work.macrosConsidered);
  assert.equal(result.quality.linkCrossings, 0);
  const report = result.constraintRepair?.crossingRepair;
  assert.ok(report, "the required crossing report survives the Worker clone boundary");
  assert.equal(report.completed, true);
  assert.equal(report.cancelled, false);
  assert.equal(report.exhausted, false);
  assert.ok(report.elapsedMs >= 0);
  assert.ok(report.crossingsConsidered >= 1);
  assert.ok(report.macrosConsidered >= 1);
  assert.ok(report.pushClosures >= 1);
  assert.ok(report.maxDepth >= 2);
  assert.ok(report.visitedStates >= 1);
  const constraintReport = result.constraintRepair;
  assert.ok(constraintReport);
  assert.equal(typeof constraintReport.extensionSearch.completed, "boolean");
  assert.equal(typeof constraintReport.extensionSearch.cancelled, "boolean");
  assert.equal(typeof constraintReport.extensionSearch.exhausted, "boolean");
  assert.equal(typeof constraintReport.maskDiversification.completed, "boolean");
  assert.equal(typeof constraintReport.maskDiversification.exhausted, "boolean");
  if (constraintReport.geometricFixedPoint) {
    assert.equal(constraintReport.extensionSearch.completed, true);
    assert.equal(constraintReport.maskDiversification.completed, true);
  }
  for (const field of [
    "rawIncumbents",
    "softIncumbents",
    "distinctLayouts",
    "maskDiversifications",
    "separatorStates",
    "separatorBranches",
    "separatorCyclePrunes",
  ] as const) {
    assert.ok(Number.isSafeInteger(constraintReport[field]));
    assert.ok(constraintReport[field] >= 0);
  }
  if (constraintReport.firstIncumbentMs !== undefined) {
    assert.ok(Number.isFinite(constraintReport.firstIncumbentMs));
    assert.ok(constraintReport.firstIncumbentMs >= 0);
  }
  const constraintProgress = progress.filter((update) =>
    update.snapshot.phase.startsWith("constraint ")
  );
  for (const field of [
    "rawIncumbents",
    "softIncumbents",
    "distinctLayouts",
    "maskDiversifications",
    "separatorStates",
    "separatorBranches",
    "separatorCyclePrunes",
  ] as const) {
    assert.ok(constraintProgress.every((update, index) => index === 0 ||
      update.snapshot.work[field] >= constraintProgress[index - 1].snapshot.work[field]
    ));
  }

  const fingerprint = deterministicFingerprint(result);
  for (let replay = 0; replay < 2; replay += 1) {
    const repeated = await runDeepCrossingFixture();
    assert.equal(deterministicFingerprint(repeated.result), fingerprint);
    assert.ok(repeated.progress.some((update) =>
      update.snapshot.phase === "crossing deep improvement" && update.improvement
    ));
  }
});

test("a quiet polish streams only finished layouts, none of them above its result", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  // The same bridge tree stored scattered across the map.
  const scattered = [
    [-8, 11], [-5, 5], [10, 2], [-2, -3], [-7, -6], [9, -4],
    [-8, -3], [-8, 5], [8, 4], [1, -6], [5, -10], [-6, 0],
  ] as const;
  const fixture = deepCrossingFixture();
  const residents = fixture.residents.map((resident, index) => ({
    ...resident,
    position: at(scattered[index][0], scattered[index][1]),
  }));
  const center = residents[0].position;
  // The mapper's quiet polish sends its local chart, every room of it a resident.
  const request: IntegralLayoutRequest = {
    ...fixture,
    residents,
    nodes: residents.map((resident) => ({
      id: resident.id,
      relative: {
        x: resident.position.x - center.x,
        y: resident.position.y - center.y,
        level: 0,
      },
    })),
  };
  const improvements: IntegralLayoutPlan[] = [];
  const result = await planIntegralLayoutAsync(request, {
    constraintRepair: {
      when: "always",
      maxDurationMs: 10_000,
      maxRestarts: 1,
      maxLayouts: 1,
      maxCrossingWork: 200,
    },
    onProgress: (update) => {
      if (update.improvement) improvements.push(update.improvement);
    },
  });

  assert.ok(improvements.length >= 2, "the polish streams the standard plan and its repairs");
  for (const [index, improvement] of improvements.entries()) {
    const plan: IntegralLayoutPlan = {
      positions: improvement.positions,
      movedExisting: new Set(),
      quality: measureIntegralLayoutQuality(improvement.positions, request.edges),
    };
    assert.equal(
      compactIntegralLayoutPlan(request, plan, { axisGroupCompaction: false }),
      plan,
      `streamed layout ${index} is not at the cheap compaction fixed point`,
    );
    assert.ok(compareLayoutQuality(result.quality, improvement.quality) >= 0);
  }
  // A streamed layout better than the repaired plan would have replaced it,
  // report and all.
  assert.ok(result.constraintRepair, "the repaired plan, with its report, is the result");
});
