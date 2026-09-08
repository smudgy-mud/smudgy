import assert from "node:assert/strict";
import test from "node:test";
import { existsSync } from "node:fs";
import { join } from "node:path";
import { copyDatabase, MudletDbError, openDatabase } from "../index.ts";
import { tempDatabase } from "./support.ts";

test("copyDatabase snapshots committed rows while the source stays open, and refuses to clobber", () => {
  const { db, dir } = tempDatabase("copysource", { sheet: { name: "" } });
  try {
    db.sheets.sheet.add({ name: "committed" });
    db.begin();
    db.sheets.sheet.add({ name: "pending" });

    const target = join(dir.path, "snapshot.db");
    assert.deepEqual(copyDatabase(db.path, target), { path: target });
    assert.ok(existsSync(target));
    const copy = openDatabase(target, { sheet: { name: "" } });
    try {
      assert.deepEqual(copy.sheets.sheet.fetch().map((row) => row.name), ["committed"], "the copy is the last committed state");
    } finally {
      copy.close();
    }
    db.rollback();

    assert.throws(() => copyDatabase(db.path, target), (error: unknown) => error instanceof MudletDbError && /already exists/.test(error.message));
    assert.deepEqual(copyDatabase(db.path, target, { overwrite: true }), { path: target });
    assert.throws(() => copyDatabase(join(dir.path, "missing.db"), target, { overwrite: true }), /no database file/);
    assert.throws(() => copyDatabase(db.path, db.path), /two different files/);
  } finally {
    db.close();
    dir.remove();
  }
});
