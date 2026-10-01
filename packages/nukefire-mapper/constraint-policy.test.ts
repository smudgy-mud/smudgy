import assert from "node:assert/strict";
import test from "node:test";
import { nukeFireConstraintRepairPolicy } from "./constraint-policy.ts";

/** The eight deterministic budgets in a fixed order for tuple comparisons. */
function budgets(scale: { residentCount: number; edgeCount: number }): number[] {
  const policy = nukeFireConstraintRepairPolicy(true, scale);
  assert.equal(policy.when, "always");
  assert.equal(policy.maxDurationMs, Number.POSITIVE_INFINITY);
  return [
    policy.maxRestarts as number,
    policy.maxLayouts as number,
    policy.maxPolishTournaments as number,
    policy.maxPolishPasses as number,
    policy.maxExtensionStates as number,
    policy.maxLiveSearchNodes as number,
    policy.maxMaskDiversifications as number,
    policy.maxCrossingWork as number,
  ];
}

test("small perfect repairs retain a millions-state ceiling without a wall deadline", () => {
  assert.deepEqual(nukeFireConstraintRepairPolicy(true), {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: 100_000,
    maxLayouts: 8,
    maxPolishTournaments: 16,
    maxPolishPasses: 16,
    maxExtensionStates: 4_194_304,
    maxLiveSearchNodes: 32_768,
    maxMaskDiversifications: 512,
    maxCrossingWork: 4_096,
  });
  // The ceilings hold across every map below the smooth formula's knee.
  assert.deepEqual(
    budgets({ residentCount: 16, edgeCount: 48 }),
    [100_000, 8, 16, 16, 4_194_304, 32_768, 512, 4_096],
  );
});

test("the largest documented shape reaches every floor and stays there", () => {
  const floors = [32_768, 2, 2, 3, 32_768, 4_096, 64, 512];
  // 256 residents with three directed edges each is the documented largest
  // shape: pressure 256 × (256 + 768) = 2^18 lands each budget on its floor.
  assert.deepEqual(budgets({ residentCount: 256, edgeCount: 768 }), floors);
  assert.deepEqual(budgets({ residentCount: 1_000, edgeCount: 4_000 }), floors);
  assert.deepEqual(budgets({ residentCount: 100_000, edgeCount: 400_000 }), floors);
});

test("interior budgets follow the reciprocal-pressure formula exactly", () => {
  // pressure = residents × (residents + edges); budget = clamp(F·2^18/pressure).
  assert.deepEqual(
    budgets({ residentCount: 64, edgeCount: 192 }),
    [100_000, 8, 16, 16, 524_288, 32_768, 512, 4_096],
  );
  assert.deepEqual(
    budgets({ residentCount: 128, edgeCount: 384 }),
    [100_000, 8, 8, 12, 131_072, 16_384, 256, 2_048],
  );
  assert.deepEqual(
    budgets({ residentCount: 192, edgeCount: 576 }),
    [58_254, 3, 3, 5, 58_254, 7_281, 113, 910],
  );
});

test("budgets shrink monotonically in residents and in edges", () => {
  const nonIncreasing = (previous: number[], next: number[]): void => {
    for (let index = 0; index < previous.length; index += 1) {
      assert.ok(
        next[index] <= previous[index],
        `budget ${index} rose from ${previous[index]} to ${next[index]}`,
      );
    }
  };

  let previous = budgets({ residentCount: 1, edgeCount: 3 });
  for (const residentCount of [2, 8, 32, 63, 64, 65, 127, 128, 129, 192, 255, 256, 257, 512]) {
    const next = budgets({ residentCount, edgeCount: residentCount * 3 });
    nonIncreasing(previous, next);
    previous = next;
  }

  previous = budgets({ residentCount: 96, edgeCount: 0 });
  for (const edgeCount of [96, 192, 288, 384, 512, 768, 1_024, 4_096]) {
    const next = budgets({ residentCount: 96, edgeCount });
    nonIncreasing(previous, next);
    previous = next;
  }
});

test("automatic NukeFire repair has a wall deadline and finite deterministic frontiers", () => {
  assert.deepEqual(nukeFireConstraintRepairPolicy(false, {
    residentCount: 1_000,
    edgeCount: 4_000,
  }), {
    when: "defects",
    maxDurationMs: 10_000,
    maxRestarts: 4_096,
    maxLayouts: 2,
    maxPolishTournaments: 2,
    maxPolishPasses: 3,
    maxExtensionStates: 32_768,
    maxLiveSearchNodes: 4_096,
    maxMaskDiversifications: 64,
    maxCrossingWork: 512,
  });
});
