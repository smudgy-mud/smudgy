import assert from "node:assert/strict";
import test from "node:test";
import {
  BOUNDED_REFLOW_PLANNING_PASSES,
  BOUNDED_REFLOW_TIMEOUT_MS,
  nukeFirePerfectReflowPolicy,
  parseReflowMode,
  perfectRepairPolicyWire,
  reflowRepairTerminalReason,
} from "./reflow-command.ts";
import { nukeFirePerfectConstraintRepairPolicy } from "../nukefire-mapper/constraint-policy.ts";

test("ordinary reflow is bounded and perfect search must be explicit", () => {
  assert.equal(parseReflowMode(""), "bounded");
  assert.equal(parseReflowMode(" PERFECT "), "perfect");
  assert.equal(parseReflowMode("thorough"), undefined);
  assert.ok(Number.isFinite(BOUNDED_REFLOW_PLANNING_PASSES));
  assert.ok(Number.isFinite(BOUNDED_REFLOW_TIMEOUT_MS));
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
