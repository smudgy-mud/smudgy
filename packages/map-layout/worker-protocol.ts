import type {
  GridPosition,
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

export const LAYOUT_WORKER_PROTOCOL_VERSION = 1 as const;

export type IntegralLayoutWireRequest = Omit<IntegralLayoutRequest, "trace">;
export type PlanLayoutWireOptions = Omit<PlanLayoutOptions, "trace">;

export interface IntegralLayoutWirePlan {
  positions: readonly (readonly [string, GridPosition])[];
  movedExisting: readonly string[];
  quality: Readonly<LayoutQuality>;
}

export interface PlannedLayoutWireResult {
  before: LayoutModel;
  after: LayoutModel;
  patch: LayoutPatch;
  positions: readonly (readonly [string, GridPosition])[];
  quality: Readonly<LayoutQuality>;
  search?: PlannedLayout["search"];
}

interface LayoutWorkerRequestBase {
  protocol: typeof LAYOUT_WORKER_PROTOCOL_VERSION;
  id: number;
  collectTrace: boolean;
}

export interface IntegralLayoutWorkerRequest extends LayoutWorkerRequestBase {
  operation: "integral";
  request: IntegralLayoutWireRequest;
}

export interface ModelLayoutWorkerRequest extends LayoutWorkerRequestBase {
  operation: "model";
  model: LayoutModel;
  change: LayoutChange;
  options: PlanLayoutWireOptions;
}

export type LayoutWorkerRequest = IntegralLayoutWorkerRequest | ModelLayoutWorkerRequest;

export interface SerializedLayoutWorkerError {
  name: string;
  message: string;
  stack?: string;
}

interface LayoutWorkerResponseBase {
  protocol: typeof LAYOUT_WORKER_PROTOCOL_VERSION;
  id: number;
  operation: LayoutWorkerRequest["operation"];
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

export interface LayoutWorkerFailure extends LayoutWorkerResponseBase {
  ok: false;
  error: SerializedLayoutWorkerError;
  traceEvents: readonly LayoutTraceEvent[];
}

export type LayoutWorkerResponse =
  | IntegralLayoutWorkerSuccess
  | ModelLayoutWorkerSuccess
  | LayoutWorkerFailure;

export function encodeIntegralLayoutPlan(plan: IntegralLayoutPlan): IntegralLayoutWirePlan {
  return {
    positions: [...plan.positions],
    movedExisting: [...plan.movedExisting],
    quality: plan.quality,
  };
}

export function decodeIntegralLayoutPlan(plan: IntegralLayoutWirePlan): IntegralLayoutPlan {
  return {
    positions: new Map(plan.positions),
    movedExisting: new Set(plan.movedExisting),
    quality: plan.quality,
  };
}

export function encodePlannedLayout(plan: PlannedLayout): PlannedLayoutWireResult {
  const result: PlannedLayoutWireResult = {
    before: plan.before,
    after: plan.after,
    patch: plan.patch,
    positions: [...plan.positions],
    quality: plan.quality,
  };
  if (plan.search) result.search = plan.search;
  return result;
}

export function decodePlannedLayout(plan: PlannedLayoutWireResult): PlannedLayout {
  const result: PlannedLayout = {
    before: plan.before,
    after: plan.after,
    patch: plan.patch,
    positions: new Map(plan.positions),
    quality: plan.quality,
  };
  if (plan.search) result.search = plan.search;
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

export function isLayoutWorkerResponse(value: unknown): value is LayoutWorkerResponse {
  if (!value || typeof value !== "object") return false;
  const candidate = value as Record<string, unknown>;
  if (candidate.protocol !== LAYOUT_WORKER_PROTOCOL_VERSION ||
    typeof candidate.id !== "number" || !Number.isSafeInteger(candidate.id) ||
    (candidate.operation !== "integral" && candidate.operation !== "model") ||
    typeof candidate.ok !== "boolean") return false;

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
  if (!Array.isArray(result.positions) || !result.quality || typeof result.quality !== "object") {
    return false;
  }
  if (candidate.operation === "integral") return Array.isArray(result.movedExisting);
  return !!result.before && typeof result.before === "object" &&
    !!result.after && typeof result.after === "object" &&
    !!result.patch && typeof result.patch === "object";
}
