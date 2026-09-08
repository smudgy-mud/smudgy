// Opening a file laid out exactly as DB.lua lays one out.
//
// No database exported by a running Mudlet is in the research corpus, so this
// fixture is SYNTHETIC: its DDL and DML are the literal statements DB.lua's
// generators emit for the schema below (double-quoted string defaults, an
// unquoted table name, `datetime('now')` and `datetime('<epoch>', 'unixepoch')`
// for timestamps, Mudlet's index name). It proves this package reads what
// DB.lua writes; it does not stand in for a file produced by Mudlet itself.

import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { eq, extractTableConstraints, inspectDatabase, openDatabase, timestamp, CURRENT_TIMESTAMP, MudletDbError } from "../index.ts";
import { tempDir } from "./support.ts";

const MUDLET_DDL = [
  // db:_build_create_table_sql for { name = "", city = "", kills = 0, seen = db:Timestamp("CURRENT_TIMESTAMP"),
  //   last = db:Timestamp(nil), _index = { "city" }, _unique = { "name" }, _violations = "REPLACE" }
  'CREATE TABLE people ("_row_id" INTEGER PRIMARY KEY AUTOINCREMENT, "city" TEXT NULL DEFAULT "", ' +
    '"name" TEXT NULL DEFAULT "" UNIQUE ON CONFLICT REPLACE, "kills" REAL NULL DEFAULT 0, ' +
    '"seen" INTEGER NULL DEFAULT CURRENT_TIMESTAMP, "last" INTEGER NULL DEFAULT NULL)',
  'CREATE INDEX IF NOT EXISTS idx_people_c_city ON people ("city");',
  // db:add via _sql_fields/_sql_values
  `INSERT INTO people ("name","city","kills","seen","last") VALUES ('Bob','Ankh''Morpork',3,datetime('now'),datetime('1748288082', 'unixepoch'))`,
  `INSERT INTO people ("name","city") VALUES ('Carrot','Ankh''Morpork')`,
];

function writeMudletFixture(directory: string): string {
  const path = join(directory, "Database_mudletfixture.db");
  const raw = new DatabaseSync(path);
  for (const sql of MUDLET_DDL) raw.exec(sql);
  raw.close();
  return path;
}

test("a DB.lua-written file opens with the same schema and needs no changes", () => {
  const dir = tempDir();
  try {
    const path = writeMudletFixture(dir.path);
    const db = openDatabase({ name: "Mudlet Fixture", directory: dir.path }, {
      people: { name: "", city: "", kills: 0, seen: timestamp(CURRENT_TIMESTAMP), last: timestamp(), _index: ["city"], _unique: ["name"], _violations: "REPLACE" },
    });
    try {
      assert.equal(db.path, path);
      assert.deepEqual(db.report, { path, createdSheets: [], addedColumns: [], addedIndexes: [], warnings: [] });
      const rows = db.sheets.people.fetch(undefined, { orderBy: db.sheets.people.fields._row_id });
      assert.equal(rows.length, 2);
      assert.deepEqual([rows[0]._row_id, rows[0].name, rows[0].city, rows[0].kills], [1, "Bob", "Ankh'Morpork", 3]);
      assert.ok(rows[0].seen instanceof Date);
      assert.deepEqual(rows[0].last, new Date("2025-05-26T19:34:42Z"));
      assert.deepEqual([rows[1].kills, rows[1].last], [0, null]);

      // Writing through the package keeps DB.lua's conventions: REPLACE on the unique name.
      db.sheets.people.add({ name: "Bob", city: "Lancre", kills: 4 });
      const bob = db.sheets.people.fetchOne(eq(db.sheets.people.fields.name, "Bob"))!;
      assert.equal(bob.city, "Lancre");
      assert.equal(db.sheets.people.count(), 2);
    } finally {
      db.close();
    }

    // What Mudlet would see on its next db:create: identical constraints, and its index by name.
    const inspected = inspectDatabase(path);
    assert.deepEqual(inspected.sheets.map((sheet) => sheet.name), ["people"]);
    assert.equal(extractTableConstraints(inspected.sheets[0].sql), 'unique("name") on conflict replace');
    assert.deepEqual(inspected.sheets[0].indexes.filter((index) => index.sql !== null).map((index) => index.name), ["idx_people_c_city"]);
  } finally {
    dir.remove();
  }
});

test("a DB.lua-written file opens without a schema, typed from its column declarations", () => {
  const dir = tempDir();
  try {
    const path = writeMudletFixture(dir.path);
    const db = openDatabase(path);
    try {
      assert.deepEqual(db.sheetNames, ["people"]);
      const people = db.sheet("people");
      assert.deepEqual(people.columns, ["city", "name", "kills", "seen", "last"]);
      assert.equal(people.fields.seen.kind, "timestamp");
      assert.equal(people.fields.kills.kind, "number");
      assert.equal(people.fields.name.kind, "text");
      const [bob] = people.fetch(eq(people.fields.name, "Bob"));
      assert.ok(bob.seen instanceof Date);
      assert.equal(bob.kills, 3);
    } finally {
      db.close();
    }
  } finally {
    dir.remove();
  }
});

test("a schema that changes the fixture's constraints is refused; one that only adds is applied", () => {
  const dir = tempDir();
  try {
    const path = writeMudletFixture(dir.path);
    assert.throws(
      () => openDatabase(path, { people: { name: "", city: "", kills: 0, seen: timestamp(CURRENT_TIMESTAMP), last: timestamp(), _unique: ["name"] } }),
      (error: unknown) => error instanceof MudletDbError && error.code === "schema-mismatch",
    );
    const db = openDatabase(path, {
      people: { name: "", city: "", kills: 0, seen: timestamp(CURRENT_TIMESTAMP), last: timestamp(), notes: "", _index: ["city", "name"], _unique: ["name"], _violations: "REPLACE" },
    });
    try {
      assert.deepEqual(db.report.addedColumns, ["people.notes"]);
      assert.deepEqual(db.report.addedIndexes, ["idx_people_c_name"]);
      assert.equal(db.sheets.people.fetch()[0].notes, "");
    } finally {
      db.close();
    }
  } finally {
    dir.remove();
  }
});

test("a timestamp column holding text this package cannot read is an explicit stored-value error", () => {
  const dir = tempDir();
  try {
    const path = join(dir.path, "odd.db");
    const raw = new DatabaseSync(path);
    raw.exec('CREATE TABLE t ("_row_id" INTEGER PRIMARY KEY AUTOINCREMENT, "seen" INTEGER NULL DEFAULT NULL)');
    raw.exec(`INSERT INTO t ("seen") VALUES ('yesterday')`);
    raw.close();
    const db = openDatabase(path, { t: { seen: timestamp() } });
    try {
      assert.throws(
        () => db.sheets.t.fetch(),
        (error: unknown) => error instanceof MudletDbError && error.code === "stored-value" && /t\.seen in row 1/.test(error.message),
      );
    } finally {
      db.close();
    }
  } finally {
    dir.remove();
  }
});
