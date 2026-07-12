// =============================================================================
//  smudgy:params — TypeScript declarations  (GENERATED — DO NOT EDIT)
// =============================================================================
//  smudgy writes and overwrites this file every time a session starts. It types the
//  per-package `smudgy:params` module, which reads this package's configured option
//  values (the `options` block of its smudgy.package.json — prompted at install).
//
//  Edits here are lost on the next launch.
// =============================================================================

declare module "smudgy:params" {
  /** A single option value: what a `string`/`number`/`boolean`/`dropdown` param stores. */
  type ParamScalar = string | number | boolean;

  /**
   * A configured option value. Simple params store one {@link ParamScalar}; a `list` param
   * stores an array of its element values; a `table` param stores an array of row objects
   * keyed by column.
   */
  type ParamValue = ParamScalar | ParamScalar[] | Record<string, ParamScalar>[];

  /**
   * Read one of your package's configured options by key (a `params[].key` from its
   * `smudgy.package.json`). Returns the value configured at install time, or
   * `undefined`/`null` when the option is unset (or when the caller isn't a package).
   * Secret options come back as plain strings; a `dropdown` returns the chosen option's
   * value.
   *
   * ```ts
   * import { get } from "smudgy:params";
   * const url = get("pg.url");       // ParamValue | null | undefined
   * if (typeof url === "string") connect(url);
   *
   * const routes = get("routes");    // a `table` param -> array of row objects
   * if (Array.isArray(routes)) {
   *   for (const row of routes) console.log(row.from, row.via);
   * }
   * ```
   */
  export function get(key: string): ParamValue | null | undefined;

  /** The default export bundles the same `get` accessor. */
  const params: { get: typeof get };
  export default params;
}
