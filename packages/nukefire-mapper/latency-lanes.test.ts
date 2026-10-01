import assert from "node:assert/strict";
import test from "node:test";
import {
  SnapshotLatencyLanes,
  type SnapshotLaneTimers,
} from "./latency-lanes.ts";
import {
  AreaPolishEntryTracker,
  MAX_FRUITLESS_QUIET_RESUMES,
  QuietPolishClaims,
  QuietResumeBudget,
} from "./polish-state.ts";

interface Snapshot {
  center: number;
  revision: number;
}

interface TimerEntry {
  at: number;
  callback: () => void;
  cleared: boolean;
}

class ManualTimers implements SnapshotLaneTimers {
  #now = 0;
  readonly #entries: TimerEntry[] = [];

  set(callback: () => void, delayMs: number): TimerEntry {
    const entry = { at: this.#now + delayMs, callback, cleared: false };
    this.#entries.push(entry);
    return entry;
  }

  clear(handle: unknown): void {
    (handle as TimerEntry).cleared = true;
  }

  advance(ms: number): void {
    this.#now += ms;
    while (true) {
      const due = this.#entries
        .filter((entry) => !entry.cleared && entry.at <= this.#now)
        .sort((left, right) => left.at - right.at)[0];
      if (!due) return;
      due.cleared = true;
      due.callback();
    }
  }
}

function deferred(): { promise: Promise<void>; resolve: () => void } {
  let resolve!: () => void;
  const promise = new Promise<void>((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

/** Lets every continuation already queued run, however many hops it takes. */
function flush(): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, 0));
}

test("full reflow waits for a deterministic quiet window after prompt topology", async () => {
  const timers = new ManualTimers();
  const topology: number[] = [];
  const reflows: number[] = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async (snapshot) => {
      topology.push(snapshot.center);
    },
    runFullReflow: async (snapshot) => {
      reflows.push(snapshot.center);
    },
    timers,
  });

  lanes.start();
  lanes.enqueue({ center: 101, revision: 1 });
  await settle();
  assert.deepEqual(topology, [101]);

  timers.advance(349);
  await settle();
  assert.deepEqual(reflows, []);
  timers.advance(1);
  await settle();
  assert.deepEqual(reflows, [101]);
});

test("a newer snapshot aborts only the active full reflow and resets quiet timing", async () => {
  const timers = new ManualTimers();
  const firstReflow = deferred();
  const topology: number[] = [];
  const reflows: number[] = [];
  const errors: unknown[] = [];
  let firstSignal: AbortSignal | undefined;
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async (snapshot) => {
      topology.push(snapshot.center);
    },
    runFullReflow: async (snapshot, signal) => {
      reflows.push(snapshot.center);
      if (!firstSignal) {
        firstSignal = signal;
        await firstReflow.promise;
      }
    },
    onError: (_lane, _snapshot, error) => errors.push(error),
    quietWindowMs: 300,
    timers,
  });

  lanes.start();
  lanes.enqueue({ center: 101, revision: 1 });
  await settle();
  timers.advance(300);
  await settle();
  assert.deepEqual(reflows, [101]);

  lanes.enqueue({ center: 102, revision: 2 });
  assert.equal(firstSignal?.aborted, true);
  assert.deepEqual(topology, [101]);

  firstReflow.resolve();
  await settle();
  assert.deepEqual(topology, [101, 102]);
  timers.advance(299);
  await settle();
  assert.deepEqual(reflows, [101]);
  timers.advance(1);
  await settle();
  assert.deepEqual(reflows, [101, 102]);
  assert.deepEqual(errors, []);
});

test("distinct centers survive topology serialization while adjacent duplicates coalesce", async () => {
  const timers = new ManualTimers();
  const first = deferred();
  const ingested: Snapshot[] = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async (snapshot) => {
      ingested.push(snapshot);
      if (snapshot.center === 1) await first.promise;
    },
    runFullReflow: async () => {},
    timers,
  });

  lanes.start();
  lanes.enqueue({ center: 1, revision: 1 });
  lanes.enqueue({ center: 2, revision: 1 });
  lanes.enqueue({ center: 2, revision: 2 });
  lanes.enqueue({ center: 3, revision: 1 });
  assert.deepEqual(ingested, [{ center: 1, revision: 1 }]);
  assert.equal(lanes.pendingTopologyCount, 2);

  first.resolve();
  await settle();
  assert.deepEqual(ingested, [
    { center: 1, revision: 1 },
    { center: 2, revision: 2 },
    { center: 3, revision: 1 },
  ]);
});

test("current-room following is independent of blocked layout work", async () => {
  const timers = new ManualTimers();
  const blocked = deferred();
  const followed: number[] = [];
  const topologyStarted: number[] = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: (snapshot) => followed.push(snapshot.center),
    runTopology: async (snapshot) => {
      topologyStarted.push(snapshot.center);
      if (snapshot.center === 1) await blocked.promise;
    },
    runFullReflow: async () => {},
    timers,
  });

  lanes.start();
  lanes.enqueue({ center: 1, revision: 1 });
  lanes.enqueue({ center: 2, revision: 1 });
  assert.deepEqual(followed, [1, 2]);
  assert.deepEqual(topologyStarted, [1]);

  blocked.resolve();
  await settle();
  assert.deepEqual(topologyStarted, [1, 2]);
});

test("a failed latest topology snapshot is never promoted into the full-reflow lane", async () => {
  const timers = new ManualTimers();
  const errors: unknown[] = [];
  const reflows: number[] = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async () => {
      throw new Error("topology failed");
    },
    runFullReflow: async (snapshot) => {
      reflows.push(snapshot.center);
    },
    onError: (_lane, _snapshot, error) => errors.push(error),
    timers,
  });

  lanes.start();
  lanes.enqueue({ center: 1, revision: 1 });
  timers.advance(350);
  await settle();
  assert.equal(errors.length, 1);
  assert.deepEqual(reflows, []);
});

test("only the first displacement of an active reflow notifies the abort hook", async () => {
  const timers = new ManualTimers();
  const blocked = deferred();
  const aborts: Array<[number, number]> = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async () => {},
    runFullReflow: async (snapshot) => {
      if (snapshot.center === 1) await blocked.promise;
    },
    onFullReflowAborted: (aborted, incoming) =>
      aborts.push([aborted.center, incoming.center]),
    quietWindowMs: 300,
    timers,
  });

  lanes.start();
  // With no active reflow there is nothing to displace.
  lanes.enqueue({ center: 1, revision: 1 });
  assert.deepEqual(aborts, []);
  await settle();
  timers.advance(300);
  await settle();
  assert.equal(lanes.fullReflowActive, true);

  lanes.enqueue({ center: 2, revision: 1 });
  // The already-aborted task is not reported again.
  lanes.enqueue({ center: 3, revision: 1 });
  assert.deepEqual(aborts, [[1, 2]]);

  blocked.resolve();
  await settle();
  assert.deepEqual(aborts, [[1, 2]]);
});

test("stop aborts the active reflow without reporting a displacement", async () => {
  const timers = new ManualTimers();
  const blocked = deferred();
  const aborts: Array<[number, number]> = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async () => {},
    runFullReflow: async () => {
      await blocked.promise;
    },
    onFullReflowAborted: (aborted, incoming) =>
      aborts.push([aborted.center, incoming.center]),
    quietWindowMs: 300,
    timers,
  });

  lanes.start();
  lanes.enqueue({ center: 1, revision: 1 });
  await settle();
  timers.advance(300);
  await settle();
  assert.equal(lanes.fullReflowActive, true);

  lanes.stop();
  blocked.resolve();
  await settle();
  assert.deepEqual(aborts, []);
});

test("exclusive work displaces the active reflow as its own snapshot and starts once it returns", async () => {
  const timers = new ManualTimers();
  const blocked = deferred();
  const aborts: Array<[number, number]> = [];
  const signals: AbortSignal[] = [];
  const events: string[] = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async () => {},
    runFullReflow: async (_snapshot, signal) => {
      signals.push(signal);
      events.push("reflow");
      if (signals.length === 1) await blocked.promise;
      events.push("reflow returned");
    },
    onFullReflowAborted: (aborted, incoming) =>
      aborts.push([aborted.center, incoming.center]),
    quietWindowMs: 300,
    timers,
  });

  lanes.start();
  // With the lanes idle, exclusive work starts at once.
  assert.equal(await lanes.exclusive(async () => "alone"), "alone");
  lanes.enqueue({ center: 4, revision: 1 });
  await flush();
  timers.advance(300);
  await flush();
  assert.equal(lanes.fullReflowActive, true);

  const work = lanes.exclusive(async () => {
    events.push("exclusive");
    return 7;
  });
  assert.equal(signals[0].aborted, true);
  assert.deepEqual(aborts, [[4, 4]]);
  await flush();
  assert.deepEqual(events, ["reflow"]);
  blocked.resolve();
  assert.equal(await work, 7);
  assert.deepEqual(events, ["reflow", "reflow returned", "exclusive"]);

  // The displaced reflow's quiet window starts over once the work returns.
  await flush();
  timers.advance(299);
  await flush();
  assert.equal(signals.length, 1);
  timers.advance(1);
  await flush();
  assert.equal(signals.length, 2);
});

test("exclusive work gets in between topology snapshots and holds the rest back until it returns", async () => {
  const timers = new ManualTimers();
  const first = deferred();
  const inside = deferred();
  const events: string[] = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async (snapshot) => {
      events.push(`topology ${snapshot.center}`);
      if (snapshot.center === 1) await first.promise;
    },
    runFullReflow: async () => {},
    timers,
  });

  lanes.start();
  lanes.enqueue({ center: 1, revision: 1 });
  lanes.enqueue({ center: 2, revision: 1 });
  const work = lanes.exclusive(async () => {
    events.push("exclusive");
    await inside.promise;
    events.push("exclusive returned");
  });
  lanes.enqueue({ center: 3, revision: 1 });
  await flush();
  assert.deepEqual(events, ["topology 1"]);

  first.resolve();
  await flush();
  assert.deepEqual(events, ["topology 1", "exclusive"]);
  assert.equal(lanes.pendingTopologyCount, 2);

  inside.resolve();
  await work;
  await flush();
  assert.deepEqual(events, [
    "topology 1",
    "exclusive",
    "exclusive returned",
    "topology 2",
    "topology 3",
  ]);
});

test("exclusive work runs in the order it was asked for, and after stop too", async () => {
  const timers = new ManualTimers();
  const first = deferred();
  const events: string[] = [];
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async () => {},
    runFullReflow: async () => {},
    timers,
  });

  lanes.start();
  const one = lanes.exclusive(async () => {
    events.push("one");
    await first.promise;
  });
  const two = lanes.exclusive(async () => {
    events.push("two");
  });
  await flush();
  assert.deepEqual(events, ["one"]);
  lanes.stop();
  first.resolve();
  await Promise.all([one, two]);
  assert.deepEqual(events, ["one", "two"]);
  assert.equal(await lanes.exclusive(async () => "stopped"), "stopped");
});

/**
 * Mirrors the mapper's quiet-polish wiring: the reflow lane consumes the
 * visit's one attempt before cancelable work, and the abort hook restores it
 * when the displacing snapshot stays within the polished area — charging the
 * fruitless-resume budget unless the displaced pass committed durable
 * progress.
 */
function quietPolishHarness(): {
  areaKey: string;
  timers: ManualTimers;
  tracker: AreaPolishEntryTracker;
  deferredAreas: Set<string>;
  attempts: Snapshot[];
  completed: Snapshot[];
  lanes: SnapshotLatencyLanes<Snapshot>;
  releaseActive: () => void;
  markActiveProgress: () => void;
} {
  const areaKey = "area-1";
  // Centers 5 and 7 are mapped rooms of the polished area; center 6 is not.
  const roomAreas = new Map<number, string>([
    [5, areaKey],
    [7, areaKey],
    [6, "area-2"],
  ]);
  const timers = new ManualTimers();
  const tracker = new AreaPolishEntryTracker();
  const claims = new QuietPolishClaims<Snapshot>((aborted, incoming) => {
    const abortedArea = roomAreas.get(aborted.center);
    return abortedArea !== undefined && abortedArea === roomAreas.get(incoming.center);
  });
  const budget = new QuietResumeBudget();
  const deferredAreas = new Set<string>();
  const attempts: Snapshot[] = [];
  const completed: Snapshot[] = [];
  let release: (() => void) | undefined;
  let active: Snapshot | undefined;
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async () => {},
    runFullReflow: async (snapshot, signal) => {
      if (!deferredAreas.has(areaKey)) return;
      claims.record(snapshot, areaKey, {
        retryConsumed: tracker.consumeRetry(areaKey),
        deferredRemoved: deferredAreas.delete(areaKey),
      });
      attempts.push(snapshot);
      active = snapshot;
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      if (signal.aborted) return;
      completed.push(snapshot);
      claims.discharge(snapshot, areaKey);
      budget.reset(areaKey);
    },
    onFullReflowAborted: (aborted, incoming) => {
      for (const [key, claim] of claims.settle(aborted, incoming)) {
        if (tracker.currentAreaKey !== key) continue;
        if (!budget.allowResume(key, claim.progressed === true)) continue;
        if (claim.retryConsumed) tracker.markPending(key);
        if (claim.deferredRemoved) deferredAreas.add(key);
      }
    },
    quietWindowMs: 300,
    timers,
  });
  tracker.observe(areaKey, true);
  deferredAreas.add(areaKey);
  return {
    areaKey,
    timers,
    tracker,
    deferredAreas,
    attempts,
    completed,
    lanes,
    releaseActive: () => release?.(),
    markActiveProgress: () => {
      if (active) claims.markProgress(active, areaKey);
    },
  };
}

test("equivalent same-center chatter mid-polish re-arms the visit's attempt", async () => {
  const harness = quietPolishHarness();
  harness.lanes.start();
  harness.lanes.enqueue({ center: 5, revision: 1 });
  await settle();
  harness.timers.advance(300);
  await settle();
  assert.equal(harness.attempts.length, 1);
  assert.equal(harness.tracker.retryAreaKey, undefined);
  assert.equal(harness.deferredAreas.size, 0);

  // Same-center, identical-payload chatter arrives as a fresh clone.
  harness.lanes.enqueue({ center: 5, revision: 1 });
  assert.equal(harness.tracker.retryAreaKey, harness.areaKey);
  assert.equal(harness.deferredAreas.has(harness.areaKey), true);

  harness.releaseActive();
  await settle();
  harness.timers.advance(300);
  await settle();
  // A new quiet attempt runs within the same visit instead of forfeiting.
  assert.equal(harness.attempts.length, 2);
  harness.releaseActive();
  await settle();
  assert.deepEqual(harness.completed, [{ center: 5, revision: 1 }]);
  assert.equal(harness.tracker.retryAreaKey, undefined);
});

test("movement within the polished area mid-search resumes the visit's attempt", async () => {
  const harness = quietPolishHarness();
  harness.lanes.start();
  harness.lanes.enqueue({ center: 5, revision: 1 });
  await settle();
  harness.timers.advance(300);
  await settle();
  assert.equal(harness.attempts.length, 1);

  // Movement to a different mapped room of the same area. The payload
  // changes — Map.Local is a window around the player — but the polish
  // opportunity is unchanged, so the attempt is restored.
  harness.lanes.enqueue({ center: 7, revision: 2 });
  assert.equal(harness.tracker.retryAreaKey, harness.areaKey);
  assert.equal(harness.deferredAreas.has(harness.areaKey), true);

  harness.releaseActive();
  await settle();
  harness.timers.advance(300);
  await settle();
  // A new quiet attempt runs this visit, planning the newest snapshot.
  assert.equal(harness.attempts.length, 2);
  assert.deepEqual(harness.attempts[1], { center: 7, revision: 2 });
  harness.releaseActive();
  await settle();
  assert.deepEqual(harness.completed, [{ center: 7, revision: 2 }]);
});

test("departing the area still aborts and forfeits until reentry", async () => {
  const harness = quietPolishHarness();
  harness.lanes.start();
  harness.lanes.enqueue({ center: 5, revision: 1 });
  await settle();
  harness.timers.advance(300);
  await settle();
  assert.equal(harness.attempts.length, 1);

  harness.lanes.enqueue({ center: 6, revision: 1 });
  assert.equal(harness.tracker.retryAreaKey, undefined);
  assert.equal(harness.deferredAreas.size, 0);

  harness.releaseActive();
  await settle();
  harness.timers.advance(300);
  await settle();
  assert.equal(harness.attempts.length, 1);
  assert.equal(harness.completed.length, 0);
});

test("repeated fruitless displacements exhaust the resume budget and forfeit", async () => {
  const harness = quietPolishHarness();
  harness.lanes.start();
  harness.lanes.enqueue({ center: 5, revision: 1 });
  await settle();

  // Each cycle starts a quiet attempt, displaces it with same-area movement
  // before it commits anything, and charges one fruitless resumption.
  for (let resume = 1; resume <= MAX_FRUITLESS_QUIET_RESUMES; resume += 1) {
    harness.timers.advance(300);
    await settle();
    assert.equal(harness.attempts.length, resume);
    harness.lanes.enqueue({ center: resume % 2 === 0 ? 5 : 7, revision: resume });
    assert.equal(
      harness.deferredAreas.has(harness.areaKey),
      true,
      `resumption ${resume} stays within the budget`,
    );
    harness.releaseActive();
    await settle();
  }

  harness.timers.advance(300);
  await settle();
  assert.equal(harness.attempts.length, MAX_FRUITLESS_QUIET_RESUMES + 1);

  // The next fruitless displacement finds the allowance spent and forfeits
  // the visit; the durable hint covers the next entry.
  harness.lanes.enqueue({ center: 5, revision: 100 });
  assert.equal(harness.tracker.retryAreaKey, undefined);
  assert.equal(harness.deferredAreas.size, 0);
  harness.releaseActive();
  await settle();
  harness.timers.advance(300);
  await settle();
  assert.equal(harness.attempts.length, MAX_FRUITLESS_QUIET_RESUMES + 1);
  assert.equal(harness.completed.length, 0);
});

test("a displaced pass that committed an improvement restarts the budget", async () => {
  const harness = quietPolishHarness();
  harness.lanes.start();
  harness.lanes.enqueue({ center: 5, revision: 1 });
  await settle();

  // Spend the entire allowance on fruitless displacements.
  for (let resume = 1; resume <= MAX_FRUITLESS_QUIET_RESUMES; resume += 1) {
    harness.timers.advance(300);
    await settle();
    harness.lanes.enqueue({ center: resume % 2 === 0 ? 5 : 7, revision: resume });
    harness.releaseActive();
    await settle();
  }
  harness.timers.advance(300);
  await settle();
  assert.equal(harness.attempts.length, MAX_FRUITLESS_QUIET_RESUMES + 1);

  // This attempt durably commits an improvement before its displacement.
  // A fruitful displacement restores the attempt and restarts the allowance.
  harness.markActiveProgress();
  harness.lanes.enqueue({ center: 7, revision: 100 });
  assert.equal(harness.deferredAreas.has(harness.areaKey), true);
  harness.releaseActive();
  await settle();
  harness.timers.advance(300);
  await settle();
  assert.equal(harness.attempts.length, MAX_FRUITLESS_QUIET_RESUMES + 2);

  // The restarted allowance covers further fruitless displacements.
  harness.lanes.enqueue({ center: 5, revision: 101 });
  assert.equal(harness.deferredAreas.has(harness.areaKey), true);
});

test("stop and restart invalidates old lane work before draining the new generation", async () => {
  const timers = new ManualTimers();
  const started: Array<[number, number]> = [];
  const reflows: Array<[number, number]> = [];
  let oldTopologySignal: AbortSignal | undefined;
  const lanes = new SnapshotLatencyLanes<Snapshot>({
    snapshotKey: (snapshot) => snapshot.center,
    followCurrent: () => {},
    runTopology: async (snapshot, signal) => {
      // Each call starts synchronously inside its own generation, so the
      // lane's live counter identifies the generation that dispatched it.
      started.push([snapshot.center, lanes.generation]);
      if (snapshot.center === 1) {
        oldTopologySignal = signal;
        await new Promise<void>((_resolve, reject) => {
          signal.addEventListener("abort", () => reject(signal.reason), { once: true });
        });
      }
    },
    runFullReflow: async (snapshot, _signal, generation) => {
      reflows.push([snapshot.center, generation]);
    },
    quietWindowMs: 300,
    timers,
  });

  lanes.start();
  lanes.enqueue({ center: 1, revision: 1 });
  const oldGeneration = lanes.generation;
  lanes.stop();
  assert.equal(oldTopologySignal?.aborted, true);
  lanes.start();
  lanes.enqueue({ center: 2, revision: 1 });
  assert.notEqual(lanes.generation, oldGeneration);
  assert.deepEqual(started, [[1, oldGeneration]]);

  timers.advance(300);
  await settle();
  assert.deepEqual(started, [
    [1, oldGeneration],
    [2, lanes.generation],
  ]);
  assert.deepEqual(reflows, [[2, lanes.generation]]);
});
