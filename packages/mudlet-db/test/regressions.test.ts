import assert from "node:assert/strict";
import test from "node:test";
import { linkSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { copyDatabase, extractTableConstraints, MudletDbError, openDatabase } from "../index.ts";
import { tempDatabase } from "./support.ts";

const code = (expected: string) => (error: unknown) => error instanceof MudletDbError && error.code === expected;

test("copy refuses path aliases and hard links without changing the source", () => {
  const { db, dir } = tempDatabase("copyidentity", { sheet: { name: "" } });
  try {
    db.sheets.sheet.add({ name: "keep me" });
    db.close();
    const before = readFileSync(db.path);
    const hardLink = join(dir.path, "hard-link.db");
    linkSync(db.path, hardLink);
    const aliases = [db.path, `${dir.path}/./Database_copyidentity.db`, hardLink];
    if (process.platform === "win32") aliases.push(db.path.toUpperCase());
    for (const alias of aliases) {
      assert.throws(() => copyDatabase(db.path, alias, { overwrite: true }), /two different files/);
      assert.deepEqual(readFileSync(db.path), before);
    }
  } finally {
    db.close();
    dir.remove();
  }
});

test("a failed snapshot preserves the previous destination and removes staging files", () => {
  const { db, dir } = tempDatabase("copyfailure", { sheet: { name: "" } });
  try {
    db.sheets.sheet.add({ name: "destination data" });
    db.close();
    const before = readFileSync(db.path);
    const corrupt = join(dir.path, "corrupt.db");
    writeFileSync(corrupt, "this is not a SQLite database");
    const files = readdirSync(dir.path).sort();
    assert.throws(() => copyDatabase(corrupt, db.path, { overwrite: true }), code("sqlite"));
    assert.deepEqual(readFileSync(db.path), before);
    assert.deepEqual(readdirSync(dir.path).sort(), files);
  } finally {
    db.close();
    dir.remove();
  }
});

test("moving a unique constraint is refused before any schema changes or REPLACE writes", () => {
  const { db, dir } = tempDatabase("uniquekey", {
    sheet: { name: "", city: "", _unique: ["name"], _violations: "REPLACE" },
  });
  try {
    db.sheets.sheet.add({ name: "Ada", city: "Boston" });
    db.close();
    const before = readFileSync(db.path);
    assert.throws(() => openDatabase(db.path, {
      newSheet: { value: "" },
      sheet: { name: "", city: "", added: 0, _unique: ["city"], _violations: "REPLACE" },
    }), code("schema-mismatch"));
    assert.deepEqual(readFileSync(db.path), before);
    const reopened = openDatabase(db.path, {
      sheet: { city: "", name: "", _unique: ["name"], _violations: "REPLACE" },
    });
    try {
      assert.deepEqual(reopened.sheetNames, ["sheet"]);
      assert.equal(reopened.sheets.sheet.fetchOne()?.name, "Ada");
    } finally { reopened.close(); }
  } finally {
    db.close();
    dir.remove();
  }
});

test("constraint comparison ignores commas and UNIQUE inside quoted defaults and nested expressions", () => {
  const a = `CREATE TABLE t ("name" TEXT DEFAULT 'hello, unique(' UNIQUE ON CONFLICT FAIL, "city" TEXT DEFAULT (printf('%s,%s', 'a', 'b')))`;
  const b = `CREATE TABLE t ("city" TEXT, "name" TEXT UNIQUE ON CONFLICT FAIL)`;
  const moved = `CREATE TABLE t ("city" TEXT UNIQUE ON CONFLICT FAIL, "name" TEXT)`;
  assert.equal(extractTableConstraints(a), extractTableConstraints(b));
  assert.notEqual(extractTableConstraints(a), extractTableConstraints(moved));
});

for (const operation of ["add", "update", "set"] as const) {
  test(`ROLLBACK conflict during ${operation} preserves the error and allows a new manual transaction`, () => {
    const { db, dir } = tempDatabase("rollbackconflict", {
      sheet: { name: "", _unique: ["name"], _violations: "ROLLBACK" },
    });
    try {
      const sheet = db.sheets.sheet;
      sheet.add({ name: "Ada" }, { name: "Ben" });
      db.begin();
      sheet.add({ name: "pending" });
      assert.throws(() => {
        if (operation === "add") sheet.add({ name: "Ada" });
        if (operation === "update") sheet.update({ ...sheet.fetch()[1], name: "Ada" });
        if (operation === "set") sheet.set("name", "Ada");
      }, (error: unknown) => error instanceof MudletDbError && error.code === "sqlite" && /UNIQUE constraint failed/.test(error.message));
      assert.equal(db.inTransaction, false);
      assert.deepEqual(sheet.fetch().map(row => row.name), ["Ada", "Ben"]);
      db.begin();
      sheet.add({ name: "after recovery" });
      db.commit();
      assert.equal(sheet.count(), 3);
    } finally {
      db.close();
      dir.remove();
    }
  });
}

test("a caught ROLLBACK conflict cannot commit later writes from an aborted callback", () => {
  const { db, dir } = tempDatabase("rollbackcallback", {
    sheet: { name: "", _unique: ["name"], _violations: "ROLLBACK" },
  });
  try {
    const sheet = db.sheets.sheet;
    sheet.add({ name: "Ada" });
    assert.throws(() => db.transaction(() => {
      sheet.add({ name: "pending" });
      assert.throws(() => db.transaction(() => sheet.add({ name: "Ada" })), /UNIQUE constraint failed/);
      assert.equal(db.inTransaction, false);
      assert.throws(() => sheet.add({ name: "must not autocommit" }), code("transaction"));
    }), code("transaction"));
    assert.equal(db.inTransaction, false);
    assert.deepEqual(sheet.fetch().map(row => row.name), ["Ada"]);
    db.transaction(() => sheet.add({ name: "recovered" }));
    assert.equal(sheet.count(), 2);
  } finally {
    db.close();
    dir.remove();
  }
});

test("manual commit and rollback cannot end a transaction callback", () => {
  const { db, dir } = tempDatabase("manualinsidecallback", { sheet: { name: "" } });
  try {
    for (const operation of ["commit", "rollback"] as const) {
      assert.throws(() => db.transaction(() => {
        db.sheets.sheet.add({ name: "pending" });
        db[operation]();
      }), code("transaction"));
      assert.equal(db.sheets.sheet.count(), 0);
      assert.equal(db.inTransaction, false);
    }
  } finally {
    db.close();
    dir.remove();
  }
});

test("preserved columns support fetch-edit-update and mergeUnique but unknown columns still fail", () => {
  const { db, dir } = tempDatabase("extracolumns", {
    sheet: { name: "", city: "", score: 0, _unique: ["name"] },
  });
  try {
    db.sheets.sheet.add({ name: "Ada", city: "Boston", score: 7 });
    db.close();
    const narrower = openDatabase(db.path, { sheet: { name: "", _unique: ["name"] } });
    try {
      const sheet = narrower.sheets.sheet;
      const row = sheet.fetchOne()!;
      sheet.update({ ...row, name: "Alice" });
      sheet.mergeUnique([{ name: "Alice" }, { name: "Ben" }]);
      assert.deepEqual(sheet.fetchOne(), { _row_id: 1, name: "Alice", city: "Boston", score: 7 });
      sheet.set(sheet.field("city"), "London");
      sheet.update({ ...sheet.fetchOne()!, city: null } as never);
      assert.equal((sheet.fetchOne() as Record<string, unknown>).city, null);
      assert.throws(() => sheet.update({ ...sheet.fetchOne()!, typo: "bad" } as never), code("schema"));
      assert.throws(() => sheet.add({ name: "Carol", typo: "bad" } as never), code("schema"));
    } finally { narrower.close(); }
  } finally {
    db.close();
    dir.remove();
  }
});
