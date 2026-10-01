// NukeFire-specific automatic mapper. Map.Local is authoritative; Room.Info
// contributes the human-readable current-area name and vertical topology.
// Mapping is shared client state, so only the oldest live session may mutate it.

import {
  createAlias,
  createProcedure,
  createState,
  echo,
  getSessions,
  mapper,
  session,
  type EventSubscription,
} from "smudgy:core";
import { created, destroyed } from "smudgy:events/sessions";
import { get, set } from "smudgy:params";
import { nukefire, watchMessage } from "smudgy://kapusniak/nukefire-gmcp";
import { AreaNames, rulesToSeed } from "./area-names.ts";
import { NUKEFIRE_AREA_NAME_RULES } from "./nukefire-maps.ts";
import { DEFAULT_DECISION_LOG_FILE } from "./decision-log.ts";
import { externalRoomId, isUsableVnum } from "./model.ts";
import { NukeFireMapper } from "./mapper.ts";
import { resolveFollowedLocation } from "./location-follow.ts";
import { ownsSharedMapping } from "./ownership.ts";
import {
  layoutPlannerState,
  layoutWorkerDiagnostics,
  type LayoutPlannerSnapshot,
  type LayoutWorkerClientDiagnostics,
} from "smudgy://kapusniak/map-layout";
import {
  LAYOUT_STATE_PUBLISH_INTERVAL_MS,
  ThrottledMirror,
} from "./state-mirror.ts";

export * from "./model.ts";
export * from "./layout.ts";
export * from "./routing.ts";
export * from "./atlas-resolution.ts";
export * from "./decision-log.ts";
export * from "./constraint-policy.ts";
export * from "./polish-state.ts";
export * from "./location-follow.ts";
export * from "./room-info.ts";
export * from "./ownership.ts";
export * from "./reflow-policy.ts";
export * from "./mapper.ts";

/** Cross-isolate layout telemetry consumed by optional NukeFire UI panels. */
export const layoutState = createState<LayoutPlannerSnapshot>("layoutState");
/** Map-free terminal heap, progress-volume, and worker-retirement accounting. */
export const layoutDiagnostics = createState<LayoutWorkerClientDiagnostics>("layoutDiagnostics");
// A long search emits planner snapshots far faster than a human-read panel
// benefits from. The throttle republishes latest-wins with a trailing edge,
// so the mirror never exceeds the interval yet always ends on the final state.
const layoutStateMirror = new ThrottledMirror<LayoutPlannerSnapshot>(
  (snapshot) => layoutState.set(snapshot),
  LAYOUT_STATE_PUBLISH_INTERVAL_MS,
);
layoutPlannerState.subscribe((snapshot) => layoutStateMirror.set(snapshot));
layoutWorkerDiagnostics.subscribe((diagnostics) => layoutDiagnostics.set(diagnostics));

/** Which map each area name belongs in: the "Area name rules" setting, seeded with what the package knows. */
function areaNames(): AreaNames {
  const SEEDED = "nukefire-mapper.area-name-rules.seeded";
  let lastSeeded: unknown;
  try {
    lastSeeded = JSON.parse(localStorage.getItem(SEEDED) ?? "null");
  } catch {
    lastSeeded = undefined;
  }
  const seed = rulesToSeed(NUKEFIRE_AREA_NAME_RULES, get("areaNameRules"), lastSeeded);
  if (seed) {
    set("areaNameRules", seed.map((rule) => ({ ...rule })));
    localStorage.setItem(SEEDED, JSON.stringify(seed));
  }
  const names = new AreaNames(get("areaNameRules"));
  if (names.problems.length > 0) {
    echo(`[nukefire-mapper] Skipped area name rules: ${names.problems.join("; ")}.`);
  }
  return names;
}

export const nukefireMapper = new NukeFireMapper({
  names: areaNames(),
  storage: "local",
  decisionLogFile: get("debugMappingDecisions") === true
    ? DEFAULT_DECISION_LOG_FILE
    : false,
});

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
  readonly geometricFixedPoint: boolean;
  readonly cutoff?: string;
  readonly polishCutoff?: string;
  readonly extensionSearch?: { readonly cancelled: boolean; readonly exhausted?: boolean };
  readonly crossingRepair?: { readonly cancelled: boolean; readonly exhausted?: boolean };
}

export interface RecordManualPolishResultPayload {
  readonly areaUuid: string;
  readonly expectedLayoutSnapshotKey: string;
  readonly policy: ManualPolishPolicyPayload;
  readonly report?: ManualPolishReportPayload;
  readonly terminalReason?: "perfect";
  readonly centerId?: string;
}

const PERFECT_POLICY_LIMITS: readonly (keyof Omit<
  ManualPolishPolicyPayload,
  "when" | "maxDurationMs"
>)[] = [
  "maxRestarts",
  "maxLayouts",
  "maxPolishTournaments",
  "maxPolishPasses",
  "maxExtensionStates",
  "maxLiveSearchNodes",
  "maxMaskDiversifications",
  "maxCrossingWork",
];

function validManualPolishResult(value: RecordManualPolishResultPayload): boolean {
  return typeof value?.areaUuid === "string" && value.areaUuid.length > 0 &&
    typeof value.expectedLayoutSnapshotKey === "string" &&
    value.expectedLayoutSnapshotKey.length > 0 && value.policy?.when === "always" &&
    value.policy.maxDurationMs === "infinity" && PERFECT_POLICY_LIMITS.every((key) =>
      Number.isSafeInteger(value.policy[key]) && value.policy[key] > 0
    ) && (value.report === undefined || typeof value.report.geometricFixedPoint === "boolean") &&
    (value.terminalReason === undefined || value.terminalReason === "perfect") &&
    (value.centerId === undefined || typeof value.centerId === "string");
}

/** Restricted cross-isolate bridge; code-importing this side-effectful package is forbidden. */
export const recordManualPolishResult = createProcedure<RecordManualPolishResultPayload>(
  async (result, caller) => {
    if (caller.origin !== "smudgy://kapusniak/nukefire-scripts" ||
      !validManualPolishResult(result)) return;
    const area = mapper.areas.find((candidate) => candidate.id === result.areaUuid);
    if (!area) return;
    const { maxDurationMs: _durationToken, ...finitePolicy } = result.policy;
    await nukefireMapper.recordManualPolishResult(area.id, {
      policy: {
        ...finitePolicy,
        maxDurationMs: Number.POSITIVE_INFINITY,
      },
      report: result.report,
      terminalReason: result.terminalReason,
      centerId: result.centerId,
      expectedLayoutSnapshotKey: result.expectedLayoutSnapshotKey,
    });
  },
);

let ownershipTimer: ReturnType<typeof setTimeout> | undefined;

// Non-owner sessions still track the player: the current-location marker is
// per-session, so follow Room.Info against the owner-written map, locate-only.
let followSubscription: EventSubscription | undefined;
let followedLocation = "";

function followRoomInfo(vnum: number | undefined): void {
  if (vnum === undefined || !isUsableVnum(vnum)) return;
  const located = resolveFollowedLocation(mapper, externalRoomId(vnum));
  if (!located) return;
  const key = `${located.area}:${located.room}`;
  if (key === followedLocation) return;
  mapper.setCurrentLocation(located.area, located.room);
  followedLocation = key;
}

function startFollowing(): void {
  if (followSubscription) return;
  followSubscription = watchMessage("Room.Info", (info) => followRoomInfo(info?.num));
  // onMessage/watchMessage have no replay; seed from the retained tree so a
  // stationary player is located immediately after losing ownership.
  followRoomInfo(nukefire.value?.Room?.Info?.num);
}

function stopFollowing(): void {
  followSubscription?.off();
  followSubscription = undefined;
  followedLocation = "";
}

function ownsMapping(): boolean {
  // Enumerating sibling sessions is itself the `reach-others` capability, so
  // the manifest keeps that grant even though nothing here acts on them.
  return ownsSharedMapping(session.id, getSessions());
}

function reconcileMappingOwner(): void {
  ownershipTimer = undefined;
  if (ownsMapping()) {
    stopFollowing();
    nukefireMapper.start();
  } else {
    nukefireMapper.stop();
    startFollowing();
  }
}

function scheduleOwnershipCheck(): void {
  if (ownershipTimer !== undefined) clearTimeout(ownershipTimer);
  // Lifecycle events are emitted as the registry changes; defer one turn so
  // a destroyed session has disappeared before electing its successor.
  ownershipTimer = setTimeout(reconcileMappingOwner, 100);
}

created.on(scheduleOwnershipCheck);
destroyed.on(scheduleOwnershipCheck);
reconcileMappingOwner();

createAlias(/^nfmap(?:\s+(?<args>.*))?$/i, ({ args }) => {
  const say = (line: string): void => echo(`[nfmap] ${line}`);
  const command = (args ?? "").trim().toLowerCase();
  if (command === "tidy") {
    if (!ownsMapping()) {
      say("Another session maps NukeFire; run nfmap tidy there.");
      return;
    }
    void nukefireMapper.tidyAllMaps(say);
  } else if (command === "stop") {
    say(
      nukefireMapper.stopTidy()
        ? "Stopping; a map being polished keeps the best layout already on it."
        : "No tidy is running.",
    );
  } else {
    say("nfmap tidy: combine every zone's map sections and polish every NukeFire map, reporting as it goes.");
    say("nfmap stop: stop tidying.");
  }
}, { name: "nfmap" });
