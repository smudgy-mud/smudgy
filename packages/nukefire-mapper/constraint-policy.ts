export interface NukeFireConstraintRepairPolicy {
  when: "defects" | "always";
  maxDurationMs: number;
  maxRestarts?: number;
  maxLayouts?: number;
  maxPolishTournaments?: number;
  maxPolishPasses?: number;
  maxExtensionStates?: number;
  maxLiveSearchNodes?: number;
  maxMaskDiversifications?: number;
  maxCrossingWork: number;
}

export interface NukeFireLayoutScale {
  residentCount: number;
  edgeCount: number;
}

interface DeterministicRepairLimits {
  maxRestarts: number;
  maxLayouts: number;
  maxPolishTournaments: number;
  maxPolishPasses: number;
  maxExtensionStates: number;
  maxLiveSearchNodes: number;
  maxMaskDiversifications: number;
  maxCrossingWork: number;
}

/** Ceilings every budget rests on for the smallest maps. */
const SMALL_MAP_CEILINGS: DeterministicRepairLimits = {
  maxRestarts: 100_000,
  maxLayouts: 8,
  maxPolishTournaments: 16,
  maxPolishPasses: 16,
  maxExtensionStates: 4_194_304,
  maxLiveSearchNodes: 32_768,
  maxMaskDiversifications: 512,
  maxCrossingWork: 4_096,
};

/** Floors every budget rests on at and beyond the largest documented shape. */
const LARGEST_MAP_FLOORS: DeterministicRepairLimits = {
  maxRestarts: 32_768,
  maxLayouts: 2,
  maxPolishTournaments: 2,
  maxPolishPasses: 3,
  maxExtensionStates: 32_768,
  maxLiveSearchNodes: 4_096,
  maxMaskDiversifications: 64,
  maxCrossingWork: 512,
};

/**
 * Planning pressure of the largest documented map shape — 256 residents with
 * three directed edges each: 256 × (256 + 768) = 2^18. Every budget reaches
 * its floor exactly here and stays on it for anything larger.
 */
const LARGEST_MAP_PRESSURE = 262_144;

/**
 * How expensive one unit of search is on this map. Work per separator state
 * grows with residents + edges (feasibility and scoring inspect the graph),
 * so residents × (residents + edges) is a conservative work-pressure proxy.
 * Retained frontier memory is bounded independently by maxLiveSearchNodes.
 */
function planningPressure(scale: Readonly<NukeFireLayoutScale>): number {
  const residents = Math.max(1, Math.floor(scale.residentCount));
  const edges = Math.max(0, Math.floor(scale.edgeCount));
  return residents * Math.max(1, residents + edges);
}

/**
 * budget = clamp(floor(F × P_max / pressure), F, C), with F the largest-map
 * floor and C the small-map ceiling. Dividing a constant by the pressure
 * keeps total work near a fixed envelope; maxLiveSearchNodes applies the same
 * pressure curve to the independently bounded retained frontier. The formula
 * is monotonically non-increasing in residents and edges, and every quantity stays an exact
 * IEEE-754 integer far below 2^53, so identical inputs yield identical
 * budgets on every machine.
 */
function scaledBudget(pressure: number, floor: number, ceiling: number): number {
  const budget = Math.floor((floor * LARGEST_MAP_PRESSURE) / pressure);
  return Math.min(ceiling, Math.max(floor, budget));
}

function perfectRepairLimits(
  scale: Readonly<NukeFireLayoutScale>,
): DeterministicRepairLimits {
  // Work per state grows sharply with the number of residents and directed
  // edges. Small maps can afford millions of separator states, while the
  // largest observed Farmlands shape needs lower counts to keep the combined
  // search, polish, and crossing stages near a several-minute envelope.
  // Neighboring map sizes always receive neighboring budgets: there are no
  // step cliffs between one resident count and the next.
  const pressure = planningPressure(scale);
  const budget = (key: keyof DeterministicRepairLimits): number =>
    scaledBudget(pressure, LARGEST_MAP_FLOORS[key], SMALL_MAP_CEILINGS[key]);
  return {
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

/**
 * Bounded limits used by passive quiet reflow. A map whose ordinary plan has
 * no defect is left as it is, without a search.
 */
export function nukeFireAutomaticConstraintRepairPolicy(
  _scale: Readonly<NukeFireLayoutScale> = { residentCount: 0, edgeCount: 0 },
): NukeFireConstraintRepairPolicy {
  return {
    when: "defects",
    maxDurationMs: 10_000,
    maxRestarts: 4_096,
    maxLayouts: 2,
    maxPolishTournaments: 2,
    maxPolishPasses: 3,
    maxExtensionStates: 32_768,
    maxLiveSearchNodes: 4_096,
    maxMaskDiversifications: 64,
    maxCrossingWork: 512,
  };
}

/** Explicit high-effort policy used only by user-requested perfect reflow and tidying. */
export function nukeFirePerfectConstraintRepairPolicy(
  scale: Readonly<NukeFireLayoutScale> = { residentCount: 0, edgeCount: 0 },
): NukeFireConstraintRepairPolicy {
  return {
    // No wall deadline: deterministic frontier limits make the result
    // independent of machine speed while retaining a finite memory envelope.
    when: "always",
    maxDurationMs: Number.POSITIVE_INFINITY,
    ...perfectRepairLimits(scale),
  };
}

/** @deprecated Prefer the explicit automatic/perfect policy functions. */
export function nukeFireConstraintRepairPolicy(
  searchForPerfectLayouts: boolean,
  scale: Readonly<NukeFireLayoutScale> = { residentCount: 0, edgeCount: 0 },
): NukeFireConstraintRepairPolicy {
  return searchForPerfectLayouts
    ? nukeFirePerfectConstraintRepairPolicy(scale)
    : nukeFireAutomaticConstraintRepairPolicy(scale);
}
