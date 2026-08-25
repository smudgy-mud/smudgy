import assert from "node:assert/strict";
import test from "node:test";
import {
  isCurrentMapperRun,
  ObsoleteNukeFireMapperRunError,
  whileCurrentMapperRun,
} from "./run-generation.ts";

test("mapper work is current only while its captured ownership run remains active", () => {
  assert.equal(isCurrentMapperRun(true, 4, 4), true);
  assert.equal(isCurrentMapperRun(false, 4, 4), false);
  assert.equal(isCurrentMapperRun(true, 5, 4), false);
});

test("stop and restart rejects old async work while the new generation can continue", async () => {
  let state = { started: true, generation: 4 };
  let finish!: () => void;
  const paused = new Promise<void>((resolve) => {
    finish = resolve;
  });
  const oldWork = whileCurrentMapperRun(4, () => state, async () => {
    await paused;
    return "old";
  });

  state = { started: false, generation: 5 };
  state = { started: true, generation: 5 };
  finish();
  await assert.rejects(oldWork, ObsoleteNukeFireMapperRunError);

  assert.equal(
    await whileCurrentMapperRun(5, () => state, async () => "new"),
    "new",
  );
});
