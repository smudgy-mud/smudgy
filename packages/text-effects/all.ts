// Explicit eager catalogue. Prefer individual effect subpaths in scripts.
import { effectNames, loadEffects } from "./load.ts";

export type * from "./types.ts";

// Module evaluation waits for every component; callers still import Effects normally.
// The lazy registry is the single list of shipped effects and preserves their prop types.
export const Effects = await loadEffects(effectNames);
