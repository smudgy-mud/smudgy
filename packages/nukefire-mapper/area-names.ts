/**
 * Which map an area name belongs in.
 *
 * The game names the area of the zone the player stands in. A table of rules
 * can send a name to another map ("Temple II" belongs in "Temple") or tell two
 * spellings apart ("DARK TEMPLE", exact case, is "Darker Temple"). A map is
 * identified by its folded name, so spellings that differ only in case share
 * one map.
 */

/** One row of the rules table. */
export interface AreaNameRule {
  readonly name: string;
  readonly map: string;
  readonly exactCase: boolean;
}

/** The map an area name belongs in. */
export interface MapName {
  /** The map's identity: its folded name, stored on the map. */
  readonly key: string;
  /** The map's title. */
  readonly display: string;
}

/** A name as displayed: trimmed, in Unicode NFC. */
function tidy(name: string): string {
  return name.trim().normalize("NFC");
}

/** A name as compared: tidied, then lowercased. */
export function fold(name: string): string {
  return tidy(name).toLowerCase().normalize("NFC");
}

/** The resolver built from a rules table. */
export class AreaNames {
  readonly #exact = new Map<string, string>();
  readonly #anyCase = new Map<string, string>();
  /** Rows that were skipped, each with its reason. */
  readonly problems: readonly string[];

  constructor(rows: unknown) {
    const problems: string[] = [];
    (Array.isArray(rows) ? rows : []).forEach((row, index) => {
      const problem = this.#add(row);
      if (problem) problems.push(`row ${index + 1}: ${problem}`);
    });
    this.problems = problems;
  }

  #add(row: unknown): string | undefined {
    const { name, map, exactCase } = (row ?? {}) as Partial<Record<keyof AreaNameRule, unknown>>;
    if (typeof name !== "string" || !tidy(name)) return "the area name is empty";
    if (typeof map !== "string" || !tidy(map)) return "the map name is empty";
    const [rules, spelling] = exactCase === true ? [this.#exact, tidy(name)] : [this.#anyCase, fold(name)];
    if (rules.has(spelling)) return `"${tidy(name)}" already has a rule`;
    rules.set(spelling, tidy(map));
    return undefined;
  }

  /** The map `observed` belongs in, or undefined when the game named no area. Exactly one rule applies. */
  resolve(observed: string): MapName | undefined {
    const name = tidy(observed);
    if (!name) return undefined;
    const display = this.#exact.get(name) ?? this.#anyCase.get(fold(name)) ?? name;
    return { key: fold(display), display };
  }
}

/**
 * The rules to save into settings, or undefined to leave them alone. An unset
 * table receives `defaults`, and a table still holding the defaults last saved
 * follows new ones; a table the player edited is theirs.
 */
export function rulesToSeed(
  defaults: readonly AreaNameRule[],
  saved: unknown,
  lastSeeded: unknown,
): readonly AreaNameRule[] | undefined {
  if (saved === null || saved === undefined) return defaults;
  const untouched = sameRules(saved, lastSeeded);
  return untouched && !sameRules(saved, defaults) ? defaults : undefined;
}

/** Compares tables by their cells, whatever order the settings store keeps object keys in. */
function sameRules(a: unknown, b: unknown): boolean {
  const cells = (rows: unknown) =>
    Array.isArray(rows)
      ? JSON.stringify(rows.map((row) => [row?.name, row?.map, row?.exactCase === true]))
      : undefined;
  const left = cells(a);
  return left !== undefined && left === cells(b);
}
