import assert from "node:assert/strict";
import { registerHooks } from "node:module";
import test, { afterEach } from "node:test";
import { executeLayoutWorkerRequest } from "./worker-executor.ts";
import { setLayoutWorkerFactoryForTesting, type LayoutWorkerLike } from "./worker-client.ts";
import { LAYOUT_WORKER_PROTOCOL_VERSION, type LayoutWorkerRequest } from "./worker-protocol.ts";

// Exercise the actual public façade with a small structural host mock. Node
// cannot resolve Smudgy's native module, so only that import is substituted.
const host = globalThis as typeof globalThis & { __mapLayoutFacadeMapper: unknown };
const positions = new Map([
  [1, { x: -2, y: 0 }], [2, { x: 2, y: 0 }],
  [3, { x: 0, y: -2 }], [4, { x: 0, y: 2 }],
]);
const area = {
  id: "facade-area", room_numbers: [1, 2, 3, 4],
  room(number: number) {
    const position = positions.get(number);
    return position ? {
      room_number: number, ...position, level: 0,
      hasTag: () => true, data: () => undefined,
      exits: number === 1 || number === 3 ? [{ from_direction: "Other", to_area_id: "facade-area", to_room_number: number + 1 }] : [],
    } : undefined;
  },
};
host.__mapLayoutFacadeMapper = { getAreaById: () => area };
const coreModule = `data:text/javascript,${encodeURIComponent("export const mapper = globalThis.__mapLayoutFacadeMapper;")}`;
const hook = registerHooks({
  resolve(specifier, context, nextResolve) {
    return specifier === "smudgy:core"
      ? { url: coreModule, shortCircuit: true }
      : nextResolve(specifier, context);
  },
});
const { layoutSnapshotKey, loadLayoutModel, planAreaChange } = await import("./smudgy.ts");
hook.deregister();

class ExecutingWorker implements LayoutWorkerLike {
  onmessage: LayoutWorkerLike["onmessage"] = null;
  onmessageerror: LayoutWorkerLike["onmessageerror"] = null;
  onerror: LayoutWorkerLike["onerror"] = null;
  #terminated = false;

  postMessage(message: unknown): void {
    const request = structuredClone(message) as LayoutWorkerRequest;
    queueMicrotask(() => {
      if (this.#terminated) return;
      const response = executeLayoutWorkerRequest(request, (event) => {
        if (!this.#terminated) this.onmessage?.({ data: structuredClone({
          protocol: LAYOUT_WORKER_PROTOCOL_VERSION, id: request.id, operation: request.operation,
          progress: true, event,
        }) });
      });
      if (!this.#terminated) this.onmessage?.({ data: structuredClone(response) });
    });
  }

  terminate(): void { this.#terminated = true; }
}

afterEach(() => setLayoutWorkerFactoryForTesting());

test("the actual area façade retains zero-move connection amendments without exposing its temporary model", async () => {
  setLayoutWorkerFactoryForTesting(() => new ExecutingWorker());
  const id = area.id as AreaId;
  const source = loadLayoutModel(id);
  const result = await planAreaChange(id, { type: "reflow" }, { includeSnapshotKeys: true });
  assert.equal(result.patch.moves.length, 0);
  assert.ok(result.routeAmendments?.length);
  assert.equal("before" in result, false);
  assert.equal(result.sourceSnapshotKey, layoutSnapshotKey(source));
  assert.equal(result.plannedSnapshotKey, layoutSnapshotKey(source));
  const rooms = new Map(source.rooms.map((room) => [room.id, room.roomNumber]));
  for (const amendment of structuredClone(result.routeAmendments)) {
    assert.ok(rooms.has(amendment.from));
    assert.ok(rooms.has(amendment.to));
    assert.ok(amendment.waypoints.length);
  }
});
