import type {
  IntegralLayoutPlan,
  IntegralLayoutRequest,
  LayoutTraceEvent,
} from "./layout.ts";
import type {
  LayoutChange,
  LayoutModel,
  PlannedLayout,
  PlanLayoutOptions,
} from "./model.ts";
import {
  decodeIntegralLayoutPlan,
  decodePlannedLayout,
  deserializeLayoutWorkerError,
  isLayoutWorkerResponse,
  LAYOUT_WORKER_PROTOCOL_VERSION,
  type IntegralLayoutWorkerSuccess,
  type LayoutWorkerRequest,
  type LayoutWorkerResponse,
  type ModelLayoutWorkerSuccess,
} from "./worker-protocol.ts";

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

export type LayoutWorkerFactory = () => LayoutWorkerLike;

interface PendingLayoutRequest {
  operation: LayoutWorkerRequest["operation"];
  trace?: (event: LayoutTraceEvent) => void;
  decode(response: LayoutWorkerResponse): unknown;
  resolve(value: unknown): void;
  reject(error: unknown): void;
}

interface LayoutWorkerConstructor {
  new (specifier: string | URL, options: { type: "module"; name?: string }): LayoutWorkerLike;
}

function defaultLayoutWorkerFactory(): LayoutWorkerLike {
  const constructor = (globalThis as unknown as { Worker?: LayoutWorkerConstructor }).Worker;
  if (!constructor) {
    throw new Error("map-layout requires Smudgy Worker support");
  }
  return new constructor(new URL("./worker.ts", import.meta.url), {
    type: "module",
    name: "map-layout",
  });
}

function errorFromUnknown(value: unknown, fallback: string): Error {
  if (value instanceof Error) return value;
  if (typeof value === "string" && value) return new Error(value);
  return new Error(fallback);
}

/**
 * One persistent Worker with ID-based response correlation. Message delivery is
 * FIFO in Smudgy, but IDs keep concurrent callers correct even if a test seam
 * or a future transport completes requests out of order.
 */
export class LayoutWorkerClient {
  readonly #factory: LayoutWorkerFactory;
  #worker: LayoutWorkerLike | null = null;
  #nextRequestId = 1;
  #pending = new Map<number, PendingLayoutRequest>();

  constructor(factory: LayoutWorkerFactory = defaultLayoutWorkerFactory) {
    this.#factory = factory;
  }

  planIntegral(request: IntegralLayoutRequest): Promise<IntegralLayoutPlan> {
    const { trace, ...wireRequest } = request;
    return this.#request(
      "integral",
      (id) => ({
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id,
        operation: "integral",
        collectTrace: !!trace,
        request: wireRequest,
      }),
      trace,
      (response) => decodeIntegralLayoutPlan((response as IntegralLayoutWorkerSuccess).result),
    );
  }

  planModel(
    model: LayoutModel,
    change: LayoutChange,
    options: PlanLayoutOptions = {},
  ): Promise<PlannedLayout> {
    const { trace, ...wireOptions } = options;
    return this.#request(
      "model",
      (id) => ({
        protocol: LAYOUT_WORKER_PROTOCOL_VERSION,
        id,
        operation: "model",
        collectTrace: !!trace,
        model,
        change,
        options: wireOptions,
      }),
      trace,
      (response) => decodePlannedLayout((response as ModelLayoutWorkerSuccess).result),
    );
  }

  /** Stop the current Worker and reject every request assigned to it. */
  terminate(reason: Error = new Error("map-layout Worker was terminated")): void {
    const worker = this.#worker;
    if (!worker) {
      this.#rejectPending(reason);
      return;
    }
    this.#resetWorker(worker, reason);
  }

  #request<T>(
    operation: LayoutWorkerRequest["operation"],
    createRequest: (id: number) => LayoutWorkerRequest,
    trace: ((event: LayoutTraceEvent) => void) | undefined,
    decode: (response: LayoutWorkerResponse) => T,
  ): Promise<T> {
    let worker: LayoutWorkerLike;
    try {
      worker = this.#ensureWorker();
    } catch (error) {
      return Promise.reject(errorFromUnknown(error, "could not start map-layout Worker"));
    }

    const id = this.#allocateRequestId();
    const request = createRequest(id);
    return new Promise<T>((resolve, reject) => {
      this.#pending.set(id, {
        operation,
        trace,
        decode,
        resolve: (value) => resolve(value as T),
        reject,
      });
      try {
        worker.postMessage(request);
      } catch (error) {
        this.#resetWorker(
          worker,
          errorFromUnknown(error, "could not send a request to the map-layout Worker"),
        );
      }
    });
  }

  #ensureWorker(): LayoutWorkerLike {
    if (this.#worker) return this.#worker;
    const worker = this.#factory();
    this.#worker = worker;
    worker.onmessage = (event): void => {
      if (this.#worker !== worker) return;
      this.#handleMessage(worker, event.data);
    };
    worker.onmessageerror = (): void => {
      this.#resetWorker(worker, new Error("map-layout Worker returned an uncloneable message"));
    };
    worker.onerror = (event): void => {
      event.preventDefault?.();
      this.#resetWorker(
        worker,
        errorFromUnknown(event.error ?? event.message, "map-layout Worker failed"),
      );
    };
    return worker;
  }

  #handleMessage(worker: LayoutWorkerLike, value: unknown): void {
    if (!isLayoutWorkerResponse(value)) {
      this.#resetWorker(worker, new Error("map-layout Worker returned an invalid response"));
      return;
    }
    const pending = this.#pending.get(value.id);
    if (!pending || pending.operation !== value.operation) {
      this.#resetWorker(worker, new Error("map-layout Worker returned an unexpected response"));
      return;
    }
    this.#pending.delete(value.id);

    if (!value.ok) {
      try {
        if (pending.trace) {
          for (const event of value.traceEvents) pending.trace(event);
        }
        pending.reject(deserializeLayoutWorkerError(value.error));
      } catch (error) {
        pending.reject(error);
      }
      return;
    }

    try {
      const result = pending.decode(value);
      if (pending.trace) {
        for (const event of value.traceEvents) pending.trace(event);
      }
      pending.resolve(result);
    } catch (error) {
      pending.reject(error);
    }
  }

  #allocateRequestId(): number {
    let id = this.#nextRequestId;
    do {
      id = this.#nextRequestId;
      this.#nextRequestId = id === Number.MAX_SAFE_INTEGER ? 1 : id + 1;
    } while (this.#pending.has(id));
    return id;
  }

  #resetWorker(worker: LayoutWorkerLike, reason: Error): void {
    if (this.#worker !== worker) return;
    this.#worker = null;
    worker.onmessage = null;
    worker.onmessageerror = null;
    worker.onerror = null;
    try {
      worker.terminate();
    } catch {
      // The Worker is already unusable; pending requests still need rejection.
    }
    this.#rejectPending(reason);
  }

  #rejectPending(reason: Error): void {
    const pending = [...this.#pending.values()];
    this.#pending.clear();
    for (const request of pending) request.reject(reason);
  }
}

let sharedFactory: LayoutWorkerFactory = defaultLayoutWorkerFactory;
let sharedClient: LayoutWorkerClient | undefined;

function getSharedLayoutWorkerClient(): LayoutWorkerClient {
  return sharedClient ??= new LayoutWorkerClient(sharedFactory);
}

export function planIntegralLayoutInWorker(
  request: IntegralLayoutRequest,
): Promise<IntegralLayoutPlan> {
  return getSharedLayoutWorkerClient().planIntegral(request);
}

export function planLayoutModelInWorker(
  model: LayoutModel,
  change: LayoutChange,
  options: PlanLayoutOptions = {},
): Promise<PlannedLayout> {
  return getSharedLayoutWorkerClient().planModel(model, change, options);
}

/** Replace the shared transport in Node tests without exposing it from index.ts. */
export function setLayoutWorkerFactoryForTesting(factory?: LayoutWorkerFactory): void {
  sharedClient?.terminate(new Error("map-layout Worker test factory changed"));
  sharedClient = undefined;
  sharedFactory = factory ?? defaultLayoutWorkerFactory;
}
