// File and identifier naming, kept byte-compatible with Mudlet's DB.lua so a
// database this package creates is one Mudlet would recognise, and vice versa.

import { MudletDbError } from "./errors.ts";

/**
 * The file name Mudlet gives a database. DB.lua's `safe_name` strips every
 * character that is not an ASCII letter (its pattern is `[^%ad]`, and `d` is a
 * letter already, so digits and punctuation go too) and lower-cases the rest:
 * `"combat_log2"` becomes `Database_combatlog.db`. Mudlet's documentation says
 * "alphanumeric"; the pinned source does not keep digits, and the file on disk
 * follows the source.
 */
export function mudletSafeName(name: string): string {
  return name.replace(/[^A-Za-z]/g, "").toLowerCase();
}

export function mudletDatabaseFileName(name: string): string {
  const safe = mudletSafeName(name);
  if (!safe) {
    throw new MudletDbError(
      "schema",
      `database name ${JSON.stringify(name)} has no letters, so Mudlet's naming rule ` +
        "leaves nothing to call the file; pass an explicit { path } instead",
    );
  }
  return `Database_${safe}.db`;
}

function joinPath(directory: string, file: string): string {
  const trimmed = directory.replace(/[\\/]+$/, "");
  const separator = trimmed.includes("\\") && !trimmed.includes("/") ? "\\" : "/";
  return `${trimmed}${separator}${file}`;
}

/** Where Mudlet would keep `name` if `directory` were its profile directory. */
export function mudletDatabasePath(directory: string, name: string): string {
  return joinPath(directory, mudletDatabaseFileName(name));
}

/** Quote an identifier for SQL. Every name this package writes goes through here. */
export function quoteIdentifier(name: string): string {
  return `"${name.replaceAll('"', '""')}"`;
}

const IDENTIFIER = /^[A-Za-z_][A-Za-z0-9_]*$/;

/**
 * Sheet names reach SQL unquoted in Mudlet, so anything that is not a plain
 * identifier could never have been created by DB.lua. Column names are quoted
 * there, but a column that needs quoting cannot be typed as a property in a
 * converted script either, so the same rule applies to both.
 */
export function assertIdentifier(kind: "sheet" | "column", name: string): void {
  if (typeof name !== "string" || !IDENTIFIER.test(name)) {
    throw new MudletDbError(
      "schema",
      `${kind} name ${JSON.stringify(name)} is not a plain identifier (letters, digits and ` +
        "underscores, not starting with a digit)",
    );
  }
}

/**
 * Mudlet's index name: `idx_<sheet>_c_<column>` for one column and the columns
 * joined with underscores for a compound index. Kept exact so the indexes this
 * package creates are the ones a later `db:create` in Mudlet finds and keeps.
 */
export function mudletIndexName(sheet: string, columns: readonly string[]): string {
  return ["idx", sheet, "c", ...columns].join("_");
}
