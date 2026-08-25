import assert from "node:assert/strict";
import test, { afterEach } from "node:test";
import {
  planIntegralLayout,
  planIntegralLayoutAsync,
  type GridPosition,
  type IntegralLayoutRequest,
  type LayoutTraceEvent,
} from "./layout.ts";
import {
  createLayoutModel,
  createLayoutWorkspace,
  planLayoutModel,
  planLayoutModelAsync,
  type LayoutModel,
} from "./model.ts";
import {
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
    const response = structuredClone(executeLayoutWorkerRequest(request));
    this.onmessage?.({ data: response });
  }
}

class ExecutingWorker extends ControlledWorker {
  override postMessage(message: unknown): void {
    super.postMessage(message);
    queueMicrotask(() => {
      if (!this.terminated && this.requests.length > 0) this.respondAt(0);
    });
  }
}

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

test("async model planning preserves the synchronous result without cloning callbacks", async () => {
  const worker = new ExecutingWorker();
  setLayoutWorkerFactoryForTesting(() => worker);
  const model = twoRoomModel();
  const expectedTrace: LayoutTraceEvent[] = [];
  const expected = planLayoutModel(
    model,
    { type: "reflow", anchor: "start" },
    { trace: (event) => expectedTrace.push(event) },
  );
  const actualTrace: LayoutTraceEvent[] = [];
  const actual = await planLayoutModelAsync(
    model,
    { type: "reflow", anchor: "start" },
    { trace: (event) => actualTrace.push(event) },
  );

  assert.deepEqual(actual, expected);
  assert.deepEqual(actualTrace, expectedTrace);
  assert.ok(actual.positions instanceof Map);
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

test("request IDs correlate concurrent responses even when a transport replies out of order", async () => {
  const worker = new ControlledWorker();
  const client = new LayoutWorkerClient(() => worker);
  const east = client.planIntegral(requestToward("East"));
  const north = client.planIntegral(requestToward("North"));
  assert.equal(worker.requests.length, 2);

  worker.respondAt(1);
  worker.respondAt(0);
  assert.deepEqual((await east).positions.get("new"), at(1, 0));
  assert.deepEqual((await north).positions.get("new"), at(0, -1));
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

test("a fatal Worker error rejects its batch, prevents default, and restarts lazily", async () => {
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
  const secondRejected = assert.rejects(second, /fake Worker crashed/);

  failed.onerror?.({
    error: new Error("fake Worker crashed"),
    preventDefault: () => {
      prevented = true;
    },
  });
  await Promise.all([firstRejected, secondRejected]);
  assert.equal(prevented, true);
  assert.equal(failed.terminated, true);

  const recovered = client.planIntegral(requestToward("East"));
  assert.equal(restarted.requests.length, 1);
  oldErrorHandler?.({ error: new Error("late stale Worker error") });
  restarted.respondAt(0);
  assert.deepEqual((await recovered).positions.get("new"), at(1, 0));
  assert.equal(factoryCalls, 2);
});

test("a synchronous postMessage failure rejects every request assigned to that Worker", async () => {
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

  await Promise.all([
    assert.rejects(first, /structured clone failed/),
    assert.rejects(second, /structured clone failed/),
  ]);
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
  assert.equal(sameLayoutSnapshot(model, {
    ...reordered,
    rooms: reordered.rooms.map((room) => room.id === "east"
      ? { ...room, movable: false }
      : room),
  }), false);
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
    { trace: (event) => expectedTrace.push(event) },
  );

  const result = await planStableLayoutSnapshot(
    () => structuredClone(snapshots[Math.min(loads++, snapshots.length - 1)]),
    { type: "reflow", anchor: "start" },
    { trace: (event) => actualTrace.push(event) },
  );

  assert.equal(loads, 4);
  assert.deepEqual(result, expected);
  assert.deepEqual(actualTrace, expectedTrace);
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
