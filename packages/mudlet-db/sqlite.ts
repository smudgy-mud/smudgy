// The slice of `node:sqlite` this package uses, typed locally so the package
// needs no ambient Node declarations. Smudgy's embedded runtime and host Node
// both provide `DatabaseSync` with this shape.

import { DatabaseSync } from "node:sqlite";
import { fromDriver } from "./errors.ts";
import type { SqlParam, SqlValue } from "./values.ts";

export type RawRow = Record<string, SqlValue>;

export interface RunResult {
  readonly changes: number | bigint;
  readonly lastInsertRowid: number | bigint;
}

export interface Statement {
  all(...params: SqlParam[]): RawRow[];
  get(...params: SqlParam[]): RawRow | undefined;
  run(...params: SqlParam[]): RunResult;
}

export interface Connection {
  readonly isTransaction: boolean;
  prepare(sql: string): Statement;
  exec(sql: string): void;
  close(): void;
}

export interface ConnectionOptions {
  readonly readOnly: boolean;
  /** Milliseconds to wait for a lock held by another connection. */
  readonly timeout: number;
}

/**
 * Open the file. Foreign keys are left at SQLite's default (off), as Mudlet's
 * driver leaves them, so an imported file behaves as it did there.
 */
export function openConnection(path: string, options: ConnectionOptions): Connection {
  try {
    const database = new DatabaseSync(path, {
      readOnly: options.readOnly,
      timeout: options.timeout,
      enableForeignKeyConstraints: false,
    });
    return database as unknown as Connection;
  } catch (error) {
    throw fromDriver(`open ${path}`, error);
  }
}

/** A small prepared-statement cache; the same SQL is prepared once per connection. */
export class StatementCache {
  readonly #connection: Connection;
  readonly #statements = new Map<string, Statement>();
  readonly #capacity: number;

  constructor(connection: Connection, capacity = 64) {
    this.#connection = connection;
    this.#capacity = capacity;
  }

  get(sql: string): Statement {
    const cached = this.#statements.get(sql);
    if (cached) {
      this.#statements.delete(sql);
      this.#statements.set(sql, cached);
      return cached;
    }
    const statement = this.#connection.prepare(sql);
    if (this.#statements.size >= this.#capacity) {
      const oldest = this.#statements.keys().next().value;
      if (oldest !== undefined) this.#statements.delete(oldest);
    }
    this.#statements.set(sql, statement);
    return statement;
  }

  clear(): void {
    this.#statements.clear();
  }
}
