import type {
  ConstraintRepairOptions,
  ConstraintRepairOutcome,
  ConstraintRepairReport,
  GridPosition,
  IntegralLayoutAsyncOptions,
  IntegralLayoutPlan,
  IntegralLayoutRequest,
  LayoutQuality,
  LayoutTraceEvent,
} from "./layout.ts";
import type {
  LayoutChange,
  LayoutModel,
  LayoutPatch,
  PlannedLayout,
  PlanLayoutOptions,
} from "./model.ts";

export const LAYOUT_WORKER_PROTOCOL_VERSION = 12 as const;

/**
 * Elbow ceiling for one route amendment, shared by the engine's detour search
 * and wire validation. A detour needing more turns than this is visual noise
 * rather than a quality improvement, so the engine emits nothing instead.
 */
export const MAX_ROUTE_AMENDMENT_WAYPOINTS = 32;

/** Wire ceiling for amendments in one plan; distinct-pair checks bound the rest. */
const MAX_ROUTE_AMENDMENTS = 1_024;

export type IntegralLayoutWireRequest = Omit<IntegralLayoutRequest, "trace">;
export type IntegralLayoutWireOptions = Omit<
  IntegralLayoutAsyncOptions,
  "signal" | "timeoutMs" | "onProgress" | "currentQuality" | "plannerContext"
>;
export type PlanLayoutWireOptions = Omit<PlanLayoutOptions, "trace">;

export interface IntegralLayoutWirePlan {
  positions: readonly (readonly [string, GridPosition])[];
  movedExisting: readonly string[];
  quality: Readonly<LayoutQuality>;
  constraintRepair?: IntegralLayoutPlan["constraintRepair"];
  /** Advisory fixed-defect detours; omitted when the plan proposes none. */
  routeAmendments?: IntegralLayoutPlan["routeAmendments"];
}

/**
 * A model response ships only the patch and its scores. Both `before` and
 * `after` are deterministic functions of inputs the requesting realm already
 * holds — `before` is the normalized input snapshot, `after` is that snapshot
 * with the patch applied plus any edges the change introduces — so they are
 * rebuilt parent-side instead of cloning two whole models across the boundary.
 */
export interface PlannedLayoutWireResult {
  patch: LayoutPatch;
  positions: readonly (readonly [string, GridPosition])[];
  quality: Readonly<LayoutQuality>;
  search?: PlannedLayout["search"];
  /** Advisory fixed-defect detours; omitted when the plan proposes none. */
  routeAmendments?: PlannedLayout["routeAmendments"];
}

interface LayoutWorkerRequestBase {
  protocol: typeof LAYOUT_WORKER_PROTOCOL_VERSION;
  id: number;
  collectTrace: boolean;
  /**
   * Stream trace events back as live progress messages while the job runs.
   * When false and `collectTrace` is false, the Worker installs no trace hook
   * at all, so jobs without a consumer build and post no per-event payloads.
   */
  streamProgress: boolean;
}

export interface IntegralLayoutWorkerRequest extends LayoutWorkerRequestBase {
  operation: "integral";
  request: IntegralLayoutWireRequest;
}

export interface ConstraintRepairWorkerRequest extends LayoutWorkerRequestBase {
  operation: "constraint-repair";
  request: IntegralLayoutWireRequest;
  standard: IntegralLayoutWirePlan;
  options: ConstraintRepairOptions;
}

export interface ModelLayoutWorkerRequest extends LayoutWorkerRequestBase {
  operation: "model";
  model: LayoutModel;
  change: LayoutChange;
  options: PlanLayoutWireOptions;
}

export type LayoutWorkerRequest =
  | IntegralLayoutWorkerRequest
  | ConstraintRepairWorkerRequest
  | ModelLayoutWorkerRequest;

export interface SerializedLayoutWorkerError {
  name: string;
  message: string;
  stack?: string;
}

interface LayoutWorkerResponseBase {
  protocol: typeof LAYOUT_WORKER_PROTOCOL_VERSION;
  id: number;
  operation: LayoutWorkerRequest["operation"];
  diagnostics: LayoutWorkerExecutionDiagnostics;
}

export type LayoutWorkerTerminalReason = "completed" | "cutoff" | "timeout" | "cancelled" | "failed";

export interface LayoutWorkerMemorySample {
  heapUsed: number;
  heapTotal: number;
  external: number;
  rss?: number;
}

export interface LayoutWorkerMemoryDiagnostics {
  samples: number;
  start: LayoutWorkerMemorySample;
  end: LayoutWorkerMemorySample;
  peak: LayoutWorkerMemorySample;
}

/**
 * Small, map-free accounting returned with every terminal Worker response.
 * Counts describe work observed by the transport trace hook, not retained
 * trace payloads. `estimatedProgressBytes` deliberately avoids serializing a
 * second copy merely to measure the structured-clone traffic.
 */
export interface LayoutWorkerExecutionDiagnostics {
  terminalReason: LayoutWorkerTerminalReason;
  elapsedMs: number;
  traceEventsObserved: number;
  traceEventsRetained: number;
  retainedPositionEntries: number;
  progressMessages: number;
  progressEventsCoalesced: number;
  estimatedProgressBytes: number;
  progressPositionEntries: number;
  candidateMaterializations: number;
  inspectedStates: number;
  peakLiveSearchNodes?: number;
  repairCutoff?: ConstraintRepairReport["cutoff"];
  memory?: LayoutWorkerMemoryDiagnostics;
  stageMs?: {
    search: number;
    compaction: number;
    polish: number;
    crossing: number;
  };
}

export interface IntegralLayoutWorkerSuccess extends LayoutWorkerResponseBase {
  ok: true;
  operation: "integral";
  result: IntegralLayoutWirePlan;
  traceEvents: readonly LayoutTraceEvent[];
}

export interface ModelLayoutWorkerSuccess extends LayoutWorkerResponseBase {
  ok: true;
  operation: "model";
  result: PlannedLayoutWireResult;
  traceEvents: readonly LayoutTraceEvent[];
}

export interface ConstraintRepairWorkerSuccess extends LayoutWorkerResponseBase {
  ok: true;
  operation: "constraint-repair";
  result: IntegralLayoutWirePlan;
  traceEvents: readonly LayoutTraceEvent[];
}

export interface LayoutWorkerFailure extends LayoutWorkerResponseBase {
  ok: false;
  error: SerializedLayoutWorkerError;
  traceEvents: readonly LayoutTraceEvent[];
}

export type LayoutWorkerResponse =
  | IntegralLayoutWorkerSuccess
  | ConstraintRepairWorkerSuccess
  | ModelLayoutWorkerSuccess
  | LayoutWorkerFailure;

/**
 * Live progress exists only for the operations with a parent-side consumer:
 * integral telemetry/improvements and the constraint-repair anytime stream.
 * Model jobs deliver their bounded trace with the response instead.
 */
export interface LayoutWorkerProgressMessage {
  protocol: typeof LAYOUT_WORKER_PROTOCOL_VERSION;
  id: number;
  operation: "integral" | "constraint-repair";
  progress: true;
  event: LayoutTraceEvent;
}

export function encodeIntegralLayoutPlan(plan: IntegralLayoutPlan): IntegralLayoutWirePlan {
  const result: IntegralLayoutWirePlan = {
    positions: [...plan.positions],
    movedExisting: [...plan.movedExisting],
    quality: plan.quality,
  };
  if (plan.constraintRepair) result.constraintRepair = plan.constraintRepair;
  if (plan.routeAmendments?.length) result.routeAmendments = plan.routeAmendments;
  return result;
}

export function decodeIntegralLayoutPlan(plan: IntegralLayoutWirePlan): IntegralLayoutPlan {
  const result: IntegralLayoutPlan = {
    positions: new Map(plan.positions),
    movedExisting: new Set(plan.movedExisting),
    quality: plan.quality,
  };
  if (plan.constraintRepair) result.constraintRepair = plan.constraintRepair;
  if (plan.routeAmendments?.length) result.routeAmendments = plan.routeAmendments;
  return result;
}

export function encodePlannedLayout(plan: PlannedLayout): PlannedLayoutWireResult {
  const result: PlannedLayoutWireResult = {
    patch: plan.patch,
    positions: [...plan.positions],
    quality: plan.quality,
  };
  if (plan.search) result.search = plan.search;
  if (plan.routeAmendments?.length) result.routeAmendments = plan.routeAmendments;
  return result;
}

/** Reattach the parent-side `before`/`after` models the wire result omits. */
export function decodePlannedLayout(
  plan: PlannedLayoutWireResult,
  models: { before: LayoutModel; after: LayoutModel },
): PlannedLayout {
  const result: PlannedLayout = {
    before: models.before,
    after: models.after,
    patch: plan.patch,
    positions: new Map(plan.positions),
    quality: plan.quality,
  };
  if (plan.search) result.search = plan.search;
  if (plan.routeAmendments?.length) result.routeAmendments = plan.routeAmendments;
  return result;
}

export function serializeLayoutWorkerError(value: unknown): SerializedLayoutWorkerError {
  if (value instanceof Error) {
    return {
      name: value.name || "Error",
      message: value.message,
      stack: value.stack,
    };
  }
  return {
    name: "Error",
    message: typeof value === "string" ? value : String(value),
  };
}

export function deserializeLayoutWorkerError(value: SerializedLayoutWorkerError): Error {
  const error = new Error(value.message);
  error.name = value.name;
  if (value.stack) error.stack = value.stack;
  return error;
}

function isLayoutWorkerMemorySample(value: unknown): boolean {
  if (!value || typeof value !== "object") return false;
  const sample = value as Record<string, unknown>;
  return isNonNegativeFiniteNumber(sample.heapUsed) &&
    isNonNegativeFiniteNumber(sample.heapTotal) &&
    isNonNegativeFiniteNumber(sample.external) &&
    (sample.rss === undefined || isNonNegativeFiniteNumber(sample.rss));
}

function isLayoutWorkerExecutionDiagnostics(value: unknown): boolean {
  if (!value || typeof value !== "object") return false;
  const diagnostics = value as Record<string, unknown>;
  if (diagnostics.terminalReason !== "completed" && diagnostics.terminalReason !== "cutoff" &&
    diagnostics.terminalReason !== "timeout" &&
    diagnostics.terminalReason !== "cancelled" && diagnostics.terminalReason !== "failed") {
    return false;
  }
  if (!isNonNegativeFiniteNumber(diagnostics.elapsedMs)) return false;
  for (const field of [
    "traceEventsObserved",
    "traceEventsRetained",
    "retainedPositionEntries",
    "progressMessages",
    "progressEventsCoalesced",
    "estimatedProgressBytes",
    "progressPositionEntries",
    "candidateMaterializations",
    "inspectedStates",
  ] as const) {
    if (!isNonNegativeSafeInteger(diagnostics[field])) return false;
  }
  if (diagnostics.peakLiveSearchNodes !== undefined &&
    !isNonNegativeSafeInteger(diagnostics.peakLiveSearchNodes)) return false;
  if (diagnostics.repairCutoff !== undefined && diagnostics.repairCutoff !== "none" &&
    diagnostics.repairCutoff !== "time" && diagnostics.repairCutoff !== "restarts" &&
    diagnostics.repairCutoff !== "layouts" && diagnostics.repairCutoff !== "extensions" &&
    diagnostics.repairCutoff !== "masks") return false;
  if (diagnostics.memory !== undefined) {
    if (!diagnostics.memory || typeof diagnostics.memory !== "object") return false;
    const memory = diagnostics.memory as Record<string, unknown>;
    if (!isNonNegativeSafeInteger(memory.samples) || memory.samples < 2 ||
      !isLayoutWorkerMemorySample(memory.start) ||
      !isLayoutWorkerMemorySample(memory.end) ||
      !isLayoutWorkerMemorySample(memory.peak)) return false;
  }
  if (diagnostics.stageMs !== undefined) {
    if (!diagnostics.stageMs || typeof diagnostics.stageMs !== "object") return false;
    const stage = diagnostics.stageMs as Record<string, unknown>;
    if (!isNonNegativeFiniteNumber(stage.search) ||
      !isNonNegativeFiniteNumber(stage.compaction) ||
      !isNonNegativeFiniteNumber(stage.polish) ||
      !isNonNegativeFiniteNumber(stage.crossing)) return false;
  }
  return true;
}

export function isLayoutWorkerResponse(value: unknown): value is LayoutWorkerResponse {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (candidate.protocol !== LAYOUT_WORKER_PROTOCOL_VERSION ||
    typeof candidate.id !== "number" || !Number.isSafeInteger(candidate.id) ||
    (candidate.operation !== "integral" && candidate.operation !== "constraint-repair" &&
      candidate.operation !== "model") ||
    typeof candidate.ok !== "boolean" ||
    !isLayoutWorkerExecutionDiagnostics(candidate.diagnostics)) return false;

  if (!candidate.ok) {
    if (!Array.isArray(candidate.traceEvents) ||
      !candidate.error || typeof candidate.error !== "object") return false;
    const error = candidate.error as Record<string, unknown>;
    return typeof error.name === "string" && typeof error.message === "string" &&
      (error.stack === undefined || typeof error.stack === "string");
  }

  if (!Array.isArray(candidate.traceEvents) ||
    !candidate.result || typeof candidate.result !== "object") return false;
  const result = candidate.result as Record<string, unknown>;
  if (candidate.operation === "integral") return isIntegralLayoutWirePlan(result);
  if (candidate.operation === "constraint-repair") {
    // Every repair reports, whether or not it searched.
    return isIntegralLayoutWirePlan(result) && result.constraintRepair !== undefined;
  }
  if (!result.patch || typeof result.patch !== "object") return false;
  const patch = result.patch as Record<string, unknown>;
  return isPositionEntries(result.positions) && isLayoutQuality(result.quality) &&
    isPlannedLayoutSearch(result.search) &&
    isRouteAmendments(result.routeAmendments, result.positions) &&
    Array.isArray(patch.moves) && Array.isArray(patch.placements);
}

function isNonNegativeSafeInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value >= 0;
}

function isNonNegativeFiniteNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value) && value >= 0;
}

function isGridPosition(value: unknown): boolean {
  if (!value || typeof value !== "object") return false;
  const position = value as Record<string, unknown>;
  return typeof position.x === "number" && Number.isSafeInteger(position.x) &&
    typeof position.y === "number" && Number.isSafeInteger(position.y) &&
    typeof position.level === "number" && Number.isSafeInteger(position.level);
}

function isPositionEntries(value: unknown): value is readonly (readonly [string, GridPosition])[] {
  if (!Array.isArray(value)) return false;
  const ids = new Set<string>();
  for (const entry of value) {
    if (!Array.isArray(entry) || entry.length !== 2 || typeof entry[0] !== "string" ||
      ids.has(entry[0]) || !isGridPosition(entry[1])) return false;
    ids.add(entry[0]);
  }
  return true;
}

function isIntegralLayoutWirePlan(value: Record<string, unknown>): boolean {
  if (!isPositionEntries(value.positions) || !isLayoutQuality(value.quality) ||
    !Array.isArray(value.movedExisting)) return false;
  const positionIds = new Set(value.positions.map(([id]) => id));
  const occupied = new Set<string>();
  for (const [, position] of value.positions) {
    const cell = `${position.level}\u0000${position.x}\u0000${position.y}`;
    if (occupied.has(cell)) return false;
    occupied.add(cell);
  }
  const movedIds = new Set<string>();
  for (const id of value.movedExisting) {
    if (typeof id !== "string" || movedIds.has(id) || !positionIds.has(id)) return false;
    movedIds.add(id);
  }
  if (!isRouteAmendments(value.routeAmendments, value.positions)) return false;
  return value.constraintRepair === undefined || isConstraintRepairReport(value.constraintRepair);
}

/**
 * Validate an optional wire `routeAmendments` field against the plan's own
 * position entries: every amendment names two distinct known rooms on one
 * shared level at most once, and its elbow waypoints are integral cells on
 * that level within the shared elbow ceiling.
 */
function isRouteAmendments(
  value: unknown,
  positions: readonly (readonly [string, GridPosition])[],
): boolean {
  if (value === undefined) return true;
  if (!Array.isArray(value) || value.length === 0 || value.length > MAX_ROUTE_AMENDMENTS) {
    return false;
  }
  const levels = new Map(positions.map(([id, position]) => [id, position.level]));
  const pairs = new Set<string>();
  for (const entry of value) {
    if (!entry || typeof entry !== "object") return false;
    const amendment = entry as Record<string, unknown>;
    if (typeof amendment.from !== "string" || typeof amendment.to !== "string" ||
      amendment.from === amendment.to) return false;
    const fromLevel = levels.get(amendment.from);
    const toLevel = levels.get(amendment.to);
    if (fromLevel === undefined || toLevel === undefined || fromLevel !== toLevel) return false;
    const pair = amendment.from < amendment.to
      ? `${amendment.from}\u0000${amendment.to}`
      : `${amendment.to}\u0000${amendment.from}`;
    if (pairs.has(pair)) return false;
    pairs.add(pair);
    if (!Array.isArray(amendment.waypoints) || amendment.waypoints.length === 0 ||
      amendment.waypoints.length > MAX_ROUTE_AMENDMENT_WAYPOINTS) return false;
    for (const waypoint of amendment.waypoints) {
      if (!isGridPosition(waypoint) ||
        (waypoint as GridPosition).level !== fromLevel) return false;
    }
  }
  return true;
}

function isConsistentPolishWork(report: Record<string, unknown>): boolean {
  const passes = report.polishPasses;
  const anchors = report.polishAnchorsTried;
  const improvements = report.polishImprovements;
  const tournaments = report.polishTournaments;
  return isNonNegativeSafeInteger(passes) && isNonNegativeSafeInteger(anchors) &&
    isNonNegativeSafeInteger(improvements) && isNonNegativeSafeInteger(tournaments) &&
    anchors <= passes && improvements <= passes && tournaments <= anchors &&
    tournaments * 2 <= passes;
}

const CONSTRAINT_REPAIR_OUTCOMES: ReadonlySet<unknown> = new Set<ConstraintRepairOutcome>([
  "searched",
  "locked",
  "no-regression",
  "clean",
  "no-constraints",
  "no-budget",
  "search-failed:analysis",
  "search-failed:time",
  "search-failed:work",
  "no-layout",
]);

function isConstraintRepairReport(value: unknown): boolean {
  if (!value || typeof value !== "object") return false;
  const report = value as Record<string, unknown>;
  if (!report.crossingRepair || typeof report.crossingRepair !== "object" ||
    !report.extensionSearch || typeof report.extensionSearch !== "object" ||
    !report.maskDiversification || typeof report.maskDiversification !== "object") return false;
  const crossing = report.crossingRepair as Record<string, unknown>;
  const extension = report.extensionSearch as Record<string, unknown>;
  const masks = report.maskDiversification as Record<string, unknown>;
  const countFields = [
    "lowerBound",
    "relaxedEdges",
    "reciprocalRelaxedEdges",
    "relaxedLevelRelations",
    "standardViolations",
    "finalViolations",
    "beforeViolations",
    "standardRoutingViolations",
    "finalRoutingViolations",
    "beforeRoutingViolations",
    "beforeSettledViolations",
    "standardSettledViolations",
    "beforeSettledRoutingViolations",
    "standardSettledRoutingViolations",
    "restarts",
    "feasibilityChecks",
    "layoutsConsidered",
    "compactionAttempts",
    "polishTournaments",
    "polishPasses",
    "polishAnchorsTried",
    "polishImprovements",
    "rawIncumbents",
    "softIncumbents",
    "distinctLayouts",
    "maskDiversifications",
    "separatorStates",
    "separatorBranches",
    "separatorCyclePrunes",
  ];
  // A report that did not search selects nothing, proves nothing and never
  // polished; only a searched one has a polish cutoff.
  const searched = report.outcome === "searched";
  return (report.trigger === "settled-regression" || report.trigger === "violation-regression" ||
      report.trigger === "defects" || report.trigger === "always") &&
    CONSTRAINT_REPAIR_OUTCOMES.has(report.outcome) &&
    (searched || report.selected === false && report.constraintOptimal === false) &&
    searched === (report.polishCutoff !== "none") &&
    typeof report.selected === "boolean" && typeof report.constraintOptimal === "boolean" &&
    report.optimal === report.constraintOptimal &&
    (report.cutoff === "none" || report.cutoff === "time" || report.cutoff === "restarts" ||
      report.cutoff === "layouts" || report.cutoff === "extensions" || report.cutoff === "masks") &&
    countFields.every((field) => isNonNegativeSafeInteger(report[field])) &&
    // Every level relation gives up at least one exit.
    (report.relaxedLevelRelations as number) <= (report.relaxedEdges as number) &&
    (report.firstIncumbentMs === undefined ||
      isNonNegativeFiniteNumber(report.firstIncumbentMs)) &&
    isNonNegativeFiniteNumber(report.searchMs) &&
    isNonNegativeFiniteNumber(report.compactionMs) &&
    typeof report.geometricFixedPoint === "boolean" &&
    typeof extension.completed === "boolean" &&
    typeof extension.cancelled === "boolean" &&
    typeof extension.exhausted === "boolean" &&
    (!extension.completed || !extension.cancelled && !extension.exhausted) &&
    typeof masks.completed === "boolean" &&
    typeof masks.exhausted === "boolean" &&
    (!masks.completed || !masks.exhausted) &&
    (report.cutoff !== "extensions" || extension.exhausted) &&
    (report.cutoff !== "masks" || masks.exhausted) &&
    isConsistentPolishWork(report) &&
    (!report.geometricFixedPoint || extension.completed && extension.cancelled === false &&
      masks.completed && report.cutoff === "none" &&
      crossing.completed === true && crossing.cancelled === false && crossing.exhausted === false &&
      report.polishCutoff === "fixed-point") &&
    (report.polishCutoff === "fixed-point" || report.polishCutoff === "time" ||
      report.polishCutoff === "tournaments" || report.polishCutoff === "passes" ||
      report.polishCutoff === "error" || report.polishCutoff === "none") &&
    isNonNegativeFiniteNumber(report.polishMs) &&
    typeof crossing.completed === "boolean" &&
    typeof crossing.cancelled === "boolean" &&
    typeof crossing.exhausted === "boolean" &&
    isNonNegativeFiniteNumber(crossing.elapsedMs) && isCrossingRepairWork(crossing);
}

function isLayoutQuality(value: unknown): boolean {
  if (!value || typeof value !== "object") return false;
  const quality = value as Record<string, unknown>;
  const required = [
    "cardinalRayViolations",
    "routingViolations",
    "exitPortViolations",
    "roomObstructions",
    "linkCrossings",
    "footprintArea",
    "footprintPerimeter",
    "cardinalSlack",
  ];
  return required.every((field) => {
    const value = quality[field];
    return typeof value === "number" && Number.isFinite(value);
  }) &&
    (quality.reciprocalRayViolations === undefined ||
      typeof quality.reciprocalRayViolations === "number" &&
        Number.isFinite(quality.reciprocalRayViolations)) &&
    (quality.levelViolations === undefined ||
      typeof quality.levelViolations === "number" &&
        Number.isFinite(quality.levelViolations)) &&
    (quality.levelSlack === undefined ||
      typeof quality.levelSlack === "number" &&
        Number.isFinite(quality.levelSlack)) &&
    (quality.reciprocalExitPortViolations === undefined ||
      typeof quality.reciprocalExitPortViolations === "number" &&
        Number.isFinite(quality.reciprocalExitPortViolations));
}

function isPlannedLayoutSearch(value: unknown): boolean {
  if (value === undefined) return true;
  if (!value || typeof value !== "object") return false;
  const search = value as Record<string, unknown>;
  if (search.effort !== "thorough" || typeof search.completed !== "boolean" ||
    !Array.isArray(search.anchorsTried) || search.anchorsTried.length === 0 ||
    !isNonNegativeSafeInteger(search.planningPasses) || search.planningPasses === 0 ||
    search.planningPasses < search.anchorsTried.length ||
    search.completed && search.planningPasses < search.anchorsTried.length * 2 ||
    (search.selectedAnchor !== null && typeof search.selectedAnchor !== "string") ||
    !isLayoutQuality(search.baselineQuality)) return false;
  const anchors = new Set<string | null>();
  for (const anchor of search.anchorsTried) {
    if (anchor !== null && typeof anchor !== "string" || anchors.has(anchor)) return false;
    anchors.add(anchor);
  }
  return anchors.has(search.selectedAnchor as string | null);
}

function isTraceCandidate(value: unknown, requirePositions: boolean): boolean {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (!isLayoutQuality(candidate.quality) ||
    !Array.isArray(candidate.movedExisting) ||
    !candidate.movedExisting.every((id) => typeof id === "string") ||
    new Set(candidate.movedExisting).size !== candidate.movedExisting.length) return false;
  if (candidate.positions === undefined) return !requirePositions;
  if (!Array.isArray(candidate.positions)) return false;
  const ids = new Set<string>();
  const occupied = new Set<string>();
  for (const value of candidate.positions) {
    if (!value || typeof value !== "object") return false;
    const position = value as Record<string, unknown>;
    if (typeof position.id !== "string" || ids.has(position.id) ||
      typeof position.x !== "number" || !Number.isSafeInteger(position.x) ||
      typeof position.y !== "number" || !Number.isSafeInteger(position.y) ||
      typeof position.level !== "number" || !Number.isSafeInteger(position.level)) return false;
    const cell = `${position.level}\u0000${position.x}\u0000${position.y}`;
    if (occupied.has(cell)) return false;
    ids.add(position.id);
    occupied.add(cell);
  }
  return candidate.movedExisting.every((id) => ids.has(id));
}

const CONSTRAINT_WORK_FIELDS = [
  "rawIncumbents",
  "softIncumbents",
  "distinctLayouts",
  "maskDiversifications",
  "separatorStates",
  "separatorBranches",
  "separatorCyclePrunes",
] as const;

function isConstraintWork(value: Record<string, unknown>): boolean {
  return CONSTRAINT_WORK_FIELDS.every((field) => isNonNegativeSafeInteger(value[field])) &&
    (value.peakLiveSearchNodes === undefined ||
      isNonNegativeSafeInteger(value.peakLiveSearchNodes)) &&
    (value.candidateMaterializations === undefined ||
      isNonNegativeSafeInteger(value.candidateMaterializations)) &&
    (value.firstIncumbentMs === undefined ||
      isNonNegativeFiniteNumber(value.firstIncumbentMs));
}

function isConstraintProgressEvent(value: Record<string, unknown>): boolean {
  return value.stage === "constraint-repair" &&
    (value.phase === "search" || value.phase === "compaction" || value.phase === "polish") &&
    isNonNegativeSafeInteger(value.restarts) &&
    isNonNegativeSafeInteger(value.feasibilityChecks) &&
    isNonNegativeSafeInteger(value.layoutsConsidered) &&
    isNonNegativeSafeInteger(value.compactionAttempts) &&
    isNonNegativeFiniteNumber(value.elapsedMs) &&
    (value.bestQuality === undefined || isLayoutQuality(value.bestQuality)) &&
    isConstraintWork(value);
}

function isConstraintImprovementEvent(value: Record<string, unknown>): boolean {
  return value.stage === "constraint-repair" &&
    isNonNegativeSafeInteger(value.restarts) &&
    isNonNegativeSafeInteger(value.feasibilityChecks) &&
    isNonNegativeSafeInteger(value.layoutsConsidered) &&
    isNonNegativeSafeInteger(value.compactionAttempts) &&
    isConstraintWork(value) && isTraceCandidate(value.candidate, true);
}

function isCrossingRepairWork(value: Record<string, unknown>): boolean {
  return isNonNegativeSafeInteger(value.crossingsConsidered) &&
    isNonNegativeSafeInteger(value.macrosConsidered) &&
    isNonNegativeSafeInteger(value.pushClosures) &&
    isNonNegativeSafeInteger(value.maxDepth) &&
    isNonNegativeSafeInteger(value.visitedStates);
}

function isCrossingRepairEvent(value: Record<string, unknown>): boolean {
  return value.stage === "crossing-repair" &&
    (value.mode === "quick" || value.mode === "deep") &&
    isNonNegativeSafeInteger(value.iteration) && isCrossingRepairWork(value) &&
    isTraceCandidate(value.before, false) && isTraceCandidate(value.after, true);
}

function isCrossingProgressEvent(value: Record<string, unknown>): boolean {
  return value.stage === "crossing-repair" &&
    (value.mode === "quick" || value.mode === "deep") &&
    (value.status === "progress" || value.status === "complete") &&
    isCrossingRepairWork(value) && isLayoutQuality(value.bestQuality);
}

export function isLayoutWorkerProgress(value: unknown): value is LayoutWorkerProgressMessage {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (candidate.protocol !== LAYOUT_WORKER_PROTOCOL_VERSION ||
    typeof candidate.id !== "number" || !Number.isSafeInteger(candidate.id) ||
    (candidate.operation !== "integral" && candidate.operation !== "constraint-repair") ||
    candidate.progress !== true ||
    !candidate.event || typeof candidate.event !== "object") return false;
  const event = candidate.event as Record<string, unknown>;
  if (event.type === "crossing-repair") return isCrossingRepairEvent(event);
  if (event.type === "preview") {
    return event.stage === "axis-compaction" && isTraceCandidate(event.candidate, true);
  }
  if (event.type === "crossing-progress") return isCrossingProgressEvent(event);
  if (event.type === "constraint-progress") return isConstraintProgressEvent(event);
  if (event.type === "constraint-improvement") return isConstraintImprovementEvent(event);
  if (event.type === "constraint-repair") {
    return event.stage === "constraint-repair" && isConstraintRepairReport(event.report);
  }
  return event.type === "candidate-batch" || event.type === "selection" ||
    event.type === "improvement" || event.type === "vacuum" ||
    event.type === "obstruction-repair" || event.type === "obstruction-candidates" ||
    event.type === "bridge-vacuum" || event.type === "axis-progress";
}
