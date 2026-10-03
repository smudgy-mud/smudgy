import assert from "node:assert/strict";
import test from "node:test";
import { memorySampleText } from "./layout-telemetry.ts";

test("absent worker memory samples are distinct from a measured zero", () => {
  for (const value of [undefined, null, Number.NaN, Number.POSITIVE_INFINITY, -1, "0"]) {
    assert.equal(memorySampleText(value), "unavailable");
  }
  assert.equal(memorySampleText(0), "0 B");
  assert.equal(memorySampleText(2048), "2048 B");
});
