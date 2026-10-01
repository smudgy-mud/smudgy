import assert from "node:assert/strict";
import test from "node:test";
import { reflowPolicy } from "./reflow-policy.ts";

test("the prompt topology lane places discoveries and defers movement", () => {
  assert.deepEqual(reflowPolicy(true, false, false, true), {
    runPlanner: true,
    moveExisting: false,
    deferExistingReflow: true,
  });
});

test("the quiet full-reflow lane performs and clears deferred movement", () => {
  assert.deepEqual(reflowPolicy(false, true, true, true), {
    runPlanner: true,
    moveExisting: true,
    deferExistingReflow: false,
  });
});

test("the prompt lane carries already-deferred work without inventing topology growth", () => {
  assert.deepEqual(reflowPolicy(false, true, false, true), {
    runPlanner: false,
    moveExisting: false,
    deferExistingReflow: true,
  });
});

test("ordinary movement skips the planner", () => {
  assert.deepEqual(reflowPolicy(false, false, true, true), {
    runPlanner: false,
    moveExisting: true,
    deferExistingReflow: false,
  });
});

test("coordinate locking never creates deferred movement work", () => {
  assert.deepEqual(reflowPolicy(true, false, false, false), {
    runPlanner: true,
    moveExisting: false,
    deferExistingReflow: false,
  });
});

test("revoking full-reflow permission preserves discoveries and defers existing moves", () => {
  assert.deepEqual(reflowPolicy(true, false, true, true), {
    runPlanner: true,
    moveExisting: true,
    deferExistingReflow: false,
  });
  assert.deepEqual(reflowPolicy(true, false, false, true), {
    runPlanner: true,
    moveExisting: false,
    deferExistingReflow: true,
  });
});
