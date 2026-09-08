// Taking a consistent copy of a database file. Copying the bytes of a
// `.db` while another program has it open can miss a write-ahead log or catch
// a half-written page; SQLite's own `VACUUM INTO` reads through a connection
// and writes a complete, compact snapshot.

import { existsSync, linkSync, mkdtempSync, renameSync, rmSync, statSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fromDriver, MudletDbError } from "./errors.ts";
import { resolveLocation, type DatabaseLocation } from "./database.ts";
import { openConnection } from "./sqlite.ts";

export interface CopyOptions {
  /** Replace an existing destination file (default: refuse). */
  readonly overwrite?: boolean;
  /** Milliseconds to wait for a lock another writer holds (default 5000). */
  readonly timeout?: number;
}

/**
 * Snapshot `source` into `destination`. The source is opened read-only, so
 * the original is never modified; a database in WAL mode needs its `-shm`
 * file to be readable for that, which is the case whenever its owner has it
 * open or closed it cleanly.
 */
export function copyDatabase(
  source: DatabaseLocation | string,
  destination: DatabaseLocation | string,
  options: CopyOptions = {},
): { path: string } {
  const from = resolveLocation(source);
  const to = resolveLocation(destination);
  if (!existsSync(from)) throw new MudletDbError("argument", `no database file at ${from}`);
  const checkDestination = () => {
    const sourceFile = statSync(from, { bigint: true });
    const targetFile = existsSync(to) ? statSync(to, { bigint: true }) : undefined;
    const sameFile = targetFile && sourceFile.dev === targetFile.dev && sourceFile.ino === targetFile.ino;
    if (resolve(from) === resolve(to) || sameFile) {
      throw new MudletDbError("argument", "copyDatabase() needs two different files");
    }
    if (!targetFile) return;
    if (!options.overwrite) {
      throw new MudletDbError("argument", `${to} already exists; pass { overwrite: true } to replace it`);
    }
  };
  let staging: string | undefined;
  try {
    checkDestination();
    // Stage on the destination filesystem. A failed snapshot leaves both files intact.
    staging = mkdtempSync(join(dirname(resolve(to)), ".mudlet-db-copy-"));
    const snapshot = join(staging, "snapshot.db");
    const connection = openConnection(from, { readOnly: true, timeout: options.timeout ?? 5000 });
    try {
      connection.prepare("VACUUM INTO ?").run(snapshot);
    } catch (error) {
      if (error instanceof Error && /too many attached databases - max 0/.test(error.message)) {
        throw new MudletDbError(
          "unsupported",
          "copyDatabase() needs a local module or trusted package: the sandbox blocks SQLite snapshots. " +
            "Copy the database from a local module, then open the copy from your sandboxed package.",
          { cause: error },
        );
      }
      throw error;
    } finally {
      connection.close();
    }
    checkDestination();
    if (options.overwrite) {
      renameSync(snapshot, to);
    } else {
      // Exclusive publication also refuses a destination created during the snapshot.
      linkSync(snapshot, to);
    }
  } catch (error) {
    throw fromDriver(`copy ${from} to ${to}`, error);
  } finally {
    if (staging) rmSync(staging, { recursive: true, force: true });
  }
  return { path: to };
}
