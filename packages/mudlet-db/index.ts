// official/mudlet-db: Mudlet's `db` sheet-and-query interface over the same
// SQLite files, for scripts converted from Mudlet and for anyone who wants a
// small typed table store. See README.md for the supported contract.

export {
  openDatabase,
  inspectDatabase,
  resolveLocation,
  DatabaseHandle,
  SheetHandle,
} from "./database.ts";
export type {
  AggregateFunction,
  Database,
  DatabaseLocation,
  DeleteTarget,
  DiscoveredSheetSpec,
  Fields,
  InspectedSheet,
  NewRow,
  OpenOptions,
  OpenReport,
  Row,
  RowUpdate,
  Sheet,
  Sheets,
} from "./database.ts";

export { copyDatabase } from "./copy.ts";
export type { CopyOptions } from "./copy.ts";

export {
  and,
  between,
  eq,
  exp,
  Expression,
  Field,
  gt,
  gte,
  isIn,
  isNotNull,
  isNull,
  like,
  lt,
  lte,
  notBetween,
  notEq,
  notIn,
  notLike,
  or,
} from "./query.ts";
export type { Comparable, OrderOptions, Query } from "./query.ts";

export { CURRENT_TIMESTAMP, timestamp } from "./values.ts";
export type { ColumnKind, SqlParam, SqlValue, TimestampColumn } from "./values.ts";

export {
  buildCreateIndexSql,
  buildCreateTableSql,
  extractTableConstraints,
  normalizeSchema,
  normalizeSheet,
} from "./schema.ts";
export type {
  ColumnNames,
  ColumnSpec,
  DatabaseSpec,
  IndexSpec,
  KeyedSheetSpec,
  ListSheetSpec,
  NormalizedSheet,
  ReadValue,
  SheetOptions,
  SheetSpec,
  Violations,
  WriteValue,
} from "./schema.ts";

export { mudletDatabaseFileName, mudletDatabasePath, mudletSafeName } from "./names.ts";

export { MudletDbError } from "./errors.ts";
export type { MudletDbErrorCode } from "./errors.ts";
