import assert from "node:assert/strict";
import test from "node:test";
import {
  NUKEFIRE_ATLAS_NAME,
  upsertLocalNukeFireAtlas,
} from "./atlas-resolution.ts";

function atlas(id: number, name: string, storage: MapStorage): Atlas {
  return {
    id: [0, id],
    name,
    storage,
    toString: () => name,
  };
}

test("reuses the existing local Nukefire atlas", async () => {
  const cloud = atlas(1, NUKEFIRE_ATLAS_NAME, "cloud");
  const local = atlas(2, NUKEFIRE_ATLAS_NAME, "local");
  let createCalls = 0;

  const resolved = await upsertLocalNukeFireAtlas({
    listAtlases: async () => [cloud, local],
    createAtlas: async () => {
      createCalls += 1;
      return atlas(3, NUKEFIRE_ATLAS_NAME, "local");
    },
  });

  assert.equal(resolved, local);
  assert.equal(createCalls, 0);
});

test("creates the Nukefire atlas in local storage when absent", async () => {
  const created = atlas(2, NUKEFIRE_ATLAS_NAME, "local");
  const calls: Array<{ name: string; storage: string }> = [];

  const resolved = await upsertLocalNukeFireAtlas({
    listAtlases: async () => [atlas(1, NUKEFIRE_ATLAS_NAME, "cloud")],
    createAtlas: async (name, options) => {
      calls.push({ name, storage: options.storage });
      return created;
    },
  });

  assert.equal(resolved, created);
  assert.deepEqual(calls, [{ name: NUKEFIRE_ATLAS_NAME, storage: "local" }]);
});
