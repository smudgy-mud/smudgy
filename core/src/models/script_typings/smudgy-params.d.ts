// =============================================================================
//  smudgy:params — TypeScript declarations  (GENERATED — DO NOT EDIT)
// =============================================================================
//  smudgy writes and overwrites this file every time a session starts. It types the
//  per-package `smudgy:params` module, which reads and writes this package's configured
//  parameter values (the `params` block of its smudgy.package.json).
//
//  Edits here are lost on the next launch.
// =============================================================================

declare module "smudgy:params" {
  /** A scalar value stored by a `string`, `number`, `boolean`, or `dropdown` parameter. */
  type ParamScalar = string | number | boolean;

  /**
   * A configured parameter value. Simple parameters store one {@link ParamScalar}; a `list`
   * parameter
   * stores an array of its element values; a `table` param stores an array of row objects
   * keyed by column.
   */
  type ParamValue = ParamScalar | ParamScalar[] | Record<string, ParamScalar>[];

  /**
   * Read one of your package's configured parameters by key (a `params[].key` from its
   * `smudgy.package.json`). Returns the saved value, or
   * `undefined`/`null` when the parameter is unset (or when the caller isn't a package).
   * Secret parameters come back as plain strings; a `dropdown` returns the chosen option's
   * value.
   *
   * ```ts
 * import { get } from "smudgy:params";
 * const url = get("pg.url");       // ParamValue | null | undefined
 * if (typeof url === "string") console.log(`Configured URL: ${url}`);
   *
 * const routes = get("routes");    // a `table` param -> array of row objects
 * if (Array.isArray(routes)) {
 *   for (const row of routes) {
 *     if (typeof row === "object" && row !== null) console.log(row.from, row.via);
 *   }
 * }
   * ```
   */
  export function get(key: string): ParamValue | null | undefined;

  /**
   * Save one of your package's declared parameters.
   *
   * The key must occur in `params` in the active `smudgy.package.json`. The value must match
   * that declaration. For example, a `boolean` parameter accepts only a boolean, and a
   * `dropdown` accepts only a declared option value. Lists must contain values of their declared
   * element type. Tables must contain row objects with declared columns and correctly typed cells.
   *
   * Smudgy saves the value in the package's configured global or profile scope. It stores a
   * declared secret in the operating system's credential store. The new value is available to
   * {@link get} immediately.
   *
   * This function throws a `TypeError` if the caller is not a package, the key is not declared,
   * or the value has the wrong shape. It also throws if Smudgy cannot save the value.
   *
   * ```ts
   * import { get, set } from "smudgy:params";
   *
   * set("enabled", true);
   * console.log(get("enabled")); // true
   *
   * set("routes", [{ from: "square", priority: 10 }]);
   * ```
   *
   * @param key A key declared in the package manifest.
   * @param value A value that matches the declared parameter type.
   */
  export function set(key: string, value: ParamValue): void;

  /** The default export bundles the same {@link get} and {@link set} functions. */
  const params: { get: typeof get; set: typeof set };
  export default params;
}
