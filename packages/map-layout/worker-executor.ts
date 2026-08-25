import { planIntegralLayout, type LayoutTraceEvent } from "./layout.ts";
import { planLayoutModel } from "./model.ts";
import {
  encodeIntegralLayoutPlan,
  encodePlannedLayout,
  LAYOUT_WORKER_PROTOCOL_VERSION,
  serializeLayoutWorkerError,
  type LayoutWorkerRequest,
  type LayoutWorkerResponse,
} from "./worker-protocol.ts";

/** Execute one clone-safe protocol request inside the Worker realm. */
export function executeLayoutWorkerRequest(request: LayoutWorkerRequest): LayoutWorkerResponse {
  const traceEvents: LayoutTraceEvent[] = [];
  const trace = request.collectTrace ? (event: LayoutTraceEvent) => traceEvents.push(event) : undefined;

  try {
    if (request.operation === "integral") {
      const result = planIntegralLayout({ ...request.request, trace });
      return {
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id: request.id,
        operation: request.operation,
        ok: true,
        result: encodeIntegralLayoutPlan(result),
        traceEvents,
      };
    }

    const result = planLayoutModel(request.model, request.change, {
      ...request.options,
      trace,
    });
    return {
      protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
      id: request.id,
      operation: request.operation,
      ok: true,
      result: encodePlannedLayout(result),
      traceEvents,
    };
  } catch (error) {
    return {
      protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
      id: request.id,
      operation: request.operation,
      ok: false,
      error: serializeLayoutWorkerError(error),
      traceEvents,
    };
  }
}
