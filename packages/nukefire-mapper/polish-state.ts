/** Durable hint that an area's existing geometry may still admit improvement. */
export const AREA_POLISH_PENDING_PROPERTY = "nukefire.layout.polish-pending";

/** Value written while polish remains eligible. An empty value is the logical clear state. */
export const AREA_POLISH_PENDING_VALUE = "true";

/**
 * Durable memo beside the pending hint. The historical property name remains
 * stable, but v3 stores one resident-geometry fingerprint plus bounded,
 * versioned settlement evidence. An empty value is the logical clear state.
 */
export const AREA_POLISH_EXHAUSTED_FINGERPRINT_PROPERTY =
  "nukefire.layout.polish-exhausted-fingerprint";

/** Durable settlement schema; older values remain readable but never suppress work. */
export const AREA_POLISH_MEMO_SCHEMA_VERSION = 3;

/**
 * Bump when identical planner inputs can explore a materially different
 * deterministic search. Storage schema and effort revisions are independent.
 * Generation 2 ranks layouts by map-layout's weighted quality order, which
 * weighs a mis-levelled exit above 16 crossings, frees levels to change, and
 * compacts every layout a polish publishes, so no settlement of generation
 * 1's search suppresses it.
 */
export const AREA_POLISH_SEARCH_GENERATION = 2;

/**
 * Monotonic automatic-search effort. Increase this when the bounded automatic
 * budget is deliberately expanded; old settlements then become eligible.
 */
export const AREA_POLISH_AUTOMATIC_EFFORT = 1;

/** Stronger effort stamped only after an explicit `nf reflow perfect` result. */
export const AREA_POLISH_PERFECT_EFFORT = 2;

/** Initial durable backoff after a machine-dependent or interrupted attempt. */
export const AREA_POLISH_RETRY_COOLDOWN_MS = 15 * 60 * 1_000;

/** Backoff is exponential, but never prevents a later manual/user-triggered visit forever. */
export const MAX_AREA_POLISH_RETRY_COOLDOWN_MS = 24 * 60 * 60 * 1_000;

/** Bounded recency set of generation/effort/policy settlements for one geometry. */
export const MAX_AREA_POLISH_MEMO_CONTEXTS = 32;

/**
 * Durable list beside the pending hint: the rooms at the seams that merging
 * map sections left, which the next polish of the map previews first. Room
 * numbers as a JSON array, oldest to newest. An empty value is the logical
 * clear state.
 */
export const AREA_POLISH_SEAMS_PROPERTY = "nukefire.layout.polish-seams";

/** Seam rooms retained for one map; the newest are kept. */
export const MAX_AREA_POLISH_SEAMS = 512;

export interface AreaPolishChartNode {
  readonly id: string;
  readonly relative: { readonly x: number; readonly y: number; readonly level: number };
}

export interface AreaPolishChartEdge {
  readonly from: string;
  readonly to: string;
  readonly direction: string;
  readonly constraintVector?: {
    readonly x: number;
    readonly y: number;
    readonly level: number;
  };
}

export interface AreaPolishWorkPolicy {
  readonly when: string;
  readonly maxDurationMs?: number;
  readonly maxRestarts?: number;
  readonly maxLayouts?: number;
  readonly maxPolishTournaments?: number;
  readonly maxPolishPasses?: number;
  readonly maxExtensionStates?: number;
  readonly maxLiveSearchNodes?: number;
  readonly maxMaskDiversifications?: number;
  readonly maxCrossingWork?: number;
}

/** Geometry, diagnostic coverage, and explicit effort for one passive attempt. */
export interface AreaPolishPlanningContext {
  readonly geometryFingerprint: string;
  /** 128-bit hash of the canonical center/chart coverage, independent of effort. */
  readonly key: string;
  /** Planner behavior generation, intentionally separate from the storage schema. */
  readonly searchGeneration: number;
  /** Monotonic automatic effort achieved/requested. */
  readonly automaticEffort: number;
  /** Exact bounded policy hash; catches budget changes even before an effort bump. */
  readonly policyKey: string;
}

export type AreaPolishTerminalReason =
  | "perfect"
  | "fixed-point"
  | "ceiling"
  | "timeout"
  | "cancelled"
  | "error"
  | "incomplete";

export interface AreaPolishSettlement {
  readonly key: string;
  readonly searchGeneration: number;
  readonly automaticEffort: number;
  readonly policyKey: string;
  readonly terminalReason: AreaPolishTerminalReason;
  /** Present only for retryable outcomes. */
  readonly retryAfterMs?: number;
  /** Consecutive retryable outcomes for exponential backoff. */
  readonly attempts?: number;
}

export type AreaPolishMemo =
  | {
    readonly kind: "contexts";
    readonly geometryFingerprint: string;
    /** Oldest to newest, with deterministic oldest-first eviction. */
    readonly settlements: readonly AreaPolishSettlement[];
  }
  | {
    /** Pre-v3 values lack complete settlement evidence and are unsafe to suppress. */
    readonly kind: "legacy";
    readonly propertyValue: string;
  };

function compareCodeUnits(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function finiteOrToken(
  value: number | undefined,
): number | "infinity" | "-infinity" | "nan" | null {
  if (value === undefined) return null;
  if (Number.isNaN(value)) return "nan";
  if (value === Number.POSITIVE_INFINITY) return "infinity";
  if (value === Number.NEGATIVE_INFINITY) return "-infinity";
  return value;
}

const FNV_1A_128_OFFSET = 0x6c62272e07bb014262b821756295c58dn;
const FNV_1A_128_PRIME = 0x0000000001000000000000000000013bn;
const UINT128_MASK = (1n << 128n) - 1n;

/** Compact the potentially large canonical Map.Local chart into 128 bits. */
function contextKey(canonical: string): string {
  let hash = FNV_1A_128_OFFSET;
  for (const byte of new TextEncoder().encode(canonical)) {
    hash ^= BigInt(byte);
    hash = (hash * FNV_1A_128_PRIME) & UINT128_MASK;
  }
  return hash.toString(16).padStart(32, "0");
}

/**
 * Canonicalize a request into independent coverage and effort identities.
 * Node/edge enumeration order is irrelevant. Search generation, automatic
 * effort, and exact policy are retained as explicit settlement evidence rather
 * than being hidden inside the coverage key.
 */
export function createAreaPolishPlanningContext(input: {
  readonly geometryFingerprint: string;
  readonly centerId: string | undefined;
  readonly nodes: readonly AreaPolishChartNode[];
  readonly edges: readonly AreaPolishChartEdge[];
  readonly searchForPerfectLayouts: boolean;
  readonly policy: Readonly<AreaPolishWorkPolicy>;
  readonly searchGeneration?: number;
  readonly automaticEffort?: number;
}): AreaPolishPlanningContext {
  const nodes = input.nodes
    .map((node) => [
      node.id,
      node.relative.x,
      node.relative.y,
      node.relative.level,
    ] as const)
    .sort((a, b) => compareCodeUnits(JSON.stringify(a), JSON.stringify(b)));
  const edges = input.edges
    .map((edge) => [
      edge.from,
      edge.to,
      edge.direction,
      edge.constraintVector
        ? [edge.constraintVector.x, edge.constraintVector.y, edge.constraintVector.level]
        : null,
    ] as const)
    .sort((a, b) => compareCodeUnits(JSON.stringify(a), JSON.stringify(b)));
  const policy = input.policy;
  const canonical = JSON.stringify([
    input.centerId ?? null,
    nodes,
    edges,
    input.searchForPerfectLayouts,
  ]);
  const policyCanonical = JSON.stringify([
    policy.when,
    finiteOrToken(policy.maxDurationMs),
    finiteOrToken(policy.maxRestarts),
    finiteOrToken(policy.maxLayouts),
    finiteOrToken(policy.maxPolishTournaments),
    finiteOrToken(policy.maxPolishPasses),
    finiteOrToken(policy.maxExtensionStates),
    finiteOrToken(policy.maxLiveSearchNodes),
    finiteOrToken(policy.maxMaskDiversifications),
    finiteOrToken(policy.maxCrossingWork),
  ]);
  return {
    geometryFingerprint: input.geometryFingerprint,
    key: contextKey(canonical),
    searchGeneration: input.searchGeneration ?? AREA_POLISH_SEARCH_GENERATION,
    automaticEffort: input.automaticEffort ?? AREA_POLISH_AUTOMATIC_EFFORT,
    policyKey: contextKey(policyCanonical),
  };
}

/**
 * The composite fixed-point bit incorporates unfinished constraint, separator,
 * mask, polish, and crossing frontiers. Automatic planning runs over the live
 * resident area; its settlement is therefore attached to that final geometry,
 * while the entry/chart context remains diagnostic. The optional cutoff and
 * cancellation fields distinguish deterministic ceiling exhaustion — another
 * outcome worth settling — from wall-deadline, cancellation, and error stops,
 * and the outcome says whether the repair searched at all.
 */
export interface AreaPolishReport {
  /**
   * map-layout's repair outcome: "searched", or why the repair returned the
   * ordinary plan without searching, such as "clean" or
   * "search-failed:analysis".
   */
  readonly outcome?: string;
  readonly geometricFixedPoint: boolean;
  /** "time" marks a machine-speed wall-deadline stop; ceilings use other values. */
  readonly cutoff?: string;
  /** "time" and "error" mark polish stops that are not deterministic ceilings. */
  readonly polishCutoff?: string;
  readonly extensionSearch?: { readonly cancelled: boolean; readonly exhausted?: boolean };
  readonly crossingRepair?: { readonly cancelled: boolean; readonly exhausted?: boolean };
}

export type AreaPolishEvent =
  | {
    readonly kind: "topology-deferred";
    /** Rooms at the seams of a merge that brought this new geometry. */
    readonly seams?: readonly number[];
  }
  | { readonly kind: "polish-started" }
  | {
    readonly kind: "polish-completed";
    readonly report?: Readonly<AreaPolishReport>;
    /** Caller-observed terminal when no repair report is needed (for example, already perfect). */
    readonly terminalReason?: AreaPolishTerminalReason;
    /** True when this attempt durably changed resident geometry. */
    readonly improved?: boolean;
    /** Exact starting context; used only when completion was fruitless. */
    readonly context?: Readonly<AreaPolishPlanningContext>;
  }
  | {
    readonly kind: "polish-interrupted";
    readonly reason: "cancelled" | "error";
    /** Ordinary movement displaced the pass but did not finish an attempt. */
    readonly displacedWithinArea?: boolean;
    readonly improved?: boolean;
    readonly context?: Readonly<AreaPolishPlanningContext>;
  };

export interface AreaPolishTransition {
  /** Semantic state to retain in the area mirror. */
  readonly pending: boolean;
  /**
   * Value for `AreaMutator.setAreaProperty`, or undefined when no durable
   * write is needed. The empty string makes an existing property logically
   * clear; backends may retain that empty value in their property storage.
   */
  readonly propertyValue?: string;
}

/** Read the mapper-owned boolean property defensively across hand-edited maps. */
export function areaPolishPending(propertyValue: string | undefined): boolean {
  return propertyValue?.trim().toLowerCase() === AREA_POLISH_PENDING_VALUE;
}

/**
 * Reduce one planning observation into the durable eligibility hint.
 *
 * `polish-pending=true` remains the migration/eligibility hint. A deterministic
 * fixed point or ceiling clears that legacy bit; the v3 settlement still asks
 * the mapper to evaluate new contexts without repeating a settled one.
 */
export function reduceAreaPolishState(
  currentPending: boolean,
  event: Readonly<AreaPolishEvent>,
): AreaPolishTransition {
  const pending = event.kind === "polish-completed" && event.context
    ? !isSettledTerminalReason(event.terminalReason ?? areaPolishTerminalReason(event.report))
    : true;
  return {
    pending,
    propertyValue: pending === currentPending
      ? undefined
      : pending
      ? AREA_POLISH_PENDING_VALUE
      : "",
  };
}

/** Read the durable memo defensively across hand-edited maps; blank clears it. */
export function polishExhaustedFingerprint(
  propertyValue: string | undefined,
): string | undefined {
  const value = propertyValue?.trim();
  return value ? value : undefined;
}

function settlementIdentity(value: Readonly<AreaPolishSettlement>): string {
  // Automatic settlement is area+geometry scoped. The entry/chart key is
  // retained as diagnostic evidence, not as permission for another heavy run.
  return `${value.searchGeneration}:${value.automaticEffort}:${value.policyKey}`;
}

function boundedUniqueSettlements(
  values: readonly AreaPolishSettlement[],
): AreaPolishSettlement[] {
  const seen = new Set<string>();
  const newestFirst: AreaPolishSettlement[] = [];
  for (let index = values.length - 1; index >= 0; index -= 1) {
    const value = values[index];
    const identity = settlementIdentity(value);
    if (seen.has(identity)) continue;
    seen.add(identity);
    newestFirst.push(value);
    if (newestFirst.length >= MAX_AREA_POLISH_MEMO_CONTEXTS) break;
  }
  return newestFirst.reverse();
}

function validTerminalReason(value: unknown): value is AreaPolishTerminalReason {
  return value === "perfect" || value === "fixed-point" || value === "ceiling" || value === "timeout" ||
    value === "cancelled" || value === "error" || value === "incomplete";
}

function parseSettlement(value: unknown): AreaPolishSettlement | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value)) return undefined;
  const record = value as Record<string, unknown>;
  if (typeof record.k !== "string" || !/^[0-9a-f]{32}$/.test(record.k) ||
    typeof record.s !== "number" || !Number.isSafeInteger(record.s) || record.s < 1 ||
    typeof record.e !== "number" || !Number.isSafeInteger(record.e) || record.e < 1 ||
    typeof record.p !== "string" || !/^[0-9a-f]{32}$/.test(record.p) ||
    !validTerminalReason(record.t)) return undefined;
  if (record.r !== undefined &&
    (typeof record.r !== "number" || !Number.isFinite(record.r) || record.r < 0)) {
    return undefined;
  }
  if (record.n !== undefined &&
    (typeof record.n !== "number" || !Number.isSafeInteger(record.n) || record.n < 1)) {
    return undefined;
  }
  return {
    key: record.k,
    searchGeneration: record.s as number,
    automaticEffort: record.e as number,
    policyKey: record.p,
    terminalReason: record.t,
    ...(record.r === undefined ? {} : { retryAfterMs: record.r as number }),
    ...(record.n === undefined ? {} : { attempts: record.n as number }),
  };
}

/** Parse durable v3 evidence; v1/v2 and malformed nonblank values stay inert. */
export function areaPolishMemo(propertyValue: string | undefined): AreaPolishMemo | undefined {
  const value = polishExhaustedFingerprint(propertyValue);
  if (!value) return undefined;
  try {
    const parsed = JSON.parse(value) as unknown;
    if (parsed && typeof parsed === "object" && !Array.isArray(parsed)) {
      const record = parsed as Record<string, unknown>;
      if (record.v === AREA_POLISH_MEMO_SCHEMA_VERSION &&
        typeof record.g === "string" && record.g.length > 0 &&
        Array.isArray(record.c)) {
        const parsedSettlements = record.c.map(parseSettlement);
        if (parsedSettlements.every((entry) => entry !== undefined)) {
          const settlements = boundedUniqueSettlements(parsedSettlements as AreaPolishSettlement[]);
          if (settlements.length === 0) return { kind: "legacy", propertyValue: value };
          return {
            kind: "contexts",
            geometryFingerprint: record.g,
            settlements,
          };
        }
      }
    }
  } catch {
    // The old property was the raw resident-geometry JSON, not a wrapper.
  }
  return { kind: "legacy", propertyValue: value };
}

/** Canonical durable representation used for semantic no-op write detection. */
export function areaPolishMemoPropertyValue(memo: Readonly<AreaPolishMemo>): string {
  return memo.kind === "legacy"
    ? memo.propertyValue
    : JSON.stringify({
      v: AREA_POLISH_MEMO_SCHEMA_VERSION,
      g: memo.geometryFingerprint,
      c: memo.settlements.map((settlement) => ({
        k: settlement.key,
        s: settlement.searchGeneration,
        e: settlement.automaticEffort,
        p: settlement.policyKey,
        t: settlement.terminalReason,
        ...(settlement.retryAfterMs === undefined ? {} : { r: settlement.retryAfterMs }),
        ...(settlement.attempts === undefined ? {} : { n: settlement.attempts }),
      })),
    });
}

/**
 * How a completed attempt ended. A repair that found the ordinary plan clean
 * had nothing to improve, which is perfect, and one that could not search the
 * geometry would return it unchanged until the map changes, which settles it
 * as a fixed point does.
 */
export function areaPolishTerminalReason(
  report: Readonly<AreaPolishReport> | undefined,
): AreaPolishTerminalReason {
  if (report?.outcome === "clean") return "perfect";
  if (report?.geometricFixedPoint === true || reportsUnsearchableGeometry(report)) {
    return "fixed-point";
  }
  if (report?.cutoff === "time" || report?.polishCutoff === "time") return "timeout";
  if (report?.extensionSearch?.cancelled === true || report?.crossingRepair?.cancelled === true) {
    return "cancelled";
  }
  if (report?.polishCutoff === "error") return "error";
  if (reportsCeilingExhaustion(report)) return "ceiling";
  return report === undefined ? "error" : "incomplete";
}

export function isSettledTerminalReason(reason: AreaPolishTerminalReason): boolean {
  return reason === "perfect" || reason === "fixed-point" || reason === "ceiling";
}

/**
 * True when a completed attempt stopped only at deterministic work ceilings
 * or genuinely exhausted frontiers without reaching the geometric fixed
 * point. A wall-deadline stop depends on machine speed, and a cancellation or
 * error reflects this run rather than the topology, so none of those prove
 * that retrying the same geometry is futile.
 */
export function reportsCeilingExhaustion(
  report: Readonly<AreaPolishReport> | undefined,
): boolean {
  const deterministicCutoff = report?.cutoff === "restarts" ||
    report?.cutoff === "layouts" || report?.cutoff === "extensions" ||
    report?.cutoff === "masks" || report?.polishCutoff === "tournaments" ||
    report?.polishCutoff === "passes" ||
    report?.extensionSearch?.exhausted === true || report?.crossingRepair?.exhausted === true;
  return report !== undefined && deterministicCutoff &&
    report.geometricFixedPoint !== true &&
    report.cutoff !== "time" &&
    report.polishCutoff !== "time" &&
    report.polishCutoff !== "error" &&
    report.extensionSearch?.cancelled !== true &&
    report.crossingRepair?.cancelled !== true;
}

/**
 * Repair outcomes that return the ordinary plan without searching for a reason
 * only a change to the map removes: nothing to repair, no exit the search can
 * express, exits that contradict each other, or no layout that keeps them.
 */
const UNSEARCHABLE_GEOMETRY_OUTCOMES: ReadonlySet<string> = new Set([
  "no-regression",
  "clean",
  "no-constraints",
  "search-failed:analysis",
  "no-layout",
]);

/**
 * True when a completed attempt's repair did not search for a reason that
 * holds until the map's geometry changes. A stop that a deadline or a work
 * ceiling cut is left to the rules for those cutoffs: a deadline depends on
 * machine speed, so a later run may search. An empty budget, or a request
 * that forbids moving rooms, is never one either.
 */
export function reportsUnsearchableGeometry(
  report: Readonly<AreaPolishReport> | undefined,
): boolean {
  return report?.outcome !== undefined &&
    UNSEARCHABLE_GEOMETRY_OUTCOMES.has(report.outcome) &&
    report.cutoff === "none";
}

/**
 * Why a completed attempt's repair did not search, for the decision log:
 * undefined when it searched or the map had nothing to repair.
 */
export function polishNotSearchedReason(
  report: Readonly<AreaPolishReport> | undefined,
): string | undefined {
  const outcome = report?.outcome;
  return outcome === undefined || outcome === "searched" || outcome === "clean" ||
      outcome === "no-regression"
    ? undefined
    : outcome;
}

export interface AreaPolishMemoTransition {
  /** Bounded context set to retain in the area mirror, or undefined when clear. */
  readonly memo: AreaPolishMemo | undefined;
  /**
   * Value for `AreaMutator.setAreaProperty`, or undefined when no durable
   * write is needed. The empty string makes an existing property logically
   * clear; backends may retain that empty value in their property storage.
   */
  readonly propertyValue?: string;
}

/**
 * Reduce one planning observation into the durable exhausted-attempt memo.
 *
 * Every completed/interrupted attempt records its final resident geometry and
 * achieved automatic effort. Deterministic outcomes settle that geometry;
 * machine-dependent outcomes carry a retry cooldown. The entry/chart key is
 * diagnostic only: automatic budgets and cooldowns are area-wide. Fresh
 * prompt-lane growth clears all earlier evidence.
 */
export function reduceAreaPolishMemo(
  currentMemo: Readonly<AreaPolishMemo> | undefined,
  event: Readonly<AreaPolishEvent>,
  nowMs = Date.now(),
): AreaPolishMemoTransition {
  let memo = currentMemo as AreaPolishMemo | undefined;
  if (event.kind === "topology-deferred") {
    memo = undefined;
  } else if ((event.kind === "polish-completed" || event.kind === "polish-interrupted") &&
    event.context && !(event.kind === "polish-interrupted" && event.displacedWithinArea)) {
    const context = event.context;
    const reason = event.kind === "polish-interrupted"
      ? event.reason
      : event.terminalReason ?? areaPolishTerminalReason(event.report);
    const currentSettlements = currentMemo?.kind === "contexts" &&
        currentMemo.geometryFingerprint === context.geometryFingerprint
      ? currentMemo.settlements
      : [];
    const identity = (settlement: Readonly<AreaPolishSettlement>): boolean =>
      settlement.searchGeneration === context.searchGeneration &&
      settlement.automaticEffort === context.automaticEffort &&
      settlement.policyKey === context.policyKey;
    const previous = [...currentSettlements].reverse().find(identity);
    const attempts = isSettledTerminalReason(reason) ? undefined : (previous?.attempts ?? 0) + 1;
    const retryAfterMs = attempts === undefined
      ? undefined
      : nowMs + Math.min(
        MAX_AREA_POLISH_RETRY_COOLDOWN_MS,
        AREA_POLISH_RETRY_COOLDOWN_MS * (2 ** Math.min(10, attempts - 1)),
      );
    const settlement: AreaPolishSettlement = {
      key: context.key,
      searchGeneration: context.searchGeneration,
      automaticEffort: context.automaticEffort,
      policyKey: context.policyKey,
      terminalReason: reason,
      ...(retryAfterMs === undefined ? {} : { retryAfterMs, attempts }),
    };
    memo = {
      kind: "contexts",
      geometryFingerprint: context.geometryFingerprint,
      settlements: boundedUniqueSettlements([
        ...currentSettlements.filter((entry) => !identity(entry)),
        settlement,
      ]),
    };
  }
  const before = currentMemo && areaPolishMemoPropertyValue(currentMemo);
  const after = memo && areaPolishMemoPropertyValue(memo);
  return {
    memo,
    propertyValue: before === after ? undefined : after ?? "",
  };
}

export type AreaPolishEligibilityReason =
  | "no-evidence"
  | "legacy-evidence"
  | "geometry-changed"
  | "search-generation-changed"
  | "effort-increased"
  | "policy-changed"
  | "settled"
  | "cooldown";

export interface AreaPolishEligibility {
  readonly eligible: boolean;
  readonly reason: AreaPolishEligibilityReason;
  readonly terminalReason?: AreaPolishTerminalReason;
  readonly retryAfterMs?: number;
}

/** Derive automatic eligibility from geometry, generation, effort, policy, and outcome. */
export function areaPolishEligibility(
  memo: Readonly<AreaPolishMemo> | undefined,
  context: Readonly<AreaPolishPlanningContext>,
  nowMs = Date.now(),
): AreaPolishEligibility {
  if (!memo) return { eligible: true, reason: "no-evidence" };
  if (memo.kind === "legacy") return { eligible: true, reason: "legacy-evidence" };
  if (memo.geometryFingerprint !== context.geometryFingerprint) {
    return { eligible: true, reason: "geometry-changed" };
  }
  const generation = memo.settlements.filter(
    (entry) => entry.searchGeneration === context.searchGeneration,
  );
  if (generation.length === 0) return { eligible: true, reason: "search-generation-changed" };
  // Only a genuinely zero-defect result at stronger effort subsumes a lower
  // tier with a different anchor/lock feasible set. A stronger fixed point or
  // ceiling is retained as evidence, but cannot certify the automatic policy.
  // At equal effort, the exact policy remains part of eligibility.
  const strongerSettlement = [...generation].reverse().find((entry) =>
    entry.automaticEffort > context.automaticEffort &&
    entry.terminalReason === "perfect"
  );
  if (strongerSettlement) {
    return {
      eligible: false,
      reason: "settled",
      terminalReason: strongerSettlement.terminalReason,
    };
  }
  const effort = generation.filter(
    (entry) => entry.automaticEffort === context.automaticEffort,
  );
  if (effort.length === 0) {
    const maximumAchieved = Math.max(...generation.map((entry) => entry.automaticEffort));
    return maximumAchieved < context.automaticEffort
      ? { eligible: true, reason: "effort-increased" }
      : { eligible: true, reason: "no-evidence" };
  }
  const policy = [...effort].reverse().find((entry) => entry.policyKey === context.policyKey);
  if (!policy) return { eligible: true, reason: "policy-changed" };
  if (isSettledTerminalReason(policy.terminalReason)) {
    return { eligible: false, reason: "settled", terminalReason: policy.terminalReason };
  }
  if ((policy.retryAfterMs ?? 0) > nowMs) {
    return {
      eligible: false,
      reason: "cooldown",
      terminalReason: policy.terminalReason,
      retryAfterMs: policy.retryAfterMs,
    };
  }
  return { eligible: true, reason: "no-evidence", terminalReason: policy.terminalReason };
}

/** Suppress an achieved deterministic effort or a retryable outcome still cooling down. */
export function polishRetrySuppressed(
  memo: Readonly<AreaPolishMemo> | undefined,
  context: Readonly<AreaPolishPlanningContext>,
  nowMs = Date.now(),
): boolean {
  return !areaPolishEligibility(memo, context, nowMs).eligible;
}

/** v3 evidence must be evaluated on entry even after the legacy pending bit is cleared. */
export function areaPolishNeedsContextEvaluation(
  pending: boolean,
  memo: Readonly<AreaPolishMemo> | undefined,
): boolean {
  return pending || memo?.kind === "contexts";
}

/** Each room once, where it last appears, keeping the newest `MAX_AREA_POLISH_SEAMS`. */
function boundedSeams(values: readonly number[]): number[] {
  const seen = new Set<number>();
  const newestFirst: number[] = [];
  for (let index = values.length - 1; index >= 0; index -= 1) {
    const value = values[index];
    if (seen.has(value)) continue;
    seen.add(value);
    newestFirst.push(value);
    if (newestFirst.length >= MAX_AREA_POLISH_SEAMS) break;
  }
  return newestFirst.reverse();
}

/**
 * Read the seam list defensively across hand-edited maps: a value that is not
 * a JSON array reads as no seams, and entries that are not room numbers are
 * dropped.
 */
export function areaPolishSeams(propertyValue: string | undefined): number[] {
  const value = propertyValue?.trim();
  if (!value) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(value);
  } catch {
    return [];
  }
  if (!Array.isArray(parsed)) return [];
  return boundedSeams(parsed.filter((entry): entry is number => Number.isSafeInteger(entry)));
}

/** Canonical durable representation used for semantic no-op write detection. */
export function areaPolishSeamsPropertyValue(seams: readonly number[]): string {
  return seams.length === 0 ? "" : JSON.stringify(seams);
}

export interface AreaPolishSeamsTransition {
  /** The seam rooms to retain in the area mirror, oldest to newest. */
  readonly seams: readonly number[];
  /**
   * Value for `AreaMutator.setAreaProperty`, or undefined when no durable
   * write is needed. The empty string makes an existing property logically
   * clear.
   */
  readonly propertyValue?: string;
}

/**
 * Reduce one planning observation into the durable seam list. A merge's new
 * geometry adds its seam rooms as the newest, and a completed polish of the
 * whole map has polished every seam, so it clears the list. Every other
 * observation leaves it as it is: growth elsewhere polishes no seam, and a
 * cancelled or failed pass never reaches the reducer.
 */
export function reduceAreaPolishSeams(
  currentSeams: readonly number[],
  event: Readonly<AreaPolishEvent>,
): AreaPolishSeamsTransition {
  let seams = currentSeams;
  if (event.kind === "topology-deferred" && event.seams && event.seams.length > 0) {
    seams = boundedSeams([...currentSeams, ...event.seams]);
  } else if (event.kind === "polish-completed") {
    seams = [];
  }
  const before = areaPolishSeamsPropertyValue(currentSeams);
  const after = areaPolishSeamsPropertyValue(seams);
  return { seams, propertyValue: before === after ? undefined : after };
}

export interface AreaPolishEntryObservation {
  readonly entered: boolean;
  readonly retry: boolean;
  /** Departed area on a genuine cross-area entry; absent on startup/same-area movement. */
  readonly previousAreaKey: string | undefined;
}

/**
 * Turns a stream of current-area observations into one passive retry per
 * entry. Movement between rooms in the same area cannot repeatedly restart an
 * expensive polish; leaving and later returning makes the durable hint
 * eligible again.
 */
export class AreaPolishEntryTracker {
  #currentAreaKey: string | undefined;
  #retryAreaKey: string | undefined;

  get currentAreaKey(): string | undefined {
    return this.#currentAreaKey;
  }

  /** Current area whose one passive attempt for this visit is still unused. */
  get retryAreaKey(): string | undefined {
    return this.#retryAreaKey;
  }

  observe(
    areaKey: string,
    pending: boolean,
    polishEnabled = true,
  ): AreaPolishEntryObservation {
    const previousAreaKey = this.#currentAreaKey;
    const entered = previousAreaKey !== areaKey;
    this.#currentAreaKey = areaKey;
    if (entered) {
      this.#retryAreaKey = pending && polishEnabled ? areaKey : undefined;
    }
    return {
      entered,
      retry: this.#retryAreaKey === areaKey && entered,
      previousAreaKey: entered ? previousAreaKey : undefined,
    };
  }

  /** New topology may justify one fresh attempt even without leaving the area. */
  markPending(areaKey: string, polishEnabled = true): void {
    if (polishEnabled && this.#currentAreaKey === areaKey) {
      this.#retryAreaKey = areaKey;
    }
  }

  /** Consume the current visit's attempt before starting cancelable work. */
  consumeRetry(areaKey: string): boolean {
    if (this.#retryAreaKey !== areaKey) return false;
    this.#retryAreaKey = undefined;
    return true;
  }

  clear(): void {
    this.#currentAreaKey = undefined;
    this.#retryAreaKey = undefined;
  }
}

/**
 * Observational equivalence for cloned snapshot payloads: the same object, or
 * byte-equal JSON. Serialization order can make equal observations compare
 * unequal, which only withholds a restoration; a false equality cannot occur.
 * Cost is proportional to the snapshot payload, never to the live area.
 */
export function equivalentSnapshotPayloads(a: unknown, b: unknown): boolean {
  return a === b || JSON.stringify(a) === JSON.stringify(b);
}

/** Per-visit bookkeeping one cancelable polish pass consumed before it ran. */
export interface QuietPolishClaim {
  readonly retryConsumed: boolean;
  readonly deferredRemoved: boolean;
  /** The pass durably committed at least one progressive improvement. */
  readonly progressed?: boolean;
}

/**
 * Tracks what each quiet polish pass consumed, keyed by the snapshot the pass
 * was planning. When the displacing snapshot satisfies the injected
 * equivalence — the mapper accepts any displacement that stays within the
 * polished area — the claims are returned for restoration instead of
 * forfeiting the visit's attempt. Any other abort settles to nothing, keeping
 * the forfeit-until-reentry posture, and a completed polish discharges its
 * claim so a later pass over the same snapshot cannot resurrect a spent
 * attempt.
 */
export class QuietPolishClaims<Snapshot extends object> {
  readonly #equivalent: (aborted: Snapshot, incoming: Snapshot) => boolean;
  readonly #claims = new WeakMap<Snapshot, Map<string, QuietPolishClaim>>();

  constructor(
    equivalent: (aborted: Snapshot, incoming: Snapshot) => boolean =
      equivalentSnapshotPayloads,
  ) {
    this.#equivalent = equivalent;
  }

  /**
   * Merge one area's consumed bookkeeping into the pass's claim. A retried
   * attempt within one pass records nothing new, so the original claim stands.
   */
  record(snapshot: Snapshot, areaKey: string, claim: QuietPolishClaim): void {
    if (!claim.retryConsumed && !claim.deferredRemoved) return;
    const byArea = this.#claims.get(snapshot) ?? new Map<string, QuietPolishClaim>();
    const existing = byArea.get(areaKey);
    byArea.set(
      areaKey,
      existing
        ? {
          retryConsumed: existing.retryConsumed || claim.retryConsumed,
          deferredRemoved: existing.deferredRemoved || claim.deferredRemoved,
          ...(existing.progressed || claim.progressed ? { progressed: true } : {}),
        }
        : claim,
    );
    this.#claims.set(snapshot, byArea);
  }

  /**
   * Note one durable progressive improvement committed by the recorded pass.
   * Progress makes a later abort of the pass fruitful rather than fruitless:
   * the durable ratchet means a resumed search starts from a strictly better
   * map. Without a recorded claim there is nothing an abort could restore, so
   * there is nothing to mark.
   */
  markProgress(snapshot: Snapshot, areaKey: string): void {
    const byArea = this.#claims.get(snapshot);
    const existing = byArea?.get(areaKey);
    if (!byArea || !existing || existing.progressed) return;
    byArea.set(areaKey, { ...existing, progressed: true });
  }

  /** A completed polish genuinely spent its claim; nothing remains to restore. */
  discharge(snapshot: Snapshot, areaKey: string): void {
    const byArea = this.#claims.get(snapshot);
    if (!byArea) return;
    byArea.delete(areaKey);
    if (byArea.size === 0) this.#claims.delete(snapshot);
  }

  /**
   * Resolve an aborted pass's claims. The claims are always cleared; they are
   * returned only when the incoming snapshot satisfies the injected
   * equivalence against the one the aborted pass was planning.
   */
  settle(aborted: Snapshot, incoming: Snapshot): ReadonlyMap<string, QuietPolishClaim> {
    const byArea = this.#claims.get(aborted);
    this.#claims.delete(aborted);
    if (!byArea || !this.#equivalent(aborted, incoming)) return new Map();
    return byArea;
  }
}

/**
 * Consecutive fruitless resumptions one visit may spend per area. Each unit
 * costs a full quiet window plus a started-and-displaced Worker search, so
 * the ceiling caps a visit's abort-restart churn at a handful of wasted
 * searches while comfortably covering ordinary walk-pause rhythms; any
 * durable improvement restarts the allowance.
 */
export const MAX_FRUITLESS_QUIET_RESUMES = 8;

/**
 * Bounds how often one visit may restore a displaced quiet pass that has yet
 * to commit anything durable. Movement inside an area displaces the active
 * pass, and restoring the attempt each time keeps polish alive for an active
 * player — but an area that cannot improve would otherwise restart a full
 * search on every step forever. A displaced pass that committed a progressive
 * improvement restarts its area's allowance, as do topology growth, completed
 * polish, and area re-entry at their call sites. Once the allowance is spent
 * the visit forfeits, and the durable pending hint remains the backstop for
 * the next entry. This is the within-visit half of the churn story; the
 * exhausted-fingerprint memo separately ends cross-visit retries over
 * geometry a completed attempt proved unimprovable.
 */
export class QuietResumeBudget {
  readonly #limit: number;
  readonly #fruitless = new Map<string, number>();

  constructor(limit = MAX_FRUITLESS_QUIET_RESUMES) {
    this.#limit = limit;
  }

  /**
   * Charge one displaced pass against its area's allowance. A pass that made
   * durable progress restarts the allowance and always resumes; a fruitless
   * one consumes a unit and resumes only while units remain.
   */
  allowResume(areaKey: string, progressed: boolean): boolean {
    if (progressed) {
      this.#fruitless.delete(areaKey);
      return true;
    }
    const used = (this.#fruitless.get(areaKey) ?? 0) + 1;
    this.#fruitless.set(areaKey, used);
    return used <= this.#limit;
  }

  /** Fresh evidence for the area — growth, completion, re-entry — restarts it. */
  reset(areaKey: string): void {
    this.#fruitless.delete(areaKey);
  }

  clear(): void {
    this.#fruitless.clear();
  }
}
