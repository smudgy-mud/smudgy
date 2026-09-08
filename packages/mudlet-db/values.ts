// How JavaScript values become SQLite values and back, following the storage
// conventions DB.lua established: text and REAL columns hold their values as
// is, timestamps are UTC `YYYY-MM-DD HH:MM:SS` text in an INTEGER column, and
// SQL NULL is `null`.

import { MudletDbError } from "./errors.ts";

/**
 * Write this into a timestamp column to store the database's idea of "now"
 * (`datetime('now')`, UTC, whole seconds). It is only meaningful as a value to
 * store or a column default; reading a row never yields it.
 */
export const CURRENT_TIMESTAMP: unique symbol = Symbol("CURRENT_TIMESTAMP");

const TIMESTAMP_COLUMN = Symbol("TimestampColumn");

/** A timestamp column declaration; make one with {@link timestamp}. */
export interface TimestampColumn {
  readonly [TIMESTAMP_COLUMN]: true;
  readonly defaultValue: Date | typeof CURRENT_TIMESTAMP | null;
}

/**
 * Declare a timestamp column. With no argument the column defaults to NULL;
 * pass {@link CURRENT_TIMESTAMP} to stamp rows as they are added, or a `Date`
 * for a fixed default. Mudlet's `db:Timestamp("CURRENT_TIMESTAMP")`,
 * `db:Timestamp(nil)` and `db:Timestamp(<epoch>)` map onto these three.
 */
export function timestamp(
  defaultValue: Date | typeof CURRENT_TIMESTAMP | null = null,
): TimestampColumn {
  if (defaultValue instanceof Date && Number.isNaN(defaultValue.getTime())) {
    throw new MudletDbError("value", "timestamp() default is an invalid Date");
  }
  return Object.freeze({ [TIMESTAMP_COLUMN]: true as const, defaultValue });
}

export function isTimestampColumn(value: unknown): value is TimestampColumn {
  return typeof value === "object" && value !== null && TIMESTAMP_COLUMN in value;
}

/** What a column holds, decided from its declared default the way DB.lua does. */
export type ColumnKind = "text" | "number" | "timestamp" | "any";

/** A value SQLite can store, as `node:sqlite` hands it back. */
export type SqlValue = string | number | bigint | Uint8Array | null;

/** A parameter this package binds. */
export type SqlParam = string | number | bigint | Uint8Array | null;

/** A fragment of SQL with the values it binds, in order. */
export interface SqlFragment {
  readonly sql: string;
  readonly params: readonly SqlParam[];
}

/** SQLite's `datetime()` output, which is what Mudlet stores. */
const TIMESTAMP_TEXT = /^(\d{4})-(\d{2})-(\d{2}) (\d{2}):(\d{2}):(\d{2})$/;

/**
 * A stored timestamp as a `Date`. Mudlet writes `datetime(...)` text; a row
 * that fell back to an epoch default holds the integer instead, and both read
 * the same way. Anything else is not a value this package can stand behind.
 */
export function readTimestamp(raw: SqlValue, where: string): Date | null {
  if (raw === null) return null;
  if (typeof raw === "number" && Number.isFinite(raw)) return new Date(raw * 1000);
  if (typeof raw === "bigint") return new Date(Number(raw) * 1000);
  if (typeof raw === "string") {
    const match = TIMESTAMP_TEXT.exec(raw);
    if (match) {
      const [year, month, day, hour, minute, second] = match.slice(1).map(Number);
      return new Date(Date.UTC(year, month - 1, day, hour, minute, second));
    }
    const epoch = Number(raw);
    if (raw.trim() !== "" && Number.isFinite(epoch)) return new Date(epoch * 1000);
  }
  throw new MudletDbError(
    "stored-value",
    `${where} holds ${JSON.stringify(String(raw))}, which is not a timestamp this package can read ` +
      "(expected UTC 'YYYY-MM-DD HH:MM:SS' text or an epoch number)",
  );
}

/** Whole seconds since the epoch, the precision Mudlet keeps. */
export function epochSeconds(date: Date): number {
  const millis = date.getTime();
  if (Number.isNaN(millis)) {
    throw new MudletDbError("value", "cannot store an invalid Date");
  }
  return Math.floor(millis / 1000);
}

/**
 * The SQL that stores `value` into a column of `kind`, with its bound
 * parameters. Timestamps become the same `datetime(...)` calls DB.lua emits so
 * the stored text is identical whichever client wrote it.
 */
export function writeValue(kind: ColumnKind, value: unknown, where: string): SqlFragment {
  if (value === null) return { sql: "NULL", params: [] };
  if (value === undefined) {
    throw new MudletDbError("value", `${where}: undefined is not a storable value; use null for SQL NULL`);
  }
  if (typeof value === "boolean") {
    throw new MudletDbError(
      "value",
      `${where}: booleans have no Mudlet storage form; store 1/0 or "true"/"false" explicitly`,
    );
  }
  if (kind === "timestamp") {
    if (value === CURRENT_TIMESTAMP) return { sql: "datetime('now')", params: [] };
    if (value instanceof Date) return { sql: "datetime(?, 'unixepoch')", params: [epochSeconds(value)] };
    throw new MudletDbError(
      "value",
      `${where}: a timestamp column takes a Date, CURRENT_TIMESTAMP or null, not ${typeof value}`,
    );
  }
  if (value === CURRENT_TIMESTAMP || value instanceof Date) {
    throw new MudletDbError(
      "value",
      `${where}: only a timestamp column stores a Date; declare the column with timestamp()`,
    );
  }
  if (typeof value === "number" || typeof value === "bigint" || value instanceof Uint8Array) {
    return { sql: "?", params: [value] };
  }
  if (typeof value === "string") return { sql: "?", params: [value] };
  throw new MudletDbError("value", `${where}: cannot store a ${typeof value}`);
}

/**
 * The value a fetched row carries for a column of `kind`. Text and number
 * columns come back as SQLite stored them (its type affinity already coerced
 * numeric text in a number column, as Mudlet's `tonumber` would have).
 */
export function readValue(kind: ColumnKind, raw: SqlValue, where: string): SqlValue | Date {
  if (kind === "timestamp") return readTimestamp(raw, where);
  return raw;
}
