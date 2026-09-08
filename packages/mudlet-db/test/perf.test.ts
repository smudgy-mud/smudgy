// Timings for the synchronous operations, on a sheet the size of a busy
// tracker (ShipDB-scale docking history). `DatabaseSync` blocks the script
// thread, so these numbers say how long a trigger callback stalls. The
// assertions are loose sanity bounds; the printed figures are the output.

import assert from "node:assert/strict";
import test from "node:test";
import { eq, like, timestamp, CURRENT_TIMESTAMP } from "../index.ts";
import { tempDatabase } from "./support.ts";

const ROWS = 10_000;

function timed<T>(label: string, work: () => T): T {
  const start = performance.now();
  const result = work();
  const millis = performance.now() - start;
  console.log(`  ${label}: ${millis.toFixed(1)} ms`);
  return result;
}

test(`synchronous operations on ${ROWS} rows`, () => {
  const { db, dir } = tempDatabase("perf", {
    docking_records: { character: "", name: "", type: "", owner: "", planet: "", dock: "", time: timestamp(CURRENT_TIMESTAMP), _index: ["name", ["character", "name"]] },
  });
  const sheet = db.sheets.docking_records;
  const f = sheet.fields;
  try {
    const rows = Array.from({ length: ROWS }, (_, index) => ({
      character: `Char${index % 7}`,
      name: `Ship ${index % 500}`,
      type: index % 3 === 0 ? "YT-1300" : "Lambda shuttle",
      owner: `Owner ${index % 50}`,
      planet: `Planet ${index % 20}`,
      dock: `Dock ${index % 9}`,
    }));

    timed(`add ${ROWS} rows one call each, autocommit`, () => {
      for (const row of rows.slice(0, 1000)) sheet.add(row);
    });
    timed(`add ${ROWS - 1000} rows in one transaction`, () => {
      db.transaction(() => {
        for (const row of rows.slice(1000)) sheet.add(row);
      });
    });
    assert.equal(sheet.count(), ROWS);

    const byName = timed("fetch by indexed name (20 rows)", () => sheet.fetch(eq(f.name, "Ship 42")));
    assert.equal(byName.length, 20);
    const byPair = timed("fetch by compound index (character, name)", () => sheet.fetch([eq(f.character, "Char0"), eq(f.name, "Ship 42")]));
    assert.ok(byPair.length >= 1);
    const scan = timed("fetch all rows (full scan + coercion)", () => sheet.fetch());
    assert.equal(scan.length, ROWS);
    const pattern = timed("fetch LIKE 'Ship 4%' (unindexed pattern)", () => sheet.fetch(like(f.name, "Ship 4%")));
    assert.ok(pattern.length > 0);
    timed("count()", () => sheet.count());
    timed("update one row by _row_id", () => sheet.update({ ...byName[0], dock: "Dock X" }));
    timed("set() across 20 rows", () => sheet.set("planet", "Elsewhere", eq(f.name, "Ship 42")));
    timed("delete 20 rows by query", () => sheet.delete(eq(f.name, "Ship 43")));
    assert.equal(sheet.count(), ROWS - 20);
  } finally {
    db.close();
    dir.remove();
  }
});
