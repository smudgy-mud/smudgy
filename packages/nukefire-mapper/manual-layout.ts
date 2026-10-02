/** Clone-safe messages for a guarded, mapper-owned manual layout commit. */
export interface ManualLayoutMove {
  readonly roomNumber: number;
  readonly x: number;
  readonly y: number;
  readonly level: number;
}

/** Engine detours use game-map room numbers across the isolate boundary. */
export interface ManualRouteAmendment {
  readonly fromRoomNumber: number;
  readonly toRoomNumber: number;
  readonly waypoints: readonly { readonly x: number; readonly y: number; readonly level: number }[];
}

// Matches map-layout's Worker protocol and leaves room for endpoint elbows.
export const MAX_MANUAL_ROUTE_AMENDMENT_WAYPOINTS = 32;
const MAX_MANUAL_ROUTE_AMENDMENTS = 1_024;

export interface ManualPolishPolicyPayload {
  readonly when: "always";
  readonly maxDurationMs: "infinity";
  readonly maxRestarts: number;
  readonly maxLayouts: number;
  readonly maxPolishTournaments: number;
  readonly maxPolishPasses: number;
  readonly maxExtensionStates: number;
  readonly maxLiveSearchNodes: number;
  readonly maxMaskDiversifications: number;
  readonly maxCrossingWork: number;
}

export interface ManualPolishReportPayload {
  readonly outcome?: string;
  readonly geometricFixedPoint: boolean;
  readonly cutoff?: string;
  readonly polishCutoff?: string;
  readonly extensionSearch?: { readonly cancelled: boolean; readonly exhausted?: boolean };
  readonly crossingRepair?: { readonly cancelled: boolean; readonly exhausted?: boolean };
}

export interface ManualLayoutRequest {
  readonly requestId: string;
  readonly areaUuid: string;
  readonly sourceSnapshotKey: string;
  readonly plannedSnapshotKey: string;
  readonly moves: readonly ManualLayoutMove[];
  readonly routeAmendments?: readonly ManualRouteAmendment[];
  readonly centerId?: string;
  readonly policy?: ManualPolishPolicyPayload;
  readonly report?: ManualPolishReportPayload;
  readonly terminalReason?: "perfect";
}

export interface ManualLayoutReply {
  readonly requestId: string;
  readonly sessionId: number;
  readonly accepted: boolean;
  readonly error?: string;
}

const POLICY_LIMITS = [
  "maxRestarts", "maxLayouts", "maxPolishTournaments", "maxPolishPasses",
  "maxExtensionStates", "maxLiveSearchNodes", "maxMaskDiversifications", "maxCrossingWork",
] as const;

export function validManualPolicy(value: ManualPolishPolicyPayload | undefined): boolean {
  return value?.when === "always" && value.maxDurationMs === "infinity" &&
    POLICY_LIMITS.every((key) => Number.isSafeInteger(value[key]) && value[key] > 0);
}

function validRoomNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) &&
    value >= -2_147_483_648 && value <= 2_147_483_647;
}

function validGridPosition(value: { x: number; y: number; level: number }): boolean {
  return typeof value === "object" && value !== null &&
    Number.isSafeInteger(value.x) && Number.isSafeInteger(value.y) && validRoomNumber(value.level);
}

export function validManualRouteAmendments(value: readonly ManualRouteAmendment[] | undefined): boolean {
  if (value === undefined) return true;
  if (!Array.isArray(value) || value.length > MAX_MANUAL_ROUTE_AMENDMENTS) return false;
  const pairs = new Set<string>();
  return [...value].every((amendment) => {
    if (typeof amendment !== "object" || amendment === null ||
      !validRoomNumber(amendment.fromRoomNumber) || !validRoomNumber(amendment.toRoomNumber) ||
      amendment.fromRoomNumber === amendment.toRoomNumber || !Array.isArray(amendment.waypoints) ||
      amendment.waypoints.length === 0 || amendment.waypoints.length > MAX_MANUAL_ROUTE_AMENDMENT_WAYPOINTS ||
      ![...amendment.waypoints].every(validGridPosition)) return false;
    const pair = JSON.stringify([Math.min(amendment.fromRoomNumber, amendment.toRoomNumber),
      Math.max(amendment.fromRoomNumber, amendment.toRoomNumber)]);
    if (pairs.has(pair)) return false;
    pairs.add(pair);
    return true;
  });
}

export function validManualLayoutRequest(value: ManualLayoutRequest): boolean {
  return typeof value?.requestId === "string" && value.requestId.length > 0 && value.requestId.length <= 128 &&
    typeof value.areaUuid === "string" && value.areaUuid.length > 0 &&
    typeof value.sourceSnapshotKey === "string" && value.sourceSnapshotKey.length > 0 &&
    typeof value.plannedSnapshotKey === "string" && value.plannedSnapshotKey.length > 0 &&
    Array.isArray(value.moves) && [...value.moves].every((move) =>
      validGridPosition(move) && validRoomNumber(move.roomNumber)
    ) && new Set(value.moves.map((move) => move.roomNumber)).size === value.moves.length &&
    validManualRouteAmendments(value.routeAmendments) &&
    (value.policy === undefined || validManualPolicy(value.policy)) &&
    (value.report === undefined || value.report !== null && typeof value.report.geometricFixedPoint === "boolean") &&
    (value.terminalReason === undefined || value.terminalReason === "perfect") &&
    (value.centerId === undefined || typeof value.centerId === "string");
}
