// Query expressions: structured predicates that compile to SQL with bound
// values. Each builder mirrors one of DB.lua's `db:eq`, `db:like`, `db:AND`
// and friends; the SQL they produce reads the same, but values travel as
// parameters instead of being pasted into the statement.

import { MudletDbError } from "./errors.ts";
import { quoteIdentifier } from "./names.ts";
import { writeValue, type ColumnKind, type SqlFragment, type SqlParam } from "./values.ts";

/** A reference to one column of a sheet, for predicates, ordering and `set`. */
export class Field<T = unknown> {
  readonly sheet: string;
  readonly name: string;
  readonly kind: ColumnKind;
  /** Phantom: the type a fetched row holds for this column. Never set. */
  declare readonly valueType: T;

  constructor(sheet: string, name: string, kind: ColumnKind) {
    this.sheet = sheet;
    this.name = name;
    this.kind = kind;
    Object.freeze(this);
  }

  /** The column name quoted for SQL. */
  get quoted(): string {
    return quoteIdentifier(this.name);
  }

  toString(): string {
    return `${this.sheet}.${this.name}`;
  }
}

/** A compiled predicate (or raw SQL) ready for a WHERE clause or a `set` value. */
export class Expression implements SqlFragment {
  readonly sql: string;
  readonly params: readonly SqlParam[];

  constructor(sql: string, params: readonly SqlParam[] = []) {
    this.sql = sql;
    this.params = params;
    Object.freeze(this);
  }

  toString(): string {
    return this.sql;
  }
}

/** Anything `fetch`, `delete`, `set` and `aggregate` accept as a filter. */
export type Query = Expression | ReadonlyArray<Expression>;

/** A value a predicate compares a column of type `T` against. */
export type Comparable<T> = Exclude<T, null>;

function assertField(field: unknown, builder: string): asserts field is Field {
  if (!(field instanceof Field)) {
    throw new MudletDbError(
      "argument",
      `${builder} takes a field reference such as mydb.sheet.column, not ${typeof field}`,
    );
  }
}

function operand(field: Field, value: unknown, builder: string): SqlFragment {
  if (value instanceof Expression) return value;
  if (value === null || value === undefined) {
    throw new MudletDbError(
      "argument",
      `${builder}(${field}) cannot compare against ${value}; use isNull()/isNotNull() for NULL`,
    );
  }
  return writeValue(field.kind, value, `${builder}(${field})`);
}

function compare(field: Field, operator: string, value: unknown, builder: string): Expression {
  assertField(field, builder);
  const right = operand(field, value, builder);
  return new Expression(`${field.quoted} ${operator} ${right.sql}`, right.params);
}

function compareFolded(field: Field, operator: string, value: unknown, builder: string): Expression {
  assertField(field, builder);
  const right = operand(field, value, builder);
  return new Expression(`lower(${field.quoted}) ${operator} lower(${right.sql})`, right.params);
}

/** `column == value`; with `caseInsensitive`, both sides are lower-cased first. */
export function eq<T>(field: Field<T>, value: Comparable<T> | Expression, caseInsensitive = false): Expression {
  return caseInsensitive ? compareFolded(field, "==", value, "eq") : compare(field, "==", value, "eq");
}

export function notEq<T>(field: Field<T>, value: Comparable<T> | Expression, caseInsensitive = false): Expression {
  return caseInsensitive ? compareFolded(field, "!=", value, "notEq") : compare(field, "!=", value, "notEq");
}

export function lt<T>(field: Field<T>, value: Comparable<T> | Expression): Expression {
  return compare(field, "<", value, "lt");
}

export function lte<T>(field: Field<T>, value: Comparable<T> | Expression): Expression {
  return compare(field, "<=", value, "lte");
}

export function gt<T>(field: Field<T>, value: Comparable<T> | Expression): Expression {
  return compare(field, ">", value, "gt");
}

export function gte<T>(field: Field<T>, value: Comparable<T> | Expression): Expression {
  return compare(field, ">=", value, "gte");
}

export function isNull(field: Field): Expression {
  assertField(field, "isNull");
  return new Expression(`${field.quoted} IS NULL`);
}

export function isNotNull(field: Field): Expression {
  assertField(field, "isNotNull");
  return new Expression(`${field.quoted} IS NOT NULL`);
}

/** SQL LIKE: `_` matches one character, `%` any run; case-insensitive for ASCII. */
export function like(field: Field, pattern: string): Expression {
  return compare(field, "LIKE", pattern, "like");
}

export function notLike(field: Field, pattern: string): Expression {
  return compare(field, "NOT LIKE", pattern, "notLike");
}

function range(field: Field, lower: unknown, upper: unknown, builder: string, negate: boolean): Expression {
  assertField(field, builder);
  const low = operand(field, lower, builder);
  const high = operand(field, upper, builder);
  return new Expression(
    `${field.quoted} ${negate ? "NOT BETWEEN" : "BETWEEN"} ${low.sql} AND ${high.sql}`,
    [...low.params, ...high.params],
  );
}

/** Inclusive on both bounds, as SQL BETWEEN is. */
export function between<T>(field: Field<T>, lower: Comparable<T>, upper: Comparable<T>): Expression {
  return range(field, lower, upper, "between", false);
}

export function notBetween<T>(field: Field<T>, lower: Comparable<T>, upper: Comparable<T>): Expression {
  return range(field, lower, upper, "notBetween", true);
}

function membership(field: Field, values: readonly unknown[], builder: string, negate: boolean): Expression {
  assertField(field, builder);
  if (!Array.isArray(values)) {
    throw new MudletDbError("argument", `${builder}(${field}) takes a list of values`);
  }
  const parts = values.map((value) => operand(field, value, builder));
  return new Expression(
    `${field.quoted} ${negate ? "NOT IN" : "IN"} (${parts.map((part) => part.sql).join(",")})`,
    parts.flatMap((part) => [...part.params]),
  );
}

/** Mudlet's `db:in_`: the column equals any of the values. */
export function isIn<T>(field: Field<T>, values: ReadonlyArray<Comparable<T>>): Expression {
  return membership(field, values, "isIn", false);
}

export function notIn<T>(field: Field<T>, values: ReadonlyArray<Comparable<T>>): Expression {
  return membership(field, values, "notIn", true);
}

/**
 * Raw SQL, passed through untouched. Use it for what the builders cannot say
 * (`exp("length(name) > 10")`, `exp("kills + 1")` as a `set` value). Values
 * still belong in `params`, not in the text.
 */
export function exp(sql: string, params: readonly SqlParam[] = []): Expression {
  if (typeof sql !== "string" || sql.trim() === "") {
    throw new MudletDbError("argument", "exp() needs SQL text");
  }
  return new Expression(sql, params);
}

function assertExpressions(expressions: readonly unknown[], builder: string): asserts expressions is Expression[] {
  for (const expression of expressions) {
    if (!(expression instanceof Expression)) {
      throw new MudletDbError(
        "argument",
        `${builder}() combines expressions built with eq(), like() and the other builders, not ${typeof expression}`,
      );
    }
  }
}

/** All of the expressions must hold. */
export function and(...expressions: Expression[]): Expression {
  assertExpressions(expressions, "and");
  if (expressions.length === 0) throw new MudletDbError("argument", "and() needs at least one expression");
  return new Expression(
    `(${expressions.map((expression) => `(${expression.sql})`).join(" AND ")})`,
    expressions.flatMap((expression) => [...expression.params]),
  );
}

/** Any of the expressions may hold. */
export function or(...expressions: Expression[]): Expression {
  assertExpressions(expressions, "or");
  if (expressions.length === 0) throw new MudletDbError("argument", "or() needs at least one expression");
  return new Expression(
    `(${expressions.map((expression) => `(${expression.sql})`).join(" OR ")})`,
    expressions.flatMap((expression) => [...expression.params]),
  );
}

/**
 * Compile a query argument to a WHERE fragment (without the keyword). A list
 * of expressions is ANDed, as DB.lua does with a table of them.
 */
export function compileQuery(query: Query | undefined, context: string): SqlFragment | null {
  if (query === undefined) return null;
  if (query instanceof Expression) return query;
  if (Array.isArray(query)) {
    if (query.length === 0) return null;
    return and(...query);
  }
  throw new MudletDbError(
    "argument",
    `${context}: a query is an expression from eq()/like()/and()/... or a list of them; ` +
      `for a plain object use sheet.where({ ... })`,
  );
}

export interface OrderOptions {
  /** Fields to sort by, in priority order. */
  readonly orderBy?: Field | ReadonlyArray<Field>;
  /** Sort every `orderBy` field descending. Ascending is the default. */
  readonly descending?: boolean;
  /** Return at most this many rows. Not part of Mudlet's interface. */
  readonly limit?: number;
  /** Skip this many rows first; only meaningful with `limit`. */
  readonly offset?: number;
}

export function compileOrder(options: OrderOptions | undefined, context: string): string {
  if (!options) return "";
  const fields = options.orderBy === undefined ? [] : Array.isArray(options.orderBy) ? options.orderBy : [options.orderBy];
  let sql = "";
  if (fields.length > 0) {
    const terms = fields.map((field) => {
      assertField(field, `${context} orderBy`);
      return `${field.quoted}${options.descending ? " DESC" : ""}`;
    });
    sql += ` ORDER BY ${terms.join(",")}`;
  }
  if (options.limit !== undefined) {
    if (!Number.isInteger(options.limit) || options.limit < 0) {
      throw new MudletDbError("argument", `${context}: limit must be a non-negative integer`);
    }
    sql += ` LIMIT ${options.limit}`;
    if (options.offset !== undefined) {
      if (!Number.isInteger(options.offset) || options.offset < 0) {
        throw new MudletDbError("argument", `${context}: offset must be a non-negative integer`);
      }
      sql += ` OFFSET ${options.offset}`;
    }
  } else if (options.offset !== undefined) {
    throw new MudletDbError("argument", `${context}: offset needs a limit`);
  }
  return sql;
}
