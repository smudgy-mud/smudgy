// Behaviour checks derived from Mudlet's DB_spec.lua (pinned revision in
// meta/plans/mudlet-import-research/provenance.json). Each block names the
// describe() it comes from. Where this package deliberately differs from
// DB.lua, the test says so in its title.

import assert from "node:assert/strict";
import test from "node:test";
import {
  and,
  between,
  CURRENT_TIMESTAMP,
  eq,
  exp,
  gt,
  gte,
  isIn,
  isNotNull,
  isNull,
  like,
  lt,
  lte,
  MudletDbError,
  notBetween,
  notEq,
  notIn,
  notLike,
  or,
  timestamp,
} from "../index.ts";
import { names, tempDatabase } from "./support.ts";

const code = (expected: string) => (error: unknown) => error instanceof MudletDbError && error.code === expected;

test("db:create and db:add: one row, REPLACE on a unique match, several rows at once", () => {
  const { db, dir } = tempDatabase("peoplettestingonly", {
    friends: ["name", "city", "notes"],
    enemies: { name: "", city: "", notes: "", enemied: "", kills: 0, _index: ["city"], _unique: ["name"], _violations: "REPLACE" },
  });
  try {
    db.sheets.enemies.add({ name: "Bob", city: "Sacramento" });
    assert.equal(db.sheets.enemies.fetch().length, 1);

    db.sheets.enemies.add({ name: "Bob", city: "San Francisco" });
    const enemies = db.sheets.enemies.fetch();
    assert.equal(enemies.length, 1);
    assert.equal(enemies[0].city, "San Francisco");

    db.sheets.friends.add({ name: "Ixokai", city: "Magnagora" }, { name: "Vadi", city: "New Celest" }, { name: "Heiko", city: "Hallifax", notes: "The Boss" });
    const friends = db.sheets.friends.fetch();
    assert.equal(friends.length, 3);
    assert.equal(friends[0].name, "Ixokai");
    assert.equal(friends[0].city, "Magnagora");
    assert.equal(friends[0].notes, "");
    assert.equal(friends[1].name, "Vadi");
    assert.equal(friends[2].name, "Heiko");
    assert.deepEqual(friends.map((row) => row._row_id), [1, 2, 3]);
  } finally {
    db.close();
    dir.remove();
  }
});

test("db:fetch sorting: by level then name, both descending", () => {
  const { db, dir } = tempDatabase("dslpnpdatattestingonly", {
    people: { name: "", race: "", class: "", level: 0, org: "", org_type: "", status: "", keyword: "", _index: ["name"], _unique: ["keyword"], _violations: "REPLACE" },
  });
  try {
    db.sheets.people.add(
      { name: "Bob", level: 12, class: "mage", race: "elf", keyword: "Bob" },
      { name: "Bob", level: 15, class: "warrior", race: "human", keyword: "Bob" },
      { name: "Boba", level: 15, class: "warrior", race: "human", keyword: "Boba" },
      { name: "Bobb", level: 15, class: "warrior", race: "human", keyword: "Bobb" },
      { name: "Bobc", level: 15, class: "warrior", race: "human", keyword: "Bobc" },
      { name: "Frank", level: 31, class: "cleric", race: "ogre", keyword: "Frank" },
    );
    const results = db.sheets.people.fetch(undefined, { orderBy: [db.sheets.people.fields.level, db.sheets.people.fields.name], descending: true });
    assert.equal(results.length, 5);
    assert.deepEqual([results[0].name, results[0].level], ["Frank", 31]);
    assert.deepEqual([results[1].name, results[1].level], ["Bobc", 15]);
    assert.deepEqual([results[2].name, results[2].level], ["Bobb", 15]);
    assert.deepEqual([results.at(-1)?.name, results.at(-1)?.level], ["Bob", 15]);
    assert.equal(db.sheets.people.fetch(undefined, { orderBy: db.sheets.people.fields.level, limit: 2, offset: 1 }).length, 2);
  } finally {
    db.close();
    dir.remove();
  }
});

test("db:create adds a new column to an existing sheet, empty or filled", () => {
  const { db, dir, reopen } = tempDatabase("addcolumn", { people: { name: "" } });
  try {
    db.sheets.people.add({ name: "Ada" });
    db.close();
    const grown = reopen({ people: { name: "", level: 7, notes: "n/a" } } as never);
    assert.deepEqual(grown.report.addedColumns, ["people.level", "people.notes"]);
    const ada = grown.sheet("people").fetch()[0] as Record<string, unknown>;
    assert.equal(ada.name, "Ada");
    assert.equal(ada.level, 7);
    assert.equal(ada.notes, "n/a");
    grown.close();
  } finally {
    dir.remove();
  }
});

test("options are recognised and indexes applied", () => {
  const { db, dir } = tempDatabase("optionstest", {
    sheet: { name: "", city: "", area: "", _index: ["name", ["city", "area"]], _unique: ["name"], _violations: "IGNORE" },
  });
  try {
    const indexes = db.query("SELECT name FROM sqlite_master WHERE type = 'index' AND tbl_name = 'sheet' AND sql IS NOT NULL ORDER BY name");
    assert.deepEqual(indexes.map((row) => row.name), ["idx_sheet_c_city_area", "idx_sheet_c_name"]);
    assert.deepEqual(db.sheets.sheet.add({ name: "Ada" }), [1]);
    assert.deepEqual(db.sheets.sheet.add({ name: "Ada" }), [null], "IGNORE drops the duplicate and reports no row id");
    assert.equal(db.sheets.sheet.count(), 1);
  } finally {
    db.close();
    dir.remove();
  }
});

test("column removal is not migrated: the column is kept and reported (DB.lua rebuilds or refuses)", () => {
  const { db, dir, reopen } = tempDatabase("dropcolumn", { sheet: { name: "", city: "" } });
  try {
    db.sheets.sheet.add({ name: "Ada", city: "Boston" });
    db.close();
    const narrower = reopen({ sheet: { name: "" } } as never);
    assert.equal(narrower.report.warnings.length, 1);
    assert.match(narrower.report.warnings[0], /column city exists in the file but not in the schema/);
    const row = narrower.sheet("sheet").fetch()[0] as Record<string, unknown>;
    assert.equal(row.city, "Boston");
    narrower.close();
  } finally {
    dir.remove();
  }
});

test("NULL handling: inserting, fetching by and setting NULL", () => {
  const { db, dir } = tempDatabase("mydbtnulltesting", { sheet: { name: "", level: 0, motto: "", _unique: ["name"] } });
  try {
    db.sheets.sheet.add({ name: "Bellman", level: null, motto: "" });
    let results = db.sheets.sheet.fetch();
    assert.equal(results.length, 1);
    assert.deepEqual(results[0], { _row_id: 1, name: "Bellman", level: null, motto: "" });

    db.sheets.sheet.add({ name: "Boots", level: 1, motto: "" });
    const nulls = db.sheets.sheet.fetch(isNull(db.sheets.sheet.fields.level));
    assert.deepEqual(names(nulls), ["Bellman"]);
    const notNulls = db.sheets.sheet.fetch(isNotNull(db.sheets.sheet.fields.level));
    assert.deepEqual(names(notNulls), ["Boots"]);

    db.sheets.sheet.set("motto", null, eq(db.sheets.sheet.fields.name, "Boots"));
    results = db.sheets.sheet.fetch(eq(db.sheets.sheet.fields.name, "Boots"));
    assert.equal(results[0].motto, null);
  } finally {
    db.close();
    dir.remove();
  }
});

test("default NULL: a null-defaulted column on creation and when added later", () => {
  const { db, dir, reopen } = tempDatabase("mydbtnulldefault", { sheet: { name: "", house: null, _unique: ["name"] } });
  try {
    db.sheets.sheet.add({ name: "Hermione", house: "Griffindor" }, { name: "Viktor" });
    assert.deepEqual(db.sheets.sheet.fetch(isNotNull(db.sheets.sheet.fields.house)), [{ _row_id: 1, name: "Hermione", house: "Griffindor" }]);
    assert.deepEqual(db.sheets.sheet.fetch(isNull(db.sheets.sheet.fields.house)), [{ _row_id: 2, name: "Viktor", house: null }]);
    db.close();

    const grown = reopen({ sheet: { name: "", house: null, level: 1, _unique: ["name"] } } as never);
    const rows = grown.sheet("sheet").fetch() as Array<Record<string, unknown>>;
    assert.equal(rows.length, 2);
    assert.equal(rows[0].level, 1);
    grown.close();
  } finally {
    dir.remove();
  }
});

test("timestamps: CURRENT_TIMESTAMP, an epoch, and a null round-trip; update keeps them", () => {
  const epoched = new Date(1748288082 * 1000); // 2025-05-26T19:34:42Z
  const tabled = new Date(Date.UTC(1970, 0, 1, 10, 0, 1));
  const { db, dir } = tempDatabase("mydbttimestamptesting", {
    sheet: { current: timestamp(CURRENT_TIMESTAMP), niled: timestamp(), epoched: timestamp(epoched), tabled: timestamp(tabled) },
  });
  try {
    const before = Math.floor(Date.now() / 1000);
    db.sheets.sheet.add({ current: CURRENT_TIMESTAMP, niled: null, epoched, tabled });
    const [row] = db.sheets.sheet.fetch();
    assert.ok(row.current instanceof Date);
    const stored = Math.floor(row.current.getTime() / 1000);
    assert.ok(stored >= before - 1 && stored <= before + 5, `CURRENT_TIMESTAMP is now: ${row.current.toISOString()}`);
    assert.deepEqual(row.epoched, epoched);
    assert.deepEqual(row.tabled, tabled);
    assert.equal(row.niled, null);

    db.sheets.sheet.update(row);
    const [again] = db.sheets.sheet.fetch();
    assert.deepEqual(again, row);

    assert.equal(db.query("SELECT epoched FROM sheet")[0].epoched, "2025-05-26 19:34:42", "stored as Mudlet's datetime() text");
    assert.deepEqual(db.sheets.sheet.fetch(eq(db.sheets.sheet.fields.epoched, epoched)).length, 1);
    assert.deepEqual(db.sheets.sheet.fetch(lt(db.sheets.sheet.fields.tabled, epoched)).length, 1);
    assert.deepEqual(db.sheets.sheet.aggregate(db.sheets.sheet.fields.epoched, "max"), epoched);
  } finally {
    db.close();
    dir.remove();
  }
});

test("a row added without a timestamp takes the column default", () => {
  const epoched = new Date(1748288082 * 1000);
  const { db, dir } = tempDatabase("timestampdefaults", {
    sheet: { name: "", current: timestamp(CURRENT_TIMESTAMP), fixed: timestamp(epoched), empty: timestamp() },
  });
  try {
    db.sheets.sheet.add({ name: "x" });
    const [row] = db.sheets.sheet.fetch();
    assert.ok(row.current instanceof Date);
    assert.deepEqual(row.fixed, epoched, "an epoch default is stored as the bare integer and still reads as a Date");
    assert.equal(row.empty, null);
  } finally {
    db.close();
    dir.remove();
  }
});

test("query-expression builders against real fetches", () => {
  const { db, dir } = tempDatabase("exprtestingonly", { people: { name: "", city: "", level: 0, _index: ["city"] } });
  const { people } = db.sheets;
  const f = people.fields;
  try {
    people.add(
      { name: "Ada", city: "Boston", level: 10 },
      { name: "Bram", city: "Chicago", level: 20 },
      { name: "Cyra", city: "Boston", level: 30 },
      { name: "Drake", city: "Denver", level: 40 },
      { name: "Eve", city: "Chicago", level: 50 },
    );
    assert.deepEqual(names(people.fetch(lt(f.level, 30))), ["Ada", "Bram"]);
    assert.deepEqual(names(people.fetch(lte(f.level, 30))), ["Ada", "Bram", "Cyra"]);
    assert.deepEqual(names(people.fetch(gt(f.level, 30))), ["Drake", "Eve"]);
    assert.deepEqual(names(people.fetch(gte(f.level, 30))), ["Cyra", "Drake", "Eve"]);
    assert.deepEqual(names(people.fetch(eq(f.city, "Boston"))), ["Ada", "Cyra"]);
    assert.deepEqual(names(people.fetch(eq(f.city, "BOSTON", true))), ["Ada", "Cyra"]);
    assert.deepEqual(names(people.fetch(notEq(f.city, "Boston"))), ["Bram", "Drake", "Eve"]);
    assert.deepEqual(names(people.fetch(notEq(f.city, "BOSTON", true))), ["Bram", "Drake", "Eve"]);
    assert.deepEqual(names(people.fetch(like(f.city, "Bo%"))), ["Ada", "Cyra"]);
    assert.deepEqual(names(people.fetch(notLike(f.city, "Bo%"))), ["Bram", "Drake", "Eve"]);
    assert.deepEqual(names(people.fetch(between(f.level, 20, 40))), ["Bram", "Cyra", "Drake"]);
    assert.deepEqual(names(people.fetch(notBetween(f.level, 20, 40))), ["Ada", "Eve"]);
    assert.deepEqual(names(people.fetch(isIn(f.city, ["Boston", "Denver"]))), ["Ada", "Cyra", "Drake"]);
    assert.deepEqual(names(people.fetch(notIn(f.city, ["Boston", "Denver"]))), ["Bram", "Eve"]);
    assert.deepEqual(names(people.fetch(isIn(f.city, []))), []);
    assert.deepEqual(names(people.fetch(exp("level > 25"))), ["Cyra", "Drake", "Eve"]);
    assert.deepEqual(names(people.fetch(exp("level > ?", [25]))), ["Cyra", "Drake", "Eve"]);
    assert.deepEqual(names(people.fetch([exp("level > 25"), eq(f.city, "Chicago")])), ["Eve"]);
    assert.deepEqual(names(people.fetch(and(exp("level > 25"), eq(f.city, "Chicago")))), ["Eve"]);
    assert.deepEqual(names(people.fetch(or(exp("level < 15"), exp("level > 45")))), ["Ada", "Eve"]);
    assert.deepEqual(names(people.fetch(and(eq(f.city, "Boston"), gt(f.level, 15)))), ["Cyra"]);
    assert.deepEqual(names(people.fetch(or(eq(f.city, "Denver"), eq(f.city, "Chicago")))), ["Bram", "Drake", "Eve"]);
    assert.deepEqual(names(people.fetch([eq(f.city, "Chicago"), gt(f.level, 30)])), ["Eve"]);
    assert.deepEqual(names(people.fetch(people.where({ city: "Chicago", level: 50 }))), ["Eve"]);
    people.set(f.city, null, eq(f.name, "Ada"));
    assert.deepEqual(names(people.fetch(isNull(f.city))), ["Ada"]);
    assert.deepEqual(names(people.fetch(isNotNull(f.city))), ["Bram", "Cyra", "Drake", "Eve"]);
    assert.deepEqual(names(people.fetch(people.where({ city: null }))), ["Ada"]);
    assert.deepEqual(names(people.fetch(eq(f.level, "20" as never))), ["Bram"], "numeric text compares as a number, as DB.lua's tonumber did");
    assert.deepEqual(names(people.fetch(eq(f.level, 20, true))), ["Bram"]);
  } finally {
    db.close();
    dir.remove();
  }
});

test("builders refuse what DB.lua would have pasted into SQL blindly", () => {
  const { db, dir } = tempDatabase("exprguards", { people: { name: "", level: 0 } });
  try {
    assert.throws(() => eq(db.sheets.people.fields.name, null as never), code("argument"));
    assert.throws(() => eq("name" as never, "x"), code("argument"));
    assert.throws(() => eq(db.sheets.people.fields.name, true as never), code("value"));
    assert.throws(() => and("x" as never), code("argument"));
    assert.throws(() => db.sheets.people.fetch({ name: "x" } as never), /sheet\.where/);
    assert.throws(() => db.sheets.people.fetch(undefined, { orderBy: "name" as never }), code("argument"));
    assert.throws(() => db.sheets.people.fetch(undefined, { offset: 2 }), code("argument"));
  } finally {
    db.close();
    dir.remove();
  }
});

test("db:delete: by _row_id, by fetched row, by expression, all rows, and its refusals", () => {
  const { db, dir } = tempDatabase("deletetestingonly", { sheet: { name: "", city: "", _index: ["name"] } });
  const { sheet } = db.sheets;
  try {
    const seed = () => {
      sheet.delete(true);
      sheet.add({ name: "Ada", city: "Boston" }, { name: "Bram", city: "Chicago" }, { name: "Cyra", city: "Boston" }, { name: "Drake", city: "Denver" });
    };
    seed();
    const ada = sheet.fetchOne(eq(sheet.fields.name, "Ada"))!;
    assert.equal(sheet.delete(ada._row_id), 1);
    assert.equal(sheet.fetch(eq(sheet.fields.name, "Ada")).length, 0);
    assert.equal(sheet.count(), 3);

    seed();
    const bram = sheet.fetchOne(eq(sheet.fields.name, "Bram"))!;
    assert.equal(sheet.delete(bram), 1);
    assert.equal(sheet.count(), 3);

    seed();
    assert.equal(sheet.delete(eq(sheet.fields.city, "Boston")), 2);
    assert.deepEqual(names(sheet.fetch()), ["Bram", "Drake"]);

    seed();
    assert.equal(sheet.delete(exp("city = 'Boston'")), 2);
    assert.deepEqual(names(sheet.fetch()), ["Bram", "Drake"]);

    seed();
    assert.equal(sheet.delete(true), 4);
    assert.equal(sheet.count(), 0);

    assert.throws(() => (sheet.delete as (target?: unknown) => number)(), /needs a _row_id, a row, a query, or true/);
    assert.throws(() => sheet.delete({ name: "Ada" } as never), /needs a row with a _row_id/);
  } finally {
    db.close();
    dir.remove();
  }
});

test("db:merge_unique: updates matching rows, inserts the rest, and its refusals", () => {
  const { db, dir } = tempDatabase("mergetestingonly", {
    friends: { name: "", city: "", level: 0, _unique: ["name"], _violations: "REPLACE" },
    pairs: { name: "", city: "", _unique: [["name", "city"]] },
  });
  try {
    db.sheets.friends.add({ name: "Ada", city: "Boston", level: 10 }, { name: "Bram", city: "Chicago", level: 20 });
    const rows = db.sheets.friends.fetch();
    for (const row of rows) row.city = "Mutantville";
    db.sheets.friends.mergeUnique([...rows, { name: "Cyra", city: "Denver", level: 5 }]);
    const after = db.sheets.friends.fetch();
    assert.equal(after.length, 3);
    const byName = Object.fromEntries(after.map((row) => [row.name, row]));
    assert.equal(byName.Ada.city, "Mutantville");
    assert.equal(byName.Bram.city, "Mutantville");
    assert.equal(byName.Ada.level, 10);
    assert.equal(byName.Cyra.city, "Denver");
    assert.equal(byName.Cyra.level, 5);

    db.sheets.friends.mergeUnique([{ name: "Ada", city: "Rome" }]);
    const adas = db.sheets.friends.fetch(eq(db.sheets.friends.fields.name, "Ada"));
    assert.equal(adas.length, 1);
    assert.equal(adas[0].city, "Rome");
    assert.equal(adas[0].level, 10);

    assert.throws(() => db.sheets.friends.mergeUnique(null as never), /needs a list of rows/);
    assert.throws(() => db.sheets.friends.mergeUnique([{ city: "Nowhere" }]), /missing its unique key name/);
    db.sheets.pairs.add({ name: "Ada", city: "Boston" });
    assert.throws(() => db.sheets.pairs.mergeUnique([{ name: "Ada", city: "Boston" }]), code("unsupported"));
  } finally {
    db.close();
    dir.remove();
  }
});

test("transaction rollback discards uncommitted rows; committed rows survive a reopen", () => {
  const { db, dir, reopen } = tempDatabase("rollbacktestingonly", { sheet: { name: "", _index: ["name"] } });
  try {
    db.sheets.sheet.add({ name: "committed" });
    assert.equal(db.sheets.sheet.count(), 1);
    db.begin();
    db.sheets.sheet.add({ name: "pending1" });
    db.sheets.sheet.add({ name: "pending2" });
    assert.equal(db.sheets.sheet.count(), 3);
    db.rollback();
    assert.deepEqual(names(db.sheets.sheet.fetch()), ["committed"]);

    db.begin();
    db.sheets.sheet.add({ name: "pending" });
    db.commit();
    db.close();
    const again = reopen();
    assert.equal(again.sheets.sheet.count(), 2);
    again.close();
  } finally {
    dir.remove();
  }
});

test("transaction(): commits on return, rolls back on throw, nests as savepoints", () => {
  const { db, dir } = tempDatabase("txnblocks", { sheet: { name: "" } });
  try {
    const ids = db.transaction(() => db.sheets.sheet.add({ name: "kept" }));
    assert.deepEqual(ids, [1]);
    assert.throws(
      () =>
        db.transaction(() => {
          db.sheets.sheet.add({ name: "dropped" });
          throw new Error("boom");
        }),
      /boom/,
    );
    assert.deepEqual(names(db.sheets.sheet.fetch()), ["kept"]);
    assert.equal(db.inTransaction, false);

    db.transaction(() => {
      db.sheets.sheet.add({ name: "outer" });
      try {
        db.transaction(() => {
          db.sheets.sheet.add({ name: "inner" });
          throw new Error("inner failure");
        });
      } catch {
        // the inner savepoint rolled back; the outer work stands
      }
      assert.equal(db.inTransaction, true);
    });
    assert.deepEqual(names(db.sheets.sheet.fetch()), ["kept", "outer"]);

    assert.throws(() => db.commit(), code("transaction"));
    assert.throws(() => db.rollback(), code("transaction"));
    db.begin();
    assert.throws(() => db.begin(), code("transaction"));
    db.rollback();
  } finally {
    db.close();
    dir.remove();
  }
});

test("db:update and db:set edge cases", () => {
  const { db, dir } = tempDatabase("updatetestingonly", { sheet: { name: "", city: "", kills: 0, _unique: ["name"], _violations: "REPLACE" } });
  const { sheet } = db.sheets;
  const f = sheet.fields;
  try {
    sheet.add({ name: "Ada", city: "Boston", kills: 3 }, { name: "Bram", city: "Chicago", kills: 7 });
    const ada = sheet.fetchOne(eq(f.name, "Ada"))!;
    ada.city = "Rome";
    sheet.update(ada);
    const reread = sheet.fetchOne(eq(f.name, "Ada"))!;
    assert.deepEqual([reread.city, reread.name, reread.kills], ["Rome", "Ada", 3]);

    assert.throws(() => sheet.update({ name: "Ada", city: "Rome" } as never), /_row_id/);
    assert.throws(() => sheet.update({ _row_id: 999, city: "Nowhere" }), /no row has _row_id 999/);
    assert.throws(() => sheet.update({ _row_id: ada._row_id }), /no columns to write/);
    assert.throws(() => sheet.update({ _row_id: ada._row_id, nope: 1 } as never), code("schema"));

    assert.equal(sheet.set("city", "Rome", eq(f.name, "Ada")), 1);
    assert.equal(sheet.fetchOne(eq(f.name, "Ada"))!.city, "Rome");
    assert.equal(sheet.fetchOne(eq(f.name, "Bram"))!.city, "Chicago");

    assert.equal(sheet.set("kills", 0), 2);
    assert.ok(sheet.fetch().every((row) => row.kills === 0));

    sheet.set("kills", 3, eq(f.name, "Ada"));
    sheet.set("kills", 7, eq(f.name, "Bram"));
    sheet.set(f.kills, exp("kills + 1"), eq(f.name, "Ada"));
    assert.equal(sheet.fetchOne(eq(f.name, "Ada"))!.kills, 4);
    assert.equal(sheet.fetchOne(eq(f.name, "Bram"))!.kills, 7);

    sheet.set("city", "Rome", exp("kills > 5"));
    assert.equal(sheet.fetchOne(eq(f.name, "Ada"))!.city, "Rome");
    assert.equal(sheet.fetchOne(eq(f.name, "Bram"))!.city, "Rome");
    assert.throws(() => sheet.set(sheet.fields._row_id, 5), /_row_id cannot be changed/);
  } finally {
    db.close();
    dir.remove();
  }
});

test("update writes null (DB.lua's update skipped falsy values, so a field could never be cleared)", () => {
  const { db, dir } = tempDatabase("updatenull", { sheet: { name: "", city: "" } });
  try {
    db.sheets.sheet.add({ name: "Ada", city: "Boston" });
    const ada = db.sheets.sheet.fetch()[0];
    ada.city = null;
    db.sheets.sheet.update(ada);
    assert.equal(db.sheets.sheet.fetch()[0].city, null);
  } finally {
    db.close();
    dir.remove();
  }
});

test("aggregates: count, total, avg, min, max, distinct and with a query", () => {
  const { db, dir } = tempDatabase("aggregatetest", { sheet: { name: "", count: 0 } });
  const { sheet } = db.sheets;
  const f = sheet.fields;
  try {
    sheet.add({ name: "Ada", count: 10 }, { name: "Bram", count: 12 }, { name: "Cyra", count: 12 }, { name: "Drake", count: 20 });
    assert.equal(sheet.aggregate(f.count, "total"), 54);
    assert.equal(sheet.aggregate(f.count, "avg"), 13.5);
    assert.equal(sheet.aggregate(f.count, "min"), 10);
    assert.equal(sheet.aggregate(f.count, "max"), 20);
    assert.equal(sheet.aggregate(f.name, "min"), "Ada");
    assert.equal(sheet.aggregate(f.name, "max"), "Drake");
    assert.equal(sheet.aggregate(f.name, "count"), 4);
    assert.equal(sheet.aggregate(f.count, "total", gt(f.count, 11)), 44);
    assert.equal(sheet.aggregate(f.count, "avg", exp("count > 11")), 44 / 3);
    assert.equal(sheet.aggregate(f.count, "count", undefined, true), 3);
    assert.equal(sheet.aggregate(f.count, "total", undefined, true), 42);
    assert.equal(sheet.count(gt(f.count, 11)), 3);
    sheet.delete(true);
    assert.equal(sheet.aggregate(f.count, "max"), null);
    assert.equal(sheet.aggregate(f.count, "total"), 0);
    assert.equal(sheet.count(), 0);
    assert.throws(() => sheet.aggregate(f.count, "median" as never), code("argument"));
  } finally {
    db.close();
    dir.remove();
  }
});

test("db.Database:_drop removes the sheet and its indexes", () => {
  const { db, dir } = tempDatabase("droptestingonly", { people: { name: "", city: "", _index: ["city"], _unique: ["name"] }, other: { x: "" } });
  try {
    db.sheets.people.add({ name: "Ada", city: "Boston" });
    db.dropSheet("people");
    assert.deepEqual(db.query("SELECT name FROM sqlite_master WHERE name = 'people' OR tbl_name = 'people'"), []);
    assert.deepEqual(db.sheetNames, ["other"]);
    assert.throws(() => db.sheet("people"), code("schema"));
  } finally {
    db.close();
    dir.remove();
  }
});

test("every call site against a closed database raises and names the file", () => {
  const { db, dir } = tempDatabase("connguardtestingonly", { sheet: { name: "", _unique: ["name"] } });
  try {
    const { sheet } = db.sheets;
    sheet.add({ name: "before" });
    db.close();
    assert.equal(db.isOpen, false);
    db.close();
    const closed = (fn: () => unknown) =>
      assert.throws(fn, (error: unknown) => error instanceof MudletDbError && error.code === "closed" && error.message.includes(db.path));
    closed(() => sheet.add({ name: "x" }));
    closed(() => sheet.fetch());
    closed(() => sheet.fetchSql("SELECT * FROM sheet"));
    closed(() => sheet.update({ _row_id: 1, name: "x" }));
    closed(() => sheet.set("name", "x"));
    closed(() => sheet.delete(1));
    closed(() => sheet.count());
    closed(() => sheet.mergeUnique([]));
    closed(() => db.begin());
    closed(() => db.commit());
    closed(() => db.rollback());
    closed(() => db.transaction(() => 1));
    closed(() => db.query("SELECT 1"));
    closed(() => db.dropSheet("sheet"));
    assert.equal(sheet.fields.name.name, "name", "field references outlive the connection");
  } finally {
    dir.remove();
  }
});

test("data persists across close and reopen, and the file carries Mudlet's name", () => {
  const { db, dir, reopen } = tempDatabase("Close Testing Only", { sheet: { name: "", city: "", _index: ["name"] } });
  try {
    assert.match(db.path, /Database_closetestingonly\.db$/);
    db.sheets.sheet.add({ name: "Ada", city: "Boston" });
    db.close();
    const again = reopen();
    assert.deepEqual(again.report.createdSheets, []);
    assert.deepEqual(again.report.warnings, []);
    const rows = again.sheets.sheet.fetch();
    assert.deepEqual(rows, [{ _row_id: 1, name: "Ada", city: "Boston" }]);
    again.close();
  } finally {
    dir.remove();
  }
});

test("a unique violation under FAIL throws a sqlite error the caller can catch", () => {
  const { db, dir } = tempDatabase("uniquefail", { sheet: { name: "", _unique: ["name"] } });
  try {
    db.sheets.sheet.add({ name: "Ada" });
    assert.throws(() => db.sheets.sheet.add({ name: "Ada" }), (error: unknown) => error instanceof MudletDbError && error.code === "sqlite" && /UNIQUE constraint failed: sheet\.name/.test(error.message));
    assert.equal(db.sheets.sheet.count(), 1);
    assert.throws(() => db.sheets.sheet.add({ name: "Bram" }, { name: "Ada" }), code("sqlite"));
    assert.equal(db.sheets.sheet.count(), 1, "a multi-row add is all or nothing (DB.lua left the earlier rows pending)");
  } finally {
    db.close();
    dir.remove();
  }
});

test("db:fetch_sql: coerced rows, empty list for no match, error for bad SQL (DB.lua returned nil)", () => {
  const { db, dir } = tempDatabase("dbinternalstestingonly", { people: { name: "", city: "", kills: 0, seen: timestamp(CURRENT_TIMESTAMP), _index: ["city"] } });
  try {
    db.sheets.people.add({ name: "Bob", city: "Ankh-Morpork", kills: 3 });
    db.sheets.people.add({ name: "Carrot", city: "Ankh-Morpork", kills: 7 });
    const rows = db.sheets.people.fetchSql("SELECT * FROM people ORDER BY name");
    assert.deepEqual(rows.map((row) => row.name), ["Bob", "Carrot"]);
    assert.equal(rows[0].kills, 3);
    assert.equal(typeof rows[0]._row_id, "number");
    assert.ok(rows[0].seen instanceof Date);
    assert.deepEqual(db.sheets.people.fetchSql("SELECT * FROM people WHERE name = ?", ["Nobody"]), []);
    assert.equal(db.sheets.people.fetchSql("SELECT * FROM people WHERE kills > 5")[0].name, "Carrot");
    assert.throws(() => db.sheets.people.fetchSql("SELECT * FROM"), code("sqlite"));
    assert.throws(() => db.sheets.people.fetchSql("SELECT * FROM nosuchsheet"), code("sqlite"));
  } finally {
    db.close();
    dir.remove();
  }
});

test("_violations changes are refused instead of rebuilding the table", () => {
  const { db, dir, reopen } = tempDatabase("violationsmigrate", { sheet: { name: "", _unique: ["name"], _violations: "FAIL" } });
  try {
    db.sheets.sheet.add({ name: "Ada" });
    db.close();
    assert.throws(
      () => reopen({ sheet: { name: "", _unique: ["name"], _violations: "REPLACE" } } as never),
      (error: unknown) => error instanceof MudletDbError && error.code === "schema-mismatch" && /sheet "sheet"/.test(error.message),
    );
    const same = reopen();
    assert.equal(same.sheets.sheet.count(), 1, "the refused open left the file untouched");
    same.close();
  } finally {
    dir.remove();
  }
});

test("read-only opening: no writes, and a schema that needs changes is refused", () => {
  const { db, dir, reopen } = tempDatabase("readonly", { sheet: { name: "" } });
  try {
    db.sheets.sheet.add({ name: "Ada" });
    db.close();
    const ro = reopen(undefined, { readOnly: true });
    assert.equal(ro.sheets.sheet.count(), 1);
    assert.throws(() => ro.sheets.sheet.add({ name: "x" }), code("unsupported"));
    ro.close();
    assert.throws(() => reopen({ sheet: { name: "", extra: 0 } } as never, { readOnly: true }), code("schema-mismatch"));
  } finally {
    dir.remove();
  }
});

test("create: false refuses a missing file; sheet names cannot collide with operations", () => {
  const { db, dir } = tempDatabase("createfalse", { sheet: { name: "" } });
  try {
    db.close();
    assert.throws(
      () => tempDatabase("nothing", { sheet: { name: "" } }, { create: false }),
      (error: unknown) => error instanceof MudletDbError && /no database file/.test(error.message),
    );
    const clash = tempDatabase("clash", { close: { name: "" }, sheet: { name: "" } });
    clash.db.sheets.close.add({ name: "fine" });
    assert.equal(clash.db.sheet("sheet").count(), 0, "sheet names never collide with the handle's operations");
    clash.db.close();
    clash.dir.remove();
  } finally {
    dir.remove();
  }
});
