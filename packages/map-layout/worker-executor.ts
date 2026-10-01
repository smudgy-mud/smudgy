import {
  planIntegralLayout,
  type ConstraintRepairReport,
  type LayoutTraceCandidate,
  type LayoutTraceEvent,
} from "./layout.ts";
import { repairIntegralLayoutConstraints } from "./constraint-layout.ts";
import { planLayoutModel } from "./model.ts";
import {
  decodeIntegralLayoutPlan,
  encodeIntegralLayoutPlan,
  encodePlannedLayout,
  LAYOUT_WORKER_PROTOCOL_VERSION,
  serializeLayoutWorkerError,
  type LayoutWorkerExecutionDiagnostics,
  type LayoutWorkerMemoryDiagnostics,
  type LayoutWorkerMemorySample,
  type LayoutWorkerRequest,
  type LayoutWorkerResponse,
  type LayoutWorkerTerminalReason,
} from "./worker-protocol.ts";

const MAX_RETAINED_TRACE_EVENTS = 4_096;
// A diagnostic trace is returned only after synchronous planning completes.
// Bound the resident-coordinate payload separately so a long-running repair
// cannot retain thousands of otherwise-complete whole-map incumbents.
const MAX_RETAINED_TRACE_POSITION_ENTRIES = 65_536;

/** Live messages are clone work queued outside this synchronous Worker turn. */
export const DEFAULT_LAYOUT_PROGRESS_INTERVAL_MS = 250;
export const MAX_LAYOUT_PROGRESS_MESSAGES = 4_096;
export const MAX_LAYOUT_PROGRESS_POSITION_ENTRIES = 65_536;

export interface LayoutWorkerExecutionOptions {
  /** Test seam for deterministic coalescing. */
  now?: () => number;
  progressIntervalMs?: number;
  maxProgressMessages?: number;
  maxProgressPositionEntries?: number;
}

interface DenoMemoryRealm {
  Deno?: {
    memoryUsage?: () => {
      heapUsed?: unknown;
      heapTotal?: unknown;
      external?: unknown;
      rss?: unknown;
    };
  };
}

function runtimeMemorySample(): LayoutWorkerMemorySample | undefined {
  try {
    const usage = (globalThis as unknown as DenoMemoryRealm).Deno?.memoryUsage?.();
    const rss = typeof usage?.rss === "number" && Number.isFinite(usage.rss) && usage.rss >= 0
      ? usage.rss
      : undefined;
    if (!usage || typeof usage.heapUsed !== "number" || !Number.isFinite(usage.heapUsed) ||
      usage.heapUsed < 0 || typeof usage.heapTotal !== "number" ||
      !Number.isFinite(usage.heapTotal) || usage.heapTotal < 0 ||
      typeof usage.external !== "number" || !Number.isFinite(usage.external) ||
      usage.external < 0 || usage.rss !== undefined && rss === undefined) {
      return undefined;
    }
    return {
      heapUsed: usage.heapUsed,
      heapTotal: usage.heapTotal,
      external: usage.external,
      ...(rss === undefined ? {} : { rss }),
    };
  } catch {
    return undefined;
  }
}

function peakMemory(
  current: LayoutWorkerMemorySample,
  sample: LayoutWorkerMemorySample,
): LayoutWorkerMemorySample {
  const rss = current.rss === undefined
    ? sample.rss
    : sample.rss === undefined
    ? current.rss
    : Math.max(current.rss, sample.rss);
  return {
    heapUsed: Math.max(current.heapUsed, sample.heapUsed),
    heapTotal: Math.max(current.heapTotal, sample.heapTotal),
    external: Math.max(current.external, sample.external),
    ...(rss === undefined ? {} : { rss }),
  };
}

function createMemoryTracker(
  now: () => number,
  intervalMs: number,
): {
  sample(): void;
  finish(): LayoutWorkerMemoryDiagnostics | undefined;
} {
  const start = runtimeMemorySample();
  let end = start;
  let peak = start;
  let samples = start ? 1 : 0;
  let lastSampleAt = now();
  const capture = (force: boolean): void => {
    const sampledAt = now();
    if (!force && sampledAt - lastSampleAt < intervalMs) return;
    const sample = runtimeMemorySample();
    if (!sample) return;
    end = sample;
    peak = peak ? peakMemory(peak, sample) : sample;
    samples += 1;
    lastSampleAt = sampledAt;
  };
  return {
    sample: () => capture(false),
    finish: () => {
      // Endpoints are always useful even when a short job never reaches the
      // periodic interval; only intermediate samples are interval-limited.
      capture(true);
      return start && end && peak && samples >= 2
        ? { samples, start, end, peak }
        : undefined;
    },
  };
}

function candidatePositionCount(candidate: LayoutTraceCandidate | undefined): number {
  return candidate?.positions?.length ?? 0;
}

function candidateMaterializationCount(candidate: LayoutTraceCandidate | undefined): number {
  return candidate?.positions ? 1 : 0;
}

function tracePositionCount(event: LayoutTraceEvent): number {
  if (event.type === "candidate-batch") return candidatePositionCount(event.best);
  if (event.type === "selection") return candidatePositionCount(event.selected);
  if (event.type === "improvement" || event.type === "vacuum" ||
    event.type === "obstruction-repair" || event.type === "bridge-vacuum" ||
    event.type === "crossing-repair") {
    return candidatePositionCount(event.before) + candidatePositionCount(event.after);
  }
  if (event.type === "obstruction-candidates") {
    return event.candidates.reduce(
      (total, candidate) => total + candidatePositionCount(candidate.result),
      0,
    );
  }
  if (event.type === "constraint-improvement" || event.type === "preview") {
    return candidatePositionCount(event.candidate);
  }
  return 0;
}

function traceCandidateMaterializations(event: LayoutTraceEvent): number {
  if (event.type === "candidate-batch") return candidateMaterializationCount(event.best);
  if (event.type === "selection") return candidateMaterializationCount(event.selected);
  if (event.type === "improvement" || event.type === "vacuum" ||
    event.type === "obstruction-repair" || event.type === "bridge-vacuum" ||
    event.type === "crossing-repair") {
    return candidateMaterializationCount(event.before) +
      candidateMaterializationCount(event.after);
  }
  if (event.type === "obstruction-candidates") {
    return event.candidates.reduce(
      (total, candidate) => total + candidateMaterializationCount(candidate.result),
      0,
    );
  }
  return event.type === "constraint-improvement" || event.type === "preview"
    ? candidateMaterializationCount(event.candidate)
    : 0;
}

function withoutPositions(candidate: LayoutTraceCandidate): LayoutTraceCandidate {
  if (!candidate.positions) return candidate;
  const { positions: _positions, ...summary } = candidate;
  return summary;
}

/**
 * A progress observer needs counters and complete strict improvements. It does
 * not need the full maps attached to nested planner diagnostics; an explicit
 * trace consumer receives those later through the separately bounded trace.
 */
function transportProgressEvent(
  operation: LayoutWorkerRequest["operation"],
  event: LayoutTraceEvent,
): LayoutTraceEvent | undefined {
  if (operation === "model" || event.type === "constraint-repair" ||
    event.type === "obstruction-candidates") return undefined;

  if (operation === "constraint-repair" && event.type !== "constraint-progress" &&
    event.type !== "constraint-improvement" && event.type !== "crossing-progress" &&
    event.type !== "crossing-repair" && event.type !== "axis-progress") return undefined;

  if (event.type === "candidate-batch") {
    return { ...event, best: event.best ? withoutPositions(event.best) : event.best };
  }
  if (event.type === "selection") {
    return { ...event, selected: withoutPositions(event.selected) };
  }
  if (event.type === "improvement" || event.type === "vacuum" ||
    event.type === "obstruction-repair" || event.type === "bridge-vacuum") {
    return {
      ...event,
      before: withoutPositions(event.before),
      after: withoutPositions(event.after),
    };
  }
  if (event.type === "crossing-repair") {
    // `after` is a validated anytime incumbent; `before` is diagnostic only.
    return { ...event, before: withoutPositions(event.before) };
  }
  return event;
}

function estimatedCloneBytes(value: unknown): number {
  const stack: unknown[] = [value];
  const seen = new Set<object>();
  let bytes = 0;
  while (stack.length) {
    const next = stack.pop();
    if (typeof next === "string") {
      bytes += 8 + next.length * 2;
    } else if (typeof next === "number") {
      bytes += 8;
    } else if (typeof next === "boolean") {
      bytes += 4;
    } else if (next && typeof next === "object" && !seen.has(next)) {
      seen.add(next);
      bytes += 16;
      if (Array.isArray(next)) {
        for (const item of next) stack.push(item);
      } else {
        for (const [key, item] of Object.entries(next)) {
          bytes += key.length * 2;
          stack.push(item);
        }
      }
    }
  }
  return Math.min(Number.MAX_SAFE_INTEGER, bytes);
}

interface ProgressAccounting {
  messages: number;
  coalesced: number;
  estimatedBytes: number;
  positionEntries: number;
}

interface ProgressStream {
  accept(event: LayoutTraceEvent): void;
  finish(): void;
}

function progressSummaryKey(event: LayoutTraceEvent): string {
  if (event.type === "axis-progress") return `${event.type}:${event.phase}`;
  if (event.type === "constraint-progress") return `${event.type}:${event.phase}`;
  if (event.type === "crossing-progress") return `${event.type}:${event.mode}:${event.status}`;
  if (event.type === "candidate-batch") return `${event.type}:${event.stage}`;
  return event.type;
}

function createProgressStream(
  request: LayoutWorkerRequest,
  progress: ((event: LayoutTraceEvent) => void) | undefined,
  accounting: ProgressAccounting,
  options: LayoutWorkerExecutionOptions,
): ProgressStream | undefined {
  if (!request.streamProgress || !progress) return undefined;
  const now = options.now ?? (() => performance.now());
  const interval = Math.max(0, options.progressIntervalMs ?? DEFAULT_LAYOUT_PROGRESS_INTERVAL_MS);
  const maximumMessages = Math.max(
    0,
    Math.floor(options.maxProgressMessages ?? MAX_LAYOUT_PROGRESS_MESSAGES),
  );
  const maximumPositions = Math.max(
    0,
    Math.floor(options.maxProgressPositionEntries ?? MAX_LAYOUT_PROGRESS_POSITION_ENTRIES),
  );
  let lastSentAt = Number.NEGATIVE_INFINITY;
  let pendingCandidate: LayoutTraceEvent | undefined;
  const pendingSummaries = new Map<string, LayoutTraceEvent>();

  const send = (event: LayoutTraceEvent, reserveCandidateSlot: boolean): boolean => {
    const positions = tracePositionCount(event);
    const messageLimit = reserveCandidateSlot ? Math.max(0, maximumMessages - 1) : maximumMessages;
    if (accounting.messages >= messageLimit ||
      accounting.positionEntries + positions > maximumPositions) return false;
    try {
      progress(event);
    } catch {
      return false;
    }
    accounting.messages += 1;
    accounting.positionEntries += positions;
    accounting.estimatedBytes = Math.min(
      Number.MAX_SAFE_INTEGER,
      accounting.estimatedBytes + estimatedCloneBytes(event),
    );
    lastSentAt = now();
    return true;
  };

  const flush = (force: boolean): void => {
    if (!force && now() - lastSentAt < interval) return;
    if (pendingCandidate) {
      const candidate = pendingCandidate;
      pendingCandidate = undefined;
      if (!send(candidate, false)) accounting.coalesced += 1;
      return;
    }
    const summaryEntry = pendingSummaries.entries().next();
    if (!summaryEntry.done) {
      const [key, summary] = summaryEntry.value;
      pendingSummaries.delete(key);
      if (!send(summary, true)) accounting.coalesced += 1;
    }
  };

  return {
    accept(event): void {
      const transported = transportProgressEvent(request.operation, event);
      if (!transported) return;
      if (transported.type === "preview") {
        // A preview shows the layout before a pass that can run for seconds,
        // so it goes at once, as the newest candidate.
        if (pendingCandidate) accounting.coalesced += 1;
        pendingCandidate = undefined;
        if (!send(transported, false)) accounting.coalesced += 1;
        return;
      }
      if (tracePositionCount(transported) > 0) {
        if (pendingCandidate) accounting.coalesced += 1;
        pendingCandidate = transported;
      } else {
        const key = progressSummaryKey(transported);
        if (pendingSummaries.has(key)) accounting.coalesced += 1;
        pendingSummaries.set(key, transported);
      }
      flush(false);
    },
    finish(): void {
      while (pendingCandidate || pendingSummaries.size) flush(true);
      if (pendingCandidate) accounting.coalesced += 1;
      accounting.coalesced += pendingSummaries.size;
      pendingCandidate = undefined;
      pendingSummaries.clear();
    },
  };
}

function terminalReason(report: Readonly<ConstraintRepairReport> | undefined): LayoutWorkerTerminalReason {
  if (!report) return "completed";
  if (report.cutoff === "time" || report.polishCutoff === "time") return "timeout";
  if (report.extensionSearch.cancelled || report.crossingRepair.cancelled) return "cancelled";
  // A repair that did not search reports no polish; it stopped at a work
  // ceiling only when its cutoff says so.
  if (report.outcome !== "searched") return report.cutoff === "none" ? "completed" : "cutoff";
  return report.cutoff === "none" && report.polishCutoff === "fixed-point"
    ? "completed"
    : "cutoff";
}

/** Execute one clone-safe protocol request inside the Worker realm. */
export function executeLayoutWorkerRequest(
  request: LayoutWorkerRequest,
  progress?: (event: LayoutTraceEvent) => void,
  executionOptions: LayoutWorkerExecutionOptions = {},
): LayoutWorkerResponse {
  const now = executionOptions.now ?? (() => performance.now());
  const startedAt = now();
  const memoryTracker = createMemoryTracker(
    now,
    Math.max(0, executionOptions.progressIntervalMs ?? DEFAULT_LAYOUT_PROGRESS_INTERVAL_MS),
  );
  const traceEvents: LayoutTraceEvent[] = [];
  let retainedTracePositions = 0;
  let traceEventsObserved = 0;
  let candidateMaterializations = 0;
  let inspectedStates = 0;
  const progressAccounting: ProgressAccounting = {
    messages: 0,
    coalesced: 0,
    estimatedBytes: 0,
    positionEntries: 0,
  };
  const progressStream = createProgressStream(
    request,
    progress,
    progressAccounting,
    executionOptions,
  );
  // Trace events exist only for a consumer: a retained diagnostic trace or a
  // requested live progress stream. Everything else plans hook-free, so jobs
  // nobody is watching build and post no per-event payloads at all.
  const trace = request.collectTrace || progressStream
    ? (event: LayoutTraceEvent) => {
      memoryTracker.sample();
      traceEventsObserved += 1;
      candidateMaterializations += traceCandidateMaterializations(event);
      if (event.type === "constraint-progress" || event.type === "constraint-improvement") {
        inspectedStates = Math.max(inspectedStates, event.separatorStates);
      }
      const positionCount = tracePositionCount(event);
      if (request.collectTrace && traceEvents.length < MAX_RETAINED_TRACE_EVENTS &&
        retainedTracePositions + positionCount <= MAX_RETAINED_TRACE_POSITION_ENTRIES) {
        traceEvents.push(event);
        retainedTracePositions += positionCount;
      }
      progressStream?.accept(event);
    }
    : undefined;

  const diagnostics = (
    reason: LayoutWorkerTerminalReason,
    report?: Readonly<ConstraintRepairReport>,
  ): LayoutWorkerExecutionDiagnostics => {
    if (report) {
      inspectedStates = Math.max(inspectedStates, report.separatorStates);
      candidateMaterializations = Math.max(
        candidateMaterializations,
        report.candidateMaterializations ?? 0,
      );
    }
    const memory = memoryTracker.finish();
    return {
      terminalReason: reason,
      elapsedMs: Math.max(0, now() - startedAt),
      traceEventsObserved,
      traceEventsRetained: traceEvents.length,
      retainedPositionEntries: retainedTracePositions,
      progressMessages: progressAccounting.messages,
      progressEventsCoalesced: progressAccounting.coalesced,
      estimatedProgressBytes: progressAccounting.estimatedBytes,
      progressPositionEntries: progressAccounting.positionEntries,
      candidateMaterializations,
      inspectedStates,
      ...(report?.peakLiveSearchNodes === undefined
        ? {}
        : { peakLiveSearchNodes: report.peakLiveSearchNodes }),
      ...(memory ? { memory } : {}),
      ...(report
        ? {
          repairCutoff: report.cutoff,
          stageMs: {
            search: report.searchMs,
            compaction: report.compactionMs,
            polish: report.polishMs,
            crossing: report.crossingRepair.elapsedMs,
          },
        }
        : {}),
    };
  };

  try {
    if (request.operation === "integral") {
      const result = planIntegralLayout({ ...request.request, trace });
      progressStream?.finish();
      return {
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id: request.id,
        operation: request.operation,
        ok: true,
        result: encodeIntegralLayoutPlan(result),
        traceEvents,
        diagnostics: diagnostics(terminalReason(result.constraintRepair), result.constraintRepair),
      };
    }

    if (request.operation === "constraint-repair") {
      const result = repairIntegralLayoutConstraints(
        { ...request.request, trace },
        decodeIntegralLayoutPlan(request.standard),
        request.options,
        trace,
      );
      progressStream?.finish();
      return {
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id: request.id,
        operation: request.operation,
        ok: true,
        result: encodeIntegralLayoutPlan(result),
        traceEvents,
        diagnostics: diagnostics(terminalReason(result.constraintRepair), result.constraintRepair),
      };
    }

    const result = planLayoutModel(request.model, request.change, {
      ...request.options,
      trace,
    });
    progressStream?.finish();
    return {
      protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
      id: request.id,
      operation: request.operation,
      ok: true,
      result: encodePlannedLayout(result),
      traceEvents,
      diagnostics: diagnostics("completed"),
    };
  } catch (error) {
    progressStream?.finish();
    return {
      protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
      id: request.id,
      operation: request.operation,
      ok: false,
      error: serializeLayoutWorkerError(error),
      traceEvents,
      diagnostics: diagnostics("failed"),
    };
  }
}
