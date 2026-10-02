export interface ManualLayoutReply {
  readonly requestId: string;
  readonly sessionId: number;
  readonly accepted: boolean;
  readonly error?: string;
}

export interface ManualRouteAmendment {
  readonly fromRoomNumber: number;
  readonly toRoomNumber: number;
  readonly waypoints: readonly { readonly x: number; readonly y: number; readonly level: number }[];
}

/** Resolve facade IDs without assuming its room IDs have any particular spelling. */
export function serializeManualRouteAmendments(
  amendments: readonly {
    readonly from: string;
    readonly to: string;
    readonly waypoints: readonly { readonly x: number; readonly y: number; readonly level: number }[];
  }[] | undefined,
  rooms: readonly { readonly id: string; readonly roomNumber?: number }[],
): ManualRouteAmendment[] | undefined {
  if (!amendments?.length) return undefined;
  const numbers = new Map(rooms.map((room) => [room.id, room.roomNumber]));
  return amendments.map((amendment) => {
    const fromRoomNumber = numbers.get(amendment.from);
    const toRoomNumber = numbers.get(amendment.to);
    if (fromRoomNumber === undefined || toRoomNumber === undefined) {
      throw new Error("A reflow route amendment refers to an unmapped room.");
    }
    return {
      fromRoomNumber, toRoomNumber,
      waypoints: amendment.waypoints.map(({ x, y, level }) => ({ x, y, level })),
    };
  });
}

/** Subscribe before posting so even a fast commit has a correlated receipt. */
export function waitForManualLayoutCommit(
  requestId: string,
  sessionId: number,
  subscribe: (receive: (reply: Readonly<ManualLayoutReply>) => void) => { off(): void },
  post: () => void,
  timeoutMs = 60_000,
): Promise<void> {
  return new Promise((resolve, reject) => {
    let finished = false;
    let subscription: { off(): void } | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const finish = (error?: Error): void => {
      if (finished) return;
      finished = true;
      if (timer !== undefined) clearTimeout(timer);
      subscription?.off();
      if (error) reject(error);
      else resolve();
    };
    try {
      subscription = subscribe((reply) => {
        if (reply.requestId !== requestId || reply.sessionId !== sessionId) return;
        finish(reply.accepted ? undefined : new Error(reply.error ?? "The mapper refused the reflow."));
      });
      if (finished) {
        subscription.off();
        return;
      }
      timer = setTimeout(() => finish(new Error(
        "The mapper did not reply to the reflow commit; its result is unknown.",
      )), timeoutMs);
      post();
    } catch (error) {
      finish(error instanceof Error ? error : new Error(String(error)));
    }
  });
}
