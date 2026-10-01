import assert from "node:assert/strict";
import test from "node:test";
import {
  AREA_POLISH_AUTOMATIC_EFFORT,
  AREA_POLISH_EXHAUSTED_FINGERPRINT_PROPERTY,
  AREA_POLISH_MEMO_SCHEMA_VERSION,
  AREA_POLISH_PENDING_PROPERTY,
  AREA_POLISH_PENDING_VALUE,
  AREA_POLISH_PERFECT_EFFORT,
  AREA_POLISH_RETRY_COOLDOWN_MS,
  AREA_POLISH_SEAMS_PROPERTY,
  AREA_POLISH_SEARCH_GENERATION,
  AreaPolishEntryTracker,
  areaPolishEligibility,
  areaPolishMemo,
  areaPolishMemoPropertyValue,
  areaPolishNeedsContextEvaluation,
  areaPolishPending,
  areaPolishSeams,
  areaPolishSeamsPropertyValue,
  areaPolishTerminalReason,
  createAreaPolishPlanningContext,
  equivalentSnapshotPayloads,
  MAX_AREA_POLISH_MEMO_CONTEXTS,
  MAX_AREA_POLISH_SEAMS,
  MAX_FRUITLESS_QUIET_RESUMES,
  polishExhaustedFingerprint,
  polishNotSearchedReason,
  polishRetrySuppressed,
  QuietPolishClaims,
  QuietResumeBudget,
  reduceAreaPolishMemo,
  reduceAreaPolishSeams,
  reduceAreaPolishState,
  reportsCeilingExhaustion,
  reportsUnsearchableGeometry,
  type AreaPolishEvent,
  type AreaPolishMemo,
  type AreaPolishPlanningContext,
  type AreaPolishReport,
  type AreaPolishSettlement,
} from "./polish-state.ts";

test("durable names stay compatible while schema and search versions are independent", () => {
  assert.equal(AREA_POLISH_PENDING_PROPERTY, "nukefire.layout.polish-pending");
  assert.equal(
    AREA_POLISH_EXHAUSTED_FINGERPRINT_PROPERTY,
    "nukefire.layout.polish-exhausted-fingerprint",
  );
  assert.equal(AREA_POLISH_PENDING_VALUE, "true");
  assert.equal(AREA_POLISH_MEMO_SCHEMA_VERSION, 3);
  assert.equal(AREA_POLISH_SEARCH_GENERATION, 2);
  assert.equal(AREA_POLISH_AUTOMATIC_EFFORT, 1);
  assert.equal(AREA_POLISH_PERFECT_EFFORT, 2);
  assert.equal(MAX_AREA_POLISH_MEMO_CONTEXTS, 32);
  assert.equal(areaPolishPending(undefined), false);
  assert.equal(areaPolishPending(""), false);
  assert.equal(areaPolishPending("pending"), false);
  assert.equal(areaPolishPending(" TRUE "), true, "legacy true remains pending");
  assert.equal(polishExhaustedFingerprint(undefined), undefined);
  assert.equal(polishExhaustedFingerprint(""), undefined);
  assert.equal(polishExhaustedFingerprint("  "), undefined);
  assert.equal(polishExhaustedFingerprint("[fp]"), "[fp]");
});

function context(overrides: {
  geometry?: string;
  center?: string;
  chartX?: number;
  maxLayouts?: number;
  generation?: number;
  effort?: number;
} = {}): AreaPolishPlanningContext {
  return createAreaPolishPlanningContext({
    geometryFingerprint: overrides.geometry ?? "geometry-a",
    centerId: overrides.center ?? "room:1",
    nodes: [
      { id: "room:2", relative: { x: overrides.chartX ?? 1, y: 0, level: 0 } },
      { id: "room:1", relative: { x: 0, y: 0, level: 0 } },
    ],
    edges: [
      { from: "room:2", to: "room:1", direction: "West" },
      { from: "room:1", to: "room:2", direction: "East" },
    ],
    searchForPerfectLayouts: false,
    policy: {
      when: "always",
      maxDurationMs: 10_000,
      maxRestarts: 4_096,
      maxLayouts: overrides.maxLayouts ?? 2,
      maxPolishTournaments: 2,
      maxPolishPasses: 3,
      maxExtensionStates: 32_768,
      maxLiveSearchNodes: 4_096,
      maxMaskDiversifications: 64,
      maxCrossingWork: 512,
    },
    searchGeneration: overrides.generation,
    automaticEffort: overrides.effort,
  });
}

function record(
  planning: Readonly<AreaPolishPlanningContext>,
  terminalReason: AreaPolishSettlement["terminalReason"] = "fixed-point",
): AreaPolishSettlement {
  return {
    key: planning.key,
    searchGeneration: planning.searchGeneration,
    automaticEffort: planning.automaticEffort,
    policyKey: planning.policyKey,
    terminalReason,
  };
}

function memo(
  planning: Readonly<AreaPolishPlanningContext>,
  settlements: readonly AreaPolishSettlement[] = [record(planning)],
): AreaPolishMemo {
  return { kind: "contexts", geometryFingerprint: planning.geometryFingerprint, settlements };
}

function ceilingReport(overrides: Partial<AreaPolishReport> = {}): AreaPolishReport {
  return {
    geometricFixedPoint: false,
    cutoff: "extensions",
    polishCutoff: "tournaments",
    extensionSearch: { cancelled: false, exhausted: true },
    crossingRepair: { cancelled: false, exhausted: false },
    ...overrides,
  };
}

test("coverage, generation, effort, and policy are distinct planning dimensions", () => {
  const base = context();
  assert.notEqual(context({ center: "room:2" }).key, base.key);
  assert.notEqual(context({ chartX: 2 }).key, base.key);
  assert.equal(context({ generation: 2 }).key, base.key);
  assert.equal(context({ effort: 2 }).key, base.key);
  assert.equal(context({ maxLayouts: 3 }).key, base.key);
  assert.notEqual(context({ maxLayouts: 3 }).policyKey, base.policyKey);
});

test("v3 evidence round-trips; raw and v2 values migrate without suppressing", () => {
  const planning = context();
  const state = memo(planning);
  assert.deepEqual(areaPolishMemo(areaPolishMemoPropertyValue(state)), state);
  assert.equal(polishRetrySuppressed(state, planning), true);

  for (const value of [
    "legacy-geometry",
    JSON.stringify({ v: 2, g: planning.geometryFingerprint, c: [planning.key] }),
    "{broken",
  ]) {
    const legacy = areaPolishMemo(value);
    assert.deepEqual(legacy, { kind: "legacy", propertyValue: value });
    assert.equal(polishRetrySuppressed(legacy, planning), false);
  }
});

test("fixed point and deterministic ceiling settle achieved automatic effort", () => {
  const planning = context();
  for (const [report, reason] of [
    [{ geometricFixedPoint: true }, "fixed-point"],
    [ceilingReport(), "ceiling"],
  ] as const) {
    const transition = reduceAreaPolishMemo(undefined, {
      kind: "polish-completed",
      report,
      context: planning,
    });
    assert.ok(transition.memo?.kind === "contexts");
    assert.deepEqual(transition.memo.settlements, [record(planning, reason)]);
    assert.equal(polishRetrySuppressed(transition.memo, planning), true);
    assert.deepEqual(reduceAreaPolishState(true, {
      kind: "polish-completed",
      report,
      context: planning,
    }), { pending: false, propertyValue: "" });
  }
});

test("an already-perfect standard plan settles without a repair report", () => {
  const planning = context();
  const event = {
    kind: "polish-completed" as const,
    terminalReason: "perfect" as const,
    context: planning,
  };
  const transition = reduceAreaPolishMemo(undefined, event);
  assert.ok(transition.memo?.kind === "contexts");
  assert.equal(transition.memo.settlements[0]?.terminalReason, "perfect");
  assert.deepEqual(reduceAreaPolishState(true, event), { pending: false, propertyValue: "" });
});

test("an improved bounded pass stamps its ceiling on final committed geometry", () => {
  const old = context();
  const final = context({ geometry: "geometry-b" });
  const transition = reduceAreaPolishMemo(memo(old), {
    kind: "polish-completed",
    report: ceilingReport(),
    improved: true,
    context: final,
  });
  assert.deepEqual(transition.memo, memo(final, [record(final, "ceiling")]));
  assert.equal(polishRetrySuppressed(transition.memo, old), false);
  assert.equal(polishRetrySuppressed(transition.memo, final), true);
});

test("A→B→A does not repeat settled automatic work", () => {
  const tracker = new AreaPolishEntryTracker();
  const planning = context();
  const state = memo(planning);
  const evaluate = areaPolishNeedsContextEvaluation(false, state);

  assert.equal(tracker.observe("area-a", evaluate).retry, true);
  assert.equal(tracker.consumeRetry("area-a"), true);
  assert.equal(areaPolishEligibility(state, planning).reason, "settled");
  assert.equal(tracker.observe("area-b", false).retry, false);
  assert.equal(tracker.observe("area-a", evaluate).retry, true);
  assert.equal(areaPolishEligibility(state, planning).eligible, false);
});

test("automatic settlement is area+geometry scoped across entry/chart coverage", () => {
  const entranceA = context({ center: "room:1" });
  const entranceB = context({ center: "room:2", chartX: 2 });
  const state = memo(entranceA);
  assert.notEqual(entranceA.key, entranceB.key);
  assert.equal(areaPolishEligibility(state, entranceB).reason, "settled");
  assert.equal(polishRetrySuppressed(state, entranceB), true);
});

test("generation, effort, policy, and geometry changes re-enable automatic work", () => {
  const planning = context();
  const state = memo(planning);
  assert.equal(areaPolishEligibility(
    state,
    context({ generation: AREA_POLISH_SEARCH_GENERATION + 1 }),
  ).reason, "search-generation-changed");
  assert.equal(areaPolishEligibility(state, context({ effort: 2 })).reason, "effort-increased");
  assert.equal(areaPolishEligibility(state, context({ maxLayouts: 3 })).reason, "policy-changed");
  assert.equal(areaPolishEligibility(state, context({ geometry: "geometry-b" })).reason, "geometry-changed");
});

test("only a stronger zero-defect result settles a different automatic feasible set", () => {
  const automatic = context();
  const perfect = context({ effort: AREA_POLISH_PERFECT_EFFORT, maxLayouts: 8 });
  assert.notEqual(perfect.policyKey, automatic.policyKey);
  const stronger = memo(perfect, [record(perfect, "perfect")]);
  assert.equal(areaPolishEligibility(stronger, automatic).reason, "settled");
  assert.equal(areaPolishEligibility(stronger, automatic).eligible, false);

  for (const terminal of ["fixed-point", "ceiling"] as const) {
    const incompatible = memo(perfect, [record(perfect, terminal)]);
    assert.equal(areaPolishEligibility(incompatible, automatic).eligible, true);
  }

  const equalEffortDifferentPolicy = memo(context({ maxLayouts: 3 }));
  assert.equal(areaPolishEligibility(equalEffortDifferentPolicy, automatic).reason, "policy-changed");
  assert.equal(areaPolishEligibility(equalEffortDifferentPolicy, automatic).eligible, true);
});

test("timeout, cancellation, and error are distinct and area-wide cooled down", () => {
  const entranceA = context({ center: "room:1" });
  const entranceB = context({ center: "room:2" });
  const now = 1_000;
  const cases = [
    [ceilingReport({ cutoff: "time", extensionSearch: { cancelled: true } }), "timeout"],
    [ceilingReport({ extensionSearch: { cancelled: true } }), "cancelled"],
    [ceilingReport({ polishCutoff: "error" }), "error"],
  ] as const;
  for (const [report, reason] of cases) {
    assert.equal(areaPolishTerminalReason(report), reason);
    const state = reduceAreaPolishMemo(undefined, {
      kind: "polish-completed",
      report,
      context: entranceA,
    }, now).memo;
    const cooled = areaPolishEligibility(state, entranceB, now);
    assert.equal(cooled.reason, "cooldown");
    assert.equal(cooled.terminalReason, reason);
    assert.equal(areaPolishEligibility(
      state,
      entranceB,
      now + AREA_POLISH_RETRY_COOLDOWN_MS,
    ).eligible, true);
  }

  const interrupted = reduceAreaPolishMemo(undefined, {
    kind: "polish-interrupted",
    reason: "cancelled",
    context: entranceA,
  }, now).memo;
  assert.equal(areaPolishEligibility(interrupted, entranceB, now).terminalReason, "cancelled");
});

test("same-area movement displacement records no cancellation cooldown", () => {
  const planning = context({ center: "room:1" });
  const current = memo(planning);
  assert.deepEqual(reduceAreaPolishMemo(current, {
    kind: "polish-interrupted",
    reason: "cancelled",
    displacedWithinArea: true,
    context: planning,
  }, 1_000), {
    memo: current,
    propertyValue: undefined,
  });
});

test("retryable outcomes exponentially extend one area-wide cooldown", () => {
  const firstEntrance = context({ center: "room:1" });
  const secondEntrance = context({ center: "room:2" });
  const first = reduceAreaPolishMemo(undefined, {
    kind: "polish-completed",
    report: ceilingReport({ cutoff: "time" }),
    context: firstEntrance,
  }, 0).memo;
  const second = reduceAreaPolishMemo(first, {
    kind: "polish-completed",
    report: ceilingReport({ cutoff: "time" }),
    context: secondEntrance,
  }, AREA_POLISH_RETRY_COOLDOWN_MS).memo;
  assert.ok(second?.kind === "contexts");
  assert.equal(second.settlements.length, 1);
  assert.equal(second.settlements[0]?.attempts, 2);
  assert.equal(second.settlements[0]?.retryAfterMs, AREA_POLISH_RETRY_COOLDOWN_MS * 3);
});

/** A repair report that returned the ordinary plan without searching. */
function unsearchedReport(outcome: string, cutoff = "none"): AreaPolishReport {
  return {
    outcome,
    geometricFixedPoint: false,
    cutoff,
    polishCutoff: "none",
    extensionSearch: { cancelled: cutoff === "time", exhausted: cutoff === "extensions" },
    crossingRepair: { cancelled: false, exhausted: false },
  };
}

test("only a repair that cannot search until the map changes qualifies as unsearchable", () => {
  for (const outcome of ["clean", "no-regression", "no-constraints", "search-failed:analysis", "no-layout"]) {
    assert.equal(reportsUnsearchableGeometry(unsearchedReport(outcome)), true, outcome);
  }
  for (const report of [
    undefined,
    unsearchedReport("search-failed:time", "time"),
    unsearchedReport("no-layout", "time"),
    unsearchedReport("no-layout", "extensions"),
    unsearchedReport("search-failed:work", "restarts"),
    unsearchedReport("no-budget"),
    unsearchedReport("locked"),
    { ...ceilingReport(), outcome: "searched" },
  ]) {
    assert.equal(reportsUnsearchableGeometry(report), false, `${report?.outcome} ${report?.cutoff}`);
  }
});

test("a repair that could not search settles its geometry until the map changes", () => {
  const planning = context();
  for (const outcome of ["search-failed:analysis", "no-constraints", "no-layout"]) {
    const report = unsearchedReport(outcome);
    assert.equal(areaPolishTerminalReason(report), "fixed-point", outcome);
    const { memo: state } = reduceAreaPolishMemo(undefined, {
      kind: "polish-completed",
      report,
      context: planning,
    });
    assert.deepEqual(state, memo(planning, [record(planning, "fixed-point")]), outcome);
    assert.equal(areaPolishEligibility(state, planning).reason, "settled", outcome);
    assert.equal(
      areaPolishEligibility(state, context({ geometry: "geometry-b" })).reason,
      "geometry-changed",
      "moved rooms fingerprint a new geometry",
    );
  }
  // A deadline cut proves nothing about the geometry, so a later, longer run
  // may search; a work ceiling that stopped compaction follows the ceiling
  // rule, and an empty budget is retried after a cooldown.
  assert.equal(areaPolishTerminalReason(unsearchedReport("search-failed:time", "time")), "timeout");
  assert.equal(areaPolishTerminalReason(unsearchedReport("no-layout", "time")), "timeout");
  assert.equal(areaPolishTerminalReason(unsearchedReport("no-layout", "extensions")), "ceiling");
  assert.equal(areaPolishTerminalReason(unsearchedReport("no-budget")), "incomplete");
});

test("a clean ordinary plan is perfect, even after the pass changed the map", () => {
  assert.equal(areaPolishTerminalReason(unsearchedReport("clean")), "perfect");
  const old = context();
  const final = context({ geometry: "geometry-clean" });
  const transition = reduceAreaPolishMemo(memo(old), {
    kind: "polish-completed",
    report: unsearchedReport("clean"),
    improved: true,
    context: final,
  });
  assert.deepEqual(transition.memo, memo(final, [record(final, "perfect")]));
  assert.equal(polishRetrySuppressed(transition.memo, final), true);
  assert.deepEqual(reduceAreaPolishState(true, {
    kind: "polish-completed",
    report: unsearchedReport("clean"),
    context: final,
  }), { pending: false, propertyValue: "" });
});

test("the decision log names why a repair did not search, and a clean map is no failure", () => {
  assert.equal(
    polishNotSearchedReason(unsearchedReport("search-failed:analysis")),
    "search-failed:analysis",
  );
  assert.equal(
    polishNotSearchedReason(unsearchedReport("search-failed:time", "time")),
    "search-failed:time",
  );
  assert.equal(polishNotSearchedReason(unsearchedReport("no-constraints")), "no-constraints");
  assert.equal(polishNotSearchedReason(unsearchedReport("clean")), undefined);
  assert.equal(polishNotSearchedReason({ ...ceilingReport(), outcome: "searched" }), undefined);
  assert.equal(polishNotSearchedReason(ceilingReport()), undefined, "a report without an outcome");
  assert.equal(polishNotSearchedReason(undefined), undefined);
});

test("the first settlement replaces a legacy memo", () => {
  const planning = context();
  const legacy = areaPolishMemo("[legacy-geometry]");
  assert.deepEqual(reduceAreaPolishMemo(legacy, { kind: "polish-started" }), {
    memo: legacy,
    propertyValue: undefined,
  });
  const upgraded = reduceAreaPolishMemo(legacy, {
    kind: "polish-completed",
    report: { geometricFixedPoint: true },
    context: planning,
  });
  assert.deepEqual(upgraded.memo, memo(planning));
  assert.equal(upgraded.propertyValue, areaPolishMemoPropertyValue(memo(planning)));
});

test("fresh topology clears settlement and makes the legacy hint pending", () => {
  const planning = context();
  assert.deepEqual(reduceAreaPolishMemo(memo(planning), { kind: "topology-deferred" }), {
    memo: undefined,
    propertyValue: "",
  });
  assert.deepEqual(reduceAreaPolishState(false, { kind: "topology-deferred" }), {
    pending: true,
    propertyValue: "true",
  });
});

test("settlement history remains bounded", () => {
  const planning = context();
  let state: AreaPolishMemo | undefined;
  for (let effort = 1; effort <= MAX_AREA_POLISH_MEMO_CONTEXTS + 2; effort += 1) {
    state = reduceAreaPolishMemo(state, {
      kind: "polish-completed",
      report: { geometricFixedPoint: true },
      context: context({ effort }),
    }).memo;
  }
  assert.ok(state?.kind === "contexts");
  assert.equal(state.settlements.length, MAX_AREA_POLISH_MEMO_CONTEXTS);
  assert.equal(state.settlements[0]?.automaticEffort, 3);
  assert.equal(polishRetrySuppressed(state, planning), false);
});

test("only deterministic ceilings qualify for settlement", () => {
  assert.equal(reportsCeilingExhaustion(ceilingReport()), true);
  assert.equal(reportsCeilingExhaustion(ceilingReport({ cutoff: "time" })), false);
  assert.equal(reportsCeilingExhaustion(ceilingReport({ polishCutoff: "error" })), false);
  assert.equal(reportsCeilingExhaustion(ceilingReport({ extensionSearch: { cancelled: true } })), false);
});

test("the seam list is room numbers in a package-owned property", () => {
  assert.equal(AREA_POLISH_SEAMS_PROPERTY, "nukefire.layout.polish-seams");
  assert.equal(MAX_AREA_POLISH_SEAMS, 512);
  assert.deepEqual(areaPolishSeams(undefined), []);
  assert.deepEqual(areaPolishSeams(""), []);
  assert.deepEqual(areaPolishSeams("  "), []);
  assert.deepEqual(areaPolishSeams("[4,1,9]"), [4, 1, 9]);
  assert.equal(areaPolishSeamsPropertyValue([4, 1, 9]), "[4,1,9]");
  assert.equal(areaPolishSeamsPropertyValue([]), "");
});

test("hand-edited seam lists are read defensively", () => {
  assert.deepEqual(areaPolishSeams("not json"), []);
  assert.deepEqual(areaPolishSeams("{\"rooms\":[1]}"), []);
  assert.deepEqual(areaPolishSeams("7"), []);
  // Entries that are not room numbers go; a repeated room counts where it last appears.
  assert.deepEqual(areaPolishSeams(' [5, 2, 5, "x", 1.5, null, 9] '), [2, 5, 9]);
});

test("a merge's seams join the list newest last, and deferred growth leaves it alone", () => {
  const first = reduceAreaPolishSeams([], { kind: "topology-deferred", seams: [3, 8] });
  assert.deepEqual(first, { seams: [3, 8], propertyValue: "[3,8]" });
  const second = reduceAreaPolishSeams(first.seams, { kind: "topology-deferred", seams: [3, 11] });
  assert.deepEqual(second, { seams: [8, 3, 11], propertyValue: "[8,3,11]" });
  for (const event of [
    { kind: "topology-deferred" },
    { kind: "topology-deferred", seams: [] },
    { kind: "polish-started" },
  ] satisfies AreaPolishEvent[]) {
    assert.deepEqual(reduceAreaPolishSeams(second.seams, event), {
      seams: second.seams,
      propertyValue: undefined,
    });
  }
  // A merge's polish request marks the map pending and clears its memo, as growth does.
  const planning = context();
  const request: AreaPolishEvent = { kind: "topology-deferred", seams: [3, 8] };
  assert.deepEqual(reduceAreaPolishState(false, request), { pending: true, propertyValue: "true" });
  assert.deepEqual(reduceAreaPolishMemo(memo(planning), request), {
    memo: undefined,
    propertyValue: "",
  });
});

test("the seam list keeps its newest rooms at its bound", () => {
  const many = Array.from({ length: MAX_AREA_POLISH_SEAMS + 88 }, (_, index) => index + 1);
  const { seams } = reduceAreaPolishSeams([], { kind: "topology-deferred", seams: many });
  assert.equal(seams.length, MAX_AREA_POLISH_SEAMS);
  assert.equal(seams[0], 89);
  assert.equal(seams.at(-1), MAX_AREA_POLISH_SEAMS + 88);
  assert.deepEqual(areaPolishSeams(JSON.stringify(many)), seams);
});

test("a completed whole-map polish clears the seams, whatever it proved", () => {
  const planning = context();
  for (const event of [
    { kind: "polish-completed" },
    { kind: "polish-completed", improved: true },
    { kind: "polish-completed", report: { geometricFixedPoint: true }, context: planning },
    { kind: "polish-completed", report: ceilingReport(), context: planning },
  ] satisfies AreaPolishEvent[]) {
    assert.deepEqual(reduceAreaPolishSeams([3, 8], event), { seams: [], propertyValue: "" });
    assert.deepEqual(reduceAreaPolishSeams([], event), { seams: [], propertyValue: undefined });
  }
});

test("entry tracking retries once per area visit", () => {
  const tracker = new AreaPolishEntryTracker();

  assert.equal(tracker.currentAreaKey, undefined);
  assert.deepEqual(tracker.observe("area-a", true), {
    entered: true,
    retry: true,
    previousAreaKey: undefined,
  });
  assert.equal(tracker.currentAreaKey, "area-a");
  assert.equal(tracker.retryAreaKey, "area-a");
  assert.equal(tracker.consumeRetry("area-a"), true);
  assert.equal(tracker.retryAreaKey, undefined);
  assert.equal(tracker.consumeRetry("area-a"), false);
  assert.deepEqual(tracker.observe("area-a", true), {
    entered: false,
    retry: false,
    previousAreaKey: undefined,
  });
  assert.deepEqual(tracker.observe("area-a", false), {
    entered: false,
    retry: false,
    previousAreaKey: undefined,
  });
  tracker.markPending("area-a");
  assert.equal(tracker.retryAreaKey, "area-a");
  tracker.markPending("area-b");
  assert.equal(tracker.retryAreaKey, "area-a");
  assert.deepEqual(tracker.observe("area-b", false), {
    entered: true,
    retry: false,
    previousAreaKey: "area-a",
  });
  assert.equal(tracker.retryAreaKey, undefined);
  assert.deepEqual(tracker.observe("area-a", true), {
    entered: true,
    retry: true,
    previousAreaKey: "area-b",
  });
});

test("snapshot payload equivalence is identity or byte-equal clones", () => {
  const snapshot = { center: 5, rooms: [{ vnum: 5, x: 0 }] };
  assert.equal(equivalentSnapshotPayloads(snapshot, snapshot), true);
  assert.equal(
    equivalentSnapshotPayloads(snapshot, JSON.parse(JSON.stringify(snapshot))),
    true,
  );
  assert.equal(
    equivalentSnapshotPayloads(snapshot, { center: 6, rooms: [{ vnum: 5, x: 0 }] }),
    false,
  );
  assert.equal(
    equivalentSnapshotPayloads(snapshot, { center: 5, rooms: [{ vnum: 5, x: 1 }] }),
    false,
  );
});

test("claims settle to a restoration only for an equivalent displacement", () => {
  const claims = new QuietPolishClaims<{ center: number }>();
  const planned = { center: 5 };

  claims.record(planned, "area-1", { retryConsumed: true, deferredRemoved: false });
  claims.record(planned, "area-1", { retryConsumed: false, deferredRemoved: true });
  assert.deepEqual([...claims.settle(planned, { center: 5 })], [
    ["area-1", { retryConsumed: true, deferredRemoved: true }],
  ]);
  // Settling clears the claim even when it was restored.
  assert.equal(claims.settle(planned, { center: 5 }).size, 0);

  claims.record(planned, "area-1", { retryConsumed: true, deferredRemoved: true });
  assert.equal(claims.settle(planned, { center: 6 }).size, 0);
  // A genuine displacement forfeits: the cleared claim cannot restore later.
  assert.equal(claims.settle(planned, { center: 5 }).size, 0);
});

test("nothing-consumed passes and completed polishes leave nothing to restore", () => {
  const claims = new QuietPolishClaims<{ center: number }>();
  const planned = { center: 5 };

  claims.record(planned, "area-1", { retryConsumed: false, deferredRemoved: false });
  assert.equal(claims.settle(planned, { center: 5 }).size, 0);

  claims.record(planned, "area-1", { retryConsumed: true, deferredRemoved: true });
  claims.discharge(planned, "area-1");
  assert.equal(claims.settle(planned, { center: 5 }).size, 0);
});

test("committed progress marks the claim and survives a merged re-record", () => {
  const claims = new QuietPolishClaims<{ center: number }>();
  const planned = { center: 5 };

  // Marking without a recorded claim is inert: nothing exists to restore.
  claims.markProgress(planned, "area-1");
  assert.equal(claims.settle(planned, { center: 5 }).size, 0);

  claims.record(planned, "area-1", { retryConsumed: true, deferredRemoved: false });
  claims.markProgress(planned, "area-1");
  claims.record(planned, "area-1", { retryConsumed: false, deferredRemoved: true });
  assert.deepEqual([...claims.settle(planned, { center: 5 })], [
    ["area-1", { retryConsumed: true, deferredRemoved: true, progressed: true }],
  ]);
});

test("the resume budget allows a bounded run of fruitless resumptions", () => {
  const budget = new QuietResumeBudget();
  for (let resume = 1; resume <= MAX_FRUITLESS_QUIET_RESUMES; resume += 1) {
    assert.equal(budget.allowResume("area-1", false), true, `resumption ${resume}`);
  }
  assert.equal(budget.allowResume("area-1", false), false);
  // Areas are budgeted independently.
  assert.equal(budget.allowResume("area-2", false), true);
});

test("progress, reset, and clear each restart the fruitless allowance", () => {
  const budget = new QuietResumeBudget(2);
  assert.equal(budget.allowResume("area-1", false), true);
  assert.equal(budget.allowResume("area-1", false), true);
  assert.equal(budget.allowResume("area-1", false), false);

  // A fruitful displacement is always resumable and restarts the count.
  assert.equal(budget.allowResume("area-1", true), true);
  assert.equal(budget.allowResume("area-1", false), true);

  budget.reset("area-1");
  assert.equal(budget.allowResume("area-1", false), true);

  assert.equal(budget.allowResume("area-1", false), true);
  assert.equal(budget.allowResume("area-1", false), false);
  budget.clear();
  assert.equal(budget.allowResume("area-1", false), true);
});

test("disabled polishing observes entry without scheduling work", () => {
  const tracker = new AreaPolishEntryTracker();

  assert.deepEqual(tracker.observe("area-a", true, false), {
    entered: true,
    retry: false,
    previousAreaKey: undefined,
  });
  assert.deepEqual(tracker.observe("area-a", true), {
    entered: false,
    retry: false,
    previousAreaKey: undefined,
  });
  tracker.clear();
  assert.equal(tracker.currentAreaKey, undefined);
  assert.equal(tracker.retryAreaKey, undefined);
  assert.deepEqual(tracker.observe("area-a", true), {
    entered: true,
    retry: true,
    previousAreaKey: undefined,
  });
});
