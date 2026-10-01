import type { IntegralLayoutPlan, LayoutQuality } from "./layout.ts";
import { mutableLayoutPlannerState } from "./planner-state-internal.ts";

export type LayoutPlannerStatus =
  | "idle"
  | "queued"
  | "planning"
  | "repairing"
  | "completed"
  | "cancelled"
  | "failed";

export type LayoutPlannerTerminalReason =
  | "completed"
  | "fixed-point"
  | "ceiling"
  | "timeout"
  | "cancelled"
  | "degraded"
  | "failed";

export interface LayoutPlannerWork {
  /** Candidate layouts generated or polished during this operation. */
  layoutsConsidered: number;
  /** Constraint compactions attempted, including infeasible variants. */
  compactionAttempts: number;
  /** Whole-constraint randomized restarts attempted. */
  restarts: number;
  /** Constraint feasibility analyses performed. */
  feasibilityChecks: number;
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
  /** Admissible child separator choices generated before state deduplication. */
  separatorBranches: number;
  /** Separator choices rejected because they would close an axis cycle. */
  separatorCyclePrunes: number;
  /** Crossing pairs selected as quick/deep transaction targets. */
  crossingsConsidered: number;
  /** Crossing-repair macro moves evaluated. */
  macrosConsidered: number;
  /** Collision-safe push closures constructed by crossing repair. */
  pushClosures: number;
  /** Deepest crossing-repair search node reached. */
  maxDepth: number;
  /** Distinct crossing-repair search states visited. */
  visitedStates: number;
}

/** Optional caller-owned identity for attributing an otherwise generic layout operation. */
export interface LayoutPlannerContext {
  source?: string;
  areaId?: string;
  areaName?: string;
  /** Stable caller planning/coverage identity; never contains room geometry. */
  contextKey?: string;
}

/** JSON-safe live telemetry for the most recently active layout operation. */
export interface LayoutPlannerSnapshot {
  sequence: number;
  status: LayoutPlannerStatus;
  operation: "integral" | "model" | "constraint-repair" | "none";
  phase: string;
  startedAt?: number;
  /** Whole-operation wall time, including deterministic planning and repair. */
  elapsedMs: number;
  nodes: number;
  residents: number;
  edges: number;
  /** Map-free caller identity, when supplied by the planner integration. */
  context?: Readonly<LayoutPlannerContext>;
  work: Readonly<LayoutPlannerWork>;
  /** Milliseconds from repair start to the first hard-valid compactor output. */
  firstIncumbentMs?: number;
  /** Quality measured from the durable resident map when planning began. */
  currentQuality?: Readonly<LayoutQuality>;
  /** Quality of the ordinary deterministic result. */
  standardQuality?: Readonly<LayoutQuality>;
  /** Best complete layout found so far. */
  bestQuality?: Readonly<LayoutQuality>;
  /** Why this operation stopped; optional for backward-compatible consumers. */
  terminalReason?: LayoutPlannerTerminalReason;
  message?: string;
}

export interface LayoutPlannerProgress {
  snapshot: Readonly<LayoutPlannerSnapshot>;
  /** Present only when a new complete best-so-far layout is available. */
  improvement?: IntegralLayoutPlan;
}

export type LayoutPlannerSubscriber = (snapshot: Readonly<LayoutPlannerSnapshot>) => void;

export interface LayoutPlannerStateHandle {
  readonly value: Readonly<LayoutPlannerSnapshot>;
  subscribe(subscriber: LayoutPlannerSubscriber): () => void;
}

/** Live state for the latest operation in this map-layout package realm. */
export const layoutPlannerState: LayoutPlannerStateHandle = mutableLayoutPlannerState;
