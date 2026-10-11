// The root opens the interactive playground. Import /fire, /emphasis, etc. in scripts.
// Use /load for shader-free catalogue loaders, /types for types, or /all to opt into everything.
import "./showcase.tsx";

export type * from "./types.ts";
export { effectNames, loadEffect, loadEffects } from "./load.ts";
export type { EffectName, EffectMap } from "./load.ts";
