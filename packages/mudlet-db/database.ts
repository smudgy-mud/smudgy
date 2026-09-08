// The database and sheet handles: opening a file against a schema, the row
// operations DB.lua offers on a sheet, and transactions.

import { existsSync } from "node:fs";
import { fromDriver, MudletDbError } from "./errors.ts";
import { mudletDatabasePath, quoteIdentifier } from "./names.ts";
import {
  compileOrder,
  compileQuery,
  eq,
  Expression,
  Field,
  and,
  isNull,
  type OrderOptions,
  type Query,
} from "./query.ts";
import {
  discoverSheet,
  normalizeSchema,
  planSheet,
  ROW_ID,
  type ColumnNames,
  type ColumnSpecOf,
  type DatabaseSpec,
  type ExistingTable,
  type NormalizedSheet,
  type ReadValue,
  type SheetSpec,
  type WriteValue,
} from "./schema.ts";
import { openConnection, StatementCache, type Connection, type RawRow } from "./sqlite.ts";
import {
  readValue,
  writeValue,
  type ColumnKind,
  type CURRENT_TIMESTAMP,
  type SqlParam,
} from "./values.ts";

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/** Where the database file is. */
export type DatabaseLocation =
  /** An explicit file path. */
  | { readonly path: string }
  /** Mudlet's naming: `Database_<letters of name, lower-cased>.db` inside `directory`. */
  | { readonly name: string; readonly directory: string };

export interface OpenOptions {
  /** Open without writing: no schema migration runs, and the schema must already fit. */
  readonly readOnly?: boolean;
  /** Refuse to create a missing file (default: create it, as `db:create` does). */
  readonly create?: boolean;
  /** How long a statement waits for a lock another connection holds (ms; default 5000). */
  readonly timeout?: number;
}

/** What opening did to the file, and what it left alone. */
export interface OpenReport {
  readonly path: string;
  readonly createdSheets: readonly string[];
  /** `sheet.column` for each column added to an existing sheet. */
  readonly addedColumns: readonly string[];
  readonly addedIndexes: readonly string[];
  /** Differences kept as they were; each one is a sentence. */
  readonly warnings: readonly string[];
}

/** A fetched row: `_row_id` plus every declared column. */
export type Row<S extends SheetSpec> = { readonly _row_id: number } & {
  [K in ColumnNames<S>]: ReadValue<ColumnSpecOf<S, K>>;
};

/** A row to add: any subset of the columns; the rest take their defaults. */
export type NewRow<S extends SheetSpec> = {
  [K in ColumnNames<S>]?: WriteValue<ColumnSpecOf<S, K>>;
} & { readonly _row_id?: number };

/** A row to update: its `_row_id` and the columns to write. */
export type RowUpdate<S extends SheetSpec> = { readonly _row_id: number } & {
  [K in ColumnNames<S>]?: WriteValue<ColumnSpecOf<S, K>>;
};

/** A sheet's column references by column name, plus `_row_id`. */
export type Fields<S extends SheetSpec> = {
  readonly [K in ColumnNames<S>]: Field<ReadValue<ColumnSpecOf<S, K>>>;
} & { readonly _row_id: Field<number> };

/** A sheet handle: the row operations, with its columns under `fields`. */
export type Sheet<S extends SheetSpec> = SheetHandle<S>;

/** The sheets of a database, by name. */
export type Sheets<D extends DatabaseSpec> = {
  readonly [N in keyof D & string]: Sheet<D[N]>;
};

/** A database handle: the operations, with its sheets under `sheets`. */
export type Database<D extends DatabaseSpec> = DatabaseHandle<D>;

/** A sheet whose columns were read from the file rather than declared: every value type is open. */
export type DiscoveredSheetSpec = { readonly [column: string]: string | number | null };

export type AggregateFunction = "COUNT" | "AVG" | "MAX" | "MIN" | "TOTAL";

/** What `delete` accepts: a `_row_id`, a fetched row, a query, or `true` for every row. */
export type DeleteTarget<S extends SheetSpec> = number | Row<S> | { readonly _row_id: number } | Query | true;

// ---------------------------------------------------------------------------
// Sheet
// ---------------------------------------------------------------------------

const AGGREGATES: readonly AggregateFunction[] = ["COUNT", "AVG", "MAX", "MIN", "TOTAL"];

export class SheetHandle<S extends SheetSpec> {
  readonly name: string;
  /**
   * The column references for predicates, ordering and `set`, keyed by column
   * name (`mydb.people.fields.city`), plus `_row_id`. Column names never
   * collide with the sheet's own methods this way.
   */
  readonly fields: Fields<S>;
  readonly #database: DatabaseHandle<DatabaseSpec>;
  readonly #sheet: NormalizedSheet;
  readonly #fields: Map<string, Field>;

  constructor(database: DatabaseHandle<DatabaseSpec>, sheet: NormalizedSheet) {
    this.name = sheet.name;
    this.#database = database;
    this.#sheet = sheet;
    this.#fields = new Map(sheet.columns.map((column) => [column.name, new Field(sheet.name, column.name, column.kind)]));
    const fields: Record<string, Field> = { [ROW_ID]: new Field<number>(sheet.name, ROW_ID, "number") };
    for (const [name, field] of this.#fields) fields[name] = field;
    this.fields = Object.freeze(fields) as Fields<S>;
  }

  /** The declared columns, followed by preserved columns from the file. */
  get columns(): readonly string[] {
    return this.#sheet.columns.map((column) => column.name);
  }

  /** The column reference for `name`, for code that picks columns dynamically. */
  field<K extends ColumnNames<S>>(name: K): Fields<S>[K];
  field(name: string): Field;
  field(name: string): Field {
    const field = name === ROW_ID ? this.fields._row_id : this.#fields.get(name);
    if (!field) {
      throw new MudletDbError(
        "schema",
        `sheet ${this.name} has no column ${JSON.stringify(name)} (it has ${this.columns.join(", ")})`,
      );
    }
    return field;
  }

  /**
   * A query matching rows whose columns equal the example's values (`null`
   * matches NULL). Values are compared exactly; there is no operator syntax.
   */
  where(example: NewRow<S>): Expression {
    const parts: Expression[] = [];
    for (const [name, value] of Object.entries(example)) {
      if (value === undefined) continue;
      const field = this.field(name);
      parts.push(value === null ? isNull(field) : eq(field, value as never));
    }
    if (parts.length === 0) {
      throw new MudletDbError("argument", `${this.name}.where() needs at least one column value`);
    }
    return and(...parts);
  }

  /**
   * Add rows. Each returns its new `_row_id`, or `null` when a unique
   * violation under `_violations: "IGNORE"` dropped it. Several rows go in
   * together: if one is refused, none of that call's rows stay.
   */
  add(...rows: NewRow<S>[]): (number | null)[] {
    if (rows.length === 0) return [];
    const inserts = rows.map((row) => this.#insertStatement(row));
    return this.#database.transaction(() =>
      inserts.map(({ sql, params }) => {
        const result = this.#database.run(sql, params, `add to ${this.name}`);
        return Number(result.changes) === 0 ? null : Number(result.lastInsertRowid);
      }),
    );
  }

  /** Every row matching `query` (all rows when omitted), in `options` order. */
  fetch(query?: Query, options?: OrderOptions): Row<S>[] {
    const where = compileQuery(query, `${this.name}.fetch`);
    const sql =
      `SELECT * FROM ${quoteIdentifier(this.name)}` +
      (where ? ` WHERE ${where.sql}` : "") +
      compileOrder(options, `${this.name}.fetch`);
    const rows = this.#database.all(sql, where?.params ?? [], `fetch from ${this.name}`);
    return rows.map((row) => this.#coerce(row));
  }

  /** The first matching row, or `undefined`. */
  fetchOne(query?: Query, options?: OrderOptions): Row<S> | undefined {
    return this.fetch(query, { ...options, limit: 1 })[0];
  }

  /**
   * Run a SELECT of your own against this sheet; its rows are typed like
   * `fetch`'s. Values belong in `params` (`?` placeholders), not in the text.
   */
  fetchSql(sql: string, params: readonly SqlParam[] = []): Row<S>[] {
    const rows = this.#database.all(sql, params, `fetchSql on ${this.name}`);
    return rows.map((row) => this.#coerce(row));
  }

  /**
   * Write a row back by its `_row_id`. Every declared column present on the
   * object is written, `null` included; columns left off stay as they are.
   */
  update(row: RowUpdate<S>): void {
    const rowId = this.#rowIdOf(row, "update");
    const sets: string[] = [];
    const params: SqlParam[] = [];
    for (const column of this.#sheet.columns) {
      if (!Object.hasOwn(row, column.name)) continue;
      const value = (row as Record<string, unknown>)[column.name];
      if (value === undefined) continue;
      const fragment = writeValue(column.kind, value, `${this.name}.${column.name}`);
      sets.push(`${quoteIdentifier(column.name)} = ${fragment.sql}`);
      params.push(...fragment.params);
    }
    this.#rejectUnknownColumns(row, "update");
    if (sets.length === 0) {
      throw new MudletDbError("argument", `${this.name}.update() was given a row with no columns to write`);
    }
    params.push(rowId);
    const sql = `UPDATE ${quoteIdentifier(this.name)} SET ${sets.join(", ")} WHERE ${quoteIdentifier(ROW_ID)} = ?`;
    const result = this.#database.run(sql, params, `update ${this.name}`);
    if (Number(result.changes) === 0) {
      throw new MudletDbError("argument", `${this.name}.update(): no row has _row_id ${rowId}`);
    }
  }

  /**
   * Set one column across the rows `query` matches (every row when omitted).
   * The value may be an expression such as `exp("found + 1")`. Returns how
   * many rows changed.
   */
  set<K extends ColumnNames<S>>(
    field: K,
    value: WriteValue<ColumnSpecOf<S, K>> | Expression,
    query?: Query,
  ): number;
  set<T>(field: Field<T>, value: T | typeof CURRENT_TIMESTAMP | Expression, query?: Query): number;
  set(field: string | Field, value: unknown, query?: Query): number {
    const target = field instanceof Field ? this.field(field.name) : this.field(field);
    if (target.name === ROW_ID) {
      throw new MudletDbError("argument", `${this.name}.set(): _row_id cannot be changed`);
    }
    const fragment =
      value instanceof Expression ? value : writeValue(target.kind, value, `${this.name}.${target.name}`);
    const where = compileQuery(query, `${this.name}.set`);
    const sql =
      `UPDATE ${quoteIdentifier(this.name)} SET ${target.quoted} = ${fragment.sql}` +
      (where ? ` WHERE ${where.sql}` : "");
    const result = this.#database.run(sql, [...fragment.params, ...(where?.params ?? [])], `set ${this}`);
    return Number(result.changes);
  }

  /**
   * Delete by `_row_id`, by a fetched row, by a query, or every row with
   * `true`. Returns how many rows went. A query is required: an accidental
   * `delete()` must not empty a sheet.
   */
  delete(target: DeleteTarget<S>): number {
    if (target === undefined || target === null) {
      throw new MudletDbError("argument", `${this.name}.delete() needs a _row_id, a row, a query, or true`);
    }
    let where: { sql: string; params: readonly SqlParam[] } | null;
    if (target === true) {
      where = null;
    } else if (typeof target === "number") {
      where = { sql: `${quoteIdentifier(ROW_ID)} = ?`, params: [target] };
    } else if (target instanceof Expression || Array.isArray(target)) {
      where = compileQuery(target as Query, `${this.name}.delete`);
    } else if (typeof target === "object") {
      where = { sql: `${quoteIdentifier(ROW_ID)} = ?`, params: [this.#rowIdOf(target, "delete")] };
    } else {
      throw new MudletDbError("argument", `${this.name}.delete() cannot delete by ${typeof target}`);
    }
    const sql = `DELETE FROM ${quoteIdentifier(this.name)}` + (where ? ` WHERE ${where.sql}` : "");
    return Number(this.#database.run(sql, where?.params ?? [], `delete from ${this.name}`).changes);
  }

  /** How many rows match `query` (all rows when omitted). */
  count(query?: Query): number {
    return Number(this.aggregate(this.fields._row_id, "COUNT", query) ?? 0);
  }

  /**
   * COUNT, AVG, MAX, MIN or TOTAL over a column. MIN and MAX come back typed
   * like the column (a `Date` for timestamps); the others are numbers. An
   * empty MIN/MAX is `null`.
   */
  aggregate(field: Field, fn: AggregateFunction | Lowercase<AggregateFunction>, query?: Query, distinct = false): number | string | Date | null {
    const upper = String(fn).toUpperCase() as AggregateFunction;
    if (!AGGREGATES.includes(upper)) {
      throw new MudletDbError("argument", `aggregate function must be one of ${AGGREGATES.join(", ")}, not ${fn}`);
    }
    if (!(field instanceof Field) || field.sheet !== this.name) {
      throw new MudletDbError("argument", `${this.name}.aggregate() takes one of this sheet's fields`);
    }
    const where = compileQuery(query, `${this.name}.aggregate`);
    const sql =
      `SELECT ${upper}(${distinct ? "DISTINCT " : ""}${field.quoted}) AS value FROM ${quoteIdentifier(this.name)}` +
      (where ? ` WHERE ${where.sql}` : "");
    const row = this.#database.get(sql, where?.params ?? [], `aggregate over ${this.name}`);
    const raw = row?.value ?? null;
    if (upper === "MIN" || upper === "MAX") {
      if (field.kind === "timestamp") return readValue("timestamp", raw, `${this}.${field.name}`) as Date | null;
      return raw === null ? null : typeof raw === "bigint" ? Number(raw) : (raw as number | string);
    }
    return raw === null ? 0 : Number(raw);
  }

  /**
   * Update the rows whose unique key matches and add the ones that do not,
   * in one transaction. Only a sheet with exactly one single-column `_unique`
   * entry qualifies; each row must carry that column.
   */
  mergeUnique(rows: ReadonlyArray<NewRow<S>>): void {
    if (!Array.isArray(rows)) {
      throw new MudletDbError("argument", `${this.name}.mergeUnique() needs a list of rows`);
    }
    const unique = this.#sheet.unique;
    if (unique.length !== 1) {
      throw new MudletDbError(
        "unsupported",
        `${this.name}.mergeUnique() works on a sheet with a single unique index; this sheet has ${unique.length}`,
      );
    }
    const key = unique[0];
    const keyColumn = typeof key === "string" ? key : key.length === 1 ? key[0] : null;
    if (keyColumn === null) {
      throw new MudletDbError(
        "unsupported",
        `${this.name}.mergeUnique() works on a single-column unique index; this sheet's spans ${key.length} columns`,
      );
    }
    const keyField = this.field(keyColumn);
    this.#database.transaction(() => {
      for (const row of rows) {
        const keyValue = (row as Record<string, unknown>)[keyColumn];
        if (keyValue === undefined || keyValue === null) {
          throw new MudletDbError("argument", `${this.name}.mergeUnique(): a row is missing its unique key ${keyColumn}`);
        }
        const existing = this.fetchOne(eq(keyField, keyValue as never));
        if (existing) {
          this.update({ ...existing, ...row, _row_id: existing._row_id } as RowUpdate<S>);
        } else {
          this.add(row);
        }
      }
    });
  }

  toString(): string {
    return this.name;
  }

  #insertStatement(row: NewRow<S>): { sql: string; params: SqlParam[] } {
    if (typeof row !== "object" || row === null) {
      throw new MudletDbError("argument", `${this.name}.add() takes row objects, not ${typeof row}`);
    }
    this.#rejectUnknownColumns(row, "add");
    const names: string[] = [];
    const values: string[] = [];
    const params: SqlParam[] = [];
    for (const column of this.#sheet.columns) {
      if (!Object.hasOwn(row, column.name)) continue;
      const value = (row as Record<string, unknown>)[column.name];
      if (value === undefined) continue;
      const fragment = writeValue(column.kind, value, `${this.name}.${column.name}`);
      names.push(quoteIdentifier(column.name));
      values.push(fragment.sql);
      params.push(...fragment.params);
    }
    const table = quoteIdentifier(this.name);
    if (names.length === 0) return { sql: `INSERT INTO ${table} DEFAULT VALUES`, params };
    return { sql: `INSERT INTO ${table} (${names.join(",")}) VALUES (${values.join(",")})`, params };
  }

  #rejectUnknownColumns(row: object, operation: string): void {
    for (const key of Object.keys(row)) {
      if (key === ROW_ID || this.#fields.has(key)) continue;
      throw new MudletDbError(
        "schema",
        `${this.name}.${operation}(): ${JSON.stringify(key)} is not a column of this sheet (it has ${this.columns.join(", ")})`,
      );
    }
  }

  #rowIdOf(row: object, operation: string): number {
    const rowId = (row as { _row_id?: unknown })._row_id;
    if (typeof rowId !== "number" || !Number.isInteger(rowId)) {
      throw new MudletDbError(
        "argument",
        `${this.name}.${operation}() needs a row with a _row_id, as fetch() returns; build a query to ${operation} by value`,
      );
    }
    return rowId;
  }

  #coerce(raw: RawRow): Row<S> {
    const row: Record<string, unknown> = {};
    for (const [name, value] of Object.entries(raw)) {
      if (name === ROW_ID) {
        row[name] = Number(value);
        continue;
      }
      const field = this.#fields.get(name);
      row[name] = field ? readValue(field.kind, value, `${this.name}.${name} in row ${String(raw[ROW_ID])}`) : value;
    }
    return row as Row<S>;
  }
}

// ---------------------------------------------------------------------------
// Database
// ---------------------------------------------------------------------------

export class DatabaseHandle<D extends DatabaseSpec> {
  readonly path: string;
  readonly report: OpenReport;
  readonly readOnly: boolean;
  /**
   * The sheet handles by name (`mydb.sheets.people`). Sheet names never
   * collide with the database's own methods this way.
   */
  readonly sheets: Sheets<D>;
  #connection: Connection | null;
  #statements: StatementCache;
  readonly #sheets = new Map<string, SheetHandle<SheetSpec>>();
  #transactionDepth = 0;
  readonly #transactionBlocks: { aborted: boolean }[] = [];

  constructor(path: string, connection: Connection, readOnly: boolean, report: OpenReport) {
    this.path = path;
    this.report = report;
    this.readOnly = readOnly;
    this.#connection = connection;
    this.#statements = new StatementCache(connection);
    this.sheets = {} as Sheets<D>;
  }

  get isOpen(): boolean {
    return this.#connection !== null;
  }

  get inTransaction(): boolean {
    return this.#connection?.isTransaction ?? false;
  }

  /** The declared sheet names. */
  get sheetNames(): readonly string[] {
    return [...this.#sheets.keys()];
  }

  /** The sheet called `name`; unlike `sheets[name]`, an unknown name is an error. */
  sheet<N extends keyof D & string>(name: N): Sheet<D[N]>;
  sheet(name: string): Sheet<SheetSpec>;
  sheet(name: string): Sheet<SheetSpec> {
    const sheet = this.#sheets.get(name);
    if (!sheet) {
      throw new MudletDbError(
        "schema",
        `database ${this.path} has no sheet ${JSON.stringify(name)} (it has ${this.sheetNames.join(", ")})`,
      );
    }
    return sheet as Sheet<SheetSpec>;
  }

  /**
   * Run `work` inside a transaction: committed when it returns, rolled back
   * when it throws. Nested calls become savepoints, so an inner failure can
   * be caught without losing the outer transaction's work, except when SQLite
   * itself rolls back the whole transaction (for example, ON CONFLICT ROLLBACK).
   */
  transaction<T>(work: () => T): T {
    const depth = this.#transactionDepth;
    const savepoint = depth === 0 ? null : `mudlet_db_sp_${depth}`;
    this.#exec(savepoint ? `SAVEPOINT ${savepoint}` : "BEGIN", "begin transaction");
    this.#transactionDepth = depth + 1;
    const block = { aborted: false };
    this.#transactionBlocks.push(block);
    try {
      const result = work();
      this.#require("commit transaction");
      this.#exec(savepoint ? `RELEASE ${savepoint}` : "COMMIT", "commit transaction");
      return result;
    } catch (error) {
      if (!block.aborted && this.inTransaction) {
        try {
          this.#exec(savepoint ? `ROLLBACK TO ${savepoint}; RELEASE ${savepoint}` : "ROLLBACK", "roll back transaction");
        } catch {
          // Do not mask the original failure or leave uncertain work open.
          try { this.close(); } catch { /* Preserve the original error. */ }
        }
      }
      throw error;
    } finally {
      this.#transactionBlocks.pop();
      this.#transactionDepth = this.inTransaction ? depth : 0;
    }
  }

  /** Start a transaction by hand; pair it with `commit()` or `rollback()`. */
  begin(): void {
    if (this.#transactionDepth > 0) {
      throw new MudletDbError("transaction", `${this.path}: a transaction is already open`);
    }
    this.#exec("BEGIN", "begin transaction");
    this.#transactionDepth = 1;
  }

  commit(): void {
    this.#requireManualTransaction("commit");
    this.#exec("COMMIT", "commit transaction");
    this.#transactionDepth = 0;
  }

  rollback(): void {
    this.#requireManualTransaction("rollback");
    this.#exec("ROLLBACK", "roll back transaction");
    this.#transactionDepth = 0;
  }

  /** Run rows of your own SQL and get the raw rows back, untyped. */
  query(sql: string, params: readonly SqlParam[] = []): RawRow[] {
    return this.all(sql, params, "query");
  }

  /** Run SQL that returns nothing. */
  exec(sql: string, params: readonly SqlParam[] = []): number {
    return Number(this.run(sql, params, "exec").changes);
  }

  /**
   * Drop a sheet and its indexes. This is the one operation here that
   * destroys data, and it does exactly what it says.
   */
  dropSheet(name: keyof D & string): void {
    const sheet = this.sheet(name);
    this.transaction(() => {
      for (const row of this.all(
        "SELECT name FROM sqlite_master WHERE type = 'index' AND lower(tbl_name) = lower(?) AND sql IS NOT NULL",
        [name],
        "drop sheet",
      )) {
        this.run(`DROP INDEX IF EXISTS ${quoteIdentifier(String(row.name))}`, [], "drop sheet");
      }
      this.run(`DROP TABLE IF EXISTS ${quoteIdentifier(sheet.name)}`, [], "drop sheet");
    });
    this.#sheets.delete(name);
    delete (this.sheets as Record<string, unknown>)[name];
  }

  /**
   * Close the file. Work inside an unfinished transaction is discarded. Every
   * later call on this handle or its sheets throws; open the file again to
   * continue.
   */
  close(): void {
    const connection = this.#connection;
    if (!connection) return;
    this.#connection = null;
    this.#statements.clear();
    this.#transactionDepth = 0;
    try {
      connection.close();
    } catch (error) {
      throw fromDriver(`close ${this.path}`, error);
    }
  }

  [Symbol.dispose ?? Symbol.for("Symbol.dispose")](): void {
    this.close();
  }

  /** @internal */
  attachSheet(sheet: NormalizedSheet): SheetHandle<SheetSpec> {
    const handle = new SheetHandle<SheetSpec>(this as DatabaseHandle<DatabaseSpec>, sheet);
    this.#sheets.set(sheet.name, handle);
    Object.defineProperty(this.sheets, sheet.name, { value: handle, enumerable: true, configurable: true });
    return handle;
  }

  /** @internal */
  run(sql: string, params: readonly SqlParam[], context: string): { changes: number | bigint; lastInsertRowid: number | bigint } {
    this.#assertWritable(context);
    try {
      return this.#statements.get(sql).run(...params);
    } catch (error) {
      this.#syncAfterFailure();
      throw fromDriver(context, error);
    }
  }

  /** @internal */
  all(sql: string, params: readonly SqlParam[], context: string): RawRow[] {
    this.#require(context);
    try {
      return this.#statements.get(sql).all(...params);
    } catch (error) {
      this.#syncAfterFailure();
      throw fromDriver(context, error);
    }
  }

  /** @internal */
  get(sql: string, params: readonly SqlParam[], context: string): RawRow | undefined {
    this.#require(context);
    try {
      return this.#statements.get(sql).get(...params);
    } catch (error) {
      this.#syncAfterFailure();
      throw fromDriver(context, error);
    }
  }

  #exec(sql: string, context: string): void {
    const connection = this.#require(context);
    try {
      connection.exec(sql);
    } catch (error) {
      this.#syncAfterFailure();
      throw fromDriver(context, error);
    }
  }

  #require(context: string): Connection {
    if (!this.#connection) {
      throw new MudletDbError("closed", `cannot ${context}: database ${this.path} is closed`);
    }
    if (this.#transactionBlocks.some((block) => block.aborted)) {
      throw new MudletDbError("transaction", `cannot ${context}: SQLite rolled back the transaction; leave the transaction() callback before continuing`);
    }
    return this.#connection;
  }

  #syncAfterFailure(): void {
    if (this.inTransaction) return;
    this.#transactionDepth = 0;
    // A caught conflict must not let the remaining callback writes autocommit.
    for (const block of this.#transactionBlocks) block.aborted = true;
  }

  #assertWritable(context: string): void {
    this.#require(context);
    if (this.readOnly) {
      throw new MudletDbError("unsupported", `cannot ${context}: database ${this.path} was opened read-only`);
    }
  }

  #requireManualTransaction(operation: string): void {
    this.#require(operation);
    if (this.#transactionDepth === 0) {
      throw new MudletDbError("transaction", `${this.path}: ${operation}() called with no transaction open`);
    }
    if (this.#transactionBlocks.length > 0) {
      throw new MudletDbError("transaction", `${this.path}: ${operation}() cannot end a transaction() block early`);
    }
  }
}

// ---------------------------------------------------------------------------
// Opening
// ---------------------------------------------------------------------------

export function resolveLocation(location: DatabaseLocation | string): string {
  if (typeof location === "string") return location;
  if (typeof location === "object" && location !== null) {
    if ("path" in location && typeof location.path === "string") return location.path;
    if ("name" in location && "directory" in location) {
      return mudletDatabasePath(location.directory, location.name);
    }
  }
  throw new MudletDbError("argument", "a database location is { path } or { name, directory }");
}

function existingTable(database: DatabaseHandle<DatabaseSpec>, name: string): ExistingTable | null {
  const table = database.get(
    "SELECT name, sql FROM sqlite_master WHERE type = 'table' AND lower(name) = lower(?)",
    [name],
    "inspect schema",
  );
  if (!table) return null;
  const columns = database
    .all(`PRAGMA table_info(${quoteIdentifier(String(table.name))})`, [], "inspect schema")
    .map((row) => ({ name: String(row.name), type: String(row.type ?? "") }));
  const indexes = database
    .all(
      "SELECT name, sql FROM sqlite_master WHERE type = 'index' AND lower(tbl_name) = lower(?)",
      [name],
      "inspect schema",
    )
    .map((row) => ({ name: String(row.name), sql: row.sql === null ? null : String(row.sql) }));
  return { sql: String(table.sql ?? ""), columns, indexes };
}

function userTables(database: DatabaseHandle<DatabaseSpec>): string[] {
  return database
    .all(
      "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY rowid",
      [],
      "inspect schema",
    )
    .map((row) => String(row.name));
}

/**
 * Open a Mudlet database file. With a schema, missing sheets, columns and
 * indexes are created (what `db:create` does) and anything the file has that
 * the schema does not is kept and reported in `report.warnings`. Without a
 * schema, the sheets are read from the file as they are.
 */
export function openDatabase<const D extends DatabaseSpec>(
  location: DatabaseLocation | string,
  schema: D,
  options?: OpenOptions,
): Database<D>;
export function openDatabase(
  location: DatabaseLocation | string,
  schema?: undefined,
  options?: OpenOptions,
): Database<Record<string, DiscoveredSheetSpec>>;
export function openDatabase(
  location: DatabaseLocation | string,
  schema?: DatabaseSpec,
  options: OpenOptions = {},
): Database<DatabaseSpec> {
  const path = resolveLocation(location);
  const readOnly = options.readOnly ?? false;
  const normalized = schema === undefined ? null : normalizeSchema(schema);
  if ((options.create === false || readOnly) && !existsSync(path)) {
    throw new MudletDbError("argument", `no database file at ${path}`);
  }
  const connection = openConnection(path, { readOnly, timeout: options.timeout ?? 5000 });
  const createdSheets: string[] = [];
  const addedColumns: string[] = [];
  const addedIndexes: string[] = [];
  const warnings: string[] = [];
  const database = new DatabaseHandle<DatabaseSpec>(path, connection, readOnly, {
    path,
    createdSheets,
    addedColumns,
    addedIndexes,
    warnings,
  });
  try {
    if (normalized === null) {
      for (const table of userTables(database)) {
        const existing = existingTable(database, table);
        if (existing) database.attachSheet(discoverSheet(table, existing));
      }
      return database as Database<DatabaseSpec>;
    }
    const statements: string[] = [];
    for (const sheet of normalized.values()) {
      const plan = planSheet(sheet, existingTable(database, sheet.name));
      statements.push(...plan.statements);
      if (plan.created) createdSheets.push(sheet.name);
      addedColumns.push(...plan.addedColumns.map((column) => `${sheet.name}.${column}`));
      addedIndexes.push(...plan.addedIndexes);
      warnings.push(...plan.warnings);
    }
    if (statements.length > 0) {
      if (readOnly) {
        throw new MudletDbError(
          "schema-mismatch",
          `${path} was opened read-only but the schema needs changes: ${statements.join("; ")}`,
        );
      }
      database.transaction(() => {
        for (const sql of statements) database.exec(sql);
      });
    }
    for (const sheet of normalized.values()) {
      const declared = new Set(sheet.columns.map((column) => column.name.toLowerCase()));
      const extraColumns = (existingTable(database, sheet.name)?.columns ?? [])
        .filter((column) => column.name.toLowerCase() !== ROW_ID && !declared.has(column.name.toLowerCase()))
        .map((column) => ({ name: column.name, kind: "any" as const, defaultValue: null }));
      // Preserved columns are valid row fields, including for fetch-edit-update
      // and mergeUnique. Keep their values raw when the caller supplied no type.
      database.attachSheet({ ...sheet, columns: [...sheet.columns, ...extraColumns] });
    }
    return database as Database<DatabaseSpec>;
  } catch (error) {
    database.close();
    throw error;
  }
}

/** One sheet as the file describes it. */
export interface InspectedSheet {
  readonly name: string;
  readonly sql: string;
  readonly columns: ReadonlyArray<{ readonly name: string; readonly type: string }>;
  readonly indexes: ReadonlyArray<{ readonly name: string; readonly sql: string | null }>;
}

/** Read a file's tables and columns without changing it. */
export function inspectDatabase(location: DatabaseLocation | string): { path: string; sheets: InspectedSheet[] } {
  const path = resolveLocation(location);
  if (!existsSync(path)) throw new MudletDbError("argument", `no database file at ${path}`);
  const connection = openConnection(path, { readOnly: true, timeout: 5000 });
  const database = new DatabaseHandle<DatabaseSpec>(path, connection, true, {
    path,
    createdSheets: [],
    addedColumns: [],
    addedIndexes: [],
    warnings: [],
  });
  try {
    const sheets: InspectedSheet[] = [];
    for (const table of userTables(database)) {
      const existing = existingTable(database, table);
      if (existing) sheets.push({ name: table, ...existing });
    }
    return { path, sheets };
  } finally {
    database.close();
  }
}

export type { ColumnKind };
