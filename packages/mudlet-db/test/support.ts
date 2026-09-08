// Shared helpers: every test opens its own file in a fresh temporary directory.

import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDatabase, type Database, type DatabaseSpec, type OpenOptions } from "../index.ts";

export interface TempDir {
  readonly path: string;
  remove(): void;
}

export function tempDir(): TempDir {
  const path = mkdtempSync(join(tmpdir(), "mudlet-db-"));
  return {
    path,
    remove() {
      rmSync(path, { recursive: true, force: true });
    },
  };
}

/** Open `name` in a fresh directory; `remove()` deletes the directory. */
export function tempDatabase<const D extends DatabaseSpec>(
  name: string,
  schema: D,
  options?: OpenOptions,
): { db: Database<D>; dir: TempDir; reopen(schema?: D, options?: OpenOptions): Database<D> } {
  const dir = tempDir();
  const db = openDatabase({ name, directory: dir.path }, schema, options);
  return {
    db,
    dir,
    reopen(again = schema, againOptions = options) {
      return openDatabase({ name, directory: dir.path }, again, againOptions);
    },
  };
}

/** The `name` column of each row, sorted, for order-insensitive comparisons. */
export function names(rows: ReadonlyArray<{ name: unknown }>): string[] {
  return rows.map((row) => String(row.name)).sort();
}
