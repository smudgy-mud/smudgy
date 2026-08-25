import { executeLayoutWorkerRequest } from "./worker-executor.ts";
import type { LayoutWorkerRequest } from "./worker-protocol.ts";

interface LayoutWorkerScope {
  onmessage: ((event: { data: LayoutWorkerRequest }) => void) | null;
  postMessage(message: unknown): void;
}

const scope = globalThis as unknown as LayoutWorkerScope;

scope.onmessage = (event): void => {
  scope.postMessage(executeLayoutWorkerRequest(event.data));
};
