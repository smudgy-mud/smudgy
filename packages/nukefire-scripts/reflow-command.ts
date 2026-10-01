export type NukeFireReflowMode = "bounded" | "perfect";

export const BOUNDED_REFLOW_PLANNING_PASSES = 8;
export const BOUNDED_REFLOW_TIMEOUT_MS = 30_000;

export interface NukeFireReflowScale {
  readonly residentCount: number;
  readonly edgeCount: number;
}

export interface NukeFirePerfectRepairPolicy {
  readonly when: "always";
  readonly maxDurationMs: number;
  readonly maxRestarts: number;
  readonly maxLayouts: number;
  readonly maxPolishTournaments: number;
  readonly maxPolishPasses: number;
  readonly maxExtensionStates: number;
  readonly maxLiveSearchNodes: number;
  readonly maxMaskDiversifications: number;
  readonly maxCrossingWork: number;
}

export interface NukeFirePerfectRepairPolicyWire
  extends Omit<NukeFirePerfectRepairPolicy, "maxDurationMs"> {
  readonly maxDurationMs: "infinity";
}

const SMALL_MAP_CEILINGS = {
  maxRestarts: 100_000,
  maxLayouts: 8,
  maxPolishTournaments: 16,
  maxPolishPasses: 16,
  maxExtensionStates: 4_194_304,
  maxLiveSearchNodes: 32_768,
  maxMaskDiversifications: 512,
  maxCrossingWork: 4_096,
} as const;

const LARGEST_MAP_FLOORS = {
  maxRestarts: 32_768,
  maxLayouts: 2,
  maxPolishTournaments: 2,
  maxPolishPasses: 3,
  maxExtensionStates: 32_768,
  maxLiveSearchNodes: 4_096,
  maxMaskDiversifications: 64,
  maxCrossingWork: 512,
} as const;

const LARGEST_MAP_PRESSURE = 262_144;

/** Side-effect-free command copy; a parity test pins it to the mapper policy. */
export function nukeFirePerfectReflowPolicy(
  scale: Readonly<NukeFireReflowScale>,
): NukeFirePerfectRepairPolicy {
  const residents = Math.max(1, Math.floor(scale.residentCount));
  const edges = Math.max(0, Math.floor(scale.edgeCount));
  const pressure = residents * Math.max(1, residents + edges);
  const budget = (key: keyof typeof SMALL_MAP_CEILINGS): number => Math.min(
    SMALL_MAP_CEILINGS[key],
    Math.max(
      LARGEST_MAP_FLOORS[key],
      Math.floor((LARGEST_MAP_FLOORS[key] * LARGEST_MAP_PRESSURE) / pressure),
    ),
  );
  return {
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    maxRestarts: budget("maxRestarts"),
    maxLayouts: budget("maxLayouts"),
    maxPolishTournaments: budget("maxPolishTournaments"),
    maxPolishPasses: budget("maxPolishPasses"),
    maxExtensionStates: budget("maxExtensionStates"),
    maxLiveSearchNodes: budget("maxLiveSearchNodes"),
    maxMaskDiversifications: budget("maxMaskDiversifications"),
    maxCrossingWork: budget("maxCrossingWork"),
  };
}

export function perfectRepairPolicyWire(
  policy: Readonly<NukeFirePerfectRepairPolicy>,
): NukeFirePerfectRepairPolicyWire {
  const { maxDurationMs: _unbounded, ...limits } = policy;
  return { ...limits, maxDurationMs: "infinity" };
}

export interface ReflowRepairReport {
  readonly geometricFixedPoint: boolean;
  readonly cutoff?: string;
  readonly polishCutoff?: string;
  readonly extensionSearch?: { readonly cancelled: boolean; readonly exhausted?: boolean };
  readonly crossingRepair?: { readonly cancelled: boolean; readonly exhausted?: boolean };
}

export type ReflowRepairTerminal =
  | "fixed-point"
  | "ceiling"
  | "timeout"
  | "cancelled"
  | "error"
  | "incomplete";

/** Shared command wording follows the mapper's durable terminal precedence. */
export function reflowRepairTerminalReason(
  report: Readonly<ReflowRepairReport>,
): ReflowRepairTerminal {
  if (report.geometricFixedPoint) return "fixed-point";
  if (report.cutoff === "time" || report.polishCutoff === "time") return "timeout";
  if (report.extensionSearch?.cancelled || report.crossingRepair?.cancelled) return "cancelled";
  if (report.polishCutoff === "error") return "error";
  const ceiling = report.cutoff === "restarts" || report.cutoff === "layouts" ||
    report.cutoff === "extensions" || report.cutoff === "masks" ||
    report.polishCutoff === "tournaments" || report.polishCutoff === "passes" ||
    report.extensionSearch?.exhausted === true || report.crossingRepair?.exhausted === true;
  return ceiling ? "ceiling" : "incomplete";
}

/** Empty arguments select the ordinary bounded command; `perfect` is explicit. */
export function parseReflowMode(argumentsText: string): NukeFireReflowMode | undefined {
  const argument = argumentsText.trim().toLowerCase();
  if (!argument) return "bounded";
  return argument === "perfect" ? "perfect" : undefined;
}
