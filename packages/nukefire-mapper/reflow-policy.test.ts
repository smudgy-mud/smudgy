import assert from "node:assert/strict";
import test from "node:test";
import { reflowPolicy } from "./reflow-policy.ts";

test("an intermediate topology snapshot places discoveries and defers movement", () => {
  assert.deepEqual(reflowPolicy(true, false, false, true), {
    runPlanner: true,
    moveExisting: false,
    deferExistingReflow: true,
  });
});

test("the newest snapshot performs and clears a deferred reflow", () => {
  assert.deepEqual(reflowPolicy(false, true, true, true), {
    runPlanner: true,
    moveExisting: true,
    deferExistingReflow: false,
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

test("a superseded full reflow preserves discoveries and defers existing moves", () => {
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
