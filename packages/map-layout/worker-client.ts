import { compareLayoutQuality } from "./layout.ts";
import type {
  IntegralLayoutAsyncOptions,
  IntegralLayoutPlan,
  IntegralLayoutRequest,
  LayoutTraceCandidate,
  LayoutTraceEvent,
  LayoutWorkerControlOptions,
} from "./layout.ts";
import {
  createLayoutModel,
  materializePlannedLayoutModels,
  type LayoutChange,
  type LayoutModel,
  type PlannedLayout,
  type PlanLayoutOptions,
} from "./model.ts";
import {
  decodeIntegralLayoutPlan,
  decodePlannedLayout,
  deserializeLayoutWorkerError,
  encodeIntegralLayoutPlan,
  isLayoutWorkerProgress,
  isLayoutWorkerResponse,
  LAYOUT_WORKER_PROTOCOL_VERSION,
  type ConstraintRepairWorkerSuccess,
  type IntegralLayoutWireOptions,
  type IntegralLayoutWireRequest,
  type IntegralLayoutWorkerSuccess,
  type LayoutWorkerExecutionDiagnostics,
  type LayoutWorkerRequest,
  type LayoutWorkerResponse,
  type ModelLayoutWorkerSuccess,
} from "./worker-protocol.ts";
import {
  layoutPlannerState,
  type LayoutPlannerProgress,
  type LayoutPlannerSnapshot,
  type LayoutPlannerTerminalReason,
} from "./planner-state.ts";
import { mutableLayoutPlannerState } from "./planner-state-internal.ts";

export interface LayoutWorkerLike {
  onmessage: ((event: { data: unknown }) => unknown) | null;
  onmessageerror: ((event: { data?: unknown }) => unknown) | null;
  onerror: ((event: {
    message?: string;
    error?: unknown;
    preventDefault?: () => void;
  }) => unknown) | null;
  postMessage(message: unknown): void;
  terminate(): void;
}

export type LayoutWorkerPurpose = "persistent" | "constraint-repair";
export type LayoutWorkerFactory = (purpose?: LayoutWorkerPurpose) => LayoutWorkerLike;

export type LayoutWorkerDisposition = "retained" | "retired";

export interface LayoutWorkerClientDiagnostics extends LayoutWorkerExecutionDiagnostics {
  requestId: number;
  operation: LayoutWorkerRequest["operation"];
  purpose: LayoutWorkerPurpose;
  workerDisposition: LayoutWorkerDisposition;
}

export interface LayoutWorkerClientOptions {
  /** Retire a warm, idle repair isolate after this delay. */
  repairIdleTimeoutMs?: number;
  /** Retire a completed repair isolate after it inspects this many separator states. */
  heavyRepairStateThreshold?: number;
  /** Receives map-free terminal accounting; observer failures are ignored. */
  onDiagnostics?: (diagnostics: Readonly<LayoutWorkerClientDiagnostics>) => void;
}

export type LayoutWorkerDiagnosticsSubscriber = (
  diagnostics: Readonly<LayoutWorkerClientDiagnostics>,
) => void;

export interface LayoutWorkerDiagnosticsHandle {
  readonly value: Readonly<LayoutWorkerClientDiagnostics> | undefined;
  subscribe(subscriber: LayoutWorkerDiagnosticsSubscriber): () => void;
}

class MutableLayoutWorkerDiagnostics implements LayoutWorkerDiagnosticsHandle {
  #value: Readonly<LayoutWorkerClientDiagnostics> | undefined;
  readonly #subscribers = new Set<LayoutWorkerDiagnosticsSubscriber>();

  get value(): Readonly<LayoutWorkerClientDiagnostics> | undefined {
    return this.#value;
  }

  subscribe(subscriber: LayoutWorkerDiagnosticsSubscriber): () => void {
    this.#subscribers.add(subscriber);
    if (this.#value) {
      try {
        subscriber(this.#value);
      } catch {
        // Retained observability cannot affect layout work.
      }
    }
    return () => this.#subscribers.delete(subscriber);
  }

  publish(diagnostics: LayoutWorkerClientDiagnostics): void {
    this.#value = Object.freeze({
      ...diagnostics,
      ...(diagnostics.stageMs
        ? { stageMs: Object.freeze({ ...diagnostics.stageMs }) }
        : {}),
      ...(diagnostics.memory ? { memory: Object.freeze({
        ...diagnostics.memory,
        start: Object.freeze({ ...diagnostics.memory.start }),
        end: Object.freeze({ ...diagnostics.memory.end }),
        peak: Object.freeze({ ...diagnostics.memory.peak }),
      }) } : {}),
    });
    for (const subscriber of this.#subscribers) {
      try {
        subscriber(this.#value);
      } catch {
        // Observability cannot fail or stall the scheduler.
      }
    }
  }
}

const mutableLayoutWorkerDiagnostics = new MutableLayoutWorkerDiagnostics();
/** Map-free terminal worker accounting for production diagnostics. */
export const layoutWorkerDiagnostics: LayoutWorkerDiagnosticsHandle =
  mutableLayoutWorkerDiagnostics;

export interface IntegralConstraintRepairControlOptions extends LayoutWorkerControlOptions {
  onProgress?: IntegralLayoutAsyncOptions["onProgress"];
  currentQuality?: IntegralLayoutAsyncOptions["currentQuality"];
  plannerContext?: IntegralLayoutAsyncOptions["plannerContext"];
}

type PersistentLayoutOperation = "integral" | "model";
type ScheduledState = "queued" | "active" | "settled";

interface ScheduledLayoutRequest {
  id: number;
  operation: PersistentLayoutOperation;
  message: LayoutWorkerRequest;
  trace?: (event: LayoutTraceEvent) => void;
  progress?: (event: LayoutTraceEvent) => void;
  decode(response: LayoutWorkerResponse): unknown;
  resolve(value: unknown): void;
  reject(error: unknown): void;
  signal?: AbortSignal;
  abortHandler?: () => void;
  timeout?: ReturnType<typeof setTimeout>;
  state: ScheduledState;
}

interface ScheduledConstraintRepair {
  id: number;
  request: IntegralLayoutWireRequest;
  standard: IntegralLayoutPlan;
  options: NonNullable<IntegralLayoutWireOptions["constraintRepair"]>;
  trace?: (event: LayoutTraceEvent) => void;
  progress?: (event: LayoutTraceEvent) => IntegralLayoutPlan | undefined;
  bestProgressive?: IntegralLayoutPlan;
  signal?: AbortSignal;
  abortHandler?: () => void;
  requestTimeout?: ReturnType<typeof setTimeout>;
  repairTimeout?: ReturnType<typeof setTimeout>;
  hardTimeoutMs: number;
  terminalReason?: LayoutPlannerTerminalReason;
  terminal?: (reason: LayoutPlannerTerminalReason | undefined) => void;
  state: ScheduledState;
  resolve(result: IntegralLayoutPlan): void;
  reject(reason: unknown): void;
}

/**
 * How settling a repair leaves the repair-lane Worker. `retain` keeps a small
 * completed job warm, `retire` consumes a terminal response but drops a
 * perfect/heavy isolate, and `reclaim` abandons in-flight work whose later
 * messages could collide with the next repair.
 */
type RepairWorkerDisposition = "retain" | "reclaim" | "retire";

interface LayoutWorkerConstructor {
  new (specifier: string | URL, options: { type: "module"; name?: string }): LayoutWorkerLike;
}

const DEFAULT_CONSTRAINT_REPAIR_TIMEOUT_MS = 10_000;
// The repair Worker starts its cooperative `maxDurationMs` budget only after
// it receives the request — behind isolate spawn and module compile when the
// lane's Worker is fresh — and its final bounded polish tournament may
// legitimately finish past that budget. The parent backstop therefore fires
// this grace after the budget, covering startup, the tournament overrun, and
// report transfer, so a full-budget repair still delivers its
// `constraintRepair` report. A Worker silent past the grace is treated as
// hung and reclaimed: the best validated streamed improvement (or the
// retained standard plan) resolves instead, exactly as an immediate deadline
// once did.
const CONSTRAINT_REPAIR_GRACE_FLOOR_MS = 2_000;
const CONSTRAINT_REPAIR_GRACE_RATIO = 0.25;
const MAX_TIMER_DELAY_MS = 2_147_483_647;
export const DEFAULT_REPAIR_WORKER_IDLE_TIMEOUT_MS = 15_000;
export const DEFAULT_HEAVY_REPAIR_STATE_THRESHOLD = 32_768;
let nextActivitySequence = 1;

type CrossingRepairMode = "quick" | "deep";

interface CrossingRepairWork {
  crossingsConsidered: number;
  macrosConsidered: number;
  pushClosures: number;
  maxDepth: number;
  visitedStates: number;
}

const EMPTY_CROSSING_REPAIR_WORK: Readonly<CrossingRepairWork> = Object.freeze({
  crossingsConsidered: 0,
  macrosConsidered: 0,
  pushClosures: 0,
  maxDepth: 0,
  visitedStates: 0,
});

function addCrossingRepairWork(
  total: CrossingRepairWork,
  work: CrossingRepairWork,
): CrossingRepairWork {
  return {
    crossingsConsidered: total.crossingsConsidered + work.crossingsConsidered,
    macrosConsidered: total.macrosConsidered + work.macrosConsidered,
    pushClosures: total.pushClosures + work.pushClosures,
    maxDepth: Math.max(total.maxDepth, work.maxDepth),
    visitedStates: total.visitedStates + work.visitedStates,
  };
}

class IntegralLayoutActivity {
  readonly #onProgress: ((progress: Readonly<LayoutPlannerProgress>) => void) | undefined;
  readonly #startedAt = performance.now();
  readonly #expectedIds: ReadonlySet<string>;
  readonly #residents: IntegralLayoutRequest["residents"];
  #snapshot: LayoutPlannerSnapshot;
  #standardLayouts = 0;
  #constraintLayouts = 0;
  #repairAxisLayouts = 0;
  #publishedQuality: Readonly<IntegralLayoutPlan["quality"]> | undefined;
  #axisPassLayouts = 0;
  #axisPassComplete = false;
  // Constraint polishing invokes the quick planner repeatedly, resetting its
  // cumulative counters at each `stable` batch. Retain finished runs here.
  #completedCrossingWork: CrossingRepairWork = { ...EMPTY_CROSSING_REPAIR_WORK };
  readonly #crossingRuns: Partial<Record<CrossingRepairMode, CrossingRepairWork>> = {};

  constructor(
    request: IntegralLayoutRequest,
    onProgress?: IntegralLayoutAsyncOptions["onProgress"],
    currentQuality?: IntegralLayoutAsyncOptions["currentQuality"],
    operation: "integral" | "constraint-repair" = "integral",
    plannerContext?: IntegralLayoutAsyncOptions["plannerContext"],
  ) {
    this.#onProgress = onProgress;
    this.#expectedIds = new Set([
      ...request.residents.map((resident) => resident.id),
      ...request.nodes.map((node) => node.id),
    ]);
    this.#residents = request.residents;
    this.#snapshot = {
      sequence: nextActivitySequence++,
      status: "queued",
      operation,
      phase: "queued",
      startedAt: Date.now(),
      elapsedMs: 0,
      nodes: request.nodes.length,
      residents: request.residents.length,
      edges: request.edges.length,
      ...(plannerContext ? { context: Object.freeze({ ...plannerContext }) } : {}),
      work: {
        layoutsConsidered: 0,
        compactionAttempts: 0,
        restarts: 0,
        feasibilityChecks: 0,
        rawIncumbents: 0,
        softIncumbents: 0,
        distinctLayouts: 0,
        maskDiversifications: 0,
        separatorStates: 0,
        separatorBranches: 0,
        separatorCyclePrunes: 0,
        crossingsConsidered: 0,
        macrosConsidered: 0,
        pushClosures: 0,
        maxDepth: 0,
        visitedStates: 0,
      },
      currentQuality,
    };
    this.#publish();
  }

  planning(): void {
    this.#update({ status: "planning", phase: "deterministic planning" });
  }

  event(event: LayoutTraceEvent): IntegralLayoutPlan | undefined {
    if (event.type === "candidate-batch") {
      if (event.stage === "stable") this.#finishCrossingRun("quick");
      this.#axisPassLayouts = 0;
      this.#axisPassComplete = false;
      if (this.#snapshot.status !== "repairing" && event.stage !== "all-candidates") {
        this.#snapshot = {
          ...this.#snapshot,
          work: {
            ...this.#snapshot.work,
            layoutsConsidered: this.#snapshot.work.layoutsConsidered + event.generated,
          },
        };
      }
      if (this.#snapshot.status !== "repairing") {
        this.#standardLayouts = this.#snapshot.work.layoutsConsidered;
      }
      this.#update({ phase: event.stage });
      return;
    }
    if (event.type === "constraint-progress") {
      this.#constraintLayouts = event.layoutsConsidered;
      this.#snapshot = {
        ...this.#snapshot,
        status: "repairing",
        phase: `constraint ${event.phase}`,
        firstIncumbentMs: this.#snapshot.firstIncumbentMs ?? event.firstIncumbentMs,
        work: {
          ...this.#snapshot.work,
          layoutsConsidered: this.#standardLayouts + this.#constraintLayouts +
            this.#repairAxisLayouts,
          compactionAttempts: event.compactionAttempts,
          restarts: event.restarts,
          feasibilityChecks: event.feasibilityChecks,
          ...this.#constraintWork(event),
        },
        bestQuality: this.#bestQuality(event.bestQuality),
      };
      this.#publish();
      return;
    }
    if (event.type === "constraint-improvement") {
      this.#constraintLayouts = event.layoutsConsidered;
      const improvement = this.#acceptedCandidate(event.candidate);
      this.#snapshot = {
        ...this.#snapshot,
        status: "repairing",
        phase: "new best layout",
        firstIncumbentMs: this.#snapshot.firstIncumbentMs ?? event.firstIncumbentMs,
        bestQuality: this.#bestQuality(improvement?.quality),
        work: {
          ...this.#snapshot.work,
          layoutsConsidered: this.#standardLayouts + this.#constraintLayouts +
            this.#repairAxisLayouts,
          compactionAttempts: event.compactionAttempts,
          restarts: event.restarts,
          feasibilityChecks: event.feasibilityChecks,
          ...this.#constraintWork(event),
        },
      };
      this.#publish(improvement);
      return improvement;
    }
    if (event.type === "crossing-repair") {
      const crossingWork = this.#recordCrossingWork(event.mode, {
        crossingsConsidered: event.crossingsConsidered,
        macrosConsidered: event.macrosConsidered,
        pushClosures: event.pushClosures,
        maxDepth: event.maxDepth,
        visitedStates: event.visitedStates,
      });
      const improvement = this.#acceptedCandidate(event.after);
      this.#snapshot = {
        ...this.#snapshot,
        status: event.mode === "deep" || this.#snapshot.status === "repairing"
          ? "repairing"
          : "planning",
        phase: `crossing ${event.mode} improvement`,
        work: {
          ...this.#snapshot.work,
          ...crossingWork,
        },
        bestQuality: this.#bestQuality(improvement?.quality),
      };
      this.#publish(improvement);
      return improvement;
    }
    if (event.type === "preview") {
      const improvement = this.#acceptedCandidate(event.candidate);
      this.#snapshot = {
        ...this.#snapshot,
        phase: "preview",
        bestQuality: this.#bestQuality(improvement?.quality),
      };
      this.#publish(improvement);
      return improvement;
    }
    if (event.type === "crossing-progress") {
      const crossingWork = this.#recordCrossingWork(event.mode, {
        crossingsConsidered: event.crossingsConsidered,
        macrosConsidered: event.macrosConsidered,
        pushClosures: event.pushClosures,
        maxDepth: event.maxDepth,
        visitedStates: event.visitedStates,
      });
      this.#snapshot = {
        ...this.#snapshot,
        status: event.mode === "deep" || this.#snapshot.status === "repairing"
          ? "repairing"
          : "planning",
        phase: `crossing ${event.mode} ${event.status}`,
        work: {
          ...this.#snapshot.work,
          ...crossingWork,
        },
        bestQuality: this.#bestQuality(event.bestQuality),
      };
      this.#publish();
      return;
    }
    if (event.type === "axis-progress") {
      if (this.#axisPassComplete || event.candidatesConsidered < this.#axisPassLayouts) {
        this.#axisPassLayouts = 0;
        this.#axisPassComplete = false;
      }
      const additional = event.candidatesConsidered - this.#axisPassLayouts;
      this.#axisPassLayouts = event.candidatesConsidered;
      this.#axisPassComplete = event.complete;
      if (this.#snapshot.status === "repairing") {
        this.#repairAxisLayouts += additional;
      } else {
        this.#standardLayouts += additional;
      }
      this.#snapshot = {
        ...this.#snapshot,
        phase: `axis ${event.phase}`,
        work: {
          ...this.#snapshot.work,
          layoutsConsidered: this.#standardLayouts + this.#constraintLayouts +
            this.#repairAxisLayouts,
        },
        bestQuality: this.#bestQuality(event.bestQuality),
      };
      this.#publish();
      return;
    }
    const candidate = event.type === "selection"
      ? event.selected
      : event.type === "improvement" || event.type === "vacuum" ||
          event.type === "obstruction-repair" || event.type === "bridge-vacuum"
      ? event.after
      : undefined;
    if (candidate) {
      this.#update({
        phase: event.stage,
        bestQuality: this.#bestQuality(candidate.quality),
      });
    }
  }

  standard(plan: IntegralLayoutPlan): void {
    this.#publishedQuality = plan.quality;
    this.#update({
      status: "planning",
      phase: "deterministic layout complete",
      standardQuality: plan.quality,
      bestQuality: this.#bestQuality(plan.quality),
    }, plan);
  }

  complete(plan: IntegralLayoutPlan, fallbackReason?: LayoutPlannerTerminalReason): void {
    const terminalReason = fallbackReason ?? this.#terminalReason(plan);
    this.#update({
      status: "completed",
      phase: terminalReason === "fixed-point"
        ? "complete: fixed point"
        : terminalReason === "ceiling"
        ? "complete: repair ceiling"
        : terminalReason === "timeout"
        ? "complete: repair timeout"
        : terminalReason === "degraded"
        ? "complete: standard fallback"
        : "complete",
      terminalReason,
      bestQuality: this.#bestQuality(plan.quality),
    });
  }

  fail(error: unknown): void {
    const cancelled = error instanceof Error && error.name === "AbortError";
    this.#update({
      status: cancelled ? "cancelled" : "failed",
      phase: cancelled ? "cancelled" : "failed",
      terminalReason: cancelled ? "cancelled" : "failed",
      message: error instanceof Error ? error.message : String(error),
    });
  }

  #terminalReason(plan: IntegralLayoutPlan): LayoutPlannerTerminalReason {
    const report = plan.constraintRepair;
    if (!report) return "completed";
    if (report.cutoff === "time" || report.polishCutoff === "time") return "timeout";
    if (report.extensionSearch.cancelled || report.crossingRepair.cancelled) return "cancelled";
    // A repair that did not search reports no polish; it stopped at a work
    // ceiling only when its cutoff says so.
    if (report.outcome !== "searched") return report.cutoff === "none" ? "completed" : "ceiling";
    if (report.constraintOptimal && report.geometricFixedPoint &&
      report.cutoff === "none" && report.polishCutoff === "fixed-point" &&
      report.crossingRepair.completed) return "fixed-point";
    if (report.cutoff !== "none" || report.polishCutoff !== "fixed-point" ||
      !report.crossingRepair.completed) return "ceiling";
    return "completed";
  }

  #finishCrossingRun(mode: CrossingRepairMode): void {
    const current = this.#crossingRuns[mode];
    if (!current) return;
    this.#completedCrossingWork = addCrossingRepairWork(
      this.#completedCrossingWork,
      current,
    );
    delete this.#crossingRuns[mode];
  }

  #recordCrossingWork(
    mode: CrossingRepairMode,
    checkpoint: CrossingRepairWork,
  ): CrossingRepairWork {
    const current = this.#crossingRuns[mode];
    if (current && (checkpoint.crossingsConsidered < current.crossingsConsidered ||
      checkpoint.macrosConsidered < current.macrosConsidered ||
      checkpoint.pushClosures < current.pushClosures ||
      checkpoint.maxDepth < current.maxDepth ||
      checkpoint.visitedStates < current.visitedStates)) {
      this.#finishCrossingRun(mode);
    }
    this.#crossingRuns[mode] = checkpoint;
    let total = { ...this.#completedCrossingWork };
    for (const active of Object.values(this.#crossingRuns)) {
      if (active) total = addCrossingRepairWork(total, active);
    }
    return total;
  }

  #constraintWork(event: Extract<LayoutTraceEvent, {
    type: "constraint-progress" | "constraint-improvement";
  }>): Pick<LayoutPlannerSnapshot["work"],
    | "rawIncumbents"
    | "softIncumbents"
    | "distinctLayouts"
    | "maskDiversifications"
    | "separatorStates"
    | "separatorBranches"
    | "separatorCyclePrunes"> {
    return {
      rawIncumbents: event.rawIncumbents,
      softIncumbents: event.softIncumbents,
      distinctLayouts: event.distinctLayouts,
      maskDiversifications: event.maskDiversifications,
      separatorStates: event.separatorStates,
      separatorBranches: event.separatorBranches,
      separatorCyclePrunes: event.separatorCyclePrunes,
    };
  }

  #bestQuality(
    quality: Readonly<IntegralLayoutPlan["quality"]> | undefined,
  ): Readonly<IntegralLayoutPlan["quality"]> | undefined {
    const current = this.#snapshot.bestQuality;
    if (!quality || current && compareLayoutQuality(quality, current) <= 0) return current;
    return quality;
  }

  #acceptedCandidate(candidate: LayoutTraceCandidate): IntegralLayoutPlan | undefined {
    const plan = this.#completeCandidate(candidate);
    if (!plan || this.#publishedQuality &&
      compareLayoutQuality(plan.quality, this.#publishedQuality) <= 0) return undefined;
    this.#publishedQuality = plan.quality;
    return plan;
  }

  /**
   * Complete a streamed candidate with the knowledge only this realm holds:
   * the request's exact room set and per-resident movability. Structure —
   * complete finite quality, unique ids, integral coordinates, and collision
   * freedom — is already proven at the message boundary, so only the
   * parent-side checks run here.
   */
  #completeCandidate(candidate: LayoutTraceCandidate): IntegralLayoutPlan | undefined {
    if (!candidate.positions ||
      candidate.positions.length !== this.#expectedIds.size) return undefined;
    const positions = new Map<string, { x: number; y: number; level: number }>();
    for (const position of candidate.positions) {
      if (!this.#expectedIds.has(position.id)) return undefined;
      positions.set(position.id, {
        x: position.x,
        y: position.y,
        level: position.level,
      });
    }

    const movedExisting = new Set<string>();
    for (const resident of this.#residents) {
      const position = positions.get(resident.id);
      if (!position) return undefined;
      const moved = position.x !== resident.position.x || position.y !== resident.position.y ||
        position.level !== resident.position.level;
      if (moved && !resident.movable) return undefined;
      if (moved) movedExisting.add(resident.id);
    }
    return {
      positions,
      movedExisting,
      quality: candidate.quality,
    };
  }

  #update(
    fields: Partial<Omit<LayoutPlannerSnapshot, "sequence" | "work">>,
    improvement?: IntegralLayoutPlan,
  ): void {
    this.#snapshot = {
      ...this.#snapshot,
      ...fields,
      elapsedMs: this.#snapshot.startedAt === undefined
        ? this.#snapshot.elapsedMs
        : Math.max(this.#snapshot.elapsedMs, performance.now() - this.#startedAt),
    };
    this.#publish(improvement);
  }

  #publish(improvement?: IntegralLayoutPlan): void {
    this.#snapshot = {
      ...this.#snapshot,
      elapsedMs: Math.max(this.#snapshot.elapsedMs, performance.now() - this.#startedAt),
    };
    if (this.#snapshot.sequence >= layoutPlannerState.value.sequence) {
      mutableLayoutPlannerState.publish(this.#snapshot);
    }
    try {
      this.#onProgress?.({ snapshot: this.#snapshot, improvement });
    } catch {
      // Observability is request-local and cannot fail the planner.
    }
  }
}

function defaultLayoutWorkerFactory(purpose: LayoutWorkerPurpose = "persistent"): LayoutWorkerLike {
  const constructor = (globalThis as unknown as { Worker?: LayoutWorkerConstructor }).Worker;
  if (!constructor) {
    throw new Error("map-layout requires Smudgy Worker support");
  }
  return new constructor(new URL("./worker.ts", import.meta.url), {
    type: "module",
    name: purpose === "persistent" ? "map-layout" : "map-layout-constraint-repair",
  });
}

function errorFromUnknown(value: unknown, fallback: string): Error {
  if (value instanceof Error) return value;
  if (typeof value === "string" && value) return new Error(value);
  return new Error(fallback);
}

function abortReason(signal: AbortSignal): unknown {
  const reason = (signal as AbortSignal & { readonly reason?: unknown }).reason;
  if (reason !== undefined) return reason;
  const error = new Error("map-layout planning was aborted");
  error.name = "AbortError";
  return error;
}

function timeoutError(timeoutMs: number): Error {
  const error = new Error(`map-layout planning timed out after ${timeoutMs} ms`);
  error.name = "TimeoutError";
  return error;
}

function timerDelay(timeoutMs: number): number {
  if (!Number.isFinite(timeoutMs)) return MAX_TIMER_DELAY_MS;
  return Math.min(MAX_TIMER_DELAY_MS, Math.max(0, timeoutMs));
}

function constraintRepairBackstopDelayMs(maxDurationMs: number): number {
  return maxDurationMs + Math.max(
    CONSTRAINT_REPAIR_GRACE_FLOOR_MS,
    maxDurationMs * CONSTRAINT_REPAIR_GRACE_RATIO,
  );
}

function remainingTimeout(timeoutMs: number | undefined, startedAt: number): number | undefined {
  return timeoutMs === undefined ? undefined : Math.max(0, timeoutMs - (performance.now() - startedAt));
}

/**
 * FIFO parent-side scheduler for one persistent compute Worker. Only the active
 * request crosses the boundary; queued requests remain independently
 * cancelable without disturbing the Worker or one another.
 */
export class LayoutWorkerClient {
  readonly #factory: LayoutWorkerFactory;
  readonly #repairIdleTimeoutMs: number;
  readonly #heavyRepairStateThreshold: number;
  readonly #onDiagnostics: LayoutWorkerClientOptions["onDiagnostics"];
  #worker: LayoutWorkerLike | null = null;
  #repairWorker: LayoutWorkerLike | null = null;
  #repairIdleTimeout: ReturnType<typeof setTimeout> | undefined;
  #closed = false;
  #lastDiagnostics: LayoutWorkerClientDiagnostics | undefined;
  #nextRequestId = 1;
  readonly #usedRequestIds = new Set<number>();
  readonly #queue: ScheduledLayoutRequest[] = [];
  #active: ScheduledLayoutRequest | undefined;
  readonly #repairQueue: ScheduledConstraintRepair[] = [];
  #activeRepair: ScheduledConstraintRepair | undefined;

  constructor(
    factory: LayoutWorkerFactory = defaultLayoutWorkerFactory,
    options: LayoutWorkerClientOptions = {},
  ) {
    this.#factory = factory;
    this.#repairIdleTimeoutMs = Math.max(
      0,
      options.repairIdleTimeoutMs ?? DEFAULT_REPAIR_WORKER_IDLE_TIMEOUT_MS,
    );
    this.#heavyRepairStateThreshold = Math.max(
      0,
      options.heavyRepairStateThreshold ?? DEFAULT_HEAVY_REPAIR_STATE_THRESHOLD,
    );
    this.#onDiagnostics = options.onDiagnostics;
  }

  get lastDiagnostics(): Readonly<LayoutWorkerClientDiagnostics> | undefined {
    return this.#lastDiagnostics;
  }

  planIntegral(
    request: IntegralLayoutRequest,
    options: IntegralLayoutAsyncOptions = {},
  ): Promise<IntegralLayoutPlan> {
    const startedAt = performance.now();
    const { trace, ...wireRequest } = request;
    const { signal, timeoutMs, onProgress, currentQuality, plannerContext, ...wireOptions } = options;
    const activity = new IntegralLayoutActivity(
      request,
      onProgress,
      currentQuality,
      "integral",
      plannerContext,
    );
    let terminalReason: LayoutPlannerTerminalReason | undefined;
    const cloneSafeOptions: IntegralLayoutWireOptions = wireOptions;
    const standard = this.#request(
      "integral",
      (id) => ({
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id,
        operation: "integral",
        collectTrace: !!trace,
        // Live telemetry crosses the boundary only when someone consumes it:
        // a trace sink or a progress observer. Unobserved jobs stream nothing.
        streamProgress: !!trace || !!onProgress,
        request: wireRequest,
      }),
      trace,
      (event) => activity.event(event),
      (response) => decodeIntegralLayoutPlan((response as IntegralLayoutWorkerSuccess).result),
      { signal, timeoutMs },
    );

    activity.planning();
    return standard.then((result) => {
      activity.standard(result);
      if (!cloneSafeOptions.constraintRepair) return result;
      return this.#repairIntegral(
        wireRequest,
        result,
        cloneSafeOptions.constraintRepair,
        trace,
        (event) => activity.event(event),
        {
          signal,
          timeoutMs: remainingTimeout(timeoutMs, startedAt),
        },
        (reason) => terminalReason = reason,
      );
    }).then((result) => {
      activity.complete(result, terminalReason);
      return result;
    }, (error) => {
      activity.fail(error);
      throw error;
    });
  }

  planModel(
    model: LayoutModel,
    change: LayoutChange,
    options: PlanLayoutOptions = {},
    control: LayoutWorkerControlOptions = {},
  ): Promise<PlannedLayout> {
    const { trace, ...wireOptions } = options;
    // Snapshot the planning inputs at request time. The response carries only
    // the patch; `before`/`after` are rebuilt from these exact values, so
    // later caller mutation of `model` or `change` must not be observable.
    const requestModel = createLayoutModel(model);
    const requestChange: LayoutChange = { ...change };
    return this.#request(
      "model",
      (id) => ({
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id,
        operation: "model",
        collectTrace: !!trace,
        // Model jobs have no live progress consumer; their bounded trace
        // arrives with the response instead.
        streamProgress: false,
        model: requestModel,
        change: requestChange,
        options: wireOptions,
      }),
      trace,
      undefined,
      (response) => {
        const result = (response as ModelLayoutWorkerSuccess).result;
        return decodePlannedLayout(
          result,
          materializePlannedLayoutModels(requestModel, requestChange, wireOptions, result.patch),
        );
      },
      control,
    );
  }

  /**
   * Schedule only the optional whole-layout repair stage around an already
   * validated standard plan. This is the low-level entry point used by manual
   * perfect-reflow commands without rerunning deterministic placement.
   */
  repairIntegral(
    request: IntegralLayoutRequest,
    standard: IntegralLayoutPlan,
    options: NonNullable<IntegralLayoutWireOptions["constraintRepair"]>,
    control: IntegralConstraintRepairControlOptions = {},
  ): Promise<IntegralLayoutPlan> {
    if (this.#closed) return Promise.reject(new Error("map-layout Worker client is shut down"));
    const { trace, ...wireRequest } = request;
    const { signal, timeoutMs, onProgress, currentQuality, plannerContext } = control;
    const activity = new IntegralLayoutActivity(
      request,
      onProgress,
      currentQuality,
      "constraint-repair",
      plannerContext,
    );
    activity.planning();
    activity.standard(standard);
    let terminalReason: LayoutPlannerTerminalReason | undefined;
    return this.#repairIntegral(
      wireRequest,
      standard,
      options,
      trace,
      (event) => activity.event(event),
      { signal, timeoutMs },
      (reason) => terminalReason = reason,
    ).then((result) => {
      activity.complete(result, terminalReason);
      return result;
    }, (error) => {
      activity.fail(error);
      throw error;
    });
  }

  /** Stop all current work. A later request lazily creates a fresh Worker. */
  terminate(reason: Error = new Error("map-layout Worker was terminated")): void {
    this.#clearRepairIdleTimeout();
    const worker = this.#worker;
    if (worker) this.#retirePersistentWorker(worker);
    const repairWorker = this.#repairWorker;
    if (repairWorker) this.#retireRepairWorker(repairWorker);

    const scheduled = [
      ...(this.#active ? [this.#active] : []),
      ...this.#queue.splice(0),
    ];
    this.#active = undefined;
    for (const request of scheduled) this.#finish(request, false, reason, false);
    const repairs = [
      ...(this.#activeRepair ? [this.#activeRepair] : []),
      ...this.#repairQueue.splice(0),
    ];
    this.#activeRepair = undefined;
    for (const repair of repairs) this.#finishRepair(repair, "failure", reason, false);
  }

  /** Permanently close this scheduler and reject queued/active work. */
  shutdown(reason: Error = new Error("map-layout Worker client was shut down")): void {
    if (this.#closed) return;
    this.#closed = true;
    this.terminate(reason);
  }

  #request<T>(
    operation: PersistentLayoutOperation,
    createRequest: (id: number) => LayoutWorkerRequest,
    trace: ((event: LayoutTraceEvent) => void) | undefined,
    progress: ((event: LayoutTraceEvent) => void) | undefined,
    decode: (response: LayoutWorkerResponse) => T,
    control: LayoutWorkerControlOptions,
  ): Promise<T> {
    if (this.#closed) return Promise.reject(new Error("map-layout Worker client is shut down"));
    const signal = control.signal;
    if (signal?.aborted) return Promise.reject(abortReason(signal));
    if (control.timeoutMs !== undefined && control.timeoutMs <= 0) {
      return Promise.reject(timeoutError(control.timeoutMs));
    }

    const id = this.#allocateRequestId();
    return new Promise<T>((resolve, reject) => {
      const scheduled: ScheduledLayoutRequest = {
        id,
        operation,
        message: createRequest(id),
        trace,
        progress,
        decode,
        resolve: (value) => resolve(value as T),
        reject,
        signal,
        state: "queued",
      };
      if (signal) {
        scheduled.abortHandler = () => this.#cancelScheduled(scheduled, abortReason(signal));
        signal.addEventListener("abort", scheduled.abortHandler, { once: true });
      }
      if (control.timeoutMs !== undefined) {
        const timeoutMs = control.timeoutMs;
        scheduled.timeout = setTimeout(
          () => this.#cancelScheduled(scheduled, timeoutError(timeoutMs)),
          timerDelay(timeoutMs),
        );
      }
      this.#queue.push(scheduled);
      this.#pump();
    });
  }

  #pump(): void {
    if (this.#active) return;
    const next = this.#queue.shift();
    if (!next) return;

    let worker: LayoutWorkerLike;
    try {
      worker = this.#ensurePersistentWorker();
    } catch (error) {
      this.#finish(
        next,
        false,
        errorFromUnknown(error, "could not start map-layout Worker"),
      );
      return;
    }

    next.state = "active";
    this.#active = next;
    try {
      worker.postMessage(next.message);
    } catch (error) {
      this.#failPersistentWorker(
        worker,
        errorFromUnknown(error, "could not send a request to the map-layout Worker"),
      );
    }
  }

  #ensurePersistentWorker(): LayoutWorkerLike {
    if (this.#worker) return this.#worker;
    const worker = this.#factory("persistent");
    this.#worker = worker;
    worker.onmessage = (event): void => {
      if (this.#worker !== worker) return;
      this.#handleMessage(worker, event.data);
    };
    worker.onmessageerror = (): void => {
      if (this.#worker !== worker) return;
      this.#failPersistentWorker(
        worker,
        new Error("map-layout Worker returned an uncloneable message"),
      );
    };
    worker.onerror = (event): void => {
      if (this.#worker !== worker) return;
      event.preventDefault?.();
      this.#failPersistentWorker(
        worker,
        errorFromUnknown(event.error ?? event.message, "map-layout Worker failed"),
      );
    };
    return worker;
  }

  #handleMessage(worker: LayoutWorkerLike, value: unknown): void {
    const active = this.#active;
    if (active && isLayoutWorkerProgress(value) && value.id === active.id &&
      value.operation === active.operation) {
      try {
        active.progress?.(value.event);
      } catch {
        // Observability is request-local and cannot fail the planner.
      }
      return;
    }
    if (!active || !isLayoutWorkerResponse(value) || value.id !== active.id ||
      value.operation !== active.operation) {
      this.#failPersistentWorker(
        worker,
        new Error("map-layout Worker returned an unexpected response"),
      );
      return;
    }
    this.#recordDiagnostics(value.diagnostics, value.id, value.operation, "persistent", "retained");

    if (!value.ok) {
      try {
        if (active.trace) {
          for (const event of value.traceEvents) active.trace(event);
        }
        this.#finish(active, false, deserializeLayoutWorkerError(value.error));
      } catch (error) {
        this.#finish(active, false, error);
      }
      return;
    }

    try {
      const result = active.decode(value);
      if (active.trace) {
        for (const event of value.traceEvents) active.trace(event);
      }
      this.#finish(active, true, result);
    } catch (error) {
      this.#finish(active, false, error);
    }
  }

  #cancelScheduled(request: ScheduledLayoutRequest, reason: unknown): void {
    if (request.state === "settled") return;
    if (request.state === "active") {
      const worker = this.#worker;
      if (worker) this.#retirePersistentWorker(worker);
      this.#active = undefined;
    } else {
      const index = this.#queue.indexOf(request);
      if (index >= 0) this.#queue.splice(index, 1);
    }
    this.#finish(request, false, reason);
  }

  #failPersistentWorker(worker: LayoutWorkerLike, reason: unknown): void {
    if (this.#worker !== worker) return;
    this.#retirePersistentWorker(worker);
    const active = this.#active;
    this.#active = undefined;
    if (active) this.#finish(active, false, reason);
    else this.#pump();
  }

  #retirePersistentWorker(worker: LayoutWorkerLike): void {
    if (this.#worker !== worker) return;
    this.#worker = null;
    worker.onmessage = null;
    worker.onmessageerror = null;
    worker.onerror = null;
    try {
      worker.terminate();
    } catch {
      // A failed Worker is already unusable; the scheduler still advances.
    }
  }

  #recordDiagnostics(
    diagnostics: LayoutWorkerExecutionDiagnostics,
    requestId: number,
    operation: LayoutWorkerRequest["operation"],
    purpose: LayoutWorkerPurpose,
    workerDisposition: LayoutWorkerDisposition,
  ): void {
    const value: LayoutWorkerClientDiagnostics = {
      ...diagnostics,
      requestId,
      operation,
      purpose,
      workerDisposition,
    };
    this.#lastDiagnostics = value;
    mutableLayoutWorkerDiagnostics.publish(value);
    try {
      this.#onDiagnostics?.(value);
    } catch {
      // Observability cannot fail or stall the scheduler.
    }
  }

  #finish(
    request: ScheduledLayoutRequest,
    succeeded: boolean,
    value: unknown,
    pump = true,
  ): void {
    if (request.state === "settled") return;
    if (this.#active === request) this.#active = undefined;
    request.state = "settled";
    if (request.abortHandler && request.signal) {
      request.signal.removeEventListener("abort", request.abortHandler);
    }
    if (request.timeout !== undefined) clearTimeout(request.timeout);
    this.#usedRequestIds.delete(request.id);

    // Post the next FIFO request before resolving this one. This keeps the
    // persistent Worker continuously occupied without allowing parallel work.
    if (pump) this.#pump();
    if (succeeded) request.resolve(value);
    else request.reject(value);
  }

  #repairIntegral(
    request: IntegralLayoutWireRequest,
    standard: IntegralLayoutPlan,
    options: NonNullable<IntegralLayoutWireOptions["constraintRepair"]>,
    trace: ((event: LayoutTraceEvent) => void) | undefined,
    progress: ((event: LayoutTraceEvent) => IntegralLayoutPlan | undefined) | undefined,
    control: LayoutWorkerControlOptions,
    terminal?: (reason: LayoutPlannerTerminalReason | undefined) => void,
  ): Promise<IntegralLayoutPlan> {
    if (this.#closed) return Promise.reject(new Error("map-layout Worker client is shut down"));
    const signal = control.signal;
    if (signal?.aborted) return Promise.reject(abortReason(signal));
    // Repair is optional polish over an already-delivered standard plan. A
    // caller deadline exhausted at or after this point adopts the repair
    // lane's own deadline posture: the retained plan resolves rather than
    // rejecting completed work. Explicit aborts still reject above.
    if (control.timeoutMs !== undefined && control.timeoutMs <= 0) {
      terminal?.("timeout");
      return Promise.resolve(standard);
    }
    // A budget that is not positive still reaches the Worker, which reports
    // "no-budget" the way it reports every other repair it does not search;
    // its backstop keeps the full grace.
    const hardTimeoutMs = Math.max(
      0,
      options.maxDurationMs ?? DEFAULT_CONSTRAINT_REPAIR_TIMEOUT_MS,
    );

    const id = this.#allocateRequestId();
    return new Promise<IntegralLayoutPlan>((resolve, reject) => {
      const scheduled: ScheduledConstraintRepair = {
        id,
        request,
        standard,
        options,
        trace,
        progress,
        signal,
        hardTimeoutMs,
        terminal,
        state: "queued",
        resolve,
        reject,
      };
      if (signal) {
        scheduled.abortHandler = () => this.#cancelRepair(scheduled, abortReason(signal));
        signal.addEventListener("abort", scheduled.abortHandler, { once: true });
      }
      if (control.timeoutMs !== undefined) {
        // Caller-deadline expiry mid-repair keeps already-computed work: the
        // best validated streamed improvement or the retained standard plan.
        scheduled.requestTimeout = setTimeout(
          () => {
            scheduled.terminalReason = "timeout";
            this.#finishRepair(scheduled, "standard");
          },
          timerDelay(control.timeoutMs),
        );
      }
      this.#repairQueue.push(scheduled);
      this.#pumpRepair();
    });
  }

  #pumpRepair(): void {
    if (this.#activeRepair) return;
    const repair = this.#repairQueue.shift();
    if (!repair) {
      this.#scheduleRepairIdleRetirement();
      return;
    }

    let worker: LayoutWorkerLike;
    try {
      worker = this.#ensureRepairWorker();
    } catch {
      repair.terminalReason = "degraded";
      this.#finishRepair(repair, "standard");
      return;
    }

    repair.state = "active";
    this.#activeRepair = repair;
    if (Number.isFinite(repair.hardTimeoutMs)) {
      repair.repairTimeout = setTimeout(
        () => {
          repair.terminalReason = "timeout";
          this.#finishRepair(repair, "standard");
        },
        timerDelay(constraintRepairBackstopDelayMs(repair.hardTimeoutMs)),
      );
    }
    try {
      worker.postMessage({
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id: repair.id,
        operation: "constraint-repair",
        collectTrace: !!repair.trace,
        // Anytime repair results flow through streamed strict improvements,
        // so repair jobs always stream regardless of parent-side observers.
        streamProgress: true,
        request: repair.request,
        standard: encodeIntegralLayoutPlan(repair.standard),
        options: repair.options,
      } satisfies LayoutWorkerRequest);
    } catch {
      repair.terminalReason = "degraded";
      this.#finishRepair(repair, "standard");
    }
  }

  /**
   * The repair lane shares one persistent Worker across sequential repairs,
   * paying isolate spawn, module compile, and JIT warmup once instead of per
   * job. It is reclaimed under exactly the discipline the ordinary lane uses:
   * abandoned in-flight work or malformed traffic terminates it, and the next
   * repair starts a fresh Worker.
   */
  #ensureRepairWorker(): LayoutWorkerLike {
    this.#clearRepairIdleTimeout();
    if (this.#repairWorker) return this.#repairWorker;
    const worker = this.#factory("constraint-repair");
    this.#repairWorker = worker;
    worker.onmessage = (event): void => {
      if (this.#repairWorker !== worker) return;
      this.#handleRepairMessage(worker, event.data);
    };
    worker.onmessageerror = (): void => {
      if (this.#repairWorker !== worker) return;
      this.#failRepairWorker(worker);
    };
    worker.onerror = (event): void => {
      if (this.#repairWorker !== worker) return;
      event.preventDefault?.();
      this.#failRepairWorker(worker);
    };
    return worker;
  }

  #handleRepairMessage(worker: LayoutWorkerLike, value: unknown): void {
    const repair = this.#activeRepair;
    if (repair && isLayoutWorkerProgress(value) && value.id === repair.id &&
      value.operation === "constraint-repair") {
      let improvement: IntegralLayoutPlan | undefined;
      try {
        improvement = repair.progress?.(value.event);
      } catch {
        // Observability is request-local and cannot fail the planner.
      }
      if (improvement && (!repair.bestProgressive ||
        compareLayoutQuality(improvement.quality, repair.bestProgressive.quality) > 0)) {
        repair.bestProgressive = improvement;
      }
      return;
    }
    if (!repair || !isLayoutWorkerResponse(value) || value.id !== repair.id ||
      value.operation !== "constraint-repair") {
      // Unattributable traffic means the Worker and this scheduler disagree
      // about what is running; reclaim it exactly as the ordinary lane does.
      this.#failRepairWorker(worker);
      return;
    }
    const disposition = this.#repairDisposition(repair, value.diagnostics);
    this.#recordDiagnostics(
      value.diagnostics,
      value.id,
      value.operation,
      "constraint-repair",
      disposition === "retain" ? "retained" : "retired",
    );
    if (!value.ok) {
      // The Worker caught and serialized a repair failure itself; it is idle
      // and healthy; ordinary jobs may stay warm, while perfect/heavy jobs are
      // retired to release the isolate's high-water heap.
      repair.terminalReason = "degraded";
      this.#finishRepair(repair, "standard", undefined, true, disposition);
      return;
    }
    let repaired: IntegralLayoutPlan;
    try {
      repaired = decodeIntegralLayoutPlan(
        (value as ConstraintRepairWorkerSuccess).result,
      );
    } catch {
      repair.terminalReason = "degraded";
      this.#finishRepair(repair, "standard", undefined, true, "retire");
      return;
    }
    try {
      if (repair.trace) {
        for (const event of value.traceEvents) repair.trace(event);
      }
    } catch (error) {
      this.#finishRepair(repair, "failure", error, true, disposition);
      return;
    }
    this.#finishRepair(
      repair,
      "success",
      repaired.constraintRepair ? repaired : repair.standard,
      true,
      disposition,
    );
  }

  #repairDisposition(
    repair: ScheduledConstraintRepair,
    diagnostics: Readonly<LayoutWorkerExecutionDiagnostics>,
  ): RepairWorkerDisposition {
    const deterministicLimits = [
      repair.hardTimeoutMs,
      repair.options.maxRestarts,
      repair.options.maxLayouts,
      repair.options.maxPolishTournaments,
      repair.options.maxPolishPasses,
      repair.options.maxExtensionStates,
      repair.options.maxLiveSearchNodes,
      repair.options.maxMaskDiversifications,
      repair.options.maxCrossingWork,
    ];
    const perfect = deterministicLimits.some((limit) => limit !== undefined &&
      !Number.isFinite(limit));
    return perfect || diagnostics.inspectedStates >= this.#heavyRepairStateThreshold
      ? "retire"
      : "retain";
  }

  #failRepairWorker(worker: LayoutWorkerLike): void {
    if (this.#repairWorker !== worker) return;
    this.#retireRepairWorker(worker);
    const active = this.#activeRepair;
    if (active) {
      active.terminalReason = "degraded";
      this.#finishRepair(active, "standard");
    }
    else this.#pumpRepair();
  }

  #retireRepairWorker(worker: LayoutWorkerLike): void {
    if (this.#repairWorker !== worker) return;
    this.#clearRepairIdleTimeout();
    this.#repairWorker = null;
    worker.onmessage = null;
    worker.onmessageerror = null;
    worker.onerror = null;
    try {
      worker.terminate();
    } catch {
      // A failed Worker is already unusable; the repair lane still advances.
    }
  }

  #scheduleRepairIdleRetirement(): void {
    this.#clearRepairIdleTimeout();
    const worker = this.#repairWorker;
    if (!worker || this.#activeRepair || this.#repairQueue.length || this.#closed) return;
    this.#repairIdleTimeout = setTimeout(() => {
      this.#repairIdleTimeout = undefined;
      if (this.#repairWorker === worker && !this.#activeRepair && !this.#repairQueue.length) {
        this.#retireRepairWorker(worker);
      }
    }, timerDelay(this.#repairIdleTimeoutMs));
    (this.#repairIdleTimeout as unknown as { unref?: () => void }).unref?.();
  }

  #clearRepairIdleTimeout(): void {
    if (this.#repairIdleTimeout === undefined) return;
    clearTimeout(this.#repairIdleTimeout);
    this.#repairIdleTimeout = undefined;
  }

  #cancelRepair(repair: ScheduledConstraintRepair, reason: unknown): void {
    repair.terminalReason = "cancelled";
    this.#finishRepair(repair, "failure", reason);
  }

  #finishRepair(
    repair: ScheduledConstraintRepair,
    outcome: "standard" | "success" | "failure",
    value?: unknown,
    pump = true,
    disposition: RepairWorkerDisposition = "reclaim",
  ): void {
    if (repair.state === "settled") return;
    const activeWorkerMustRetire = this.#activeRepair === repair && disposition !== "retain";
    if (this.#activeRepair === repair) this.#activeRepair = undefined;
    else if (repair.state === "queued") {
      const index = this.#repairQueue.indexOf(repair);
      if (index >= 0) this.#repairQueue.splice(index, 1);
    }
    repair.state = "settled";
    if (repair.abortHandler && repair.signal) {
      repair.signal.removeEventListener("abort", repair.abortHandler);
    }
    if (repair.requestTimeout !== undefined) clearTimeout(repair.requestTimeout);
    if (repair.repairTimeout !== undefined) clearTimeout(repair.repairTimeout);
    this.#usedRequestIds.delete(repair.id);
    // Abandoned work must be reclaimed before a queued successor starts.
    // Completed perfect/heavy work is also retired here, after its terminal
    // response was consumed, to return the isolate's high-water heap.
    if (activeWorkerMustRetire) {
      const worker = this.#repairWorker;
      if (worker) this.#retireRepairWorker(worker);
    }

    if (pump) this.#pumpRepair();
    repair.terminal?.(repair.terminalReason);
    if (outcome === "failure") repair.reject(value);
    else if (outcome === "success") {
      const result = value as IntegralLayoutPlan;
      repair.resolve(repair.bestProgressive &&
          compareLayoutQuality(result.quality, repair.bestProgressive.quality) < 0
        ? repair.bestProgressive
        : result);
    } else {
      // A parent-side repair deadline or failed repair Worker cannot retract
      // geometry already published as a validated strict improvement. Streamed
      // candidates carry no final report, which keeps their coordinates and
      // metadata truthful when they become the fallback result.
      repair.resolve(repair.bestProgressive ?? repair.standard);
    }
  }

  #allocateRequestId(): number {
    let id: number;
    do {
      id = this.#nextRequestId;
      this.#nextRequestId = id === Number.MAX_SAFE_INTEGER ? 1 : id + 1;
    } while (this.#usedRequestIds.has(id));
    this.#usedRequestIds.add(id);
    return id;
  }
}

let sharedFactory: LayoutWorkerFactory = defaultLayoutWorkerFactory;
let sharedClient: LayoutWorkerClient | undefined;

function getSharedLayoutWorkerClient(): LayoutWorkerClient {
  return sharedClient ??= new LayoutWorkerClient(sharedFactory);
}

/** A client with Workers of its own, which start when it first plans and end when it terminates. */
export function createLayoutWorkerClient(): LayoutWorkerClient {
  return new LayoutWorkerClient(sharedFactory);
}

/** Plans on `client`'s Workers, the shared ones by default. */
export function planIntegralLayoutInWorker(
  request: IntegralLayoutRequest,
  options: IntegralLayoutAsyncOptions = {},
  client: LayoutWorkerClient = getSharedLayoutWorkerClient(),
): Promise<IntegralLayoutPlan> {
  return client.planIntegral(request, options);
}

export function planLayoutModelInWorker(
  model: LayoutModel,
  change: LayoutChange,
  options: PlanLayoutOptions = {},
  control: LayoutWorkerControlOptions = {},
): Promise<PlannedLayout> {
  return getSharedLayoutWorkerClient().planModel(model, change, options, control);
}

export function repairIntegralLayoutInWorker(
  request: IntegralLayoutRequest,
  standard: IntegralLayoutPlan,
  options: NonNullable<IntegralLayoutWireOptions["constraintRepair"]>,
  control: IntegralConstraintRepairControlOptions = {},
): Promise<IntegralLayoutPlan> {
  return getSharedLayoutWorkerClient().repairIntegral(request, standard, options, control);
}

/**
 * Stop and forget the shared schedulers. A future request creates fresh
 * Workers, which lets mapper/package shutdown release isolate memory without
 * preventing a later restart.
 */
export function shutdownLayoutWorkers(
  reason: Error = new Error("map-layout shared Workers were shut down"),
): void {
  const client = sharedClient;
  sharedClient = undefined;
  client?.shutdown(reason);
}

/** Replace the shared transport in Node tests without exposing it from index.ts. */
export function setLayoutWorkerFactoryForTesting(factory?: LayoutWorkerFactory): void {
  shutdownLayoutWorkers(new Error("map-layout Worker test factory changed"));
  sharedFactory = factory ?? defaultLayoutWorkerFactory;
}
