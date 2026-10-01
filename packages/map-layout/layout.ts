import {
  createLayoutWorkerClient,
  type LayoutWorkerClient,
  planIntegralLayoutInWorker,
} from "./worker-client.ts";
import { MAX_ROUTE_AMENDMENT_WAYPOINTS } from "./worker-protocol.ts";
import type { LayoutPlannerContext, LayoutPlannerProgress } from "./planner-state.ts";

/** An integral cell in Smudgy's map grid. */
export interface GridPosition {
  x: number;
  y: number;
  level: number;
}

export type LayoutDirection =
  | "North"
  | "East"
  | "South"
  | "West"
  | "Northeast"
  | "Northwest"
  | "Southeast"
  | "Southwest"
  | "Up"
  | "Down"
  | "In"
  | "Out"
  | "Special"
  | "Other";

/** One room visible in the current player-relative Map.Local chart. */
export interface LayoutNode {
  id: string;
  relative: GridPosition;
}

/** One room that already occupies the durable map. */
export interface LayoutResident {
  id: string;
  position: GridPosition;
  /** False reserves the cell but prevents automatic reflow of this room. */
  movable: boolean;
}

/** A directed topological constraint. */
export interface LayoutEdge {
  from: string;
  to: string;
  direction: LayoutDirection;
  /**
   * Optional authoritative unit vector for this particular traversal.
   *
   * This keeps semantic directions separate from their presentation. For
   * example, a same-level Up exit may use `{ x: 1, y: -1, level: 0 }` while
   * its reciprocal Down traversal uses the exact inverse. When omitted, the
   * normal cardinal/vertical vector for `direction` is used.
   */
  constraintVector?: GridPosition;
}

export interface IntegralLayoutRequest {
  nodes: readonly LayoutNode[];
  residents: readonly LayoutResident[];
  edges: readonly LayoutEdge[];
  centerId?: string;
  allowExistingMoves?: boolean;
  /** Optional diagnostic sink for candidate generation and accepted repairs. */
  trace?: (event: LayoutTraceEvent) => void;
}

/** Synchronous-only candidate admission used by constraint-aware polish. */
export interface IntegralLayoutControl {
  /** A complete candidate must satisfy this predicate before it can compete or be traced. */
  acceptsPositions?: (positions: ReadonlyMap<string, GridPosition>) => boolean;
  /**
   * False skips axis-group compaction (gravity and squeeze), the most
   * expensive stage of a planner or compaction pass. Constraint repair's
   * early lane turns it off; it never crosses a Worker boundary.
   */
  axisGroupCompaction?: boolean;
}

/** Synchronous controls for the bounded, compaction-only layout pass. */
export interface IntegralLayoutCompactionControl extends IntegralLayoutControl {
  /** Stop between gravity/vacuum transactions and return the best complete plan found so far. */
  shouldCancel?: () => boolean;
}

export interface ConstraintRepairOptions {
  /**
   * Which standard plans the repair runs on. "settled-regression" runs it
   * when the plan draws the exits between existing rooms worse than those
   * rooms had them: a higher count of directional and routing violations,
   * each directional violation weighing `DIRECTIONAL_VIOLATION_WEIGHT`, or
   * `LEVEL_VIOLATION_WEIGHT` when it is mis-levelled, as in
   * `compareLayoutQuality`, or an equal count with more mis-levelled, then
   * directional, violations; "violation-regression" compares every exit the same way, so
   * newly observed topology counts too. "defects" runs it on any plan with a
   * directional, routing or crossing defect. "always" runs it on every plan,
   * and every other mode returns a plan without such defects as it is.
   */
  when: "settled-regression" | "violation-regression" | "defects" | "always";
  /** Cooperative search ceiling. The final bounded polish may finish after it. */
  maxDurationMs?: number;
  /**
   * Deterministic ceiling for the fallback restart search. Mask selection
   * certifies optimality by exact hitting set; restarts run only when that
   * certification hits its own deterministic budgets.
   */
  maxRestarts?: number;
  /** Complete compact layouts to polish before stopping; Infinity searches until perfect/cancelled. */
  maxLayouts?: number;
  /** Complete multi-anchor polish tournaments to run; Infinity continues to fixed point/cancellation. */
  maxPolishTournaments?: number;
  /**
   * Aggregate integral-planner passes shared by the early preview and every
   * later polish tournament. Infinity continues to the tournament ceiling.
   */
  maxPolishPasses?: number;
  /**
   * Geometric separator states to build across every canonical relaxation
   * mask. Every state is charged, including states which still collide.
   * Infinity continues until the extension frontier is exhausted, a perfect
   * plan is found, or the request is cancelled.
   */
  maxExtensionStates?: number;
  /**
   * Maximum separator-search nodes retained at once, independently of the
   * total state budget. Infinity disables the production memory ceiling.
   */
  maxLiveSearchNodes?: number;
  /**
   * Distinct canonical relation-group masks to compact, including the master
   * search incumbent. Infinity continues until masks are exhausted, a perfect
   * plan is found, or the request is cancelled.
   */
  maxMaskDiversifications?: number;
  /** Crossing macro-expansion ceiling; Infinity runs until perfect, exhausted, or cancelled. */
  maxCrossingWork?: number;
}

export interface LayoutWorkerControlOptions {
  /** Cancel queued or active Worker planning from the caller realm. */
  signal?: AbortSignal;
  /** Hard parent-side wall-clock ceiling for the complete Worker operation. */
  timeoutMs?: number;
}

export interface IntegralLayoutAsyncOptions extends LayoutWorkerControlOptions {
  /** Optional whole-layout constraint search, executed only inside the Worker. */
  constraintRepair?: ConstraintRepairOptions;
  /** Receives throttled work counters and every complete best-so-far layout. */
  onProgress?: (progress: Readonly<LayoutPlannerProgress>) => void;
  /** Override the pre-plan quality shown by telemetry consumers. */
  currentQuality?: Readonly<LayoutQuality>;
  /** Map-free caller identity copied into every live planner snapshot. */
  plannerContext?: Readonly<LayoutPlannerContext>;
}

export interface ConstraintRepairWorkStats {
  /** Hard-valid compactor outputs which were freshly evaluated. */
  rawIncumbents: number;
  /** Raw incumbents which strictly improved the public quality frontier. */
  softIncumbents: number;
  /** Canonical position maps observed across every compaction mask. */
  distinctLayouts: number;
  /** Distinct canonical relation-group masks sent to geometric compaction. */
  maskDiversifications: number;
  /** Separator states built, including states which still contain collisions. */
  separatorStates: number;
  /** Separator alternatives attempted, including later cycle/no-op prunes. */
  separatorBranches: number;
  /** Separator choices rejected because they would close an axis cycle. */
  separatorCyclePrunes: number;
  /** Largest live separator-search frontier, aggregated across compacted masks. */
  peakLiveSearchNodes?: number;
  /** Full position snapshots materialized lazily from separator scratch. */
  candidateMaterializations?: number;
  /** Milliseconds from repair start to the first hard-valid compactor output. */
  firstIncumbentMs?: number;
}

/**
 * How a constraint repair ended. "searched": it ran its constraint search and
 * carried the result through compaction, polish and crossing repair. Every
 * other value says why it returned the standard plan as it was instead:
 *
 * - "locked": the request forbids moving existing rooms.
 * - "no-regression": the plan does not regress the way `when` asks for.
 * - "clean": the plan has no directional, routing or crossing defect.
 * - "no-constraints": no exit is one the search can express, a single step
 *   along one axis; diagonals and the like are only measured.
 * - "no-budget": `maxDurationMs` is not positive.
 * - "search-failed:analysis": the search's first check found the relations
 *   the standard plan keeps contradictory. A plan that keeps them shows they
 *   are not, so only an inconsistent plan reaches this, such as one that puts
 *   two rooms on one cell.
 * - "search-failed:time": the deadline passed before the search finished its
 *   first check.
 * - "search-failed:work": the search's check budget ran out before its first
 *   check.
 * - "no-layout": the search ran, but no compaction produced a layout that
 *   keeps the exits it retained; `cutoff` says whether a deadline or a work
 *   ceiling stopped it first.
 *
 * Every outcome but "search-failed:time", and a "no-layout" whose `cutoff` is
 * "time", depends only on the map and the request; those two depend on how
 * fast the machine ran.
 */
export type ConstraintRepairOutcome =
  | "searched"
  | "locked"
  | "no-regression"
  | "clean"
  | "no-constraints"
  | "no-budget"
  | "search-failed:analysis"
  | "search-failed:time"
  | "search-failed:work"
  | "no-layout";

/**
 * What a constraint repair did. A report whose `outcome` is not "searched"
 * selects nothing and proves nothing: `selected`, `constraintOptimal` and
 * `geometricFixedPoint` are false, `polishCutoff` is "none", its final
 * violation counts are the standard plan's, and its work counts cover only
 * what ran before the repair stopped.
 */
export interface ConstraintRepairReport extends ConstraintRepairWorkStats {
  trigger: ConstraintRepairOptions["when"];
  /** Whether the repair searched, or why it returned the standard plan instead. */
  outcome: ConstraintRepairOutcome;
  selected: boolean;
  /** True when the constraint search proved its minimum relaxed-edge objective. */
  constraintOptimal: boolean;
  /** Deprecated alias for `constraintOptimal`; retained for protocol compatibility. */
  optimal: boolean;
  cutoff: "none" | "time" | "restarts" | "layouts" | "extensions" | "masks";
  lowerBound: number;
  /** Directed exits the winner's relaxation may draw wrong. */
  relaxedEdges: number;
  /** Those of `relaxedEdges` whose exact reverse exit also exists. */
  reciprocalRelaxedEdges: number;
  /**
   * Level relations the winner's relaxation gives up. A level relation is the
   * shared level of the flat exits between two rooms along one ray (a
   * reciprocal pair has one). Given up, the rooms may lie on different levels
   * and those exits, which `relaxedEdges` counts too, are drawn wrong. The
   * search weighs a level relation as those exits, and of two equally costly
   * relaxations gives up fewer level relations. Only rooms whose levels the
   * exits contradict (an Up/Down exit between rooms that flat exits join, or a
   * cycle of Up/Down exits) or the standard plan already breaks carry level
   * relations; everywhere else a flat exit's shared level is never given up.
   */
  relaxedLevelRelations: number;
  standardViolations: number;
  finalViolations: number;
  beforeViolations: number;
  standardRoutingViolations: number;
  finalRoutingViolations: number;
  beforeRoutingViolations: number;
  beforeSettledViolations: number;
  standardSettledViolations: number;
  beforeSettledRoutingViolations: number;
  standardSettledRoutingViolations: number;
  restarts: number;
  feasibilityChecks: number;
  layoutsConsidered: number;
  compactionAttempts: number;
  /** Aggregate termination of geometric separator DFS across compacted masks. */
  extensionSearch: {
    completed: boolean;
    cancelled: boolean;
    exhausted: boolean;
  };
  /** Termination of the distinct canonical-mask compaction frontier. */
  maskDiversification: {
    completed: boolean;
    exhausted: boolean;
  };
  searchMs: number;
  compactionMs: number;
  /** Complete multi-anchor tournaments run after constraint compaction. */
  polishTournaments: number;
  /** Integral planner passes evaluated by the preview and later tournaments. */
  polishPasses: number;
  /** Anchor choices evaluated across the preview and all polish tournaments. */
  polishAnchorsTried: number;
  /** Strict best-so-far improvements published during polishing. */
  polishImprovements: number;
  /**
   * True only when another complete tournament found no strict quality gain
   * and neither the crossing repair nor the final finishing improved on it.
   */
  geometricFixedPoint: boolean;
  /**
   * Why geometric polishing stopped independently of the constraint cutoff;
   * "none" when the repair returned before polishing (see `outcome`).
   */
  polishCutoff: "fixed-point" | "time" | "tournaments" | "passes" | "error" | "none";
  polishMs: number;
  crossingRepair: Readonly<CrossingRepairStats & {
    completed: boolean;
    cancelled: boolean;
    exhausted: boolean;
    elapsedMs: number;
  }>;
}

/**
 * A declarative per-link detour for a defect room movement can never resolve
 * because every participating room is immovable: a link crossing whose four
 * endpoint rooms are all fixed, or an obstructed link whose endpoints and
 * every obstructing room are all fixed. `waypoints` are the interior elbow
 * cells of an orthogonal route, in room-grid coordinates on the link's level,
 * ordered from room `from` to room `to`; the amended link is drawn
 * `from` → each waypoint → `to` instead of as the straight segment.
 *
 * Amendments are advisory presentation-layer truth. They never change room
 * positions, and the plan's quality tuple still scores the straight segment —
 * `linkCrossings` and `roomObstructions` keep counting the geometric defect
 * rather than pretending the detour resolved it. A consumer that ignores the
 * field loses nothing it has today.
 */
export interface RouteAmendment {
  from: string;
  to: string;
  waypoints: readonly GridPosition[];
}

export interface IntegralLayoutPlan {
  /** Final cells for every resident and every node in the request. */
  positions: ReadonlyMap<string, GridPosition>;
  /** Resident ids whose durable coordinates changed. */
  movedExisting: ReadonlySet<string>;
  /** The geometry tuple `compareLayoutQuality` selected this layout by. */
  quality: Readonly<LayoutQuality>;
  /**
   * Present whenever the opt-in whole-layout constraint repair returned,
   * including when it did not search. A requested repair that never reported
   * leaves it absent: a parent-side deadline, a cancellation or a failed
   * Worker resolves the retained plan instead.
   */
  constraintRepair?: Readonly<ConstraintRepairReport>;
  /**
   * Advisory detours for this plan's permanent fixed-room defects; see
   * RouteAmendment. Present only when at least one detour was found, and
   * never when the request disabled existing-room movement.
   */
  routeAmendments?: readonly RouteAmendment[];
}

export interface CrossingRepairStats {
  /** Crossing pairs selected as transaction targets. */
  crossingsConsidered: number;
  /** Macro expansions attempted, including non-publishable continuation states. */
  macrosConsidered: number;
  /** Geometry-safe one-cell push closures applied inside those transactions. */
  pushClosures: number;
  /** Deepest nested bridge transaction entered. */
  maxDepth: number;
  /** Canonical transaction states admitted after deduplication. */
  visitedStates: number;
}

interface CrossingRepairProgressBase extends CrossingRepairStats {
  bestQuality: Readonly<LayoutQuality>;
}

export type CrossingRepairProgress =
  | CrossingRepairProgressBase & { kind: "progress" | "complete" }
  | CrossingRepairProgressBase & {
    kind: "improvement";
    /**
     * Always complete, including positions: a transaction result at the
     * cheap compaction fixed point.
     */
    candidate: LayoutTraceCandidate;
  };

export interface CrossingRepairControl {
  /** Macro-expansion budget. Infinity searches until perfect, exhausted, or cancelled. */
  maximumWork?: number;
  /** Cooperative cancellation hook, checked between and within transactions. */
  shouldCancel?: () => boolean;
  /** Receives cumulative counters; intermediate transaction geometry is never exposed. */
  onProgress?: (progress: Readonly<CrossingRepairProgress>) => void;
  /** Reject complete transaction candidates before frontier admission or publication. */
  acceptsPositions?: (positions: ReadonlyMap<string, GridPosition>) => boolean;
  /**
   * False skips the axis-group compaction in the deep repair's axis heal,
   * which the macro budget does not bound. Constraint repair's early lane
   * turns it off; it never crosses a Worker boundary.
   */
  axisGroupCompaction?: boolean;
}

export interface CrossingRepairResult {
  /**
   * The last layout published, the best complete plan, or the exact supplied
   * seed when no strict improvement exists.
   */
  plan: IntegralLayoutPlan;
  /** True when crossings reached zero or the deterministic search space was exhausted. */
  completed: boolean;
  cancelled: boolean;
  /** True when the macro-expansion budget stopped the search. */
  exhausted: boolean;
  stats: Readonly<CrossingRepairStats>;
  /**
   * Advisory detours for the returned plan's permanent fixed-room defects;
   * see RouteAmendment. Mirrored onto `plan.routeAmendments` when the search
   * improved the seed; the exact-seed fallback keeps `plan` reference-equal
   * to the caller's object, so this field is the only carrier in that case.
   */
  routeAmendments?: readonly RouteAmendment[];
}

interface RayQuality {
  cardinalRayViolations: number;
  /** Violating directed edges which have an exact protected reverse edge. */
  reciprocalRayViolations: number;
  /** Violating directed edges whose rooms sit on levels the exit rules out. */
  levelViolations: number;
  /** Extra levels that exits drawn right climb or descend. */
  levelSlack: number;
  cardinalSlack: number;
}

interface ExitPortQuality {
  exitPortViolations: number;
  reciprocalExitPortViolations: number;
}

interface CandidateScore {
  // Each stage is scored on first use, so a comparison decided early never
  // pays for the later ones.
  collisions?: number;
  ray?: RayQuality;
  indexed?: PositionIndex;
  physicalEdges?: ScoredPhysicalEdge[];
  roomObstructions?: number;
  exitPorts?: ExitPortQuality;
  linkCrossings?: number;
  footprint?: FootprintQuality;
  footprintIndex?: Map<number, LevelCoordinateIndex>;
  movedExisting?: Set<string>;
  quality?: LayoutQuality;
  /** XOR-combined dedup-fingerprint lanes; see candidateFingerprintLanes. */
  fingerprintLanes?: [number, number];
  /** Cardinal-series decompositions of this candidate's geometry. */
  cardinalSeriesAll?: CardinalSeries[];
  cardinalSeriesReciprocal?: CardinalSeries[];
  /** Spacing penalties keyed by the definition set they were scored against. */
  spacingPenalties?: WeakMap<readonly CardinalSeries[], readonly [number, number]>;
  /** Cell -> occupant ids, shared by exit-port deltas derived from this base. */
  occupants?: Map<CellKey, string | string[]>;
  /** Port cell -> scoreable cardinal edges, shared by exit-port deltas. */
  portIndex?: Map<CellKey, LayoutEdge[]>;
}

interface CandidateDerivation {
  base: Candidate;
  changedIds: ReadonlySet<string>;
  /**
   * Present when this candidate is exactly its base with every changed id
   * translated by one shared integral offset. The annotation is a promise
   * made by the call site — it is never re-derived from the position maps —
   * and lets every geometry stage rescore from the moved/stationary boundary
   * instead of rebuilding whole-map indexes per one-cell trial.
   */
  translation?: RigidTranslation;
}

interface RigidTranslation {
  offset: Origin;
  /** Shared per-(base, moved-group) scoring caches, reused across offsets. */
  context: RigidTranslationContext;
}

interface Candidate {
  positions: Map<string, GridPosition>;
  current: ReadonlyMap<string, GridPosition>;
  edges: readonly LayoutEdge[];
  score: CandidateScore;
  derivation?: CandidateDerivation;
  cacheHash?: number;
  cacheEpoch?: number;
}

interface CandidateEvaluator {
  epoch: number;
  ids: readonly string[];
  idIndexes: Map<string, number>;
  cache: Map<number, Candidate[]>;
  maximumEntries?: number;
  recency?: Set<Candidate>;
}

const CANDIDATE_EVALUATORS = new WeakMap<ReadonlyMap<string, GridPosition>, CandidateEvaluator>();
const CANDIDATE_ID_INDEXES = new WeakMap<readonly string[], Map<string, number>>();
const POSITION_ADMISSION_CACHE = new WeakMap<
  ReadonlyMap<string, GridPosition>,
  { predicate: NonNullable<IntegralLayoutControl["acceptsPositions"]>; accepted: boolean }
>();
const ACTIVE_POSITION_ADMISSIONS: NonNullable<IntegralLayoutControl["acceptsPositions"]>[] = [];
let nextCandidateEvaluatorEpoch = 1;

function acceptedPositions(
  positions: ReadonlyMap<string, GridPosition>,
  predicate: IntegralLayoutControl["acceptsPositions"],
): boolean {
  if (!predicate) return true;
  const cached = POSITION_ADMISSION_CACHE.get(positions);
  if (cached?.predicate === predicate) return cached.accepted;
  const accepted = predicate(positions);
  POSITION_ADMISSION_CACHE.set(positions, { predicate, accepted });
  return accepted;
}

/**
 * Apply a synchronous candidate predicate to nested planner calls (notably
 * `planLayoutModel`) without adding a function to a Worker-cloneable request.
 */
export function withIntegralLayoutCandidateAdmission<T>(
  predicate: NonNullable<IntegralLayoutControl["acceptsPositions"]>,
  action: () => T,
): T {
  ACTIVE_POSITION_ADMISSIONS.push(predicate);
  try {
    return action();
  } finally {
    ACTIVE_POSITION_ADMISSIONS.pop();
  }
}

function candidateIdIndexes(ids: readonly string[]): Map<string, number> {
  const known = CANDIDATE_ID_INDEXES.get(ids);
  if (known) return known;
  const result = new Map(ids.map((id, index) => [id, index]));
  CANDIDATE_ID_INDEXES.set(ids, result);
  return result;
}

function beginCandidateEvaluatorEpoch(
  current: ReadonlyMap<string, GridPosition>,
  ids: readonly string[],
  maximumEntries: number,
): () => void {
  const previous = CANDIDATE_EVALUATORS.get(current);
  const evaluator: CandidateEvaluator = {
    epoch: nextCandidateEvaluatorEpoch++,
    ids,
    idIndexes: candidateIdIndexes(ids),
    cache: new Map(),
    maximumEntries,
    recency: new Set(),
  };
  CANDIDATE_EVALUATORS.set(current, evaluator);
  return () => {
    evaluator.cache.clear();
    evaluator.recency?.clear();
    if (CANDIDATE_EVALUATORS.get(current) !== evaluator) return;
    if (previous) CANDIDATE_EVALUATORS.set(current, previous);
    else CANDIDATE_EVALUATORS.delete(current);
  };
}

/**
 * A description of a layout. Every field is minimized. `compareLayoutQuality`
 * ranks layouts by a weighted score of directional violations, mis-levelled
 * exits among them, routing violations and crossings first, then by the
 * fields in `QUALITY_FIELDS` order; no amount of compactness can pay for a
 * protected directional exit leaving its proper ray.
 */
export interface LayoutQuality {
  /** Legacy field name: violations include every protected directional exit. */
  cardinalRayViolations: number;
  /** Directed violations belonging to reciprocal protected exit pairs. Always returned by 0.2.1+. */
  reciprocalRayViolations?: number;
  /**
   * The directional violations whose rooms sit on levels their exit rules
   * out: an exit on one level, such as a compass exit, joining rooms on
   * different levels, an Up exit whose room is not higher, or a Down exit
   * whose room is not lower. Level 2 is above level 1. The exit's own vector
   * decides, so a projected Up/Down exit drawn on one level is not counted.
   * Every measured quality has it; a quality without it counts none.
   */
  levelViolations?: number;
  /** Obstructed direct links plus blocked cardinal endpoint ports. */
  routingViolations: number;
  /** Directed cardinal exits whose first route cell is occupied by another room. */
  exitPortViolations: number;
  /** Blocked endpoint ports which belong to reciprocal protected exit pairs. */
  reciprocalExitPortViolations?: number;
  roomObstructions: number;
  linkCrossings: number;
  /**
   * The extra levels that exits drawn right climb or descend: an Up exit to
   * the room three levels up spans two more than it needs. Part of a map's
   * size, with its footprint. Every measured quality has it; a quality
   * without it counts none.
   */
  levelSlack?: number;
  /** Sum of the occupied bounding-box areas on each level. */
  footprintArea: number;
  /** Sum of the occupied bounding-box perimeters on each level. */
  footprintPerimeter: number;
  /** Extra cells along correctly oriented protected exits. */
  cardinalSlack: number;
}

const QUALITY_FIELDS: readonly (keyof LayoutQuality)[] = [
  "levelViolations",
  "cardinalRayViolations",
  "reciprocalRayViolations",
  "routingViolations",
  "exitPortViolations",
  "reciprocalExitPortViolations",
  "roomObstructions",
  "linkCrossings",
  "levelSlack",
  "footprintArea",
  "footprintPerimeter",
  "cardinalSlack",
];

/**
 * Routing violations and crossings, combined, that one directional violation
 * is worth in the quality order's primary score. A layout that removes a
 * directional violation is better only while that costs at most this many
 * extra routing violations and crossings; one that adds a directional
 * violation is better once it saves more than this many.
 */
export const DIRECTIONAL_VIOLATION_WEIGHT = 8;

/**
 * A mis-levelled exit's weight in the quality order's primary score, in place
 * of `DIRECTIONAL_VIOLATION_WEIGHT`: one outweighs 16 routing violations and
 * crossings combined, and two exits drawn wrong on their own level.
 */
export const LEVEL_VIOLATION_WEIGHT = 17;

/**
 * The quality order's primary score, lower is better. `level` counts the
 * mis-levelled exits among the `directional` violations.
 */
function weightedQualityScore(
  directional: number,
  level: number,
  routing: number,
  crossings: number,
): number {
  return DIRECTIONAL_VIOLATION_WEIGHT * (directional - level) + LEVEL_VIOLATION_WEIGHT * level +
    routing + crossings;
}

function layoutQualityScore(quality: LayoutQuality): number {
  return weightedQualityScore(
    quality.cardinalRayViolations ?? 0,
    quality.levelViolations ?? 0,
    quality.routingViolations ?? 0,
    quality.linkCrossings ?? 0,
  );
}

/**
 * Compare two public quality tuples in the planner's quality order. Positive
 * means `a` is better, zero means geometrically tied.
 *
 * The primary score weighs each directional violation
 * `DIRECTIONAL_VIOLATION_WEIGHT`, or `LEVEL_VIOLATION_WEIGHT` when it is
 * mis-levelled, and adds the routing violations and the crossings, lower
 * first. A violation is therefore traded for routing violations and
 * crossings, in either direction, at its weight. Layouts with equal scores
 * are ranked field by field in `QUALITY_FIELDS` order, fewest mis-levelled
 * exits first, and after every defect by size: the extra levels exits span,
 * then footprint. The order is total: it is lexicographic over the score and
 * then every field, so every sort and pairwise comparison agrees with it.
 */
export function compareLayoutQuality(a: LayoutQuality, b: LayoutQuality): number {
  const primary = layoutQualityScore(b) - layoutQualityScore(a);
  if (primary !== 0) return primary;
  for (const field of QUALITY_FIELDS) {
    const aValue = a[field] ?? 0;
    const bValue = b[field] ?? 0;
    if (aValue !== bValue) return bValue - aValue;
  }
  return 0;
}

/** Measure the planner's complete public quality tuple for fixed positions. */
export function measureIntegralLayoutQuality(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): LayoutQuality {
  // The scoring pipeline is read-only and its candidate-local indexes never
  // escape this synchronous call. Avoid cloning the complete position map
  // twice: constraint repair invokes this entry point for every separator
  // state and supplies a stable view for the duration of the call.
  return candidateQuality({
    positions: positions as Map<string, GridPosition>,
    current: positions,
    edges,
    score: {},
  });
}

export interface LayoutTraceCandidate {
  quality: LayoutQuality;
  movedExisting: string[];
  /** Omitted for rejected-candidate summaries to avoid duplicating whole maps. */
  positions?: { id: string; x: number; y: number; level: number }[];
}

export type LayoutTraceStage =
  | "stable"
  | "golden"
  | "exact-new"
  | "chart-reflow"
  | "all-candidates"
  | "initial-selection"
  | "greedy-cardinal-repair"
  | "link-obstruction-repair"
  | "crossing-repair"
  | "bridge-vacuum"
  | "vacuum"
  | "axis-compaction"
  | "constraint-repair"
  | "final-selection";

export type LayoutTraceEvent =
  | {
    type: "candidate-batch";
    stage: LayoutTraceStage;
    generated: number;
    collisionFree: number;
    best?: LayoutTraceCandidate;
  }
  | {
    /**
     * A whole-map request's layout as it stands before the axis-group pass,
     * the standard pass's longest stage, at the cheap compaction fixed point,
     * so a caller can show it while that pass runs. The plan never ranks below
     * it. Positions are always present.
     */
    type: "preview";
    stage: "axis-compaction";
    candidate: LayoutTraceCandidate;
  }
  | {
    type: "selection";
    stage: "initial-selection" | "final-selection";
    selected: LayoutTraceCandidate;
    /** Final selection only: the plan's advisory fixed-defect detours. */
    routeAmendments?: readonly RouteAmendment[];
  }
  | {
    type: "improvement";
    stage: "greedy-cardinal-repair";
    iteration: number;
    before: LayoutTraceCandidate;
    after: LayoutTraceCandidate;
  }
  | {
    type: "vacuum";
    stage: "vacuum";
    iteration: number;
    axis: "x" | "y";
    lower: number;
    upper: number;
    distance: number;
    moved: string[];
    before: LayoutTraceCandidate;
    after: LayoutTraceCandidate;
  }
  | {
    type: "obstruction-repair";
    stage: "link-obstruction-repair";
    iteration: number;
    edge: LayoutEdge;
    offset: GridPosition;
    obstructing: string[];
    moved: string[];
    before: LayoutTraceCandidate;
    after: LayoutTraceCandidate;
  }
  | {
    type: "obstruction-candidates";
    stage: "link-obstruction-repair";
    iteration: number;
    candidates: {
      edge: LayoutEdge;
      offset: GridPosition;
      obstructing: string[];
      moved: string[];
      result: LayoutTraceCandidate;
    }[];
  }
  | {
    type: "crossing-repair";
    stage: "crossing-repair";
    mode: "quick" | "deep";
    iteration: number;
    crossingsConsidered: number;
    macrosConsidered: number;
    pushClosures: number;
    maxDepth: number;
    visitedStates: number;
    before: LayoutTraceCandidate;
    /**
     * The published layout: an accepted complete transaction, finished as its
     * request's published layouts are. Positions are always present.
     */
    after: LayoutTraceCandidate;
  }
  | {
    type: "crossing-progress";
    stage: "crossing-repair";
    mode: "quick" | "deep";
    status: "progress" | "complete";
    crossingsConsidered: number;
    macrosConsidered: number;
    pushClosures: number;
    maxDepth: number;
    visitedStates: number;
    bestQuality: Readonly<LayoutQuality>;
  }
  | {
    type: "bridge-vacuum";
    stage: "bridge-vacuum";
    iteration: number;
    edge: LayoutEdge;
    movingEndpoint: string;
    offset: GridPosition;
    moved: string[];
    before: LayoutTraceCandidate;
    after: LayoutTraceCandidate;
  }
  | {
    type: "constraint-repair";
    stage: "constraint-repair";
    report: Readonly<ConstraintRepairReport>;
  }
  | {
    type: "constraint-progress";
    stage: "constraint-repair";
    phase: "search" | "compaction" | "polish";
    restarts: number;
    feasibilityChecks: number;
    layoutsConsidered: number;
    compactionAttempts: number;
    elapsedMs: number;
    bestQuality?: Readonly<LayoutQuality>;
    rawIncumbents: number;
    softIncumbents: number;
    distinctLayouts: number;
    maskDiversifications: number;
    separatorStates: number;
    separatorBranches: number;
    separatorCyclePrunes: number;
    firstIncumbentMs?: number;
  }
  | {
    type: "constraint-improvement";
    stage: "constraint-repair";
    restarts: number;
    feasibilityChecks: number;
    layoutsConsidered: number;
    compactionAttempts: number;
    rawIncumbents: number;
    softIncumbents: number;
    distinctLayouts: number;
    maskDiversifications: number;
    separatorStates: number;
    separatorBranches: number;
    separatorCyclePrunes: number;
    firstIncumbentMs?: number;
    candidate: LayoutTraceCandidate;
  }
  | {
    type: "axis-progress";
    stage: "axis-compaction";
    phase: "gravity" | "spacing";
    candidatesConsidered: number;
    complete: boolean;
    elapsedMs: number;
    bestQuality: Readonly<LayoutQuality>;
  };

interface Origin {
  x: number;
  y: number;
  level: number;
}

const CARDINAL_VECTORS: Partial<Record<LayoutDirection, GridPosition>> = {
  North: { x: 0, y: -1, level: 0 },
  East: { x: 1, y: 0, level: 0 },
  South: { x: 0, y: 1, level: 0 },
  West: { x: -1, y: 0, level: 0 },
};

const VERTICAL_VECTORS: Partial<Record<LayoutDirection, GridPosition>> = {
  Up: { x: 0, y: 0, level: 1 },
  Down: { x: 0, y: 0, level: -1 },
};

/** Directions whose geometry is an authoritative integral-grid constraint. */
const ORTHOGONAL_VECTORS: Partial<Record<LayoutDirection, GridPosition>> = {
  ...CARDINAL_VECTORS,
  ...VERTICAL_VECTORS,
};

const DIRECTION_VECTORS: Partial<Record<LayoutDirection, GridPosition>> = {
  ...ORTHOGONAL_VECTORS,
  Northeast: { x: 1, y: -1, level: 0 },
  Northwest: { x: -1, y: -1, level: 0 },
  Southeast: { x: 1, y: 1, level: 0 },
  Southwest: { x: -1, y: 1, level: 0 },
};

const SEARCH_RADIUS = 12;
const ISLAND_GAP = 4;
/** Small edits are cheaper to rescore from their exact edge/room deltas. */
const INCREMENTAL_SCORE_ROOM_LIMIT = 4;

/**
 * Code-unit string ordering. Layout selection must be identical across
 * installs, so no internal ordering may depend on the host ICU locale.
 */
function compareStrings(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

interface IndexedPhysicalLink {
  a: string;
  b: string;
  edges: LayoutEdge[];
}

interface LayoutTopologyIndex {
  incident: Map<string, LayoutEdge[]>;
  physical: IndexedPhysicalLink[];
  /** Same links keyed by their normalized `a|b` pair for O(1) lookups. */
  byPair: Map<string, IndexedPhysicalLink>;
}

const TOPOLOGY_INDEXES = new WeakMap<readonly LayoutEdge[], LayoutTopologyIndex>();

/** Cache topology-only indexes for the lifetime of one request's edge array. */
function topologyIndex(edges: readonly LayoutEdge[]): LayoutTopologyIndex {
  const known = TOPOLOGY_INDEXES.get(edges);
  if (known) return known;
  const incident = new Map<string, LayoutEdge[]>();
  const physicalByKey = new Map<string, IndexedPhysicalLink>();
  for (const edge of edges) {
    for (const id of edge.from === edge.to ? [edge.from] : [edge.from, edge.to]) {
      const values = incident.get(id) ?? [];
      values.push(edge);
      incident.set(id, values);
    }
    if (edge.from === edge.to) continue;
    const [a, b] = edge.from <= edge.to
      ? [edge.from, edge.to]
      : [edge.to, edge.from];
    const key = `${a}|${b}`;
    const link = physicalByKey.get(key) ?? { a, b, edges: [] };
    link.edges.push(edge);
    physicalByKey.set(key, link);
  }
  const result = {
    incident,
    physical: [...physicalByKey.values()].sort((a, b) =>
      compareStrings(a.a, b.a) || compareStrings(a.b, b.b)
    ),
    byPair: physicalByKey,
  };
  TOPOLOGY_INDEXES.set(edges, result);
  return result;
}

function integral(position: GridPosition): GridPosition {
  return {
    x: Math.round(position.x),
    y: Math.round(position.y),
    level: Math.round(position.level),
  };
}

function add(position: GridPosition, offset: Origin): GridPosition {
  return {
    x: position.x + offset.x,
    y: position.y + offset.y,
    level: position.level + offset.level,
  };
}

function subtract(a: GridPosition, b: GridPosition): Origin {
  return { x: a.x - b.x, y: a.y - b.y, level: a.level - b.level };
}

function samePosition(a: GridPosition, b: GridPosition): boolean {
  return a.x === b.x && a.y === b.y && a.level === b.level;
}

/**
 * Occupancy keys are packed integers on the hot path. The packing assumes
 * integral coordinates with |x|, |y| < 2^20 and |level| < 2^9 — comfortably
 * beyond any real map, and the guarded fallback returns the legacy string
 * key for anything outside that envelope (including the non-integral
 * positions public measurement entry points may receive), so distinct cells
 * can never collide: a number and a string are never equal as Map keys, and
 * each format is injective over its own domain. Both key kinds live in the
 * same Maps/Sets; insertion order, the only order these structures expose,
 * is unaffected by key type.
 */
type CellKey = number | string;

const CELL_AXIS_LIMIT = 1 << 20;
const CELL_LEVEL_LIMIT = 1 << 9;
const CELL_AXIS_SPAN = 1 << 21;

function cellKeyAt(x: number, y: number, level: number): CellKey {
  if (
    x >= -CELL_AXIS_LIMIT && x < CELL_AXIS_LIMIT &&
    y >= -CELL_AXIS_LIMIT && y < CELL_AXIS_LIMIT &&
    level >= -CELL_LEVEL_LIMIT && level < CELL_LEVEL_LIMIT &&
    Number.isInteger(x) && Number.isInteger(y) && Number.isInteger(level)
  ) {
    // Highest magnitude ~2^52, inside the safe-integer range.
    return ((level + CELL_LEVEL_LIMIT) * CELL_AXIS_SPAN + (x + CELL_AXIS_LIMIT)) *
        CELL_AXIS_SPAN + (y + CELL_AXIS_LIMIT);
  }
  return `${level}:${x}:${y}`;
}

function cellKey(position: GridPosition): CellKey {
  return cellKeyAt(position.x, position.y, position.level);
}

/**
 * The canonical textual cell key. Retained solely for positionMapKey, whose
 * strings participate in lexicographic tie-breaks and therefore may never
 * change shape.
 */
function cellKeyString(position: GridPosition): string {
  return `${position.level}:${position.x}:${position.y}`;
}

/** Row/column bucket key: a level paired with one perpendicular coordinate. */
function laneKey(level: number, coordinate: number): CellKey {
  if (
    coordinate >= -CELL_AXIS_LIMIT && coordinate < CELL_AXIS_LIMIT &&
    level >= -CELL_LEVEL_LIMIT && level < CELL_LEVEL_LIMIT &&
    Number.isInteger(coordinate) && Number.isInteger(level)
  ) {
    return (level + CELL_LEVEL_LIMIT) * CELL_AXIS_SPAN + (coordinate + CELL_AXIS_LIMIT);
  }
  return `${level}:${coordinate}`;
}

function offsetKey(offset: Origin): string {
  return `${offset.level}:${offset.x}:${offset.y}`;
}

function manhattan(a: GridPosition, b: GridPosition): number {
  return Math.abs(a.x - b.x) + Math.abs(a.y - b.y) + Math.abs(a.level - b.level);
}

function cardinalRayDistance(
  direction: LayoutDirection,
  delta: GridPosition,
): number | undefined {
  const expected = CARDINAL_VECTORS[direction];
  if (!expected || delta.level !== 0) return undefined;
  const onRay = expected.x !== 0
    ? delta.y === 0 && Math.sign(delta.x) === expected.x
    : delta.x === 0 && Math.sign(delta.y) === expected.y;
  if (!onRay) return undefined;
  return expected.x !== 0 ? Math.abs(delta.x) : Math.abs(delta.y);
}

/**
 * Distance along an authoritative integral ray. Unit vectors may be
 * cardinal, vertical, or diagonal; every non-zero component must advance by
 * the same positive multiple.
 */
function vectorRayDistance(
  expected: GridPosition,
  delta: GridPosition,
): number | undefined {
  let distance: number | undefined;
  for (const axis of ["x", "y", "level"] as const) {
    const unit = expected[axis];
    const actual = delta[axis];
    if (unit === 0) {
      if (actual !== 0) return undefined;
      continue;
    }
    if (Math.sign(actual) !== Math.sign(unit)) return undefined;
    const axisDistance = Math.abs(actual / unit);
    if (!Number.isInteger(axisDistance) || axisDistance <= 0) return undefined;
    if (distance !== undefined && distance !== axisDistance) return undefined;
    distance = axisDistance;
  }
  return distance;
}

function protectedVector(edge: LayoutEdge): GridPosition | undefined {
  return edge.constraintVector
    ? integral(edge.constraintVector)
    : ORTHOGONAL_VECTORS[edge.direction];
}

const RECIPROCAL_PROTECTED_EDGES = new WeakMap<readonly LayoutEdge[], ReadonlySet<LayoutEdge>>();

function protectedEdgeKey(from: string, to: string, vector: GridPosition): string {
  return `${from}\u0000${to}\u0000${vector.x},${vector.y},${vector.level}`;
}

/** Directed protected edges whose exact inverse is also present. */
function reciprocalProtectedEdges(edges: readonly LayoutEdge[]): ReadonlySet<LayoutEdge> {
  const known = RECIPROCAL_PROTECTED_EDGES.get(edges);
  if (known) return known;
  const keyed = new Map<string, LayoutEdge[]>();
  for (const edge of edges) {
    const vector = protectedVector(edge);
    if (!vector) continue;
    const key = protectedEdgeKey(edge.from, edge.to, vector);
    const group = keyed.get(key) ?? [];
    group.push(edge);
    keyed.set(key, group);
  }
  const result = new Set<LayoutEdge>();
  for (const edge of edges) {
    const vector = protectedVector(edge);
    if (!vector) continue;
    if (keyed.has(protectedEdgeKey(edge.to, edge.from, {
      x: -vector.x,
      y: -vector.y,
      level: -vector.level,
    }))) result.add(edge);
  }
  RECIPROCAL_PROTECTED_EDGES.set(edges, result);
  return result;
}

function protectedRayDistance(
  edge: LayoutEdge,
  delta: GridPosition,
): number | undefined {
  const expected = protectedVector(edge);
  return expected ? vectorRayDistance(expected, delta) : undefined;
}

/**
 * Distance on the proper horizontal ray, or on the same x/y cell in the
 * proper vertical direction. Vertical exits therefore cannot be satisfied by
 * drawing their destination beside the source room on the same level.
 */
function orthogonalRayDistance(
  direction: LayoutDirection,
  delta: GridPosition,
): number | undefined {
  const vertical = VERTICAL_VECTORS[direction];
  if (!vertical) return cardinalRayDistance(direction, delta);
  if (delta.x !== 0 || delta.y !== 0 || Math.sign(delta.level) !== vertical.level) return undefined;
  return Math.abs(delta.level);
}

function segmentIntersectsRoomCell(
  from: GridPosition,
  to: GridPosition,
  room: GridPosition,
): boolean {
  if (from.level !== room.level || to.level !== room.level) return false;
  if (samePosition(from, room) || samePosition(to, room)) return false;
  const half = 0.32;
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  // Work in room-relative coordinates. Rigid scoring relies on a segment and
  // its rooms producing exactly the same result after an integral translation;
  // spelling these as `from - (room - half)` made corner contact depend on
  // absolute-coordinate floating-point rounding.
  const relativeX = from.x - room.x;
  const relativeY = from.y - room.y;
  let enter = 0;
  let leave = 1;
  let p = -dx;
  let q = relativeX + half;
  if (p === 0) {
    if (q < 0) return false;
  } else {
    const ratio = q / p;
    if (p < 0) enter = Math.max(enter, ratio);
    else leave = Math.min(leave, ratio);
    if (enter > leave) return false;
  }
  p = dx;
  q = half - relativeX;
  if (p === 0) {
    if (q < 0) return false;
  } else {
    const ratio = q / p;
    if (p < 0) enter = Math.max(enter, ratio);
    else leave = Math.min(leave, ratio);
    if (enter > leave) return false;
  }
  p = -dy;
  q = relativeY + half;
  if (p === 0) {
    if (q < 0) return false;
  } else {
    const ratio = q / p;
    if (p < 0) enter = Math.max(enter, ratio);
    else leave = Math.min(leave, ratio);
    if (enter > leave) return false;
  }
  p = dy;
  q = half - relativeY;
  if (p === 0) {
    if (q < 0) return false;
  } else {
    const ratio = q / p;
    if (p < 0) enter = Math.max(enter, ratio);
    else leave = Math.min(leave, ratio);
    if (enter > leave) return false;
  }
  return true;
}

function collisionGroups(positions: ReadonlyMap<string, GridPosition>): string[][] {
  const cells = new Map<CellKey, string | string[]>();
  for (const [id, position] of positions) {
    const key = cellKey(position);
    const occupants = cells.get(key);
    if (occupants === undefined) cells.set(key, id);
    else if (typeof occupants === "string") cells.set(key, [occupants, id]);
    else occupants.push(id);
  }
  return [...cells.values()].filter((occupants): occupants is string[] => typeof occupants !== "string");
}

function collisionGroupCount(positions: ReadonlyMap<string, GridPosition>): number {
  const occupied = new Set<CellKey>();
  const collided = new Set<CellKey>();
  for (const position of positions.values()) {
    const key = cellKey(position);
    if (occupied.has(key)) collided.add(key);
    else occupied.add(key);
  }
  return collided.size;
}

function hasCollisions(positions: ReadonlyMap<string, GridPosition>): boolean {
  const occupied = new Set<CellKey>();
  for (const position of positions.values()) {
    const key = cellKey(position);
    if (occupied.has(key)) return true;
    occupied.add(key);
  }
  return false;
}

function translationOffsets(radius = SEARCH_RADIUS): Origin[] {
  const result: Origin[] = [{ x: 0, y: 0, level: 0 }];
  for (let distance = 1; distance <= radius; distance += 1) {
    for (let dx = -distance; dx <= distance; dx += 1) {
      const dy = distance - Math.abs(dx);
      result.push({ x: dx, y: dy, level: 0 });
      if (dy !== 0) result.push({ x: dx, y: -dy, level: 0 });
    }
  }
  return result;
}

const NEARBY_OFFSETS = translationOffsets();

// Extrema are accumulated in a loop: spreading a per-room array into
// Math.min/Math.max exceeds the engine's argument limit on very large areas.
function bounds(positions: Iterable<GridPosition>): {
  minX: number;
  maxX: number;
  minY: number;
  maxY: number;
} | undefined {
  let result: { minX: number; maxX: number; minY: number; maxY: number } | undefined;
  for (const position of positions) {
    if (!result) {
      result = { minX: position.x, maxX: position.x, minY: position.y, maxY: position.y };
      continue;
    }
    if (position.x < result.minX) result.minX = position.x;
    if (position.x > result.maxX) result.maxX = position.x;
    if (position.y < result.minY) result.minY = position.y;
    if (position.y > result.maxY) result.maxY = position.y;
  }
  return result;
}

function farOffsets(
  moving: Iterable<GridPosition>,
  occupied: Iterable<GridPosition>,
): Origin[] {
  const movingBounds = bounds(moving);
  const occupiedBounds = bounds(occupied);
  if (!movingBounds || !occupiedBounds) return [];
  return [
    { x: occupiedBounds.maxX + ISLAND_GAP - movingBounds.minX, y: 0, level: 0 },
    { x: occupiedBounds.minX - ISLAND_GAP - movingBounds.maxX, y: 0, level: 0 },
    { x: 0, y: occupiedBounds.maxY + ISLAND_GAP - movingBounds.minY, level: 0 },
    { x: 0, y: occupiedBounds.minY - ISLAND_GAP - movingBounds.maxY, level: 0 },
  ];
}

function uniqueOffsets(offsets: Iterable<Origin>): Origin[] {
  const seen = new Set<string>();
  const result: Origin[] = [];
  for (const offset of offsets) {
    const key = offsetKey(offset);
    if (seen.has(key)) continue;
    seen.add(key);
    result.push(offset);
  }
  return result;
}

interface PositionIndex {
  entries: [string, GridPosition][];
  rows: Map<CellKey, number[]>;
  columns: Map<CellKey, number[]>;
  /** Per level, the sorted distinct y of its rows; built by the first diagonal query. */
  rowLanes?: Map<number, number[]>;
  /** Per level, the sorted distinct x of its columns; built by the first diagonal query. */
  columnLanes?: Map<number, number[]>;
  footprintArea: number;
  footprintPerimeter: number;
}

interface AxisCoordinateIndex {
  counts: Map<number, number>;
  sorted: number[];
}

interface LevelCoordinateIndex {
  x: AxisCoordinateIndex;
  y: AxisCoordinateIndex;
}

interface FootprintQuality {
  area: number;
  perimeter: number;
}

function positionIndex(positions: ReadonlyMap<string, GridPosition>): PositionIndex {
  const entries = [...positions];
  const rows = new Map<CellKey, number[]>();
  const columns = new Map<CellKey, number[]>();
  const levelBounds = new Map<number, {
    minX: number;
    maxX: number;
    minY: number;
    maxY: number;
  }>();
  for (const [, position] of entries) {
    const rowKey = laneKey(position.level, position.y);
    const row = rows.get(rowKey) ?? [];
    row.push(position.x);
    rows.set(rowKey, row);
    const columnKey = laneKey(position.level, position.x);
    const column = columns.get(columnKey) ?? [];
    column.push(position.y);
    columns.set(columnKey, column);

    const known = levelBounds.get(position.level);
    if (known) {
      known.minX = Math.min(known.minX, position.x);
      known.maxX = Math.max(known.maxX, position.x);
      known.minY = Math.min(known.minY, position.y);
      known.maxY = Math.max(known.maxY, position.y);
    } else {
      levelBounds.set(position.level, {
        minX: position.x,
        maxX: position.x,
        minY: position.y,
        maxY: position.y,
      });
    }
  }
  for (const values of rows.values()) values.sort((a, b) => a - b);
  for (const values of columns.values()) values.sort((a, b) => a - b);
  let footprintArea = 0;
  let footprintPerimeter = 0;
  for (const value of levelBounds.values()) {
    const width = value.maxX - value.minX + 1;
    const height = value.maxY - value.minY + 1;
    footprintArea += width * height;
    footprintPerimeter += 2 * (width + height);
  }
  return {
    entries,
    rows,
    columns,
    footprintArea,
    footprintPerimeter,
  };
}

function lowerBound(values: readonly number[], target: number): number {
  let low = 0;
  let high = values.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (values[middle] < target) low = middle + 1;
    else high = middle;
  }
  return low;
}

function countStrictlyBetween(values: readonly number[] | undefined, a: number, b: number): number {
  if (!values || values.length === 0) return 0;
  const minimum = Math.min(a, b);
  const maximum = Math.max(a, b);
  return Math.max(0, lowerBound(values, maximum) - lowerBound(values, minimum + 1));
}

interface ScoredPhysicalEdge {
  fromId: string;
  toId: string;
  from: GridPosition;
  to: GridPosition;
}

interface HorizontalSegment {
  minimum: number;
  maximum: number;
  y: number;
}

interface VerticalSegment {
  x: number;
  minimum: number;
  maximum: number;
}

/**
 * Count crossings between integral horizontal/vertical links with a sweep
 * line. Diagonal links retain the exact pairwise predicate, but the overwhelmingly
 * common cardinal case becomes O((E + K) log E) instead of O(E^2).
 */
function linkCrossingCount(edges: readonly ScoredPhysicalEdge[]): number {
  const general = new Set<number>();
  for (let index = 0; index < edges.length; index += 1) {
    const edge = edges[index];
    if (edge.from.y !== edge.to.y && edge.from.x !== edge.to.x) {
      general.add(index);
    }
  }

  let crossings = 0;
  const horizontalLevels = new Map<number, HorizontalSegment[]>();
  const verticalLevels = new Map<number, VerticalSegment[]>();
  for (const edge of edges) {
    // Colliding intermediate layouts can collapse a physical link to a point.
    // It has no swept path, and admitting it as a horizontal interval would
    // remove its y value before adding it at the same x, making the Fenwick
    // count negative while a perpendicular segment is queried there.
    if (samePosition(edge.from, edge.to)) continue;
    if (edge.from.y === edge.to.y) {
      const values = horizontalLevels.get(edge.from.level) ?? [];
      values.push({
        minimum: Math.min(edge.from.x, edge.to.x),
        maximum: Math.max(edge.from.x, edge.to.x),
        y: edge.from.y,
      });
      horizontalLevels.set(edge.from.level, values);
    } else if (edge.from.x === edge.to.x) {
      const values = verticalLevels.get(edge.from.level) ?? [];
      values.push({
        x: edge.from.x,
        minimum: Math.min(edge.from.y, edge.to.y),
        maximum: Math.max(edge.from.y, edge.to.y),
      });
      verticalLevels.set(edge.from.level, values);
    }
  }

  for (const [level, levelHorizontals] of horizontalLevels) {
    const levelVerticals = verticalLevels.get(level);
    if (!levelVerticals || levelHorizontals.length === 0) continue;
    const yValues = [...new Set(levelHorizontals.map((edge) => edge.y))].sort((a, b) => a - b);
    const starts = new Map<number, number[]>();
    const ends = new Map<number, number[]>();
    const queries = new Map<number, VerticalSegment[]>();
    const xValues = new Set<number>();
    for (const edge of levelHorizontals) {
      const yIndex = lowerBound(yValues, edge.y) + 1;
      const start = starts.get(edge.minimum) ?? [];
      start.push(yIndex);
      starts.set(edge.minimum, start);
      const end = ends.get(edge.maximum) ?? [];
      end.push(yIndex);
      ends.set(edge.maximum, end);
      xValues.add(edge.minimum);
      xValues.add(edge.maximum);
    }
    for (const edge of levelVerticals) {
      const values = queries.get(edge.x) ?? [];
      values.push(edge);
      queries.set(edge.x, values);
      xValues.add(edge.x);
    }

    const tree = new Int32Array(yValues.length + 1);
    const update = (index: number, delta: number): void => {
      for (let cursor = index; cursor < tree.length; cursor += cursor & -cursor) tree[cursor] += delta;
    };
    const prefix = (index: number): number => {
      let total = 0;
      for (let cursor = index; cursor > 0; cursor -= cursor & -cursor) total += tree[cursor];
      return total;
    };
    const orderedX = [...xValues].sort((a, b) => a - b);
    for (const x of orderedX) {
      // Strict intersection excludes horizontal endpoints at this x.
      for (const yIndex of ends.get(x) ?? []) update(yIndex, -1);
      for (const edge of queries.get(x) ?? []) {
        const belowMaximum = lowerBound(yValues, edge.maximum);
        const atOrBelowMinimum = lowerBound(yValues, edge.minimum + 1);
        crossings += prefix(belowMaximum) - prefix(atOrBelowMinimum);
      }
      for (const yIndex of starts.get(x) ?? []) update(yIndex, 1);
    }
  }

  // Only pairs involving a diagonal link remain. Axis/axis crossings were
  // handled by the sweep and parallel axis links cannot strictly intersect,
  // so iterate the (usually tiny) diagonal set instead of every pair.
  for (const a of general) {
    for (let b = 0; b < edges.length; b += 1) {
      if (b === a || (general.has(b) && b < a)) continue;
      const first = edges[a];
      const second = edges[b];
      if (first.fromId === second.fromId || first.fromId === second.toId ||
        first.toId === second.fromId || first.toId === second.toId) continue;
      if (strictSegmentsIntersect(first.from, first.to, second.from, second.to)) crossings += 1;
    }
  }
  return crossings;
}

/**
 * Whether an exit's rooms sit on levels its protected vector rules out: a
 * vector on one level needs both rooms on one level, and one that climbs or
 * descends needs the far room higher or lower. A self-loop never is, since
 * no layout can draw it otherwise. Every such exit is off its ray too.
 */
function misLevelled(edge: LayoutEdge, expected: GridPosition, delta: GridPosition): boolean {
  return edge.from !== edge.to && Math.sign(delta.level) !== Math.sign(expected.level);
}

function edgeRayQuality(
  positions: ReadonlyMap<string, GridPosition>,
  edge: LayoutEdge,
  reciprocal = false,
): RayQuality {
  const from = positions.get(edge.from);
  const to = positions.get(edge.to);
  const expected = protectedVector(edge);
  if (!expected || !from || !to) {
    return {
      cardinalRayViolations: 0,
      reciprocalRayViolations: 0,
      levelViolations: 0,
      levelSlack: 0,
      cardinalSlack: 0,
    };
  }
  const delta = subtract(to, from);
  const distance = protectedRayDistance(edge, delta);
  return distance === undefined
    ? {
      cardinalRayViolations: 1,
      reciprocalRayViolations: reciprocal ? 1 : 0,
      levelViolations: misLevelled(edge, expected, delta) ? 1 : 0,
      levelSlack: 0,
      cardinalSlack: 0,
    }
    : {
      cardinalRayViolations: 0,
      reciprocalRayViolations: 0,
      levelViolations: 0,
      levelSlack: expected.level === 0 ? 0 : Math.max(0, distance - 1),
      cardinalSlack: Math.max(0, distance - 1),
    };
}

/** Protected directional constraints which are off their required ray. */
export function directionalViolationEdges(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): LayoutEdge[] {
  return edges.filter((edge) => edgeRayQuality(positions, edge).cardinalRayViolations > 0);
}

/** Protected directional constraints whose rooms sit on levels the exit rules out. */
export function levelViolationEdges(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): LayoutEdge[] {
  return edges.filter((edge) => edgeRayQuality(positions, edge).levelViolations > 0);
}

function fullRayQuality(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): RayQuality {
  const result = {
    cardinalRayViolations: 0,
    reciprocalRayViolations: 0,
    levelViolations: 0,
    levelSlack: 0,
    cardinalSlack: 0,
  };
  const reciprocal = reciprocalProtectedEdges(edges);
  for (const edge of edges) {
    const contribution = edgeRayQuality(positions, edge, reciprocal.has(edge));
    result.cardinalRayViolations += contribution.cardinalRayViolations;
    result.reciprocalRayViolations += contribution.reciprocalRayViolations;
    result.levelViolations += contribution.levelViolations;
    result.levelSlack += contribution.levelSlack;
    result.cardinalSlack += contribution.cardinalSlack;
  }
  return result;
}

function fullExitPortQuality(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): ExitPortQuality {
  const occupants = new Map<CellKey, string | string[]>();
  for (const [id, position] of positions) {
    const key = cellKey(position);
    const known = occupants.get(key);
    if (known === undefined) occupants.set(key, id);
    else if (typeof known === "string") occupants.set(key, [known, id]);
    else known.push(id);
  }
  const reciprocal = reciprocalProtectedEdges(edges);
  let exitPortViolations = 0;
  let reciprocalExitPortViolations = 0;
  for (const edge of edges) {
    const expected = protectedVector(edge);
    if (!expected || expected.level !== 0 ||
      Math.abs(expected.x) + Math.abs(expected.y) !== 1) continue;
    const from = positions.get(edge.from);
    const to = positions.get(edge.to);
    if (!from || !to || from.level !== to.level) continue;
    const port = add(from, expected);
    const occupant = occupants.get(cellKey(port));
    const blocked = typeof occupant === "string"
      ? occupant !== edge.from && occupant !== edge.to
      : occupant?.some((id) => id !== edge.from && id !== edge.to) ?? false;
    if (!blocked) continue;
    exitPortViolations += 1;
    if (reciprocal.has(edge)) reciprocalExitPortViolations += 1;
  }
  return { exitPortViolations, reciprocalExitPortViolations };
}

/**
 * Shared scoring caches for one rigid group translation. Everything is keyed
 * to the exact (base, moved-set) pair and derived lazily from base geometry,
 * so a compaction loop trying many offsets for one group pays each index
 * build once. Members treat the moved set as authoritative: the call site
 * guarantees every changed id moved by exactly the candidate's offset.
 */
interface RigidTranslationContext {
  base: Candidate;
  movedIds: ReadonlySet<string>;
  /** Moved ids present in the base map, at their base positions. */
  movedEntries: [string, GridPosition][];
  linkClasses?: RigidLinkClasses;
  boundaryRayEdges?: LayoutEdge[];
  movedIndex?: PositionIndex;
  movedOccupants?: Map<CellKey, string | string[]>;
  portEdges?: RigidPortEdges;
  movedCrossingIndex?: RigidCrossingClassIndex;
  stationaryCrossingIndex?: RigidCrossingClassIndex;
  movedLinkRows?: RigidLinkRows;
  stationaryLinkRows?: RigidLinkRows;
  affectedCrossingsBefore?: number;
}

interface RigidLinkClasses {
  moved: IndexedPhysicalLink[];
  stationary: IndexedPhysicalLink[];
  boundary: IndexedPhysicalLink[];
}

interface RigidPortEdges {
  /** Intra-group cardinal edges with their base port cells. */
  moved: { edge: LayoutEdge; port: GridPosition }[];
  /** Cardinal edges with exactly one moved endpoint; re-evaluated per side. */
  boundary: { edge: LayoutEdge; expected: GridPosition }[];
}

/** One axis-aligned link segment prepared for perpendicular crossing queries. */
interface RigidAxisSegment {
  /** The fixed coordinate: x for a vertical segment, y for a horizontal one. */
  at: number;
  lower: number;
  upper: number;
  a: string;
  b: string;
}

interface RigidCrossingClassIndex {
  /** Vertical segments per level, sorted by x. */
  verticalsByLevel: Map<number, RigidAxisSegment[]>;
  /** Horizontal segments per level, sorted by y. */
  horizontalsByLevel: Map<number, RigidAxisSegment[]>;
  diagonals: IndexedPhysicalLink[];
  axisLinks: IndexedPhysicalLink[];
  /** Every link of this class with crossable geometry (planar, non-point). */
  links: IndexedPhysicalLink[];
}

function rigidTranslationContext(
  base: Candidate,
  movedIds: ReadonlySet<string>,
): RigidTranslationContext {
  const movedEntries: [string, GridPosition][] = [];
  for (const id of movedIds) {
    const position = base.positions.get(id);
    if (position) movedEntries.push([id, position]);
  }
  return { base, movedIds, movedEntries };
}

/**
 * The translation annotation is honored only when its context provably
 * belongs to this candidate's exact base and moved set; anything else falls
 * back to the generic scoring paths.
 */
function rigidTranslationOf(value: Candidate): RigidTranslation | undefined {
  const derivation = value.derivation;
  const translation = derivation?.translation;
  if (!derivation || !translation) return undefined;
  if (derivation.base.edges !== value.edges) return undefined;
  const context = translation.context;
  if (context.base !== derivation.base || context.movedIds !== derivation.changedIds) {
    return undefined;
  }
  return translation;
}

function contextLinkClasses(context: RigidTranslationContext): RigidLinkClasses {
  if (context.linkClasses) return context.linkClasses;
  const moved: IndexedPhysicalLink[] = [];
  const stationary: IndexedPhysicalLink[] = [];
  const boundary: IndexedPhysicalLink[] = [];
  for (const link of topologyIndex(context.base.edges).physical) {
    const aMoves = context.movedIds.has(link.a);
    const bMoves = context.movedIds.has(link.b);
    if (aMoves === bMoves) (aMoves ? moved : stationary).push(link);
    else boundary.push(link);
  }
  context.linkClasses = { moved, stationary, boundary };
  return context.linkClasses;
}

function contextBoundaryRayEdges(context: RigidTranslationContext): LayoutEdge[] {
  if (context.boundaryRayEdges) return context.boundaryRayEdges;
  const result: LayoutEdge[] = [];
  // A boundary edge has exactly one endpoint in the group, so walking a
  // small group's incident lists visits it exactly once — no dedup, and the
  // cost scales with the group's degree. Large groups walk every incident
  // list twice per interior edge, so they scan the flat edge array instead;
  // both enumerations are exact, and only summed deltas consume this list,
  // so its order carries no behavior.
  if (context.movedIds.size * 2 <= context.base.positions.size) {
    const incident = topologyIndex(context.base.edges).incident;
    for (const id of context.movedIds) {
      for (const edge of incident.get(id) ?? []) {
        if (context.movedIds.has(edge.from) === context.movedIds.has(edge.to)) continue;
        if (protectedVector(edge)) result.push(edge);
      }
    }
  } else {
    for (const edge of context.base.edges) {
      if (context.movedIds.has(edge.from) === context.movedIds.has(edge.to)) continue;
      if (protectedVector(edge)) result.push(edge);
    }
  }
  context.boundaryRayEdges = result;
  return result;
}

function contextMovedIndex(context: RigidTranslationContext): PositionIndex {
  return context.movedIndex ??= positionIndex(new Map(context.movedEntries));
}

function contextMovedOccupants(
  context: RigidTranslationContext,
): Map<CellKey, string | string[]> {
  if (context.movedOccupants) return context.movedOccupants;
  const result = new Map<CellKey, string | string[]>();
  for (const [id, position] of context.movedEntries) {
    const key = cellKey(position);
    const known = result.get(key);
    if (known === undefined) result.set(key, id);
    else if (typeof known === "string") result.set(key, [known, id]);
    else known.push(id);
  }
  context.movedOccupants = result;
  return result;
}

function contextPortEdges(context: RigidTranslationContext): RigidPortEdges {
  if (context.portEdges) return context.portEdges;
  const moved: RigidPortEdges["moved"] = [];
  const boundary: RigidPortEdges["boundary"] = [];
  const positions = context.base.positions;
  const classify = (edge: LayoutEdge, id?: string): void => {
    const expected = protectedVector(edge);
    if (!expected || expected.level !== 0 ||
      Math.abs(expected.x) + Math.abs(expected.y) !== 1) return;
    const fromMoves = context.movedIds.has(edge.from);
    const toMoves = context.movedIds.has(edge.to);
    if (fromMoves !== toMoves) {
      boundary.push({ edge, expected });
      return;
    }
    if (!fromMoves) return;
    // On the incident walk an intra-group edge appears once per endpoint;
    // collecting it only from its `from` side keeps it single.
    if (id !== undefined && edge.from !== id && edge.from !== edge.to) return;
    const from = positions.get(edge.from);
    const to = positions.get(edge.to);
    if (!from || !to || from.level !== to.level) return;
    moved.push({ edge, port: add(from, expected) });
  };
  // Small groups walk their incident lists; large ones scan the flat edge
  // array (the walk would visit interior edges twice). Both enumerations are
  // exact; only summed deltas consume these lists, so order carries no
  // behavior. Stationary edges are found through the base's port index.
  if (context.movedIds.size * 2 <= context.base.positions.size) {
    const incident = topologyIndex(context.base.edges).incident;
    for (const id of context.movedIds) {
      for (const edge of incident.get(id) ?? []) classify(edge, id);
    }
  } else {
    for (const edge of context.base.edges) classify(edge);
  }
  context.portEdges = { moved, boundary };
  return context.portEdges;
}

/** Cell -> occupant ids for a candidate, cached for derived delta scoring. */
function candidateOccupants(value: Candidate): Map<CellKey, string | string[]> {
  if (value.score.occupants) return value.score.occupants;
  const result = new Map<CellKey, string | string[]>();
  for (const [id, position] of value.positions) {
    const key = cellKey(position);
    const known = result.get(key);
    if (known === undefined) result.set(key, id);
    else if (typeof known === "string") result.set(key, [known, id]);
    else known.push(id);
  }
  value.score.occupants = result;
  return result;
}

/**
 * Port cell -> the cardinal edges scored against that cell, using exactly the
 * eligibility rules of fullExitPortQuality. Cached on the base so translation
 * deltas can find the stationary edges affected by a vacated or entered cell.
 */
function candidatePortIndex(value: Candidate): Map<CellKey, LayoutEdge[]> {
  if (value.score.portIndex) return value.score.portIndex;
  const result = new Map<CellKey, LayoutEdge[]>();
  for (const edge of value.edges) {
    const expected = protectedVector(edge);
    if (!expected || expected.level !== 0 ||
      Math.abs(expected.x) + Math.abs(expected.y) !== 1) continue;
    const from = value.positions.get(edge.from);
    const to = value.positions.get(edge.to);
    if (!from || !to || from.level !== to.level) continue;
    const key = cellKey(add(from, expected));
    const known = result.get(key);
    if (known) known.push(edge);
    else result.set(key, [edge]);
  }
  value.score.portIndex = result;
  return result;
}

/** True when any occupant outside the moved set is neither edge endpoint. */
function blockedByStationaryOccupant(
  occupant: string | readonly string[] | undefined,
  movedIds: ReadonlySet<string>,
  edge: LayoutEdge,
): boolean {
  if (occupant === undefined) return false;
  if (typeof occupant === "string") {
    return !movedIds.has(occupant) && occupant !== edge.from && occupant !== edge.to;
  }
  return occupant.some((id) => !movedIds.has(id) && id !== edge.from && id !== edge.to);
}

/** True when any occupant is neither edge endpoint — the standard port test. */
function blockedByAnyOccupant(
  occupant: string | readonly string[] | undefined,
  edge: LayoutEdge,
): boolean {
  if (occupant === undefined) return false;
  if (typeof occupant === "string") return occupant !== edge.from && occupant !== edge.to;
  return occupant.some((id) => id !== edge.from && id !== edge.to);
}

function negatedOrigin(offset: Origin): Origin {
  return { x: -offset.x, y: -offset.y, level: -offset.level };
}

/**
 * Exit-port tuple for a rigid translation, as base plus boundary-sensitive
 * deltas. Intra-group edges keep their moved-room occupancy (it translates
 * with them) and can only gain or lose a stationary blocker; stationary
 * edges can only gain or lose a moved blocker, and only at cells the group
 * vacated or entered; edges crossing the boundary are re-evaluated whole.
 */
function rigidExitPortQuality(
  value: Candidate,
  translation: RigidTranslation,
): ExitPortQuality {
  const context = translation.context;
  const base = context.base;
  const offset = translation.offset;
  const negated = negatedOrigin(offset);
  const reciprocal = reciprocalProtectedEdges(value.edges);
  const baseOccupants = candidateOccupants(base);
  const movedOccupants = contextMovedOccupants(context);
  const ports = contextPortEdges(context);
  const basePorts = candidateExitPortQuality(base);
  let exitPortViolations = basePorts.exitPortViolations;
  let reciprocalExitPortViolations = basePorts.reciprocalExitPortViolations;
  const apply = (edge: LayoutEdge, before: boolean, after: boolean): void => {
    if (before === after) return;
    const sign = after ? 1 : -1;
    exitPortViolations += sign;
    if (reciprocal.has(edge)) reciprocalExitPortViolations += sign;
  };

  for (const { edge, port } of ports.moved) {
    // A moved third room sits at the port before exactly when it does after.
    if (blockedByAnyOccupant(movedOccupants.get(cellKey(port)), edge)) continue;
    apply(
      edge,
      blockedByStationaryOccupant(baseOccupants.get(cellKey(port)), context.movedIds, edge),
      blockedByStationaryOccupant(
        baseOccupants.get(cellKey(add(port, offset))),
        context.movedIds,
        edge,
      ),
    );
  }

  // Only cells the group left or entered can change a stationary edge's port.
  const portIndex = candidatePortIndex(base);
  const seenEdges = new Set<LayoutEdge>();
  const visitCell = (cell: GridPosition): void => {
    for (const edge of portIndex.get(cellKey(cell)) ?? []) {
      if (context.movedIds.has(edge.from) || context.movedIds.has(edge.to)) continue;
      if (seenEdges.has(edge)) continue;
      seenEdges.add(edge);
      // A stationary third room blocks this fixed port on both sides.
      if (blockedByStationaryOccupant(
        baseOccupants.get(cellKey(cell)),
        context.movedIds,
        edge,
      )) continue;
      apply(
        edge,
        blockedByAnyOccupant(movedOccupants.get(cellKey(cell)), edge),
        blockedByAnyOccupant(movedOccupants.get(cellKey(add(cell, negated))), edge),
      );
    }
  };
  for (const [, position] of context.movedEntries) {
    visitCell(position);
    visitCell(add(position, offset));
  }

  for (const { edge, expected } of ports.boundary) {
    const baseFrom = base.positions.get(edge.from);
    const baseTo = base.positions.get(edge.to);
    if (!baseFrom || !baseTo) continue;
    // The base map holds every room, so before-side blocking is the plain
    // any-third-occupant test against the base port cell.
    const before = baseFrom.level === baseTo.level &&
      blockedByAnyOccupant(baseOccupants.get(cellKey(add(baseFrom, expected))), edge);
    const afterFrom = context.movedIds.has(edge.from) ? add(baseFrom, offset) : baseFrom;
    const afterTo = context.movedIds.has(edge.to) ? add(baseTo, offset) : baseTo;
    let after = false;
    if (afterFrom.level === afterTo.level) {
      const port = add(afterFrom, expected);
      after = blockedByStationaryOccupant(
        baseOccupants.get(cellKey(port)),
        context.movedIds,
        edge,
      ) || blockedByAnyOccupant(movedOccupants.get(cellKey(add(port, negated))), edge);
    }
    apply(edge, before, after);
  }

  return { exitPortViolations, reciprocalExitPortViolations };
}

/**
 * Room-obstruction count for a rigid translation. The group's own rooms keep
 * their relation to intra-group segments, so those need only the stationary
 * rooms entering or leaving; stationary segments need only the moved rooms;
 * boundary segments change shape and are re-counted from the same indexes.
 * All-room counts come from the base's cached index minus the moved index —
 * both sorted, so every axis-aligned query stays logarithmic.
 */
function rigidRoomObstructions(
  value: Candidate,
  translation: RigidTranslation,
): number {
  const context = translation.context;
  const base = context.base;
  const offset = translation.offset;
  const negated = negatedOrigin(offset);
  const baseIndex = candidateIndex(base);
  const movedIndex = contextMovedIndex(context);
  const { moved, stationary, boundary } = contextLinkClasses(context);
  let result = candidateRoomObstructions(base);

  for (const link of moved) {
    const from = base.positions.get(link.a);
    const to = base.positions.get(link.b);
    if (!from || !to || from.level !== to.level) continue;
    const afterFrom = add(from, offset);
    const afterTo = add(to, offset);
    const beforeStationary = indexedSegmentObstructions(baseIndex, from, to) -
      indexedSegmentObstructions(movedIndex, from, to);
    const afterStationary = indexedSegmentObstructions(baseIndex, afterFrom, afterTo) -
      indexedSegmentObstructions(movedIndex, afterFrom, afterTo);
    result += afterStationary - beforeStationary;
  }

  for (const link of stationary) {
    const from = base.positions.get(link.a);
    const to = base.positions.get(link.b);
    if (!from || !to || from.level !== to.level) continue;
    // Moved rooms sit on this fixed segment after the move exactly when their
    // base positions sit on the segment translated backwards.
    result += indexedSegmentObstructions(movedIndex, add(from, negated), add(to, negated)) -
      indexedSegmentObstructions(movedIndex, from, to);
  }

  for (const link of boundary) {
    const beforeFrom = base.positions.get(link.a);
    const beforeTo = base.positions.get(link.b);
    if (!beforeFrom || !beforeTo) continue;
    const before = indexedSegmentObstructions(baseIndex, beforeFrom, beforeTo);
    const afterFrom = context.movedIds.has(link.a) ? add(beforeFrom, offset) : beforeFrom;
    const afterTo = context.movedIds.has(link.b) ? add(beforeTo, offset) : beforeTo;
    let after = 0;
    if (afterFrom.level === afterTo.level) {
      after = indexedSegmentObstructions(baseIndex, afterFrom, afterTo) -
        indexedSegmentObstructions(movedIndex, afterFrom, afterTo) +
        indexedSegmentObstructions(movedIndex, add(afterFrom, negated), add(afterTo, negated));
    }
    result += after - before;
  }
  return result;
}

function buildRigidCrossingClassIndex(
  links: readonly IndexedPhysicalLink[],
  positions: ReadonlyMap<string, GridPosition>,
): RigidCrossingClassIndex {
  const verticalsByLevel = new Map<number, RigidAxisSegment[]>();
  const horizontalsByLevel = new Map<number, RigidAxisSegment[]>();
  const diagonals: IndexedPhysicalLink[] = [];
  const axisLinks: IndexedPhysicalLink[] = [];
  const crossable: IndexedPhysicalLink[] = [];
  for (const link of links) {
    const from = positions.get(link.a);
    const to = positions.get(link.b);
    // Cross-level and zero-length links can never strictly cross anything.
    if (!from || !to || from.level !== to.level || samePosition(from, to)) continue;
    crossable.push(link);
    if (from.x !== to.x && from.y !== to.y) diagonals.push(link);
    else axisLinks.push(link);
    if (from.y === to.y) {
      const values = horizontalsByLevel.get(from.level) ?? [];
      values.push({
        at: from.y,
        lower: Math.min(from.x, to.x),
        upper: Math.max(from.x, to.x),
        a: link.a,
        b: link.b,
      });
      horizontalsByLevel.set(from.level, values);
    } else if (from.x === to.x) {
      const values = verticalsByLevel.get(from.level) ?? [];
      values.push({
        at: from.x,
        lower: Math.min(from.y, to.y),
        upper: Math.max(from.y, to.y),
        a: link.a,
        b: link.b,
      });
      verticalsByLevel.set(from.level, values);
    }
  }
  for (const values of verticalsByLevel.values()) values.sort((a, b) => a.at - b.at);
  for (const values of horizontalsByLevel.values()) values.sort((a, b) => a.at - b.at);
  return { verticalsByLevel, horizontalsByLevel, diagonals, axisLinks, links: crossable };
}

function contextMovedCrossingIndex(context: RigidTranslationContext): RigidCrossingClassIndex {
  return context.movedCrossingIndex ??= buildRigidCrossingClassIndex(
    contextLinkClasses(context).moved,
    context.base.positions,
  );
}

function contextStationaryCrossingIndex(
  context: RigidTranslationContext,
): RigidCrossingClassIndex {
  return context.stationaryCrossingIndex ??= buildRigidCrossingClassIndex(
    contextLinkClasses(context).stationary,
    context.base.positions,
  );
}

/**
 * Count perpendicular segments strictly crossed by one axis-aligned segment:
 * fixed coordinate strictly inside the query interval and query coordinate
 * strictly inside theirs — exactly the strict predicate for perpendicular
 * integral segments. Shared-endpoint pairs never strictly cross, and the id
 * exclusion also keeps boundary-link queries from counting themselves.
 */
function countPerpendicularCrossings(
  segments: readonly RigidAxisSegment[] | undefined,
  atLow: number,
  atHigh: number,
  cross: number,
  aId: string,
  bId: string,
): number {
  if (!segments || segments.length === 0) return 0;
  let low = 0;
  let high = segments.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (segments[middle].at <= atLow) low = middle + 1;
    else high = middle;
  }
  let result = 0;
  for (let index = low; index < segments.length && segments[index].at < atHigh; index += 1) {
    const segment = segments[index];
    if (segment.lower < cross && cross < segment.upper &&
      segment.a !== aId && segment.b !== bId && segment.a !== bId && segment.b !== aId) {
      result += 1;
    }
  }
  return result;
}

function rigidSegmentCrossings(
  index: RigidCrossingClassIndex,
  from: GridPosition,
  to: GridPosition,
  aId: string,
  bId: string,
): number {
  if (from.y === to.y) {
    return countPerpendicularCrossings(
      index.verticalsByLevel.get(from.level),
      Math.min(from.x, to.x),
      Math.max(from.x, to.x),
      from.y,
      aId,
      bId,
    );
  }
  return countPerpendicularCrossings(
    index.horizontalsByLevel.get(from.level),
    Math.min(from.y, to.y),
    Math.max(from.y, to.y),
    from.x,
    aId,
    bId,
  );
}

function rigidPairCrosses(
  aFrom: GridPosition | undefined,
  aTo: GridPosition | undefined,
  bFrom: GridPosition | undefined,
  bTo: GridPosition | undefined,
): boolean {
  return !!aFrom && !!aTo && !!bFrom && !!bTo &&
    aFrom.level === aTo.level && bFrom.level === bTo.level &&
    strictSegmentsIntersect(aFrom, aTo, bFrom, bTo);
}

function sharesLinkEndpoint(a: IndexedPhysicalLink, bA: string, bB: string): boolean {
  return a.a === bA || a.a === bB || a.b === bA || a.b === bB;
}

const RIGID_ZERO_OFFSET: Origin = { x: 0, y: 0, level: 0 };

/** A crossing class's links with their base coordinates as `x1, y1, x2, y2, level` rows. */
interface RigidLinkRows {
  links: readonly IndexedPhysicalLink[];
  rows: Float64Array;
}

function rigidLinkRows(
  index: RigidCrossingClassIndex,
  positions: ReadonlyMap<string, GridPosition>,
): RigidLinkRows {
  const links = index.links;
  const rows = new Float64Array(links.length * 5);
  for (let at = 0; at < links.length; at += 1) {
    // Every indexed link has both endpoints on one level.
    const from = positions.get(links[at].a) as GridPosition;
    const to = positions.get(links[at].b) as GridPosition;
    rows[at * 5] = from.x;
    rows[at * 5 + 1] = from.y;
    rows[at * 5 + 2] = to.x;
    rows[at * 5 + 3] = to.y;
    rows[at * 5 + 4] = from.level;
  }
  return { links, rows };
}

function contextStationaryLinkRows(context: RigidTranslationContext): RigidLinkRows {
  return context.stationaryLinkRows ??= rigidLinkRows(
    contextStationaryCrossingIndex(context),
    context.base.positions,
  );
}

function contextMovedLinkRows(context: RigidTranslationContext): RigidLinkRows {
  return context.movedLinkRows ??= rigidLinkRows(
    contextMovedCrossingIndex(context),
    context.base.positions,
  );
}

/**
 * How many links of one class a diagonal segment strictly crosses, with the
 * class translated by `offset`: exactly the pairs `rigidPairCrosses` counts.
 * The segment is translated by the negated offset instead, which keeps every
 * cross product, and a link whose bounding box misses the segment's is
 * skipped before the exact test.
 */
function rigidDiagonalCrossings(
  { links, rows }: RigidLinkRows,
  from: GridPosition,
  to: GridPosition,
  aId: string,
  bId: string,
  offset: Origin,
): number {
  const ax = from.x - offset.x;
  const ay = from.y - offset.y;
  const bx = to.x - offset.x;
  const by = to.y - offset.y;
  const level = from.level - offset.level;
  const minX = Math.min(ax, bx);
  const maxX = Math.max(ax, bx);
  const minY = Math.min(ay, by);
  const maxY = Math.max(ay, by);
  let result = 0;
  for (let at = 0; at < links.length; at += 1) {
    const row = at * 5;
    if (rows[row + 4] !== level) continue;
    const cx = rows[row];
    const cy = rows[row + 1];
    const dx = rows[row + 2];
    const dy = rows[row + 3];
    if (Math.max(cx, dx) < minX || Math.min(cx, dx) > maxX ||
      Math.max(cy, dy) < minY || Math.min(cy, dy) > maxY) continue;
    if (sharesLinkEndpoint(links[at], aId, bId)) continue;
    const abC = (bx - ax) * (cy - ay) - (by - ay) * (cx - ax);
    const abD = (bx - ax) * (dy - ay) - (by - ay) * (dx - ax);
    const cdA = (dx - cx) * (ay - cy) - (dy - cy) * (ax - cx);
    const cdB = (dx - cx) * (by - cy) - (dy - cy) * (bx - cx);
    if (((abC < 0 && abD > 0) || (abC > 0 && abD < 0)) &&
      ((cdA < 0 && cdB > 0) || (cdA > 0 && cdB < 0))) result += 1;
  }
  return result;
}

/**
 * Crossings among the pairs a rigid translation can affect: group x
 * stationary, and every pair involving a boundary link. Pairs inside the
 * group and pairs inside the stationary remainder translate rigidly (or not
 * at all) and keep their crossing status, so the full count moves by exactly
 * the difference of this tally between the translated and base offsets.
 * Group links are indexed at base coordinates; queries against them translate
 * by the negated offset instead, which preserves strict intersection.
 */
function rigidAffectedCrossings(context: RigidTranslationContext, offset: Origin): number {
  const base = context.base;
  const { boundary } = contextLinkClasses(context);
  const movedIndex = contextMovedCrossingIndex(context);
  const stationaryIndex = contextStationaryCrossingIndex(context);
  const negated = negatedOrigin(offset);
  const movedAt = (id: string): GridPosition | undefined => {
    const position = base.positions.get(id);
    return position && add(position, offset);
  };
  let result = 0;

  // Group x stationary, axis-aligned on the group side.
  for (const [level, values] of movedIndex.horizontalsByLevel) {
    for (const segment of values) {
      result += countPerpendicularCrossings(
        stationaryIndex.verticalsByLevel.get(level + offset.level),
        segment.lower + offset.x,
        segment.upper + offset.x,
        segment.at + offset.y,
        segment.a,
        segment.b,
      );
    }
  }
  for (const [level, values] of movedIndex.verticalsByLevel) {
    for (const segment of values) {
      result += countPerpendicularCrossings(
        stationaryIndex.horizontalsByLevel.get(level + offset.level),
        segment.lower + offset.y,
        segment.upper + offset.y,
        segment.at + offset.x,
        segment.a,
        segment.b,
      );
    }
  }
  // Diagonal group links pair against every stationary link; stationary
  // diagonals then pair against the group's axis-aligned remainder so each
  // diagonal-diagonal pair is counted exactly once.
  for (const link of movedIndex.diagonals) {
    const from = movedAt(link.a);
    const to = movedAt(link.b);
    for (const other of stationaryIndex.links) {
      if (rigidPairCrosses(
        from,
        to,
        base.positions.get(other.a),
        base.positions.get(other.b),
      )) result += 1;
    }
  }
  for (const link of stationaryIndex.diagonals) {
    const from = base.positions.get(link.a);
    const to = base.positions.get(link.b);
    for (const other of movedIndex.axisLinks) {
      if (rigidPairCrosses(from, to, movedAt(other.a), movedAt(other.b))) result += 1;
    }
  }

  // Boundary links change shape; each pairs against both classes and, once
  // per unordered pair, against the other boundary links.
  const boundaryFrom: (GridPosition | undefined)[] = [];
  const boundaryTo: (GridPosition | undefined)[] = [];
  for (const link of boundary) {
    boundaryFrom.push(context.movedIds.has(link.a) ? movedAt(link.a) : base.positions.get(link.a));
    boundaryTo.push(context.movedIds.has(link.b) ? movedAt(link.b) : base.positions.get(link.b));
  }
  for (let index = 0; index < boundary.length; index += 1) {
    const link = boundary[index];
    const from = boundaryFrom[index];
    const to = boundaryTo[index];
    if (!from || !to || from.level !== to.level || samePosition(from, to)) continue;
    const axisAligned = from.y === to.y || from.x === to.x;
    if (axisAligned) {
      result += rigidSegmentCrossings(stationaryIndex, from, to, link.a, link.b);
      result += rigidSegmentCrossings(
        movedIndex,
        add(from, negated),
        add(to, negated),
        link.a,
        link.b,
      );
      for (const other of stationaryIndex.diagonals) {
        if (sharesLinkEndpoint(other, link.a, link.b)) continue;
        if (rigidPairCrosses(
          from,
          to,
          base.positions.get(other.a),
          base.positions.get(other.b),
        )) result += 1;
      }
      for (const other of movedIndex.diagonals) {
        if (sharesLinkEndpoint(other, link.a, link.b)) continue;
        if (rigidPairCrosses(from, to, movedAt(other.a), movedAt(other.b))) result += 1;
      }
    } else {
      result += rigidDiagonalCrossings(
        contextStationaryLinkRows(context),
        from,
        to,
        link.a,
        link.b,
        RIGID_ZERO_OFFSET,
      );
      result += rigidDiagonalCrossings(contextMovedLinkRows(context), from, to, link.a, link.b, offset);
    }
    for (let otherIndex = index + 1; otherIndex < boundary.length; otherIndex += 1) {
      if (sharesLinkEndpoint(boundary[otherIndex], link.a, link.b)) continue;
      if (rigidPairCrosses(from, to, boundaryFrom[otherIndex], boundaryTo[otherIndex])) result += 1;
    }
  }
  return result;
}

function contextAffectedCrossingsBefore(context: RigidTranslationContext): number {
  return context.affectedCrossingsBefore ??= rigidAffectedCrossings(context, RIGID_ZERO_OFFSET);
}

function candidateRayQuality(value: Candidate): RayQuality {
  if (value.score.ray) return value.score.ray;
  const derivation = value.derivation;
  if (!derivation || derivation.base.edges !== value.edges) {
    value.score.ray = fullRayQuality(value.positions, value.edges);
    return value.score.ray;
  }

  const base = candidateRayQuality(derivation.base);
  const result = { ...base };
  const reciprocal = reciprocalProtectedEdges(value.edges);
  const translation = rigidTranslationOf(value);
  let affected: Iterable<LayoutEdge>;
  if (translation) {
    // Intra-group deltas depend only on endpoint differences, which a rigid
    // translation preserves; only edges crossing the boundary can change.
    affected = contextBoundaryRayEdges(translation.context);
  } else {
    const affectedSet = new Set<LayoutEdge>();
    const incident = topologyIndex(value.edges).incident;
    for (const id of derivation.changedIds) {
      for (const edge of incident.get(id) ?? []) affectedSet.add(edge);
    }
    affected = affectedSet;
  }
  for (const edge of affected) {
    const isReciprocal = reciprocal.has(edge);
    const before = edgeRayQuality(derivation.base.positions, edge, isReciprocal);
    const after = edgeRayQuality(value.positions, edge, isReciprocal);
    result.cardinalRayViolations += after.cardinalRayViolations - before.cardinalRayViolations;
    result.reciprocalRayViolations += after.reciprocalRayViolations - before.reciprocalRayViolations;
    result.levelViolations += after.levelViolations - before.levelViolations;
    result.levelSlack += after.levelSlack - before.levelSlack;
    result.cardinalSlack += after.cardinalSlack - before.cardinalSlack;
  }
  value.score.ray = result;
  return result;
}

/**
 * Exit-port tuple for a small non-rigid edit, mirroring candidateRayQuality's
 * delta shape: only edges incident to a changed id, plus fixed-port edges
 * whose port cell a changed room vacated or entered, can change their blocked
 * status. After-state occupancy composes the base occupant map with the
 * changed rooms' removals and additions, so no whole-map occupant rebuild is
 * paid per candidate.
 */
function incrementalExitPortQuality(
  value: Candidate,
  derivation: CandidateDerivation,
): ExitPortQuality {
  const base = derivation.base;
  const baseOccupants = candidateOccupants(base);
  const portIndex = candidatePortIndex(base);
  const reciprocal = reciprocalProtectedEdges(value.edges);
  const basePorts = candidateExitPortQuality(base);
  let exitPortViolations = basePorts.exitPortViolations;
  let reciprocalExitPortViolations = basePorts.reciprocalExitPortViolations;

  const addedAt = new Map<CellKey, string[]>();
  const affected = new Set<LayoutEdge>();
  const incident = topologyIndex(value.edges).incident;
  for (const id of derivation.changedIds) {
    for (const edge of incident.get(id) ?? []) affected.add(edge);
    const before = base.positions.get(id);
    const after = value.positions.get(id);
    if (before) {
      for (const edge of portIndex.get(cellKey(before)) ?? []) affected.add(edge);
    }
    if (after) {
      const key = cellKey(after);
      const list = addedAt.get(key);
      if (list) list.push(id);
      else addedAt.set(key, [id]);
      for (const edge of portIndex.get(key) ?? []) affected.add(edge);
    }
  }

  const blockedAfterAt = (key: CellKey, edge: LayoutEdge): boolean => {
    const occupant = baseOccupants.get(key);
    if (occupant !== undefined) {
      if (typeof occupant === "string") {
        if (!derivation.changedIds.has(occupant) &&
          occupant !== edge.from && occupant !== edge.to) return true;
      } else if (occupant.some((id) =>
        !derivation.changedIds.has(id) && id !== edge.from && id !== edge.to)) {
        return true;
      }
    }
    const added = addedAt.get(key);
    return added !== undefined && added.some((id) => id !== edge.from && id !== edge.to);
  };

  for (const edge of affected) {
    const expected = protectedVector(edge);
    if (!expected || expected.level !== 0 ||
      Math.abs(expected.x) + Math.abs(expected.y) !== 1) continue;
    const beforeFrom = base.positions.get(edge.from);
    const beforeTo = base.positions.get(edge.to);
    const before = !!beforeFrom && !!beforeTo && beforeFrom.level === beforeTo.level &&
      blockedByAnyOccupant(baseOccupants.get(cellKey(add(beforeFrom, expected))), edge);
    const afterFrom = value.positions.get(edge.from);
    const afterTo = value.positions.get(edge.to);
    const after = !!afterFrom && !!afterTo && afterFrom.level === afterTo.level &&
      blockedAfterAt(cellKey(add(afterFrom, expected)), edge);
    if (before === after) continue;
    const sign = after ? 1 : -1;
    exitPortViolations += sign;
    if (reciprocal.has(edge)) reciprocalExitPortViolations += sign;
  }
  return { exitPortViolations, reciprocalExitPortViolations };
}

function candidateExitPortQuality(value: Candidate): ExitPortQuality {
  if (value.score.exitPorts) return value.score.exitPorts;
  const translation = rigidTranslationOf(value);
  if (translation) {
    value.score.exitPorts = rigidExitPortQuality(value, translation);
    return value.score.exitPorts;
  }
  const derivation = value.derivation;
  if (derivation && derivation.base.edges === value.edges &&
    derivation.changedIds.size <= INCREMENTAL_SCORE_ROOM_LIMIT) {
    value.score.exitPorts = incrementalExitPortQuality(value, derivation);
    return value.score.exitPorts;
  }
  return value.score.exitPorts = fullExitPortQuality(value.positions, value.edges);
}

function candidateIndex(value: Candidate): PositionIndex {
  value.score.indexed ??= positionIndex(value.positions);
  return value.score.indexed;
}

function footprintFromBounds(
  x: readonly [number, number] | undefined,
  y: readonly [number, number] | undefined,
): FootprintQuality {
  if (!x || !y) return { area: 0, perimeter: 0 };
  const width = x[1] - x[0] + 1;
  const height = y[1] - y[0] + 1;
  return { area: width * height, perimeter: 2 * (width + height) };
}

function adjustedAxisBounds(
  base: AxisCoordinateIndex | undefined,
  deltas: ReadonlyMap<number, number>,
): readonly [number, number] | undefined {
  let minimum: number | undefined;
  let maximum: number | undefined;
  if (base) {
    for (const coordinate of base.sorted) {
      if ((base.counts.get(coordinate) ?? 0) + (deltas.get(coordinate) ?? 0) > 0) {
        minimum = coordinate;
        break;
      }
    }
    for (let index = base.sorted.length - 1; index >= 0; index -= 1) {
      const coordinate = base.sorted[index];
      if ((base.counts.get(coordinate) ?? 0) + (deltas.get(coordinate) ?? 0) > 0) {
        maximum = coordinate;
        break;
      }
    }
  }
  for (const [coordinate, delta] of deltas) {
    if (delta <= 0 || base?.counts.has(coordinate)) continue;
    minimum = minimum === undefined ? coordinate : Math.min(minimum, coordinate);
    maximum = maximum === undefined ? coordinate : Math.max(maximum, coordinate);
  }
  return minimum === undefined || maximum === undefined ? undefined : [minimum, maximum];
}

function candidateFootprintIndex(value: Candidate): Map<number, LevelCoordinateIndex> {
  if (value.score.footprintIndex) return value.score.footprintIndex;
  const coordinates = new Map<number, { x: Map<number, number>; y: Map<number, number> }>();
  for (const position of value.positions.values()) {
    const level = coordinates.get(position.level) ?? {
      x: new Map<number, number>(),
      y: new Map<number, number>(),
    };
    level.x.set(position.x, (level.x.get(position.x) ?? 0) + 1);
    level.y.set(position.y, (level.y.get(position.y) ?? 0) + 1);
    coordinates.set(position.level, level);
  }
  const result = new Map<number, LevelCoordinateIndex>();
  for (const [level, axes] of coordinates) {
    result.set(level, {
      x: { counts: axes.x, sorted: [...axes.x.keys()].sort((a, b) => a - b) },
      y: { counts: axes.y, sorted: [...axes.y.keys()].sort((a, b) => a - b) },
    });
  }
  value.score.footprintIndex = result;
  return result;
}

function candidateFootprint(value: Candidate): FootprintQuality {
  if (value.score.footprint) return value.score.footprint;
  const derivation = value.derivation;
  if (!derivation) {
    const indexed = candidateIndex(value);
    return value.score.footprint = {
      area: indexed.footprintArea,
      perimeter: indexed.footprintPerimeter,
    };
  }

  const baseIndex = candidateFootprintIndex(derivation.base);
  const changes = new Map<number, { x: Map<number, number>; y: Map<number, number> }>();
  const add = (position: GridPosition, delta: number): void => {
    const level = changes.get(position.level) ?? {
      x: new Map<number, number>(),
      y: new Map<number, number>(),
    };
    level.x.set(position.x, (level.x.get(position.x) ?? 0) + delta);
    level.y.set(position.y, (level.y.get(position.y) ?? 0) + delta);
    changes.set(position.level, level);
  };
  for (const id of derivation.changedIds) {
    const before = derivation.base.positions.get(id);
    const after = value.positions.get(id);
    if (before && (!after || !samePosition(before, after))) add(before, -1);
    if (after && (!before || !samePosition(before, after))) add(after, 1);
  }

  const baseFootprint = candidateFootprint(derivation.base);
  let area = baseFootprint.area;
  let perimeter = baseFootprint.perimeter;
  for (const [level, deltas] of changes) {
    const baseLevel = baseIndex.get(level);
    const before = baseLevel
      ? footprintFromBounds(
        [baseLevel.x.sorted[0], baseLevel.x.sorted.at(-1) as number],
        [baseLevel.y.sorted[0], baseLevel.y.sorted.at(-1) as number],
      )
      : { area: 0, perimeter: 0 };
    const after = footprintFromBounds(
      adjustedAxisBounds(baseLevel?.x, deltas.x),
      adjustedAxisBounds(baseLevel?.y, deltas.y),
    );
    area += after.area - before.area;
    perimeter += after.perimeter - before.perimeter;
  }
  return value.score.footprint = { area, perimeter };
}

function candidatePhysicalEdges(value: Candidate): ScoredPhysicalEdge[] {
  if (value.score.physicalEdges) return value.score.physicalEdges;
  const result: ScoredPhysicalEdge[] = [];
  for (const link of topologyIndex(value.edges).physical) {
    const from = value.positions.get(link.a);
    const to = value.positions.get(link.b);
    if (!from || !to || from.level !== to.level) continue;
    result.push({ fromId: link.a, toId: link.b, from, to });
  }
  value.score.physicalEdges = result;
  return result;
}

function physicalLinkObstructions(
  positions: ReadonlyMap<string, GridPosition>,
  link: IndexedPhysicalLink,
): number {
  const from = positions.get(link.a);
  const to = positions.get(link.b);
  if (!from || !to || from.level !== to.level) return 0;
  let result = 0;
  for (const room of positions.values()) {
    if (segmentIntersectsRoomCell(from, to, room)) result += 1;
  }
  return result;
}

function indexLanes(indexed: PositionIndex, byRows: boolean): Map<number, number[]> {
  const known = byRows ? indexed.rowLanes : indexed.columnLanes;
  if (known) return known;
  const coordinates = new Map<number, Set<number>>();
  for (const [, position] of indexed.entries) {
    const level = coordinates.get(position.level) ?? new Set<number>();
    level.add(byRows ? position.y : position.x);
    coordinates.set(position.level, level);
  }
  const result = new Map<number, number[]>();
  for (const [level, values] of coordinates) result.set(level, [...values].sort((a, b) => a - b));
  if (byRows) indexed.rowLanes = result;
  else indexed.columnLanes = result;
  return result;
}

function indexedSegmentObstructions(
  indexed: PositionIndex,
  from: GridPosition | undefined,
  to: GridPosition | undefined,
): number {
  if (!from || !to || from.level !== to.level) return 0;
  if (from.y === to.y) {
    return countStrictlyBetween(indexed.rows.get(laneKey(from.level, from.y)), from.x, to.x);
  }
  if (from.x === to.x) {
    return countStrictlyBetween(indexed.columns.get(laneKey(from.level, from.x)), from.y, to.y);
  }
  // A room's cell reaches less than one unit past its coordinates, so only
  // rooms inside the segment's box widened by one can meet it. Walking the
  // index lanes across the box's shorter side and testing each room there
  // counts exactly the rooms a scan of every entry would.
  const byRows = Math.abs(to.y - from.y) <= Math.abs(to.x - from.x);
  const lanes = indexLanes(indexed, byRows).get(from.level);
  if (!lanes) return 0;
  const laneMinimum = (byRows ? Math.min(from.y, to.y) : Math.min(from.x, to.x)) - 1;
  const laneMaximum = (byRows ? Math.max(from.y, to.y) : Math.max(from.x, to.x)) + 1;
  const spanMinimum = (byRows ? Math.min(from.x, to.x) : Math.min(from.y, to.y)) - 1;
  const spanMaximum = (byRows ? Math.max(from.x, to.x) : Math.max(from.y, to.y)) + 1;
  const lists = byRows ? indexed.rows : indexed.columns;
  let result = 0;
  for (let lane = lowerBound(lanes, laneMinimum); lane < lanes.length && lanes[lane] <= laneMaximum; lane += 1) {
    const coordinate = lanes[lane];
    const values = lists.get(laneKey(from.level, coordinate)) as number[];
    for (let index = lowerBound(values, spanMinimum); index < values.length && values[index] <= spanMaximum; index += 1) {
      const room = byRows
        ? { x: values[index], y: coordinate, level: from.level }
        : { x: coordinate, y: values[index], level: from.level };
      if (segmentIntersectsRoomCell(from, to, room)) result += 1;
    }
  }
  return result;
}

function changedRoomObstructionDelta(
  before: ReadonlyMap<string, GridPosition>,
  after: ReadonlyMap<string, GridPosition>,
  link: IndexedPhysicalLink,
  changedIds: ReadonlySet<string>,
): number {
  const from = after.get(link.a);
  const to = after.get(link.b);
  if (!from || !to || from.level !== to.level) return 0;
  let result = 0;
  for (const id of changedIds) {
    const beforeRoom = before.get(id);
    const afterRoom = after.get(id);
    if (beforeRoom && segmentIntersectsRoomCell(from, to, beforeRoom)) result -= 1;
    if (afterRoom && segmentIntersectsRoomCell(from, to, afterRoom)) result += 1;
  }
  return result;
}

function candidateRoomObstructions(value: Candidate): number {
  if (value.score.roomObstructions !== undefined) return value.score.roomObstructions;
  const translation = rigidTranslationOf(value);
  if (translation) {
    value.score.roomObstructions = rigidRoomObstructions(value, translation);
    return value.score.roomObstructions;
  }
  const derivation = value.derivation;
  if (derivation && derivation.base.edges === value.edges &&
    derivation.changedIds.size <= INCREMENTAL_SCORE_ROOM_LIMIT) {
    let result = candidateRoomObstructions(derivation.base);
    const baseIndex = candidateIndex(derivation.base);
    for (const link of topologyIndex(value.edges).physical) {
      if (derivation.changedIds.has(link.a) || derivation.changedIds.has(link.b)) {
        const afterFrom = value.positions.get(link.a);
        const afterTo = value.positions.get(link.b);
        const afterBaseRooms = indexedSegmentObstructions(baseIndex, afterFrom, afterTo);
        const afterChangedRooms = afterFrom && afterTo && afterFrom.level === afterTo.level
          ? changedRoomObstructionDelta(
            derivation.base.positions,
            value.positions,
            link,
            derivation.changedIds,
          )
          : 0;
        const before = indexedSegmentObstructions(
          baseIndex,
          derivation.base.positions.get(link.a),
          derivation.base.positions.get(link.b),
        );
        result += afterBaseRooms + afterChangedRooms - before;
      } else {
        result += changedRoomObstructionDelta(
          derivation.base.positions,
          value.positions,
          link,
          derivation.changedIds,
        );
      }
    }
    value.score.roomObstructions = result;
    return result;
  }

  const indexed = candidateIndex(value);
  let result = 0;
  for (const edge of candidatePhysicalEdges(value)) {
    const { from, to } = edge;
    if (from.y === to.y) {
      result += countStrictlyBetween(
        indexed.rows.get(laneKey(from.level, from.y)),
        from.x,
        to.x,
      );
    } else if (from.x === to.x) {
      result += countStrictlyBetween(
        indexed.columns.get(laneKey(from.level, from.x)),
        from.y,
        to.y,
      );
    } else {
      for (const [, room] of indexed.entries) {
        if (segmentIntersectsRoomCell(from, to, room)) result += 1;
      }
    }
  }
  value.score.roomObstructions = result;
  return result;
}

export interface LayoutRoutingQuality {
  routingViolations: number;
  exitPortViolations: number;
  reciprocalExitPortViolations: number;
  roomObstructions: number;
}

/** Measure route geometry without running the complete layout planner. */
export function measureLayoutRoutingQuality(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): LayoutRoutingQuality {
  let roomObstructions = 0;
  for (const link of topologyIndex(edges).physical) {
    roomObstructions += physicalLinkObstructions(positions, link);
  }
  const ports = fullExitPortQuality(positions, edges);
  return {
    routingViolations: roomObstructions + ports.exitPortViolations,
    ...ports,
    roomObstructions,
  };
}

function physicalLinksCross(
  positions: ReadonlyMap<string, GridPosition>,
  first: IndexedPhysicalLink,
  second: IndexedPhysicalLink,
): number {
  if (first.a === second.a || first.a === second.b ||
    first.b === second.a || first.b === second.b) return 0;
  const aFrom = positions.get(first.a);
  const aTo = positions.get(first.b);
  const bFrom = positions.get(second.a);
  const bTo = positions.get(second.b);
  return aFrom && aTo && bFrom && bTo &&
      aFrom.level === aTo.level && bFrom.level === bTo.level &&
      strictSegmentsIntersect(aFrom, aTo, bFrom, bTo)
    ? 1
    : 0;
}

function candidateLinkCrossings(value: Candidate): number {
  if (value.score.linkCrossings !== undefined) return value.score.linkCrossings;
  const translation = rigidTranslationOf(value);
  if (translation) {
    // Rigid pairs keep their crossing status; the count moves by exactly the
    // affected-pair tally at the translated offset minus the cached tally at
    // the base offset.
    value.score.linkCrossings = candidateLinkCrossings(translation.context.base) +
      rigidAffectedCrossings(translation.context, translation.offset) -
      contextAffectedCrossingsBefore(translation.context);
    return value.score.linkCrossings;
  }
  const derivation = value.derivation;
  if (derivation && derivation.base.edges === value.edges &&
    derivation.changedIds.size <= INCREMENTAL_SCORE_ROOM_LIMIT) {
    const links = topologyIndex(value.edges).physical;
    const changedLinks: number[] = [];
    const changedFlags = new Uint8Array(links.length);
    for (let index = 0; index < links.length; index += 1) {
      const link = links[index];
      if (derivation.changedIds.has(link.a) || derivation.changedIds.has(link.b)) {
        changedLinks.push(index);
        changedFlags[index] = 1;
      }
    }
    let result = candidateLinkCrossings(derivation.base);
    for (const first of changedLinks) {
      for (let second = 0; second < links.length; second += 1) {
        if (first === second || (changedFlags[second] === 1 && second < first)) continue;
        const lower = Math.min(first, second);
        const upper = Math.max(first, second);
        result += physicalLinksCross(value.positions, links[lower], links[upper]) -
          physicalLinksCross(derivation.base.positions, links[lower], links[upper]);
      }
    }
    value.score.linkCrossings = result;
    return result;
  }
  value.score.linkCrossings = linkCrossingCount(candidatePhysicalEdges(value));
  return value.score.linkCrossings;
}

function candidateCollisions(value: Candidate): number {
  value.score.collisions ??= collisionGroupCount(value.positions);
  return value.score.collisions;
}

function candidateMovedExisting(value: Candidate): Set<string> {
  if (value.score.movedExisting) return value.score.movedExisting;
  const derivation = value.derivation;
  const result = derivation
    ? new Set(candidateMovedExisting(derivation.base))
    : new Set<string>();
  const ids = derivation?.changedIds ?? value.current.keys();
  for (const id of ids) {
    const before = value.current.get(id);
    if (!before) continue;
    const after = value.positions.get(id);
    if (after && !samePosition(before, after)) result.add(id);
    else result.delete(id);
  }
  value.score.movedExisting = result;
  return result;
}

function candidateQuality(value: Candidate): LayoutQuality {
  if (value.score.quality) return value.score.quality;
  const ray = candidateRayQuality(value);
  const roomObstructions = candidateRoomObstructions(value);
  const ports = candidateExitPortQuality(value);
  const linkCrossings = candidateLinkCrossings(value);
  const footprint = candidateFootprint(value);
  value.score.quality = {
    cardinalRayViolations: ray.cardinalRayViolations,
    reciprocalRayViolations: ray.reciprocalRayViolations,
    levelViolations: ray.levelViolations,
    routingViolations: roomObstructions + ports.exitPortViolations,
    exitPortViolations: ports.exitPortViolations,
    reciprocalExitPortViolations: ports.reciprocalExitPortViolations,
    roomObstructions,
    linkCrossings,
    levelSlack: ray.levelSlack,
    cardinalSlack: ray.cardinalSlack,
    footprintArea: footprint.area,
    footprintPerimeter: footprint.perimeter,
  };
  return value.score.quality;
}

/**
 * Reconcile a candidate's cached scalar score with an independent full pass.
 * Internal search keeps using incremental scoring; only publication seams pay
 * this cost so a latent delta defect can never escape as an inconsistent plan
 * or final trace event.
 */
function refreshCandidateQuality(value: Candidate): LayoutQuality {
  const quality = measureIntegralLayoutQuality(value.positions, value.edges);
  value.score.ray = {
    cardinalRayViolations: quality.cardinalRayViolations,
    reciprocalRayViolations: quality.reciprocalRayViolations ?? 0,
    levelViolations: quality.levelViolations ?? 0,
    levelSlack: quality.levelSlack ?? 0,
    cardinalSlack: quality.cardinalSlack,
  };
  value.score.roomObstructions = quality.roomObstructions;
  value.score.exitPorts = {
    exitPortViolations: quality.exitPortViolations,
    reciprocalExitPortViolations: quality.reciprocalExitPortViolations ?? 0,
  };
  value.score.linkCrossings = quality.linkCrossings;
  value.score.footprint = {
    area: quality.footprintArea,
    perimeter: quality.footprintPerimeter,
  };
  value.score.quality = quality;
  return quality;
}

/**
 * Positive means `a` is the preferred exploration state. This ordering is
 * deliberately not the public LayoutQuality contract: it ranks directional
 * violations, each mis-levelled one at its own weight, strictly first instead
 * of weighing them against routing and crossings, ignores exit ports, and
 * ranks slack above footprint, so greedy
 * search can traverse stepping stones the public order rejects on its way to
 * deeper repairs. Every publication seam re-ranks with
 * `comparePublicCandidates`, so a publicly regressing state can be explored
 * but never shipped. Pulling score stages in tuple order is intentional: a
 * candidate that loses early never pays for spatial indexes, link crossings,
 * footprint bounds, or movement accounting.
 */
function compareCandidates(a: Candidate, b: Candidate): number {
  const collisionsA = candidateCollisions(a);
  const collisionsB = candidateCollisions(b);
  if (collisionsA !== collisionsB) return collisionsB - collisionsA;

  const rayA = candidateRayQuality(a);
  const rayB = candidateRayQuality(b);
  const rayScoreA = rayQualityScore(rayA);
  const rayScoreB = rayQualityScore(rayB);
  if (rayScoreA !== rayScoreB) return rayScoreB - rayScoreA;
  if (rayA.levelViolations !== rayB.levelViolations) {
    return rayB.levelViolations - rayA.levelViolations;
  }
  if (rayA.cardinalRayViolations !== rayB.cardinalRayViolations) {
    return rayB.cardinalRayViolations - rayA.cardinalRayViolations;
  }
  if (rayA.reciprocalRayViolations !== rayB.reciprocalRayViolations) {
    return rayB.reciprocalRayViolations - rayA.reciprocalRayViolations;
  }

  const obstructionsA = candidateRoomObstructions(a);
  const obstructionsB = candidateRoomObstructions(b);
  if (obstructionsA !== obstructionsB) return obstructionsB - obstructionsA;

  const crossingsA = candidateLinkCrossings(a);
  const crossingsB = candidateLinkCrossings(b);
  if (crossingsA !== crossingsB) return crossingsB - crossingsA;
  if (rayA.cardinalSlack !== rayB.cardinalSlack) {
    return rayB.cardinalSlack - rayA.cardinalSlack;
  }
  const footprintA = candidateFootprint(a);
  const footprintB = candidateFootprint(b);
  if (footprintA.area !== footprintB.area) {
    return footprintB.area - footprintA.area;
  }
  if (footprintA.perimeter !== footprintB.perimeter) {
    return footprintB.perimeter - footprintA.perimeter;
  }
  return candidateMovedExisting(b).size - candidateMovedExisting(a).size;
}

/** The directional part of the public primary score: weighted directional and mis-levelled exits. */
function rayQualityScore(ray: RayQuality): number {
  return weightedQualityScore(ray.cardinalRayViolations, ray.levelViolations, 0, 0);
}

/** A candidate's primary score in the public quality order; see compareLayoutQuality. */
function candidateQualityScore(value: Candidate): number {
  const ray = candidateRayQuality(value);
  return weightedQualityScore(
    ray.cardinalRayViolations,
    ray.levelViolations,
    candidateRoomObstructions(value) + candidateExitPortQuality(value).exitPortViolations,
    candidateLinkCrossings(value),
  );
}

/**
 * Whether ray quality `a` loses the quality order's first tie-breaks to `b`
 * outright: more mis-levelled exits, or as many and more directional
 * violations.
 */
function rayTieLoses(
  a: Pick<RayQuality, "levelViolations" | "cardinalRayViolations">,
  b: Pick<RayQuality, "levelViolations" | "cardinalRayViolations">,
): boolean {
  return a.levelViolations !== b.levelViolations
    ? a.levelViolations > b.levelViolations
    : a.cardinalRayViolations > b.cardinalRayViolations;
}

/** A term of the public primary score beyond directional and mis-levelled violations. */
interface PublicScoreTerm {
  scored(value: Candidate): boolean;
  score(value: Candidate): number;
}

/** The score's routing and crossing terms, cheapest to score first. */
const PUBLIC_SCORE_TERMS: readonly PublicScoreTerm[] = [
  {
    scored: (value) => value.score.exitPorts !== undefined,
    score: (value) => candidateExitPortQuality(value).exitPortViolations,
  },
  {
    scored: (value) => value.score.roomObstructions !== undefined,
    score: candidateRoomObstructions,
  },
  {
    scored: (value) => value.score.linkCrossings !== undefined,
    score: candidateLinkCrossings,
  },
];

/**
 * The sign of the public primary score comparison, positive when `a` scores
 * lower. A term not yet scored counts as zero, the least it can be, so a side
 * whose partial score already exceeds the other's complete score loses
 * without scoring the rest; terms are scored cheapest first, and only while
 * the scores known so far leave the comparison open. An equal score falls to
 * mis-levelled and then directional violations first, so a partial score that
 * equals the other's complete score and loses those tie-breaks loses as well.
 */
function comparePublicScores(
  a: Candidate,
  b: Candidate,
  rayA: RayQuality,
  rayB: RayQuality,
): number {
  let scoreA = rayQualityScore(rayA);
  let scoreB = rayQualityScore(rayB);
  const pendingA: PublicScoreTerm[] = [];
  const pendingB: PublicScoreTerm[] = [];
  for (const term of PUBLIC_SCORE_TERMS) {
    if (term.scored(a)) scoreA += term.score(a);
    else pendingA.push(term);
    if (term.scored(b)) scoreB += term.score(b);
    else pendingB.push(term);
  }
  for (let nextA = 0, nextB = 0; ;) {
    const completeA = nextA === pendingA.length;
    const completeB = nextB === pendingB.length;
    if (completeA && completeB) return scoreB - scoreA;
    if (completeB && (scoreA > scoreB || (scoreA === scoreB && rayTieLoses(rayA, rayB)))) return -1;
    if (completeA && (scoreB > scoreA || (scoreB === scoreA && rayTieLoses(rayB, rayA)))) return 1;
    if (!completeA && (completeB || pendingA.length - nextA >= pendingB.length - nextB)) {
      scoreA += pendingA[nextA++].score(a);
    } else {
      scoreB += pendingB[nextB++].score(b);
    }
  }
}

function comparePublicCandidates(a: Candidate, b: Candidate): number {
  // The same order as comparing the complete LayoutQuality objects. The
  // primary score is pulled term by term only while it is undecided; the
  // remaining fields, footprint and movement are paid for only on a tie.
  const rayA = candidateRayQuality(a);
  const rayB = candidateRayQuality(b);
  const primary = comparePublicScores(a, b, rayA, rayB);
  if (primary !== 0) return primary;

  const obstructionsA = candidateRoomObstructions(a);
  const obstructionsB = candidateRoomObstructions(b);
  const portsA = candidateExitPortQuality(a);
  const portsB = candidateExitPortQuality(b);
  const routingA = obstructionsA + portsA.exitPortViolations;
  const routingB = obstructionsB + portsB.exitPortViolations;
  const crossingsA = candidateLinkCrossings(a);
  const crossingsB = candidateLinkCrossings(b);
  if (rayA.levelViolations !== rayB.levelViolations) {
    return rayB.levelViolations - rayA.levelViolations;
  }
  if (rayA.cardinalRayViolations !== rayB.cardinalRayViolations) {
    return rayB.cardinalRayViolations - rayA.cardinalRayViolations;
  }
  if (rayA.reciprocalRayViolations !== rayB.reciprocalRayViolations) {
    return rayB.reciprocalRayViolations - rayA.reciprocalRayViolations;
  }
  if (routingA !== routingB) return routingB - routingA;
  if (portsA.exitPortViolations !== portsB.exitPortViolations) {
    return portsB.exitPortViolations - portsA.exitPortViolations;
  }
  if (portsA.reciprocalExitPortViolations !== portsB.reciprocalExitPortViolations) {
    return portsB.reciprocalExitPortViolations - portsA.reciprocalExitPortViolations;
  }
  if (obstructionsA !== obstructionsB) return obstructionsB - obstructionsA;
  if (crossingsA !== crossingsB) return crossingsB - crossingsA;
  if (rayA.levelSlack !== rayB.levelSlack) return rayB.levelSlack - rayA.levelSlack;
  const footprintA = candidateFootprint(a);
  const footprintB = candidateFootprint(b);
  if (footprintA.area !== footprintB.area) return footprintB.area - footprintA.area;
  if (footprintA.perimeter !== footprintB.perimeter) {
    return footprintB.perimeter - footprintA.perimeter;
  }
  if (rayA.cardinalSlack !== rayB.cardinalSlack) {
    return rayB.cardinalSlack - rayA.cardinalSlack;
  }
  return candidateMovedExisting(b).size - candidateMovedExisting(a).size;
}

function traceCandidate(value: Candidate, includePositions = true): LayoutTraceCandidate {
  const result: LayoutTraceCandidate = {
    quality: { ...candidateQuality(value) },
    movedExisting: [...candidateMovedExisting(value)].sort(),
  };
  if (includePositions) {
    result.positions = [...value.positions]
      .sort(([a], [b]) => compareStrings(a, b))
      .map(([id, position]) => ({ id, ...position }));
  }
  return result;
}

function traceCandidateBatch(
  trace: IntegralLayoutRequest["trace"],
  stage: LayoutTraceStage,
  values: readonly (Candidate | undefined)[],
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
): void {
  if (!trace) return;
  const generated = values.filter((value): value is Candidate =>
    value !== undefined && acceptedPositions(value.positions, acceptsPositions)
  );
  const collisionFree = generated.filter((value) => candidateCollisions(value) === 0);
  collisionFree.sort((a, b) => compareCandidates(b, a));
  trace({
    type: "candidate-batch",
    stage,
    generated: generated.length,
    collisionFree: collisionFree.length,
    best: collisionFree[0] ? traceCandidate(collisionFree[0], false) : undefined,
  });
}

class DisjointSet {
  readonly #parent = new Map<string, string>();

  add(id: string): void {
    if (!this.#parent.has(id)) this.#parent.set(id, id);
  }

  find(id: string): string {
    this.add(id);
    const parent = this.#parent.get(id) ?? id;
    if (parent === id) return id;
    const root = this.find(parent);
    this.#parent.set(id, root);
    return root;
  }

  union(a: string, b: string): void {
    const rootA = this.find(a);
    const rootB = this.find(b);
    if (rootA !== rootB) this.#parent.set(rootB, rootA);
  }
}

function coherentBlocks(
  current: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): Map<string, Set<string>> {
  const sets = new DisjointSet();
  for (const id of current.keys()) sets.add(id);
  for (const edge of edges) {
    const from = current.get(edge.from);
    const to = current.get(edge.to);
    const expected = DIRECTION_VECTORS[edge.direction];
    if (from && to && expected && samePosition(subtract(to, from), expected)) {
      sets.union(edge.from, edge.to);
    }
  }

  const byRoot = new Map<string, Set<string>>();
  for (const id of current.keys()) {
    const root = sets.find(id);
    const block = byRoot.get(root) ?? new Set<string>();
    block.add(id);
    byRoot.set(root, block);
  }

  const result = new Map<string, Set<string>>();
  for (const block of byRoot.values()) {
    for (const id of block) result.set(id, block);
  }
  return result;
}

function candidate(
  positions: Map<string, GridPosition>,
  current: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  derivation?: CandidateDerivation,
): Candidate {
  const evaluator = CANDIDATE_EVALUATORS.get(current);
  const hash = evaluator ? evaluationHash(positions, evaluator, derivation) : undefined;
  // The incremental hash only selects a bucket; exact coordinate comparison
  // below makes collisions harmless and preserves deterministic selection.
  const known = hash === undefined
    ? undefined
    : evaluator?.cache.get(hash)?.find((value) =>
      sameEvaluation(value.positions, positions, evaluator.ids)
    );
  if (known) {
    if (evaluator?.recency) {
      evaluator.recency.delete(known);
      evaluator.recency.add(known);
    }
    if (!known.derivation && derivation) known.derivation = derivation;
    return known;
  }
  const result: Candidate = {
    positions,
    current,
    edges,
    score: {},
    derivation,
    cacheHash: hash,
    cacheEpoch: evaluator?.epoch,
  };
  if (hash !== undefined && evaluator) {
    if (evaluator.maximumEntries !== undefined && evaluator.recency &&
      evaluator.recency.size >= evaluator.maximumEntries) {
      const oldest = evaluator.recency.values().next().value as Candidate | undefined;
      if (oldest) {
        evaluator.recency.delete(oldest);
        const oldestHash = oldest.cacheHash;
        const oldestBucket = oldestHash === undefined ? undefined : evaluator.cache.get(oldestHash);
        if (oldestBucket) {
          const oldestIndex = oldestBucket.indexOf(oldest);
          if (oldestIndex >= 0) oldestBucket.splice(oldestIndex, 1);
          if (oldestBucket.length === 0 && oldestHash !== undefined) evaluator.cache.delete(oldestHash);
        }
      }
    }
    const bucket = evaluator.cache.get(hash) ?? [];
    bucket.push(result);
    evaluator.cache.set(hash, bucket);
    evaluator.recency?.add(result);
  }
  return result;
}

/** Retain a complete scored state without evaluator buckets or derivation chains. */
function detachedCandidate(value: Candidate): Candidate {
  const quality = { ...candidateQuality(value) };
  return {
    positions: new Map(value.positions),
    current: value.current,
    edges: value.edges,
    score: {
      collisions: candidateCollisions(value),
      fingerprintLanes: value.score.fingerprintLanes,
      // Preserve the scalar components used by incremental child scoring. The
      // heavyweight position/edge indexes remain detached, while descendants
      // avoid rebuilding their adopted base's complete score on every trial.
      ray: {
        cardinalRayViolations: quality.cardinalRayViolations,
        reciprocalRayViolations: quality.reciprocalRayViolations ?? 0,
        levelViolations: quality.levelViolations ?? 0,
        levelSlack: quality.levelSlack ?? 0,
        cardinalSlack: quality.cardinalSlack,
      },
      roomObstructions: quality.roomObstructions,
      exitPorts: {
        exitPortViolations: quality.exitPortViolations,
        reciprocalExitPortViolations: quality.reciprocalExitPortViolations ?? 0,
      },
      linkCrossings: quality.linkCrossings,
      footprint: {
        area: quality.footprintArea,
        perimeter: quality.footprintPerimeter,
      },
      movedExisting: new Set(candidateMovedExisting(value)),
      quality,
    },
  };
}

/**
 * Score temporary changes against a mutable working map, materializing a
 * complete candidate only when `compare` ranks it strictly above the retained
 * winner, so the retained one keeps its place among equals. Every changed
 * coordinate is restored before returning. This avoids cloning every resident
 * for each translation offset.
 */
function preferTemporaryPlacement(
  working: Map<string, GridPosition>,
  placement: ReadonlyMap<string, GridPosition>,
  current: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  derivation: CandidateDerivation,
  retained: Candidate | undefined,
  knownCollisions = candidateCollisions(derivation.base),
  compare: (a: Candidate, b: Candidate) => number = compareCandidates,
): Candidate | undefined {
  const previous = new Map<string, GridPosition>();
  const added = new Set<string>();
  for (const [id, position] of placement) {
    const before = working.get(id);
    if (before) previous.set(id, before);
    else added.add(id);
    working.set(id, position);
  }
  try {
    const transient: Candidate = {
      positions: working,
      current,
      edges,
      score: { collisions: knownCollisions },
      derivation,
    };
    if (retained && compare(transient, retained) <= 0) return retained;
    const materialized = candidate(new Map(working), current, edges, derivation);
    materialized.score.collisions = knownCollisions;
    return materialized;
  } finally {
    for (const id of added) working.delete(id);
    for (const [id, position] of previous) working.set(id, position);
  }
}

function changedPositionIds(
  before: ReadonlyMap<string, GridPosition>,
  after: ReadonlyMap<string, GridPosition>,
): Set<string> {
  const result = new Set<string>();
  for (const id of new Set([...before.keys(), ...after.keys()])) {
    const a = before.get(id);
    const b = after.get(id);
    if (!a || !b || !samePosition(a, b)) result.add(id);
  }
  return result;
}

function connectedComponents(ids: ReadonlySet<string>, edges: readonly LayoutEdge[]): string[][] {
  const sets = new DisjointSet();
  for (const id of ids) sets.add(id);
  for (const edge of edges) {
    if (ids.has(edge.from) && ids.has(edge.to)) sets.union(edge.from, edge.to);
  }
  const groups = new Map<string, string[]>();
  for (const id of ids) {
    const root = sets.find(id);
    const group = groups.get(root) ?? [];
    group.push(id);
    groups.set(root, group);
  }
  return [...groups.values()].sort((a, b) => b.length - a.length || compareStrings(a[0], b[0]));
}

/** Place new islands with the strongest established topological seam first. */
function connectionFirstComponents(
  ids: ReadonlySet<string>,
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): string[][] {
  const ranked = connectedComponents(ids, edges).map((component) => {
    const members = new Set(component);
    const anchors = new Set<string>();
    let edgeCount = 0;
    for (const edge of edges) {
      if (members.has(edge.from) && !members.has(edge.to) && positions.has(edge.to)) {
        edgeCount += 1;
        anchors.add(edge.to);
      }
      if (members.has(edge.to) && !members.has(edge.from) && positions.has(edge.from)) {
        edgeCount += 1;
        anchors.add(edge.from);
      }
    }
    return { component, edgeCount, anchorCount: anchors.size };
  });
  ranked.sort((a, b) =>
    b.anchorCount - a.anchorCount ||
    b.edgeCount - a.edgeCount ||
    b.component.length - a.component.length ||
    compareStrings(a.component[0], b.component[0])
  );
  return ranked.map(({ component }) => component);
}

function anchorOrigins(
  component: ReadonlySet<string>,
  positions: ReadonlyMap<string, GridPosition>,
  nodes: ReadonlyMap<string, LayoutNode>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
): Origin[] {
  const adjacent = new Set<string>();
  const adjacencyCount = new Map<string, number>();
  const seamOrigins: Origin[] = [];
  const recordAdjacent = (id: string): void => {
    adjacent.add(id);
    adjacencyCount.set(id, (adjacencyCount.get(id) ?? 0) + 1);
  };
  for (const edge of edges) {
    if (component.has(edge.from) && !component.has(edge.to) && positions.has(edge.to)) {
      if (nodes.has(edge.to)) recordAdjacent(edge.to);
      const vector = protectedVector(edge);
      const fromNode = nodes.get(edge.from);
      if (vector && fromNode && !nodes.has(edge.to)) {
        seamOrigins.push(subtract(
          subtract(positions.get(edge.to) as GridPosition, vector),
          fromNode.relative,
        ));
      }
    }
    if (component.has(edge.to) && !component.has(edge.from) && positions.has(edge.from)) {
      if (nodes.has(edge.from)) recordAdjacent(edge.from);
      const vector = protectedVector(edge);
      const toNode = nodes.get(edge.to);
      if (vector && toNode && !nodes.has(edge.from)) {
        seamOrigins.push(subtract(
          add(positions.get(edge.from) as GridPosition, vector),
          toNode.relative,
        ));
      }
    }
  }

  let anchors = [...nodes.keys()].filter((id) => positions.has(id) && !component.has(id));
  anchors.sort((a, b) => {
    if (adjacent.has(a) !== adjacent.has(b)) return adjacent.has(a) ? -1 : 1;
    const connectionDifference = (adjacencyCount.get(b) ?? 0) - (adjacencyCount.get(a) ?? 0);
    if (connectionDifference !== 0) return connectionDifference;
    if (a === centerId) return -1;
    if (b === centerId) return 1;
    return compareStrings(a, b);
  });

  // Once a component actually touches the durable topology, unrelated chart
  // anchors are noise. Seed it exclusively from the rooms at that seam.
  if (adjacent.size > 0) anchors = anchors.filter((id) => adjacent.has(id));

  return uniqueOffsets([
    // A directional edge to a resident omitted from the incoming chart is a
    // complete placement seam: its vector supplies the missing relative
    // coordinate. Prefer those exact origins before weaker chart anchors.
    ...seamOrigins,
    ...anchors.slice(0, 8).map((id) => {
      const position = positions.get(id) as GridPosition;
      const node = nodes.get(id) as LayoutNode;
      return subtract(position, node.relative);
    }),
  ]);
}

function knownOrigins(
  positions: ReadonlyMap<string, GridPosition>,
  nodes: ReadonlyMap<string, LayoutNode>,
  centerId: string | undefined,
): Origin[] {
  const anchors = [...nodes.keys()].filter((id) => positions.has(id));
  anchors.sort((a, b) => {
    if (a === centerId) return -1;
    if (b === centerId) return 1;
    return compareStrings(a, b);
  });
  return uniqueOffsets(anchors.slice(0, 8).map((id) =>
    subtract(positions.get(id) as GridPosition, (nodes.get(id) as LayoutNode).relative)
  ));
}

function packedOrigin(
  component: readonly string[],
  positions: ReadonlyMap<string, GridPosition>,
  nodes: ReadonlyMap<string, LayoutNode>,
): Origin {
  const relative = component.map((id) => nodes.get(id)?.relative).filter((value): value is GridPosition => !!value);
  const relativeBounds = bounds(relative);
  const occupiedBounds = bounds(positions.values());
  let minLevel = 0;
  for (const [index, position] of relative.entries()) {
    if (index === 0 || position.level < minLevel) minLevel = position.level;
  }
  if (!relativeBounds || !occupiedBounds) {
    return {
      x: relativeBounds ? -relativeBounds.minX : 0,
      y: relativeBounds ? -relativeBounds.minY : 0,
      level: -minLevel,
    };
  }
  return {
    x: occupiedBounds.maxX + ISLAND_GAP - relativeBounds.minX,
    y: occupiedBounds.minY - relativeBounds.minY,
    level: -minLevel,
  };
}

function componentPositions(
  ids: readonly string[],
  nodes: ReadonlyMap<string, LayoutNode>,
  origin: Origin,
  translation: Origin = { x: 0, y: 0, level: 0 },
): Map<string, GridPosition> {
  const result = new Map<string, GridPosition>();
  const combined = {
    x: origin.x + translation.x,
    y: origin.y + translation.y,
    level: origin.level + translation.level,
  };
  for (const id of ids) {
    const node = nodes.get(id);
    if (node) result.set(id, add(node.relative, combined));
  }
  return result;
}

function occupiedCells(positions: ReadonlyMap<string, GridPosition>): Map<CellKey, string> {
  const occupied = new Map<CellKey, string>();
  for (const [id, position] of positions) occupied.set(cellKey(position), id);
  return occupied;
}

function fits(
  placement: ReadonlyMap<string, GridPosition>,
  occupied: ReadonlyMap<CellKey, string>,
): boolean {
  const staged = new Set<CellKey>();
  for (const [id, position] of placement) {
    const key = cellKey(position);
    const occupant = occupied.get(key);
    if ((occupant && occupant !== id) || staged.has(key)) return false;
    staged.add(key);
  }
  return true;
}

/**
 * Find the nearest collision-free rigid translation in each cardinal
 * direction. Collision repair used to enumerate every offset in a Manhattan
 * radius (313 offsets at the default radius) and globally score each valid
 * one. A collision can only change when one of the moving cells crosses a
 * stationary cell, so walking outward to the first clear integral distance on
 * each axis is sufficient for the Arctic-style fallback and preserves the
 * preferred cardinal character of the map.
 */
function cardinalClearanceOffsets(
  moving: readonly GridPosition[],
  stationary: ReadonlyMap<string, GridPosition>,
): Origin[] {
  if (moving.length === 0) return [];
  const occupied = new Set<CellKey>([...stationary.values()].map(cellKey));
  const movingBounds = bounds(moving);
  const stationaryBounds = bounds(stationary.values());
  const maximumDistance = movingBounds && stationaryBounds
    ? Math.max(
      Math.abs(stationaryBounds.maxX - movingBounds.minX),
      Math.abs(movingBounds.maxX - stationaryBounds.minX),
      Math.abs(stationaryBounds.maxY - movingBounds.minY),
      Math.abs(movingBounds.maxY - stationaryBounds.minY),
    ) + 1
    : 1;
  const result: Origin[] = [];
  for (const axis of EXPANSION_AXES) {
    for (let distance = 1; distance <= maximumDistance; distance += 1) {
      const offset = { x: axis.dx * distance, y: axis.dy * distance, level: 0 };
      if (moving.every((position) => !occupied.has(cellKey(add(position, offset))))) {
        result.push(offset);
        break;
      }
    }
  }
  return result;
}

/** Cardinal translations which make a boundary constraint exactly adjacent. */
function cardinalConstraintOffsets(
  movingIds: ReadonlySet<string>,
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): Origin[] {
  const result: Origin[] = [];
  for (const edge of edges) {
    const expected = CARDINAL_VECTORS[edge.direction];
    const fromMoves = movingIds.has(edge.from);
    const toMoves = movingIds.has(edge.to);
    if (!expected || fromMoves === toMoves) continue;
    const from = positions.get(edge.from);
    const to = positions.get(edge.to);
    if (!from || !to) continue;
    const offset = fromMoves
      ? subtract({
        x: to.x - expected.x,
        y: to.y - expected.y,
        level: to.level - expected.level,
      }, from)
      : subtract(add(from, expected), to);
    const cardinal = offset.level === 0 &&
      ((offset.x === 0) !== (offset.y === 0));
    if (cardinal) result.push(offset);
  }
  return uniqueOffsets(result);
}

function bestStablePlacement(
  initial: Map<string, GridPosition>,
  current: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  nodes: ReadonlyMap<string, LayoutNode>,
  centerId: string | undefined,
): Candidate | undefined {
  const positions = new Map(initial);
  const newIds = new Set([...nodes.keys()].filter((id) => !current.has(id)));
  for (const component of connectionFirstComponents(newIds, positions, edges)) {
    const componentSet = new Set(component);
    const origins = anchorOrigins(componentSet, positions, nodes, edges, centerId);
    if (origins.length === 0) origins.push(packedOrigin(component, positions, nodes));
    const occupied = occupiedCells(positions);
    const baseCandidate = candidate(new Map(positions), current, edges);

    let best: Candidate | undefined;
    for (const origin of origins) {
      const base = componentPositions(component, nodes, origin);
      const offsets = uniqueOffsets([
        ...NEARBY_OFFSETS,
        ...farOffsets(base.values(), positions.values()),
      ]);
      for (const offset of offsets) {
        const placement = componentPositions(component, nodes, origin, offset);
        // Offsets are ordered by distance, not by a monotonic collision
        // predicate. A blocked origin says nothing about the next translation.
        if (!fits(placement, occupied)) continue;
        best = preferTemporaryPlacement(positions, placement, current, edges, {
          base: baseCandidate,
          changedIds: componentSet,
        }, best);
      }
    }
    if (!best) return undefined;
    for (const id of component) {
      const position = best.positions.get(id);
      if (position) positions.set(id, position);
    }
  }
  return candidate(positions, current, edges);
}

/** One block of rooms that moves as a unit; `relative` positions are the block's own coordinates. */
export interface RigidBlock {
  readonly id: string;
  readonly rooms: readonly LayoutNode[];
}

export interface RigidBlockPlacementRequest {
  /** Rooms already in the destination map; they never move. */
  readonly residents: readonly LayoutResident[];
  readonly blocks: readonly RigidBlock[];
  /** Traversals among residents and block rooms; edges naming unknown ids are ignored. */
  readonly edges: readonly LayoutEdge[];
}

/**
 * An integer offset per block, such that every block room lands on an
 * unoccupied cell.
 *
 * Each block is one rigid component whatever its internal connectivity: every
 * room lands on `relative + offset`. Blocks are placed one at a time, the block
 * with the most edges to already placed rooms first and ties by block id, and a
 * placed block occupies its cells for the next, so a chain of blocks aligns
 * transitively.
 *
 * The candidate offsets are the ones a new chart component is anchored with:
 * the origin each seam link with a protected direction (cardinal, Up/Down, or
 * a constraint vector) implies, the nearby shifts of an origin, and islands
 * past the occupied bounds. Origins are ranked by the seam links they satisfy
 * and only the best eight are searched, so links that all disagree cost no
 * more than eight origins do. Among the candidates on which no block room
 * meets an occupied cell, the `compareLayoutQuality` order chooses: an offset
 * that satisfies more directions wins unless another saves more blocked
 * routes, exit ports and crossings than the directions it gives up are worth,
 * and all of those count before footprint and slack. Candidates are compared
 * the most satisfied directions first, then in search order (origins by rank,
 * each with its shifts nearest first and then its islands), and the first of
 * equals wins; one that satisfies fewer directions is compared only while it
 * could still win. Comparing a candidate scores the whole map, so fewer are
 * compared as the map grows: hundreds on a map of a few dozen rooms, the first
 * eight that fit on a map of thousands.
 *
 * A block without such a link to placed rooms becomes an island east of them,
 * aligned with their top edge and keeping its own levels; with nothing placed
 * yet it keeps its coordinates. Rooms of one block that share a cell keep
 * sharing it. Blocks without rooms are omitted; the result lists the others in
 * request order.
 *
 * Room ids must be unique across residents and blocks, and block ids across
 * blocks. The result depends only on the request, not on the order of its
 * residents, blocks, rooms or edges. Offsets are exact only while every
 * coordinate stays a safe integer; a block that no candidate places within
 * that range on unoccupied cells is an error.
 */
export function placeRigidBlocks(request: RigidBlockPlacementRequest): Map<string, GridPosition> {
  const known = new Set<string>();
  const claim = (id: string): void => {
    if (known.has(id)) throw new Error(`layout id ${id} appears more than once`);
    known.add(id);
  };
  const current = new Map<string, GridPosition>();
  for (const resident of request.residents) {
    claim(resident.id);
    current.set(resident.id, integral(resident.position));
  }
  const blockIds = new Set<string>();
  const pending: { id: string; nodes: Map<string, LayoutNode> }[] = [];
  for (const block of request.blocks) {
    if (blockIds.has(block.id)) throw new Error(`rigid block ${block.id} appears more than once`);
    blockIds.add(block.id);
    const nodes = new Map<string, LayoutNode>();
    for (const room of block.rooms) {
      claim(room.id);
      nodes.set(room.id, { id: room.id, relative: integral(room.relative) });
    }
    if (nodes.size > 0) pending.push({ id: block.id, nodes });
  }
  const edges = request.edges.filter((edge) => known.has(edge.from) && known.has(edge.to));
  const incident = topologyIndex(edges).incident;

  const placed = new Map(current);
  const offsets = new Map<string, GridPosition>();
  while (pending.length > 0) {
    let chosen = 0;
    let chosenLinks = -1;
    for (const [index, block] of pending.entries()) {
      let links = 0;
      for (const id of block.nodes.keys()) {
        for (const edge of incident.get(id) ?? []) {
          const inside = block.nodes.has(edge.from);
          if (inside !== block.nodes.has(edge.to) && placed.has(inside ? edge.to : edge.from)) links += 1;
        }
      }
      if (links > chosenLinks ||
        (links === chosenLinks && compareStrings(block.id, pending[chosen].id) < 0)) {
        chosen = index;
        chosenLinks = links;
      }
    }
    const [block] = pending.splice(chosen, 1);
    const offset = rigidBlockOffset(block.id, block.nodes, placed, current, edges);
    offsets.set(block.id, offset);
    for (const [id, node] of block.nodes) placed.set(id, add(node.relative, offset));
  }

  const result = new Map<string, GridPosition>();
  for (const block of request.blocks) {
    const offset = offsets.get(block.id);
    if (offset) result.set(block.id, offset);
  }
  return result;
}

/**
 * The most seam-implied origins one rigid block searches around, as many as
 * the chart anchors a new component is seeded from. Each origin brings a
 * radius of nearby shifts and its islands, so without a cap the search would
 * grow with every seam link that disagrees with the others.
 */
const RIGID_SEARCH_ORIGINS = 8;

/**
 * Rooms and edges that comparing one block's candidates may score, each
 * candidate counting every room and edge of the map. A map of a few dozen
 * rooms compares hundreds of candidates; one of thousands compares the
 * minimum, in search order.
 */
const RIGID_TIE_SCORING_BUDGET = 50_000;
const RIGID_TIE_MINIMUM = 8;

/**
 * Seam edges between a block and placed rooms, grouped by the block offsets
 * that keep them on their protected rays: an edge holds exactly when
 * `offset - base` is a positive multiple of `step`, with that multiple less
 * one as its slack. Both edges of a two-way link share one ray.
 */
interface SeamRay {
  base: Origin;
  step: GridPosition;
  /** Directed seam edges on this ray. */
  edges: number;
  /** Those with an exact protected reverse edge. */
  reciprocal: number;
}

/** The part of an offset's ray quality that depends on the offset. */
interface SeamScore {
  violations: number;
  reciprocalViolations: number;
  /** The violations whose rooms sit on levels their exit rules out. */
  levelViolations: number;
  slack: number;
}

function seamRays(
  nodes: ReadonlyMap<string, LayoutNode>,
  placed: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): SeamRay[] {
  const reciprocal = reciprocalProtectedEdges(edges);
  const rays = new Map<string, SeamRay>();
  for (const edge of edges) {
    const fromNode = nodes.get(edge.from);
    const toNode = nodes.get(edge.to);
    if ((fromNode === undefined) === (toNode === undefined)) continue;
    const other = placed.get(fromNode ? edge.to : edge.from);
    const vector = protectedVector(edge);
    if (!other || !vector) continue;
    // With the block room at `relative + offset`, the edge's `to - from` is
    // `base - offset` when it leaves the block and `offset - base` when it
    // enters it; negating both sides of the ray test turns the first into the
    // second.
    const base = subtract(other, ((fromNode ?? toNode) as LayoutNode).relative);
    const step = fromNode ? negatedOrigin(vector) : vector;
    const key = `${offsetKey(base)}/${offsetKey(step)}`;
    let ray = rays.get(key);
    if (!ray) {
      ray = { base, step, edges: 0, reciprocal: 0 };
      rays.set(key, ray);
    }
    ray.edges += 1;
    if (reciprocal.has(edge)) ray.reciprocal += 1;
  }
  return [...rays.values()];
}

/**
 * Only seam edges change their ray quality when a block moves rigidly, so
 * this orders offsets exactly as the ray fields of the full quality would.
 */
function seamScore(rays: readonly SeamRay[], offset: Origin): SeamScore {
  const delta = { x: 0, y: 0, level: 0 };
  const score = { violations: 0, reciprocalViolations: 0, levelViolations: 0, slack: 0 };
  for (const ray of rays) {
    delta.x = offset.x - ray.base.x;
    delta.y = offset.y - ray.base.y;
    delta.level = offset.level - ray.base.level;
    const distance = vectorRayDistance(ray.step, delta);
    if (distance === undefined) {
      score.violations += ray.edges;
      score.reciprocalViolations += ray.reciprocal;
      // An edge leaving the block negates both its delta and its step, which
      // keeps whether their levels agree.
      if (Math.sign(delta.level) !== Math.sign(ray.step.level)) score.levelViolations += ray.edges;
    } else {
      score.slack += ray.edges * (distance - 1);
    }
  }
  return score;
}

/**
 * The routing violations and crossings of a block's placement that no offset
 * changes: those among the placed rooms and their links, and those inside the
 * block. An offset only adds more, where the block's rooms and links meet the
 * placed ones.
 */
function offsetIndependentRoutingAndCrossings(
  nodes: ReadonlyMap<string, LayoutNode>,
  placed: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): number {
  const block = new Map([...nodes].map(([id, node]) => [id, node.relative]));
  let result = 0;
  for (const positions of [placed, block]) {
    const inside = edges.filter((edge) => positions.has(edge.from) && positions.has(edge.to));
    const quality = measureIntegralLayoutQuality(positions, inside);
    result += quality.routingViolations + quality.linkCrossings;
  }
  return result;
}

/** A seam's part of the primary score: its weighted directional and mis-levelled exits. */
function seamRayScore(score: SeamScore): number {
  return weightedQualityScore(score.violations, score.levelViolations, 0, 0);
}

/**
 * Positive when `a` is worse in the ray part of the quality order: its
 * weighted score, then its mis-levelled, directional and reciprocal
 * violations. Slack is the order's last field.
 */
function compareSeamDirections(a: SeamScore, b: SeamScore): number {
  return seamRayScore(a) - seamRayScore(b) || a.levelViolations - b.levelViolations ||
    a.violations - b.violations || a.reciprocalViolations - b.reciprocalViolations;
}

/**
 * The best collision-free offset for one rigid block. The seam score decides
 * the ray fields of the quality order by itself, so candidates are taken best
 * weighted ray score first and scored in full: as translations of the block
 * from the best origin, rescored from the moved/stationary boundary alone, as
 * many as the tie budget allows. A worse ray score is scored only while it can
 * still win, that is while the routing violations and crossings the best
 * offset has beyond those no offset removes outweigh its extra weighted
 * directional and mis-levelled violations.
 */
function rigidBlockOffset(
  blockId: string,
  nodes: ReadonlyMap<string, LayoutNode>,
  placed: ReadonlyMap<string, GridPosition>,
  current: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): Origin {
  const ids = [...nodes.keys()];
  const moving = new Set(ids);
  // Rooms sharing a cell inside the block share it at every offset, so the
  // fit test probes one room per distinct cell.
  const cells = new Set<CellKey>();
  const probe: GridPosition[] = [];
  for (const node of nodes.values()) {
    const key = cellKey(node.relative);
    if (cells.has(key)) continue;
    cells.add(key);
    probe.push(node.relative);
  }
  // A translation keeps distinct rooms on distinct cells only while every
  // coordinate stays a safe integer, so the fit test also demands that.
  const safe = Number.isSafeInteger;
  const extent = {
    minX: Infinity, maxX: -Infinity,
    minY: Infinity, maxY: -Infinity,
    minLevel: Infinity, maxLevel: -Infinity,
  };
  let exact = true;
  for (const relative of probe) {
    exact &&= safe(relative.x) && safe(relative.y) && safe(relative.level);
    extent.minX = Math.min(extent.minX, relative.x);
    extent.maxX = Math.max(extent.maxX, relative.x);
    extent.minY = Math.min(extent.minY, relative.y);
    extent.maxY = Math.max(extent.maxY, relative.y);
    extent.minLevel = Math.min(extent.minLevel, relative.level);
    extent.maxLevel = Math.max(extent.maxLevel, relative.level);
  }
  const occupied = occupiedCells(placed);
  const fitsAt = (offset: Origin): boolean => {
    if (!exact ||
      !safe(offset.x + extent.minX) || !safe(offset.x + extent.maxX) ||
      !safe(offset.y + extent.minY) || !safe(offset.y + extent.maxY) ||
      !safe(offset.level + extent.minLevel) || !safe(offset.level + extent.maxLevel)) return false;
    for (const relative of probe) {
      const key = cellKeyAt(relative.x + offset.x, relative.y + offset.y, relative.level + offset.level);
      if (occupied.has(key)) return false;
    }
    return true;
  };
  const unplaceable = (): Error =>
    new Error(`could not find a collision-free offset for rigid block ${blockId}`);

  const rays = seamRays(nodes, placed, edges);
  if (rays.length === 0) {
    if (placed.size === 0) return { x: 0, y: 0, level: 0 };
    // The island an unanchored chart component gets, except that a block's
    // levels are real map levels and stay as they are.
    const island = packedOrigin(ids, placed, nodes);
    const offset = { x: island.x, y: island.y, level: 0 };
    if (!fitsAt(offset)) throw unplaceable();
    return offset;
  }

  // Every origin a seam edge implies, ranked by its own seam score and then
  // by coordinates, so the ranking ignores the order of the request's edges.
  const origins = uniqueOffsets(rays.map((ray) => add(ray.base, ray.step)))
    .map((origin) => ({ origin, score: seamScore(rays, origin) }))
    .sort((a, b) =>
      compareSeamDirections(a.score, b.score) || a.score.slack - b.score.slack ||
      a.origin.level - b.origin.level || a.origin.x - b.origin.x || a.origin.y - b.origin.y
    )
    .slice(0, RIGID_SEARCH_ORIGINS)
    .map(({ origin }) => origin);

  // Nearby origins share most of their shifts; each offset keeps the place
  // it was first searched at, which breaks ties in the quality order.
  const searched = new Set<string>();
  const candidates: { offset: Origin; order: number; score: SeamScore }[] = [];
  for (const origin of origins) {
    const islands = farOffsets(probe.map((relative) => add(relative, origin)), placed.values());
    for (const shift of [...NEARBY_OFFSETS, ...islands]) {
      const offset = add(origin, shift);
      const key = offsetKey(offset);
      if (searched.has(key)) continue;
      searched.add(key);
      candidates.push({ offset, order: candidates.length, score: seamScore(rays, offset) });
    }
  }
  candidates.sort((a, b) => compareSeamDirections(a.score, b.score) || a.order - b.order);

  const reference = origins[0];
  const base = candidate(
    new Map([...placed, ...componentPositions(ids, nodes, reference)]),
    current,
    edges,
  );
  const context = rigidTranslationContext(base, moving);
  const working = new Map(base.positions);
  const collisions = collisionGroupCount(placed);
  // Full scoring touches every room and edge of the map, so large maps
  // compare fewer candidates.
  const fullScores = Math.max(
    RIGID_TIE_MINIMUM,
    Math.floor(RIGID_TIE_SCORING_BUDGET / (placed.size + nodes.size + edges.length)),
  );
  let scored = 0;
  let best: Candidate | undefined;
  let bestEntry: (typeof candidates)[number] | undefined;
  let bestScore = 0;
  let bestRay: RayQuality | undefined;
  // Only seam edges change their rays when the block moves, so every offset
  // draws this many other exits wrong, this many of them mis-levelled.
  let fixedDirectional = 0;
  let fixedLevel = 0;
  let fixedRoutingAndCrossings: number | undefined;
  for (const entry of candidates) {
    if (scored === fullScores) break;
    if (bestEntry && bestRay && seamRayScore(entry.score) > seamRayScore(bestEntry.score)) {
      // Every later candidate's seam scores no better than this one's, which
      // is worse than the best one's. An offset scores at least its weighted
      // directional and mis-levelled violations plus the routing violations
      // and crossings among the placed rooms and inside the block, which are
      // the same at every offset. An equal score ties, and the tie goes to
      // fewer mis-levelled, then directional, violations; a later candidate
      // with this seam score has at least as many of either.
      const ray = {
        cardinalRayViolations: fixedDirectional + entry.score.violations,
        levelViolations: fixedLevel + entry.score.levelViolations,
      };
      const leastRay = weightedQualityScore(ray.cardinalRayViolations, ray.levelViolations, 0, 0);
      const cannotWin = (least: number): boolean =>
        least > bestScore || (least === bestScore && rayTieLoses(ray, bestRay as RayQuality));
      if (cannotWin(leastRay)) break;
      fixedRoutingAndCrossings ??= offsetIndependentRoutingAndCrossings(nodes, placed, edges);
      if (cannotWin(leastRay + fixedRoutingAndCrossings)) break;
    }
    if (!fitsAt(entry.offset)) continue;
    scored += 1;
    const preferred = preferTemporaryPlacement(working, componentPositions(ids, nodes, entry.offset), current, edges, {
      base,
      changedIds: moving,
      translation: { offset: subtract(entry.offset, reference), context },
    }, best, collisions, comparePublicCandidates);
    if (preferred && preferred !== best) {
      best = preferred;
      bestEntry = entry;
      bestScore = candidateQualityScore(preferred);
      bestRay = candidateRayQuality(preferred);
      fixedDirectional = bestRay.cardinalRayViolations - entry.score.violations;
      fixedLevel = bestRay.levelViolations - entry.score.levelViolations;
    }
  }
  // Each origin's eastern island clears every occupied cell, so only
  // coordinates past the safe-integer range leave nothing that fits.
  if (!bestEntry) throw unplaceable();
  return bestEntry.offset;
}

function chooseKeeper(
  occupants: readonly string[],
  protectedIds: ReadonlySet<string>,
  residents: ReadonlyMap<string, LayoutResident>,
  centerId: string | undefined,
): string {
  return [...occupants].sort((a, b) => {
    if (a === centerId) return -1;
    if (b === centerId) return 1;
    if (protectedIds.has(a) !== protectedIds.has(b)) return protectedIds.has(a) ? -1 : 1;
    const fixedA = residents.get(a)?.movable === false;
    const fixedB = residents.get(b)?.movable === false;
    if (fixedA !== fixedB) return fixedA ? -1 : 1;
    return compareStrings(a, b);
  })[0];
}

function moveCollisionBlocks(
  initial: Map<string, GridPosition>,
  protectedIds: ReadonlySet<string>,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  blocks: ReadonlyMap<string, Set<string>>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
): Map<string, GridPosition> | undefined {
  let positions = new Map(initial);
  for (let iteration = 0; iteration <= residents.size; iteration += 1) {
    const collision = collisionGroups(positions)[0];
    if (!collision) return positions;
    const keeper = chooseKeeper(collision, protectedIds, residents, centerId);
    const loser = collision.find((id) =>
      id !== keeper && !protectedIds.has(id) && residents.get(id)?.movable !== false
    );
    if (!loser) return undefined;

    const coherent = blocks.get(loser) ?? new Set([loser]);
    const canMoveWholeBlock = [...coherent].every((id) =>
      !protectedIds.has(id) && residents.get(id)?.movable !== false
    );
    const movingIds = canMoveWholeBlock ? [...coherent] : [loser];
    const movingPositions = movingIds.map((id) => positions.get(id)).filter((value): value is GridPosition => !!value);
    const stationary = new Map(positions);
    for (const id of movingIds) stationary.delete(id);
    const occupied = occupiedCells(stationary);
    const stationaryCollisions = collisionGroupCount(stationary);
    const baseCandidate = candidate(positions, current, edges);

    let best: Candidate | undefined;
    const movingIdSet = new Set(movingIds);
    const offsets = uniqueOffsets([
      ...cardinalConstraintOffsets(movingIdSet, positions, edges),
      ...cardinalClearanceOffsets(movingPositions, stationary),
      ...farOffsets(movingPositions, stationary.values()),
    ]);
    for (const offset of offsets) {
      const placement = new Map<string, GridPosition>();
      for (const id of movingIds) {
        const position = positions.get(id);
        if (position) placement.set(id, add(position, offset));
      }
      // Other collisions may still be waiting for the next loop iteration;
      // this translation only has to avoid introducing one for this block.
      if (!fits(placement, occupied)) continue;
      best = preferTemporaryPlacement(stationary, placement, current, edges, {
        base: baseCandidate,
        changedIds: movingIdSet,
      }, best, stationaryCollisions);
    }
    if (!best) return undefined;
    positions = best.positions;
  }
  return undefined;
}

function positionMapKey(positions: ReadonlyMap<string, GridPosition>): string {
  return [...positions]
    .sort(([a], [b]) => compareStrings(a, b))
    .map(([id, position]) => `${id}@${cellKeyString(position)}`)
    .join("|");
}

/**
 * Two independent 32-bit lanes, XOR-combined over per-entry hashes, condense
 * a whole position map into a 16-character fingerprint without materializing
 * the O(n) sorted canonical string — XOR makes the result independent of
 * entry order, so no sort is needed either. The dedup sets retaining these
 * fingerprints (crossing-repair state sets and the compaction seen sets,
 * following the same posture the retained-state reduction established) exist
 * only to suppress re-expansion of already visited states: a lane collision
 * can at most skip re-exploring one candidate state — it can never admit or
 * publish an unvetted layout. Coordinates are hashed through `| 0`; every
 * producer of these states rounds to integers first.
 */
function positionEntryHashLanes(id: string, position: GridPosition): [number, number] {
  let first = 0x811c9dc5;
  let second = 0x9e3779b9;
  for (let index = 0; index < id.length; index += 1) {
    const code = id.charCodeAt(index);
    first = Math.imul(first ^ code, 0x01000193) >>> 0;
    second = Math.imul(second ^ (code + 0x7f4a7c15), 0x85ebca6b) >>> 0;
  }
  // Coordinates get murmur3's full block mix (multiply, rotate, multiply,
  // fold, rotate, affine). A plain xor-then-multiply fold is not enough
  // here: XOR-combining entries exposed structured whole-map collisions in
  // practice — a room at (-7,1) hashed identically to (7,-1), and pairs of
  // rooms translated by one shared offset came out XOR-neutral. The second
  // lane folds the coordinates in a different order so the lanes stay
  // independent even for ids whose FNV states happen to collide.
  first = mixCoordinate32(first, position.x | 0);
  first = mixCoordinate32(first, position.y | 0);
  first = mixCoordinate32(first, position.level | 0);
  second = mixCoordinate32(second, position.y | 0);
  second = mixCoordinate32(second, position.level | 0);
  second = mixCoordinate32(second, position.x | 0);
  return [avalanche32(first), avalanche32(second)];
}

/** Murmur3 block mix: absorb one 32-bit value into a running lane state. */
function mixCoordinate32(state: number, value: number): number {
  let block = Math.imul(value, 0xcc9e2d51);
  block = ((block << 15) | (block >>> 17)) >>> 0;
  block = Math.imul(block, 0x1b873593);
  let result = (state ^ block) >>> 0;
  result = ((result << 13) | (result >>> 19)) >>> 0;
  return (Math.imul(result, 5) + 0xe6546b64) >>> 0;
}

/** Murmur3-style 32-bit finalizer: every input bit reaches every output bit. */
function avalanche32(value: number): number {
  let result = value >>> 0;
  result ^= result >>> 16;
  result = Math.imul(result, 0x85ebca6b) >>> 0;
  result ^= result >>> 13;
  result = Math.imul(result, 0xc2b2ae35) >>> 0;
  result ^= result >>> 16;
  return result >>> 0;
}

function fingerprintFromLanes(lanes: readonly [number, number]): string {
  return `${lanes[0].toString(16).padStart(8, "0")}${lanes[1].toString(16).padStart(8, "0")}`;
}

function positionsFingerprintLanes(
  positions: ReadonlyMap<string, GridPosition>,
): [number, number] {
  let first = 0;
  let second = 0;
  for (const [id, position] of positions) {
    const entry = positionEntryHashLanes(id, position);
    first ^= entry[0];
    second ^= entry[1];
  }
  return [first >>> 0, second >>> 0];
}

function positionsFingerprint(positions: ReadonlyMap<string, GridPosition>): string {
  return fingerprintFromLanes(positionsFingerprintLanes(positions));
}

/**
 * A candidate's fingerprint, computed from its base's lanes and the changed
 * ids when a derivation is available: XOR combining lets each changed entry's
 * old hash cancel out and its new hash fold in. Identical to the full-map
 * fingerprint by construction, so candidates and raw maps share one dedup
 * key space.
 */
function candidateFingerprintLanes(value: Candidate): [number, number] {
  if (value.score.fingerprintLanes) return value.score.fingerprintLanes;
  const derivation = value.derivation;
  if (derivation) {
    const base = candidateFingerprintLanes(derivation.base);
    let first = base[0];
    let second = base[1];
    for (const id of derivation.changedIds) {
      const before = derivation.base.positions.get(id);
      const after = value.positions.get(id);
      if (before) {
        const entry = positionEntryHashLanes(id, before);
        first ^= entry[0];
        second ^= entry[1];
      }
      if (after) {
        const entry = positionEntryHashLanes(id, after);
        first ^= entry[0];
        second ^= entry[1];
      }
    }
    value.score.fingerprintLanes = [first >>> 0, second >>> 0];
    return value.score.fingerprintLanes;
  }
  value.score.fingerprintLanes = positionsFingerprintLanes(value.positions);
  return value.score.fingerprintLanes;
}

function candidateFingerprint(value: Candidate): string {
  return fingerprintFromLanes(candidateFingerprintLanes(value));
}

function orderedPositionMapKey(
  positions: ReadonlyMap<string, GridPosition>,
  ids: readonly string[],
): string {
  if (positions.size !== ids.length) return positionMapKey(positions);
  const values: string[] = [];
  for (const id of ids) {
    const position = positions.get(id);
    if (!position) return positionMapKey(positions);
    values.push(`${id}@${cellKeyString(position)}`);
  }
  return values.join("|");
}

function positionHash(index: number, position: GridPosition): number {
  let result = Math.imul(index + 1, -1640531527);
  result ^= Math.imul(position.x | 0, -2048144789);
  result ^= Math.imul(position.y | 0, -1028477387);
  result ^= Math.imul(position.level | 0, 668265263);
  return result >>> 0;
}

function evaluationHash(
  positions: ReadonlyMap<string, GridPosition>,
  evaluator: CandidateEvaluator,
  derivation?: CandidateDerivation,
): number {
  if (derivation?.base.cacheHash !== undefined &&
    derivation.base.cacheEpoch === evaluator.epoch) {
    let result = derivation.base.cacheHash;
    for (const id of derivation.changedIds) {
      const index = evaluator.idIndexes.get(id);
      if (index === undefined) continue;
      const before = derivation.base.positions.get(id);
      const after = positions.get(id);
      if (before) result ^= positionHash(index, before);
      if (after) result ^= positionHash(index, after);
    }
    return result >>> 0;
  }
  let result = 0;
  for (let index = 0; index < evaluator.ids.length; index += 1) {
    const id = evaluator.ids[index];
    const position = positions.get(id);
    if (position) result ^= positionHash(index, position);
  }
  return result >>> 0;
}

function sameEvaluation(
  a: ReadonlyMap<string, GridPosition>,
  b: ReadonlyMap<string, GridPosition>,
  ids: readonly string[],
): boolean {
  for (const id of ids) {
    const left = a.get(id);
    const right = b.get(id);
    if (left === undefined || right === undefined) {
      if (left !== right) return false;
    } else if (!samePosition(left, right)) {
      return false;
    }
  }
  return true;
}

interface ExpansionAxis {
  dx: number;
  dy: number;
  includes(position: GridPosition, cut: GridPosition): boolean;
}

const EXPANSION_AXES: readonly ExpansionAxis[] = [
  { dx: 0, dy: -1, includes: (position, cut) => position.y <= cut.y },
  { dx: 1, dy: 0, includes: (position, cut) => position.x >= cut.x },
  { dx: 0, dy: 1, includes: (position, cut) => position.y >= cut.y },
  { dx: -1, dy: 0, includes: (position, cut) => position.x <= cut.x },
];

function strictSegmentsIntersect(
  a: GridPosition,
  b: GridPosition,
  c: GridPosition,
  d: GridPosition,
): boolean {
  if (a.level !== b.level || a.level !== c.level || a.level !== d.level) return false;
  const cross = (p: GridPosition, q: GridPosition, r: GridPosition): number =>
    (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x);
  const abC = cross(a, b, c);
  const abD = cross(a, b, d);
  const cdA = cross(c, d, a);
  const cdB = cross(c, d, b);
  // Collinear contact and shared endpoints do not cross the open swept path.
  return ((abC < 0 && abD > 0) || (abC > 0 && abD < 0)) &&
    ((cdA < 0 && cdB > 0) || (cdA > 0 && cdB < 0));
}

interface RoomOccupantIndex {
  cells: Map<CellKey, string | string[]>;
  order: ReadonlyMap<string, number>;
  collisions: number;
}

function roomOccupantIndex(positions: ReadonlyMap<string, GridPosition>): RoomOccupantIndex {
  const cells = new Map<CellKey, string | string[]>();
  const order = new Map<string, number>();
  let collisions = 0;
  let index = 0;
  for (const [id, position] of positions) {
    order.set(id, index++);
    const key = cellKey(position);
    const known = cells.get(key);
    if (known === undefined) cells.set(key, id);
    else if (typeof known === "string") {
      cells.set(key, [known, id]);
      collisions += 1;
    } else known.push(id);
  }
  return { cells, order, collisions };
}

function removeIndexedOccupant(indexed: RoomOccupantIndex, key: CellKey, id: string): void {
  const known = indexed.cells.get(key);
  if (known === undefined) return;
  if (typeof known === "string") {
    if (known === id) indexed.cells.delete(key);
    return;
  }
  const position = known.indexOf(id);
  if (position < 0) return;
  known.splice(position, 1);
  if (known.length === 1) {
    indexed.cells.set(key, known[0]);
    indexed.collisions -= 1;
  } else if (known.length === 0) {
    indexed.cells.delete(key);
    indexed.collisions -= 1;
  }
}

function addIndexedOccupant(indexed: RoomOccupantIndex, key: CellKey, id: string): void {
  const known = indexed.cells.get(key);
  if (known === undefined) {
    indexed.cells.set(key, id);
    return;
  }
  if (typeof known === "string") {
    const values = [known, id];
    values.sort((a, b) => (indexed.order.get(a) ?? 0) - (indexed.order.get(b) ?? 0));
    indexed.cells.set(key, values);
    indexed.collisions += 1;
    return;
  }
  const insertionOrder = indexed.order.get(id) ?? 0;
  let position = 0;
  while (position < known.length && (indexed.order.get(known[position]) ?? 0) <= insertionOrder) {
    position += 1;
  }
  known.splice(position, 0, id);
}

/**
 * Reflect a rigid translation in an occupant index without owning a mutable
 * positions map, mirroring translateIndexedRooms' remove-all-then-add-all
 * discipline so intra-group displacement cannot momentarily collide.
 */
function retranslateIndexedOccupants(
  indexed: RoomOccupantIndex,
  ids: Iterable<string>,
  before: ReadonlyMap<string, GridPosition>,
  offset: Origin,
): void {
  const moved: [string, GridPosition][] = [];
  for (const id of ids) {
    const position = before.get(id);
    if (!position) continue;
    moved.push([id, position]);
    removeIndexedOccupant(indexed, cellKey(position), id);
  }
  for (const [id, position] of moved) {
    addIndexedOccupant(indexed, cellKey(add(position, offset)), id);
  }
}

function translateIndexedRooms(
  positions: Map<string, GridPosition>,
  ids: Iterable<string>,
  offset: Origin,
  indexed: RoomOccupantIndex,
): void {
  const moved: [string, GridPosition][] = [];
  for (const id of ids) {
    const position = positions.get(id);
    if (!position) continue;
    moved.push([id, position]);
    removeIndexedOccupant(indexed, cellKey(position), id);
  }
  for (const [id, position] of moved) {
    const translated = add(position, offset);
    positions.set(id, translated);
    addIndexedOccupant(indexed, cellKey(translated), id);
  }
}

function safePushClosure(
  positions: ReadonlyMap<string, GridPosition>,
  roots: ReadonlySet<string>,
  protectedIds: ReadonlySet<string>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  offset: Origin,
  indexedOccupants?: RoomOccupantIndex,
): Set<string> | undefined {
  const topology = topologyIndex(edges);
  const occupants = indexedOccupants?.cells ?? roomOccupantIndex(positions).cells;
  // An integral one-cell cardinal push cannot strictly cross an axis-aligned
  // link: an integral row/column can only meet at an endpoint or collinearly.
  // Only diagonal links need the substantially more expensive segment test.
  const sweptLinks: IndexedPhysicalLink[] = [];
  const sweptLinkPositions: GridPosition[] = [];
  for (const link of topology.physical) {
    const from = positions.get(link.a);
    const to = positions.get(link.b);
    if (!from || !to || from.level !== to.level || from.x === to.x || from.y === to.y) continue;
    sweptLinks.push(link);
    sweptLinkPositions.push(from, to);
  }

  const closure = new Set<string>();
  const queued = new Set<string>();
  const queue: string[] = [];
  let queueIndex = 0;
  const enqueue = (id: string): void => {
    // Protected ids describe the staged patch. Its cells may be crossed during
    // an intermediate one-cell push, but the patch itself never moves.
    if (protectedIds.has(id) || queued.has(id)) return;
    queued.add(id);
    queue.push(id);
  };
  const enqueueOccupants = (value: string | readonly string[] | undefined): void => {
    if (typeof value === "string") enqueue(value);
    else if (value) for (const id of value) enqueue(id);
  };
  for (const id of roots) enqueue(id);

  while (queueIndex < queue.length) {
    const id = queue[queueIndex++];
    const position = positions.get(id);
    if (!position) continue;
    if (residents.get(id)?.movable === false) return undefined;
    closure.add(id);

    const destination = add(position, offset);
    enqueueOccupants(occupants.get(cellKey(destination)));

    for (const edge of topology.incident.get(id) ?? []) {
      const from = positions.get(edge.from);
      const to = positions.get(edge.to);
      const expected = DIRECTION_VECTORS[edge.direction];
      if (!from || !to || !expected) continue;
      const directedDelta = subtract(to, from);
      const coherent = CARDINAL_VECTORS[edge.direction]
        ? cardinalRayDistance(edge.direction, directedDelta) !== undefined
        : samePosition(directedDelta, expected);
      if (!coherent) continue;
      const otherId = edge.from === id ? edge.to : edge.from;
      if (protectedIds.has(otherId)) continue;
      const other = positions.get(otherId);
      if (!other) continue;
      const delta = subtract(other, position);
      const perpendicular = delta.x * offset.x + delta.y * offset.y === 0;
      if (!perpendicular) continue;

      enqueue(otherId);
      if (other.level !== position.level) continue;
      const pushedOther = add(other, offset);
      const minX = Math.min(position.x, pushedOther.x);
      const maxX = Math.max(position.x, pushedOther.x);
      const minY = Math.min(position.y, pushedOther.y);
      const maxY = Math.max(position.y, pushedOther.y);
      const cellCount = (maxX - minX + 1) * (maxY - minY + 1);
      if (cellCount <= positions.size) {
        for (let x = minX; x <= maxX; x += 1) {
          for (let y = minY; y <= maxY; y += 1) {
            enqueueOccupants(occupants.get(cellKeyAt(x, y, position.level)));
          }
        }
      } else {
        for (const [roomId, roomPosition] of positions) {
          if (protectedIds.has(roomId) || roomPosition.level !== position.level) continue;
          if (roomPosition.x >= minX && roomPosition.x <= maxX &&
            roomPosition.y >= minY && roomPosition.y <= maxY) {
            enqueue(roomId);
          }
        }
      }
    }

    // A push must not sweep a room through an existing link. Pull both link
    // endpoints into the closure, matching Arctic's recursive map-push rule.
    for (let linkIndex = 0; linkIndex < sweptLinks.length; linkIndex += 1) {
      const link = sweptLinks[linkIndex];
      if (link.a === id || link.b === id ||
        protectedIds.has(link.a) || protectedIds.has(link.b)) continue;
      const from = sweptLinkPositions[linkIndex * 2];
      const to = sweptLinkPositions[linkIndex * 2 + 1];
      if (from.level !== position.level) continue;
      // The swept path is one cardinal cell. Test its constant axis first, then
      // test only the two path endpoints against the link. This is the same
      // strict two-sided segment predicate as strictSegmentsIntersect, with two
      // of its four cross products reduced to scalar comparisons.
      let crosses = false;
      if (offset.x !== 0) {
        if ((from.y < position.y && to.y > position.y) ||
          (to.y < position.y && from.y > position.y)) {
          const sideA = (to.x - from.x) * (position.y - from.y) -
            (to.y - from.y) * (position.x - from.x);
          const sideB = (to.x - from.x) * (destination.y - from.y) -
            (to.y - from.y) * (destination.x - from.x);
          crosses = (sideA < 0 && sideB > 0) || (sideA > 0 && sideB < 0);
        }
      } else {
        if ((from.x < position.x && to.x > position.x) ||
          (to.x < position.x && from.x > position.x)) {
          const sideA = (to.x - from.x) * (position.y - from.y) -
            (to.y - from.y) * (position.x - from.x);
          const sideB = (to.x - from.x) * (destination.y - from.y) -
            (to.y - from.y) * (destination.x - from.x);
          crosses = (sideA < 0 && sideB > 0) || (sideA > 0 && sideB < 0);
        }
      }
      if (crosses) {
        enqueue(link.a);
        enqueue(link.b);
      }
    }
  }
  return closure.size > 0 ? closure : undefined;
}

/**
 * Insert space with a sequence of one-cell, geometry-safe pushes. Unlike a
 * half-plane shift, the moving set is the recursive causal closure of the
 * blockers: perpendicular correct-ray cardinal neighbors (or exact non-cardinal
 * neighbors), destination occupants, swept rooms, and endpoints of crossed links.
 */
export function safePushRepairs(
  initial: Map<string, GridPosition>,
  protectedIds: ReadonlySet<string>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
): Map<string, GridPosition>[] {
  const protectedPositions = [...protectedIds]
    .map((id) => initial.get(id))
    .filter((position): position is GridPosition => position !== undefined);
  const patchBounds = bounds(protectedPositions);
  if (!patchBounds) return [];

  const initialRoots = new Set<string>();
  for (const occupants of collisionGroups(initial)) {
    if (!occupants.some((id) => protectedIds.has(id))) continue;
    for (const id of occupants) if (!protectedIds.has(id)) initialRoots.add(id);
  }
  if (initialRoots.size === 0) return [];

  const result: Map<string, GridPosition>[] = [];
  const seen = new Set<string>();
  for (const axis of EXPANSION_AXES) {
    const positions = new Map(initial);
    const indexedOccupants = roomOccupantIndex(positions);
    let moving = new Set(initialRoots);
    const span = axis.dx === 0
      ? patchBounds.maxY - patchBounds.minY + 1
      : patchBounds.maxX - patchBounds.minX + 1;
    for (let distance = 1; distance <= span + 1; distance += 1) {
      for (const occupants of collisionGroups(positions)) {
        if (!occupants.some((id) => protectedIds.has(id))) continue;
        for (const id of occupants) if (!protectedIds.has(id)) moving.add(id);
      }
      const closure = safePushClosure(
        positions,
        moving,
        protectedIds,
        residents,
        edges,
        { x: axis.dx, y: axis.dy, level: 0 },
        indexedOccupants,
      );
      if (!closure) break;
      translateIndexedRooms(
        positions,
        closure,
        { x: axis.dx, y: axis.dy, level: 0 },
        indexedOccupants,
      );
      moving = closure;

      if (indexedOccupants.collisions > 0) continue;
      const key = positionMapKey(positions);
      if (!seen.has(key)) {
        seen.add(key);
        result.push(new Map(positions));
      }
      break;
    }
  }
  return result;
}

interface ObstructionRepairCandidate {
  candidate: Candidate;
  edge: LayoutEdge;
  offset: GridPosition;
  obstructing: string[];
  moved: string[];
}

interface ObstructionRoomIndex {
  coordinates: PositionIndex;
  occupants: Map<CellKey, string | string[]>;
}

function obstructionRoomIndex(value: Candidate): ObstructionRoomIndex {
  const occupants = new Map<CellKey, string | string[]>();
  for (const [id, position] of value.positions) {
    const key = cellKey(position);
    const known = occupants.get(key);
    if (known === undefined) occupants.set(key, id);
    else if (typeof known === "string") occupants.set(key, [known, id]);
    else known.push(id);
  }
  return { coordinates: candidateIndex(value), occupants };
}

function obstructingRoomIds(
  positions: ReadonlyMap<string, GridPosition>,
  fromId: string,
  toId: string,
  indexed?: ObstructionRoomIndex,
): string[] {
  const from = positions.get(fromId);
  const to = positions.get(toId);
  if (!from || !to || from.level !== to.level) return [];
  if (indexed && (from.y === to.y || from.x === to.x)) {
    const horizontal = from.y === to.y;
    const values = horizontal
      ? indexed.coordinates.rows.get(laneKey(from.level, from.y))
      : indexed.coordinates.columns.get(laneKey(from.level, from.x));
    if (!values) return [];
    const minimum = Math.min(horizontal ? from.x : from.y, horizontal ? to.x : to.y);
    const maximum = Math.max(horizontal ? from.x : from.y, horizontal ? to.x : to.y);
    const result = new Set<string>();
    for (let index = lowerBound(values, minimum + 1); index < lowerBound(values, maximum); index += 1) {
      const coordinate = values[index];
      const key = horizontal
        ? cellKeyAt(coordinate, from.y, from.level)
        : cellKeyAt(from.x, coordinate, from.level);
      const occupants = indexed.occupants.get(key);
      if (typeof occupants === "string") result.add(occupants);
      else if (occupants) for (const id of occupants) result.add(id);
    }
    result.delete(fromId);
    result.delete(toId);
    return [...result].sort();
  }
  return [...positions]
    .filter(([id, position]) =>
      id !== fromId && id !== toId && segmentIntersectsRoomCell(from, to, position)
    )
    .map(([id]) => id)
    .sort();
}

function obstructingRoomCount(
  positions: ReadonlyMap<string, GridPosition>,
  fromId: string,
  toId: string,
  ids: Iterable<string>,
): number {
  const from = positions.get(fromId);
  const to = positions.get(toId);
  if (!from || !to || from.level !== to.level) return 0;
  let result = 0;
  for (const id of ids) {
    if (id === fromId || id === toId) continue;
    const position = positions.get(id);
    if (position && segmentIntersectsRoomCell(from, to, position)) result += 1;
  }
  return result;
}

/**
 * Extend a set of rooms on a link through the cardinal branch trailing behind
 * the requested push. Leaving that branch in place would merely replace a room
 * obstruction with a stretched link crossing the line we are trying to clear.
 */
function trailingCardinalRoots(
  positions: ReadonlyMap<string, GridPosition>,
  initialRoots: ReadonlySet<string>,
  protectedIds: ReadonlySet<string>,
  edges: readonly LayoutEdge[],
  lineOrigin: GridPosition,
  offset: GridPosition,
): Set<string> {
  const result = new Set(initialRoots);
  const queue = [...initialRoots];
  const incident = topologyIndex(edges).incident;
  while (queue.length > 0) {
    const id = queue.shift() as string;
    const position = positions.get(id);
    if (!position) continue;
    for (const edge of incident.get(id) ?? []) {
      const from = positions.get(edge.from);
      const to = positions.get(edge.to);
      if (!from || !to || from.level !== to.level ||
        cardinalRayDistance(edge.direction, subtract(to, from)) === undefined) continue;
      const otherId = edge.from === id ? edge.to : edge.from;
      if (protectedIds.has(otherId) || result.has(otherId)) continue;
      const other = positions.get(otherId);
      if (!other || other.level !== lineOrigin.level) continue;
      const delta = subtract(other, position);
      const parallelToPush = delta.x * offset.y - delta.y * offset.x === 0;
      if (!parallelToPush) continue;

      // Positive projection is already on the leading side of the protected
      // line. Zero and negative projections would trail through it.
      const projection = (other.x - lineOrigin.x) * offset.x +
        (other.y - lineOrigin.y) * offset.y;
      if (projection > 0) continue;
      result.add(otherId);
      queue.push(otherId);
    }
  }
  return result;
}

function pushDistancePastLine(
  positions: ReadonlyMap<string, GridPosition>,
  roots: ReadonlySet<string>,
  lineOrigin: GridPosition,
  offset: GridPosition,
): number {
  let minimumProjection = Number.POSITIVE_INFINITY;
  for (const id of roots) {
    const position = positions.get(id);
    if (!position || position.level !== lineOrigin.level) continue;
    minimumProjection = Math.min(
      minimumProjection,
      (position.x - lineOrigin.x) * offset.x + (position.y - lineOrigin.y) * offset.y,
    );
  }
  return Number.isFinite(minimumProjection)
    ? Math.max(0, 1 - minimumProjection)
    : 0;
}

/**
 * Clear rooms from the open segment of an existing link. The endpoints stay
 * fixed while every obstructing room and its trailing cardinal branch are
 * pushed perpendicular to the segment with the same recursive closure used by
 * Arctic-style map push. Repeated one-cell pushes carry the full branch beyond
 * the protected line instead of replacing room obstructions with link crossings.
 */
function* obstructionRepairCandidates(
  base: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
): Generator<ObstructionRepairCandidate> {
  const obstructed: { edge: LayoutEdge; ids: string[]; distance: number }[] = [];
  const seenEdges = new Set<string>();
  const indexed = obstructionRoomIndex(base);
  for (const edge of edges) {
    if (edge.from === edge.to) continue;
    const from = base.positions.get(edge.from);
    const to = base.positions.get(edge.to);
    if (!from || !to || from.level !== to.level) continue;
    const key = edge.from <= edge.to
      ? `${edge.from}|${edge.to}`
      : `${edge.to}|${edge.from}`;
    if (seenEdges.has(key)) continue;
    seenEdges.add(key);
    const ids = obstructingRoomIds(base.positions, edge.from, edge.to, indexed);
    if (ids.length > 0) obstructed.push({ edge, ids, distance: manhattan(from, to) });
  }
  obstructed.sort((a, b) => b.ids.length - a.ids.length || b.distance - a.distance);

  const seenPositions = new Set<string>();
  for (const { edge, ids } of obstructed.slice(0, 48)) {
    const from = base.positions.get(edge.from) as GridPosition;
    const to = base.positions.get(edge.to) as GridPosition;
    const delta = subtract(to, from);
    const offsets: GridPosition[] = delta.y === 0
      ? [{ x: 0, y: -1, level: 0 }, { x: 0, y: 1, level: 0 }]
      : delta.x === 0
      ? [{ x: -1, y: 0, level: 0 }, { x: 1, y: 0, level: 0 }]
      : EXPANSION_AXES.map(({ dx, dy }) => ({ x: dx, y: dy, level: 0 }));
    const protectedIds = new Set([edge.from, edge.to]);

    for (const offset of offsets) {
      const trial = new Map(base.positions);
      const indexedOccupants = roomOccupantIndex(trial);
      let bestForDirection: ObstructionRepairCandidate | undefined;
      let moving = trailingCardinalRoots(
        base.positions,
        new Set(ids),
        protectedIds,
        edges,
        from,
        offset,
      );
      let distanceLimit = pushDistancePastLine(
        base.positions,
        moving,
        from,
        offset,
      );
      const allMoved = new Set<string>();
      for (let distance = 1; distance <= distanceLimit; distance += 1) {
        const closure = safePushClosure(
          trial,
          moving,
          protectedIds,
          residents,
          edges,
          offset,
          indexedOccupants,
        );
        if (!closure) break;
        translateIndexedRooms(trial, closure, offset, indexedOccupants);
        for (const id of closure) allMoved.add(id);
        moving = closure;
        distanceLimit = Math.max(
          distanceLimit,
          distance + pushDistancePastLine(trial, closure, from, offset),
        );

        if (indexedOccupants.collisions > 0) continue;
        // Every original obstructing room is a push root and therefore joins
        // allMoved on the first successful closure. Unmoved rooms cannot have
        // changed their intersection with the fixed protected segment.
        if (obstructingRoomCount(trial, edge.from, edge.to, allMoved) >= ids.length) continue;
        const derivation = { base, changedIds: new Set(allMoved) };
        const transient: Candidate = {
          positions: trial,
          current,
          edges,
          score: { collisions: 0 },
          derivation,
        };
        // The mutable trial advances after this comparison. Validate it before
        // it can displace an admissible direction winner, then materialize only
        // accepted geometry.
        if (acceptsPositions && !acceptsPositions(trial)) continue;
        if (bestForDirection &&
          compareCandidates(transient, bestForDirection.candidate) <= 0) continue;
        // The working map advances again on the next unit push. Materialize
        // only an improving direction winner; cached score objects retain the
        // immutable GridPosition values captured by this shallow map copy.
        const repair = {
          candidate: {
            ...transient,
            positions: new Map(trial),
          },
          edge,
          offset: {
            x: offset.x * distance,
            y: offset.y * distance,
            level: offset.level * distance,
          },
          obstructing: ids,
          moved: [...allMoved].sort(),
        } satisfies ObstructionRepairCandidate;
        bestForDirection = repair;
      }
      if (bestForDirection) {
        const key = positionMapKey(bestForDirection.candidate.positions);
        if (!seenPositions.has(key)) {
          seenPositions.add(key);
          yield bestForDirection;
        }
      }
    }
  }
}

/**
 * Retain the same stable, descending prefix produced by Array#sort without
 * keeping every full position map alive. Equal candidates stay in generation
 * order, matching the stable sort used by the original repair pass.
 */
function retainPreferredObstructionRepair(
  retained: ObstructionRepairCandidate[],
  value: ObstructionRepairCandidate,
  maximum: number,
): void {
  let index = 0;
  while (index < retained.length &&
    compareCandidates(value.candidate, retained[index].candidate) <= 0) index += 1;
  if (index >= maximum) return;
  retained.splice(index, 0, value);
  if (retained.length > maximum) retained.pop();
}

function greedyObstructionRepair(
  seed: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  trace: IntegralLayoutRequest["trace"],
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
): Candidate | undefined {
  let best = seed;
  let changed = false;
  const limit = Math.min(8, Math.max(1, edges.length));
  for (let iteration = 0; iteration < limit; iteration += 1) {
    const repairs: ObstructionRepairCandidate[] = [];
    for (const repair of obstructionRepairCandidates(
      best,
      current,
      residents,
      edges,
      acceptsPositions,
    )) {
      retainPreferredObstructionRepair(repairs, repair, 8);
    }
    trace?.({
      type: "obstruction-candidates",
      stage: "link-obstruction-repair",
      iteration,
      candidates: repairs.slice(0, 8).map((repair) => ({
        edge: { ...repair.edge },
        offset: { ...repair.offset },
        obstructing: repair.obstructing,
        moved: repair.moved,
        result: traceCandidate(repair.candidate, false),
      })),
    });
    const accepted = repairs[0];
    if (!accepted || compareCandidates(accepted.candidate, best) <= 0) break;
    trace?.({
      type: "obstruction-repair",
      stage: "link-obstruction-repair",
      iteration,
      edge: { ...accepted.edge },
      offset: { ...accepted.offset },
      obstructing: accepted.obstructing,
      moved: accepted.moved,
      before: traceCandidate(best),
      after: traceCandidate(accepted.candidate),
    });
    best = detachedCandidate(accepted.candidate);
    changed = true;
  }
  return changed ? best : undefined;
}

/**
 * Resolve a patch collision by inserting rows or columns into the map. Every
 * room on the far side of the collision moves together, so the map grows
 * instead of ejecting one blocker to a remote island.
 */
function expansionRepairs(
  initial: Map<string, GridPosition>,
  protectedIds: ReadonlySet<string>,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  blocks: ReadonlyMap<string, Set<string>>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
): Map<string, GridPosition>[] {
  const protectedPositions = [...protectedIds]
    .map((id) => initial.get(id))
    .filter((position): position is GridPosition => position !== undefined);
  const patchBounds = bounds(protectedPositions);
  if (!patchBounds) return [];

  const result: Map<string, GridPosition>[] = [];
  const seen = new Set<string>();
  const collisions = collisionGroups(initial);
  for (const occupants of collisions) {
    if (!occupants.some((id) => protectedIds.has(id))) continue;
    const blockerIds = occupants.filter((id) => !protectedIds.has(id));
    if (blockerIds.length === 0) continue;
    const cut = initial.get(occupants[0]);
    if (!cut) continue;

    for (const axis of EXPANSION_AXES) {
      const movingIds = [...initial]
        .filter(([id, position]) =>
          !protectedIds.has(id) && position.level === cut.level && axis.includes(position, cut)
        )
        .map(([id]) => id);
      if (!blockerIds.some((id) => movingIds.includes(id))) continue;
      if (movingIds.some((id) => residents.get(id)?.movable === false)) continue;

      const span = axis.dx === 0
        ? patchBounds.maxY - patchBounds.minY + 1
        : patchBounds.maxX - patchBounds.minX + 1;
      for (let distance = 1; distance <= span + 1; distance += 1) {
        const offset = { x: axis.dx * distance, y: axis.dy * distance, level: 0 };
        const trial = new Map(initial);
        for (const id of movingIds) {
          const position = trial.get(id);
          if (position) trial.set(id, add(position, offset));
        }

        const repaired = !hasCollisions(trial)
          ? trial
          : moveCollisionBlocks(
            trial,
            protectedIds,
            current,
            residents,
            blocks,
            edges,
            centerId,
          );
        if (!repaired || hasCollisions(repaired)) continue;
        const key = positionMapKey(repaired);
        if (seen.has(key)) continue;
        seen.add(key);
        result.push(repaired);
      }
    }
  }
  return result;
}

function movableRegion(
  ids: ReadonlySet<string>,
  residents: ReadonlyMap<string, LayoutResident>,
): boolean {
  return [...ids].every((id) => residents.get(id)?.movable !== false);
}

function endpointRegions(
  positions: ReadonlyMap<string, GridPosition>,
  movingId: string,
  stationaryId: string,
  coherent: ReadonlyMap<string, Set<string>>,
): Set<string>[] {
  const moving = positions.get(movingId);
  const stationary = positions.get(stationaryId);
  if (!moving || !stationary) return [];

  const result: Set<string>[] = [];
  const seen = new Set<string>();
  const addRegion = (ids: Iterable<string>): void => {
    const region = new Set(ids);
    if (!region.has(movingId) || region.has(stationaryId)) return;
    const key = [...region].sort().join("|");
    if (seen.has(key)) return;
    seen.add(key);
    result.push(region);
  };

  addRegion(coherent.get(movingId) ?? [movingId]);
  if (moving.x !== stationary.x) {
    addRegion([...positions]
      .filter(([, position]) => position.level === moving.level &&
        (moving.x < stationary.x ? position.x <= moving.x : position.x >= moving.x))
      .map(([id]) => id));
  }
  if (moving.y !== stationary.y) {
    addRegion([...positions]
      .filter(([, position]) => position.level === moving.level &&
        (moving.y < stationary.y ? position.y <= moving.y : position.y >= moving.y))
      .map(([id]) => id));
  }
  return result;
}

function shiftedRegionRepairs(
  base: Candidate,
  region: ReadonlySet<string>,
  offset: Origin,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  liveBlocks: ReadonlyMap<string, Set<string>>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
): Candidate[] {
  if (!movableRegion(region, residents)) return [];
  // Keep the player's current anchor stable while elastic regions around it
  // are tried. Golden/reflow candidates may still move it when truly needed.
  if (centerId && region.has(centerId)) return [];
  if (offset.x === 0 && offset.y === 0 && offset.level === 0) return [];

  const shifted = new Map(base.positions);
  for (const id of region) {
    const position = shifted.get(id);
    if (position) shifted.set(id, add(position, offset));
  }

  const maps: Map<string, GridPosition>[] = [];
  if (!hasCollisions(shifted)) {
    maps.push(shifted);
  } else {
    const repaired = moveCollisionBlocks(
      shifted,
      region,
      current,
      residents,
      liveBlocks,
      edges,
      centerId,
    );
    if (repaired) maps.push(repaired);
  }

  return maps
    .filter((positions) => !hasCollisions(positions))
    .map((positions) => candidate(positions, current, edges, {
      base,
      changedIds: changedPositionIds(base.positions, positions),
    }));
}

interface RayAlignmentAttempt {
  movingId: string;
  stationaryId: string;
  unit: Origin;
  distance: number;
}

/**
 * Perpendicular one-cell pushes which put a correctly directed cardinal link
 * on its proper row or column without forcing its endpoints adjacent. This is
 * deliberately distinct from exact-edge repair: added slack is always better
 * than leaving a cardinal link angled.
 */
function rayAlignmentAttempts(
  edge: LayoutEdge,
  expected: GridPosition,
  from: GridPosition,
  to: GridPosition,
): RayAlignmentAttempt[] {
  if (from.level !== to.level) return [];
  const delta = subtract(to, from);
  if (expected.x !== 0) {
    if (Math.sign(delta.x) !== expected.x || delta.y === 0) return [];
    const dy = Math.sign(delta.y);
    return [
      {
        movingId: edge.from,
        stationaryId: edge.to,
        unit: { x: 0, y: dy, level: 0 },
        distance: Math.abs(delta.y),
      },
      {
        movingId: edge.to,
        stationaryId: edge.from,
        unit: { x: 0, y: -dy, level: 0 },
        distance: Math.abs(delta.y),
      },
    ];
  }
  if (expected.y !== 0) {
    if (Math.sign(delta.y) !== expected.y || delta.x === 0) return [];
    const dx = Math.sign(delta.x);
    return [
      {
        movingId: edge.from,
        stationaryId: edge.to,
        unit: { x: dx, y: 0, level: 0 },
        distance: Math.abs(delta.x),
      },
      {
        movingId: edge.to,
        stationaryId: edge.from,
        unit: { x: -dx, y: 0, level: 0 },
        distance: Math.abs(delta.x),
      },
    ];
  }
  return [];
}

/** Carry one endpoint's causal Arctic closure as far as ray alignment needs. */
function repeatedRayAlignmentPush(
  base: Candidate,
  attempt: RayAlignmentAttempt,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
): Candidate | undefined {
  const positions = new Map(base.positions);
  const indexedOccupants = roomOccupantIndex(positions);
  const protectedIds = new Set([attempt.stationaryId]);
  if (centerId) protectedIds.add(centerId);
  let moving = new Set([attempt.movingId]);
  const allMoved = new Set<string>();

  for (let step = 0; step < attempt.distance; step += 1) {
    const closure = safePushClosure(
      positions,
      moving,
      protectedIds,
      residents,
      edges,
      attempt.unit,
      indexedOccupants,
    );
    if (!closure) return undefined;
    translateIndexedRooms(positions, closure, attempt.unit, indexedOccupants);
    for (const id of closure) allMoved.add(id);
    if (indexedOccupants.collisions > 0) return undefined;
    moving = closure;
  }
  return candidate(positions, current, edges, { base, changedIds: allMoved });
}

/** Try rigid-block and whole-side translations which make a bad orthogonal edge exact. */
function* edgeRepairCandidates(
  base: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  nodes: ReadonlyMap<string, LayoutNode>,
  centerId: string | undefined,
): Generator<Candidate> {
  const liveBlocks = coherentBlocks(base.positions, edges);
  const seen = new Set<string>();
  const keyIds = [...base.positions.keys()].sort();
  const seenConstraints = new Set<string>();
  const badEdges = edges.flatMap((edge) => {
    const expected = ORTHOGONAL_VECTORS[edge.direction];
    const from = base.positions.get(edge.from);
    const to = base.positions.get(edge.to);
    if (!expected || !from || !to || samePosition(subtract(to, from), expected)) return [];
    const forward = edge.from <= edge.to;
    const normalized = forward
      ? expected
      : { x: -expected.x, y: -expected.y, level: -expected.level };
    const key = forward
      ? `${edge.from}|${edge.to}|${offsetKey(normalized)}`
      : `${edge.to}|${edge.from}|${offsetKey(normalized)}`;
    if (seenConstraints.has(key)) return [];
    seenConstraints.add(key);
    const visible = Number(nodes.has(edge.from)) + Number(nodes.has(edge.to));
    const priority = (edge.from === centerId || edge.to === centerId ? 4 : 0) + visible;
    const distance = manhattan(from, to);
    return [{ edge, expected, from, to, priority, distance }];
  }).sort((a, b) => b.priority - a.priority || b.distance - a.distance).slice(0, 48);

  for (const { edge, expected, from, to } of badEdges) {

    for (const attempt of rayAlignmentAttempts(edge, expected, from, to)) {
      const aligned = repeatedRayAlignmentPush(
        base,
        attempt,
        current,
        residents,
        edges,
        centerId,
      );
      if (!aligned) continue;
      const alignedFrom = aligned.positions.get(edge.from);
      const alignedTo = aligned.positions.get(edge.to);
      if (!alignedFrom || !alignedTo ||
        cardinalRayDistance(edge.direction, subtract(alignedTo, alignedFrom)) === undefined) continue;
      const key = orderedPositionMapKey(aligned.positions, keyIds);
      if (seen.has(key)) continue;
      seen.add(key);
      yield aligned;
    }

    const targetOffset = subtract(add(from, expected), to);
    const sourceGoal = {
      x: to.x - expected.x,
      y: to.y - expected.y,
      level: to.level - expected.level,
    };
    const sourceOffset = subtract(sourceGoal, from);
    const attempts: [string, string, Origin][] = [
      [edge.to, edge.from, targetOffset],
      [edge.from, edge.to, sourceOffset],
    ];
    for (const [movingId, stationaryId, offset] of attempts) {
      for (const region of endpointRegions(base.positions, movingId, stationaryId, liveBlocks)) {
        for (const repaired of shiftedRegionRepairs(
          base,
          region,
          offset,
          current,
          residents,
          liveBlocks,
          edges,
          centerId,
        )) {
          const key = orderedPositionMapKey(repaired.positions, keyIds);
          if (seen.has(key)) continue;
          seen.add(key);
          yield repaired;
        }
      }
    }
  }
}

interface GreedyCardinalRepairResult {
  /** Endpoint of the established private exploration path. */
  endpoint: Candidate;
  /** Best publishable state observed along that path. */
  publicBest: Candidate;
}

function greedyCardinalRepair(
  seed: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  nodes: ReadonlyMap<string, LayoutNode>,
  centerId: string | undefined,
  evaluationIds: readonly string[],
  trace: IntegralLayoutRequest["trace"],
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
): GreedyCardinalRepairResult | undefined {
  let best = seed;
  let publicBest = seed;
  let changed = false;
  const limit = Math.min(8, Math.max(1, edges.length));
  for (let iteration = 0; iteration < limit; iteration += 1) {
    const endEpoch = beginCandidateEvaluatorEpoch(current, evaluationIds, 8);
    try {
      let next = best;
      for (const repaired of edgeRepairCandidates(
        best,
        current,
        residents,
        edges,
        nodes,
        centerId,
      )) {
        if (!acceptedPositions(repaired.positions, acceptsPositions)) continue;
        if (compareCandidates(repaired, next) > 0) next = repaired;
      }
      if (next === best) break;
      const retained = detachedCandidate(next);
      // Private exploration may cross a temporary public regression on its
      // way to a later repair. Keep that state internal: only the monotonic
      // public frontier is traced and retained as the final fallback.
      if (comparePublicCandidates(retained, publicBest) > 0) {
        trace?.({
          type: "improvement",
          stage: "greedy-cardinal-repair",
          iteration,
          before: traceCandidate(publicBest),
          after: traceCandidate(retained),
        });
        publicBest = retained;
      }
      best = retained;
      changed = true;
    } finally {
      endEpoch();
    }
  }
  return changed ? { endpoint: best, publicBest } : undefined;
}

function exactNewCandidates(
  initial: Map<string, GridPosition>,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  blocks: ReadonlyMap<string, Set<string>>,
  edges: readonly LayoutEdge[],
  nodes: ReadonlyMap<string, LayoutNode>,
  centerId: string | undefined,
): Candidate[] {
  const positions = new Map(initial);
  const newIds = new Set([...nodes.keys()].filter((id) => !current.has(id)));
  for (const component of connectionFirstComponents(newIds, positions, edges)) {
    const origins = anchorOrigins(new Set(component), positions, nodes, edges, centerId);
    const origin = origins[0] ?? packedOrigin(component, positions, nodes);
    for (const [id, position] of componentPositions(component, nodes, origin)) positions.set(id, position);
  }
  const protectedIds = new Set(newIds);
  if (centerId) protectedIds.add(centerId);
  const result = safePushRepairs(
    positions,
    protectedIds,
    residents,
    edges,
  ).map((pushed) => candidate(pushed, current, edges));
  result.push(...expansionRepairs(
    positions,
    protectedIds,
    current,
    residents,
    blocks,
    edges,
    centerId,
  ).map((expanded) => candidate(expanded, current, edges)));
  const repaired = moveCollisionBlocks(
    positions,
    protectedIds,
    current,
    residents,
    blocks,
    edges,
    centerId,
  );
  if (repaired) result.push(candidate(repaired, current, edges));
  return result;
}

function reflowCandidates(
  initial: Map<string, GridPosition>,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  blocks: ReadonlyMap<string, Set<string>>,
  edges: readonly LayoutEdge[],
  nodes: ReadonlyMap<string, LayoutNode>,
  centerId: string | undefined,
): Candidate[] {
  const nodeIds = new Set(nodes.keys());
  const origins = knownOrigins(initial, nodes, centerId);
  if (origins.length === 0) origins.push(packedOrigin([...nodeIds], initial, nodes));

  const result: Candidate[] = [];
  for (const origin of origins) {
    const positions = new Map(initial);
    const patch = componentPositions([...nodeIds], nodes, origin);
    if (hasCollisions(patch)) continue;
    for (const [id, position] of patch) positions.set(id, position);
    for (const pushed of safePushRepairs(positions, nodeIds, residents, edges)) {
      result.push(candidate(pushed, current, edges));
    }
    for (const expanded of expansionRepairs(
      positions,
      nodeIds,
      current,
      residents,
      blocks,
      edges,
      centerId,
    )) {
      result.push(candidate(expanded, current, edges));
    }
    const repaired = moveCollisionBlocks(
      positions,
      nodeIds,
      current,
      residents,
      blocks,
      edges,
      centerId,
    );
    if (repaired) result.push(candidate(repaired, current, edges));
  }
  return result;
}

interface GoldenComponent {
  ids: string[];
  relative: Map<string, GridPosition>;
  hasFixedRoom: boolean;
}

/**
 * Solve every protected edge as an exact difference constraint. A solution is
 * "golden": its connected shapes are fixed up to integral translation, so
 * disconnected shapes can be packed without weakening any cardinal exit.
 */
function goldenCandidate(
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  nodes: ReadonlyMap<string, LayoutNode>,
  centerId: string | undefined,
): Candidate | undefined {
  const ids = new Set([...current.keys(), ...nodes.keys()]);
  const adjacency = new Map<string, { to: string; delta: GridPosition }[]>();
  for (const id of ids) adjacency.set(id, []);
  for (const edge of edges) {
    const delta = protectedVector(edge);
    if (!delta || !ids.has(edge.from) || !ids.has(edge.to)) continue;
    adjacency.get(edge.from)?.push({ to: edge.to, delta });
    adjacency.get(edge.to)?.push({
      to: edge.from,
      delta: { x: -delta.x, y: -delta.y, level: -delta.level },
    });
  }

  const visited = new Set<string>();
  const components: GoldenComponent[] = [];
  for (const root of [...ids].sort()) {
    if (visited.has(root)) continue;
    const relative = new Map<string, GridPosition>([[root, { x: 0, y: 0, level: 0 }]]);
    const queue = [root];
    visited.add(root);
    while (queue.length > 0) {
      const from = queue.shift() as string;
      const fromPosition = relative.get(from) as GridPosition;
      for (const constraint of adjacency.get(from) ?? []) {
        const expected = add(fromPosition, constraint.delta);
        const known = relative.get(constraint.to);
        if (known) {
          if (!samePosition(known, expected)) return undefined;
          continue;
        }
        relative.set(constraint.to, expected);
        visited.add(constraint.to);
        queue.push(constraint.to);
      }
    }

    if (new Set([...relative.values()].map(cellKey)).size !== relative.size) return undefined;
    const componentIds = [...relative.keys()].sort();
    components.push({
      ids: componentIds,
      relative,
      hasFixedRoom: componentIds.some((id) => residents.get(id)?.movable === false),
    });
  }

  components.sort((a, b) => {
    if (a.hasFixedRoom !== b.hasFixedRoom) return a.hasFixedRoom ? -1 : 1;
    if (centerId) {
      if (a.ids.includes(centerId) !== b.ids.includes(centerId)) return a.ids.includes(centerId) ? -1 : 1;
    }
    return b.ids.length - a.ids.length || compareStrings(a.ids[0], b.ids[0]);
  });

  const placed = new Map<string, GridPosition>();
  for (const component of components) {
    const componentSet = new Set(component.ids);
    const fixedOrigins = component.ids
      .filter((id) => residents.get(id)?.movable === false && current.has(id))
      .map((id) => subtract(current.get(id) as GridPosition, component.relative.get(id) as GridPosition));
    if (fixedOrigins.length > 1) {
      const first = offsetKey(fixedOrigins[0]);
      if (fixedOrigins.some((origin) => offsetKey(origin) !== first)) return undefined;
    }

    let origins = fixedOrigins.length > 0
      ? [fixedOrigins[0]]
      : component.ids
        .filter((id) => current.has(id))
        .sort((a, b) => {
          if (a === centerId) return -1;
          if (b === centerId) return 1;
          return compareStrings(a, b);
        })
        .slice(0, 8)
        .map((id) => subtract(current.get(id) as GridPosition, component.relative.get(id) as GridPosition));
    origins = uniqueOffsets(origins);

    if (origins.length === 0) {
      const relativeBounds = bounds(component.relative.values());
      const placedBounds = bounds(placed.values());
      origins = [{
        x: relativeBounds && placedBounds ? placedBounds.maxX + ISLAND_GAP - relativeBounds.minX : 0,
        y: relativeBounds && placedBounds ? placedBounds.minY - relativeBounds.minY : 0,
        level: 0,
      }];
    }

    const occupied = occupiedCells(placed);
    const baseCandidate = candidate(new Map(placed), current, edges);
    let best: Candidate | undefined;
    for (const origin of origins) {
      const base = new Map(component.ids.map((id) => [
        id,
        add(component.relative.get(id) as GridPosition, origin),
      ]));
      const offsets = fixedOrigins.length > 0
        ? [{ x: 0, y: 0, level: 0 }]
        : uniqueOffsets([
          ...NEARBY_OFFSETS,
          ...farOffsets(base.values(), placed.values()),
        ]);
      for (const offset of offsets) {
        const placement = new Map([...base].map(([id, position]) => [id, add(position, offset)]));
        if (!fits(placement, occupied)) continue;
        best = preferTemporaryPlacement(placed, placement, current, edges, {
          base: baseCandidate,
          changedIds: componentSet,
        }, best);
      }
    }
    if (!best) return undefined;
    for (const id of component.ids) {
      const position = best.positions.get(id);
      if (position) placed.set(id, position);
    }
  }

  const result = candidate(placed, current, edges);
  const ray = candidateRayQuality(result);
  return ray.cardinalRayViolations === 0 && ray.cardinalSlack === 0
    ? result
    : undefined;
}

interface PhysicalLink {
  key: string;
  a: string;
  b: string;
  edges: LayoutEdge[];
}

function physicalLinks(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): PhysicalLink[] {
  return topologyIndex(edges).physical
    .filter((link) => positions.has(link.a) && positions.has(link.b))
    .map((link) => ({
      key: `${link.a}|${link.b}`,
      a: link.a,
      b: link.b,
      edges: link.edges,
    }));
}

interface LinkNeighbor {
  id: string;
  link: PhysicalLink;
}

interface BridgeGraph {
  links: PhysicalLink[];
  adjacency: Map<string, LinkNeighbor[]>;
  /** DFS-child endpoint, making this side the canonical strict subtree. */
  strictEndpoints: Map<string, string>;
  entered: Map<string, number>;
  subtreeEnd: Map<string, number>;
}

function linkAdjacency(
  positions: ReadonlyMap<string, GridPosition>,
  links: readonly PhysicalLink[],
): Map<string, LinkNeighbor[]> {
  const result = new Map<string, LinkNeighbor[]>([...positions.keys()].map((id) => [id, []]));
  for (const link of links) {
    result.get(link.a)?.push({ id: link.b, link });
    result.get(link.b)?.push({ id: link.a, link });
  }
  for (const neighbors of result.values()) {
    neighbors.sort((a, b) => compareStrings(a.link.key, b.link.key));
  }
  return result;
}

/** Find the unique physical links whose removal disconnects their endpoints. */
function bridgeLinks(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): BridgeGraph {
  const links = physicalLinks(positions, edges);
  const adjacency = linkAdjacency(positions, links);
  const entered = new Map<string, number>();
  const low = new Map<string, number>();
  const subtreeEnd = new Map<string, number>();
  const bridges: PhysicalLink[] = [];
  const strictEndpoints = new Map<string, string>();
  let clock = 0;

  for (const id of [...positions.keys()].sort()) {
    if (entered.has(id)) continue;
    clock += 1;
    entered.set(id, clock);
    low.set(id, clock);
    const stack: Array<{
      id: string;
      parentId?: string;
      parentLink?: PhysicalLink;
      nextNeighbor: number;
    }> = [{ id, nextNeighbor: 0 }];
    while (stack.length > 0) {
      const frame = stack[stack.length - 1];
      const neighbors = adjacency.get(frame.id) ?? [];
      if (frame.nextNeighbor < neighbors.length) {
        const neighbor = neighbors[frame.nextNeighbor++];
        if (neighbor.link.key === frame.parentLink?.key) continue;
        const known = entered.get(neighbor.id);
        if (known !== undefined) {
          low.set(frame.id, Math.min(low.get(frame.id) as number, known));
          continue;
        }
        clock += 1;
        entered.set(neighbor.id, clock);
        low.set(neighbor.id, clock);
        stack.push({
          id: neighbor.id,
          parentId: frame.id,
          parentLink: neighbor.link,
          nextNeighbor: 0,
        });
        continue;
      }
      stack.pop();
      subtreeEnd.set(frame.id, clock);
      if (!frame.parentId || !frame.parentLink) continue;
      low.set(
        frame.parentId,
        Math.min(low.get(frame.parentId) as number, low.get(frame.id) as number),
      );
      if ((low.get(frame.id) as number) > (entered.get(frame.parentId) as number)) {
        bridges.push(frame.parentLink);
        strictEndpoints.set(frame.parentLink.key, frame.id);
      }
    }
  }
  bridges.sort((a, b) => compareStrings(a.key, b.key));
  return { links: bridges, adjacency, strictEndpoints, entered, subtreeEnd };
}

function bridgeSide(
  start: string,
  blockedLink: string,
  adjacency: ReadonlyMap<string, readonly LinkNeighbor[]>,
): Set<string> {
  const result = new Set([start]);
  const queue = [start];
  let queueIndex = 0;
  while (queueIndex < queue.length) {
    const id = queue[queueIndex++];
    for (const neighbor of adjacency.get(id) ?? []) {
      if (neighbor.link.key === blockedLink || result.has(neighbor.id)) continue;
      result.add(neighbor.id);
      queue.push(neighbor.id);
    }
  }
  return result;
}

interface BridgeSlide {
  candidate: Candidate;
  edge: LayoutEdge;
  movingEndpoint: string;
  offset: GridPosition;
  moved: string[];
}

/**
 * Slide a lobe toward the other endpoint of its sole physical connection.
 * Internal lobe geometry cannot change because every room on that side moves
 * together. The first collision stops the slide so geometry is never moved
 * through an unrelated room merely to find a free cell farther away.
 */
function bridgeSlideCandidates(
  base: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  bridgeGraph: ReturnType<typeof bridgeLinks>,
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
): BridgeSlide[] {
  const { links, adjacency } = bridgeGraph;
  const result: BridgeSlide[] = [];
  const seen = new Set<string>();

  for (const link of links) {
    const a = base.positions.get(link.a);
    const b = base.positions.get(link.b);
    if (!a || !b || a.level !== b.level) continue;
    const delta = subtract(b, a);
    if ((delta.x === 0) === (delta.y === 0)) continue;
    const representative = link.edges.find((edge) => {
      const from = base.positions.get(edge.from);
      const to = base.positions.get(edge.to);
      return from && to && cardinalRayDistance(edge.direction, subtract(to, from)) !== undefined;
    });
    if (!representative) continue;
    const length = Math.abs(delta.x) + Math.abs(delta.y);
    if (length <= 1) continue;

    const attempts = [
      { movingEndpoint: link.a, stationaryEndpoint: link.b },
      { movingEndpoint: link.b, stationaryEndpoint: link.a },
    ];
    for (const attempt of attempts) {
      const moving = bridgeSide(attempt.movingEndpoint, link.key, adjacency);
      if (!movableRegion(moving, residents)) continue;
      const stationary = new Map(base.positions);
      for (const id of moving) stationary.delete(id);
      const occupied = occupiedCells(stationary);
      const movingPosition = base.positions.get(attempt.movingEndpoint) as GridPosition;
      const stationaryPosition = base.positions.get(attempt.stationaryEndpoint) as GridPosition;
      const unit = {
        x: Math.sign(stationaryPosition.x - movingPosition.x),
        y: Math.sign(stationaryPosition.y - movingPosition.y),
        level: 0,
      };
      const translationContext = rigidTranslationContext(base, moving);
      let bestForSide: BridgeSlide | undefined;
      for (let distance = 1; distance < length; distance += 1) {
        const offset = {
          x: unit.x * distance,
          y: unit.y * distance,
          level: 0,
        };
        const placement = new Map<string, GridPosition>();
        for (const id of moving) {
          const position = base.positions.get(id);
          if (position) placement.set(id, add(position, offset));
        }
        if (!fits(placement, occupied)) continue;
        const trial = new Map(stationary);
        for (const [id, position] of placement) trial.set(id, position);
        if (!acceptedPositions(trial, acceptsPositions)) continue;
        const evaluated = candidate(trial, current, edges, {
          base,
          changedIds: moving,
          translation: { offset, context: translationContext },
        });
        const slide = {
          candidate: evaluated,
          edge: representative,
          movingEndpoint: attempt.movingEndpoint,
          offset,
          moved: [...moving].sort(),
        } satisfies BridgeSlide;
        if (!bestForSide || compareCandidates(evaluated, bestForSide.candidate) > 0) {
          bestForSide = slide;
        }
      }
      if (!bestForSide) continue;
      const key = positionMapKey(bestForSide.candidate.positions);
      if (seen.has(key)) continue;
      seen.add(key);
      result.push(bestForSide);
    }
  }
  return result;
}

function bridgeLobeVacuum(
  seed: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  trace: IntegralLayoutRequest["trace"],
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
): Candidate {
  let best = seed;
  const bridgeGraph = bridgeLinks(seed.positions, edges);
  const iterationLimit = Math.max(1, best.positions.size * 2);
  for (let iteration = 0; iteration < iterationLimit; iteration += 1) {
    const slides = bridgeSlideCandidates(
      best,
      current,
      residents,
      edges,
      bridgeGraph,
      acceptsPositions,
    )
      .sort((a, b) => compareCandidates(b.candidate, a.candidate));
    const accepted = slides[0];
    if (!accepted || compareCandidates(accepted.candidate, best) <= 0) break;
    trace?.({
      type: "bridge-vacuum",
      stage: "bridge-vacuum",
      iteration,
      edge: { ...accepted.edge },
      movingEndpoint: accepted.movingEndpoint,
      offset: { ...accepted.offset },
      moved: accepted.moved,
      before: traceCandidate(best),
      after: traceCandidate(accepted.candidate),
    });
    best = accepted.candidate;
  }
  return best;
}

const CROSSING_PREFIX_FIELDS: readonly (keyof LayoutQuality)[] = [
  "levelViolations",
  "cardinalRayViolations",
  "reciprocalRayViolations",
  "routingViolations",
  "exitPortViolations",
  "reciprocalExitPortViolations",
  "roomObstructions",
];
const CROSSING_PUSH_DIRECTIONS: readonly Origin[] = [
  { x: 0, y: -1, level: 0 },
  { x: 1, y: 0, level: 0 },
  { x: 0, y: 1, level: 0 },
  { x: -1, y: 0, level: 0 },
];
const QUICK_CROSSING_WORK = 48;
const CROSSING_LOCAL_HEAL_STEPS = 4;
const QUICK_CROSSING_HEAL_CANDIDATES = 1;
const DEEP_CROSSING_HEAL_CANDIDATES = 4;
/**
 * Minimum spacing between throttled counter/telemetry trace emissions.
 * Improvement-class events always publish per occurrence.
 */
const PROGRESS_INTERVAL_MS = 30;

interface PhysicalLinkCrossing {
  key: string;
  first: PhysicalLink;
  second: PhysicalLink;
}

interface BridgeMoveSide {
  bridge: PhysicalLink;
  movingEndpoint: string;
  ids: Set<string>;
  strict: boolean;
}

interface CrossingExpansion {
  crossing: PhysicalLinkCrossing;
  side: BridgeMoveSide;
  offset: Origin;
  key: string;
}

interface CrossingTransactionCandidate {
  candidate: Candidate;
  strict: boolean;
  sideSize: number;
  key: string;
}

interface RawCrossingTransactionCandidate extends CrossingTransactionCandidate {
  prefixRegression: number;
}

interface CrossingRepairContext {
  mode: "quick" | "deep";
  current: ReadonlyMap<string, GridPosition>;
  evaluationIds: readonly string[];
  residents: ReadonlyMap<string, LayoutResident>;
  nodes: ReadonlyMap<string, LayoutNode>;
  edges: readonly LayoutEdge[];
  centerId: string | undefined;
  allowAxisCompaction: boolean;
  trace: IntegralLayoutRequest["trace"];
  control: CrossingRepairControl;
  maximumWork: number;
  stats: CrossingRepairStats;
  /** State fingerprints, not full keys; see positionsFingerprint. */
  seenStates: Set<string>;
  expandedStates: Set<string>;
  axisPolishedStates: Set<string>;
  cancelled: boolean;
  exhausted: boolean;
  improvements: number;
  /** The search's own frontier: the best transaction result it has reached. */
  best: Candidate;
  /**
   * The last layout published, or the seed, which a deep repair returns. A
   * deep repair publishes each new `best` finished, so this never ranks below
   * `best`.
   */
  published: Candidate;
  /**
   * How a published layout is finished, when set: the cheap compaction fixed
   * point for a quick repair, which hands its finished result back to the
   * planner to continue from, and a full compaction for a deep repair, which
   * keeps searching from the raw transaction and finishes only the layout it
   * publishes.
   */
  finish: ((value: Candidate) => Candidate) | undefined;
  lastProgressAt: number;
}

function crossingPairs(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): PhysicalLinkCrossing[] {
  const links = physicalLinks(positions, edges);
  const result = new Map<string, PhysicalLinkCrossing>();
  const addCrossing = (a: PhysicalLink, b: PhysicalLink): void => {
    if (a.a === b.a || a.a === b.b || a.b === b.a || a.b === b.b) return;
    const [first, second] = a.key < b.key ? [a, b] : [b, a];
    const key = `${first.key}&${second.key}`;
    result.set(key, { key, first, second });
  };
  interface CrossingHorizontal extends HorizontalSegment { link: PhysicalLink }
  interface CrossingVertical extends VerticalSegment { link: PhysicalLink }
  const horizontalLevels = new Map<number, CrossingHorizontal[]>();
  const verticalLevels = new Map<number, CrossingVertical[]>();
  const general = new Set<number>();
  for (let index = 0; index < links.length; index += 1) {
    const link = links[index];
    const from = positions.get(link.a);
    const to = positions.get(link.b);
    if (!from || !to || from.level !== to.level) continue;
    if (from.y === to.y) {
      const values = horizontalLevels.get(from.level) ?? [];
      values.push({
        link,
        minimum: Math.min(from.x, to.x),
        maximum: Math.max(from.x, to.x),
        y: from.y,
      });
      horizontalLevels.set(from.level, values);
    } else if (from.x === to.x) {
      const values = verticalLevels.get(from.level) ?? [];
      values.push({
        link,
        x: from.x,
        minimum: Math.min(from.y, to.y),
        maximum: Math.max(from.y, to.y),
      });
      verticalLevels.set(from.level, values);
    } else {
      general.add(index);
    }
  }

  for (const [level, horizontals] of horizontalLevels) {
    const verticals = verticalLevels.get(level);
    if (!verticals || horizontals.length === 0) continue;
    const yValues = [...new Set(horizontals.map((value) => value.y))].sort((a, b) => a - b);
    const yIndexes = new Map(yValues.map((value, index) => [value, index]));
    const starts = new Map<number, CrossingHorizontal[]>();
    const ends = new Map<number, CrossingHorizontal[]>();
    const queries = new Map<number, CrossingVertical[]>();
    const xValues = new Set<number>();
    for (const horizontal of horizontals) {
      const start = starts.get(horizontal.minimum) ?? [];
      start.push(horizontal);
      starts.set(horizontal.minimum, start);
      const end = ends.get(horizontal.maximum) ?? [];
      end.push(horizontal);
      ends.set(horizontal.maximum, end);
      xValues.add(horizontal.minimum);
      xValues.add(horizontal.maximum);
    }
    for (const vertical of verticals) {
      const values = queries.get(vertical.x) ?? [];
      values.push(vertical);
      queries.set(vertical.x, values);
      xValues.add(vertical.x);
    }
    const treeSize = 1 << Math.ceil(Math.log2(Math.max(1, yValues.length)));
    const activeCounts = new Int32Array(treeSize * 2);
    const activeAtY: Array<Set<PhysicalLink> | undefined> = new Array(yValues.length);
    const update = (y: number, link: PhysicalLink, add: boolean): void => {
      const yIndex = yIndexes.get(y);
      if (yIndex === undefined) return;
      let active = activeAtY[yIndex];
      if (!active) activeAtY[yIndex] = active = new Set();
      const changed = add ? !active.has(link) : active.has(link);
      if (!changed) return;
      if (add) active.add(link);
      else active.delete(link);
      const delta = add ? 1 : -1;
      for (let cursor = treeSize + yIndex; cursor > 0; cursor >>= 1) {
        activeCounts[cursor] += delta;
      }
    };
    const report = (
      queryMinimum: number,
      queryMaximum: number,
      vertical: CrossingVertical,
      node = 1,
      minimum = 0,
      maximum = treeSize,
    ): void => {
      if (queryMaximum <= minimum || maximum <= queryMinimum || activeCounts[node] === 0) return;
      if (maximum - minimum === 1) {
        for (const horizontal of activeAtY[minimum] ?? []) addCrossing(horizontal, vertical.link);
        return;
      }
      const middle = (minimum + maximum) >>> 1;
      report(queryMinimum, queryMaximum, vertical, node * 2, minimum, middle);
      report(queryMinimum, queryMaximum, vertical, node * 2 + 1, middle, maximum);
    };
    for (const x of [...xValues].sort((a, b) => a - b)) {
      for (const horizontal of ends.get(x) ?? []) update(horizontal.y, horizontal.link, false);
      for (const vertical of queries.get(x) ?? []) {
        report(
          lowerBound(yValues, vertical.minimum + 1),
          lowerBound(yValues, vertical.maximum),
          vertical,
        );
      }
      for (const horizontal of starts.get(x) ?? []) update(horizontal.y, horizontal.link, true);
    }
  }

  // Diagonal links retain the exact predicate. The common all-axis case above
  // is output-sensitive and avoids quadratic scans on long corridor chains;
  // iterating only the diagonal set keeps the remainder linear as well.
  for (const first of general) {
    for (let second = 0; second < links.length; second += 1) {
      if (second === first || (general.has(second) && second < first)) continue;
      const a = links[first];
      const b = links[second];
      if (a.a === b.a || a.a === b.b || a.b === b.a || a.b === b.b) continue;
      const aFrom = positions.get(a.a);
      const aTo = positions.get(a.b);
      const bFrom = positions.get(b.a);
      const bTo = positions.get(b.b);
      if (aFrom && aTo && bFrom && bTo && strictSegmentsIntersect(aFrom, aTo, bFrom, bTo)) {
        addCrossing(a, b);
      }
    }
  }
  return [...result.values()].sort((a, b) => compareStrings(a.key, b.key));
}

function bridgeMoveSides(
  graph: BridgeGraph,
  centerId: string | undefined,
  crossings?: readonly PhysicalLinkCrossing[],
): BridgeMoveSide[] {
  const result: BridgeMoveSide[] = [];
  for (const bridge of graph.links) {
    const strictEndpoint = graph.strictEndpoints.get(bridge.key);
    if (!strictEndpoint) continue;
    if (crossings) {
      const start = graph.entered.get(strictEndpoint) as number;
      const end = graph.subtreeEnd.get(strictEndpoint) as number;
      const inStrictSide = (id: string): boolean => {
        const entered = graph.entered.get(id);
        return entered !== undefined && entered >= start && entered <= end;
      };
      const relevant = crossings.some((crossing) => {
        const endpoints = [crossing.first.a, crossing.first.b, crossing.second.a, crossing.second.b];
        const included = endpoints.filter(inStrictSide).length;
        return included > 0 && included < endpoints.length;
      });
      if (!relevant) continue;
    }
    const otherEndpoint = strictEndpoint === bridge.a ? bridge.b : bridge.a;
    result.push({
      bridge,
      movingEndpoint: strictEndpoint,
      ids: bridgeSide(strictEndpoint, bridge.key, graph.adjacency),
      strict: true,
    });
    result.push({
      bridge,
      movingEndpoint: otherEndpoint,
      ids: bridgeSide(otherEndpoint, bridge.key, graph.adjacency),
      strict: false,
    });
  }
  result.sort((a, b) =>
    Number(a.ids.has(centerId ?? "")) - Number(b.ids.has(centerId ?? "")) ||
    a.ids.size - b.ids.size || Number(b.strict) - Number(a.strict) ||
    compareStrings(a.bridge.key, b.bridge.key) ||
    compareStrings(a.movingEndpoint, b.movingEndpoint));
  return result;
}

function crossingTouchesSide(crossing: PhysicalLinkCrossing, ids: ReadonlySet<string>): boolean {
  const endpoints = [crossing.first.a, crossing.first.b, crossing.second.a, crossing.second.b];
  const included = endpoints.filter((id) => ids.has(id)).length;
  return included > 0 && included < endpoints.length;
}

/**
 * The exact pair predicate realized by crossingPairs' sweep, including its
 * degenerate-segment behavior: a zero-length link (two collided rooms) enters
 * the sweep as a one-point horizontal whose removal precedes its insertion,
 * so it stays active and pairs with every true vertical strictly to its
 * right whose interval strictly contains its y. Intermediate push states can
 * collide rooms, so signature maintenance must reproduce that behavior
 * bit-for-bit rather than idealize it away.
 */
function sweepEquivalentCross(
  aFrom: GridPosition,
  aTo: GridPosition,
  bFrom: GridPosition,
  bTo: GridPosition,
): boolean {
  if (aFrom.level !== aTo.level || bFrom.level !== bTo.level ||
    aFrom.level !== bFrom.level) return false;
  const aPoint = aFrom.x === aTo.x && aFrom.y === aTo.y;
  const bPoint = bFrom.x === bTo.x && bFrom.y === bTo.y;
  if (aPoint || bPoint) {
    if (aPoint && bPoint) return false;
    const point = aPoint ? aFrom : bFrom;
    const verticalFrom = aPoint ? bFrom : aFrom;
    const verticalTo = aPoint ? bTo : aTo;
    if (verticalFrom.x !== verticalTo.x || verticalFrom.y === verticalTo.y) return false;
    return verticalFrom.x > point.x &&
      Math.min(verticalFrom.y, verticalTo.y) < point.y &&
      point.y < Math.max(verticalFrom.y, verticalTo.y);
  }
  return strictSegmentsIntersect(aFrom, aTo, bFrom, bTo);
}

/**
 * Advance a transaction's restricted crossing set across one push step.
 * Pairs whose links have no endpoint in the moved closure kept their exact
 * geometry and carry over; every pair involving a moved link is recomputed
 * with the sweep-equivalent predicate. The signature's set semantics survive
 * intact: the composed entries are exactly the full restricted
 * recomputation, entry order aside, and only membership and length are ever
 * consulted.
 */
function advanceRestrictedCrossings(
  previous: readonly PhysicalLinkCrossing[],
  positions: ReadonlyMap<string, GridPosition>,
  links: readonly PhysicalLink[],
  edges: readonly LayoutEdge[],
  signatureIds: ReadonlySet<string>,
  movedIds: ReadonlySet<string>,
): PhysicalLinkCrossing[] {
  const touches = (link: PhysicalLink): boolean =>
    movedIds.has(link.a) || movedIds.has(link.b);
  const movedLinks = links.filter(touches);
  // Both branches are exact; the threshold only bounds the pairwise cost
  // when a closure drags most of the map along.
  if (movedLinks.length * links.length > 16_384) {
    return crossingPairs(positions, edges)
      .filter((crossing) => crossingTouchesSide(crossing, signatureIds));
  }
  const result: PhysicalLinkCrossing[] = [];
  for (const entry of previous) {
    if (touches(entry.first) || touches(entry.second)) continue;
    result.push(entry);
  }
  for (const moved of movedLinks) {
    const movedFrom = positions.get(moved.a);
    const movedTo = positions.get(moved.b);
    if (!movedFrom || !movedTo) continue;
    for (const other of links) {
      if (other === moved) continue;
      // A pair inside the moved set is enumerated once, from its lower key.
      if (touches(other) && other.key < moved.key) continue;
      if (moved.a === other.a || moved.a === other.b ||
        moved.b === other.a || moved.b === other.b) continue;
      const otherFrom = positions.get(other.a);
      const otherTo = positions.get(other.b);
      if (!otherFrom || !otherTo) continue;
      if (!sweepEquivalentCross(movedFrom, movedTo, otherFrom, otherTo)) continue;
      const [first, second] = moved.key < other.key ? [moved, other] : [other, moved];
      const crossing = { key: `${first.key}&${second.key}`, first, second };
      if (!crossingTouchesSide(crossing, signatureIds)) continue;
      result.push(crossing);
    }
  }
  return result;
}

function strictSignatureReduction(before: readonly string[], after: readonly string[]): boolean {
  if (after.length >= before.length) return false;
  const known = new Set(before);
  return after.every((key) => known.has(key));
}

function crossingPrefixRestored(after: LayoutQuality, before: LayoutQuality): boolean {
  return CROSSING_PREFIX_FIELDS.every((field) => (after[field] ?? 0) <= (before[field] ?? 0));
}

/**
 * A transaction is complete when it removes crossings and the quality order
 * ranks its layout above its base, which lets it trade a directional or
 * routing violation for enough crossings. Removing crossings is the repair's
 * own progress: it bounds how deeply accepted transactions nest.
 */
function completedCrossingImprovement(candidateValue: Candidate, base: Candidate): boolean {
  if (candidateCollisions(candidateValue) !== 0) return false;
  const after = candidateQuality(candidateValue);
  const before = candidateQuality(base);
  return after.linkCrossings < before.linkCrossings && compareLayoutQuality(after, before) > 0;
}

function crossingStats(context: CrossingRepairContext): CrossingRepairStats {
  return { ...context.stats };
}

/**
 * The layout a crossing repair reports as its best: for a deep repair, the
 * last layout it published, which it returns; for a quick one, its search
 * frontier.
 */
function crossingRepairBest(context: CrossingRepairContext): Candidate {
  return context.mode === "deep" ? context.published : context.best;
}

function publishCrossingProgress(
  context: CrossingRepairContext,
  status: "progress" | "complete",
  force = false,
): void {
  if (context.mode === "quick" && (!force || status !== "complete")) return;
  const now = performance.now();
  if (!force && now - context.lastProgressAt < PROGRESS_INTERVAL_MS) return;
  context.lastProgressAt = now;
  const stats = crossingStats(context);
  const bestQuality = { ...candidateQuality(crossingRepairBest(context)) };
  context.trace?.({
    type: "crossing-progress",
    stage: "crossing-repair",
    mode: context.mode,
    status,
    ...stats,
    bestQuality,
  });
  context.control.onProgress?.({
    kind: status,
    ...stats,
    bestQuality,
  });
}

function crossingExpansionAllowed(context: CrossingRepairContext): boolean {
  if (context.control.shouldCancel?.()) {
    context.cancelled = true;
    return false;
  }
  if (context.stats.macrosConsidered >= context.maximumWork) {
    context.exhausted = true;
    return false;
  }
  context.stats.macrosConsidered += 1;
  publishCrossingProgress(context, "progress");
  return true;
}

function recordCrossingState(
  context: CrossingRepairContext,
  positions: ReadonlyMap<string, GridPosition>,
): boolean {
  const key = positionsFingerprint(positions);
  if (context.seenStates.has(key)) return false;
  context.seenStates.add(key);
  context.stats.visitedStates += 1;
  return true;
}

function crossingExpansionOrder(
  base: Candidate,
  context: CrossingRepairContext,
  baseCrossings?: readonly PhysicalLinkCrossing[],
): CrossingExpansion[] {
  const crossings = baseCrossings ?? crossingPairs(base.positions, context.edges);
  const sides = bridgeMoveSides(
    bridgeLinks(base.positions, context.edges),
    context.centerId,
    crossings,
  );
  const result: CrossingExpansion[] = [];
  const seen = new Set<string>();
  for (const crossing of crossings) {
    context.stats.crossingsConsidered += 1;
    for (const side of sides) {
      if (!crossingTouchesSide(crossing, side.ids) ||
        !movableRegion(side.ids, context.residents)) continue;
      const sideKey = [...side.ids].sort().join(",");
      for (let direction = 0; direction < CROSSING_PUSH_DIRECTIONS.length; direction += 1) {
        const offset = CROSSING_PUSH_DIRECTIONS[direction];
        const key = `${sideKey}|${offset.x},${offset.y}`;
        if (seen.has(key)) continue;
        seen.add(key);
        result.push({ crossing, side, offset, key });
      }
    }
  }
  result.sort((a, b) =>
    Number(a.side.ids.has(context.centerId ?? "")) - Number(b.side.ids.has(context.centerId ?? "")) ||
    a.side.ids.size - b.side.ids.size || Number(b.side.strict) - Number(a.side.strict) ||
    compareStrings(a.crossing.key, b.crossing.key) ||
    compareStrings(a.side.bridge.key, b.side.bridge.key) || compareStrings(a.key, b.key));
  return result;
}

function axisSpan(
  positions: ReadonlyMap<string, GridPosition>,
  ids: Iterable<string>,
  axis: PlanarAxis,
): number | undefined {
  // Extrema are accumulated in a loop: spreading a per-room array into
  // Math.min/Math.max exceeds the engine's argument limit on very large areas.
  let minimum: number | undefined;
  let maximum: number | undefined;
  for (const id of ids) {
    const value = positions.get(id)?.[axis];
    if (value === undefined) continue;
    if (minimum === undefined || value < minimum) minimum = value;
    if (maximum === undefined || value > maximum) maximum = value;
  }
  return minimum === undefined || maximum === undefined ? undefined : maximum - minimum + 1;
}

function crossingPassageLimit(
  positions: ReadonlyMap<string, GridPosition>,
  expansion: CrossingExpansion,
): number {
  const axis = expansion.offset.x === 0 ? "y" : "x";
  const movingSpan = axisSpan(positions, expansion.side.ids, axis);
  const crossingSpan = axisSpan(positions, [
    expansion.crossing.first.a,
    expansion.crossing.first.b,
    expansion.crossing.second.a,
    expansion.crossing.second.b,
  ], axis);
  if (movingSpan === undefined || crossingSpan === undefined) return 1;
  // Sparse layouts may need a long lobe passage; the shared macro budget, not
  // an arbitrary three-cell cutoff, is the authoritative latency bound.
  return Math.max(2, movingSpan + crossingSpan + CROSSING_LOCAL_HEAL_STEPS);
}

function applyCrossingPush(
  positions: Map<string, GridPosition>,
  roots: ReadonlySet<string>,
  context: CrossingRepairContext,
  offset: Origin,
): Set<string> | undefined {
  if (!crossingExpansionAllowed(context)) return undefined;
  const closure = safePushClosure(
    positions,
    roots,
    new Set<string>(),
    context.residents,
    context.edges,
    offset,
  );
  if (!closure) return undefined;
  for (const id of closure) {
    const position = positions.get(id);
    if (position) positions.set(id, add(position, offset));
  }
  context.stats.pushClosures += 1;
  return closure;
}

function crossingCandidateAt(
  positions: ReadonlyMap<string, GridPosition>,
  base: Candidate,
  context: CrossingRepairContext,
): Candidate | undefined {
  const changedIds = changedPositionIds(base.positions, positions);
  if (changedIds.size === 0) return undefined;
  return candidate(new Map(positions), context.current, context.edges, { base, changedIds });
}

function polishCrossingCandidate(
  raw: Candidate,
  context: CrossingRepairContext,
): Candidate {
  let working = raw;
  const cardinal = greedyCardinalRepair(
    working,
    context.current,
    context.residents,
    context.edges,
    context.nodes,
    context.centerId,
    context.evaluationIds,
    undefined,
    context.control.acceptsPositions,
  );
  if (cardinal) working = cardinal.endpoint;
  if (context.control.shouldCancel?.()) {
    context.cancelled = true;
    return working;
  }
  const unobstructed = greedyObstructionRepair(
    working,
    context.current,
    context.residents,
    context.edges,
    undefined,
    context.control.acceptsPositions,
  );
  if (unobstructed) working = unobstructed;
  working = bridgeLobeVacuum(
    working,
    context.current,
    context.residents,
    context.edges,
    undefined,
    context.control.acceptsPositions,
  );
  working = vacuumLayout(
    working,
    context.current,
    context.residents,
    context.edges,
    undefined,
    context.control.acceptsPositions,
    context.control.shouldCancel,
  );
  if (context.control.shouldCancel?.()) context.cancelled = true;
  return working;
}

function axisPolishCrossingCandidate(
  working: Candidate,
  context: CrossingRepairContext,
): Candidate {
  if (context.mode !== "deep" || !context.allowAxisCompaction || context.control.shouldCancel?.()) {
    if (context.control.shouldCancel?.()) context.cancelled = true;
    return working;
  }
  const compacted = context.control.axisGroupCompaction === false
    ? working
    : axisGroupCompaction(
      working,
      context.current,
      context.residents,
      context.edges,
      context.centerId,
      undefined,
      context.control.acceptsPositions,
      context.control.shouldCancel,
    );
  let vacuumed = vacuumLayout(
    compacted,
    context.current,
    context.residents,
    context.edges,
    undefined,
    context.control.acceptsPositions,
    context.control.shouldCancel,
  );
  if (!context.control.shouldCancel?.()) {
    vacuumed = evenCardinalSeries(
      vacuumed,
      context.current,
      context.residents,
      context.edges,
      context.centerId,
      undefined,
      context.control.acceptsPositions,
      context.control.shouldCancel,
    );
  }
  if (context.control.shouldCancel?.()) context.cancelled = true;
  return vacuumed;
}

function crossingPrefixRegression(after: LayoutQuality, before: LayoutQuality): number {
  return CROSSING_PREFIX_FIELDS.reduce(
    (total, field) => total + Math.max(0, (after[field] ?? 0) - (before[field] ?? 0)),
    0,
  );
}

function addRawCrossingTransactionCandidate(
  result: RawCrossingTransactionCandidate[],
  known: Set<string>,
  evaluated: Candidate | undefined,
  base: Candidate,
  context: CrossingRepairContext,
  strict: boolean,
  sideSize: number,
): boolean {
  if (!evaluated || !acceptedPositions(evaluated.positions, context.control.acceptsPositions) ||
    candidateCollisions(evaluated) !== 0) return false;
  const quality = candidateQuality(evaluated);
  const baseQuality = candidateQuality(base);
  if (quality.linkCrossings >= baseQuality.linkCrossings) return false;
  const key = positionMapKey(evaluated.positions);
  if (known.has(key)) return false;
  known.add(key);
  const candidateValue = detachedCandidate(evaluated);
  const retained: RawCrossingTransactionCandidate = {
    candidate: candidateValue,
    strict,
    sideSize,
    key,
    prefixRegression: crossingPrefixRegression(quality, baseQuality),
  };
  result.push(retained);
  publishAdmittedRawCrossingImprovement(retained.candidate, base, context);
  return quality.linkCrossings === 0;
}

function publishAdmittedRawCrossingImprovement(
  value: Candidate,
  base: Candidate,
  context: CrossingRepairContext,
): void {
  if (context.mode !== "deep" ||
    !acceptedPositions(value.positions, context.control.acceptsPositions) ||
    !completedCrossingImprovement(value, base) ||
    compareLayoutQuality(candidateQuality(value), candidateQuality(context.best)) <= 0) return;
  for (const [id, resident] of context.residents) {
    if (resident.movable !== false) continue;
    const expected = context.current.get(id);
    const actual = value.positions.get(id);
    if (!expected || !actual || !samePosition(actual, expected)) return;
  }
  emitCrossingImprovement(context, value);
}

function rawCrossingCandidateOrder(
  a: RawCrossingTransactionCandidate,
  b: RawCrossingTransactionCandidate,
): number {
  return a.prefixRegression - b.prefixRegression ||
    candidateLinkCrossings(a.candidate) - candidateLinkCrossings(b.candidate) ||
    Number(b.strict) - Number(a.strict) || a.sideSize - b.sideSize ||
    compareLayoutQuality(candidateQuality(b.candidate), candidateQuality(a.candidate)) ||
    compareStrings(a.key, b.key);
}

function selectRawCrossingCandidates(
  raw: readonly RawCrossingTransactionCandidate[],
  maximum: number,
): RawCrossingTransactionCandidate[] {
  if (maximum <= 0) return [];
  const selected: RawCrossingTransactionCandidate[] = [];
  const selectedKeys = new Set<string>();
  const sideSizes = new Set<number>();
  // Preserve at least one distinct bridge-lobe scale before filling from the
  // overall ranking. This keeps a larger parent lobe available for the strict
  // subtree continuation that can heal its one-room leaf.
  for (const value of raw) {
    if (sideSizes.has(value.sideSize)) continue;
    selected.push(value);
    selectedKeys.add(value.key);
    sideSizes.add(value.sideSize);
    if (selected.length >= maximum) return selected;
  }
  for (const value of raw) {
    if (selectedKeys.has(value.key)) continue;
    selected.push(value);
    selectedKeys.add(value.key);
    if (selected.length >= maximum) break;
  }
  return selected;
}

interface RawCrossingFrontier {
  bestBySideSize: Map<number, RawCrossingTransactionCandidate>;
  globalTop: RawCrossingTransactionCandidate[];
  admittedKeys: Set<string>;
  selectedKeys: Set<string>;
}

function rawCrossingFrontier(): RawCrossingFrontier {
  return {
    bestBySideSize: new Map(),
    globalTop: [],
    admittedKeys: new Set(),
    selectedKeys: new Set(),
  };
}

function admitRawCrossingFrontier(
  frontier: RawCrossingFrontier,
  value: RawCrossingTransactionCandidate,
  maximum: number,
): RawCrossingTransactionCandidate[] {
  if (frontier.admittedKeys.has(value.key)) return [];
  frontier.admittedKeys.add(value.key);
  const sideBest = frontier.bestBySideSize.get(value.sideSize);
  if (!sideBest || rawCrossingCandidateOrder(value, sideBest) < 0) {
    frontier.bestBySideSize.set(value.sideSize, value);
  }
  if (!frontier.globalTop.some((known) => known.key === value.key)) {
    frontier.globalTop.push(value);
    frontier.globalTop.sort(rawCrossingCandidateOrder);
    if (frontier.globalTop.length > maximum) frontier.globalTop.length = maximum;
  }
  const current = [...frontier.bestBySideSize.values()].sort(rawCrossingCandidateOrder).slice(0, maximum);
  const currentKeys = new Set(current.map((candidate) => candidate.key));
  for (const candidate of frontier.globalTop) {
    if (current.length >= maximum) break;
    if (currentKeys.has(candidate.key)) continue;
    current.push(candidate);
    currentKeys.add(candidate.key);
  }
  const entered: RawCrossingTransactionCandidate[] = [];
  for (const candidate of current) {
    if (frontier.selectedKeys.has(candidate.key)) continue;
    frontier.selectedKeys.add(candidate.key);
    entered.push(candidate);
  }
  return entered;
}

function finalizeCrossingTransactionCandidates(
  raw: RawCrossingTransactionCandidate[],
  base: Candidate,
  context: CrossingRepairContext,
  depth: number,
  preserveSelectionHistory = false,
): CrossingTransactionCandidate[] {
  if (raw.length === 0 || context.cancelled) return [];
  raw.sort(rawCrossingCandidateOrder);
  const maximumHeals = context.mode === "quick"
    ? QUICK_CROSSING_HEAL_CANDIDATES
    : DEEP_CROSSING_HEAL_CANDIDATES;
  const result: CrossingTransactionCandidate[] = [];
  const known = new Set<string>();
  const selected = preserveSelectionHistory ? raw : selectRawCrossingCandidates(raw, maximumHeals);
  for (const value of selected) {
    if (context.control.shouldCancel?.()) {
      context.cancelled = true;
      break;
    }
    // A raw transaction is already complete when the quality order ranks it
    // above its base. Preserve it before optional compaction: a later polish
    // can legitimately reintroduce the crossing it was meant to heal.
    const acceptedBeforeValue = result.length;
    addCrossingTransactionCandidate(
      result,
      known,
      value.candidate,
      base,
      context,
      value.strict,
      value.sideSize,
    );
    const evaluated = polishCrossingCandidate(value.candidate, context);
    recordCrossingState(context, evaluated.positions);
    addCrossingTransactionCandidate(
      result,
      known,
      evaluated,
      base,
      context,
      value.strict,
      value.sideSize,
    );
    if (result.length === acceptedBeforeValue && context.mode === "deep" &&
      value.sideSize > 1 && !context.cancelled && !context.exhausted) {
      const evaluatedQuality = candidateQuality(evaluated);
      const baseQuality = candidateQuality(base);
      if (crossingPrefixRestored(evaluatedQuality, baseQuality) &&
        evaluatedQuality.linkCrossings <= baseQuality.linkCrossings) {
        for (const nested of completeStrictSubtreeTransactions(
          evaluated,
          base,
          context,
          depth + 1,
          value.sideSize,
        )) {
          if (known.has(nested.key)) continue;
          known.add(nested.key);
          result.push(nested);
        }
      }
    }
    if (context.cancelled) break;
  }
  // Axis compaction is the most expensive local heal. Apply it only to an
  // already publishable provisional winner rather than to every raw macro.
  if (result.length > 0 && context.mode === "deep" && context.allowAxisCompaction && !context.cancelled) {
    result.sort((a, b) =>
      compareLayoutQuality(candidateQuality(b.candidate), candidateQuality(a.candidate)) ||
      compareStrings(a.key, b.key));
    const provisional = result[0];
    const provisionalState = candidateFingerprint(provisional.candidate);
    if (compareLayoutQuality(candidateQuality(provisional.candidate), candidateQuality(context.best)) >= 0 &&
      !context.axisPolishedStates.has(provisionalState)) {
      context.axisPolishedStates.add(provisionalState);
      const evaluated = axisPolishCrossingCandidate(provisional.candidate, context);
      recordCrossingState(context, evaluated.positions);
      addCrossingTransactionCandidate(
        result,
        known,
        evaluated,
        base,
        context,
        provisional.strict,
        provisional.sideSize,
      );
    }
  }
  return result;
}

function addCrossingTransactionCandidate(
  result: CrossingTransactionCandidate[],
  known: Set<string>,
  evaluated: Candidate | undefined,
  base: Candidate,
  context: CrossingRepairContext,
  strict: boolean,
  sideSize: number,
): boolean {
  if (!evaluated || !acceptedPositions(evaluated.positions, context.control.acceptsPositions) ||
    !completedCrossingImprovement(evaluated, base)) return false;
  const key = positionMapKey(evaluated.positions);
  if (known.has(key)) return false;
  known.add(key);
  const retained = detachedCandidate(evaluated);
  result.push({ candidate: retained, strict, sideSize, key });
  return candidateLinkCrossings(retained) === 0;
}

function nestedCrossingHealing(
  base: Candidate,
  positions: ReadonlyMap<string, GridPosition>,
  activeIds: ReadonlySet<string>,
  parentSideSize: number,
  context: CrossingRepairContext,
  depth: number,
  result: RawCrossingTransactionCandidate[],
  known: Set<string>,
): void {
  if (context.mode !== "deep" || context.cancelled || context.exhausted || parentSideSize <= 1) return;
  context.stats.maxDepth = Math.max(context.stats.maxDepth, depth);
  const graph = bridgeLinks(positions, context.edges);
  const crossings = crossingPairs(positions, context.edges);
  const sides = bridgeMoveSides(graph, context.centerId, crossings).filter((side) =>
    side.ids.size < parentSideSize &&
    [...side.ids].every((id) => activeIds.has(id)) && movableRegion(side.ids, context.residents)
  );
  const seenPrograms = new Set<string>();
  for (const side of sides) {
    const sideKey = [...side.ids].sort().join(",");
    for (const offset of CROSSING_PUSH_DIRECTIONS) {
      const programKey = `${sideKey}|${offset.x},${offset.y}`;
      if (seenPrograms.has(programKey)) continue;
      seenPrograms.add(programKey);
      const working = new Map(positions);
      let roots = new Set(side.ids);
      for (let step = 0; step < CROSSING_LOCAL_HEAL_STEPS; step += 1) {
        const closure = applyCrossingPush(working, roots, context, offset);
        if (!closure || context.cancelled || context.exhausted) break;
        // Strict descent is the termination proof for nested prefix healing.
        // Pulling a room from outside the active parent lobe invalidates it.
        if ([...closure].some((id) => !activeIds.has(id))) break;
        if (!recordCrossingState(context, working)) break;
        const raw = crossingCandidateAt(working, base, context);
        addRawCrossingTransactionCandidate(
          result,
          known,
          raw,
          base,
          context,
          side.strict,
          side.ids.size,
        );
        nestedCrossingHealing(
          base,
          working,
          activeIds,
          side.ids.size,
          context,
          depth + 1,
          result,
          known,
        );
        roots = closure;
      }
      if (context.cancelled || context.exhausted) return;
    }
  }
}

function runCrossingTransaction(
  base: Candidate,
  expansion: CrossingExpansion,
  context: CrossingRepairContext,
  depth: number,
  baseCrossings?: readonly PhysicalLinkCrossing[],
  baseLinks?: readonly PhysicalLink[],
): RawCrossingTransactionCandidate[] {
  const result: RawCrossingTransactionCandidate[] = [];
  const known = new Set<string>();
  const working = new Map(base.positions);
  const signatureIds = new Set(expansion.side.ids);
  // Link membership depends only on ids, so the base link list serves every
  // step of the transaction; entries then advance per push closure instead
  // of re-sweeping the whole map.
  const links = baseLinks ?? physicalLinks(base.positions, context.edges);
  let entries = (baseCrossings ?? crossingPairs(base.positions, context.edges))
    .filter((crossing) => crossingTouchesSide(crossing, signatureIds));
  const initialSignature = entries.map((crossing) => crossing.key);
  if (!initialSignature.includes(expansion.crossing.key)) return result;
  let activeIds = new Set(expansion.side.ids);
  let roots = new Set(expansion.side.ids);
  let reductionStep: number | undefined;
  let nestedSeed: { positions: Map<string, GridPosition>; activeIds: Set<string> } | undefined;
  const passageLimit = crossingPassageLimit(working, expansion);
  for (let step = 0; step < passageLimit; step += 1) {
    const closure = applyCrossingPush(working, roots, context, expansion.offset);
    if (!closure || context.cancelled || context.exhausted) break;
    for (const id of closure) activeIds.add(id);
    if (!recordCrossingState(context, working)) break;
    entries = advanceRestrictedCrossings(
      entries,
      working,
      links,
      context.edges,
      signatureIds,
      closure,
    );
    const nextSignature = entries.map((crossing) => crossing.key);
    if (strictSignatureReduction(initialSignature, nextSignature)) {
      reductionStep ??= step;
      nestedSeed ??= { positions: new Map(working), activeIds: new Set(activeIds) };
      const raw = crossingCandidateAt(working, base, context);
      if (addRawCrossingTransactionCandidate(
        result,
        known,
        raw,
        base,
        context,
        expansion.side.strict,
        expansion.side.ids.size,
      )) break;
    }
    if (reductionStep !== undefined && step - reductionStep >= CROSSING_LOCAL_HEAL_STEPS) break;
    roots = closure;
  }
  if (nestedSeed) {
    nestedCrossingHealing(
      base,
      nestedSeed.positions,
      nestedSeed.activeIds,
      expansion.side.ids.size,
      context,
      depth + 1,
      result,
      known,
    );
  }
  return result;
}

function completeStrictSubtreeTransactions(
  seed: Candidate,
  base: Candidate,
  context: CrossingRepairContext,
  depth: number,
  parentSideSize: number,
): CrossingTransactionCandidate[] {
  if (context.mode !== "deep" || context.cancelled || context.exhausted || parentSideSize <= 1) {
    return [];
  }
  const expansionKey =
    `subtree:${parentSideSize}:${candidateFingerprint(seed)}`;
  if (context.expandedStates.has(expansionKey)) return [];
  context.expandedStates.add(expansionKey);
  context.stats.maxDepth = Math.max(context.stats.maxDepth, depth);
  const frontier = rawCrossingFrontier();
  const seedCrossings = crossingPairs(seed.positions, context.edges);
  const seedLinks = physicalLinks(seed.positions, context.edges);
  for (const expansion of crossingExpansionOrder(seed, context, seedCrossings)) {
    // Strictly smaller bridge components are the recursion/termination proof;
    // DFS-child strictness is only a deterministic tie-break.
    if (expansion.side.ids.size >= parentSideSize) continue;
    for (
      const value of runCrossingTransaction(seed, expansion, context, depth, seedCrossings, seedLinks)
    ) {
      for (const entered of admitRawCrossingFrontier(
        frontier,
        value,
        DEEP_CROSSING_HEAL_CANDIDATES,
      )) {
        for (const completed of finalizeCrossingTransactionCandidates(
          [entered],
          base,
          context,
          depth,
          true,
        )) {
          exploreCompletedCrossingCandidate(completed, context, depth);
        }
      }
    }
    if (context.cancelled || context.exhausted) break;
  }
  return [];
}

function completeCrossingTransactions(
  base: Candidate,
  context: CrossingRepairContext,
  depth: number,
): CrossingTransactionCandidate[] {
  context.stats.maxDepth = Math.max(context.stats.maxDepth, depth);
  const raw: RawCrossingTransactionCandidate[] = [];
  const known = new Set<string>();
  const frontier = context.mode === "deep" ? rawCrossingFrontier() : undefined;
  const baseCrossings = crossingPairs(base.positions, context.edges);
  const baseLinks = physicalLinks(base.positions, context.edges);
  for (const expansion of crossingExpansionOrder(base, context, baseCrossings)) {
    const transaction = runCrossingTransaction(base, expansion, context, depth, baseCrossings, baseLinks);
    for (const value of transaction) {
      if (context.mode === "deep" && frontier) {
        for (const entered of admitRawCrossingFrontier(
          frontier,
          value,
          DEEP_CROSSING_HEAL_CANDIDATES,
        )) {
          for (const completed of finalizeCrossingTransactionCandidates(
            [entered],
            base,
            context,
            depth,
            true,
          )) {
            exploreCompletedCrossingCandidate(completed, context, depth);
          }
        }
      } else {
        if (known.has(value.key)) continue;
        known.add(value.key);
        raw.push(value);
      }
    }
    if (candidateLinkCrossings(context.best) === 0) break;
    if (context.cancelled || context.exhausted) break;
  }
  if (context.mode === "deep") return [];
  return finalizeCrossingTransactionCandidates(raw, base, context, depth);
}

function exploreCompletedCrossingCandidate(
  value: CrossingTransactionCandidate,
  context: CrossingRepairContext,
  depth: number,
): void {
  if (compareLayoutQuality(candidateQuality(value.candidate), candidateQuality(context.best)) > 0) {
    emitCrossingImprovement(context, value.candidate);
  }
  if (candidateLinkCrossings(context.best) === 0 || context.cancelled || context.exhausted) return;
  deepCrossingRepair(value.candidate, context, depth + 1);
}

function emitCrossingImprovement(
  context: CrossingRepairContext,
  after: Candidate,
): void {
  if (!acceptedPositions(after.positions, context.control.acceptsPositions) ||
    candidateCollisions(after) !== 0) return;
  // Progressive geometry crosses a public boundary immediately. Reconcile the
  // complete tuple before comparing it with the last published global best:
  // a candidate may be a strict improvement over its local transaction seed
  // while still regressing the area-wide stream, and incremental score drift
  // must never make either decision for a durable checkpoint.
  const afterQuality = refreshCandidateQuality(after);
  const before = context.best;
  if (compareLayoutQuality(afterQuality, candidateQuality(before)) <= 0) return;
  context.best = after;
  // The search goes on from the raw transaction; the layout a deep repair
  // publishes is that transaction compacted. A quick repair finishes its
  // result before it gets here.
  const published = context.mode === "deep" && context.finish ? context.finish(after) : after;
  const publishedQuality = published === after ? afterQuality : refreshCandidateQuality(published);
  const previous = context.published;
  if (compareLayoutQuality(publishedQuality, candidateQuality(previous)) <= 0) return;
  context.published = published;
  context.improvements += 1;
  const stats = crossingStats(context);
  const traced = traceCandidate(published);
  context.trace?.({
    type: "crossing-repair",
    stage: "crossing-repair",
    mode: context.mode,
    iteration: context.improvements,
    ...stats,
    before: traceCandidate(context.mode === "deep" ? previous : before),
    after: traced,
  });
  context.control.onProgress?.({
    kind: "improvement",
    ...stats,
    bestQuality: { ...publishedQuality },
    candidate: traced,
  });
}

function quickCrossingRepair(seed: Candidate, context: CrossingRepairContext): Candidate {
  if (compareLayoutQuality(candidateQuality(seed), candidateQuality(context.best)) > 0) {
    const seedQuality = refreshCandidateQuality(seed);
    if (compareLayoutQuality(seedQuality, candidateQuality(context.best)) > 0) context.best = seed;
  }
  if (candidateLinkCrossings(seed) === 0 || context.cancelled || context.exhausted) return seed;
  const completed = completeCrossingTransactions(seed, context, 1);
  let selected = seed;
  for (const value of completed) {
    const comparison = compareLayoutQuality(candidateQuality(value.candidate), candidateQuality(selected));
    if (comparison > 0 || (comparison === 0 && value.key < positionMapKey(selected.positions))) {
      selected = value.candidate;
    }
  }
  if (selected === seed) return seed;
  // The planner goes on from what this returns, so the finished layout both
  // replaces the transaction and is the one published.
  const finished = context.finish ? context.finish(selected) : selected;
  emitCrossingImprovement(context, finished);
  return finished;
}

function deepCrossingRepair(seed: Candidate, context: CrossingRepairContext, depth: number): void {
  if (context.cancelled || context.exhausted || candidateLinkCrossings(context.best) === 0) return;
  const stateKey = candidateFingerprint(seed);
  if (context.expandedStates.has(stateKey)) return;
  context.expandedStates.add(stateKey);
  context.stats.maxDepth = Math.max(context.stats.maxDepth, depth);
  const completed = completeCrossingTransactions(seed, context, depth);
  completed.sort((a, b) => a.sideSize - b.sideSize || Number(b.strict) - Number(a.strict) ||
    compareLayoutQuality(candidateQuality(b.candidate), candidateQuality(a.candidate)) ||
    compareStrings(a.key, b.key));
  for (const value of completed) {
    if (compareLayoutQuality(candidateQuality(value.candidate), candidateQuality(context.best)) > 0) {
      emitCrossingImprovement(context, value.candidate);
    }
    if (candidateLinkCrossings(context.best) === 0 || context.cancelled || context.exhausted) return;
    deepCrossingRepair(value.candidate, context, depth + 1);
  }
}

function crossingRepairContext(
  mode: CrossingRepairContext["mode"],
  seed: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  nodes: ReadonlyMap<string, LayoutNode>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
  allowAxisCompaction: boolean,
  trace: IntegralLayoutRequest["trace"],
  control: CrossingRepairControl,
  defaultMaximumWork: number,
  finish?: (value: Candidate) => Candidate,
): CrossingRepairContext {
  const requestedWork = control.maximumWork ?? defaultMaximumWork;
  const maximumWork = requestedWork === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : Number.isFinite(requestedWork)
    ? Math.max(0, Math.floor(requestedWork))
    : 0;
  // The seed is the baseline for every progressive publication. Measure it
  // independently once so later exact-monotonic comparisons never inherit a
  // caller or incremental cache's stale tuple.
  refreshCandidateQuality(seed);
  const now = performance.now();
  return {
    mode,
    current,
    evaluationIds: [...seed.positions.keys()].sort(),
    residents,
    nodes,
    edges,
    centerId,
    allowAxisCompaction,
    trace,
    control,
    maximumWork,
    stats: {
      crossingsConsidered: 0,
      macrosConsidered: 0,
      pushClosures: 0,
      maxDepth: 0,
      visitedStates: 1,
    },
    seenStates: new Set([candidateFingerprint(seed)]),
    expandedStates: new Set(),
    axisPolishedStates: new Set(),
    cancelled: false,
    exhausted: false,
    improvements: 0,
    best: seed,
    published: seed,
    finish,
    lastProgressAt: now,
  };
}

// ---------------------------------------------------------------------------
// Route amendments: declarative detours for permanent fixed-room defects
// ---------------------------------------------------------------------------

/** Cells of slack around a defect's own geometry for the detour search. */
const ROUTE_AMENDMENT_ENVELOPE_MARGIN = 4;
/** Hard ceiling on one detour search's grid, keeping every search bounded. */
const ROUTE_AMENDMENT_ENVELOPE_CELL_LIMIT = 4_096;
const ROUTE_AMENDMENT_STEP_COST = 10;
const ROUTE_AMENDMENT_TURN_COST = 3;

interface DetourSegment {
  key: string;
  from: GridPosition;
  to: GridPosition;
}

interface LinkDetour {
  waypoints: GridPosition[];
  /** Unit steps in the full route; the cross-link "shorter detour" measure. */
  steps: number;
}

interface DetourSearchNode {
  x: number;
  y: number;
  /** Index into CROSSING_PUSH_DIRECTIONS, or -1 before the first step. */
  direction: number;
  cost: number;
  priority: number;
  /** Deterministic final tie-break: heap insertion order. */
  sequence: number;
  parent?: DetourSearchNode;
}

/**
 * The single cardinal vector every `from`→`to` traversal of a link declares,
 * or undefined when none is declared or declarations disagree. Only plainly
 * cardinal directions constrain a detour's walls; verticals, diagonals, and
 * `Other` leave the route free.
 */
function declaredWallVector(
  edges: readonly LayoutEdge[],
  from: string,
  to: string,
): GridPosition | undefined {
  let found: GridPosition | undefined;
  for (const edge of edges) {
    if (edge.from !== from || edge.to !== to) continue;
    const vector = CARDINAL_VECTORS[edge.direction];
    if (!vector) continue;
    if (found && (found.x !== vector.x || found.y !== vector.y)) return undefined;
    found = vector;
  }
  return found;
}

/** Exact integral point-on-closed-segment test on one level. */
function pointOnDetourSegment(point: GridPosition, from: GridPosition, to: GridPosition): boolean {
  if (point.level !== from.level || from.level !== to.level) return false;
  if ((to.x - from.x) * (point.y - from.y) !== (to.y - from.y) * (point.x - from.x)) return false;
  return Math.min(from.x, to.x) <= point.x && point.x <= Math.max(from.x, to.x) &&
    Math.min(from.y, to.y) <= point.y && point.y <= Math.max(from.y, to.y);
}

/** Binary min-heap over (priority, cost, sequence); fully deterministic. */
class DetourFrontier {
  readonly #values: DetourSearchNode[] = [];

  get size(): number {
    return this.#values.length;
  }

  static #before(a: DetourSearchNode, b: DetourSearchNode): boolean {
    return (a.priority - b.priority || a.cost - b.cost || a.sequence - b.sequence) < 0;
  }

  push(value: DetourSearchNode): void {
    const values = this.#values;
    values.push(value);
    let index = values.length - 1;
    while (index > 0) {
      const parent = (index - 1) >> 1;
      if (!DetourFrontier.#before(value, values[parent])) break;
      values[index] = values[parent];
      index = parent;
    }
    values[index] = value;
  }

  pop(): DetourSearchNode | undefined {
    const values = this.#values;
    const first = values[0];
    const last = values.pop();
    if (first === undefined || last === undefined || values.length === 0) return first;
    let index = 0;
    for (;;) {
      const left = index * 2 + 1;
      if (left >= values.length) break;
      const right = left + 1;
      const child = right < values.length && DetourFrontier.#before(values[right], values[left])
        ? right
        : left;
      if (!DetourFrontier.#before(values[child], last)) break;
      values[index] = values[child];
      index = child;
    }
    values[index] = last;
    return first;
  }
}

/**
 * Bounded deterministic A* for one link's orthogonal detour. The route runs
 * from the link's `a` room to its `b` room through integral cells inside an
 * envelope around the defect geometry, never entering an occupied cell,
 * never touching or crossing any other drawn link segment, and honoring the
 * link's declared cardinal exit walls at both ends. Turns are charged so an
 * equal-length route with fewer elbows wins; every remaining tie breaks on
 * the fixed North/East/South/West expansion order and insertion sequence.
 */
function computeLinkDetour(
  link: PhysicalLink,
  positions: ReadonlyMap<string, GridPosition>,
  occupied: ReadonlyMap<CellKey, string>,
  segments: readonly DetourSegment[],
  partnerPoints: readonly GridPosition[],
): LinkDetour | undefined {
  const from = positions.get(link.a);
  const to = positions.get(link.b);
  if (!from || !to || from.level !== to.level) return undefined;
  const level = from.level;

  let minX = Math.min(from.x, to.x);
  let maxX = Math.max(from.x, to.x);
  let minY = Math.min(from.y, to.y);
  let maxY = Math.max(from.y, to.y);
  for (const point of partnerPoints) {
    if (point.level !== level) continue;
    if (point.x < minX) minX = point.x;
    if (point.x > maxX) maxX = point.x;
    if (point.y < minY) minY = point.y;
    if (point.y > maxY) maxY = point.y;
  }
  minX -= ROUTE_AMENDMENT_ENVELOPE_MARGIN;
  maxX += ROUTE_AMENDMENT_ENVELOPE_MARGIN;
  minY -= ROUTE_AMENDMENT_ENVELOPE_MARGIN;
  maxY += ROUTE_AMENDMENT_ENVELOPE_MARGIN;
  if ((maxX - minX + 1) * (maxY - minY + 1) > ROUTE_AMENDMENT_ENVELOPE_CELL_LIMIT) {
    return undefined;
  }

  const foreign = segments.filter((segment) =>
    segment.key !== link.key && segment.from.level === level &&
    Math.min(segment.from.x, segment.to.x) <= maxX &&
    Math.max(segment.from.x, segment.to.x) >= minX &&
    Math.min(segment.from.y, segment.to.y) <= maxY &&
    Math.max(segment.from.y, segment.to.y) >= minY
  );
  const startWall = declaredWallVector(link.edges, link.a, link.b);
  const endWall = declaredWallVector(link.edges, link.b, link.a);

  const frontier = new DetourFrontier();
  const bestCost = new Map<number, number>();
  const width = maxX - minX + 1;
  const stateKey = (x: number, y: number, direction: number): number =>
    ((y - minY) * width + (x - minX)) * 5 + direction + 1;
  let sequence = 0;
  const start: DetourSearchNode = {
    x: from.x,
    y: from.y,
    direction: -1,
    cost: 0,
    priority: (Math.abs(to.x - from.x) + Math.abs(to.y - from.y)) * ROUTE_AMENDMENT_STEP_COST,
    sequence: sequence++,
  };
  frontier.push(start);
  bestCost.set(stateKey(start.x, start.y, -1), 0);

  let goal: DetourSearchNode | undefined;
  while (frontier.size > 0) {
    const current = frontier.pop() as DetourSearchNode;
    if (current.x === to.x && current.y === to.y) {
      goal = current;
      break;
    }
    if (current.cost !== bestCost.get(stateKey(current.x, current.y, current.direction))) continue;
    const currentPoint: GridPosition = { x: current.x, y: current.y, level };
    for (let direction = 0; direction < CROSSING_PUSH_DIRECTIONS.length; direction += 1) {
      const step = CROSSING_PUSH_DIRECTIONS[direction];
      if (current.direction < 0 && startWall &&
        (step.x !== startWall.x || step.y !== startWall.y)) continue;
      const x = current.x + step.x;
      const y = current.y + step.y;
      if (x < minX || x > maxX || y < minY || y > maxY) continue;
      const isGoal = x === to.x && y === to.y;
      if (isGoal && endWall && (step.x !== -endWall.x || step.y !== -endWall.y)) continue;
      const point: GridPosition = { x, y, level };
      if (!isGoal) {
        if (occupied.has(cellKeyAt(x, y, level))) continue;
        if (foreign.some((segment) => pointOnDetourSegment(point, segment.from, segment.to))) {
          continue;
        }
      }
      if (foreign.some((segment) =>
        strictSegmentsIntersect(currentPoint, point, segment.from, segment.to))) continue;
      const cost = current.cost + ROUTE_AMENDMENT_STEP_COST +
        (current.direction >= 0 && current.direction !== direction ? ROUTE_AMENDMENT_TURN_COST : 0);
      const key = stateKey(x, y, direction);
      const known = bestCost.get(key);
      if (known !== undefined && known <= cost) continue;
      bestCost.set(key, cost);
      frontier.push({
        x,
        y,
        direction,
        cost,
        priority: cost + (Math.abs(to.x - x) + Math.abs(to.y - y)) * ROUTE_AMENDMENT_STEP_COST,
        sequence: sequence++,
        parent: current,
      });
    }
  }
  if (!goal) return undefined;

  const path: GridPosition[] = [];
  for (let node: DetourSearchNode | undefined = goal; node; node = node.parent) {
    path.push({ x: node.x, y: node.y, level });
  }
  path.reverse();
  const waypoints: GridPosition[] = [];
  for (let index = 1; index < path.length - 1; index += 1) {
    const before = path[index - 1];
    const current = path[index];
    const after = path[index + 1];
    if (after.x - current.x !== current.x - before.x ||
      after.y - current.y !== current.y - before.y) {
      waypoints.push(current);
    }
  }
  if (waypoints.length === 0 || waypoints.length > MAX_ROUTE_AMENDMENT_WAYPOINTS) return undefined;
  return { waypoints, steps: path.length - 1 };
}

/**
 * Propose detours for every defect whose resolution set is empty because all
 * of its participating rooms are immovable: crossings between two links whose
 * four endpoints are fixed, and obstructed links whose endpoints and every
 * obstructing room are fixed. At most one amendment is proposed per physical
 * link; because a detour avoids every occupied cell and every other drawn
 * segment, one detour discharges all of its link's defects at once. For a
 * crossing, the link with the shorter detour is amended; ties (and a partner
 * with no detour) fall back to link id order. Returns undefined when there is
 * nothing to propose, so untouched plans stay byte-identical on the wire.
 */
function computeRouteAmendments(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  residents: ReadonlyMap<string, LayoutResident>,
  quality: Readonly<LayoutQuality>,
): RouteAmendment[] | undefined {
  if (quality.linkCrossings === 0 && quality.roomObstructions === 0) return undefined;
  let anyImmovable = false;
  for (const resident of residents.values()) {
    if (resident.movable === false) {
      anyImmovable = true;
      break;
    }
  }
  if (!anyImmovable) return undefined;
  const immovable = (id: string): boolean => residents.get(id)?.movable === false;

  const links = physicalLinks(positions, edges);
  const crossings = crossingPairs(positions, edges).filter((crossing) =>
    immovable(crossing.first.a) && immovable(crossing.first.b) &&
    immovable(crossing.second.a) && immovable(crossing.second.b)
  );

  // An obstructed link is permanent only when movement has nothing left to
  // offer: a movable obstructing room keeps the defect in the movement
  // engine's hands, so such links are deliberately not amended yet.
  const obstructed: PhysicalLink[] = [];
  for (const linkValue of links) {
    if (!immovable(linkValue.a) || !immovable(linkValue.b)) continue;
    const from = positions.get(linkValue.a);
    const to = positions.get(linkValue.b);
    if (!from || !to || from.level !== to.level) continue;
    let permanent = false;
    let movableObstruction = false;
    for (const [id, room] of positions) {
      if (id === linkValue.a || id === linkValue.b) continue;
      if (!segmentIntersectsRoomCell(from, to, room)) continue;
      if (immovable(id)) {
        permanent = true;
      } else {
        movableObstruction = true;
        break;
      }
    }
    if (permanent && !movableObstruction) obstructed.push(linkValue);
  }
  if (crossings.length === 0 && obstructed.length === 0) return undefined;

  const partnerPoints = new Map<string, GridPosition[]>();
  const addPartners = (key: string, other: PhysicalLink): void => {
    const points = partnerPoints.get(key) ?? [];
    const from = positions.get(other.a);
    const to = positions.get(other.b);
    if (from) points.push(from);
    if (to) points.push(to);
    partnerPoints.set(key, points);
  };
  for (const crossing of crossings) {
    addPartners(crossing.first.key, crossing.second);
    addPartners(crossing.second.key, crossing.first);
  }

  const occupied = occupiedCells(positions);
  const segments: DetourSegment[] = [];
  for (const linkValue of links) {
    const from = positions.get(linkValue.a);
    const to = positions.get(linkValue.b);
    if (from && to && from.level === to.level) {
      segments.push({ key: linkValue.key, from, to });
    }
  }
  const detours = new Map<string, LinkDetour | undefined>();
  const detourFor = (linkValue: PhysicalLink): LinkDetour | undefined => {
    if (detours.has(linkValue.key)) return detours.get(linkValue.key);
    const detour = computeLinkDetour(
      linkValue,
      positions,
      occupied,
      segments,
      partnerPoints.get(linkValue.key) ?? [],
    );
    detours.set(linkValue.key, detour);
    return detour;
  };

  const amended = new Map<string, RouteAmendment>();
  const amend = (linkValue: PhysicalLink, detour: LinkDetour): void => {
    amended.set(linkValue.key, {
      from: linkValue.a,
      to: linkValue.b,
      waypoints: detour.waypoints,
    });
  };
  // crossingPairs returns pairs in sorted key order and physicalLinks follows
  // the topology index's sorted order, so both passes are deterministic.
  for (const crossing of crossings) {
    if (amended.has(crossing.first.key) || amended.has(crossing.second.key)) continue;
    const first = detourFor(crossing.first);
    const second = detourFor(crossing.second);
    if (first && (!second || first.steps <= second.steps)) amend(crossing.first, first);
    else if (second) amend(crossing.second, second);
  }
  for (const linkValue of obstructed) {
    if (amended.has(linkValue.key)) continue;
    const detour = detourFor(linkValue);
    if (detour) amend(linkValue, detour);
  }
  if (amended.size === 0) return undefined;
  return [...amended.values()].sort((a, b) =>
    compareStrings(a.from, b.from) || compareStrings(a.to, b.to));
}

/**
 * Compute the advisory route amendments for a finished plan's exact
 * geometry. `planIntegralLayout` and `repairIntegralLayoutCrossingsDeep`
 * already attach this to their own results; the whole-layout constraint
 * repair applies it to its final winner so the repair result path carries
 * the same advisory detours as the ordinary path. Returns undefined for the
 * no-movement prompt lane and whenever there is nothing to propose.
 */
export function computeIntegralRouteAmendments(
  request: Pick<IntegralLayoutRequest, "residents" | "edges" | "allowExistingMoves">,
  plan: Pick<IntegralLayoutPlan, "positions" | "quality">,
): readonly RouteAmendment[] | undefined {
  if (request.allowExistingMoves === false) return undefined;
  const residents = new Map(request.residents.map((resident) => [resident.id, resident]));
  return computeRouteAmendments(plan.positions, request.edges, residents, plan.quality);
}

type PlanarAxis = "x" | "y";

const QUALITY_THROUGH_FOOTPRINT: readonly (keyof LayoutQuality)[] = [
  "levelViolations",
  "cardinalRayViolations",
  "reciprocalRayViolations",
  "routingViolations",
  "exitPortViolations",
  "reciprocalExitPortViolations",
  "roomObstructions",
  "linkCrossings",
  "levelSlack",
  "footprintArea",
  "footprintPerimeter",
];

/** `compareLayoutQuality` without cardinal slack, its last field. */
function compareQualityThroughFootprint(a: LayoutQuality, b: LayoutQuality): number {
  const primary = layoutQualityScore(b) - layoutQualityScore(a);
  if (primary !== 0) return primary;
  for (const field of QUALITY_THROUGH_FOOTPRINT) {
    const aValue = a[field] ?? 0;
    const bValue = b[field] ?? 0;
    if (aValue !== bValue) return bValue - aValue;
  }
  return 0;
}

/**
 * Rooms which must translate together on one planar axis to preserve every
 * perpendicular or multi-axis protected ray. Along-axis cardinal constraints
 * remain inequalities, so separate groups can close or redistribute slack.
 */
function axisTranslationGroups(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  axis: PlanarAxis,
  coherentOnly = false,
): Set<string>[] {
  const sets = new DisjointSet();
  for (const id of positions.keys()) sets.add(id);
  for (const edge of edges) {
    if (!positions.has(edge.from) || !positions.has(edge.to)) continue;
    const expected = protectedVector(edge);
    if (!expected) continue;
    if (coherentOnly) {
      const from = positions.get(edge.from) as GridPosition;
      const to = positions.get(edge.to) as GridPosition;
      if (protectedRayDistance(edge, subtract(to, from)) === undefined) continue;
    }
    const nonzeroAxes = Number(expected.x !== 0) + Number(expected.y !== 0) +
      Number(expected.level !== 0);
    if (expected[axis] === 0 || nonzeroAxes > 1) sets.union(edge.from, edge.to);
  }
  const groups = new Map<string, Set<string>>();
  for (const id of positions.keys()) {
    const root = sets.find(id);
    const group = groups.get(root) ?? new Set<string>();
    group.add(id);
    groups.set(root, group);
  }
  return [...groups.values()].sort((a, b) => {
    const firstA = [...a].sort()[0] ?? "";
    const firstB = [...b].sort()[0] ?? "";
    return compareStrings(firstA, firstB);
  });
}

function boundsByLevel(
  positions: ReadonlyMap<string, GridPosition>,
): Map<number, { minX: number; maxX: number; minY: number; maxY: number }> {
  const result = new Map<number, { minX: number; maxX: number; minY: number; maxY: number }>();
  for (const position of positions.values()) {
    const known = result.get(position.level);
    if (!known) {
      result.set(position.level, {
        minX: position.x,
        maxX: position.x,
        minY: position.y,
        maxY: position.y,
      });
      continue;
    }
    known.minX = Math.min(known.minX, position.x);
    known.maxX = Math.max(known.maxX, position.x);
    known.minY = Math.min(known.minY, position.y);
    known.maxY = Math.max(known.maxY, position.y);
  }
  return result;
}

function planarGravity(positions: ReadonlyMap<string, GridPosition>): number {
  let result = 0;
  for (const position of positions.values()) result += position.x + position.y;
  return result;
}

/**
 * All rooms bucketed by level and perpendicular coordinate for one axis, so
 * per-group distance scans touch only the rooms actually sharing a lane with
 * a moving room instead of the whole stationary map. Buckets deliberately
 * include the moving rooms; callers exclude the current group by membership.
 */
function axisLaneIndex(
  positions: ReadonlyMap<string, GridPosition>,
  axis: PlanarAxis,
): Map<CellKey, [string, number][]> {
  const perpendicular: PlanarAxis = axis === "x" ? "y" : "x";
  const result = new Map<CellKey, [string, number][]>();
  for (const [id, position] of positions) {
    const key = laneKey(position.level, position[perpendicular]);
    const lane = result.get(key);
    const entry: [string, number] = [id, position[axis]];
    if (lane) lane.push(entry);
    else result.set(key, [entry]);
  }
  return result;
}

function axisCandidateDistances(
  positions: ReadonlyMap<string, GridPosition>,
  moving: ReadonlySet<string>,
  lanes: Map<CellKey, [string, number][]>,
  edges: readonly LayoutEdge[],
  axis: PlanarAxis,
  minimum: number,
  maximum: number,
): number[] {
  const result = new Set<number>([minimum, maximum]);
  const perpendicular: PlanarAxis = axis === "x" ? "y" : "x";
  for (const id of moving) {
    const position = positions.get(id);
    if (!position) continue;
    const lane = lanes.get(laneKey(position.level, position[perpendicular]));
    if (!lane) continue;
    for (const [otherId, otherAxis] of lane) {
      if (moving.has(otherId)) continue;
      const collisionDistance = otherAxis - position[axis];
      result.add(collisionDistance - 1);
      result.add(collisionDistance + 1);
    }
  }
  for (const edge of edges) {
    const fromMoves = moving.has(edge.from);
    const toMoves = moving.has(edge.to);
    if (fromMoves === toMoves) continue;
    const expected = protectedVector(edge);
    if (!expected || expected[axis] === 0) continue;
    const movingId = fromMoves ? edge.from : edge.to;
    const stationaryId = fromMoves ? edge.to : edge.from;
    const movingPosition = positions.get(movingId);
    const stationaryPosition = positions.get(stationaryId);
    if (!movingPosition || !stationaryPosition) continue;
    const target = fromMoves
      ? stationaryPosition[axis] - Math.sign(expected[axis])
      : stationaryPosition[axis] + Math.sign(expected[axis]);
    const exactDistance = target - movingPosition[axis];
    result.add(exactDistance);
    result.add(exactDistance - 1);
    result.add(exactDistance + 1);
  }
  return [...result]
    .filter((distance) => distance !== 0 && distance >= minimum && distance <= maximum)
    .sort((a, b) => a - b);
}

interface CardinalSeries {
  axis: PlanarAxis;
  ids: readonly string[];
}

/** Maximal, unbranched, currently coherent physical cardinal runs. */
function cardinalSeries(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  reciprocalOnly: boolean,
): CardinalSeries[] {
  const reciprocalEdges = reciprocalProtectedEdges(edges);
  const result: CardinalSeries[] = [];
  for (const axis of ["x", "y"] as const) {
    const perpendicular: PlanarAxis = axis === "x" ? "y" : "x";
    const adjacency = new Map<string, Set<string>>();
    const connect = (a: string, b: string): void => {
      const values = adjacency.get(a) ?? new Set<string>();
      values.add(b);
      adjacency.set(a, values);
    };
    for (const link of topologyIndex(edges).physical) {
      const a = positions.get(link.a);
      const b = positions.get(link.b);
      if (!a || !b || a.level !== b.level || a[perpendicular] !== b[perpendicular] ||
        a[axis] === b[axis]) continue;
      const planar = link.edges.filter((edge) => {
        const expected = protectedVector(edge);
        return !!expected && expected.level === 0 && expected[axis] !== 0 &&
          expected[perpendicular] === 0;
      });
      if (planar.length === 0 || planar.some((edge) => {
        const from = positions.get(edge.from);
        const to = positions.get(edge.to);
        return !from || !to || protectedRayDistance(edge, subtract(to, from)) === undefined;
      })) continue;
      const reciprocal = planar.some((edge) => reciprocalEdges.has(edge));
      if (reciprocalOnly && !reciprocal) continue;
      connect(link.a, link.b);
      connect(link.b, link.a);
    }

    // Junctions terminate rather than invalidate a series. Walking each edge
    // out of every non-degree-two room extracts the maximal unbranched arms of
    // T/Y-shaped corridors while naturally skipping chorded all-degree-two
    // cycles. Physical edges, not rooms, are marked so each arm appears once.
    const walked = new Set<string>();
    const edgeKey = (a: string, b: string): string => a < b ? `${a}\u0000${b}` : `${b}\u0000${a}`;
    const terminals = [...adjacency.keys()]
      .filter((id) => (adjacency.get(id)?.size ?? 0) !== 2)
      .sort();
    for (const root of terminals) {
      for (const first of [...(adjacency.get(root) ?? [])].sort()) {
        if (walked.has(edgeKey(root, first))) continue;
        const path = [root];
        let previous = root;
        let cursor = first;
        walked.add(edgeKey(previous, cursor));
        for (;;) {
          path.push(cursor);
          const neighbors = [...(adjacency.get(cursor) ?? [])].sort();
          if (neighbors.length !== 2) break;
          const next = neighbors[0] === previous ? neighbors[1] : neighbors[0];
          if (!next || walked.has(edgeKey(cursor, next))) break;
          walked.add(edgeKey(cursor, next));
          previous = cursor;
          cursor = next;
        }
        if (path.length < 3) continue;
        const firstPosition = positions.get(path[0]) as GridPosition;
        const lastPosition = positions.get(path[path.length - 1]) as GridPosition;
        if (firstPosition[axis] > lastPosition[axis] ||
          (firstPosition[axis] === lastPosition[axis] && path[0] > path[path.length - 1])) {
          path.reverse();
        }
        if (path.some((id, index) => index > 0 &&
          (positions.get(path[index - 1]) as GridPosition)[axis] >=
            (positions.get(id) as GridPosition)[axis])) continue;
        result.push({ axis, ids: path });
      }
    }
  }
  return result.sort((a, b) =>
    compareStrings(a.axis, b.axis) || compareStrings(a.ids.join("\u0000"), b.ids.join("\u0000"))
  );
}

function cardinalSeriesPenalty(
  series: CardinalSeries,
  positions: ReadonlyMap<string, GridPosition>,
): number {
  const first = positions.get(series.ids[0]);
  const last = positions.get(series.ids[series.ids.length - 1]);
  if (!first || !last) return Number.MAX_SAFE_INTEGER;
  const intervals = series.ids.length - 1;
  const span = last[series.axis] - first[series.axis];
  let result = 0;
  for (let index = 1; index < series.ids.length; index += 1) {
    const before = positions.get(series.ids[index - 1]);
    const after = positions.get(series.ids[index]);
    if (!before || !after) return Number.MAX_SAFE_INTEGER;
    const error = intervals * (after[series.axis] - before[series.axis]) - span;
    result += error * error;
  }
  return result;
}

function axisSpacingPenalty(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  definitions?: {
    reciprocal: readonly CardinalSeries[];
    all: readonly CardinalSeries[];
  },
): readonly [number, number] {
  const reciprocalSeries = definitions?.reciprocal ?? cardinalSeries(positions, edges, true);
  const allSeries = definitions?.all ?? cardinalSeries(positions, edges, false);
  const reciprocal = reciprocalSeries.every((series) => seriesRemainsCoherent(series, positions, edges))
    ? reciprocalSeries.reduce((total, series) => total + cardinalSeriesPenalty(series, positions), 0)
    : Number.POSITIVE_INFINITY;
  const all = allSeries.every((series) => seriesRemainsCoherent(series, positions, edges))
    ? allSeries.reduce((total, series) => total + cardinalSeriesPenalty(series, positions), 0)
    : Number.POSITIVE_INFINITY;
  return [reciprocal, all];
}

/** Series decomposition cached per candidate; the geometry never mutates. */
function candidateCardinalSeries(value: Candidate, reciprocalOnly: boolean): CardinalSeries[] {
  if (reciprocalOnly) {
    return value.score.cardinalSeriesReciprocal ??=
      cardinalSeries(value.positions, value.edges, true);
  }
  return value.score.cardinalSeriesAll ??= cardinalSeries(value.positions, value.edges, false);
}

/**
 * Spacing penalties cached per (candidate, definition-set). Keying the cache
 * by the `all` array's identity works because candidateCardinalSeries hands
 * out one stable array per incumbent, so repeated comparisons against the
 * same incumbent hit.
 */
function candidateAxisSpacingPenalty(
  value: Candidate,
  definitions: { reciprocal: readonly CardinalSeries[]; all: readonly CardinalSeries[] },
): readonly [number, number] {
  const cache = value.score.spacingPenalties ??= new WeakMap();
  const known = cache.get(definitions.all);
  if (known) return known;
  const result = axisSpacingPenalty(value.positions, value.edges, definitions);
  cache.set(definitions.all, result);
  return result;
}

/** Public quality first, then aesthetics from the incumbent's fixed series. */
function compareCompactionCandidates(a: Candidate, b: Candidate): number {
  const quality = compareLayoutQuality(candidateQuality(a), candidateQuality(b));
  if (quality !== 0) return quality;
  const definitions = {
    reciprocal: candidateCardinalSeries(b, true),
    all: candidateCardinalSeries(b, false),
  };
  const spacingA = candidateAxisSpacingPenalty(a, definitions);
  const spacingB = candidateAxisSpacingPenalty(b, definitions);
  if (spacingA[0] !== spacingB[0]) return spacingB[0] - spacingA[0];
  if (spacingA[1] !== spacingB[1]) return spacingB[1] - spacingA[1];
  return 0;
}

function axisOffset(axis: PlanarAxis, distance: number): Origin {
  return axis === "x"
    ? { x: distance, y: 0, level: 0 }
    : { x: 0, y: distance, level: 0 };
}

/**
 * The fits() predicate against a whole-map occupant index, treating the
 * moving ids as already vacated. Equivalent to cloning the stationary
 * remainder and probing it, without the per-group O(n) clone.
 */
function fitsAmongStationary(
  placement: ReadonlyMap<string, GridPosition>,
  occupants: RoomOccupantIndex,
  moving: ReadonlySet<string>,
): boolean {
  const staged = new Set<CellKey>();
  for (const [, position] of placement) {
    const key = cellKey(position);
    if (staged.has(key)) return false;
    const occupant = occupants.cells.get(key);
    if (occupant !== undefined) {
      if (typeof occupant === "string") {
        if (!moving.has(occupant)) return false;
      } else if (occupant.some((id) => !moving.has(id))) return false;
    }
    staged.add(key);
  }
  return true;
}

function translatedAxisCandidate(
  base: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
  moving: ReadonlySet<string>,
  axis: PlanarAxis,
  distance: number,
  occupants: RoomOccupantIndex,
  translationContext?: RigidTranslationContext,
): Candidate | undefined {
  const placement = new Map<string, GridPosition>();
  for (const id of moving) {
    const position = base.positions.get(id);
    if (!position) continue;
    placement.set(id, { ...position, [axis]: position[axis] + distance });
  }
  if (!fitsAmongStationary(placement, occupants, moving)) return undefined;
  const positions = new Map(base.positions);
  for (const [id, position] of placement) positions.set(id, position);
  return {
    positions,
    current,
    edges,
    score: { collisions: 0 },
    derivation: {
      base,
      changedIds: moving,
      translation: translationContext
        ? { offset: axisOffset(axis, distance), context: translationContext }
        : undefined,
    },
  };
}

/**
 * Pack planar axis groups in two phases. The first walks four independent,
 * recursively pushed directional plateaus without worsening protected quality
 * or footprint; the second greedily closes corridor slack. Keeping every trial
 * inside the seed's per-level bounds makes each gravity trajectory finite and
 * prevents compaction from expanding the map.
 */
function axisGroupCompaction(
  seed: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
  trace?: (event: LayoutTraceEvent) => void,
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
  shouldCancel?: IntegralLayoutCompactionControl["shouldCancel"],
): Candidate {
  let winner = seed;
  // Fingerprints, not canonical keys: this set only suppresses re-yielding a
  // state the tournament already explored, so the retained-state reduction's
  // collision argument applies unchanged.
  const seen = new Set([candidateFingerprint(seed)]);
  const iterationLimit = Math.max(1, seed.positions.size * 2);
  const progressStarted = trace ? performance.now() : 0;
  let lastProgressAt = progressStarted;
  let candidatesConsidered = 0;
  const publishProgress = (
    phase: "gravity" | "spacing",
    force = false,
    complete = false,
  ): void => {
    if (!trace || candidatesConsidered === 0) return;
    const now = performance.now();
    if (!force && now - lastProgressAt < PROGRESS_INTERVAL_MS) return;
    lastProgressAt = now;
    const bestQuality = complete
      ? refreshCandidateQuality(winner)
      : candidateQuality(winner);
    trace({
      type: "axis-progress",
      stage: "axis-compaction",
      phase,
      candidatesConsidered,
      complete,
      elapsedMs: Math.max(0, now - progressStarted),
      bestQuality: { ...bestQuality },
    });
  };

  const candidates = function* (base: Candidate): Generator<Candidate> {
    const levelBounds = boundsByLevel(base.positions);
    // One occupant index and one lane index per axis serve every group of
    // this base; group trials exclude their own rooms by membership.
    const baseOccupants = roomOccupantIndex(base.positions);
    for (const axis of ["x", "y"] as const) {
      const lanes = axisLaneIndex(base.positions, axis);
      for (const moving of axisTranslationGroups(base.positions, edges, axis)) {
        if (centerId && moving.has(centerId)) continue;
        if (!movableRegion(moving, residents)) continue;
        let minimum = Number.NEGATIVE_INFINITY;
        let maximum = Number.POSITIVE_INFINITY;
        for (const id of moving) {
          const position = base.positions.get(id);
          if (!position) continue;
          const level = levelBounds.get(position.level);
          if (!level) continue;
          const lower = axis === "x" ? level.minX : level.minY;
          const upper = axis === "x" ? level.maxX : level.maxY;
          minimum = Math.max(minimum, lower - position[axis]);
          maximum = Math.min(maximum, upper - position[axis]);
        }
        if (!Number.isFinite(minimum) || !Number.isFinite(maximum)) continue;
        const distances = axisCandidateDistances(
          base.positions,
          moving,
          lanes,
          edges,
          axis,
          minimum,
          maximum,
        );
        // One rigid-translation context serves every trial distance of this
        // group, so index builds amortize across the whole scan.
        const translationContext = rigidTranslationContext(base, moving);
        for (const distance of distances) {
          const evaluated = translatedAxisCandidate(
            base,
            current,
            edges,
            moving,
            axis,
            distance,
            baseOccupants,
            translationContext,
          );
          if (!evaluated) continue;
          if (!acceptedPositions(evaluated.positions, acceptsPositions)) continue;
          candidatesConsidered += 1;
          publishProgress("spacing");
          if (!seen.has(candidateFingerprint(evaluated))) yield evaluated;
        }
      }
    }
  };

  type GravitySign = -1 | 1;
  interface GravityOrientation {
    x: GravitySign;
    y: GravitySign;
  }
  const gravityOrientations: readonly GravityOrientation[] = [
    { x: 1, y: 1 },
    { x: 1, y: -1 },
    { x: -1, y: 1 },
    { x: -1, y: -1 },
  ];
  const immutableBounds = boundsByLevel(seed.positions);
  interface GravityGroups {
    groups: Record<PlanarAxis, Set<string>[]>;
    byId: Record<PlanarAxis, Map<string, ReadonlySet<string>>>;
  }
  const buildGravityGroups = (positions: ReadonlyMap<string, GridPosition>): GravityGroups => {
    const groups: GravityGroups["groups"] = {
      // Gravity preserves only relations which are coherent in its seed. A
      // currently relaxed/violated edge must remain available for repair rather
      // than permanently welding its endpoints into one translation group.
      x: axisTranslationGroups(positions, edges, "x", true),
      y: axisTranslationGroups(positions, edges, "y", true),
    };
    const byId: GravityGroups["byId"] = { x: new Map(), y: new Map() };
    for (const axis of ["x", "y"] as const) {
      for (const group of groups[axis]) {
        for (const id of group) byId[axis].set(id, group);
      }
    }
    return { groups, byId };
  };
  const unprotected = new Set<string>();
  const directionalGravity = (
    positions: ReadonlyMap<string, GridPosition>,
    orientation: GravityOrientation,
  ): number => {
    let result = 0;
    for (const position of positions.values()) {
      result += orientation.x * position.x + orientation.y * position.y;
    }
    return result;
  };
  const groupAwarePushClosure = (
    positions: ReadonlyMap<string, GridPosition>,
    roots: ReadonlySet<string>,
    axis: PlanarAxis,
    sign: GravitySign,
    occupants: RoomOccupantIndex,
    groupById: ReadonlyMap<string, ReadonlySet<string>>,
  ): Set<string> | undefined => {
    const offset: Origin = axis === "x"
      ? { x: sign, y: 0, level: 0 }
      : { x: 0, y: sign, level: 0 };
    const expandedRoots = new Set(roots);
    for (;;) {
      const closure = safePushClosure(
        positions,
        expandedRoots,
        unprotected,
        residents,
        edges,
        offset,
        occupants,
      );
      if (!closure) return undefined;
      let expanded = false;
      for (const id of closure) {
        for (const attached of groupById.get(id) ?? [id]) {
          if (closure.has(attached)) continue;
          expandedRoots.add(attached);
          expanded = true;
        }
      }
      if (!expanded) return closure;
    }
  };
  const runGravityTrajectory = (
    start: Candidate,
    orientation: GravityOrientation,
    gravityGroups: GravityGroups,
  ): Candidate => {
    let working = start;
    let potential = directionalGravity(working.positions, orientation);
    // One occupant index survives the whole trajectory: candidate cloning
    // preserves map insertion order, so the index's tie-break ordering stays
    // valid as accepted one-cell advances are folded in below.
    const occupants = roomOccupantIndex(working.positions);
    for (;;) {
      if (shouldCancel?.()) return working;
      let advanced = false;
      for (const axis of ["x", "y"] as const) {
        if (shouldCancel?.()) return working;
        const sign = orientation[axis];
        for (const roots of gravityGroups.groups[axis]) {
          if (shouldCancel?.()) return working;
          if (centerId && roots.has(centerId)) continue;
          if (!movableRegion(roots, residents)) continue;
          const closure = groupAwarePushClosure(
            working.positions,
            roots,
            axis,
            sign,
            occupants,
            gravityGroups.byId[axis],
          );
          if (!closure || (centerId && closure.has(centerId))) continue;

          let insideBounds = true;
          for (const id of closure) {
            const position = working.positions.get(id);
            if (!position) continue;
            const level = immutableBounds.get(position.level);
            if (!level) {
              insideBounds = false;
              break;
            }
            const translated = position[axis] + sign;
            const lower = axis === "x" ? level.minX : level.minY;
            const upper = axis === "x" ? level.maxX : level.maxY;
            if (translated < lower || translated > upper) {
              insideBounds = false;
              break;
            }
          }
          if (!insideBounds) continue;

          const evaluated = translatedAxisCandidate(
            working,
            current,
            edges,
            closure,
            axis,
            sign,
            occupants,
            rigidTranslationContext(working, closure),
          );
          if (!evaluated) continue;
          if (!acceptedPositions(evaluated.positions, acceptsPositions)) continue;
          candidatesConsidered += 1;
          publishProgress("gravity");
          if (compareQualityThroughFootprint(
            candidateQuality(evaluated),
            candidateQuality(working),
          ) < 0) continue;
          const nextPotential = directionalGravity(evaluated.positions, orientation);
          if (nextPotential <= potential) continue;

          retranslateIndexedOccupants(
            occupants,
            closure,
            working.positions,
            axisOffset(axis, sign),
          );
          const retained = detachedCandidate(evaluated);
          working = retained;
          potential = nextPotential;
          advanced = true;
          if (comparePublicCandidates(retained, winner) > 0) winner = retained;
        }
      }
      if (!advanced) return working;
    }
  };

  // Preserve the useful neutral basin explored by the original compactor.
  // Recursive coherent-group gravity is intentionally stricter and therefore
  // does not subsume this one-pass southeast sweep: a neutral legacy endpoint
  // can expose a strict corridor squeeze which none of the four recursive
  // trajectories reaches. Keep it independent so its plateau is never mixed
  // into, or used to bias, another directional trajectory.
  const runLegacyGravityTrajectory = (start: Candidate): Candidate => {
    let working = start;
    // As in the recursive trajectories, one maintained occupant index stands
    // in for the per-group stationary clone.
    const occupants = roomOccupantIndex(working.positions);
    for (const axis of ["x", "y"] as const) {
      const groups = axisTranslationGroups(working.positions, edges, axis);
      for (const moving of groups) {
        if (shouldCancel?.()) return working;
        if (centerId && moving.has(centerId)) continue;
        if (!movableRegion(moving, residents)) continue;
        const levelBounds = boundsByLevel(working.positions);
        let maximum = Number.POSITIVE_INFINITY;
        for (const id of moving) {
          const position = working.positions.get(id);
          if (!position) continue;
          const level = levelBounds.get(position.level);
          if (!level) continue;
          const upper = axis === "x" ? level.maxX : level.maxY;
          maximum = Math.min(maximum, upper - position[axis]);
        }
        if (!Number.isFinite(maximum) || maximum <= 0) continue;
        const lanes = axisLaneIndex(working.positions, axis);
        const translationContext = rigidTranslationContext(working, moving);
        let selected: Candidate | undefined;
        let selectedDistance = 0;
        for (const distance of axisCandidateDistances(
          working.positions,
          moving,
          lanes,
          edges,
          axis,
          1,
          maximum,
        )) {
          if (shouldCancel?.()) return working;
          const evaluated = translatedAxisCandidate(
            working,
            current,
            edges,
            moving,
            axis,
            distance,
            occupants,
            translationContext,
          );
          if (!evaluated || !acceptedPositions(evaluated.positions, acceptsPositions)) continue;
          candidatesConsidered += 1;
          publishProgress("gravity");
          const phaseComparison = compareQualityThroughFootprint(
            candidateQuality(evaluated),
            candidateQuality(working),
          );
          if (phaseComparison < 0) continue;
          if (comparePublicCandidates(evaluated, winner) > 0) winner = evaluated;
          if (!selected || phaseComparison > compareQualityThroughFootprint(
            candidateQuality(selected),
            candidateQuality(working),
          ) || (phaseComparison === compareQualityThroughFootprint(
            candidateQuality(selected),
            candidateQuality(working),
          ) && planarGravity(evaluated.positions) > planarGravity(selected.positions))) {
            selected = evaluated;
            selectedDistance = distance;
          }
        }
        if (selected) {
          retranslateIndexedOccupants(
            occupants,
            moving,
            working.positions,
            axisOffset(axis, selectedDistance),
          );
          working = detachedCandidate(selected);
        }
      }
    }
    return working;
  };

  const squeeze = (start: Candidate): void => {
    let base = start;
    for (let iteration = 0; iteration < iterationLimit; iteration += 1) {
      if (shouldCancel?.()) return;
      let selected = base;
      for (const evaluated of candidates(base)) {
        if (comparePublicCandidates(evaluated, selected) > 0) selected = evaluated;
      }
      if (selected === base) break;
      const key = candidateFingerprint(selected);
      if (seen.has(key)) break;
      seen.add(key);
      base = selected;
      if (comparePublicCandidates(base, winner) > 0) winner = base;
    }
  };

  // Four independent directional plateaus start from the same strict seed.
  // After spacing, a newly strict winner starts another complete tournament;
  // neutral endpoints from different orientations are never mixed together.
  for (;;) {
    if (shouldCancel?.()) break;
    const tournamentSeed = winner;
    const gravityGroups = buildGravityGroups(tournamentSeed.positions);
    const gravityEndpoints = gravityOrientations.map((orientation) =>
      runGravityTrajectory(tournamentSeed, orientation, gravityGroups)
    );
    const legacyEndpoint = runLegacyGravityTrajectory(tournamentSeed);
    publishProgress("gravity", true);

    const squeezeStarts = [winner, legacyEndpoint, ...gravityEndpoints];
    const squeezed = new Set<string>();
    for (const start of squeezeStarts) {
      if (shouldCancel?.()) break;
      const key = candidateFingerprint(start);
      if (squeezed.has(key)) continue;
      squeezed.add(key);
      squeeze(start);
    }
    publishProgress("spacing", true);
    if (comparePublicCandidates(winner, tournamentSeed) <= 0) break;
  }
  publishProgress("spacing", true, true);
  return winner;
}

/**
 * Remove globally empty rows and columns without changing the ordering of any
 * occupied rows or columns. Each gap can be closed from either side; locked
 * residents simply make that side unavailable. Closing a whole gap at once is
 * equivalent to repeated one-cell half-plane shifts, but avoids making large
 * sparse maps expensive to compact.
 */
function vacuumLayout(
  seed: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  trace: IntegralLayoutRequest["trace"],
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
  shouldCancel?: IntegralLayoutCompactionControl["shouldCancel"],
): Candidate {
  let best = seed;
  const iterationLimit = Math.max(1, best.positions.size * 2);

  for (let iteration = 0; iteration < iterationLimit; iteration += 1) {
    if (shouldCancel?.()) break;
    let next = best;
    let accepted: {
      axis: "x" | "y";
      lower: number;
      upper: number;
      distance: number;
      moved: string[];
    } | undefined;
    for (const axis of ["x", "y"] as const) {
      if (shouldCancel?.()) break;
      const coordinates = [...new Set([...best.positions.values()].map((position) => position[axis]))]
        .sort((a, b) => a - b);
      for (let index = 0; index + 1 < coordinates.length; index += 1) {
        if (shouldCancel?.()) break;
        const lower = coordinates[index];
        const upper = coordinates[index + 1];
        const gap = upper - lower - 1;
        if (gap <= 0) continue;

        const attempts: readonly [
          (position: GridPosition) => boolean,
          number,
        ][] = [
          [(position) => position[axis] <= lower, gap],
          [(position) => position[axis] >= upper, -gap],
        ];
        for (const [includes, distance] of attempts) {
          if (shouldCancel?.()) break;
          const moving = new Set([...best.positions]
            .filter(([, position]) => includes(position))
            .map(([id]) => id));
          if (moving.size === 0 || !movableRegion(moving, residents)) continue;

          const trial = new Map(best.positions);
          for (const id of moving) {
            const position = trial.get(id);
            if (!position) continue;
            trial.set(id, {
              ...position,
              [axis]: position[axis] + distance,
            });
          }
          if (hasCollisions(trial)) continue;
          if (!acceptedPositions(trial, acceptsPositions)) continue;
          const evaluated = candidate(trial, current, edges, {
            base: best,
            changedIds: moving,
            translation: {
              offset: axisOffset(axis, distance),
              context: rigidTranslationContext(best, moving),
            },
          });
          if (compareCandidates(evaluated, next) > 0) {
            next = evaluated;
            accepted = {
              axis,
              lower,
              upper,
              distance,
              moved: [...moving].sort(),
            };
          }
        }
      }
    }
    if (next === best) break;
    if (accepted) {
      trace?.({
        type: "vacuum",
        stage: "vacuum",
        iteration,
        ...accepted,
        before: traceCandidate(best),
        after: traceCandidate(next),
      });
    }
    best = next;
  }
  return best;
}

interface SeriesSpacingCandidate {
  candidate: Candidate;
  reciprocalPenalty: number;
  totalPenalty: number;
  changedGroups: number;
  displacement: number;
  key: string;
}

function seriesRemainsCoherent(
  series: CardinalSeries,
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): boolean {
  const byPair = topologyIndex(edges).byPair;
  for (let index = 1; index < series.ids.length; index += 1) {
    const first = series.ids[index - 1];
    const second = series.ids[index];
    const link = first <= second
      ? byPair.get(`${first}|${second}`)
      : byPair.get(`${second}|${first}`);
    if (!link) return false;
    const perpendicular: PlanarAxis = series.axis === "x" ? "y" : "x";
    const planar = link.edges.filter((edge) => {
      const expected = protectedVector(edge);
      return !!expected && expected.level === 0 && expected[series.axis] !== 0 &&
        expected[perpendicular] === 0;
    });
    if (planar.length === 0 || planar.some((edge) => {
      const from = positions.get(edge.from);
      const to = positions.get(edge.to);
      return !from || !to || protectedRayDistance(edge, subtract(to, from)) === undefined;
    })) return false;
  }
  return true;
}

/**
 * Redistribute unavoidable slack across straight cardinal runs after the map's
 * envelope is compact. Each run is moved atomically to its canonical integral
 * spacing; no public quality field, existing run, fixed room, or hard admission
 * may regress. Reciprocal series are the first aesthetic tie-break.
 */
function evenCardinalSeries(
  seed: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
  trace?: (event: LayoutTraceEvent) => void,
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
  shouldCancel?: IntegralLayoutCompactionControl["shouldCancel"],
): Candidate {
  const definitions = seed.edges === edges
    ? candidateCardinalSeries(seed, false)
    : cardinalSeries(seed.positions, edges, false);
  if (definitions.length === 0) return seed;
  const reciprocalDefinitions = seed.edges === edges
    ? candidateCardinalSeries(seed, true)
    : cardinalSeries(seed.positions, edges, true);
  const immutableBounds = boundsByLevel(seed.positions);
  const groupsByAxis = new Map<PlanarAxis, Map<string, ReadonlySet<string>>>();
  for (const axis of ["x", "y"] as const) {
    const byId = new Map<string, ReadonlySet<string>>();
    for (const group of axisTranslationGroups(seed.positions, edges, axis, true)) {
      for (const id of group) byId.set(id, group);
    }
    groupsByAxis.set(axis, byId);
  }
  const penalties = (
    positions: ReadonlyMap<string, GridPosition>,
  ): {
    reciprocal: number;
    total: number;
    reciprocalEach: number[];
    each: number[];
  } => ({
    reciprocal: reciprocalDefinitions.reduce(
      (total, series) => total + cardinalSeriesPenalty(series, positions),
      0,
    ),
    total: definitions.reduce(
      (total, series) => total + cardinalSeriesPenalty(series, positions),
      0,
    ),
    reciprocalEach: reciprocalDefinitions.map((series) => cardinalSeriesPenalty(series, positions)),
    each: definitions.map((series) => cardinalSeriesPenalty(series, positions)),
  });
  const compare = (a: SeriesSpacingCandidate, b: SeriesSpacingCandidate): number => {
    const quality = compareLayoutQuality(candidateQuality(a.candidate), candidateQuality(b.candidate));
    if (quality !== 0) return quality;
    if (a.reciprocalPenalty !== b.reciprocalPenalty) {
      return b.reciprocalPenalty - a.reciprocalPenalty;
    }
    if (a.totalPenalty !== b.totalPenalty) return b.totalPenalty - a.totalPenalty;
    if (a.changedGroups !== b.changedGroups) return b.changedGroups - a.changedGroups;
    if (a.displacement !== b.displacement) return b.displacement - a.displacement;
    return compareStrings(b.key, a.key);
  };
  let best = seed;
  const seen = new Set([positionMapKey(seed.positions)]);
  let candidatesConsidered = 0;
  const started = trace ? performance.now() : 0;
  let lastProgressAt = started;
  const publishProgress = (force = false, complete = false): void => {
    if (!trace || candidatesConsidered === 0) return;
    const now = performance.now();
    if (!force && now - lastProgressAt < PROGRESS_INTERVAL_MS) return;
    lastProgressAt = now;
    const bestQuality = complete
      ? refreshCandidateQuality(best)
      : candidateQuality(best);
    trace({
      type: "axis-progress",
      stage: "axis-compaction",
      phase: "spacing",
      candidatesConsidered,
      complete,
      elapsedMs: Math.max(0, now - started),
      bestQuality: { ...bestQuality },
    });
  };

  for (;;) {
    if (shouldCancel?.()) break;
    const beforeQuality = candidateQuality(best);
    const beforePenalties = penalties(best.positions);
    let selected: SeriesSpacingCandidate | undefined;
    for (const series of definitions) {
      const byId = groupsByAxis.get(series.axis) as Map<string, ReadonlySet<string>>;
      const endpointGroups = new Set([
        byId.get(series.ids[0]),
        byId.get(series.ids[series.ids.length - 1]),
      ]);
      const anchors = new Set([0, series.ids.length - 1]);
      for (let index = 1; index + 1 < series.ids.length; index += 1) {
        const group = byId.get(series.ids[index]);
        if (!group || endpointGroups.has(group) || (centerId && group.has(centerId)) ||
          !movableRegion(group, residents)) anchors.add(index);
      }
      const orderedAnchors = [...anchors].sort((a, b) => a - b);
      for (let anchor = 1; anchor < orderedAnchors.length; anchor += 1) {
        if (shouldCancel?.()) break;
        const startIndex = orderedAnchors[anchor - 1];
        const endIndex = orderedAnchors[anchor];
        if (endIndex - startIndex < 2) continue;
        const start = best.positions.get(series.ids[startIndex]);
        const end = best.positions.get(series.ids[endIndex]);
        if (!start || !end) continue;
        const intervals = endIndex - startIndex;
        const span = end[series.axis] - start[series.axis];
        if (span < intervals) continue;
        const quotient = Math.floor(span / intervals);
        const remainder = span % intervals;
        const offsets = new Map<ReadonlySet<string>, number>();
        let valid = true;
        for (let index = startIndex + 1; index < endIndex; index += 1) {
          const id = series.ids[index];
          const position = best.positions.get(id);
          const group = byId.get(id);
          if (!position || !group) {
            valid = false;
            break;
          }
          const relativeIndex = index - startIndex;
          const target = start[series.axis] + relativeIndex * quotient +
            Math.floor((relativeIndex * remainder + Math.floor(intervals / 2)) / intervals);
          const offset = target - position[series.axis];
          const known = offsets.get(group);
          if (known !== undefined && known !== offset) {
            valid = false;
            break;
          }
          offsets.set(group, offset);
        }
        if (!valid || [...offsets.values()].every((offset) => offset === 0)) continue;

        const trial = new Map(best.positions);
        const changedIds = new Set<string>();
        let displacement = 0;
        let changedGroups = 0;
        for (const [group, offset] of offsets) {
          if (offset === 0) continue;
          changedGroups += 1;
          for (const id of group) {
            const position = trial.get(id);
            if (!position) continue;
            const level = immutableBounds.get(position.level);
            const translated = position[series.axis] + offset;
            const lower = series.axis === "x" ? level?.minX : level?.minY;
            const upper = series.axis === "x" ? level?.maxX : level?.maxY;
            if (lower === undefined || upper === undefined || translated < lower || translated > upper) {
              valid = false;
              break;
            }
            trial.set(id, { ...position, [series.axis]: translated });
            changedIds.add(id);
            displacement += Math.abs(offset);
          }
          if (!valid) break;
        }
        if (!valid || changedIds.size === 0 || hasCollisions(trial) ||
          !acceptedPositions(trial, acceptsPositions) ||
          reciprocalDefinitions.some((definition) =>
            !seriesRemainsCoherent(definition, trial, edges)
          ) ||
          definitions.some((definition) => !seriesRemainsCoherent(definition, trial, edges))) continue;

        const evaluated = candidate(trial, current, edges, { base: best, changedIds });
        candidatesConsidered += 1;
        publishProgress();
        const quality = candidateQuality(evaluated);
        // This is an aesthetic-only pass: unlike the planner's quality order,
        // it never trades one quality field for another.
        if (QUALITY_FIELDS.some((field) =>
          (quality[field] ?? 0) > (beforeQuality[field] ?? 0)
        )) continue;
        const afterPenalties = penalties(trial);
        if (afterPenalties.reciprocalEach.some(
          (penalty, index) => penalty > beforePenalties.reciprocalEach[index]
        ) || afterPenalties.each.some(
          (penalty, index) => penalty > beforePenalties.each[index]
        )) {
          continue;
        }
        const improvesPenalty = afterPenalties.reciprocalEach.some(
          (penalty, index) => penalty < beforePenalties.reciprocalEach[index]
        ) || afterPenalties.each.some(
          (penalty, index) => penalty < beforePenalties.each[index],
        );
        if (!improvesPenalty && compareLayoutQuality(quality, beforeQuality) === 0) continue;
        const value: SeriesSpacingCandidate = {
          candidate: evaluated,
          reciprocalPenalty: afterPenalties.reciprocal,
          totalPenalty: afterPenalties.total,
          changedGroups,
          displacement,
          key: positionMapKey(trial),
        };
        if (!selected || compare(value, selected) > 0) selected = value;
      }
    }
    if (!selected || seen.has(selected.key)) break;
    seen.add(selected.key);
    best = detachedCandidate(selected.candidate);
  }

  publishProgress(true, true);
  return best;
}

/**
 * The cheap compaction fixed point: remove globally empty rows and columns,
 * evenly space straight cardinal series, and repeat until a pass gains nothing
 * in `compareCompactionCandidates`. A pass is kept only when it ranks above
 * its start, so the result never ranks below the seed in the quality order.
 * For one request, each pass depends only on the positions it starts from, so
 * finishing a layout this returned gives it back unchanged, unless
 * cancellation or a pass that revisits an earlier layout ended the loop.
 */
function finishCompaction(
  seed: Candidate,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  edges: readonly LayoutEdge[],
  centerId: string | undefined,
  trace: IntegralLayoutRequest["trace"],
  acceptsPositions?: IntegralLayoutControl["acceptsPositions"],
  shouldCancel?: IntegralLayoutCompactionControl["shouldCancel"],
): Candidate {
  let working = seed;
  const seen = new Set([positionMapKey(seed.positions)]);
  for (;;) {
    if (shouldCancel?.()) break;
    let pass = vacuumLayout(working, current, residents, edges, trace, acceptsPositions, shouldCancel);
    // Vacuum acceptance follows the exploration order, which may trade a
    // routing or crossing regression for slack. Such a pass goes on from its
    // own start instead.
    if (compareQualityThroughFootprint(candidateQuality(pass), candidateQuality(working)) < 0) {
      pass = working;
    }
    if (!shouldCancel?.()) {
      const spaced = evenCardinalSeries(
        pass,
        current,
        residents,
        edges,
        centerId,
        trace,
        acceptsPositions,
        shouldCancel,
      );
      if (compareCompactionCandidates(spaced, pass) > 0) pass = spaced;
    }
    if (compareCompactionCandidates(pass, working) <= 0) break;
    const key = positionMapKey(pass.positions);
    if (seen.has(key)) break;
    seen.add(key);
    working = pass;
  }
  return working === seed ? seed : detachedCandidate(working);
}

/**
 * Compact one already-complete integral plan without rerunning placement or
 * topology repair. The first pass removes globally empty rows and columns;
 * the second recursively packs mutually blocking axis groups; a final vacuum
 * closes any global gap exposed by gravity; the third evenly spaces straight
 * cardinal series. Every compaction ends at the cheap fixed point of the
 * first and last passes, repeated until neither gains, which is the whole of
 * a compaction with `axisGroupCompaction: false`. The result never regresses
 * public quality, and a quality tie is returned only when the aesthetic
 * spacing strictly improves.
 */
export function compactIntegralLayoutPlan(
  request: IntegralLayoutRequest,
  seed: IntegralLayoutPlan,
  control: IntegralLayoutCompactionControl = {},
): IntegralLayoutPlan {
  if (request.allowExistingMoves === false || control.shouldCancel?.()) return seed;

  const residents = new Map(request.residents.map((resident) => [resident.id, {
    ...resident,
    position: integral(resident.position),
  }]));
  const current = new Map([...residents].map(([id, resident]) => [id, resident.position]));
  const initial = candidate(new Map(seed.positions), current, request.edges);
  // Force a fresh score at this boundary. Callers may supply a plan received
  // through structured clone or retained from an earlier heuristic phase.
  candidateQuality(initial);

  let working = initial;
  const seen = new Set([positionMapKey(initial.positions)]);
  // Full passes with axis groups run until one gains nothing.
  while (control.axisGroupCompaction !== false) {
    const passStart = working;
    let pass = vacuumLayout(
      passStart,
      current,
      residents,
      request.edges,
      request.trace,
      control.acceptsPositions,
      control.shouldCancel,
    );
    // Vacuum acceptance follows the exploration order, which may trade a
    // routing or crossing regression for slack. Compaction publishes, so a
    // pass whose protected prefix regressed restarts from its own seed.
    if (compareQualityThroughFootprint(candidateQuality(pass), candidateQuality(passStart)) < 0) {
      pass = passStart;
    }
    if (!control.shouldCancel?.()) {
      pass = axisGroupCompaction(
        pass,
        current,
        residents,
        request.edges,
        request.centerId,
        request.trace,
        control.acceptsPositions,
        control.shouldCancel,
      );
    }
    if (!control.shouldCancel?.()) {
      const vacuumed = vacuumLayout(
        pass,
        current,
        residents,
        request.edges,
        request.trace,
        control.acceptsPositions,
        control.shouldCancel,
      );
      if (compareCompactionCandidates(vacuumed, pass) > 0) pass = vacuumed;
    }
    if (!control.shouldCancel?.()) {
      const spaced = evenCardinalSeries(
        pass,
        current,
        residents,
        request.edges,
        request.centerId,
        request.trace,
        control.acceptsPositions,
        control.shouldCancel,
      );
      if (compareCompactionCandidates(spaced, pass) > 0) pass = spaced;
    }

    if (compareCompactionCandidates(pass, passStart) <= 0) break;
    const key = positionMapKey(pass.positions);
    if (seen.has(key)) break;
    seen.add(key);
    working = pass;
    if (control.shouldCancel?.()) break;

    // Canonical spacing can align several groups while leaving a newly empty
    // global line. Adopt that cheap gain immediately when present, then always
    // begin another complete pass after any strict completed improvement.
    // Axis compaction can expose a second axis basin without creating an empty
    // global line; treating a neutral vacuum probe as a fixed-point proof made
    // a second call visibly compact plans returned by the first. Strict
    // compaction ordering plus the exact seen-state set terminates the closure.
    const healed = vacuumLayout(
      working,
      current,
      residents,
      request.edges,
      request.trace,
      control.acceptsPositions,
      control.shouldCancel,
    );
    if (compareCompactionCandidates(healed, working) > 0) {
      const healedKey = positionMapKey(healed.positions);
      if (seen.has(healedKey)) break;
      seen.add(healedKey);
      working = healed;
    }
  }
  // The probe above stops on a vacuum alone, while spacing a spaced layout
  // again can still gain once its series have moved.
  if (!control.shouldCancel?.()) {
    working = finishCompaction(
      working,
      current,
      residents,
      request.edges,
      request.centerId,
      request.trace,
      control.acceptsPositions,
      control.shouldCancel,
    );
  }

  // Cancellation is transactional at this public seam. Internal phases may
  // retain a last complete candidate for their own progress, but callers that
  // cancel compaction must never receive a partially completed transaction.
  if (control.shouldCancel?.()) return seed;

  const selected = compareCompactionCandidates(working, initial) > 0 ? working : initial;
  if (selected === initial) return seed;
  const quality = refreshCandidateQuality(selected);
  return {
    positions: new Map(selected.positions),
    movedExisting: new Set(candidateMovedExisting(selected)),
    quality: { ...quality },
    constraintRepair: seed.constraintRepair,
  };
}

/**
 * Whether every room of a request is already on the map: it has no chart node
 * that is not also a resident. Such a request reflows or polishes the whole
 * map; one with a chart node of its own places new rooms.
 */
function topologyFullyResident(request: IntegralLayoutRequest): boolean {
  if (request.nodes.length === 0) return true;
  const residentIds = new Set(request.residents.map((resident) => resident.id));
  return request.nodes.every((node) => residentIds.has(node.id));
}

/**
 * Embed a player-relative NukeFire chart in an integral grid.
 *
 * Candidate layouts are published in the `compareLayoutQuality` order:
 * cardinal and vertical exits on their proper rays, links clear of rooms and
 * of each other first, weighed together, and only then footprint and link
 * slack. It may insert whole rows/columns or translate coherent regions;
 * movement count is only the final tie-breaker.
 *
 * A request that lets existing rooms move and whose chart nodes are all
 * residents reflows or polishes the whole map. It gets the whole-map
 * compaction stages, and the layouts its crossing repair publishes and the
 * plan it returns end at the cheap compaction fixed point of
 * `compactIntegralLayoutPlan` with `axisGroupCompaction: false`. The plan never
 * ranks below a layout the crossing repair published.
 */
export function planIntegralLayout(
  request: IntegralLayoutRequest,
  control: IntegralLayoutControl = {},
): IntegralLayoutPlan {
  const acceptsPositions = control.acceptsPositions ??
    ACTIVE_POSITION_ADMISSIONS[ACTIVE_POSITION_ADMISSIONS.length - 1];
  const nodes = new Map(request.nodes.map((node) => [node.id, {
    ...node,
    relative: integral(node.relative),
  }]));
  const residents = new Map(request.residents.map((resident) => [resident.id, {
    ...resident,
    position: integral(resident.position),
  }]));
  const current = new Map([...residents].map(([id, resident]) => [id, resident.position]));
  const evaluationIds = [...new Set([...current.keys(), ...nodes.keys()])].sort();
  CANDIDATE_EVALUATORS.set(current, {
    epoch: nextCandidateEvaluatorEpoch++,
    ids: evaluationIds,
    idIndexes: candidateIdIndexes(evaluationIds),
    cache: new Map(),
  });
  const blocks = coherentBlocks(current, request.edges);
  const initial = new Map(current);

  const stable = bestStablePlacement(
    initial,
    current,
    request.edges,
    nodes,
    request.centerId,
  );
  traceCandidateBatch(request.trace, "stable", [stable], acceptsPositions);
  const alternatives: Candidate[] = [];
  let golden: Candidate | undefined;
  let exactNew: Candidate[] = [];
  let chartReflow: Candidate[] = [];
  if (request.allowExistingMoves !== false) {
    golden = goldenCandidate(
      current,
      residents,
      request.edges,
      nodes,
      request.centerId,
    );
    if (golden) alternatives.push(golden);
    exactNew = exactNewCandidates(
      initial,
      current,
      residents,
      blocks,
      request.edges,
      nodes,
      request.centerId,
    );
    alternatives.push(...exactNew);
    chartReflow = reflowCandidates(
      initial,
      current,
      residents,
      blocks,
      request.edges,
      nodes,
      request.centerId,
    );
    alternatives.push(...chartReflow);
  }
  traceCandidateBatch(request.trace, "golden", [golden], acceptsPositions);
  traceCandidateBatch(request.trace, "exact-new", exactNew, acceptsPositions);
  traceCandidateBatch(request.trace, "chart-reflow", chartReflow, acceptsPositions);

  const collisionFree = [stable, ...alternatives]
    .filter((value): value is Candidate => value !== undefined &&
      acceptedPositions(value.positions, acceptsPositions) && candidateCollisions(value) === 0);
  traceCandidateBatch(request.trace, "all-candidates", [stable, ...alternatives], acceptsPositions);
  if (collisionFree.length === 0) {
    throw new Error("could not produce a collision-free integral layout");
  }
  collisionFree.sort((a, b) => compareCandidates(b, a));
  request.trace?.({
    type: "selection",
    stage: "initial-selection",
    selected: traceCandidate(collisionFree[0]),
  });
  // Cross-stage caching turns every rejected repair candidate into a strong
  // reference for the rest of the plan. Initial placement is the only phase
  // which benefits from sharing candidates produced by independent builders;
  // later repair passes already perform exact position-key deduplication.
  // Retain only the immutable seeds which can still participate in selection.
  const initialWinner = collisionFree[0];
  collisionFree.length = 1;
  collisionFree[0] = initialWinner;
  alternatives.length = 0;
  golden = undefined;
  exactNew = [];
  chartReflow = [];
  CANDIDATE_EVALUATORS.delete(current);
  // A request whose rooms are all on the map already reflows or polishes the
  // whole map: it gets the whole-map compaction stages, and every layout it
  // publishes, its plan included, ends at the cheap compaction fixed point.
  // New-room placement keeps its low-latency path without either.
  const fullyResident = topologyFullyResident(request);
  const finishesLayouts = request.allowExistingMoves !== false && fullyResident;
  const finishLayout = (value: Candidate): Candidate => finishCompaction(
    value,
    current,
    residents,
    request.edges,
    request.centerId,
    undefined,
    acceptsPositions,
  );
  const quickCrossingContext = request.allowExistingMoves === false
    ? undefined
    : crossingRepairContext(
      "quick",
      initialWinner,
      current,
      residents,
      nodes,
      request.edges,
      request.centerId,
      fullyResident,
      request.trace,
      { acceptsPositions },
      QUICK_CROSSING_WORK,
      finishesLayouts ? finishLayout : undefined,
    );
  const repairAdoptedCrossings = (value: Candidate): Candidate =>
    quickCrossingContext ? quickCrossingRepair(value, quickCrossingContext) : value;
  if (quickCrossingContext) {
    const repaired = repairAdoptedCrossings(collisionFree[0]);
    if (repaired !== collisionFree[0]) {
      collisionFree.push(repaired);
      collisionFree.sort((a, b) => compareCandidates(b, a));
    }
  }
  // At most one detached public frontier per cardinal repair seed. These are
  // kept outside the private continuation path and considered again only at
  // final public selection.
  const cardinalPublicFallbacks: Candidate[] = [];
  if (request.allowExistingMoves !== false) {
    // The initially selected seed may already have traded a large reflow for
    // fewer room obstructions. Proper-ray repair can dominate that trade, so
    // also repair the stable seed and let the full quality tuple choose. This
    // is what permits a small local branch push to beat an otherwise equally
    // clean whole-map translation.
    const repairSeed = collisionFree[0];
    const repairSeeds = [repairSeed];
    if (stable && stable !== repairSeed &&
      acceptedPositions(stable.positions, acceptsPositions) && candidateCollisions(stable) === 0 &&
      candidateRayQuality(stable).cardinalRayViolations > 0) {
      repairSeeds.push(stable);
    }
    const cardinalRepairs: Candidate[] = [];
    for (const seed of repairSeeds) {
      const repaired = greedyCardinalRepair(
        seed,
        current,
        residents,
        request.edges,
        nodes,
        request.centerId,
        evaluationIds,
        request.trace,
        acceptsPositions,
      );
      // A secondary seed is useful only when ray alignment improves the
      // primary objective. Otherwise its unrelated packing can displace the
      // established/current component merely to win on footprint.
      const endpoint = repaired?.endpoint;
      const improvesPrimary = endpoint &&
        rayQualityScore(candidateRayQuality(endpoint)) < rayQualityScore(candidateRayQuality(seed));
      if (endpoint && (seed === repairSeed || improvesPrimary)) {
        cardinalRepairs.push(endpoint);
        collisionFree.push(endpoint);
      }
      const publicBest = repaired?.publicBest;
      const publicImprovesPrimary = publicBest &&
        candidateQualityScore(publicBest) < candidateQualityScore(seed);
      if (publicBest && (seed === repairSeed ||
        (publicBest !== seed && publicImprovesPrimary))) {
        cardinalPublicFallbacks.push(publicBest);
        if (publicBest !== seed && publicBest !== endpoint) collisionFree.push(publicBest);
      }
    }
    collisionFree.sort((a, b) => compareCandidates(b, a));
    const cardinalCrossingRepair = repairAdoptedCrossings(collisionFree[0]);
    if (cardinalCrossingRepair !== collisionFree[0]) {
      collisionFree.push(cardinalCrossingRepair);
      collisionFree.sort((a, b) => compareCandidates(b, a));
    }
    // A locally repaired stable seed may still contain an unrelated room
    // obstruction that the initial winner had already cleared. Give each
    // cardinal repair the same obstruction pass before comparing them.
    const obstructionSeeds = [...new Set([collisionFree[0], ...cardinalRepairs])];
    for (const seed of obstructionSeeds) {
      const unobstructed = greedyObstructionRepair(
        seed,
        current,
        residents,
        request.edges,
        request.trace,
        acceptsPositions,
      );
      if (unobstructed) collisionFree.push(unobstructed);
    }
    collisionFree.sort((a, b) => compareCandidates(b, a));
    const obstructionCrossingRepair = repairAdoptedCrossings(collisionFree[0]);
    if (obstructionCrossingRepair !== collisionFree[0]) {
      collisionFree.push(obstructionCrossingRepair);
      collisionFree.sort((a, b) => compareCandidates(b, a));
    }
  }
  // Exploration order may rank a publicly worse candidate first, so the
  // publicly best retained candidate competes again at final selection: the
  // lobe/vacuum/gravity/spacing chain can only replace it publicly non-worse.
  const preCompaction = collisionFree[0];
  let publicFrontier = preCompaction;
  for (const value of collisionFree) {
    if (comparePublicCandidates(value, publicFrontier) > 0) publicFrontier = value;
  }
  const lobeCompactedRaw = request.allowExistingMoves === false
    ? preCompaction
    : bridgeLobeVacuum(
      preCompaction,
      current,
      residents,
      request.edges,
      request.trace,
      acceptsPositions,
    );
  const lobeCompacted = repairAdoptedCrossings(lobeCompactedRaw);
  const vacuumedRaw = request.allowExistingMoves === false
    ? lobeCompacted
    : vacuumLayout(
      lobeCompacted,
      current,
      residents,
      request.edges,
      request.trace,
      acceptsPositions,
    );
  const vacuumed = repairAdoptedCrossings(vacuumedRaw);
  // New-room placement already performs its own local repair. The group pass
  // is a final whole-map reflow/constraint-polish stage and is intentionally
  // reserved for requests whose topology is already fully resident.
  const runsAxisGroups = request.allowExistingMoves !== false && fullyResident &&
    control.axisGroupCompaction !== false;
  // The group pass can take far longer than everything before it, so a caller
  // watching the plan is shown the layout as it stands first, finished.
  const preview = runsAxisGroups && request.trace ? finishLayout(vacuumed) : undefined;
  if (preview) {
    request.trace?.({ type: "preview", stage: "axis-compaction", candidate: traceCandidate(preview) });
  }
  const axisCompactedRaw = !runsAxisGroups
    ? vacuumed
    : axisGroupCompaction(
      vacuumed,
      current,
      residents,
      request.edges,
      request.centerId,
      request.trace,
      acceptsPositions,
    );
  const axisCompacted = repairAdoptedCrossings(axisCompactedRaw);
  const finalVacuumRaw = request.allowExistingMoves === false || axisCompacted === vacuumed
    ? axisCompacted
    : vacuumLayout(
      axisCompacted,
      current,
      residents,
      request.edges,
      request.trace,
      acceptsPositions,
    );
  const finalVacuum = repairAdoptedCrossings(finalVacuumRaw);
  let selected = comparePublicCandidates(finalVacuum, axisCompacted) > 0
    ? finalVacuum
    : axisCompacted;
  let selectedNeedsFullCompaction = false;
  for (const fallback of cardinalPublicFallbacks) {
    if (comparePublicCandidates(fallback, selected) > 0) {
      selected = fallback;
      // Public fallbacks deliberately live outside the private continuation
      // path, so they have not received vacuum/gravity/spacing yet.
      selectedNeedsFullCompaction = true;
    }
  }
  if (comparePublicCandidates(publicFrontier, selected) > 0) {
    // Exploration may wander publicly downhill without finding its way back.
    // The published plan does not rest on the search recovering: a retained
    // candidate that still publicly beats every continuation wins the seam
    // here, and receives the full compaction transaction it bypassed.
    selected = publicFrontier;
    selectedNeedsFullCompaction = true;
  }
  // The quick crossing repair's best layout may already be published, and a
  // plan never ranks below a layout it published.
  if (quickCrossingContext && comparePublicCandidates(quickCrossingContext.best, selected) > 0) {
    selected = quickCrossingContext.best;
    selectedNeedsFullCompaction = true;
  }

  const compactSelected = (
    base: Candidate,
    trace: IntegralLayoutRequest["trace"] = request.trace,
  ): Candidate => {
    const seedPlan: IntegralLayoutPlan = {
      positions: base.positions,
      movedExisting: candidateMovedExisting(base),
      quality: candidateQuality(base),
    };
    const plan = compactIntegralLayoutPlan(
      trace === request.trace ? request : { ...request, trace },
      seedPlan,
      { acceptsPositions, axisGroupCompaction: control.axisGroupCompaction },
    );
    if (plan === seedPlan) return base;
    const result = candidate(new Map(plan.positions), current, request.edges);
    result.score.collisions = 0;
    result.score.quality = { ...plan.quality };
    return detachedCandidate(result);
  };

  // New-room requests retain their low-latency local placement path. A late
  // public fallback, however, must receive the entire transaction because it
  // bypassed the earlier compactor. Otherwise an immediate `nf reflow` can
  // visibly compact the plan we just returned.
  if (finishesLayouts) {
    if (selectedNeedsFullCompaction) {
      const compacted = repairAdoptedCrossings(compactSelected(selected));
      if (compareCompactionCandidates(compacted, selected) > 0) selected = compacted;
    } else {
      // Spacing is deliberately last: its canonical redistribution can be a
      // LayoutQuality tie, so running it before the public fallbacks would let
      // the movement-count tie-break silently replace the aesthetically cleaner
      // map.
      const spacedRaw = evenCardinalSeries(
        selected,
        current,
        residents,
        request.edges,
        request.centerId,
        request.trace,
        acceptsPositions,
      );
      const spaced = repairAdoptedCrossings(spacedRaw);
      if (compareCompactionCandidates(spaced, selected) > 0) selected = spaced;
    }

    // Canonical spacing can expose a globally empty line. Probe that cheap
    // condition without trace noise and pay for another complete fixed-point
    // transaction only when the probe proves that more compaction is possible.
    const postSpacingVacuum = vacuumLayout(
      selected,
      current,
      residents,
      request.edges,
      undefined,
      acceptsPositions,
    );
    if (compareCompactionCandidates(postSpacingVacuum, selected) > 0) {
      const compacted = repairAdoptedCrossings(compactSelected(selected));
      if (compareCompactionCandidates(compacted, selected) > 0) selected = compacted;
    }

    // A quick crossing transaction can be selected after the ordinary axis
    // closure. Pay for one trace-silent fixed-point compaction only when that
    // lane actually published an improvement; the common no-crossing-change
    // path avoids another expensive whole-area axis pass.
    if ((quickCrossingContext?.improvements ?? 0) > 0) {
      const postCrossingCompacted = compactSelected(selected, undefined);
      if (compareCompactionCandidates(postCrossingCompacted, selected) > 0) {
        selected = postCrossingCompacted;
      }
    }
  }
  if (quickCrossingContext && comparePublicCandidates(quickCrossingContext.best, selected) > 0) {
    selected = quickCrossingContext.best;
  }
  if (preview && comparePublicCandidates(preview, selected) > 0) selected = preview;
  // The plan ends at the same fixed point as every layout published before it.
  if (finishesLayouts) selected = finishLayout(selected);
  if (quickCrossingContext) {
    refreshCandidateQuality(quickCrossingContext.best);
    publishCrossingProgress(quickCrossingContext, "complete", true);
  }
  const finalQuality = refreshCandidateQuality(selected);
  // The prompt lane (allowExistingMoves: false) skips repair entirely, so it
  // also proposes no detours; the reflow lane that owns repair proposes them.
  const routeAmendments = request.allowExistingMoves === false
    ? undefined
    : computeRouteAmendments(
      selected.positions,
      request.edges,
      residents,
      finalQuality,
    );
  request.trace?.({
    type: "selection",
    stage: "final-selection",
    selected: traceCandidate(selected),
    ...(routeAmendments ? { routeAmendments } : {}),
  });
  return {
    positions: selected.positions,
    movedExisting: candidateMovedExisting(selected),
    quality: finalQuality,
    ...(routeAmendments ? { routeAmendments } : {}),
  };
}

/**
 * One deep crossing transaction as the repair publishes it. Raw bridge pushes
 * can leave passages of any width, which a later transaction may still need,
 * so the search keeps the raw layout and only the published copy is compacted:
 * once per strict global improvement, never per frontier state.
 */
function settleDeepCrossingLayout(
  value: Candidate,
  request: IntegralLayoutRequest,
  current: ReadonlyMap<string, GridPosition>,
  residents: ReadonlyMap<string, LayoutResident>,
  control: CrossingRepairControl,
): Candidate {
  let cancelled = false;
  const shouldCancel = (): boolean => {
    cancelled ||= control.shouldCancel?.() === true;
    return cancelled;
  };
  const seed: IntegralLayoutPlan = {
    positions: value.positions,
    movedExisting: candidateMovedExisting(value),
    quality: refreshCandidateQuality(value),
  };
  let compacted: IntegralLayoutPlan | undefined;
  try {
    compacted = compactIntegralLayoutPlan({ ...request, trace: undefined }, seed, {
      acceptsPositions: control.acceptsPositions,
      axisGroupCompaction: control.axisGroupCompaction,
      shouldCancel,
    });
  } catch {
    // Compaction is cleanup: its failure leaves the cheap fixed point below.
  }
  if (!compacted || cancelled) {
    return finishCompaction(
      value,
      current,
      residents,
      request.edges,
      request.centerId,
      undefined,
      control.acceptsPositions,
    );
  }
  if (compacted === seed) return value;
  const result = candidate(new Map(compacted.positions), current, request.edges);
  result.score.collisions = 0;
  result.score.quality = { ...compacted.quality };
  return detachedCandidate(result);
}

/**
 * Deterministic recursive crossing repair for the deep/ephemeral Worker lane.
 * The synchronous planner uses only its separately bounded quick checkpoints.
 * The search goes on from raw transactions, while each layout it publishes,
 * and so the plan it returns, is that transaction at the fixed point of a
 * trace-silent `compactIntegralLayoutPlan` under `control.acceptsPositions`:
 * a layout it streams is one compaction leaves as it is. A compaction that
 * `control.shouldCancel` cuts short leaves the cheap fixed point instead.
 */
export function repairIntegralLayoutCrossingsDeep(
  request: IntegralLayoutRequest,
  seed: IntegralLayoutPlan,
  control: CrossingRepairControl = {},
): CrossingRepairResult {
  const emptyStats: CrossingRepairStats = {
    crossingsConsidered: 0,
    macrosConsidered: 0,
    pushClosures: 0,
    maxDepth: 0,
    visitedStates: 0,
  };
  const finishWithoutSearch = (completed: boolean, cancelled: boolean): CrossingRepairResult => {
    const bestQuality = { ...seed.quality };
    request.trace?.({
      type: "crossing-progress",
      stage: "crossing-repair",
      mode: "deep",
      status: "complete",
      ...emptyStats,
      bestQuality,
    });
    control.onProgress?.({ kind: "complete", ...emptyStats, bestQuality });
    return {
      plan: seed,
      completed,
      cancelled,
      exhausted: false,
      stats: emptyStats,
    };
  };
  // This gate intentionally precedes resident maps, bridge decomposition, and
  // every search allocation: settled zero-crossing areas pay effectively zero.
  if (seed.quality.linkCrossings === 0 || request.allowExistingMoves === false) {
    return finishWithoutSearch(true, false);
  }
  if (control.shouldCancel?.()) return finishWithoutSearch(false, true);

  const nodes = new Map(request.nodes.map((node) => [node.id, {
    ...node,
    relative: integral(node.relative),
  }]));
  const residents = new Map(request.residents.map((resident) => [resident.id, {
    ...resident,
    position: integral(resident.position),
  }]));
  const current = new Map([...residents].map(([id, resident]) => [id, resident.position]));
  const seedCandidate = candidate(new Map(seed.positions), current, request.edges);
  seedCandidate.score.collisions = 0;
  seedCandidate.score.quality = { ...seed.quality };
  const context = crossingRepairContext(
    "deep",
    seedCandidate,
    current,
    residents,
    nodes,
    request.edges,
    request.centerId,
    topologyFullyResident(request),
    request.trace,
    control,
    10_000,
    (value) => settleDeepCrossingLayout(value, request, current, residents, control),
  );
  deepCrossingRepair(seedCandidate, context, 1);
  // Every layout the repair published is finished, and it returns the last.
  const result = context.published;
  const finalQuality = refreshCandidateQuality(result);
  publishCrossingProgress(context, "complete", true);

  const improved = result !== seedCandidate;
  const routeAmendments = computeRouteAmendments(
    result.positions,
    request.edges,
    residents,
    finalQuality,
  );
  const plan = improved
    ? {
      positions: result.positions,
      movedExisting: candidateMovedExisting(result),
      quality: finalQuality,
      constraintRepair: seed.constraintRepair,
      ...(routeAmendments ? { routeAmendments } : {}),
    }
    : seed;
  const solved = candidateLinkCrossings(context.best) === 0 || candidateLinkCrossings(result) === 0;
  return {
    plan,
    completed: solved || (!context.cancelled && !context.exhausted),
    cancelled: context.cancelled,
    // Reaching the exact zero-crossing objective on the final charged macro is
    // a completed proof, not a truncated search.
    exhausted: !solved && context.exhausted,
    stats: crossingStats(context),
    ...(routeAmendments ? { routeAmendments } : {}),
  };
}

/** Run the deterministic integral planner in map-layout's shared background Worker. */
export function planIntegralLayoutAsync(
  request: IntegralLayoutRequest,
  options: IntegralLayoutAsyncOptions = {},
): Promise<IntegralLayoutPlan> {
  return planIntegralLayoutOn(undefined, request, options);
}

/**
 * Plans as `planIntegralLayoutAsync` does, on Workers of its own instead of
 * the shared ones: a long search neither waits behind other planning nor
 * holds it up. `close` ends its Workers and any planning still running.
 */
export interface LayoutPlanner {
  planIntegral(
    request: IntegralLayoutRequest,
    options?: IntegralLayoutAsyncOptions,
  ): Promise<IntegralLayoutPlan>;
  close(): void;
}

/** A planner whose Workers start when it first plans and end when it closes. */
export function createLayoutPlanner(): LayoutPlanner {
  const client = createLayoutWorkerClient();
  return {
    planIntegral: (request, options = {}) => planIntegralLayoutOn(client, request, options),
    close: () => client.terminate(new Error("map-layout planner was closed")),
  };
}

/** Plans on `client`'s Workers, the shared ones when undefined, measuring the current quality if not given. */
function planIntegralLayoutOn(
  client: LayoutWorkerClient | undefined,
  request: IntegralLayoutRequest,
  options: IntegralLayoutAsyncOptions,
): Promise<IntegralLayoutPlan> {
  if (options.currentQuality) return planIntegralLayoutInWorker(request, options, client);
  const residentPositions = new Map(
    request.residents.map((resident) => [resident.id, {
      x: Math.round(resident.position.x),
      y: Math.round(resident.position.y),
      level: Math.round(resident.position.level),
    }]),
  );
  const currentEdges = request.edges.filter((edge) =>
    residentPositions.has(edge.from) && residentPositions.has(edge.to)
  );
  return planIntegralLayoutInWorker(request, {
    ...options,
    currentQuality: measureIntegralLayoutQuality(residentPositions, currentEdges),
  }, client);
}
