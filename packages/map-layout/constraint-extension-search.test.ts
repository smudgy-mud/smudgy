import assert from "node:assert/strict";
import test from "node:test";
import {
  searchConstraintExtensions,
  type ConstraintExtensionArc,
  type ConstraintExtensionInspection,
} from "./constraint-extension-search.ts";

const arc = (
  from: number,
  to: number,
  axis: 0 | 1 | 2 = 0,
): ConstraintExtensionArc => ({ axis, from, to });

const hasArc = (
  arcs: readonly ConstraintExtensionArc[],
  from: number,
  to: number,
): boolean => arcs.some((candidate) => (
  candidate.axis === 0 && candidate.from === from && candidate.to === to
));

test("backtracks from a deterministic greedy dead end", () => {
  const arrivals: string[] = [];
  const inspectedCounters: unknown[] = [];
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [2, 1, 1],
    inspect: ({ extensionArcs, state, depth, branches, cyclePrunes, maxDepth }) => {
      inspectedCounters.push({ state, depth, branches, cyclePrunes, maxDepth });
      if (extensionArcs.length === 0) {
        return {
          type: "hard-conflict",
          conflict: {
            kind: "collision",
            alternatives: [
              { arcs: [arc(0, 1)] },
              { arcs: [arc(1, 0)] },
            ],
          },
        };
      }
      if (hasArc(extensionArcs, 0, 1)) {
        return {
          type: "hard-conflict",
          conflict: {
            kind: "second-collision",
            alternatives: [{ arcs: [arc(1, 0)] }],
          },
        };
      }
      return { type: "candidate", candidate: "right branch", score: 1 };
    },
    compareScores: (left, right) => left - right,
    onIncumbent: ({ candidate }) => arrivals.push(candidate),
  });

  assert.equal(result.completed, true);
  assert.equal(result.hardInfeasible, false);
  assert.equal(result.best, "right branch");
  assert.equal(result.states, 3);
  assert.equal(result.branches, 3);
  assert.equal(result.cyclePrunes, 1);
  assert.deepEqual(arrivals, ["right branch"]);
  assert.deepEqual(inspectedCounters, [
    { state: 1, depth: 0, branches: 0, cyclePrunes: 0, maxDepth: 0 },
    { state: 2, depth: 1, branches: 1, cyclePrunes: 0, maxDepth: 1 },
    { state: 3, depth: 1, branches: 3, cyclePrunes: 1, maxDepth: 1 },
  ]);
});

test("finds a complete extension two conflicts deep", () => {
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [3, 1, 1],
    inspect: ({ extensionArcs }) => {
      if (!hasArc(extensionArcs, 0, 1)) {
        return {
          type: "hard-conflict",
          conflict: {
            kind: "first",
            alternatives: [{ arcs: [arc(0, 1)] }],
          },
        };
      }
      if (!hasArc(extensionArcs, 1, 2)) {
        return {
          type: "hard-conflict",
          conflict: {
            kind: "second",
            alternatives: [{ arcs: [arc(1, 2)] }],
          },
        };
      }
      return { type: "candidate", candidate: "complete", score: 2 };
    },
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.best, "complete");
  assert.equal(result.states, 3);
  assert.equal(result.maxDepth, 2);
  assert.equal(result.completed, true);
});

test("leaves caller-owned fixed coordinates unchanged", () => {
  type TinyLayout = ReadonlyMap<string, number>;
  const result = searchConstraintExtensions<TinyLayout, number>({
    axisNodeCounts: [2, 1, 1],
    inspect: ({ extensionArcs }): ConstraintExtensionInspection<TinyLayout, number> => {
      if (extensionArcs.length === 0) {
        return {
          type: "hard-conflict",
          conflict: {
            kind: "fixed-room-collision",
            alternatives: [
              { arcs: [arc(1, 0)] },
              { arcs: [arc(0, 1)] },
            ],
          },
        };
      }
      const movable = hasArc(extensionArcs, 1, 0) ? -1 : 1;
      return {
        type: "candidate",
        candidate: new Map([["fixed", 0], ["movable", movable]]),
        score: movable,
      };
    },
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.best?.get("fixed"), 0);
  assert.equal(result.best?.get("movable"), 1);
  assert.equal(result.rawIncumbents, 2);
});

test("cancels after a charged inspection without starting a branch", () => {
  let cancelled = false;
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [2, 1, 1],
    inspect: () => {
      cancelled = true;
      return {
        type: "hard-conflict",
        conflict: {
          kind: "collision",
          alternatives: [{ arcs: [arc(0, 1)] }],
        },
      };
    },
    compareScores: (left, right) => left - right,
    shouldCancel: () => cancelled,
  });

  assert.equal(result.cancelled, true);
  assert.equal(result.completed, false);
  assert.equal(result.exhausted, false);
  assert.equal(result.states, 1);
  assert.equal(result.branches, 0);
});

test("a cancellation inspected at the root frame is neither conflict nor completion", () => {
  // No shouldCancel is supplied: the inspection outcome alone must terminate
  // the traversal, without relying on a latched caller predicate.
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [2, 1, 1],
    inspect: () => ({ type: "cancelled" }),
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.cancelled, true);
  assert.equal(result.completed, false);
  assert.equal(result.exhausted, false);
  assert.equal(result.hardInfeasible, false);
  assert.equal(result.hardConflicts, 0);
  assert.equal(result.states, 1);
  assert.equal(result.best, undefined);
});

test("a cancellation inspected on the final remaining state cannot complete the search", () => {
  let inspected = 0;
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [2, 1, 1],
    inspect: ({ extensionArcs }) => {
      inspected += 1;
      if (extensionArcs.length === 0) {
        return {
          type: "candidate",
          candidate: "root",
          score: 0,
          softDefect: {
            kind: "crossing",
            alternatives: [
              { arcs: [arc(0, 1)] },
              { arcs: [arc(1, 0)] },
            ],
          },
        };
      }
      if (hasArc(extensionArcs, 0, 1)) {
        return { type: "candidate", candidate: "first", score: 1 };
      }
      // The deterministic traversal holds no further state after this one, so
      // a fabricated conflict here would have drained the stack as completed.
      return { type: "cancelled" };
    },
    compareScores: (left, right) => left - right,
  });

  assert.equal(inspected, 3);
  assert.equal(result.cancelled, true);
  assert.equal(result.completed, false);
  assert.equal(result.exhausted, false);
  assert.equal(result.hardConflicts, 0, "cancellation never counts as a hard conflict");
  assert.equal(result.candidateStates, 2);
  assert.equal(result.best, "first");
  assert.equal(result.bestScore, 1);
});

function budgetFixture(maxExtensionStates: number) {
  return searchConstraintExtensions<string, number>({
    axisNodeCounts: [2, 1, 1],
    maxExtensionStates,
    inspect: ({ extensionArcs }) => {
      if (extensionArcs.length === 0) {
        return {
          type: "candidate" as const,
          candidate: "root",
          score: 0,
          softDefect: {
            kind: "crossing",
            alternatives: [
              { arcs: [arc(0, 1)] },
              { arcs: [arc(1, 0)] },
            ],
          },
        };
      }
      const left = hasArc(extensionArcs, 0, 1);
      return {
        type: "candidate" as const,
        candidate: left ? "first" : "second",
        score: left ? 1 : 2,
      };
    },
    compareScores: (left, right) => left - right,
  });
}

test("larger and infinite state budgets improve monotonically", () => {
  const one = budgetFixture(1);
  const two = budgetFixture(2);
  const three = budgetFixture(3);
  const unlimited = budgetFixture(Number.POSITIVE_INFINITY);

  assert.deepEqual(
    [one.bestScore, two.bestScore, three.bestScore, unlimited.bestScore],
    [0, 1, 2, 2],
  );
  assert.equal(one.exhausted, true);
  assert.equal(two.exhausted, true);
  assert.equal(three.completed, true);
  assert.equal(unlimited.completed, true);
  assert.equal(unlimited.states, 3);
});

test("a soft defect whose alternatives cycle preserves the incumbent", () => {
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [2, 1, 1],
    baseArcs: [arc(0, 1)],
    inspect: () => ({
      type: "candidate",
      candidate: "hard-valid",
      score: 7,
      softDefect: {
        kind: "obstruction",
        alternatives: [{ arcs: [arc(1, 0)] }],
      },
    }),
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.best, "hard-valid");
  assert.equal(result.completed, true);
  assert.equal(result.hardInfeasible, false);
  assert.equal(result.cyclePrunes, 1);
  assert.equal(result.softIncumbents, 1);
});

test("emits strict incumbents in stable alternative order", () => {
  const arrivals: number[] = [];
  const result = searchConstraintExtensions<number, number>({
    axisNodeCounts: [3, 1, 1],
    inspect: ({ extensionArcs }) => {
      if (extensionArcs.length === 0) {
        return {
          type: "candidate" as const,
          candidate: 0,
          score: 0,
          softDefect: {
            kind: "crossing",
            alternatives: [
              { arcs: [arc(0, 1)] },
              { arcs: [arc(1, 0)] },
              { arcs: [arc(0, 2)] },
            ],
          },
        };
      }
      const score = hasArc(extensionArcs, 0, 1) ? 2
        : hasArc(extensionArcs, 1, 0) ? 2
        : 1;
      return { type: "candidate" as const, candidate: score, score };
    },
    compareScores: (left, right) => left - right,
    onIncumbent: ({ score }) => arrivals.push(score),
  });

  assert.deepEqual(arrivals, [0, 2]);
  assert.equal(result.rawIncumbents, 2);
  assert.equal(result.candidateStates, 4);
});

test("proves hard infeasibility without inventing a soft explanation", () => {
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [2, 1, 1],
    inspect: ({ extensionArcs }) => {
      if (extensionArcs.length === 0) {
        return {
          type: "hard-conflict" as const,
          conflict: {
            kind: "collision",
            alternatives: [{ arcs: [arc(0, 1)] }],
          },
        };
      }
      return {
        type: "hard-conflict" as const,
        conflict: {
          kind: "collision",
          alternatives: [{ arcs: [arc(1, 0)] }],
        },
      };
    },
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.completed, true);
  assert.equal(result.hardInfeasible, true);
  assert.equal(result.best, undefined);
  assert.equal(result.hardExplanation, undefined);
});

test("returns only a caller-supplied sound root explanation", () => {
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [1, 1, 1],
    inspect: () => ({
      type: "hard-conflict",
      conflict: {
        kind: "fixed-collision",
        alternatives: [],
        explanation: { scope: "base-relations", relationGroups: [2, 5] },
      },
    }),
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.hardInfeasible, true);
  assert.deepEqual(result.hardExplanation, {
    scope: "base-relations",
    relationGroups: [2, 5],
  });
});

test("uses sparse reachability for large axes and prunes a closing cycle", () => {
  const baseArcs = Array.from({ length: 1_099 }, (_, from) => arc(from, from + 1));
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [1_100, 1, 1],
    baseArcs,
    inspect: () => ({
      type: "hard-conflict",
      conflict: {
        kind: "collision",
        alternatives: [{ arcs: [arc(1_099, 0)] }],
      },
    }),
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.states, 1);
  assert.equal(result.cyclePrunes, 1);
  assert.equal(result.hardInfeasible, true);
});

test("a live-node ceiling does not change a search which fits inside it", () => {
  const run = (maxLiveSearchNodes?: number) =>
    searchConstraintExtensions<string, number>({
      axisNodeCounts: [4, 1, 1],
      ...(maxLiveSearchNodes === undefined ? {} : { maxLiveSearchNodes }),
      inspect: ({ extensionArcs, depth }) => {
        if (depth < 2) {
          return {
            type: "candidate" as const,
            candidate: `depth-${depth}`,
            score: depth,
            softDefect: {
              kind: "crossing",
              alternatives: [
                { arcs: [arc(depth, depth + 1)] },
                { arcs: [arc(depth + 1, depth)] },
              ],
            },
          };
        }
        return {
          type: "candidate" as const,
          candidate: `leaf-${extensionArcs.map(({ from, to }) => `${from}-${to}`).join("/")}`,
          score: 2,
        };
      },
      compareScores: (left, right) => left - right,
    });

  const unlimited = run();
  const bounded = run(16);
  assert.equal(unlimited.completed, true);
  assert.equal(bounded.completed, true);
  assert.equal(bounded.exhausted, false);
  assert.equal(bounded.frontierPrunes, 0);
  assert.deepEqual(
    {
      states: bounded.states,
      branches: bounded.branches,
      cyclePrunes: bounded.cyclePrunes,
      candidateStates: bounded.candidateStates,
      best: bounded.best,
      bestScore: bounded.bestScore,
    },
    {
      states: unlimited.states,
      branches: unlimited.branches,
      cyclePrunes: unlimited.cyclePrunes,
      candidateStates: unlimited.candidateStates,
      best: unlimited.best,
      bestScore: unlimited.bestScore,
    },
  );
});

interface LiveFrontierStressSummary {
  readonly states: number;
  readonly branches: number;
  readonly best: number | undefined;
  readonly bestScore: number | undefined;
  readonly cancelled: boolean;
  readonly exhausted: boolean;
  readonly frontierPrunes: number;
  readonly peakFrontierNodes: number;
  readonly peakLiveSearchNodes: number;
}

function liveFrontierStress(
  maxExtensionStates: number,
  cancelAfterInspections = Number.POSITIVE_INFINITY,
): LiveFrontierStressSummary {
  let inspections = 0;
  const result = searchConstraintExtensions<number, number>({
    // Sparse reachability with 300 independent targets lets a chosen path add
    // 100,000 unique arcs without making the regression itself memory-heavy.
    axisNodeCounts: [1_300, 1, 1],
    maxExtensionStates,
    maxLiveSearchNodes: 128,
    shouldCancel: () => inspections >= cancelAfterInspections,
    inspect: ({ depth }) => {
      inspections += 1;
      const source = depth % 1_000;
      const targetBand = Math.floor(depth / 1_000) * 3;
      return {
        type: "candidate" as const,
        candidate: depth,
        score: depth,
        softDefect: {
          kind: "three-way-stress",
          alternatives: [0, 1, 2].map((choice) => ({
            arcs: [arc(source, 1_000 + targetBand + choice)],
          })),
        },
      };
    },
    compareScores: (left, right) => left - right,
  });
  return {
    states: result.states,
    branches: result.branches,
    best: result.best,
    bestScore: result.bestScore,
    cancelled: result.cancelled,
    exhausted: result.exhausted,
    frontierPrunes: result.frontierPrunes,
    peakFrontierNodes: result.peakFrontierNodes,
    peakLiveSearchNodes: result.peakLiveSearchNodes,
  };
}

test("a three-way 100k-state search has deterministic bounded live memory and cancellation", () => {
  const first = liveFrontierStress(100_000);
  const second = liveFrontierStress(100_000);
  assert.deepEqual(second, first);
  assert.equal(first.states, 100_000);
  assert.equal(first.cancelled, false);
  assert.equal(first.exhausted, true);
  assert.ok(first.frontierPrunes > 0);
  assert.ok(first.peakFrontierNodes <= 128);
  assert.ok(first.peakLiveSearchNodes <= 128);

  const cancelled = liveFrontierStress(100_000, 4_096);
  assert.equal(cancelled.cancelled, true);
  assert.equal(cancelled.states, 4_096);
  assert.ok(cancelled.peakFrontierNodes <= 128);
  assert.ok(cancelled.peakLiveSearchNodes <= 128);
});

test("the production default bounds a wide frontier independently of total work", () => {
  const result = searchConstraintExtensions<number, number>({
    axisNodeCounts: [2, 1, 1],
    maxExtensionStates: 100_000,
    inspect: ({ depth }) => depth === 0
      ? {
        type: "hard-conflict" as const,
        conflict: {
          kind: "wide-frontier",
          alternatives: Array.from({ length: 40_000 }, () => ({
            arcs: [arc(0, 1)],
          })),
        },
      }
      : { type: "candidate" as const, candidate: depth, score: depth },
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.exhausted, true);
  assert.equal(result.completed, false);
  assert.equal(result.states, 32_769);
  assert.equal(result.frontierPrunes, 40_000 - 32_768);
  assert.equal(result.peakFrontierNodes, 32_768);
  assert.equal(result.peakLiveSearchNodes, 32_768);
});

test("lazy candidates materialize only when they become the strict incumbent", () => {
  let materializations = 0;
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [3, 1, 1],
    inspect: ({ extensionArcs }) => ({
      type: "candidate",
      materializeCandidate: () => {
        materializations += 1;
        return extensionArcs.length === 0 ? "root" : `child-${extensionArcs.length}`;
      },
      score: 10 - extensionArcs.length,
      softDefect: extensionArcs.length === 0
        ? {
          kind: "lower-scoring-children",
          alternatives: [{ arcs: [arc(0, 1)] }, { arcs: [arc(0, 2)] }],
        }
        : undefined,
    }),
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.states, 3);
  assert.equal(result.best, "root");
  assert.equal(materializations, 1);
});

test("an explicitly provided undefined candidate remains a valid eager value", () => {
  const result = searchConstraintExtensions<undefined, number>({
    axisNodeCounts: [1, 1, 1],
    inspect: () => ({ type: "candidate", candidate: undefined, score: 1 }),
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.completed, true);
  assert.equal(result.candidateStates, 1);
  assert.equal(result.best, undefined);
});

test("extension fingerprints are canonical across branch order and rollback", () => {
  const firstArc = arc(0, 1);
  const secondArc = arc(1, 2);
  const completeFingerprints: string[] = [];
  const result = searchConstraintExtensions<string, number>({
    axisNodeCounts: [3, 1, 1],
    inspect: ({
      extensionArcs,
      extensionFingerprintFirst,
      extensionFingerprintSecond,
      extensionFingerprintCount,
    }) => {
      if (extensionArcs.length === 0) {
        return {
          type: "hard-conflict" as const,
          conflict: {
            kind: "choose-order",
            alternatives: [{ arcs: [firstArc] }, { arcs: [secondArc] }],
          },
        };
      }
      if (extensionArcs.length === 1) {
        const missing = hasArc(extensionArcs, 0, 1) ? secondArc : firstArc;
        return {
          type: "hard-conflict" as const,
          conflict: { kind: "complete-set", alternatives: [{ arcs: [missing] }] },
        };
      }
      completeFingerprints.push(
        `${extensionFingerprintCount}:${extensionFingerprintFirst}:${extensionFingerprintSecond}`,
      );
      return { type: "candidate" as const, candidate: "complete", score: 1 };
    },
    compareScores: (left, right) => left - right,
  });

  assert.equal(result.completed, true);
  assert.equal(completeFingerprints.length, 2);
  assert.equal(completeFingerprints[0], completeFingerprints[1]);
  assert.match(completeFingerprints[0], /^2:/);
});
