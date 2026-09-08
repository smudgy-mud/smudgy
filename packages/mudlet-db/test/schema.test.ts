// Naming, DDL and constraint extraction, checked against the strings DB.lua
// produces (its unit specs at DB_spec.lua "Tests db's internal SQL helpers").

import assert from "node:assert/strict";
import test from "node:test";
import {
  buildCreateIndexSql,
  buildCreateTableSql,
  CURRENT_TIMESTAMP,
  extractTableConstraints,
  MudletDbError,
  mudletDatabaseFileName,
  mudletSafeName,
  normalizeSheet,
  timestamp,
} from "../index.ts";
import { planSheet } from "../schema.ts";

test("safe_name keeps letters only and lower-cases, as the pinned DB.lua does", () => {
  assert.equal(mudletSafeName("my_database"), "mydatabase");
  assert.equal(mudletSafeName("../../../../etc/passwd"), "etcpasswd");
  assert.equal(mudletSafeName("combat_log2"), "combatlog");
  assert.equal(mudletSafeName("DB Internals Testing Only"), "dbinternalstestingonly");
  assert.equal(mudletDatabaseFileName("peoplettestingonly"), "Database_peoplettestingonly.db");
  assert.throws(() => mudletDatabaseFileName("12345"), (error: unknown) => (error as MudletDbError).code === "schema");
});

test("_build_create_table_sql: an autoincrementing _row_id and columns typed by default", () => {
  const sheet = normalizeSheet("people", { name: "", level: 0, seen: timestamp(CURRENT_TIMESTAMP), house: null });
  assert.equal(
    buildCreateTableSql(sheet),
    'CREATE TABLE people ("_row_id" INTEGER PRIMARY KEY AUTOINCREMENT, ' +
      `"name" TEXT NULL DEFAULT '', "level" REAL NULL DEFAULT 0, ` +
      '"seen" INTEGER NULL DEFAULT CURRENT_TIMESTAMP, "house" NULL NULL DEFAULT NULL)',
  );
});

test("_build_create_table_sql: unique columns and compound constraints carry the conflict resolution", () => {
  const single = normalizeSheet("enemies", { name: "", city: "", _unique: ["name"], _violations: "IGNORE" });
  assert.match(buildCreateTableSql(single), /"name" TEXT NULL DEFAULT '' UNIQUE ON CONFLICT IGNORE, "city"/);
  const asString = normalizeSheet("enemies", { name: "", _unique: "name" });
  assert.match(buildCreateTableSql(asString), /"name" TEXT NULL DEFAULT '' UNIQUE ON CONFLICT FAIL\)/);
  const compound = normalizeSheet("kills", { name: "", area: "", _unique: [["name", "area"]], _violations: "REPLACE" });
  assert.match(buildCreateTableSql(compound), /, UNIQUE\("name", "area"\) ON CONFLICT REPLACE\)$/);
  const plain = normalizeSheet("kills", { name: "", area: "" });
  assert.doesNotMatch(buildCreateTableSql(plain), /UNIQUE/);
});

test("a timestamp default of a Date is stored as its epoch, a null one as NULL", () => {
  const sheet = normalizeSheet("t", { epoched: timestamp(new Date(1748288082 * 1000)), niled: timestamp() });
  assert.match(buildCreateTableSql(sheet), /"epoched" INTEGER NULL DEFAULT 1748288082, "niled" INTEGER NULL DEFAULT NULL/);
});

test("_index_name and _sql_columns: index names and lower-cased quoted columns", () => {
  assert.deepEqual(buildCreateIndexSql("kills", ["Name", "area"]), {
    name: "idx_kills_c_Name_area",
    sql: 'CREATE INDEX IF NOT EXISTS idx_kills_c_Name_area ON kills ("name","area");',
  });
  assert.equal(buildCreateIndexSql("enemies", ["city"]).sql, 'CREATE INDEX IF NOT EXISTS idx_enemies_c_city ON enemies ("city");');
});

test("a sheet given as a list of names makes text columns", () => {
  const sheet = normalizeSheet("friends", ["name", "city", "notes"]);
  assert.deepEqual(
    sheet.columns.map((column) => [column.name, column.kind, column.defaultValue]),
    [["name", "text", ""], ["city", "text", ""], ["notes", "text", ""]],
  );
});

test("schema faults are refused with a message naming the fault", () => {
  const schema = (error: unknown) => error instanceof MudletDbError && error.code === "schema";
  assert.throws(() => normalizeSheet("s", { name: "", _violations: "MAYBE" as never }), schema);
  assert.throws(() => normalizeSheet("s", { name: "", _violations: 7 as never }), schema);
  assert.throws(() => normalizeSheet("s", { name: "", _unique: [[7]] as never }), schema);
  assert.throws(() => normalizeSheet("s", { name: "", _unique: 7 as never }), schema);
  assert.throws(() => normalizeSheet("s", { name: "", _index: 7 as never }), schema);
  assert.throws(() => normalizeSheet("s", { name: "", _index: [true] as never }), schema);
  assert.throws(() => normalizeSheet("s", { name: "", _index: ["city"] }), /names "city", which is not one of its columns/);
  assert.throws(() => normalizeSheet("s", { name: "", _unique: ["_row_id"] }), /_row_id/);
  assert.throws(() => normalizeSheet("s", { name: "", _index: [[]] }), /no column names/);
  assert.throws(() => normalizeSheet("s", { name: "", _other: 1 } as never), /_other is not a sheet option/);
  assert.throws(() => normalizeSheet("s", {}), /declares no columns/);
  assert.throws(() => normalizeSheet("bad name", { name: "" }), /not a plain identifier/);
  assert.throws(() => normalizeSheet("s", { "bad-col": "" }), /not a plain identifier/);
  assert.throws(() => normalizeSheet("s", { flag: true } as never), /string, number, null or timestamp/);
});

test("a falsy _index means no indexes", () => {
  assert.deepEqual(normalizeSheet("s", { name: "", _index: false as never }).indexes, []);
});

test("_extract_table_constraints: the DB_spec cases, retaining inline column names", () => {
  assert.equal(extractTableConstraints(""), "");
  assert.equal(extractTableConstraints(null), "");
  assert.equal(extractTableConstraints("SELECT * FROM x"), "");
  assert.equal(extractTableConstraints('CREATE TABLE t ("_row_id" INTEGER PRIMARY KEY, "name" TEXT)'), "");
  assert.equal(
    extractTableConstraints('CREATE TABLE t ("name" TEXT NULL DEFAULT "" UNIQUE ON CONFLICT REPLACE)'),
    'unique("name") on conflict replace',
  );
  assert.equal(
    extractTableConstraints('CREATE TABLE t ("a" TEXT, "b" TEXT, UNIQUE("a", "b") ON CONFLICT FAIL)'),
    'unique("a", "b") on conflict fail',
  );
  assert.equal(
    extractTableConstraints('CREATE\n TABLE  t ("a" TEXT   UNIQUE\r\n ON   CONFLICT  Ignore)'),
    'unique("a") on conflict ignore',
  );
  const one = 'CREATE TABLE t ("a" TEXT UNIQUE ON CONFLICT FAIL, UNIQUE("b", "c") ON CONFLICT REPLACE)';
  const other = 'CREATE TABLE t (UNIQUE("b", "c") ON CONFLICT REPLACE, "a" TEXT UNIQUE ON CONFLICT FAIL)';
  assert.equal(extractTableConstraints(one), extractTableConstraints(other));
  assert.notEqual(
    extractTableConstraints('CREATE TABLE t ("a" TEXT UNIQUE ON CONFLICT FAIL)'),
    extractTableConstraints('CREATE TABLE t ("a" TEXT UNIQUE ON CONFLICT REPLACE)'),
  );
  assert.equal(
    extractTableConstraints('CREATE TABLE t ("a" TEXT UNIQUE ON CONFLICT FAIL, "added" TEXT)'),
    extractTableConstraints('CREATE TABLE t ("a" TEXT UNIQUE ON CONFLICT FAIL)'),
  );
  assert.equal(extractTableConstraints('CREATE TABLE t ("a" TEXT UNIQUE)'), 'unique("a")');
  assert.notEqual(extractTableConstraints('CREATE TABLE t ("a" TEXT UNIQUE)'), extractTableConstraints('CREATE TABLE t ("a" TEXT UNIQUE ON CONFLICT FAIL)'));
  assert.equal(extractTableConstraints('CREATE TABLE t ("unique_id" TEXT, "unique" TEXT DEFAULT "unique")'), "");
});

test("the constraints this package writes read back equal to Mudlet's double-quoted form", () => {
  const sheet = normalizeSheet("enemies", { name: "", city: "", _unique: ["name", ["name", "city"]], _violations: "REPLACE" });
  const mudlet =
    'CREATE TABLE enemies ("_row_id" INTEGER PRIMARY KEY AUTOINCREMENT, "city" TEXT NULL DEFAULT "", ' +
    '"name" TEXT NULL DEFAULT "" UNIQUE ON CONFLICT REPLACE, UNIQUE("name", "city") ON CONFLICT REPLACE)';
  assert.equal(extractTableConstraints(buildCreateTableSql(sheet)), extractTableConstraints(mudlet));
});

test("planSheet creates, adds columns and indexes, keeps extras, and refuses constraint changes", () => {
  const sheet = normalizeSheet("people", { name: "", city: "", level: 0, _index: ["city"], _unique: ["name"] });
  const fresh = planSheet(sheet, null);
  assert.equal(fresh.created, true);
  assert.deepEqual(fresh.addedIndexes, ["idx_people_c_city"]);
  assert.equal(fresh.statements.length, 2);

  const existing = {
    sql: buildCreateTableSql(normalizeSheet("people", { name: "", city: "", _unique: ["name"] })),
    columns: [
      { name: "_row_id", type: "INTEGER" },
      { name: "name", type: "TEXT" },
      { name: "city", type: "TEXT" },
      { name: "legacy", type: "TEXT" },
    ],
    indexes: [{ name: "idx_people_c_legacy", sql: 'CREATE INDEX idx_people_c_legacy ON people ("legacy")' }],
  };
  const plan = planSheet(sheet, existing);
  assert.equal(plan.created, false);
  assert.deepEqual(plan.addedColumns, ["level"]);
  assert.deepEqual(plan.addedIndexes, ["idx_people_c_city"]);
  assert.deepEqual(plan.statements, [
    'ALTER TABLE people ADD COLUMN "level" REAL NULL DEFAULT 0',
    'CREATE INDEX IF NOT EXISTS idx_people_c_city ON people ("city");',
  ]);
  assert.equal(plan.warnings.length, 2);
  assert.match(plan.warnings[0], /column legacy exists in the file but not in the schema/);
  assert.match(plan.warnings[1], /index idx_people_c_legacy exists in the file but not in _index/);

  const changed = normalizeSheet("people", { name: "", city: "", level: 0, _unique: ["name"], _violations: "REPLACE" });
  assert.throws(
    () => planSheet(changed, existing),
    (error: unknown) => error instanceof MudletDbError && error.code === "schema-mismatch" && /Rebuilding a table is not supported/.test(error.message),
  );
});
