import assert from "node:assert/strict";
import test from "node:test";
import {
  BOUNDED_REFLOW_PLANNING_PASSES,
  BOUNDED_REFLOW_TIMEOUT_MS,
  nukeFirePerfectReflowPolicy,
  parseReflowMode,
  perfectRepairPolicyWire,
  reflowRepairTerminalReason,
  reflowRepairStatusText,
} from "./reflow-command.ts";
import { nukeFirePerfectConstraintRepairPolicy } from "../nukefire-mapper/constraint-policy.ts";
import { constraintLayoutInternalsForTesting, repairIntegralLayoutConstraints } from "../map-layout/constraint-layout.ts";
import { measureIntegralLayoutQuality, type IntegralLayoutPlan, type IntegralLayoutRequest } from "../map-layout/layout.ts";

test("ordinary reflow is bounded and perfect search must be explicit", () => {
  assert.equal(parseReflowMode(""), "bounded");
  assert.equal(parseReflowMode(" PERFECT "), "perfect");
  assert.equal(parseReflowMode("thorough"), undefined);
  assert.ok(Number.isFinite(BOUNDED_REFLOW_PLANNING_PASSES));
  assert.ok(Number.isFinite(BOUNDED_REFLOW_TIMEOUT_MS));
});

function storedPlan(request: IntegralLayoutRequest): IntegralLayoutPlan {
  const positions = new Map(request.residents.map(({ id, position }) => [id, position]));
  return { positions, movedExisting: new Set(), quality: measureIntegralLayoutQuality(positions, request.edges) };
}

test("actual skipped repair reports explain clean, unsearchable, and unavailable search", () => {
  const clean: IntegralLayoutRequest = {
    residents: [{ id: "a", position: { x: 0, y: 0, level: 0 }, movable: true }, { id: "b", position: { x: 1, y: 0, level: 0 }, movable: true }],
    nodes: [], edges: [{ from: "a", to: "b", direction: "East" }], allowExistingMoves: true,
  };
  const projected: IntegralLayoutRequest = {
    ...clean,
    residents: [{ id: "a", position: { x: 0, y: 0, level: 0 }, movable: true }, { id: "b", position: { x: 3, y: 2, level: 0 }, movable: true }],
    edges: [{ from: "a", to: "b", direction: "Up", constraintVector: { x: 1, y: -1, level: 0 } }],
  };
  const defective: IntegralLayoutRequest = {
    ...projected, edges: [{ from: "a", to: "b", direction: "East" }],
  };
  const cases = [
    [repairIntegralLayoutConstraints(clean, storedPlan(clean), { when: "defects" }), "clean", "clean"],
    [repairIntegralLayoutConstraints(projected, storedPlan(projected), { when: "always" }), "no-constraints", "not-searched"],
    [repairIntegralLayoutConstraints(defective, storedPlan(defective), { when: "always", maxDurationMs: 0 }), "no-budget", "not-searched"],
    [constraintLayoutInternalsForTesting.repairWithFailure(defective, storedPlan(defective), { when: "always" }, "search"), "search-failed:analysis", "not-searched"],
  ] as const;
  for (const [plan, outcome, terminal] of cases) {
    assert.ok(plan.constraintRepair);
    assert.equal(plan.constraintRepair.outcome, outcome);
    assert.equal(reflowRepairTerminalReason(plan.constraintRepair), terminal);
    assert.doesNotMatch(reflowRepairStatusText(plan.constraintRepair), /incomplete|fixed point/);
  }
  assert.match(reflowRepairStatusText(cases[1][0].constraintRepair!), /no exits can be expressed/);
  assert.match(reflowRepairStatusText(cases[3][0].constraintRepair!), /analysis failed/);
});

test("actual deadline and work-ceiling reports retain their terminal precedence", () => {
  const request: IntegralLayoutRequest = {
    residents: [0, 1, 2].map((x) => ({ id: String(x), position: { x, y: 0, level: 0 }, movable: true })),
    nodes: [],
    edges: [{ from: "0", to: "1", direction: "East" }, { from: "1", to: "2", direction: "East" }, { from: "2", to: "0", direction: "East" }],
    allowExistingMoves: true,
  };
  const seed = storedPlan(request);
  let tick = 0;
  const timed = constraintLayoutInternalsForTesting.repairWithClock(request, seed, { when: "always", maxDurationMs: 1 }, () => tick += 1000);
  assert.ok(timed.constraintRepair);
  assert.equal(reflowRepairTerminalReason(timed.constraintRepair), "timeout");
  const limited = repairIntegralLayoutConstraints(request, seed, {
    when: "always", maxDurationMs: Infinity, maxRestarts: 1, maxLayouts: 1,
    maxExtensionStates: 0, maxMaskDiversifications: 1, maxCrossingWork: 0,
  });
  assert.ok(limited.constraintRepair);
  assert.equal(reflowRepairTerminalReason(limited.constraintRepair), "ceiling");
  assert.match(reflowRepairStatusText(limited.constraintRepair), /deterministic ceiling/);
  assert.equal(reflowRepairTerminalReason({ ...limited.constraintRepair, extensionSearch: { cancelled: true } }), "cancelled");
  assert.equal(reflowRepairTerminalReason({ ...timed.constraintRepair, extensionSearch: { cancelled: true } }), "timeout");
});

test("the side-effect-free perfect command policy stays identical to the mapper policy", () => {
  for (const scale of [
    { residentCount: 1, edgeCount: 0 },
    { residentCount: 48, edgeCount: 120 },
    { residentCount: 256, edgeCount: 768 },
  ]) {
    const policy = nukeFirePerfectReflowPolicy(scale);
    assert.deepEqual(policy, nukeFirePerfectConstraintRepairPolicy(scale));
    assert.deepEqual(perfectRepairPolicyWire(policy), {
      ...policy,
      maxDurationMs: "infinity",
    });
  }
});

test("deadline terminal classification outranks cooperative cancellation", () => {
  assert.equal(reflowRepairTerminalReason({
    geometricFixedPoint: false,
    cutoff: "time",
    extensionSearch: { cancelled: true },
  }), "timeout");
});
