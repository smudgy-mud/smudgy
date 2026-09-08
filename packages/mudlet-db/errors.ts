// Every failure this package raises is a MudletDbError. The `code` names the
// contract that was broken, so a caller can react to "the file is closed" or
// "this schema change is unsupported" without parsing the message.

export type MudletDbErrorCode =
  /** The database handle has been closed; open it again to keep working. */
  | "closed"
  /** A name (sheet, column, index option) the schema cannot accept. */
  | "schema"
  /** The file's existing tables differ from the schema in a way this package
   *  will not migrate automatically (constraint changes, column removal). */
  | "schema-mismatch"
  /** The Mudlet operation exists but this package deliberately does not
   *  provide it; the message says what to do instead. */
  | "unsupported"
  /** A value that cannot be stored or compared as asked. */
  | "value"
  /** A stored value that cannot be represented (an unreadable timestamp). */
  | "stored-value"
  /** A call that needs a row identity (`_row_id`) or a query and got neither. */
  | "argument"
  /** The runtime refused to open the file: the package lacks a read/write grant. */
  | "permission"
  /** SQLite refused the statement; `cause` carries the driver error. */
  | "sqlite"
  /** A transaction call made outside, or inside, a transaction that forbids it. */
  | "transaction";

export class MudletDbError extends Error {
  readonly code: MudletDbErrorCode;

  constructor(code: MudletDbErrorCode, message: string, options?: { cause?: unknown }) {
    super(message, options);
    this.name = "MudletDbError";
    this.code = code;
  }
}

interface DriverErrorShape {
  code?: unknown;
  name?: unknown;
}

function describe(error: unknown): string {
  if (error instanceof Error) return error.message;
  return String(error);
}

/**
 * Wrap whatever `node:sqlite` threw. Permission denials from the sandbox are
 * told apart from SQL failures so the message can name the fix (a manifest
 * grant) rather than the symptom.
 */
export function fromDriver(context: string, error: unknown): MudletDbError {
  if (error instanceof MudletDbError) return error;
  const shape = (error ?? {}) as DriverErrorShape;
  const denied =
    shape.name === "NotCapable" ||
    shape.code === "ERR_ACCESS_DENIED" ||
    /requires (read|write) access|NotCapable/i.test(describe(error));
  if (denied) {
    return new MudletDbError(
      "permission",
      `${context}: the runtime denied file access (${describe(error)}). A sandboxed package ` +
        'must grant "read" and "write" on the directory holding the database in its ' +
        'smudgy.package.json, for example { "permissions": { "read": ["$DATA"], "write": ["$DATA"] } }.',
      { cause: error },
    );
  }
  return new MudletDbError("sqlite", `${context}: ${describe(error)}`, { cause: error });
}
