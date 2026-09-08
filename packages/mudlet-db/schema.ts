// Schema declarations, the DDL they produce, and how an existing file is
// reconciled with them. The DDL follows DB.lua's generator closely enough that
// Mudlet's own migration recognises the tables and indexes this package makes.

import { MudletDbError } from "./errors.ts";
import { assertIdentifier, mudletIndexName, quoteIdentifier } from "./names.ts";
import {
  CURRENT_TIMESTAMP,
  epochSeconds,
  isTimestampColumn,
  timestamp,
  type ColumnKind,
  type SqlValue,
  type TimestampColumn,
} from "./values.ts";

// ---------------------------------------------------------------------------
// Declarations
// ---------------------------------------------------------------------------

/** What a sheet with a unique constraint does with a row that violates it. */
export type Violations = "FAIL" | "IGNORE" | "REPLACE" | "ABORT" | "ROLLBACK";

/**
 * Columns to index: one column name, a list of names (one index each), or a
 * list holding lists for compound indexes. `_unique` takes the same shapes.
 */
export type IndexSpec = string | ReadonlyArray<string | ReadonlyArray<string>>;

/**
 * A column is declared by its default: a string makes a text column, a number
 * a numeric one, `null` a typeless column that defaults to NULL, and
 * {@link timestamp} a timestamp column.
 */
export type ColumnSpec = string | number | null | TimestampColumn;

export interface SheetOptions {
  readonly _index?: IndexSpec;
  readonly _unique?: IndexSpec;
  readonly _violations?: Violations;
}

/** `{ name: "", level: 0, _index: ["name"] }`: columns by default, options by underscore. */
export type KeyedSheetSpec = {
  readonly [column: string]: ColumnSpec | IndexSpec | Violations | undefined;
};

/** `["name", "city"]`: text columns defaulting to `""`, with no options. */
export type ListSheetSpec = ReadonlyArray<string>;

export type SheetSpec = KeyedSheetSpec | ListSheetSpec;

export type DatabaseSpec = { readonly [sheet: string]: SheetSpec };

/** The column names a sheet spec declares (option keys excluded). */
export type ColumnNames<S extends SheetSpec> = S extends ListSheetSpec
  ? S[number]
  : Exclude<keyof S & string, `_${string}`>;

/** The declaration behind one column of a spec. */
export type ColumnSpecOf<S extends SheetSpec, K extends string> = S extends ListSheetSpec
  ? string
  : K extends keyof S
    ? S[K]
    : never;

/** What a fetched row holds for a column declared as `C`. */
export type ReadValue<C> = C extends TimestampColumn
  ? Date | null
  : C extends string
    ? string | null
    : C extends number
      ? number | null
      : SqlValue;

/** What can be stored into a column declared as `C`. */
export type WriteValue<C> = C extends TimestampColumn
  ? Date | typeof CURRENT_TIMESTAMP | null
  : C extends string
    ? string | number | null
    : C extends number
      ? number | null
      : string | number | bigint | Uint8Array | null;

// ---------------------------------------------------------------------------
// Normalized form
// ---------------------------------------------------------------------------

export interface NormalizedColumn {
  readonly name: string;
  readonly kind: ColumnKind;
  readonly defaultValue: ColumnSpec;
}

/** One unique constraint: a single column (column-level UNIQUE) or a list (table-level). */
export type UniqueEntry = string | readonly string[];

export interface NormalizedSheet {
  readonly name: string;
  readonly columns: readonly NormalizedColumn[];
  readonly indexes: ReadonlyArray<readonly string[]>;
  readonly unique: readonly UniqueEntry[];
  readonly violations: Violations;
}

export type NormalizedSchema = ReadonlyMap<string, NormalizedSheet>;

const VIOLATIONS: readonly Violations[] = ["FAIL", "IGNORE", "REPLACE", "ABORT", "ROLLBACK"];
const OPTION_KEYS = new Set(["_index", "_unique", "_violations"]);
export const ROW_ID = "_row_id";

export function columnKind(spec: ColumnSpec): ColumnKind {
  if (typeof spec === "string") return "text";
  if (typeof spec === "number") return "number";
  if (spec === null) return "any";
  if (isTimestampColumn(spec)) return "timestamp";
  throw new MudletDbError("schema", `a column default must be a string, number, null or timestamp()`);
}

function schemaError(sheet: string, message: string): MudletDbError {
  return new MudletDbError("schema", `sheet ${JSON.stringify(sheet)}: ${message}`);
}

function normalizeIndexSpec(
  sheet: string,
  option: "_index" | "_unique",
  spec: unknown,
  columns: ReadonlySet<string>,
): Array<string | string[]> {
  if (spec === undefined || spec === null || spec === false) return [];
  const entries: unknown[] = typeof spec === "string" ? [spec] : Array.isArray(spec) ? spec : [];
  if (typeof spec !== "string" && !Array.isArray(spec)) {
    throw schemaError(sheet, `${option} must be a column name or a list, not ${typeof spec}`);
  }
  const result: Array<string | string[]> = [];
  for (const entry of entries) {
    const names = typeof entry === "string" ? [entry] : Array.isArray(entry) ? entry : null;
    if (names === null) {
      throw schemaError(sheet, `${option} entries must be column names or lists of them`);
    }
    if (names.length === 0) throw schemaError(sheet, `${option} has an entry with no column names`);
    for (const name of names) {
      if (typeof name !== "string") {
        throw schemaError(sheet, `${option} names must be strings, not ${typeof name}`);
      }
      if (name === ROW_ID) {
        throw schemaError(
          sheet,
          `${option} names ${ROW_ID}, which every sheet already keys uniquely; name the columns you meant`,
        );
      }
      if (!columns.has(name)) {
        throw schemaError(sheet, `${option} names ${JSON.stringify(name)}, which is not one of its columns`);
      }
    }
    result.push(typeof entry === "string" ? entry : [...(names as string[])]);
  }
  return result;
}

/**
 * Check a sheet declaration and put it in the form the DDL and query builders
 * read. Mudlet skips an index or unique entry it cannot honour and prints a
 * warning; here every such fault is an error, since a sheet quietly built
 * without a rule is the outcome the diagnostics exist to prevent.
 */
export function normalizeSheet(name: string, spec: SheetSpec): NormalizedSheet {
  assertIdentifier("sheet", name);
  const columns: NormalizedColumn[] = [];
  const options: { _index?: unknown; _unique?: unknown; _violations?: unknown } = {};

  if (Array.isArray(spec)) {
    for (const column of spec as ListSheetSpec) {
      if (typeof column !== "string") {
        throw schemaError(name, `a sheet given as a list takes column names, not ${typeof column}`);
      }
      assertIdentifier("column", column);
      columns.push({ name: column, kind: "text", defaultValue: "" });
    }
  } else if (typeof spec === "object" && spec !== null) {
    for (const [key, value] of Object.entries(spec)) {
      if (key.startsWith("_")) {
        if (!OPTION_KEYS.has(key)) {
          throw schemaError(name, `${key} is not a sheet option (_index, _unique, _violations)`);
        }
        (options as Record<string, unknown>)[key] = value;
        continue;
      }
      if (value === undefined) continue;
      assertIdentifier("column", key);
      const kind = columnKind(value as ColumnSpec);
      columns.push({ name: key, kind, defaultValue: value as ColumnSpec });
    }
  } else {
    throw schemaError(name, "a sheet must be an object of column defaults or a list of column names");
  }

  if (columns.length === 0) throw schemaError(name, "declares no columns");
  const names = new Set(columns.map((column) => column.name));
  if (names.size !== columns.length) throw schemaError(name, "declares a column twice");

  let violations: Violations = "FAIL";
  if (options._violations !== undefined) {
    if (!VIOLATIONS.includes(options._violations as Violations)) {
      throw schemaError(
        name,
        `_violations must be one of ${VIOLATIONS.join(", ")}; received ${JSON.stringify(options._violations)}`,
      );
    }
    violations = options._violations as Violations;
  }

  const indexes = normalizeIndexSpec(name, "_index", options._index, names).map((entry) =>
    typeof entry === "string" ? [entry] : entry,
  );
  const unique = normalizeIndexSpec(name, "_unique", options._unique, names);
  return { name, columns, indexes, unique, violations };
}

export function normalizeSchema(spec: DatabaseSpec): NormalizedSchema {
  if (typeof spec !== "object" || spec === null) {
    throw new MudletDbError("schema", "the schema must be an object of sheets");
  }
  const sheets = new Map<string, NormalizedSheet>();
  for (const [name, sheet] of Object.entries(spec)) {
    sheets.set(name, normalizeSheet(name, sheet));
  }
  if (sheets.size === 0) throw new MudletDbError("schema", "the schema declares no sheets");
  return sheets;
}

// ---------------------------------------------------------------------------
// DDL
// ---------------------------------------------------------------------------

function sqlLiteral(text: string): string {
  return `'${text.replaceAll("'", "''")}'`;
}

/** DB.lua `_sql_type`: the declared type is decided by the default's kind. */
export function columnSqlType(column: NormalizedColumn): string {
  switch (column.kind) {
    case "text":
      return "TEXT";
    case "number":
      return "REAL";
    case "timestamp":
      return "INTEGER";
    case "any":
      return "NULL";
  }
}

/** DB.lua `_sql_convert`: the DEFAULT clause value. */
export function columnSqlDefault(column: NormalizedColumn): string {
  const value = column.defaultValue;
  if (value === null) return "NULL";
  if (typeof value === "string") return sqlLiteral(value);
  if (typeof value === "number") return String(value);
  if (value.defaultValue === null) return "NULL";
  if (value.defaultValue === CURRENT_TIMESTAMP) return "CURRENT_TIMESTAMP";
  return String(epochSeconds(value.defaultValue));
}

/**
 * The CREATE TABLE statement, in DB.lua's layout: an autoincrementing
 * `_row_id`, then each column typed by its default, a column-level UNIQUE for
 * single unique columns and table-level UNIQUE(...) clauses for compound ones,
 * all carrying the sheet's ON CONFLICT resolution.
 */
export function buildCreateTableSql(sheet: NormalizedSheet): string {
  const onConflict = `ON CONFLICT ${sheet.violations}`;
  const singleUnique = new Set(sheet.unique.filter((entry): entry is string => typeof entry === "string"));
  const chunks = [`${quoteIdentifier(ROW_ID)} INTEGER PRIMARY KEY AUTOINCREMENT`];
  for (const column of sheet.columns) {
    let sql = `${quoteIdentifier(column.name)} ${columnSqlType(column)} NULL DEFAULT ${columnSqlDefault(column)}`;
    if (singleUnique.has(column.name)) sql += ` UNIQUE ${onConflict}`;
    chunks.push(sql);
  }
  for (const entry of sheet.unique) {
    if (typeof entry === "string") continue;
    chunks.push(`UNIQUE(${entry.map(quoteIdentifier).join(", ")}) ${onConflict}`);
  }
  return `CREATE TABLE ${sheet.name} (${chunks.join(", ")})`;
}

/** DB.lua `_migrate`'s ALTER TABLE for a column the file lacks. */
export function buildAddColumnSql(sheet: string, column: NormalizedColumn): string {
  return `ALTER TABLE ${sheet} ADD COLUMN ${quoteIdentifier(column.name)} ${columnSqlType(column)} NULL DEFAULT ${columnSqlDefault(column)}`;
}

/** DB.lua `_migrate_indexes`: the name and statement of one wanted index. */
export function buildCreateIndexSql(sheet: string, columns: readonly string[]): { name: string; sql: string } {
  const name = mudletIndexName(sheet, columns);
  const list = columns.map((column) => quoteIdentifier(column.toLowerCase())).join(",");
  return { name, sql: `CREATE INDEX IF NOT EXISTS ${name} ON ${sheet} (${list});` };
}

// ---------------------------------------------------------------------------
// Reconciling with an existing file
// ---------------------------------------------------------------------------

function normalizeSql(sql: string): string {
  return sql.toLowerCase().replaceAll(/[\r\n]/g, " ").replaceAll(/\s+/g, " ").trim();
}

function blankQuoted(text: string): string {
  return text.replaceAll(/"[^"]*"|'[^']*'/g, (quoted) => " ".repeat(quoted.length));
}

/**
 * The UNIQUE constraints a CREATE TABLE statement carries, normalized and
 * sorted for migration comparison. Unlike Mudlet's extractor, this retains
 * the column name of an inline UNIQUE constraint, so moving it is a change.
 */
export function extractTableConstraints(sql: string | null | undefined): string {
  if (!sql) return "";
  const normalized = normalizeSql(sql);
  const body = /create\s+table\s+[\w"]+\s*\((.+)\)/.exec(normalized)?.[1];
  if (!body) return "";
  const blank = blankQuoted(body);
  const definitions: string[] = [];
  let startOfDefinition = 0;
  let depth = 0;
  for (let i = 0; i < blank.length; i++) {
    if (blank[i] === "(") depth++;
    if (blank[i] === ")") depth--;
    if (blank[i] === "," && depth === 0) {
      definitions.push(body.slice(startOfDefinition, i).trim());
      startOfDefinition = i + 1;
    }
  }
  definitions.push(body.slice(startOfDefinition).trim());
  const constraints: string[] = [];
  for (const definition of definitions) {
    const searchable = blankQuoted(definition);
    let position = 0;
    for (;;) {
      const start = searchable.indexOf("unique", position);
      if (start < 0) break;
      const stop = start + "unique".length;
      position = stop;
      const before = start > 0 ? searchable[start - 1] : " ";
      const after = searchable[stop] ?? " ";
      if (/\w/.test(before) || /\w/.test(after)) continue;
      let constraint = "unique";
      const columns = /^\s*\([^)]+\)/.exec(definition.slice(position));
      if (columns) {
        constraint += columns[0];
        position += columns[0].length;
      } else {
        const column = /^(?:"([^"]+)"|(\w+))/.exec(definition);
        if (column) constraint += `(${quoteIdentifier(column[1] ?? column[2])})`;
      }
      const conflict = /^\s+on\s+conflict\s+\w+/.exec(definition.slice(position));
      if (conflict) {
        constraint += conflict[0];
        position += conflict[0].length;
      }
      constraints.push(constraint);
    }
  }
  constraints.sort();
  return constraints.join("|");
}

/** What the file says about a table, gathered by the caller from PRAGMA and sqlite_master. */
export interface ExistingTable {
  readonly sql: string;
  readonly columns: ReadonlyArray<{ readonly name: string; readonly type: string }>;
  readonly indexes: ReadonlyArray<{ readonly name: string; readonly sql: string | null }>;
}

export interface SheetPlan {
  /** Statements to run, in order. */
  readonly statements: readonly string[];
  readonly created: boolean;
  readonly addedColumns: readonly string[];
  readonly addedIndexes: readonly string[];
  /** Differences left alone, each stated once so the caller can surface them. */
  readonly warnings: readonly string[];
}

/**
 * Decide how to bring one table in line with its sheet. Supported: creating
 * the table, adding missing columns (with their defaults) and creating missing
 * indexes. Everything else the file carries is kept: extra columns, extra or
 * unique indexes, and differing column types are reported, never rewritten. A
 * change to the UNIQUE constraints or the conflict resolution would need the
 * table rebuilt, which DB.lua does by copying through a temporary table; this
 * package refuses it explicitly rather than risk a user's rows.
 */
export function planSheet(sheet: NormalizedSheet, existing: ExistingTable | null): SheetPlan {
  const statements: string[] = [];
  const warnings: string[] = [];
  const addedColumns: string[] = [];
  const addedIndexes: string[] = [];
  let created = false;

  if (existing === null) {
    statements.push(buildCreateTableSql(sheet));
    created = true;
  } else {
    const expected = extractTableConstraints(buildCreateTableSql(sheet));
    const actual = extractTableConstraints(existing.sql);
    if (expected !== actual) {
      throw new MudletDbError(
        "schema-mismatch",
        `sheet ${JSON.stringify(sheet.name)} exists with different unique constraints or conflict ` +
          `resolution than the schema declares (file: ${JSON.stringify(actual)}, schema: ` +
          `${JSON.stringify(expected)}). Rebuilding a table is not supported; keep the file's ` +
          `_unique/_violations, or migrate the rows into a new sheet yourself.`,
      );
    }
    const present = new Map(existing.columns.map((column) => [column.name.toLowerCase(), column]));
    for (const column of sheet.columns) {
      const found = present.get(column.name.toLowerCase());
      if (!found) {
        statements.push(buildAddColumnSql(sheet.name, column));
        addedColumns.push(column.name);
        continue;
      }
      const wanted = columnSqlType(column);
      if (found.type.toUpperCase() !== wanted) {
        warnings.push(
          `sheet ${sheet.name}: column ${column.name} is ${found.type || "untyped"} in the file but the ` +
            `schema declares ${wanted}; the stored type is kept`,
        );
      }
    }
    const declared = new Set(sheet.columns.map((column) => column.name.toLowerCase()));
    for (const column of existing.columns) {
      const lower = column.name.toLowerCase();
      if (lower !== ROW_ID && !declared.has(lower)) {
        warnings.push(
          `sheet ${sheet.name}: column ${column.name} exists in the file but not in the schema; ` +
            `it is kept and its values are returned untyped`,
        );
      }
    }
  }

  const wantedIndexes = new Set<string>();
  for (const columns of sheet.indexes) {
    const { name, sql } = buildCreateIndexSql(sheet.name, columns);
    wantedIndexes.add(name.toLowerCase());
    const already = existing?.indexes.some((index) => index.name.toLowerCase() === name.toLowerCase());
    if (!already) {
      statements.push(sql);
      addedIndexes.push(name);
    }
  }
  for (const index of existing?.indexes ?? []) {
    if (index.sql === null || wantedIndexes.has(index.name.toLowerCase())) continue;
    const unique = /^\s*create\s+unique\s+index/i.test(index.sql);
    warnings.push(
      `sheet ${sheet.name}: index ${index.name} exists in the file but not in _index; it is kept` +
        (unique ? " (Mudlet no longer creates unique indexes and would drop it)" : ""),
    );
  }

  return { statements, created, addedColumns, addedIndexes, warnings };
}

/**
 * A sheet declaration recovered from a table this package did not declare,
 * using DB.lua's type mapping in reverse: TEXT is text, REAL is a number,
 * INTEGER is a timestamp (the only INTEGER column DB.lua makes besides
 * `_row_id`), and anything else is untyped.
 */
export function discoverSheet(name: string, existing: ExistingTable): NormalizedSheet {
  const columns: NormalizedColumn[] = [];
  for (const column of existing.columns) {
    if (column.name === ROW_ID) continue;
    const type = column.type.toUpperCase();
    const kind: ColumnKind =
      type === "TEXT" ? "text" : type === "REAL" ? "number" : type === "INTEGER" ? "timestamp" : "any";
    const defaultValue: ColumnSpec =
      kind === "text" ? "" : kind === "number" ? 0 : kind === "timestamp" ? timestamp() : null;
    columns.push({ name: column.name, kind, defaultValue });
  }
  return { name, columns, indexes: [], unique: [], violations: "FAIL" };
}
