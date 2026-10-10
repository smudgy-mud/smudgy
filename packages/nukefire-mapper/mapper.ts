import { echo, mapper, type EventSubscription } from "smudgy:core";
import {
  createLayoutModel,
  layoutSnapshotKey,
  loadLayoutModel,
  resolveElevationGeometry,
} from "smudgy://kapusniak/map-layout";
import type { ManualLayoutRequest } from "./manual-layout.ts";
import { maintainAuthoredRoutePoints } from "./authored-route-maintenance.ts";
import {
  nukefire,
  onMessage,
  watchMessage,
  type NukeFireMapLink,
  type NukeFireMapLocal,
  type NukeFireMapRoom,
  type RoomInfo,
} from "smudgy://kapusniak/nukefire-gmcp";
import {
  directionSide,
  externalRoomId,
  isFiniteCoordinate,
  isUsableVnum,
  mapDirection,
  terrainColor,
  type MappedDirection,
} from "./model.ts";
import {
  compareLayoutQuality,
  computeIntegralRouteAmendments,
  createLayoutPlanner,
  measureIntegralLayoutQuality,
  planIntegralLayoutAsync,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
  type LayoutDirection,
  type LayoutEdge,
  type LayoutNode,
  type LayoutPlanner,
  type LayoutPlannerProgress,
  type LayoutQuality,
  type LayoutResident,
  type LayoutTraceEvent,
  type RouteAmendment,
} from "./layout.ts";
import { seamRegion } from "smudgy://kapusniak/map-layout/seam-region.ts";
import {
  DEFAULT_DECISION_LOG_FILE,
  MappingDecisionLogger,
  type DecisionLogRecord,
} from "./decision-log.ts";
import { AreaNames } from "./area-names.ts";
import { copyDoor, doorIsClosed, reportedDoor, sameDoor } from "./doors.ts";
import { NUKEFIRE_MARKS } from "./nukefire-maps.ts";
import { glimpsedZoneMap, readZoneFacts, type Settlement, settleZone, type ZoneContext } from "./zone-settle.ts";
import { counted, elapsedClock, polishProgressLine, qualitySummary, zoneNameFromMaps } from "./tidy.ts";
import {
  afterAreaRefresh,
  createdAtlasDecisionSummary,
  upsertLocalNukeFireAtlas,
} from "./atlas-resolution.ts";
import {
  amendedConnectionRoute,
  amendmentWaypointsBetween,
  directRoomObstructions,
  indexRouteAmendments,
  planConnectionRoute,
  routeCrossesCells,
  type RouteSide,
} from "./routing.ts";
import {
  verticalExitObservations,
  verticalMapLinks,
  type VerticalExitObservation,
} from "./room-info.ts";
import {
  restoreUnanchoredChartLevels,
  stackVerticalTraversals,
} from "./vertical-levels.ts";
import { reflowPolicy } from "./reflow-policy.ts";
import { planningFingerprint } from "./planning-fingerprint.ts";
import {
  assertCurrentMapperRun,
  ObsoleteNukeFireMapperRunError,
  whileCurrentMapperRun,
} from "./run-generation.ts";
import { SnapshotLatencyLanes } from "./latency-lanes.ts";
import { LatestValueQueue } from "./latest-value-queue.ts";
import { reconciliationUpdates } from "./layout-reconciliation.ts";
import {
  nukeFireAutomaticConstraintRepairPolicy,
  nukeFirePerfectConstraintRepairPolicy,
} from "./constraint-policy.ts";
import {
  CurrentLocationFreshness,
  type CurrentLocationObservation,
} from "./current-location-freshness.ts";
import {
  disambiguateOneWayArrivalPorts,
  routedEndpointSide,
  routeIsDormant,
  routeIsManuallyAuthored,
  type OneWayPortConnection,
} from "./connection-ports.ts";
import {
  AREA_POLISH_EXHAUSTED_FINGERPRINT_PROPERTY,
  AREA_POLISH_PENDING_PROPERTY,
  AREA_POLISH_PERFECT_EFFORT,
  AREA_POLISH_SEAMS_PROPERTY,
  AreaPolishEntryTracker,
  areaPolishEligibility,
  areaPolishMemo,
  areaPolishNeedsContextEvaluation,
  areaPolishPending,
  areaPolishSeams,
  areaPolishSeamsPropertyValue,
  createAreaPolishPlanningContext,
  MAX_FRUITLESS_QUIET_RESUMES,
  polishNotSearchedReason,
  polishRetrySuppressed,
  QuietPolishClaims,
  QuietResumeBudget,
  reduceAreaPolishMemo,
  reduceAreaPolishSeams,
  reduceAreaPolishState,
  type AreaPolishMemo,
  type AreaPolishEvent,
  type AreaPolishPlanningContext,
  type AreaPolishReport,
  type AreaPolishTerminalReason,
  type AreaPolishWorkPolicy,
} from "./polish-state.ts";
import {
  improvesThroughCrossings,
  keepsSeamPins,
  SeamPreviews,
  type SeamRoundPlan,
} from "./seam-preview.ts";
import {
  coordinateWriteAllowed,
  reconcilableResidentIds,
} from "./coordinate-write-policy.ts";

const ROOM_TERRAIN_PROPERTY = "terrain";
const ROOM_LAYOUT_LOCK_PROPERTY = "nukefire.layout.locked";

/**
 * Floor between progressive durable applies of one quiet search. Improvements
 * can arrive far faster than durable writes and viewport recentering are
 * worth watching; the first improvement of a search still applies immediately
 * and the final plan is never paced, so the anytime contract stays visible
 * without write amplification.
 */
export const PROGRESSIVE_APPLY_FLOOR_MS = 1_500;

export interface NukeFireMapperOptions {
  /** Which map each area name belongs in. Defaults to no rules: each name is a map of its own. */
  names?: AreaNames;
  /** Explicit storage for newly managed areas. Defaults to local. */
  storage?: MapStorage;
  /**
   * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
   * Use `storage: "session"` instead.
   */
  ephemeral?: boolean;
  /** Allow the integral-grid planner to reflow existing NukeFire rooms. Default true. */
  updateCoordinates?: boolean;
  /** Append structured decisions beneath package $DATA, or false to disable. */
  decisionLogFile?: string | false;
  /** @deprecated Automatic quiet reflow is always bounded. Use `nf reflow perfect` explicitly. */
  searchForPerfectLayouts?: boolean;
}

interface Assignment {
  source: NukeFireMapRoom;
  area: AreaMirror;
  room?: RoomMirror;
  position?: GridPosition;
  positionApplied?: boolean;
  /** Whether the pass that planned this position could move existing rooms. */
  moveExisting?: boolean;
  /** Whether this position came from a planner run rather than the identity plan. */
  planned?: boolean;
}

interface AssignmentPlanStats {
  plannedAreas: number;
  topologyGrowthAreas: number;
  movedRooms: number;
  plannerMs: number;
  coordinateWriteMs: number;
  routeWriteMs: number;
  batchCommitMs: number;
}

interface ExitMirror {
  /** Missing only for traversals just created atomically with a connection. */
  id?: ExitId;
  /** Missing only for unusual createRoomExit fallbacks until the area is rehydrated. */
  connectionId?: ConnectionId;
  fromDirection: ExitDirection;
  toDirection: ExitDirection | null;
  toAreaId: AreaId | null;
  toRoomNumber: RoomNumber | null;
  hidden: boolean;
  door: Door | null;
  weight: number;
  command: string | null;
}

interface RoomMirror {
  areaId: AreaId;
  roomNumber: RoomNumber;
  vnum?: number;
  externalId?: string;
  title: string;
  color: string;
  position: GridPosition;
  layoutLocked: boolean;
  zone?: string;
  terrain?: string;
  exits: ExitMirror[];
}

interface ConnectionMirror {
  id: ConnectionId;
  endpointA: ConnectionEndpoint;
  endpointB: ConnectionEndpoint | null;
  /**
   * The kind the host derives from where the rooms are. An update inside an
   * edit is checked against the kind the Connection had before the edit, so
   * route sync keeps this in step with the levels each edit leaves the rooms
   * on.
   */
  kind: ConnectionKind;
  routing: ConnectionRouting;
  segmentShape: ConnectionSegmentShape;
  corner: ConnectionCorner;
  routePoints: MapPoint[];
}

interface AreaMirror {
  id: AreaId;
  name: string;
  storage: MapStorage;
  polishPending: boolean;
  /** Bounded exact contexts which already completed fruitlessly on this geometry. */
  polishMemo: AreaPolishMemo | undefined;
  /** Rooms at the seams of merges the whole-map polish has not yet polished. */
  polishSeams: readonly RoomNumber[];
  roomsByNumber: Map<RoomNumber, RoomMirror>;
  connections: Map<string, ConnectionMirror>;
}

interface DesiredConnectionGeometry {
  endpoint_a: ConnectionEndpoint;
  endpoint_b: ConnectionEndpoint;
  routing: ConnectionRouting;
  segment_shape: ConnectionSegmentShape;
  corner: ConnectionCorner;
  route_points: MapPoint[];
}

/**
 * What a pass that moved no room changed: the rooms whose links it created,
 * and the cells of the rooms it placed. Adding rooms and links can change
 * only the routes at those rooms, routes an engine amendment redraws, and
 * routes a placed room now stands on; a new obstacle never opens a better
 * route for any other Connection.
 */
interface RouteSyncScope {
  readonly rooms: ReadonlySet<RoomNumber>;
  readonly cells: readonly GridPosition[];
}

function clone<T>(value: Readonly<T>): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function areaKey(area: { id: AreaId }): string {
  return areaIdKey(area.id);
}

function areaIdKey(areaId: AreaId): string {
  return areaId;
}

function sameAreaId(a: AreaId, b: AreaId): boolean {
  return areaIdKey(a) === areaIdKey(b);
}

function residentId(roomNumber: RoomNumber): string {
  return `room:${roomNumber}`;
}

function newRoomId(vnum: number): string {
  return `vnum:${vnum}`;
}

function geometrySignature(geometry: DesiredConnectionGeometry): string {
  return JSON.stringify({
    a: geometry.endpoint_a,
    b: geometry.endpoint_b,
    routing: geometry.routing,
    shape: geometry.segment_shape,
    corner: geometry.corner,
    points: geometry.route_points,
  });
}

function connectionSignature(connection: ConnectionMirror): string {
  if (!connection.endpointB) return "";
  return geometrySignature({
    endpoint_a: connection.endpointA,
    endpoint_b: connection.endpointB,
    routing: connection.routing,
    segment_shape: connection.segmentShape,
    corner: connection.corner,
    route_points: connection.routePoints,
  });
}

function roomVnum(room: Room | RoomMirror): number | undefined {
  if ("vnum" in room) return room.vnum;
  const raw = room.externalId?.trim();
  if (!raw) return undefined;
  const value = Number(raw);
  return isUsableVnum(value) && String(value) === raw ? value : undefined;
}

function roundedPosition(x: number, y: number, level: number): GridPosition {
  return { x: Math.round(x), y: Math.round(y), level: Math.round(level) };
}

function sameGridPosition(a: Readonly<GridPosition>, b: Readonly<GridPosition>): boolean {
  return a.x === b.x && a.y === b.y && a.level === b.level;
}

function mirrorPlanningFingerprint(area: AreaMirror): string {
  return planningFingerprint([...area.roomsByNumber.values()].map((room) => ({
    roomNumber: room.roomNumber,
    vnum: room.vnum,
    position: room.position,
    movable: room.vnum !== undefined && !room.layoutLocked,
    internalExits: room.exits
      .filter((exit) =>
        exit.toAreaId !== null &&
        exit.toRoomNumber !== null &&
        sameAreaId(exit.toAreaId, area.id)
      )
      .map((exit) => ({
        direction: exit.fromDirection,
        toRoomNumber: exit.toRoomNumber as RoomNumber,
      })),
  })));
}

function livePlanningFingerprint(area: Area): string {
  return planningFingerprint(area.room_numbers.flatMap((roomNumber) => {
    const room = area.room(roomNumber);
    if (!room) return [];
    const vnum = roomVnum(room);
    return [{
      roomNumber: room.room_number,
      vnum,
      position: roundedPosition(room.x, room.y, room.level),
      movable: vnum !== undefined &&
        room.data(ROOM_LAYOUT_LOCK_PROPERTY)?.trim().toLowerCase() !== "true",
      internalExits: room.exits
        .filter((exit) =>
          exit.to_area_id !== null &&
          exit.to_room_number !== null &&
          sameAreaId(exit.to_area_id, area.id)
        )
        .map((exit) => ({
          direction: exit.from_direction,
          toRoomNumber: exit.to_room_number as RoomNumber,
        })),
    }];
  }));
}

function restoreSet<T>(target: Set<T>, values: ReadonlySet<T>): void {
  target.clear();
  for (const value of values) target.add(value);
}

function assertNotAborted(signal: AbortSignal | undefined): void {
  if (!signal?.aborted) return;
  const reason = (signal as AbortSignal & { readonly reason?: unknown }).reason;
  if (reason instanceof Error) throw reason;
  const error = new Error("NukeFire full reflow was superseded by a newer snapshot");
  error.name = "AbortError";
  throw error;
}

type StalePlanPhase =
  | "before Worker planning"
  | "after Worker planning"
  | "before applying layout"
  | "before recording the polish";

class StaleNukeFireLayoutPlanError extends Error {
  constructor(
    area: AreaMirror,
    phase: StalePlanPhase,
  ) {
    super(
      `NukeFire area ${area.name} (${areaIdKey(area.id)}) changed ${phase}`,
    );
    this.name = "StaleNukeFireLayoutPlanError";
  }
}

function serializedPositions(positions: ReadonlyMap<string, GridPosition>): {
  id: string;
  x: number;
  y: number;
  level: number;
}[] {
  return [...positions]
    .sort(([a], [b]) => a.localeCompare(b))
    .map(([id, position]) => ({ id, ...position }));
}

function validRoom(room: NukeFireMapRoom): boolean {
  return isUsableVnum(room.vnum) &&
    isFiniteCoordinate(room.x) &&
    isFiniteCoordinate(room.y) &&
    isFiniteCoordinate(room.z) &&
    Number.isSafeInteger(room.zone);
}

function commandKey(command: string | null): string {
  return (command ?? "").trim().toLowerCase();
}

function matchingExit(room: RoomMirror, mapped: MappedDirection): ExitMirror | undefined {
  if (mapped.direction === "Special") {
    return room.exits.find((exit) =>
      exit.fromDirection === "Special" && commandKey(exit.command) === mapped.command
    );
  }
  return room.exits.find((exit) => exit.fromDirection === mapped.direction);
}

function exitLeadsTo(exit: ExitMirror | undefined, destination: RoomMirror | undefined): boolean {
  return !!exit && !!destination && exit.toAreaId !== null && exit.toRoomNumber !== null &&
    sameAreaId(exit.toAreaId, destination.areaId) &&
    exit.toRoomNumber === destination.roomNumber;
}

function topologyTraversalKey(from: number, to: number, command: string): string {
  return `${from}>${to}:${command}`;
}

function verticalPendingKey(link: Readonly<NukeFireMapLink>): string {
  return `${link.from}:${mapDirection(link.direction).direction}`;
}

function observedLinkKey(link: Readonly<NukeFireMapLink>): string {
  const mapped = mapDirection(link.direction);
  const identity = mapped.direction === "Special" ? mapped.command : mapped.direction;
  return `${link.from}>${link.to}:${identity}`;
}

function copyEndpoint(endpoint: ConnectionEndpoint): ConnectionEndpoint {
  return {
    room_number: endpoint.room_number,
    side: endpoint.side,
    port_offset: endpoint.port_offset,
    port_mode: endpoint.port_mode,
  };
}

function copyExit(exit: Exit): ExitMirror {
  return {
    id: exit.id,
    connectionId: exit.connection_id,
    fromDirection: exit.from_direction,
    toDirection: exit.to_direction,
    toAreaId: exit.to_area_id,
    toRoomNumber: exit.to_room_number,
    hidden: exit.is_hidden,
    door: copyDoor(exit.door),
    weight: exit.weight,
    command: exit.command,
  };
}

function exitFromFields(
  fields: ExitArgs,
  id?: ExitId,
  connectionId?: ConnectionId,
): ExitMirror {
  return {
    id,
    connectionId,
    fromDirection: fields.from_direction,
    toDirection: fields.to_direction ?? null,
    toAreaId: fields.to_area_id ?? null,
    toRoomNumber: fields.to_room_number ?? null,
    hidden: fields.is_hidden ?? false,
    door: copyDoor(fields.door),
    weight: fields.weight ?? 1,
    command: fields.command ?? null,
  };
}

function sameOptionalArea(a: AreaId | null, b: AreaId | undefined): boolean {
  return a === null ? b === undefined : b !== undefined && sameAreaId(a, b);
}

function exitMatchesFields(exit: ExitMirror, fields: ExitArgs): boolean {
  return exit.fromDirection === fields.from_direction &&
    exit.toDirection === (fields.to_direction ?? null) &&
    sameOptionalArea(exit.toAreaId, fields.to_area_id) &&
    exit.toRoomNumber === (fields.to_room_number ?? null) &&
    exit.hidden === (fields.is_hidden ?? false) &&
    sameDoor(exit.door, copyDoor(fields.door)) &&
    exit.weight === (fields.weight ?? 1) &&
    commandKey(exit.command) === commandKey(fields.command ?? null);
}

function applyExitFields(exit: ExitMirror, fields: ExitArgs): void {
  exit.fromDirection = fields.from_direction;
  exit.toDirection = fields.to_direction ?? null;
  exit.toAreaId = fields.to_area_id ?? null;
  exit.toRoomNumber = fields.to_room_number ?? null;
  exit.hidden = fields.is_hidden ?? false;
  exit.door = copyDoor(fields.door);
  exit.weight = fields.weight ?? 1;
  exit.command = fields.command ?? null;
}

function copyConnection(connection: Connection): ConnectionMirror {
  return {
    id: connection.id,
    endpointA: copyEndpoint(connection.endpoint_a),
    endpointB: connection.endpoint_b ? copyEndpoint(connection.endpoint_b) : null,
    kind: connection.kind,
    routing: connection.routing,
    segmentShape: connection.segment_shape,
    corner: connection.corner,
    routePoints: connection.route_points.map((point) => ({ x: point.x, y: point.y })),
  };
}

const CONNECTION_SIDE_ORDER: Record<ConnectionEndpoint["side"], number> = {
  North: 0,
  East: 1,
  South: 2,
  West: 3,
};

/** Mirror the backend's canonical endpoint ordering before any later update. */
function canonicalConnectionGeometry(
  geometry: Readonly<DesiredConnectionGeometry>,
): DesiredConnectionGeometry {
  const endpointA = geometry.endpoint_a;
  const endpointB = geometry.endpoint_b;
  const flip = endpointA.room_number === endpointB.room_number
    ? CONNECTION_SIDE_ORDER[endpointA.side] > CONNECTION_SIDE_ORDER[endpointB.side] ||
      (endpointA.side === endpointB.side && endpointA.port_offset > endpointB.port_offset)
    : endpointA.room_number > endpointB.room_number;
  if (!flip) {
    return {
      ...geometry,
      endpoint_a: copyEndpoint(endpointA),
      endpoint_b: copyEndpoint(endpointB),
      route_points: geometry.route_points.map((point) => ({ ...point })),
    };
  }
  return {
    ...geometry,
    endpoint_a: copyEndpoint(endpointB),
    endpoint_b: copyEndpoint(endpointA),
    route_points: [...geometry.route_points].reverse().map((point) => ({ ...point })),
  };
}

function connectionMirrorKey(id: ConnectionId): string {
  return id;
}

/** No exit drawn against its direction, no blocked route and no crossing. */
function hasNoDefects(quality: Readonly<LayoutQuality>): boolean {
  return quality.cardinalRayViolations === 0 && quality.routingViolations === 0 &&
    quality.linkCrossings === 0;
}

/**
 * The settlement context of an explicit perfect polish of the map as it now
 * is. Its stronger effort keeps a following automatic visit from repeating
 * lower-budget work; the coverage key is diagnostic.
 */
function perfectPolishContext(
  geometryFingerprint: string,
  policy: Readonly<AreaPolishWorkPolicy>,
  centerId?: string,
): AreaPolishPlanningContext {
  return createAreaPolishPlanningContext({
    geometryFingerprint,
    centerId,
    nodes: [],
    edges: [],
    searchForPerfectLayouts: true,
    policy,
    automaticEffort: AREA_POLISH_PERFECT_EFFORT,
  });
}

/**
 * Reconciles NukeFire's authoritative local map snapshots into Smudgy areas.
 * Calls are serialized because mapper mutations acknowledge asynchronously.
 */
export class NukeFireMapper {
  readonly #options: Required<Omit<
    NukeFireMapperOptions,
    "ephemeral" | "storage" | "names" | "searchForPerfectLayouts"
  >> & {
    storage: MapStorage;
  };
  readonly #decisionLogger: MappingDecisionLogger;
  readonly #names: AreaNames;
  readonly #subscriptions: EventSubscription[] = [];
  /** The map each zone's new rooms go to this run; merges can delete maps, so they clear it. */
  readonly #zoneMaps = new Map<number, AreaId>();
  /** Zones gathered into their area's map this run. */
  readonly #settledZones = new Set<number>();
  readonly #areasById = new Map<string, AreaMirror>();
  readonly #roomsByVnum = new Map<number, RoomMirror>();
  readonly #latencyLanes: SnapshotLatencyLanes<NukeFireMapLocal>;
  readonly #currentLocationFreshness = new CurrentLocationFreshness();
  readonly #polishEntries = new AreaPolishEntryTracker();
  readonly #quietPolishClaims = new QuietPolishClaims<NukeFireMapLocal>(
    (aborted, incoming) => this.#snapshotsShareArea(aborted, incoming),
  );
  readonly #sameAreaPolishDisplacements = new WeakMap<NukeFireMapLocal, Set<string>>();
  readonly #quietResumeBudget = new QuietResumeBudget();
  /** Each map's seam round and the geometry its whole-map polish plans from. */
  readonly #seamPreviews = new SeamPreviews<ReadonlyMap<string, GridPosition>>();
  readonly #snapshotCurrentLocations = new WeakMap<
    NukeFireMapLocal,
    CurrentLocationObservation
  >();
  #lastRoomInfo: RoomInfo | undefined;
  #lastSnapshot: NukeFireMapLocal | undefined;
  /** Traversals already allowed to trigger one expensive geometry reflow. */
  readonly #plannedTopology = new Set<string>();
  /** Areas whose persisted AutoPinned ports were reconciled in this run. */
  readonly #reconciledPortAreas = new Set<string>();
  /** Areas whose prompt topology placement still needs a quiet full reflow. */
  readonly #deferredReflowAreas = new Set<string>();
  /** Numeric Room.Info vertical exits waiting for their destination room. */
  readonly #pendingVerticalLinks = new Map<string, NukeFireMapLink>();
  #localAtlasUpsert: Promise<Atlas> | undefined;
  #localAtlasUpsertGeneration: number | undefined;
  #areasReady = false;
  #runGeneration = 0;
  #started = false;
  /** Stops the `nfmap tidy` in progress; the quiet polish waits while one runs. */
  #tidy: AbortController | undefined;
  /**
   * The quiet polish's own planner. Movement aborts a quiet polish, and an
   * aborted search ends its Worker; on this planner that is never the shared
   * Worker the next new room is placed on.
   */
  #quietPlanner: LayoutPlanner | undefined;
  #currentLocation = "";
  #lastError = "";
  #lastDecisionLogError = "";
  #mutationSequence = 0;

  constructor(options: NukeFireMapperOptions = {}) {
    this.#names = options.names ?? new AreaNames([]);
    this.#options = {
      storage: options.storage ?? (options.ephemeral ? "session" : "local"),
      updateCoordinates: options.updateCoordinates ?? true,
      decisionLogFile: options.decisionLogFile ?? DEFAULT_DECISION_LOG_FILE,
    };
    this.#decisionLogger = new MappingDecisionLogger(this.#options.decisionLogFile, (error) => {
      if (error === this.#lastDecisionLogError) return;
      this.#lastDecisionLogError = error;
      echo(`[nukefire-mapper] ${error}`);
    });
    this.#latencyLanes = new SnapshotLatencyLanes({
      snapshotKey: (snapshot) => snapshot.center,
      followCurrent: (snapshot) => this.#observeSnapshotCurrentRoom(snapshot),
      runTopology: (snapshot, signal) => this.#runSnapshotLane(snapshot, false, signal),
      runFullReflow: (snapshot, signal) => {
        const currentAreaKey = this.#polishEntries.currentAreaKey;
        const snapshotRoom = this.#roomsByVnum.get(snapshot.center);
        if (
          this.#tidy || !currentAreaKey || !snapshotRoom ||
          areaIdKey(snapshotRoom.areaId) !== currentAreaKey ||
          !this.#deferredReflowAreas.has(currentAreaKey)
        ) {
          return Promise.resolve();
        }
        return this.#runSnapshotLane(snapshot, true, signal);
      },
      onFullReflowAborted: (aborted, incoming) =>
        this.#restorePolishAfterDisplacement(aborted, incoming),
      onError: (_lane, snapshot, error) => this.#reportSnapshotError(snapshot, error),
    });
  }

  /**
   * Whether the displacing snapshot keeps the player inside the same area the
   * displaced pass was polishing. Same-center chatter and movement between an
   * area's mapped rooms both qualify. An unmapped destination does not — but
   * that is always topology growth, whose prompt-lane pass re-arms the
   * attempt on its own.
   */
  #snapshotsShareArea(
    aborted: Readonly<NukeFireMapLocal>,
    incoming: Readonly<NukeFireMapLocal>,
  ): boolean {
    const abortedRoom = this.#roomsByVnum.get(aborted.center);
    const incomingRoom = this.#roomsByVnum.get(incoming.center);
    return abortedRoom !== undefined && incomingRoom !== undefined &&
      sameAreaId(abortedRoom.areaId, incomingRoom.areaId);
  }

  /**
   * A quiet pass consumes its visit's polish attempt before cancelable work
   * begins. When the displacing snapshot stays inside the area the pass was
   * polishing — same-center chatter or movement between its rooms — the abort
   * loses no opportunity: re-arm the attempt and the deferred-reflow gate so
   * the re-armed quiet timer resumes the search within this visit. A
   * fruitless-resume budget bounds the churn: each restored pass that had
   * committed nothing durable spends one unit, and a spent allowance forfeits
   * the visit until re-entry or growth. Leaving the area keeps the plain
   * forfeit-until-reentry posture, with the durable hint as the backstop.
   *
   * The exhausted-fingerprint memo is deliberately untouched here — aborted
   * passes never write it. The memo ends cross-visit retries over geometry a
   * completed attempt proved unimprovable; this budget bounds within-visit
   * abort-restart churn before any attempt can complete.
   */
  #restorePolishAfterDisplacement(
    aborted: NukeFireMapLocal,
    incoming: NukeFireMapLocal,
  ): void {
    for (const [claimedAreaKey, claim] of this.#quietPolishClaims.settle(aborted, incoming)) {
      if (this.#polishEntries.currentAreaKey !== claimedAreaKey) continue;
      const displacedAreas = this.#sameAreaPolishDisplacements.get(aborted) ?? new Set<string>();
      displacedAreas.add(claimedAreaKey);
      this.#sameAreaPolishDisplacements.set(aborted, displacedAreas);
      const area = this.#areasById.get(claimedAreaKey);
      if (!this.#quietResumeBudget.allowResume(claimedAreaKey, claim.progressed === true)) {
        this.#logDecision({
          kind: "layout-polish-resume-exhausted",
          area: area ? { id: area.id, name: area.name } : { key: claimedAreaKey },
          center: incoming.center,
          fruitlessResumes: MAX_FRUITLESS_QUIET_RESUMES,
        });
        continue;
      }
      if (claim.retryConsumed) {
        this.#polishEntries.markPending(claimedAreaKey, this.#options.updateCoordinates);
      }
      if (claim.deferredRemoved) this.#deferredReflowAreas.add(claimedAreaKey);
      this.#logDecision({
        kind: "layout-polish-restored",
        area: area ? { id: area.id, name: area.name } : { key: claimedAreaKey },
        center: incoming.center,
      });
    }
  }

  #takeSameAreaPolishDisplacement(snapshot: NukeFireMapLocal, claimedAreaKey: string): boolean {
    const displacedAreas = this.#sameAreaPolishDisplacements.get(snapshot);
    if (!displacedAreas?.delete(claimedAreaKey)) return false;
    if (displacedAreas.size === 0) this.#sameAreaPolishDisplacements.delete(snapshot);
    return true;
  }

  #registerRoom(area: AreaMirror, room: RoomMirror): void {
    area.roomsByNumber.set(room.roomNumber, room);
    if (room.vnum !== undefined) this.#roomsByVnum.set(room.vnum, room);
  }

  #registerArea(area: AreaMirror): AreaMirror {
    this.#areasById.set(areaIdKey(area.id), area);
    return area;
  }

  /** Drops a deleted map from the mirror. */
  #forgetArea(id: AreaId): void {
    const known = this.#areasById.get(areaIdKey(id));
    if (!known) return;
    for (const room of known.roomsByNumber.values()) {
      if (room.vnum !== undefined && this.#roomsByVnum.get(room.vnum) === room) {
        this.#roomsByVnum.delete(room.vnum);
      }
    }
    this.#areasById.delete(areaIdKey(id));
    this.#deferredReflowAreas.delete(areaIdKey(id));
    this.#reconciledPortAreas.delete(areaIdKey(id));
    this.#seamPreviews.forget(areaIdKey(id));
  }

  #inTier(area: { storage: MapStorage }): boolean {
    return area.storage === this.#options.storage;
  }

  /**
   * Copy one immutable Smudgy area snapshot into ordinary VM-owned records.
   * This is the only full-area read path; steady-state mapping uses the mirror.
   */
  #hydrateArea(source: Area, force = false): AreaMirror {
    const id = source.id;
    const key = areaIdKey(id);
    const known = this.#areasById.get(key);
    if (known && !force) return known;
    if (known) {
      for (const room of known.roomsByNumber.values()) {
        if (room.vnum !== undefined && this.#roomsByVnum.get(room.vnum) === room) {
          this.#roomsByVnum.delete(room.vnum);
        }
      }
    }

    const area: AreaMirror = {
      id,
      name: source.name,
      storage: source.storage,
      polishPending: areaPolishPending(source.data(AREA_POLISH_PENDING_PROPERTY)),
      polishMemo: areaPolishMemo(
        source.data(AREA_POLISH_EXHAUSTED_FINGERPRINT_PROPERTY),
      ),
      polishSeams: areaPolishSeams(source.data(AREA_POLISH_SEAMS_PROPERTY)),
      roomsByNumber: new Map(),
      connections: new Map(),
    };
    for (const roomNumber of source.room_numbers) {
      const room = source.room(roomNumber);
      if (!room) continue;
      const externalId = room.externalId;
      const mirrored: RoomMirror = {
        areaId: id,
        roomNumber: room.room_number,
        vnum: roomVnum(room),
        externalId,
        title: room.title,
        color: room.color,
        position: roundedPosition(room.x, room.y, room.level),
        layoutLocked: room.data(ROOM_LAYOUT_LOCK_PROPERTY)?.trim().toLowerCase() === "true",
        zone: room.data(NUKEFIRE_MARKS.zone),
        terrain: room.data(ROOM_TERRAIN_PROPERTY),
        exits: room.exits.map(copyExit),
      };
      this.#registerRoom(area, mirrored);
    }
    for (const connection of source.connections) {
      const mirrored = copyConnection(connection);
      area.connections.set(connectionMirrorKey(mirrored.id), mirrored);
    }
    return this.#registerArea(area);
  }

  get started(): boolean {
    return this.#started;
  }

  /** Absolute runtime path of the JSONL decision log, when enabled. */
  get decisionLogPath(): string | undefined {
    return this.#decisionLogger.path;
  }

  /**
   * Persist an explicit perfect-reflow result against the area's live final
   * geometry. The stronger effort prevents a following automatic visit from
   * immediately repeating lower-budget work; the coverage key is diagnostic.
   */
  async recordManualPolishResult(
    areaId: AreaId,
    result: {
      readonly policy: Readonly<AreaPolishWorkPolicy>;
      readonly report?: Readonly<AreaPolishReport>;
      readonly terminalReason?: AreaPolishTerminalReason;
      readonly centerId?: string;
      /** Canonical final model returned by the stable plan that produced this evidence. */
      readonly expectedLayoutSnapshotKey: string;
    },
  ): Promise<boolean> {
    return await this.#latencyLanes.exclusive(async () => {
      const live = mapper.getAreaById(areaId);
      const currentLayoutKey = layoutSnapshotKey(loadLayoutModel(live, {
        isRoomMovable: (room) =>
          !room.hasTag("LAYOUT_LOCKED") &&
          room.data("layoutLocked") !== "true" &&
          room.data(ROOM_LAYOUT_LOCK_PROPERTY) !== "true",
      }));
      if (currentLayoutKey !== result.expectedLayoutSnapshotKey) return false;
      const geometryFingerprint = livePlanningFingerprint(live);
      await this.#persistAreaPolishState(this.#hydrateArea(live, true), {
        kind: "polish-completed",
        report: result.report,
        terminalReason: result.terminalReason,
        context: perfectPolishContext(geometryFingerprint, result.policy, result.centerId),
      });
      return true;
    });
  }

  /** The owner validates and commits a manual plan between topology writes. */
  async applyManualLayout(result: Readonly<ManualLayoutRequest>): Promise<void> {
    await this.#latencyLanes.exclusive(async () => {
      const runGeneration = this.#runGeneration;
      this.#assertCurrentRun(runGeneration);
      const source = mapper.areas.find((area) => area.id === result.areaUuid);
      if (!source) throw new Error("The reflow map is no longer active.");
      const before = loadLayoutModel(source, {
        isRoomMovable: (room) =>
          !room.hasTag("LAYOUT_LOCKED") && room.data("layoutLocked") !== "true" &&
          room.data(ROOM_LAYOUT_LOCK_PROPERTY) !== "true",
      });
      if (layoutSnapshotKey(before) !== result.sourceSnapshotKey) {
        throw new Error("Map changed while reflow was planning; run nf reflow again.");
      }
      const moves = new Map(result.moves.map((move) => [move.roomNumber, move]));
      const numbers = new Set(before.rooms.map((room) => room.roomNumber));
      if (result.moves.some((move) => !numbers.has(move.roomNumber))) {
        throw new Error("The reflow refers to a missing room.");
      }
      const planned = resolveElevationGeometry(createLayoutModel({
        ...before,
        rooms: before.rooms.map((room) => {
          const move = room.roomNumber === undefined ? undefined : moves.get(room.roomNumber);
          if (!move) return room;
          if (room.movable === false && (move.x !== room.position.x || move.y !== room.position.y ||
            move.level !== room.position.level)) throw new Error("The reflow moves a locked room.");
          return { ...room, position: { x: move.x, y: move.y, level: move.level } };
        }),
        // Up/Down geometry must describe the final levels, just as the facade's key does.
        edges: before.edges.map(({ constraintVector: _resolved, ...edge }) => edge),
      }));
      if (layoutSnapshotKey(planned) !== result.plannedSnapshotKey) {
        throw new Error("The reflow moves do not match its final snapshot.");
      }
      const positions = new Map(planned.rooms.flatMap((room) => room.roomNumber === undefined ? [] :
        [[residentId(room.roomNumber), room.position] as const]));
      const routeAmendments: RouteAmendment[] | undefined = result.routeAmendments?.map((amendment) => {
        const from = residentId(amendment.fromRoomNumber);
        const to = residentId(amendment.toRoomNumber);
        const fromPosition = positions.get(from);
        const toPosition = positions.get(to);
        if (!fromPosition || !toPosition || fromPosition.level !== toPosition.level ||
          amendment.waypoints.some((point) => point.level !== fromPosition.level)) {
          throw new Error("The reflow detour does not match its final rooms.");
        }
        return { from, to, waypoints: amendment.waypoints };
      });
      const updates: [RoomNumber, UpdateRoomParams][] = result.moves.map((move) => [move.roomNumber, {
        x: move.x, y: move.y, level: move.level,
      }]);
      const committed = await this.#commitLayoutMoves(
        this.#hydrateArea(source, true), positions, updates, runGeneration,
        "Reflow NukeFire rooms", routeAmendments,
      );
      const after = loadLayoutModel(mapper.getAreaById(source.id), {
        isRoomMovable: (room) =>
          !room.hasTag("LAYOUT_LOCKED") && room.data("layoutLocked") !== "true" &&
          room.data(ROOM_LAYOUT_LOCK_PROPERTY) !== "true",
      });
      if (layoutSnapshotKey(after) !== result.plannedSnapshotKey) {
        throw new Error("Map changed during the reflow commit; no settlement was recorded.");
      }
      if (result.policy) {
        const { maxDurationMs: _durationToken, ...policy } = result.policy;
        await this.#persistAreaPolishState(committed, {
          kind: "polish-completed", report: result.report, terminalReason: result.terminalReason,
          context: perfectPolishContext(livePlanningFingerprint(mapper.getAreaById(source.id)), {
            ...policy, maxDurationMs: Number.POSITIVE_INFINITY,
          }, result.centerId),
        }, runGeneration);
      }
    });
  }

  /** Every host envelope must remain valid while a large move is split into batches. */
  async #commitLayoutMoves(
    area: AreaMirror,
    positions: ReadonlyMap<string, GridPosition>,
    updates: readonly [RoomNumber, UpdateRoomParams][],
    runGeneration: number,
    description: string,
    routeAmendments?: readonly RouteAmendment[],
    timings?: { coordinateWriteMs: number; routeWriteMs: number },
    routeScope?: RouteSyncScope,
  ): Promise<AreaMirror> {
    const restores: { id: ConnectionId; routing: ConnectionRouting; points?: MapPoint[] }[] = [];
    const fieldsByNumber = new Map(updates);
    const source = mapper.getAreaById(area.id);
    const storedPositions = new Map<RoomNumber, GridPosition>();
    const storedPosition = (number: RoomNumber): GridPosition | undefined => {
      const cached = storedPositions.get(number);
      if (cached) return cached;
      const room = source.room(number);
      if (!room) return undefined;
      // Authored points follow actual stored coordinates, including fractional
      // editor positions which the integral planning mirror rounds.
      const position = { x: room.x, y: room.y, level: room.level };
      storedPositions.set(number, position);
      return position;
    };
    const levelChanges = new Set(updates.filter(([number, fields]) =>
      fields.level !== undefined && fields.level !== area.roomsByNumber.get(number)?.position.level
    ).map(([number]) => number));
    let routeAfterCommit = false;
    const routeStartedAt = performance.now();
    let coordinateWriteMs = 0;
    await this.#whileCurrentRun(runGeneration, () => this.#mutateArea(area.id, async (mutation) => {
      // Routing changes precede ALL coordinates. The host stages ordered 256-op
      // envelopes, so endpoints can briefly be on different levels even when
      // the completed plan brings both to the same new level.
      for (const connection of area.connections.values()) {
        const endpointB = connection.endpointB;
        if (!endpointB) continue;
        const authored = routeIsManuallyAuthored(connection.routing);
        const dormant = routeIsDormant(connection.routing, connection.routePoints);
        const preservePoints = (authored || dormant) &&
          (fieldsByNumber.has(connection.endpointA.room_number) || fieldsByNumber.has(endpointB.room_number));
        const beforeA = preservePoints ? storedPosition(connection.endpointA.room_number) : undefined;
        const beforeB = preservePoints ? storedPosition(endpointB.room_number) : undefined;
        const fieldsA = fieldsByNumber.get(connection.endpointA.room_number);
        const fieldsB = fieldsByNumber.get(endpointB.room_number);
        const afterA = beforeA ? {
          x: fieldsA?.x ?? beforeA.x, y: fieldsA?.y ?? beforeA.y, level: fieldsA?.level ?? beforeA.level,
        } : undefined;
        const afterB = beforeB ? {
          x: fieldsB?.x ?? beforeB.x, y: fieldsB?.y ?? beforeB.y, level: fieldsB?.level ?? beforeB.level,
        } : undefined;
        const deltaA = beforeA && afterA ? { x: afterA.x - beforeA.x, y: afterA.y - beforeA.y } : undefined;
        const deltaB = beforeB && afterB ? { x: afterB.x - beforeB.x, y: afterB.y - beforeB.y } : undefined;
        const points = (authored || dormant) && beforeA && beforeB && afterA && afterB && deltaA && deltaB &&
          (deltaA.x !== 0 || deltaA.y !== 0 || deltaB.x !== 0 || deltaB.y !== 0)
          ? maintainAuthoredRoutePoints({
            points: connection.routePoints, endpointA: connection.endpointA, endpointB,
            beforeA, beforeB, afterA, afterB, segmentShape: connection.segmentShape,
            authored: authored && afterA.level === afterB.level,
          })
          : undefined;
        const demote = (levelChanges.has(connection.endpointA.room_number) ||
          levelChanges.has(endpointB.room_number)) && (connection.routing === "Automatic" || authored);
        if (points || demote && authored) restores.push({ id: connection.id, routing: connection.routing, points });
        if (!demote) continue;
        await mutation.setConnection(connection.id, {
          routing: "Simple",
          ...(authored ? {} : { segment_shape: "Direct", route_points: [] }),
        });
        connection.routing = "Simple";
        if (!authored) { connection.segmentShape = "Direct"; connection.routePoints = []; }
      }
      if (updates.length > 0) {
        const coordinateStartedAt = performance.now();
        await this.#whileCurrentRun(runGeneration, () => mutation.updateRooms([...updates]));
        coordinateWriteMs += performance.now() - coordinateStartedAt;
        for (const [number, fields] of updates) {
          const room = area.roomsByNumber.get(number);
          if (room) room.position = roundedPosition(
            fields.x ?? room.position.x, fields.y ?? room.position.y, fields.level ?? room.position.level,
          );
        }
      }
      // The host maintains stored points after room moves, including points
      // explicitly set in that envelope. Generate final routes afterward so
      // a shared endpoint translation cannot shift a new detour twice.
      routeAfterCommit = updates.length > 0 || await this.#syncAreaConnectionRoutes(
        area, area.roomsByNumber, positions, runGeneration, mutation, routeAmendments, routeScope,
      );
    }, description));
    // Topology growth plans before creating its new rooms. Its assignments
    // must keep the registered mirror they will populate afterward. With no
    // moves, route sync already updated that mirror and no host point
    // maintenance needs to be read back.
    if (updates.length === 0 && !routeAfterCommit && restores.length === 0) {
      if (timings) timings.routeWriteMs += performance.now() - routeStartedAt;
      return area;
    }
    // The host may translate stored authored points when both endpoints move together.
    let committed = this.#hydrateArea(mapper.getAreaById(area.id), true);
    if (restores.length > 0) {
      await this.#whileCurrentRun(runGeneration, () => this.#mutateArea(area.id, async (mutation) => {
        for (const restore of restores) {
          const connection = committed.connections.get(connectionMirrorKey(restore.id));
          if (!connection) continue;
          await mutation.setConnection(restore.id, {
            routing: connection.kind === "CrossLevel" ? "Simple" : restore.routing,
            ...(restore.points ? { route_points: restore.points } : {}),
          });
        }
      }, `Preserve authored NukeFire routes in ${area.name}`));
      committed = this.#hydrateArea(mapper.getAreaById(area.id), true);
    }
    if (routeAfterCommit) {
      await this.#routeConnectionsAfterCommit(
        committed, committed.roomsByNumber, positions, runGeneration, routeAmendments,
      );
      committed = this.#hydrateArea(mapper.getAreaById(area.id), true);
    }
    // Progressive callers retain their resident objects for reconciliation.
    // Refresh their fields as well as the mapper cache's newly hydrated mirror.
    const retainedRooms = new Map(area.roomsByNumber);
    area.roomsByNumber.clear();
    for (const [number, room] of committed.roomsByNumber) {
      const existing = retainedRooms.get(number);
      this.#registerRoom(area, existing ? Object.assign(existing, room) : room);
    }
    area.name = committed.name;
    area.storage = committed.storage;
    area.polishPending = committed.polishPending;
    area.polishMemo = committed.polishMemo;
    area.polishSeams = committed.polishSeams;
    area.connections = committed.connections;
    this.#registerArea(area);
    if (timings) {
      timings.coordinateWriteMs += coordinateWriteMs;
      timings.routeWriteMs += performance.now() - routeStartedAt - coordinateWriteMs;
    }
    const refreshed = this.#refreshMovedCurrentRoom(
      area, updates.map(([number]) => ({ id: residentId(number) })), runGeneration,
    );
    // A manual command can select a room without a Room.Info observation in the owner.
    const current = mapper.getCurrentLocation();
    if (!refreshed && current?.room !== undefined && current.area === area.id &&
      updates.some(([number]) => number === current.room)) mapper.setCurrentLocation(current.area, current.room);
    return area;
  }

  #logDecision(record: DecisionLogRecord): void {
    const error = this.#decisionLogger.append(record);
    if (!error) {
      this.#lastDecisionLogError = "";
      return;
    }
    if (error === this.#lastDecisionLogError) return;
    this.#lastDecisionLogError = error;
    echo(`[nukefire-mapper] ${error}`);
  }

  #mutationError(error: unknown): Record<string, unknown> {
    const committedOperations = typeof error === "object" && error !== null
      ? (error as { committedOperations?: unknown }).committedOperations
      : undefined;
    return {
      message: error instanceof Error ? error.message : String(error),
      stack: error instanceof Error ? error.stack : undefined,
      ...(Array.isArray(committedOperations) ? { committedOperations } : {}),
    };
  }

  async #directMutation<T>(
    areaId: AreaId | undefined,
    api: string,
    description: string,
    callback: () => Promise<T>,
    summarize: (result: T) => unknown = (result) => result,
  ): Promise<T> {
    const mutationId = ++this.#mutationSequence;
    const startedAt = performance.now();
    this.#logDecision({
      kind: "mutation-start",
      mutationId,
      api,
      areaId,
      description,
      queuedSnapshots: this.#latencyLanes.pendingTopologyCount,
    });
    try {
      const result = await callback();
      this.#logDecision({
        kind: "mutation-complete",
        mutationId,
        api,
        areaId,
        description,
        result: summarize(result),
        durationMs: performance.now() - startedAt,
      });
      return result;
    } catch (error) {
      if (error instanceof ObsoleteNukeFireMapperRunError) throw error;
      this.#logDecision({
        kind: "mutation-error",
        mutationId,
        api,
        areaId,
        description,
        error: this.#mutationError(error),
        durationMs: performance.now() - startedAt,
      });
      throw error;
    }
  }

  /**
   * Draft callbacks update the VM-owned mirror so later writes in the same batch
   * see their predecessors. If submission fails (including after an oversized
   * batch partially commits), rebuild that mirror from the mapper's durable
   * state before allowing the serialized mapping loop to continue.
   */
  async #mutateArea(
    areaId: AreaId,
    callback: (mutation: AreaMutator) => void | Promise<void>,
    description: string,
  ): Promise<void> {
    const mutationId = ++this.#mutationSequence;
    const startedAt = performance.now();
    let draftCompleted = false;
    this.#logDecision({
      kind: "mutation-start",
      mutationId,
      api: "mutateArea",
      areaId,
      description,
      queuedSnapshots: this.#latencyLanes.pendingTopologyCount,
    });
    try {
      const operationIds = await mapper.mutateArea(areaId, async (mutation) => {
        await callback(mutation);
        draftCompleted = true;
        this.#logDecision({
          kind: "mutation-draft-complete",
          mutationId,
          api: "mutateArea",
          areaId,
          description,
          durationMs: performance.now() - startedAt,
        });
      }, { description });
      this.#logDecision({
        kind: "mutation-complete",
        mutationId,
        api: "mutateArea",
        areaId,
        description,
        operationIds,
        durationMs: performance.now() - startedAt,
      });
    } catch (error) {
      if (error instanceof ObsoleteNukeFireMapperRunError) throw error;
      this.#logDecision({
        kind: "mutation-error",
        mutationId,
        api: "mutateArea",
        areaId,
        description,
        phase: draftCompleted ? "submission" : "draft",
        error: this.#mutationError(error),
        durationMs: performance.now() - startedAt,
      });
      try {
        this.#hydrateArea(mapper.getAreaById(areaId), true);
      } catch {
        // Preserve the original mutation failure if recovery itself cannot read.
      }
      throw error;
    }
  }

  async #persistAreaPolishState(
    area: AreaMirror,
    event: Readonly<AreaPolishEvent>,
    runGeneration?: number,
  ): Promise<void> {
    const transition = reduceAreaPolishState(area.polishPending, event);
    const memoTransition = reduceAreaPolishMemo(area.polishMemo, event);
    const seamsTransition = reduceAreaPolishSeams(area.polishSeams, event);
    const writes: [string, string][] = [];
    if (transition.propertyValue !== undefined) {
      writes.push([AREA_POLISH_PENDING_PROPERTY, transition.propertyValue]);
    }
    if (memoTransition.propertyValue !== undefined) {
      writes.push([
        AREA_POLISH_EXHAUSTED_FINGERPRINT_PROPERTY,
        memoTransition.propertyValue,
      ]);
    }
    if (seamsTransition.propertyValue !== undefined) {
      writes.push([AREA_POLISH_SEAMS_PROPERTY, seamsTransition.propertyValue]);
    }
    if (writes.length > 0) {
      const persist = () => this.#mutateArea(
        area.id,
        async (mutation) => {
          for (const [name, value] of writes) {
            await mutation.setAreaProperty(name, value);
          }
        },
        `${transition.pending ? "Mark" : "Clear"} passive NukeFire layout polish for ${area.name}`,
      );
      if (runGeneration === undefined) await persist();
      else await this.#whileCurrentRun(runGeneration, persist);
    }
    area.polishPending = transition.pending;
    area.polishMemo = memoTransition.memo;
    area.polishSeams = seamsTransition.seams;
    this.#logDecision({
      kind: "layout-polish-state",
      area: { id: area.id, name: area.name },
      event: event.kind,
      pending: transition.pending,
      exhaustedMemo: memoTransition.memo?.kind === "contexts",
      exhaustedContexts: memoTransition.memo?.kind === "contexts"
        ? memoTransition.memo.settlements.length
        : 0,
      seams: seamsTransition.seams.length,
      terminalReason: event.kind === "polish-interrupted"
        ? event.reason
        : event.kind === "polish-completed" && memoTransition.memo?.kind === "contexts"
        ? memoTransition.memo.settlements.at(-1)?.terminalReason
        : undefined,
      propertyChanged: writes.length > 0,
    });
    if (event.kind !== "polish-completed") return;
    const reason = polishNotSearchedReason(event.report);
    if (reason === undefined) return;
    this.#logDecision({
      kind: "layout-polish-not-searched",
      area: { id: area.id, name: area.name },
      reason,
      cutoff: event.report?.cutoff,
      // A memoized context is skipped until the map's geometry changes.
      memoized: event.context !== undefined &&
        polishRetrySuppressed(memoTransition.memo, event.context),
    });
  }

  async #ensureLocalAtlas(runGeneration: number): Promise<Atlas | undefined> {
    this.#assertCurrentRun(runGeneration);
    if (this.#options.storage !== "local") return undefined;

    const existing = this.#localAtlasUpsert;
    if (existing && this.#localAtlasUpsertGeneration !== runGeneration) {
      try {
        await existing;
      } catch {
        // The previous ownership run is expected to reject its stale upsert.
      }
      this.#assertCurrentRun(runGeneration);
      return await this.#ensureLocalAtlas(runGeneration);
    }

    const upsert = existing ?? upsertLocalNukeFireAtlas({
      listAtlases: () => this.#whileCurrentRun(runGeneration, () => mapper.listAtlases()),
      createAtlas: (name, options) => this.#whileCurrentRun(
        runGeneration,
        () => this.#directMutation(
          undefined,
          "createAtlas",
          `Create local atlas ${name}`,
          () => mapper.createAtlas(name, options),
          (atlas) => createdAtlasDecisionSummary(atlas, options.storage),
        ),
      ),
    });
    if (!existing) {
      this.#localAtlasUpsert = upsert;
      this.#localAtlasUpsertGeneration = runGeneration;
    }
    try {
      return await this.#whileCurrentRun(runGeneration, () => upsert);
    } finally {
      if (this.#localAtlasUpsert === upsert) {
        this.#localAtlasUpsert = undefined;
        this.#localAtlasUpsertGeneration = undefined;
      }
    }
  }

  #assertCurrentRun(runGeneration: number): void {
    assertCurrentMapperRun(this.#started, this.#runGeneration, runGeneration);
  }

  #setCurrentRoom(
    room: RoomMirror,
    observation: Readonly<CurrentLocationObservation>,
    runGeneration: number,
    forceMapRefresh = false,
  ): void {
    if (!this.#currentLocationFreshness.isCurrent(observation)) return;
    const area = this.#areasById.get(areaIdKey(room.areaId));
    if (area) {
      const entry = this.#polishEntries.observe(
        areaKey(area),
        areaPolishNeedsContextEvaluation(area.polishPending, area.polishMemo),
        this.#options.updateCoordinates,
      );
      if (entry.previousAreaKey) {
        this.#deferredReflowAreas.delete(entry.previousAreaKey);
      }
      // A fresh visit restores the resume allowance alongside the entry retry.
      if (entry.entered) this.#quietResumeBudget.reset(areaKey(area));
      if (entry.retry) {
        // The exact suppression key needs the quiet lane's canonical chart,
        // anchor, edges, and scaled budgets. Schedule the attempt here, then
        // let #planAssignments skip it before any Worker work if that full
        // context is already memoized.
        this.#deferredReflowAreas.add(areaKey(area));
        this.#logDecision({
          kind: "layout-polish-retry",
          area: { id: area.id, name: area.name },
          vnum: observation.vnum,
        });
      }
    }
    const key = `${areaIdKey(room.areaId)}:${room.roomNumber}`;
    const locationChanged = key !== this.#currentLocation;
    if (!locationChanged && !forceMapRefresh) return;
    this.#assertCurrentRun(runGeneration);
    mapper.setCurrentLocation(room.areaId, room.roomNumber);
    this.#currentLocation = key;
    if (!locationChanged) return;
    this.#logDecision({
      kind: "current-location",
      areaId: room.areaId,
      roomNumber: room.roomNumber,
      vnum: observation.vnum,
    });
  }

  #followCachedCurrentRoom(
    observation: Readonly<CurrentLocationObservation>,
  ): void {
    const room = this.#roomsByVnum.get(observation.vnum);
    if (!room) return;
    try {
      this.#setCurrentRoom(room, observation, this.#runGeneration);
    } catch {
      // A stale external map mutation can invalidate the mirror. The queued
      // authoritative path refreshes and retries it without dropping data.
    }
  }

  #cachedCurrentRoom(vnum: number): RoomMirror | undefined {
    const room = this.#roomsByVnum.get(vnum);
    const area = room && this.#areasById.get(areaIdKey(room.areaId));
    return room && area && this.#inTier(area)
      ? room
      : undefined;
  }

  #hydrateCurrentRoom(
    vnum: number,
    scanConfiguredAreas: boolean,
  ): RoomMirror | undefined {
    const cached = this.#cachedCurrentRoom(vnum);
    if (cached) return cached;

    const externalId = externalRoomId(vnum);
    const hostRoom = mapper.findRoomByExternalId(externalId);
    if (hostRoom) {
      const hostArea = mapper.getAreaById(hostRoom.area_id);
      if (this.#inTier(hostArea)) {
        const area = this.#hydrateArea(hostArea);
        const room = [...area.roomsByNumber.values()].find(
          (candidate) => candidate.vnum === vnum,
        );
        if (room) return room;
      }
    }

    if (!scanConfiguredAreas) return undefined;

    // Room.Info can be retained without a matching Map.Local snapshot. In
    // that case no topology pass will perform the configured-tier fallback.
    for (const hostArea of mapper.areas) {
      if (!this.#inTier(hostArea)) continue;
      const area = this.#hydrateArea(hostArea);
      const room = [...area.roomsByNumber.values()].find(
        (candidate) => candidate.vnum === vnum,
      );
      if (room) return room;
    }
    return undefined;
  }

  #followCurrentRoomAfterRefresh(runGeneration: number): void {
    try {
      this.#currentLocationFreshness.publishIfCurrent(
        (vnum) => this.#hydrateCurrentRoom(
          vnum,
          this.#lastSnapshot?.center !== vnum,
        ),
        (room, observation) => {
          this.#setCurrentRoom(room, observation, runGeneration);
        },
      );
    } catch {
      // An external map mutation can invalidate a host handle between refresh
      // and hydration. The next authoritative snapshot retries from the host.
    }
  }

  #refreshMovedCurrentRoom(
    area: AreaMirror,
    reconciled: readonly { readonly id: string }[],
    runGeneration: number,
  ): boolean {
    return this.#currentLocationFreshness.publishIfCurrent(
      (vnum) => {
        const room = this.#roomsByVnum.get(vnum);
        if (!room || !sameAreaId(room.areaId, area.id)) return undefined;
        return reconciled.some((update) => update.id === residentId(room.roomNumber))
          ? room
          : undefined;
      },
      (room, observation) => {
        this.#setCurrentRoom(room, observation, runGeneration, true);
      },
    );
  }

  #observeCurrentRoom(vnum: number): void {
    const observation = this.#currentLocationFreshness.observe(vnum);
    if (observation) this.#followCachedCurrentRoom(observation);
  }

  #observeSnapshotCurrentRoom(snapshot: NukeFireMapLocal): void {
    const observation = this.#currentLocationFreshness.observe(snapshot.center);
    if (!observation) return;
    this.#snapshotCurrentLocations.set(snapshot, observation);
    this.#followCachedCurrentRoom(observation);
  }

  #snapshotCurrentRoom(
    snapshot: NukeFireMapLocal,
  ): CurrentLocationObservation | undefined {
    return this.#snapshotCurrentLocations.get(snapshot);
  }

  async #whileCurrentRun<T>(runGeneration: number, operation: () => Promise<T>): Promise<T> {
    return await whileCurrentMapperRun(
      runGeneration,
      () => ({ started: this.#started, generation: this.#runGeneration }),
      operation,
    );
  }

  #reloadAreaMirrors(): void {
    this.#zoneMaps.clear();
    this.#areasById.clear();
    this.#roomsByVnum.clear();
    for (const area of mapper.areas) this.#hydrateArea(area);
  }

  #assertLivePlanningFingerprint(
    area: AreaMirror,
    expected: string,
    phase: StalePlanPhase,
  ): void {
    let actual: string;
    try {
      actual = livePlanningFingerprint(mapper.getAreaById(area.id));
    } catch {
      throw new StaleNukeFireLayoutPlanError(area, phase);
    }
    if (actual !== expected) throw new StaleNukeFireLayoutPlanError(area, phase);
  }

  /** Waits for the session's maps; sessions sharing local maps see each other's writes without a refresh. */
  async #awaitMapsReady(): Promise<void> {
    if (this.#areasReady) return;
    const generation = this.#runGeneration;
    await mapper.ready();
    if (this.#runGeneration === generation && !this.#areasReady) {
      this.#areasReady = true;
      this.#followCurrentRoomAfterRefresh(generation);
    }
  }

  start(): void {
    if (this.#started) return;
    this.#started = true;
    this.#latencyLanes.start();
    const runGeneration = this.#runGeneration;

    this.#logDecision({
      kind: "session-start",
      options: { ...this.#options },
    });
    if (this.#decisionLogger.path) {
      echo(`[nukefire-mapper] mapping decisions: ${this.#decisionLogger.path}`);
    }

    // A package can start before the session's initial map load. Zone
    // resolution looks maps up by what they hold, so it waits for them.
    const mapsReady = this.#awaitMapsReady();
    void mapsReady.catch((caught) => {
      const message = caught instanceof Error ? caught.message : String(caught);
      echo(`[nukefire-mapper] failed to load existing maps: ${message}`);
    });

    // The atlas is part of mapper initialization, rather than a side effect of
    // receiving the first Map.Local snapshot. Let the maps finish loading
    // first so an older catalogue publication cannot hide the new atlas.
    // Area creation below awaits this same in-flight upsert, so startup and
    // mapping cannot create duplicates.
    void afterAreaRefresh(
      mapsReady,
      () => this.#ensureLocalAtlas(runGeneration),
    ).catch((caught) => {
      if (caught instanceof ObsoleteNukeFireMapperRunError) return;
      const message = caught instanceof Error ? caught.message : String(caught);
      echo(`[nukefire-mapper] failed to create local atlas: ${message}`);
    });

    this.#subscriptions.push(
      watchMessage("Room.Info", (info) => {
        this.#lastRoomInfo = info ? clone(info) : undefined;
        if (info && isUsableVnum(info.num)) this.#observeCurrentRoom(info.num);
        if (info && this.#lastSnapshot?.center === info.num) {
          this.#enqueue(this.#lastSnapshot);
        }
      }),
      onMessage("NukeFire.Map.Local", (snapshot) => {
        const stable = clone(snapshot);
        this.#lastSnapshot = stable;
        this.#enqueue(stable);
      }),
    );

    // onMessage preserves every future arrival but intentionally has no
    // replay. Rebuild retained state on every start so an ownership pause
    // cannot leave the previous run's room or snapshot authoritative. Seed
    // Map.Local first, then let direct Room.Info break a retained-state tie.
    const currentRoom = nukefire.value?.Room?.Info;
    const current = nukefire.value?.NukeFire?.Map?.Local;
    const retainedSnapshot = current ? clone(current) : undefined;
    this.#lastRoomInfo = currentRoom ? clone(currentRoom) : undefined;
    this.#lastSnapshot = retainedSnapshot;
    if (retainedSnapshot) this.#enqueue(retainedSnapshot);
    if (currentRoom && isUsableVnum(currentRoom.num)) {
      this.#observeCurrentRoom(currentRoom.num);
    }
  }

  stop(): void {
    for (const subscription of this.#subscriptions.splice(0)) subscription.off();
    this.#tidy?.abort();
    this.#quietPlanner?.close();
    this.#quietPlanner = undefined;
    this.#started = false;
    this.#runGeneration += 1;
    this.#latencyLanes.stop();
    this.#zoneMaps.clear();
    this.#settledZones.clear();
    this.#areasById.clear();
    this.#roomsByVnum.clear();
    this.#plannedTopology.clear();
    this.#reconciledPortAreas.clear();
    this.#deferredReflowAreas.clear();
    this.#polishEntries.clear();
    this.#quietResumeBudget.clear();
    this.#seamPreviews.clear();
    this.#pendingVerticalLinks.clear();
    this.#currentLocationFreshness.clear();
    this.#currentLocation = "";
    this.#areasReady = false;
  }

  /**
   * Settles every zone the mapper's maps name, as visiting it would, then
   * polishes each of the mapper's maps in turn. `say` hears each zone and map
   * as it is checked, each combination, and every second how the current
   * polish is going. One tidy runs at a time, and the quiet polish waits
   * while it does. Mapping goes on meanwhile: the tidy has the maps to itself
   * only while it reads or writes one, and it searches on a planner of its
   * own, never holding up the one mapping plans on. `stopTidy` ends it; a map
   * it was polishing keeps the best layout already on it.
   */
  async tidyAllMaps(say: (line: string) => void): Promise<void> {
    if (this.#tidy) {
      say("Already tidying the maps; nfmap stop stops it.");
      return;
    }
    if (!this.#started || !this.#areasReady) {
      say("The mapper has not loaded the maps yet.");
      return;
    }
    const controller = new AbortController();
    this.#tidy = controller;
    const runGeneration = this.#runGeneration;
    const startedAt = performance.now();
    const planner = createLayoutPlanner();
    try {
      const combined = await this.#tidyZones(say, controller.signal, runGeneration);
      const { checked, improved } = await this.#tidyPolishAll(
        say,
        controller.signal,
        runGeneration,
        planner,
      );
      say(
        `Done in ${elapsedClock(performance.now() - startedAt)}: ` +
          `${counted(combined, "zone")} combined, ${improved} of ${counted(checked, "map")} improved.`,
      );
    } catch (error) {
      if (controller.signal.aborted) {
        say(`Stopped after ${elapsedClock(performance.now() - startedAt)}.`);
      } else if (error instanceof ObsoleteNukeFireMapperRunError) {
        say("Stopped: the mapper restarted.");
      } else {
        say(`Stopped by an error: ${error instanceof Error ? error.message : String(error)}`);
      }
    } finally {
      planner.close();
      if (this.#tidy === controller) this.#tidy = undefined;
    }
  }

  /** The quiet polish's planner, started with its first search. */
  #quietPolishPlanner(): LayoutPlanner {
    return this.#quietPlanner ??= createLayoutPlanner();
  }

  /** Stops the tidy in progress; false when none is running. */
  stopTidy(): boolean {
    const tidy = this.#tidy;
    if (!tidy || tidy.signal.aborted) return false;
    tidy.abort();
    return true;
  }

  /** The mapper's own maps in its storage tier. */
  #managedAreas(): Area[] {
    const { managed } = NUKEFIRE_MARKS;
    return mapper.areas.filter((area) =>
      area.storage === this.#options.storage && area.data(managed.property) === managed.value
    );
  }

  /**
   * Settles each zone whose rooms the mapper's maps hold under the name its
   * maps give it, skipping zones settled this session and zones only
   * placeholders hold. Each settles with the maps to itself, as a visit's
   * would. Returns how many zones it gathered rooms of.
   */
  async #tidyZones(
    say: (line: string) => void,
    signal: AbortSignal,
    runGeneration: number,
  ): Promise<number> {
    const maps = this.#managedAreas();
    const zones = new Set<number>();
    for (const area of maps) {
      for (const room of this.#hydrateArea(area).roomsByNumber.values()) {
        const zone = Number(room.zone);
        if (room.zone && Number.isSafeInteger(zone)) zones.add(zone);
      }
    }
    say(`Checking ${counted(zones.size, "zone")} in ${counted(maps.length, "map")}.`);
    let combined = 0;
    for (const zone of [...zones].sort((a, b) => a - b)) {
      assertNotAborted(signal);
      const name = zoneNameFromMaps(zone, readZoneFacts(zone, this.#zoneContext(runGeneration)));
      if (name === undefined) {
        say(`Zone ${zone}: only placeholder maps hold it, so it settles when you visit it.`);
        continue;
      }
      const into = this.#names.resolve(name)?.display ?? name;
      const label = into === name ? `Zone ${zone} (${name})` : `Zone ${zone} (${name}, in ${into})`;
      if (this.#settledZones.has(zone)) {
        say(`${label}: settled already this session.`);
        continue;
      }
      say(`${label}: checking.`);
      const settlement = await this.#latencyLanes.exclusive(() =>
        this.#settleZone(zone, name, [], runGeneration)
      );
      if (settlement?.kind === "retry") {
        say(`${label}: its maps were busy, so it settles when you next visit it.`);
      } else if (settlement?.kind === "player") {
        const map = mapper.areas.find((area) => sameAreaId(area.id, settlement.map))?.name;
        say(`${label}: most of it is in a map you made${map ? `, ${map}` : ""}, so it stays there.`);
      } else if (
        settlement?.kind === "settled" &&
        (settlement.deleted.length > 0 || settlement.changed.length > 1)
      ) {
        combined += 1;
      }
    }
    return combined;
  }

  /**
   * Polishes each of the mapper's maps in turn: how many had anything to
   * polish, and how many of those it improved.
   */
  async #tidyPolishAll(
    say: (line: string) => void,
    signal: AbortSignal,
    runGeneration: number,
    planner: LayoutPlanner,
  ): Promise<{ checked: number; improved: number }> {
    const areas = this.#managedAreas().sort((a, b) => a.name.localeCompare(b.name));
    say(`Polishing ${counted(areas.length, "map")}.`);
    let checked = 0;
    let improved = 0;
    for (const area of areas) {
      assertNotAborted(signal);
      const outcome = await this.#tidyPolish(area.id, say, signal, runGeneration, planner);
      if (outcome !== "skipped") checked += 1;
      if (outcome === "improved") improved += 1;
    }
    return { checked, improved };
  }

  /**
   * One map as the tidy plans it, read from the map as it is now: its rooms,
   * the exits between them, and the fingerprint its writes must still match.
   * Undefined when the map is gone.
   */
  #tidyPolishInput(areaId: AreaId): {
    area: AreaMirror;
    residents: LayoutResident[];
    edges: LayoutEdge[];
    fingerprint: string;
  } | undefined {
    let area: AreaMirror;
    try {
      area = this.#hydrateArea(mapper.getAreaById(areaId), true);
    } catch {
      return undefined;
    }
    const residents: LayoutResident[] = [];
    const idByRoomNumber = new Map<RoomNumber, string>();
    for (const room of area.roomsByNumber.values()) {
      const id = residentId(room.roomNumber);
      idByRoomNumber.set(room.roomNumber, id);
      residents.push({
        id,
        position: { ...room.position },
        movable: room.vnum !== undefined && !room.layoutLocked,
      });
    }
    const edges: LayoutEdge[] = [];
    const edgeKeys = new Set<string>();
    for (const room of area.roomsByNumber.values()) {
      const from = idByRoomNumber.get(room.roomNumber);
      if (!from || room.vnum === undefined) continue;
      for (const exit of room.exits) {
        if (!exit.toAreaId || exit.toRoomNumber === null || !sameAreaId(exit.toAreaId, area.id)) continue;
        const to = idByRoomNumber.get(exit.toRoomNumber);
        if (!to || area.roomsByNumber.get(exit.toRoomNumber)?.vnum === undefined) continue;
        const key = `${from}>${to}:${exit.fromDirection}`;
        if (edgeKeys.has(key)) continue;
        edgeKeys.add(key);
        edges.push({ from, to, direction: exit.fromDirection as LayoutDirection });
      }
    }
    return { area, residents, edges, fingerprint: mirrorPlanningFingerprint(area) };
  }

  /**
   * Polishes one whole map as the quiet polish does, without a chart: the
   * standard pass and then the constraint repair, applying better layouts as
   * they come and the final one at the end, and saying every second how it
   * goes. The search runs while the player maps; reading the map, each write
   * and recording the polish each wait for mapping in flight and hold it
   * back. A map the player changes meanwhile keeps what is on it.
   */
  async #tidyPolish(
    areaId: AreaId,
    say: (line: string) => void,
    signal: AbortSignal,
    runGeneration: number,
    planner: LayoutPlanner,
  ): Promise<"improved" | "unchanged" | "skipped"> {
    const input = await this.#latencyLanes.exclusive(async () => this.#tidyPolishInput(areaId));
    if (!input) return "skipped";
    const { area, residents, edges } = input;
    if (!residents.some((resident) => resident.movable) || edges.length === 0) {
      say(`${area.name}: nothing to polish.`);
      return "skipped";
    }
    const currentQuality = measureIntegralLayoutQuality(
      new Map(residents.map((resident) => [resident.id, resident.position])),
      edges,
    );
    say(
      `Polishing ${area.name}: ${counted(residents.length, "room")}, ${counted(edges.length, "exit")}, ` +
        `now ${qualitySummary(currentQuality)} (wrong exits/blocked routes/crossings).`,
    );

    const policy = nukeFirePerfectConstraintRepairPolicy({
      residentCount: residents.length,
      edgeCount: edges.length,
    });
    const startedAt = performance.now();
    let fingerprint = input.fingerprint;
    let onMap: Readonly<LayoutQuality> = currentQuality;
    let applied = 0;
    let latest: LayoutPlannerProgress["snapshot"] | undefined;
    let failure: unknown;
    const planning = new AbortController();
    const stopPlanning = (): void => planning.abort();
    signal.addEventListener("abort", stopPlanning, { once: true });
    if (signal.aborted) planning.abort();

    // Each better layout goes to the map as the mapper holds it then, which
    // mapping may have rebuilt since the search began, so long as the player
    // has not changed what the search planned.
    const apply = (plan: IntegralLayoutPlan): Promise<void> =>
      this.#latencyLanes.exclusive(async () => {
        assertNotAborted(planning.signal);
        if (compareLayoutQuality(plan.quality, onMap) <= 0) return;
        this.#assertLivePlanningFingerprint(area, fingerprint, "before applying layout");
        const live = this.#hydrateArea(mapper.getAreaById(areaId));
        const rooms = new Map(
          [...live.roomsByNumber.values()].map((room) => [residentId(room.roomNumber), room]),
        );
        const updates: [RoomNumber, UpdateRoomParams][] = reconciliationUpdates(
          rooms,
          plan.positions,
          (room) => room.roomNumber,
        ).map((update) => [update.key, {
          x: update.position.x,
          y: update.position.y,
          level: update.position.level,
        }]);
        const committed = await this.#commitLayoutMoves(
          live, plan.positions, updates, runGeneration, `Tidy NukeFire area ${live.name}`, plan.routeAmendments,
        );
        fingerprint = mirrorPlanningFingerprint(committed);
        onMap = plan.quality;
        applied += 1;
      });
    // Writes are paced as the quiet polish paces them, and a better layout
    // arriving meanwhile replaces the one waiting: the newest is written, never
    // a stale one. A failed write ends the polish of this map.
    const writes = new LatestValueQueue<IntegralLayoutPlan>(apply, (error) => {
      failure = error;
      planning.abort();
    }, { minIntervalMs: PROGRESSIVE_APPLY_FLOOR_MS });
    let offered: Readonly<LayoutQuality> = currentQuality;
    const offer = (plan: IntegralLayoutPlan): void => {
      if (compareLayoutQuality(plan.quality, offered) <= 0) return;
      offered = plan.quality;
      writes.push(plan);
    };
    const progress = setInterval(
      () => say(polishProgressLine(area.name, performance.now() - startedAt, latest, applied)),
      1_000,
    );
    let plan: IntegralLayoutPlan | undefined;
    try {
      plan = await planner.planIntegral({ residents, nodes: [], edges, allowExistingMoves: true }, {
        signal: planning.signal,
        currentQuality,
        constraintRepair: policy,
        plannerContext: { source: "nukefire:tidy", areaId: areaKey(area), areaName: area.name },
        onProgress: (update) => {
          latest = update.snapshot;
          if (update.improvement) offer(update.improvement);
        },
      });
      offer(plan);
    } catch {
      // A stopped tidy, a changed map or a failed planner: the outcome below
      // says which.
    } finally {
      clearInterval(progress);
      signal.removeEventListener("abort", stopPlanning);
    }
    // A stopped tidy writes nothing more; the write in progress finishes.
    if (signal.aborted) writes.discardPending();
    try {
      await writes.flush();
    } catch {
      // The failure is recorded; the outcome below reports it.
    }
    const elapsed = elapsedClock(performance.now() - startedAt);
    const outcome = applied > 0 ? "improved" : "unchanged";
    const kept = `it keeps ${qualitySummary(onMap)}`;
    if (signal.aborted) {
      say(`Stopped polishing ${area.name} after ${elapsed}; ${kept}.`);
      throw new Error("The tidy was stopped.");
    }
    if (failure instanceof ObsoleteNukeFireMapperRunError) throw failure;
    if (failure !== undefined) {
      say(
        failure instanceof StaleNukeFireLayoutPlanError
          ? `${area.name} changed while it was being polished; ${kept}.`
          : `${area.name}: ${failure instanceof Error ? failure.message : String(failure)}`,
      );
      return outcome;
    }
    if (!plan) {
      say(`${area.name}: the planner stopped after ${elapsed}; ${kept}.`);
      return outcome;
    }
    // The search covered the map as it is, unless the player changed it
    // after the last write; the polish it records must not hide that change.
    // It settles the map as an explicit perfect polish does.
    const report = plan.constraintRepair;
    try {
      await this.#latencyLanes.exclusive(async () => {
        this.#assertLivePlanningFingerprint(area, fingerprint, "before recording the polish");
        await this.#persistAreaPolishState(
          this.#hydrateArea(mapper.getAreaById(areaId)),
          {
            kind: "polish-completed",
            report,
            terminalReason: hasNoDefects(onMap) ? "perfect" : undefined,
            improved: applied > 0,
            context: perfectPolishContext(fingerprint, policy),
          },
          runGeneration,
        );
      });
    } catch (error) {
      if (!(error instanceof StaleNukeFireLayoutPlanError)) throw error;
      say(`${area.name} changed while it was being polished; ${kept}.`);
      return outcome;
    }
    say(
      applied > 0
        ? `Polished ${area.name} in ${elapsed}: ${qualitySummary(currentQuality)} -> ${qualitySummary(onMap)}.`
        : `${area.name} needed nothing (${elapsed}); it stays ${qualitySummary(onMap)}.`,
    );
    return outcome;
  }

  #enqueue(snapshot: NukeFireMapLocal): void {
    this.#latencyLanes.enqueue(snapshot);
  }

  async #runSnapshotLane(
    snapshot: NukeFireMapLocal,
    allowExistingReflow: boolean,
    signal?: AbortSignal,
  ): Promise<void> {
    const runGeneration = this.#runGeneration;
    await this.#syncSnapshot(snapshot, allowExistingReflow, runGeneration, signal);
    this.#lastError = "";
  }

  #reportSnapshotError(snapshot: NukeFireMapLocal, caught: unknown): void {
    if (caught instanceof ObsoleteNukeFireMapperRunError) return;
    const message = caught instanceof Error ? caught.message : String(caught);
    if (message === this.#lastError) return;
    this.#lastError = message;
    echo(`[nukefire-mapper] ${message}`);
    this.#logDecision({
      kind: "mapping-error",
      snapshot,
      error: {
        message,
        stack: caught instanceof Error ? caught.stack : undefined,
      },
    });
  }

  async #syncSnapshot(
    snapshot: NukeFireMapLocal,
    allowExistingReflow: boolean,
    runGeneration: number,
    signal?: AbortSignal,
  ): Promise<void> {
    assertNotAborted(signal);
    const plannedTopologyBefore = new Set(this.#plannedTopology);
    const deferredReflowBefore = new Set(this.#deferredReflowAreas);
    for (let attempt = 0; attempt < 2; attempt += 1) {
      assertNotAborted(signal);
      try {
        await this.#syncSnapshotAttempt(
          snapshot,
          allowExistingReflow,
          runGeneration,
          signal,
        );
        return;
      } catch (caught) {
        assertNotAborted(signal);
        if (!(caught instanceof StaleNukeFireLayoutPlanError)) throw caught;
        this.#assertCurrentRun(runGeneration);

        // A prior area in this attempt may have completed safely, but the
        // snapshot as a whole has not. Recompute its bookkeeping on retry.
        restoreSet(this.#plannedTopology, plannedTopologyBefore);
        restoreSet(this.#deferredReflowAreas, deferredReflowBefore);
        const retryAreaKey = this.#polishEntries.retryAreaKey;
        for (const key of this.#deferredReflowAreas) {
          if (key !== retryAreaKey) this.#deferredReflowAreas.delete(key);
        }
        if (retryAreaKey) this.#deferredReflowAreas.add(retryAreaKey);
        if (attempt > 0) {
          throw new Error(
            `${caught.message} again after one refresh; discarded the stale plan without writing its coordinates`,
          );
        }
        this.#reloadAreaMirrors();
        assertNotAborted(signal);
        this.#assertCurrentRun(runGeneration);
      }
    }
  }

  /*
   * Topology snapshots are serialized by SnapshotLatencyLanes. A distinct
   * full-reflow lane calls the same authoritative reconciliation only after a
   * quiet window, and supplies the sole cancelable Worker signal.
   */
  async #syncSnapshotAttempt(
    snapshot: NukeFireMapLocal,
    allowExistingReflow: boolean,
    runGeneration: number,
    signal?: AbortSignal,
  ): Promise<void> {
    assertNotAborted(signal);
    await this.#whileCurrentRun(runGeneration, () => this.#awaitMapsReady());
    assertNotAborted(signal);
    const startedAt = performance.now();
    if (!isUsableVnum(snapshot.center)) {
      throw new Error(`ignored Map.Local with invalid center ${snapshot.center}`);
    }

    const byVnum = new Map<number, NukeFireMapRoom>();
    for (const room of snapshot.rooms) {
      if (validRoom(room)) byVnum.set(room.vnum, room);
    }
    const centerSource = byVnum.get(snapshot.center);
    if (!centerSource) {
      throw new Error(`Map.Local omitted its center room #${snapshot.center}`);
    }

    const currentRoomInfo = this.#lastRoomInfo?.num === snapshot.center
      ? this.#lastRoomInfo
      : undefined;
    // The chart names the zone of every room it shows, stored with it or not.
    const chartedIn = (zone: number) =>
      [...byVnum.values()].filter((room) => room.zone === zone).map((room) => externalRoomId(room.vnum));
    // Room.Info names the area of the zone the player stands in. That is the
    // moment the zone's rooms can be gathered into its area's map, before
    // this snapshot's rooms are looked up or placed.
    if (currentRoomInfo) {
      await this.#settleZone(centerSource.zone, currentRoomInfo.area, chartedIn(centerSource.zone), runGeneration);
      assertNotAborted(signal);
    }
    const verticalExits = currentRoomInfo
      ? verticalExitObservations(currentRoomInfo.exits)
      : [];
    const supplementalLinks = verticalMapLinks(snapshot.center, verticalExits);
    for (const link of supplementalLinks) {
      this.#pendingVerticalLinks.set(verticalPendingKey(link), link);
    }
    const links: NukeFireMapLink[] = [];
    const linkKeys = new Set<string>();
    for (const link of [...snapshot.links, ...this.#pendingVerticalLinks.values()]) {
      const key = observedLinkKey(link);
      if (linkKeys.has(key)) continue;
      linkKeys.add(key);
      links.push(link);
    }

    const sources = [...byVnum.values()];
    const existing = new Map<number, RoomMirror>();
    for (const source of sources) {
      const room = this.#roomsByVnum.get(source.vnum);
      const area = room && this.#areasById.get(areaIdKey(room.areaId));
      if (room && area && this.#inTier(area)) {
        existing.set(source.vnum, room);
      }
    }

    // Hydrate an existing matching area at most once. Subsequent snapshots use
    // #roomsByVnum and never repeat these atomic host reads.
    if (existing.size < sources.length) {
      for (const source of sources) {
        if (existing.has(source.vnum)) continue;
        const cached = this.#roomsByVnum.get(source.vnum);
        const cachedArea = cached && this.#areasById.get(areaIdKey(cached.areaId));
        if (cached && cachedArea && this.#inTier(cachedArea)) {
          existing.set(source.vnum, cached);
          continue;
        }
        const hostRoom = mapper.findRoomByExternalId(externalRoomId(source.vnum));
        if (!hostRoom) continue;
        const hostArea = mapper.getAreaById(hostRoom.area_id);
        if (!this.#inTier(hostArea)) continue;
        this.#hydrateArea(hostArea);
        const room = this.#roomsByVnum.get(source.vnum);
        if (room) existing.set(source.vnum, room);
      }
    }
    // The global external-id index returns one of potentially several maps. If
    // it chose another storage tier, scan configured-tier areas once instead.
    if (existing.size < sources.length) {
      const wanted = new Set(sources.map((source) => source.vnum));
      for (const hostArea of mapper.areas) {
        if (existing.size >= wanted.size) break;
        if (!this.#inTier(hostArea)) continue;
        const area = this.#hydrateArea(hostArea);
        for (const room of area.roomsByNumber.values()) {
          if (room.vnum !== undefined && wanted.has(room.vnum) && !existing.has(room.vnum)) {
            existing.set(room.vnum, room);
          }
        }
      }
    }

    const areaByZone = new Map<number, AreaMirror>();
    for (const zone of new Set(sources.map((room) => room.zone))) {
      areaByZone.set(zone, await this.#zoneMap(zone, chartedIn(zone), runGeneration));
      assertNotAborted(signal);
    }

    const assignments: Assignment[] = sources.map((source) => {
      const indexedRoom = existing.get(source.vnum);
      // A known room stays where it is: border rooms appear in both zones'
      // charts, and re-creating one under its other zone would duplicate it.
      const area = (indexedRoom && this.#areasById.get(areaIdKey(indexedRoom.areaId))) ??
        areaByZone.get(source.zone);
      if (!area) throw new Error(`could not resolve an area for NukeFire zone ${source.zone}`);
      const room = indexedRoom && sameAreaId(indexedRoom.areaId, area.id)
        ? indexedRoom
        : [...area.roomsByNumber.values()].find((candidate) => candidate.vnum === source.vnum);
      return { source, area, room };
    });

    // Established current rooms do not depend on the reflow result. Reflect
    // movement immediately while the potentially expensive Worker plan runs.
    const establishedCurrent = assignments.find((assignment) => assignment.source.vnum === snapshot.center)?.room;
    const currentObservation = this.#snapshotCurrentRoom(snapshot);
    if (establishedCurrent && currentObservation) {
      this.#setCurrentRoom(establishedCurrent, currentObservation, runGeneration);
    }

    assertNotAborted(signal);
    const planningStartedAt = performance.now();
    const planning = await this.#planAssignments(
      assignments,
      snapshot,
      centerSource,
      links,
      allowExistingReflow,
      runGeneration,
      signal,
    );
    assertNotAborted(signal);
    this.#assertCurrentRun(runGeneration);
    const planningFinishedAt = performance.now();

    // Read before syncing, which gives each assignment its room.
    const addsRooms = assignments.some((assignment) => !assignment.room);
    const rooms = new Map<number, RoomMirror>();
    const assignmentsByArea = new Map<string, Assignment[]>();
    for (const assignment of assignments) {
      const key = areaIdKey(assignment.area.id);
      const group = assignmentsByArea.get(key) ?? [];
      group.push(assignment);
      assignmentsByArea.set(key, group);
    }
    for (const group of assignmentsByArea.values()) {
      assertNotAborted(signal);
      const area = group[0].area;
      // Invariant: the live fingerprint is verified wherever planner-derived
      // geometry can be written — before Worker planning, before every
      // progressive and final apply, and here before creating rooms at
      // planned positions. A plain walking group's plan is the identity over
      // mirror positions and its established-room coordinate writes are
      // clamped off, so re-reading and serializing the whole live area on
      // every step would protect nothing; those groups skip the check.
      if (group.some((assignment) => assignment.planned)) {
        this.#assertLivePlanningFingerprint(
          area,
          mirrorPlanningFingerprint(area),
          "before applying layout",
        );
      }
      await this.#whileCurrentRun(
        runGeneration,
        () => this.#mutateArea(area.id, async (mutation) => {
          for (const assignment of group) {
            const mapped = await this.#syncRoom(assignment, mutation, runGeneration);
            assertNotAborted(signal);
            rooms.set(assignment.source.vnum, mapped);
          }
        }, `Apply NukeFire rooms for ${area.name}`),
      );
      assertNotAborted(signal);
    }
    const roomsFinishedAt = performance.now();

    const current = rooms.get(snapshot.center);
    const currentWasRepositioned = assignments.some((assignment) =>
      assignment.source.vnum === snapshot.center && assignment.positionApplied === true
    );
    if (current && currentObservation) {
      this.#setCurrentRoom(current, currentObservation, runGeneration);
    }

    assertNotAborted(signal);
    const linksChanged = await this.#syncLinks(links, rooms, runGeneration);
    assertNotAborted(signal);
    this.#assertCurrentRun(runGeneration);
    for (const [key, link] of this.#pendingVerticalLinks) {
      const from = rooms.get(link.from) ?? this.#roomsByVnum.get(link.from);
      const to = rooms.get(link.to) ?? this.#roomsByVnum.get(link.to);
      const mapped = mapDirection(link.direction);
      if (to && exitLeadsTo(from && matchingExit(from, mapped), to)) {
        this.#pendingVerticalLinks.delete(key);
      }
    }
    const closedExitsChanged = await this.#syncClosedVerticalExits(
      verticalExits, rooms.get(snapshot.center), runGeneration,
    );
    assertNotAborted(signal);
    this.#assertCurrentRun(runGeneration);

    // Repeat the otherwise-deduplicated location once every write, links
    // included, has committed. SetPlayerLocation derives the MapView
    // translation from the room's coordinates, so the viewport follows a player
    // room the layout moved, and a map restyles what the player has not visited
    // when it hears the location, so it shows the rooms this snapshot added.
    if ((currentWasRepositioned || addsRooms || linksChanged || closedExitsChanged) && current && currentObservation) {
      this.#setCurrentRoom(current, currentObservation, runGeneration, true);
    }

    const finishedAt = performance.now();
    if (planning.plannedAreas > 0 || finishedAt - startedAt >= 100) {
      this.#logDecision({
        kind: "mapping-performance",
        center: snapshot.center,
        rooms: assignments.length,
        links: links.length,
        allowExistingReflow,
        queuedSnapshots: this.#latencyLanes.pendingTopologyCount,
        planning,
        durationMs: {
          total: finishedAt - startedAt,
          resolve: planningStartedAt - startedAt,
          planning: planningFinishedAt - planningStartedAt,
          roomWrites: roomsFinishedAt - planningFinishedAt,
          linkWrites: finishedAt - roomsFinishedAt,
        },
      });
    }
  }

  /** The map this zone's new rooms go to: its settled map, else the map holding it, else its placeholder. */
  async #zoneMap(zone: number, charted: readonly string[], runGeneration: number): Promise<AreaMirror> {
    const remembered = this.#zoneMaps.get(zone);
    if (remembered !== undefined) {
      try {
        return this.#hydrateArea(mapper.getAreaById(remembered));
      } catch {
        // The player deleted it; look the zone up again.
        this.#zoneMaps.delete(zone);
      }
    }
    const id = await this.#whileCurrentRun(
      runGeneration,
      () => glimpsedZoneMap(zone, this.#zoneContext(runGeneration), charted),
    );
    this.#zoneMaps.set(zone, id);
    return this.#hydrateArea(mapper.getAreaById(id));
  }

  /** On a zone's first named visit this run, gathers all of its rooms into the one map for its area name. */
  /** Settles `zone` under `areaName` once a run; undefined when it already settled or the name has no map. */
  async #settleZone(
    zone: number,
    areaName: string,
    charted: readonly string[],
    runGeneration: number,
  ): Promise<Settlement | undefined> {
    const name = this.#names.resolve(areaName);
    if (!name || this.#settledZones.has(zone)) return undefined;
    const settlement = await this.#whileCurrentRun(
      runGeneration,
      () => settleZone(zone, name, this.#zoneContext(runGeneration), charted),
    );
    if (settlement.kind === "retry") return settlement;
    this.#settledZones.add(zone);
    if (settlement.kind === "player") {
      this.#zoneMaps.set(zone, settlement.map);
      return settlement;
    }
    if (settlement.deleted.length > 0) this.#zoneMaps.clear();
    this.#zoneMaps.set(zone, settlement.into);
    for (const id of settlement.deleted) this.#forgetArea(id);
    for (const id of settlement.changed) this.#hydrateArea(mapper.getAreaById(id), true);
    return settlement;
  }

  #zoneContext(runGeneration: number): ZoneContext {
    return {
      names: this.#names,
      marks: NUKEFIRE_MARKS,
      storage: this.#options.storage,
      atlas: () => this.#ensureLocalAtlas(runGeneration),
      mutation: (area, api, description, call, summarize) =>
        this.#directMutation(area, api, description, call, summarize),
      // Merged sections are new geometry, exactly as a deferred topology
      // change is: polish is pending again and no fruitless context applies.
      // Their seams are remembered for the next polish to preview first.
      polish: (area, seams) =>
        this.#persistAreaPolishState(
          this.#hydrateArea(mapper.getAreaById(area), true),
          { kind: "topology-deferred", seams },
          runGeneration,
        ),
      log: (record) => this.#logDecision(record),
      notice: (text) => echo(`[nukefire-mapper] ${text}`),
    };
  }

  async #planAssignments(
    assignments: Assignment[],
    snapshot: Readonly<NukeFireMapLocal>,
    centerSource: Readonly<NukeFireMapRoom>,
    links: readonly NukeFireMapLink[],
    allowExistingReflow: boolean,
    runGeneration: number,
    signal?: AbortSignal,
  ): Promise<AssignmentPlanStats> {
    assertNotAborted(signal);
    this.#assertCurrentRun(runGeneration);
    const stats: AssignmentPlanStats = {
      plannedAreas: 0,
      topologyGrowthAreas: 0,
      movedRooms: 0,
      plannerMs: 0,
      coordinateWriteMs: 0,
      routeWriteMs: 0,
      batchCommitMs: 0,
    };
    const groups = new Map<string, Assignment[]>();
    for (const assignment of assignments) {
      const key = areaKey(assignment.area);
      const group = groups.get(key) ?? [];
      group.push(assignment);
      groups.set(key, group);
    }

    for (const group of groups.values()) {
      const area = group[0].area;
      const assignmentByVnum = new Map(group.map((assignment) => [assignment.source.vnum, assignment]));
      const residentRooms = new Map(area.roomsByNumber);
      for (const assignment of group) {
        if (assignment.room && sameAreaId(assignment.room.areaId, area.id)) {
          residentRooms.set(assignment.room.roomNumber, assignment.room);
        }
      }

      const assignmentIds = new Map<number, string>();
      for (const assignment of group) {
        assignmentIds.set(
          assignment.source.vnum,
          assignment.room ? residentId(assignment.room.roomNumber) : newRoomId(assignment.source.vnum),
        );
      }

      const residents: LayoutResident[] = [];
      const roomById = new Map<string, RoomMirror>();
      const idByRoomNumber = new Map<RoomNumber, string>();
      for (const room of residentRooms.values()) {
        const id = residentId(room.roomNumber);
        idByRoomNumber.set(room.roomNumber, id);
        roomById.set(id, room);
        residents.push({
          id,
          position: room.position,
          movable: room.vnum !== undefined && !room.layoutLocked,
        });
      }

      const layoutIdForVnum = (vnum: number): string | undefined => {
        const assignment = assignmentByVnum.get(vnum);
        if (assignment && sameAreaId(assignment.area.id, area.id)) return assignmentIds.get(vnum);
        const room = this.#roomsByVnum.get(vnum);
        return room && sameAreaId(room.areaId, area.id)
          ? idByRoomNumber.get(room.roomNumber)
          : undefined;
      };
      const knownRoomForVnum = (vnum: number): RoomMirror | undefined =>
        assignmentByVnum.get(vnum)?.room ?? this.#roomsByVnum.get(vnum);

      const introducesRoom = group.some((assignment) => !assignment.room);
      const introducedTopology = new Set<string>();
      for (const link of links) {
        if (!layoutIdForVnum(link.from) || !layoutIdForVnum(link.to)) continue;
        const from = knownRoomForVnum(link.from);
        const to = knownRoomForVnum(link.to);
        const mapped = mapDirection(link.direction);
        const forwardKey = topologyTraversalKey(link.from, link.to, mapped.command);
        if (!this.#plannedTopology.has(forwardKey) && !exitLeadsTo(from && matchingExit(from, mapped), to)) {
          introducedTopology.add(forwardKey);
        }

        if (link.bidirectional && mapped.opposite && mapped.reverseCommand) {
          const reverseMapped = {
            direction: mapped.opposite,
            command: mapped.reverseCommand,
            opposite: mapped.direction,
            reverseCommand: mapped.command,
          } satisfies MappedDirection;
          const reverseKey = topologyTraversalKey(link.to, link.from, reverseMapped.command);
          if (!this.#plannedTopology.has(reverseKey) && !exitLeadsTo(to && matchingExit(to, reverseMapped), from)) {
            introducedTopology.add(reverseKey);
          }
        }
      }
      const topologyGrowth = introducesRoom || introducedTopology.size > 0;
      const reconcilePorts = !this.#reconciledPortAreas.has(areaKey(area));
      const deferredReflow = this.#deferredReflowAreas.has(areaKey(area));
      const policy = reflowPolicy(
        topologyGrowth,
        deferredReflow,
        allowExistingReflow && this.#latencyLanes.pendingTopologyCount === 0,
        this.#options.updateCoordinates,
      );
      let runPlanner = policy.runPlanner;
      const { moveExisting } = policy;
      for (const assignment of group) {
        assignment.moveExisting = moveExisting;
        assignment.planned = runPlanner;
      }
      if (topologyGrowth) stats.topologyGrowthAreas += 1;
      if (policy.deferExistingReflow) {
        if (topologyGrowth) {
          await this.#persistAreaPolishState(area, { kind: "topology-deferred" }, runGeneration);
          assertNotAborted(signal);
          // Growth is fresh evidence that polish can gain ground here; the
          // fruitless-resume allowance starts over with it. Merely carrying
          // already-deferred work must preserve both settlement evidence and
          // the bounded resume budget.
          this.#quietResumeBudget.reset(areaKey(area));
        }
        if (this.#polishEntries.currentAreaKey === areaKey(area)) {
          // Arm the current visit before the rest of topology reconciliation.
          // If a later write fails after a partial commit, the next successful
          // snapshot can still promote this durable hint to the quiet lane.
          this.#polishEntries.markPending(areaKey(area), this.#options.updateCoordinates);
          this.#deferredReflowAreas.add(areaKey(area));
        }
      }

      const identityPlan = (): Pick<IntegralLayoutPlan, "positions" | "movedExisting"> => ({
        positions: new Map(residents.map((resident) => [resident.id, resident.position])),
        movedExisting: new Set<string>(),
      });
      // Tracks every resident coordinate durably written during this planning
      // operation, including transient progressive candidates. The outer
      // snapshot flow uses it to refresh the current marker after a final
      // reconciliation and to avoid overwriting already-applied positions.
      const appliedPositionIds = new Set<string>();
      let plan: Pick<IntegralLayoutPlan, "positions" | "movedExisting"> &
        Partial<Pick<IntegralLayoutPlan, "constraintRepair" | "routeAmendments">> = identityPlan();
      let polishContext: AreaPolishPlanningContext | undefined;
      // The whole-map polish ended no better than the seam preview on the
      // map, which therefore stays as the pass's result.
      let keptPreview = false;
      if (runPlanner) {
        const chartNodes: LayoutNode[] = group.map((assignment) => ({
          id: assignmentIds.get(assignment.source.vnum) as string,
          relative: roundedPosition(
            assignment.source.x - centerSource.x,
            assignment.source.y - centerSource.y,
            assignment.source.z - centerSource.z,
          ),
        }));

        const edges: LayoutEdge[] = [];
        const edgeKeys = new Set<string>();
        const pushEdge = (from: string, to: string, direction: LayoutDirection): void => {
          const key = `${from}>${to}:${direction}`;
          if (edgeKeys.has(key)) return;
          edgeKeys.add(key);
          edges.push({ from, to, direction });
        };

        for (const room of residentRooms.values()) {
          const from = idByRoomNumber.get(room.roomNumber);
          if (!from || room.vnum === undefined) continue;
          for (const exit of room.exits) {
            if (!exit.toAreaId || exit.toRoomNumber === null || !sameAreaId(exit.toAreaId, area.id)) continue;
            const to = idByRoomNumber.get(exit.toRoomNumber);
            const toRoom = residentRooms.get(exit.toRoomNumber);
            if (to && toRoom && toRoom.vnum !== undefined) {
              pushEdge(from, to, exit.fromDirection as LayoutDirection);
            }
          }
        }

        for (const link of links) {
          const from = layoutIdForVnum(link.from);
          const to = layoutIdForVnum(link.to);
          if (!from || !to) continue;
          const mapped = mapDirection(link.direction);
          pushEdge(from, to, mapped.direction as LayoutDirection);
          if (link.bidirectional && mapped.opposite) {
            pushEdge(to, from, mapped.opposite as LayoutDirection);
          }
        }

        const centerId = assignmentIds.get(snapshot.center);
        const establishedLevels = new Map<string, number>();
        for (const room of residentRooms.values()) {
          const id = idByRoomNumber.get(room.roomNumber);
          if (id) establishedLevels.set(id, room.position.level);
        }
        // NukeFire may flow an up/down destination on its source's z plane;
        // this mapper always stacks vertical traversals across map levels. A
        // cross-level endpoint is necessarily a resident outside Map.Local,
        // so all durable residents are available as immutable level seeds.
        const nodes = stackVerticalTraversals(
          chartNodes,
          edges,
          establishedLevels,
          centerId,
          Number.isSafeInteger(snapshot.plane) ? snapshot.plane : 0,
        );
        const constraintRepairPolicy = nukeFireAutomaticConstraintRepairPolicy({
          residentCount: residents.length,
          edgeCount: edges.length,
        });
        const startingFingerprint = mirrorPlanningFingerprint(area);
        // A cached mirror cannot authorize suppression: editor/package writes
        // may have changed live geometry without touching NukeFire's snapshot.
        this.#assertLivePlanningFingerprint(area, startingFingerprint, "before Worker planning");
        // A map with seams first shows a quick polish of them, and its
        // whole-map polish plans from the geometry that seam round began
        // with, whose only changes since are the round's own writes. The live
        // check above proves the latter for a round an earlier pass began.
        const seamPass = moveExisting && !introducesRoom && area.polishSeams.length > 0
          ? this.#seamPreviews.begin(
            areaKey(area),
            areaPolishSeamsPropertyValue(area.polishSeams),
            {
              positions: new Map(residents.map((resident) => [resident.id, resident.position])),
              fingerprint: startingFingerprint,
            },
            () => seamRegion(
              new Map(residents.map((resident) => [resident.id, resident.position])),
              edges,
              area.polishSeams.map((roomNumber) => residentId(roomNumber)),
            ),
          )
          : undefined;
        const base = seamPass?.base;
        // The whole-map request: each resident where the base has it, and as
        // movable as it is now; the fingerprint shows movability unchanged.
        const planningResidents = base === undefined ? residents : residents.map((resident) => {
          const position = base.positions.get(resident.id);
          return position ? { ...resident, position } : resident;
        });
        const candidatePolishContext = createAreaPolishPlanningContext({
          geometryFingerprint: base?.fingerprint ?? startingFingerprint,
          centerId,
          nodes,
          edges,
          searchForPerfectLayouts: false,
          policy: constraintRepairPolicy,
        });
        const polishEligibility = areaPolishEligibility(area.polishMemo, candidatePolishContext);
        if (moveExisting && !polishEligibility.eligible) {
          this.#polishEntries.consumeRetry(areaKey(area));
          this.#deferredReflowAreas.delete(areaKey(area));
          runPlanner = false;
          for (const assignment of group) assignment.planned = false;
          this.#logDecision({
            kind: "layout-polish-retry-skipped",
            area: { id: area.id, name: area.name },
            vnum: snapshot.center,
            memoContexts: area.polishMemo?.kind === "contexts"
              ? area.polishMemo.settlements.length
              : 0,
            reason: polishEligibility.reason,
            terminalReason: polishEligibility.terminalReason,
            retryAfterMs: polishEligibility.retryAfterMs,
          });
        } else {
          polishContext = candidatePolishContext;
          stats.plannedAreas += 1;
          if (moveExisting) {
            // Spend at most one automatic attempt per area visit. The durable
            // settlement itself is area+geometry scoped; the entry/chart key
            // remains diagnostic rather than authorizing another heavy run.
            this.#quietPolishClaims.record(snapshot, areaKey(area), {
              retryConsumed: this.#polishEntries.consumeRetry(areaKey(area)),
              deferredRemoved: this.#deferredReflowAreas.delete(areaKey(area)),
            });
            await this.#persistAreaPolishState(area, { kind: "polish-started" }, runGeneration);
            assertNotAborted(signal);
          }
          let expectedFingerprint = startingFingerprint;
          const trace: LayoutTraceEvent[] | undefined = this.#decisionLogger.path ? [] : undefined;
          const diagnosticContext = trace ? {
            area: {
              id: area.id,
              name: area.name,
              zone: group[0].source.zone,
            },
            trigger: {
              introducesRoom,
              deferredReflow,
              moveExisting,
              introducedRooms: group
                .filter((assignment) => !assignment.room)
                .map((assignment) => assignment.source.vnum)
                .sort((a, b) => a - b),
              introducedTopology: [...introducedTopology].sort(),
            },
            identities: (() => {
              const identities = new Map<string, {
                id: string;
                vnum?: number;
                roomNumber?: RoomNumber;
                title: string;
              }>();
              for (const room of residentRooms.values()) {
                const id = residentId(room.roomNumber);
                identities.set(id, {
                  id,
                  vnum: room.vnum,
                  roomNumber: room.roomNumber,
                  title: room.title,
                });
              }
              for (const assignment of group) {
                const id = assignmentIds.get(assignment.source.vnum) as string;
                identities.set(id, {
                  id,
                  vnum: assignment.source.vnum,
                  roomNumber: assignment.room?.roomNumber,
                  title: assignment.source.name,
                });
              }
              return [...identities.values()].sort((a, b) => a.id.localeCompare(b.id));
            })(),
            snapshot,
            request: {
              centerId,
              allowExistingMoves: moveExisting,
              nodes,
              residents: planningResidents,
              edges,
            },
          } : undefined;

          // The Worker can find substantially better complete layouts long
          // before its exhaustive repair finishes. Persist those improvements
          // through a latest-wins serial queue: the active mutation completes,
          // intermediate superseded candidates are discarded, and the newest
          // candidate is then applied with the same stale/run guards as final
          // publication.
          const progressiveController = moveExisting ? new AbortController() : undefined;
          const forwardAbort = (): void => {
            if (!progressiveController || progressiveController.signal.aborted) return;
            const reason = signal && "reason" in signal
              ? (signal as AbortSignal & { readonly reason?: unknown }).reason
              : undefined;
            progressiveController.abort(reason);
          };
          if (signal?.aborted) forwardAbort();
          else signal?.addEventListener("abort", forwardAbort, { once: true });
          const planningSignal = progressiveController?.signal ?? signal;
          let queuedQuality: Readonly<LayoutQuality> | undefined;
          // Which layouts the queue is applying: the seam round's, whose
          // writes the whole-map polish still plans beneath, or the polish's
          // own. The round flushes the queue before the polish starts.
          let applyingSeamRound = false;
          let polishWrote = false;
          // The quality of the preview on the map when the whole-map polish
          // starts, when the map shows the round's writes over its base.
          let displayedQuality: Readonly<LayoutQuality> | undefined;
          const progressive = moveExisting
            ? new LatestValueQueue<IntegralLayoutPlan>(async (candidate) => {
              assertNotAborted(planningSignal);
              this.#assertCurrentRun(runGeneration);
              this.#assertLivePlanningFingerprint(
                area,
                expectedFingerprint,
                "before applying layout",
              );
              const reconciled = reconciliationUpdates(
                roomById,
                candidate.positions,
                (room) => room.roomNumber,
              );
              const updates: [RoomNumber, UpdateRoomParams][] = reconciled.map((update) => [
                update.key,
                {
                  x: update.position.x,
                  y: update.position.y,
                  level: update.position.level,
                },
              ]);
              if (updates.length === 0) return;

              const batchStartedAt = performance.now();
              assertNotAborted(planningSignal);
              await this.#commitLayoutMoves(
                area, candidate.positions, updates, runGeneration,
                `Apply progressive NukeFire reflow for ${area.name}`, candidate.routeAmendments, stats,
              );
              stats.movedRooms += updates.length;
              for (const update of reconciled) appliedPositionIds.add(update.id);
              expectedFingerprint = mirrorPlanningFingerprint(area);
              // The layout is on the map now, even if movement aborts this
              // pass before the rest of this runs, so whose write it was is
              // noted first: a later pass must see the preview's own writes
              // as the preview's, or it would plan from the preview instead
              // of the merged geometry.
              if (seamPass && applyingSeamRound) {
                this.#seamPreviews.previewApplied(areaKey(area), expectedFingerprint);
              } else if (seamPass) {
                polishWrote = true;
                this.#seamPreviews.polishApplied(areaKey(area));
              }
              // The transaction can move a newer current room which belongs to
              // this area even when this plan's snapshot center is stale.
              assertNotAborted(planningSignal);
              stats.batchCommitMs += performance.now() - batchStartedAt;
              this.#logDecision({
                kind: "layout-progress-applied",
                area: { id: area.id, name: area.name },
                quality: candidate.quality,
                movedRooms: updates.length,
                ...(applyingSeamRound ? { seamRound: true } : {}),
              });
              // The durable improvement makes this pass fruitful: an abort now
              // resumes with a fresh allowance, since the ratchet means every
              // retry starts from a strictly better map.
              this.#quietPolishClaims.markProgress(snapshot, areaKey(area));

            }, (error) => progressiveController?.abort(error), {
              minIntervalMs: PROGRESSIVE_APPLY_FLOOR_MS,
            })
            : undefined;
          const publishImprovement = (progress: Readonly<LayoutPlannerProgress>): void => {
            const candidate = progress.improvement;
            if (!candidate || !progressive) return;
            const baseline = queuedQuality ?? progress.snapshot.currentQuality;
            if (baseline && compareLayoutQuality(candidate.quality, baseline) <= 0) return;
            queuedQuality = candidate.quality;
            progressive.push(candidate);
          };
          // The seam round: planner passes that may move only the rooms of
          // the seam region, each from the best layout so far, while a pass
          // gains on the quality up to crossings. It runs no constraint
          // repair, whose search ignores pins. Its layouts go through the
          // queue like any other, and it flushes the queue before returning.
          const runSeamRound = async (round: SeamRoundPlan): Promise<void> => {
            const startedAt = performance.now();
            const key = areaKey(area);
            const movableIds = new Set(
              residents
                .filter((resident) => resident.movable && round.region.has(resident.id))
                .map((resident) => resident.id),
            );
            // The map as it is: the base, or where an interrupted round left it.
            let positions: ReadonlyMap<string, GridPosition> = new Map(
              residents.map((resident) => [resident.id, resident.position]),
            );
            const before = measureIntegralLayoutQuality(positions, edges);
            let best: Readonly<LayoutQuality> = before;
            let passes = 0;
            let stop: "passes" | "no-gain" | "nothing-movable" | "failed" = movableIds.size === 0
              ? "nothing-movable"
              : "passes";
            let failure: string | undefined;
            try {
              for (let pass = 0; movableIds.size > 0 && pass < round.passesLeft; pass += 1) {
                const passBefore = best;
                const request: IntegralLayoutRequest = {
                  nodes: [],
                  residents: residents.map((resident) => ({
                    id: resident.id,
                    position: positions.get(resident.id) ?? resident.position,
                    movable: movableIds.has(resident.id),
                  })),
                  edges,
                  allowExistingMoves: true,
                };
                const consider = (candidate: IntegralLayoutPlan | undefined): void => {
                  if (!candidate || !progressive || !keepsSeamPins(request.residents, candidate.positions)) {
                    return;
                  }
                  if (compareLayoutQuality(candidate.quality, best) <= 0) return;
                  best = candidate.quality;
                  positions = candidate.positions;
                  queuedQuality = candidate.quality;
                  // A detour the round proposes treats the rooms it pinned
                  // as fixed; route amendments are the whole-map polish's.
                  progressive.push({
                    positions: candidate.positions,
                    movedExisting: candidate.movedExisting,
                    quality: candidate.quality,
                  });
                };
                let planned: IntegralLayoutPlan;
                try {
                  planned = await this.#whileCurrentRun(
                    runGeneration,
                    () => this.#quietPolishPlanner().planIntegral(request, {
                      signal: planningSignal,
                      onProgress: (progress) => consider(progress.improvement),
                      currentQuality: best,
                    }),
                  );
                } catch (error) {
                  if (error instanceof ObsoleteNukeFireMapperRunError || planningSignal?.aborted) {
                    throw error;
                  }
                  // The round only previews: a pass the planner cannot lay
                  // out ends it, and the whole-map polish runs regardless.
                  stop = "failed";
                  failure = error instanceof Error ? error.message : String(error);
                  break;
                }
                consider(planned);
                passes += 1;
                this.#seamPreviews.passRan(key);
                if (!improvesThroughCrossings(best, passBefore, compareLayoutQuality)) {
                  stop = "no-gain";
                  break;
                }
              }
              this.#seamPreviews.roundEnded(key);
              await progressive?.flush();
            } catch (error) {
              progressive?.discardPending();
              await progressive?.flush();
              throw error;
            }
            this.#logDecision({
              kind: "layout-seam-round",
              area: { id: area.id, name: area.name },
              seams: area.polishSeams.filter((roomNumber) => roomById.has(residentId(roomNumber)))
                .length,
              region: round.region.size,
              movable: movableIds.size,
              resumed: round.resumed,
              passes,
              stop,
              ...(failure === undefined ? {} : { error: failure }),
              before,
              after: best,
              durationMs: performance.now() - startedAt,
            });
          };
          try {
            if (seamPass?.round) {
              applyingSeamRound = true;
              try {
                await runSeamRound(seamPass.round);
              } finally {
                applyingSeamRound = false;
              }
            }
            if (base) {
              // Rooms the map shows away from the base the polish plans from
              // are the seam round's writes, from this pass or an interrupted
              // one. Every layout of the polish is complete, so the final
              // reconciliation diffs them as it diffs rooms it moved itself,
              // and the polish shows a layout only once it beats the preview.
              for (const resident of planningResidents) {
                const shown = roomById.get(resident.id)?.position;
                if (shown && !sameGridPosition(shown, resident.position)) {
                  appliedPositionIds.add(resident.id);
                  displayedQuality ??= queuedQuality ?? measureIntegralLayoutQuality(
                    new Map([...roomById].map(([id, room]) => [id, room.position])),
                    edges,
                  );
                }
              }
              if (displayedQuality) queuedQuality = displayedQuality;
            }
            const runWorker = async (): Promise<IntegralLayoutPlan> => {
              assertNotAborted(planningSignal);
              const plannerStartedAt = performance.now();
              let planned: IntegralLayoutPlan;
              try {
                const planningRequest: IntegralLayoutRequest = {
                  nodes,
                  residents: planningResidents,
                  edges,
                  centerId,
                  allowExistingMoves: moveExisting,
                  trace: trace ? (event) => trace.push(event) : undefined,
                };
                planned = await this.#whileCurrentRun(
                  runGeneration,
                  () => moveExisting
                    ? this.#quietPolishPlanner().planIntegral(planningRequest, {
                      signal: planningSignal,
                      onProgress: publishImprovement,
                      constraintRepair: constraintRepairPolicy,
                      plannerContext: {
                        source: "nukefire:auto-polish",
                        areaId: areaKey(area),
                        areaName: area.name,
                        contextKey: candidatePolishContext.key,
                      },
                      // What the map shows before this plan is the preview.
                      ...(displayedQuality ? { currentQuality: displayedQuality } : {}),
                    })
                    : planIntegralLayoutAsync(planningRequest, { signal: planningSignal }),
                );
                stats.plannerMs += performance.now() - plannerStartedAt;
                await progressive?.flush();
                // The final Worker plan and its repair report stay one
                // authoritative value: the result is at least as good as every
                // streamed improvement, and replacing its positions with a
                // checkpoint would detach the candidate-specific report fields
                // from the geometry they describe.
              } catch (error) {
                progressive?.discardPending();
                try {
                  await progressive?.flush();
                } catch (progressiveError) {
                  throw progressiveError;
                }
                throw error;
              }
              assertNotAborted(planningSignal);
              this.#assertLivePlanningFingerprint(area, expectedFingerprint, "after Worker planning");
              return planned;
            };

            const workerPlan = await runWorker();
            assertNotAborted(signal);
            const restoredPositions = restoreUnanchoredChartLevels(
              workerPlan.positions,
              nodes,
              edges,
              establishedLevels,
              centerId,
            );
            const planned = restoredPositions === workerPlan.positions
              ? workerPlan
              : { ...workerPlan, positions: restoredPositions };
            plan = planned;
            if (diagnosticContext) {
              this.#logDecision({
                kind: "layout-decision",
                ...diagnosticContext,
                trace,
                result: {
                  quality: planned.quality,
                  constraintRepair: planned.constraintRepair,
                  movedExisting: [...planned.movedExisting].sort(),
                  positions: serializedPositions(planned.positions),
                },
              });
            }
            if (
              displayedQuality !== undefined && !polishWrote &&
              compareLayoutQuality(planned.quality, displayedQuality) <= 0
            ) {
              // The whole-map polish ended no better than the preview, so the
              // map keeps the preview: the pass's plan is what the map shows.
              keptPreview = true;
              const positions = new Map([...roomById].map(([id, room]) => [id, room.position]));
              const routeAmendments = computeIntegralRouteAmendments(
                { residents, edges, allowExistingMoves: true },
                { positions, quality: displayedQuality },
              );
              plan = {
                positions,
                movedExisting: new Set(
                  planningResidents
                    .filter((resident) => {
                      const shown = positions.get(resident.id);
                      return shown !== undefined && !sameGridPosition(shown, resident.position);
                    })
                    .map((resident) => resident.id),
                ),
                ...(planned.constraintRepair ? { constraintRepair: planned.constraintRepair } : {}),
                ...(routeAmendments ? { routeAmendments } : {}),
              };
              this.#logDecision({
                kind: "layout-seam-preview-kept",
                area: { id: area.id, name: area.name },
                preview: displayedQuality,
                polish: planned.quality,
              });
            }
          } catch (caught) {
            if (caught instanceof ObsoleteNukeFireMapperRunError) throw caught;
            if (moveExisting && polishContext) {
              const interruptedFingerprint = mirrorPlanningFingerprint(area);
              const displacedWithinArea = signal?.aborted === true &&
                this.#takeSameAreaPolishDisplacement(snapshot, areaKey(area));
              await this.#persistAreaPolishState(area, {
                kind: "polish-interrupted",
                reason: signal?.aborted ? "cancelled" : "error",
                displacedWithinArea,
                improved: interruptedFingerprint !== polishContext.geometryFingerprint,
                context: {
                  ...polishContext,
                  geometryFingerprint: interruptedFingerprint,
                },
              }, runGeneration);
            }
            if (signal?.aborted) throw caught;
            if (diagnosticContext) {
              this.#logDecision({
                kind: "layout-error",
                ...diagnosticContext,
                trace,
                error: {
                  message: caught instanceof Error ? caught.message : String(caught),
                  stack: caught instanceof Error ? caught.stack : undefined,
                },
              });
            }
            throw caught;
          } finally {
            signal?.removeEventListener("abort", forwardAbort);
          }
        }
      }

      assertNotAborted(signal);
      for (const assignment of group) {
        const id = assignmentIds.get(assignment.source.vnum) as string;
        const position = plan.positions.get(id);
        if (!position) throw new Error(`layout omitted room #${assignment.source.vnum}`);
        assignment.position = position;
      }

      // Progressive candidates have already changed the live room mirror. A
      // final plan's movedExisting set is relative to the original request,
      // so it omits any checkpoint move which the final plan restores. Diff
      // every resident touched by either plan against the post-checkpoint
      // mirror to make the durable coordinates and final routes agree while
      // leaving the no-op walking lane cheap.
      const finalReconciliationIds = reconcilableResidentIds(
        moveExisting,
        plan.movedExisting,
        appliedPositionIds,
      );
      const reconciled = reconciliationUpdates(
        roomById,
        plan.positions,
        (room) => room.roomNumber,
        finalReconciliationIds,
      );
      const updates: [RoomNumber, UpdateRoomParams][] = reconciled.map((update) => [
        update.key,
        {
          x: update.position.x,
          y: update.position.y,
          level: update.position.level,
        },
      ]);
      if (topologyGrowth || updates.length > 0 || reconcilePorts) {
        assertNotAborted(signal);
        this.#assertLivePlanningFingerprint(
          area,
          mirrorPlanningFingerprint(area),
          "before applying layout",
        );
        const batchStartedAt = performance.now();
        const residentIds = new Set([...residentRooms.keys()].map(residentId));
        await this.#commitLayoutMoves(
          area, plan.positions, updates, runGeneration, `Reflow NukeFire area ${area.name}`,
          plan.routeAmendments, stats, updates.length > 0 ? undefined : {
            rooms: new Set(),
            cells: [...plan.positions].filter(([id]) => !residentIds.has(id)).map(([, position]) => position),
          },
        );
        stats.movedRooms += updates.length;
        for (const update of reconciled) appliedPositionIds.add(update.id);
        this.#reconciledPortAreas.add(areaKey(area));
        assertNotAborted(signal);
        stats.batchCommitMs += performance.now() - batchStartedAt;
      }

      assertNotAborted(signal);
      if (runPlanner && moveExisting) {
        const finalFingerprint = mirrorPlanningFingerprint(area);
        const improved = polishContext !== undefined &&
          finalFingerprint !== polishContext.geometryFingerprint;
        // A fruitful non-fixed-point pass invalidates every old context. If
        // the same pass proves a fixed point, retain that proof against the
        // final geometry so re-entering through the same chart does not repeat
        // the completed tournament. A kept seam preview is not the geometry
        // the polish proved anything about, so it records no context.
        const completedContext = keptPreview
          ? undefined
          : polishContext === undefined || !improved
          ? polishContext
          : { ...polishContext, geometryFingerprint: finalFingerprint };
        await this.#persistAreaPolishState(area, {
          kind: "polish-completed",
          report: plan.constraintRepair,
          improved,
          context: completedContext,
        }, runGeneration);
        assertNotAborted(signal);
        // The attempt is genuinely spent: a later abort over the same
        // snapshot must not resurrect it. Completion is also fresh evidence,
        // so the fruitless-resume allowance starts over. The whole map is
        // polished, seams included, so their preview is done with too.
        this.#quietPolishClaims.discharge(snapshot, areaKey(area));
        this.#quietResumeBudget.reset(areaKey(area));
        this.#seamPreviews.forget(areaKey(area));
      }
      if (
        policy.deferExistingReflow &&
        this.#polishEntries.currentAreaKey === areaKey(area)
      ) {
        this.#deferredReflowAreas.add(areaKey(area));
      } else {
        this.#deferredReflowAreas.delete(areaKey(area));
      }
      for (const key of introducedTopology) this.#plannedTopology.add(key);

      for (const assignment of group) {
        const id = assignmentIds.get(assignment.source.vnum) as string;
        assignment.positionApplied = assignment.room !== undefined &&
          ((moveExisting && plan.movedExisting.has(id)) || appliedPositionIds.has(id));
      }
    }
    return stats;
  }

  #desiredConnectionGeometry(
    roomA: RoomNumber,
    roomB: RoomNumber,
    positionA: GridPosition,
    positionB: GridPosition,
    occupied: readonly GridPosition[],
    preferredStart: RouteSide,
    preferredEnd: RouteSide,
    endpointA?: ConnectionEndpoint,
    endpointB?: ConnectionEndpoint,
    knownObstructed?: boolean,
    amendmentWaypoints?: readonly MapPoint[],
  ): DesiredConnectionGeometry {
    const baseA: ConnectionEndpoint = endpointA ?? {
      room_number: roomA,
      side: preferredStart,
      port_offset: 0.5,
      port_mode: "AutoPinned",
    };
    const baseB: ConnectionEndpoint = endpointB ?? {
      room_number: roomB,
      side: preferredEnd,
      port_offset: 0.5,
      port_mode: "AutoPinned",
    };
    // Only two rooms on one level may be joined by a drawn route, so a
    // Connection between levels, or from a room to itself, runs straight.
    if (roomA === roomB || positionA.level !== positionB.level) {
      return {
        endpoint_a: baseA,
        endpoint_b: baseB,
        routing: "Simple",
        segment_shape: "Direct",
        corner: "Rounded",
        route_points: [],
      };
    }
    const routedStart = routedEndpointSide(baseA, preferredStart) as RouteSide;
    const routedEnd = routedEndpointSide(baseB, preferredEnd) as RouteSide;
    // An engine amendment is the plan's own answer for a defect movement can
    // never resolve, so it takes the place of local route recomputation; a
    // later plan without the amendment recomputes the plain route here.
    const route = amendmentWaypoints && amendmentWaypoints.length > 0
      ? amendedConnectionRoute(
        positionA,
        positionB,
        amendmentWaypoints,
        routedStart,
        routedEnd,
      )
      : planConnectionRoute(
        positionA,
        positionB,
        occupied,
        routedStart,
        routedEnd,
        knownObstructed,
      );
    return {
      endpoint_a: { ...baseA, side: routedEndpointSide(baseA, route.startSide) },
      endpoint_b: { ...baseB, side: routedEndpointSide(baseB, route.endSide) },
      // The data model reserves `Manual` routing for author-drawn centerlines.
      // Every route produced here is solver-generated, so it persists as
      // `Automatic` with `route_points` carrying any detour, leaving `Manual`
      // as the unambiguous user-ownership marker.
      routing: "Automatic",
      segment_shape: route.segmentShape,
      corner: route.corner,
      route_points: route.routePoints,
    };
  }

  /**
   * Routes the area's Connections for rooms at `positions`, in `mutation`'s
   * edit when one is given. With a `scope`, from a pass that moved no room,
   * only the Connections it can have changed are routed again; every other
   * one keeps its route, while ports are still settled across the area. True
   * when a Connection the edit brings onto one level still runs straight,
   * because the host accepts its route only in a later edit: the caller then
   * syncs again once this edit has committed.
   */
  async #syncAreaConnectionRoutes(
    area: AreaMirror,
    residentRooms: ReadonlyMap<RoomNumber, RoomMirror>,
    positions: ReadonlyMap<string, GridPosition>,
    runGeneration: number,
    mutation?: AreaMutator,
    routeAmendments?: readonly RouteAmendment[],
    scope?: RouteSyncScope,
  ): Promise<boolean> {
    this.#assertCurrentRun(runGeneration);
    const byRoomNumber = new Map<RoomNumber, GridPosition>();
    for (const number of residentRooms.keys()) {
      const position = positions.get(residentId(number));
      if (position) byRoomNumber.set(number, position);
    }
    const roomNumbersByLayoutId = new Map<string, RoomNumber>();
    for (const room of residentRooms.values()) {
      roomNumbersByLayoutId.set(residentId(room.roomNumber), room.roomNumber);
      if (room.vnum !== undefined) {
        roomNumbersByLayoutId.set(newRoomId(room.vnum), room.roomNumber);
      }
    }
    const amendmentIndex = indexRouteAmendments(routeAmendments, roomNumbersByLayoutId);
    // Include planned rooms which have not been created yet. They can obstruct
    // an older connection during the same topology-growth transaction.
    const occupied = [...positions.values()];
    const changes: unknown[] = [];
    const membersByConnection = new Map<string, {
      room: RoomMirror;
      exit: ExitMirror;
    }[]>();
    for (const room of area.roomsByNumber.values()) {
      for (const exit of room.exits) {
        if (!exit.connectionId) continue;
        const key = connectionMirrorKey(exit.connectionId);
        const members = membersByConnection.get(key) ?? [];
        members.push({ room, exit });
        membersByConnection.set(key, members);
      }
    }

    const geometryOf = (
      connection: ConnectionMirror,
      endpointB: ConnectionEndpoint,
    ): DesiredConnectionGeometry => ({
      endpoint_a: copyEndpoint(connection.endpointA),
      endpoint_b: copyEndpoint(endpointB),
      routing: connection.routing,
      segment_shape: connection.segmentShape,
      corner: connection.corner,
      route_points: connection.routePoints.map((point) => ({ ...point })),
    });
    const writeGeometry = async (
      connection: ConnectionMirror,
      desired: DesiredConnectionGeometry,
    ): Promise<void> => {
      await this.#whileCurrentRun(
        runGeneration,
        () => mutation
          ? mutation.setConnection(connection.id, desired)
          : this.#directMutation(
            area.id,
            "setConnection",
            `Update NukeFire connection ${String(connection.id)}`,
            () => mapper.setConnection(area.id, connection.id, desired),
          ),
      );
      connection.endpointA = copyEndpoint(desired.endpoint_a);
      connection.endpointB = copyEndpoint(desired.endpoint_b);
      connection.routing = desired.routing;
      connection.segmentShape = desired.segment_shape;
      connection.corner = desired.corner;
      connection.routePoints = desired.route_points.map((point) => ({ ...point }));
    };

    // The host checks every Connection's routing against the kind it derives
    // from where the edit leaves the Connection's rooms, and only rooms on one
    // level may be joined by a drawn route. A Connection whose rooms end on
    // different levels therefore runs straight from here on: the mapper's own
    // route is dropped, and an author-drawn one stays stored, dormant, for its
    // author to restore. A Connection whose rooms end on one level after
    // running between levels keeps running straight until the edit commits,
    // because inside the edit the host checks an update against the kind the
    // Connection had before it; the caller routes it in an edit of its own.
    const levelOf = (number: RoomNumber): number | undefined =>
      (byRoomNumber.get(number) ?? area.roomsByNumber.get(number)?.position)?.level;
    const routeAfterCommit = new Set<string>();
    for (const connection of area.connections.values()) {
      const endpointB = connection.endpointB;
      if (!endpointB || endpointB.room_number === connection.endpointA.room_number) continue;
      const levelA = levelOf(connection.endpointA.room_number);
      const levelB = levelOf(endpointB.room_number);
      if (levelA === undefined || levelB === undefined) continue;
      if (levelA === levelB) {
        if (connection.kind === "CrossLevel") {
          if (mutation) routeAfterCommit.add(connectionMirrorKey(connection.id));
          connection.kind = "Internal";
        }
        continue;
      }
      connection.kind = "CrossLevel";
      if (connection.routing !== "Automatic" && connection.routing !== "Manual") continue;
      const authored = routeIsManuallyAuthored(connection.routing);
      const before = geometryOf(connection, endpointB);
      const desired: DesiredConnectionGeometry = {
        ...before,
        routing: "Simple",
        segment_shape: authored ? before.segment_shape : "Direct",
        route_points: authored ? before.route_points : [],
      };
      await writeGeometry(connection, desired);
      changes.push({
        connectionId: connection.id,
        levels: [levelA, levelB],
        reason: authored ? "authored-route-dormant-between-levels" : "route-dropped-between-levels",
        before,
        after: desired,
      });
    }

    const proposals: {
      key: string;
      connection: ConnectionMirror;
      roomA: RoomMirror;
      roomB: RoomMirror;
      positionA: GridPosition;
      positionB: GridPosition;
      exitA?: ExitMirror;
      exitB?: ExitMirror;
      preferredStart: RouteSide;
      preferredEnd: RouteSide;
      obstructions: GridPosition[];
      desired: DesiredConnectionGeometry;
      amended: boolean;
      oneWayOriginRoom?: RoomNumber;
    }[] = [];

    for (const connection of area.connections.values()) {
      const endpointB = connection.endpointB;
      if (!endpointB) continue;
      // An author-drawn route is user-owned, exactly as Manual ports are:
      // recomputation proposes nothing for it, nor for one left dormant, so
      // its geometry survives every commit while its endpoints still reserve
      // their wall slots below.
      if (
        routeIsManuallyAuthored(connection.routing) ||
        routeIsDormant(connection.routing, connection.routePoints)
      ) continue;
      if (routeAfterCommit.has(connectionMirrorKey(connection.id))) continue;
      const roomA = residentRooms.get(connection.endpointA.room_number);
      const roomB = residentRooms.get(endpointB.room_number);
      if (!roomA || !roomB || roomA.vnum === undefined || roomB.vnum === undefined) continue;
      const positionA = byRoomNumber.get(roomA.roomNumber);
      const positionB = byRoomNumber.get(roomB.roomNumber);
      if (!positionA || !positionB || roomA.roomNumber === roomB.roomNumber || positionA.level !== positionB.level) {
        continue;
      }
      const deltaX = positionB.x - positionA.x;
      const deltaY = positionB.y - positionA.y;
      const key = connectionMirrorKey(connection.id);
      const members = membersByConnection.get(key) ?? [];
      const exactExitA = members.find((member) =>
        member.room.roomNumber === roomA.roomNumber
      )?.exit;
      const exactExitB = members.find((member) =>
        member.room.roomNumber === roomB.roomNumber
      )?.exit;
      // A freshly created fallback traversal may not have its Connection id in
      // the VM mirror until rehydration. Preserve the old topology fallback in
      // that narrow case, but never borrow another connection's reciprocal
      // member when exact membership is available.
      const exitA = exactExitA ?? (members.length === 0
        ? roomA.exits.find((exit) =>
          exit.toAreaId && sameAreaId(exit.toAreaId, area.id) &&
          exit.toRoomNumber === roomB.roomNumber
        )
        : undefined);
      const exitB = exactExitB ?? (members.length === 0
        ? roomB.exits.find((exit) =>
          exit.toAreaId && sameAreaId(exit.toAreaId, area.id) &&
          exit.toRoomNumber === roomA.roomNumber
        )
        : undefined);
      const directionA = exitA?.fromDirection ?? exitB?.toDirection ?? "Other";
      const directionB = exitB?.fromDirection ?? exitA?.toDirection ?? "Other";
      const preferredStart = directionSide(directionA, deltaX, deltaY) as RouteSide;
      const preferredEnd = directionSide(directionB, -deltaX, -deltaY) as RouteSide;
      // A matching engine amendment supplies the generated route directly,
      // oriented from this connection's endpoint A toward endpoint B. Manual
      // connections never reach this point, so an amendment can never touch
      // an author-drawn route.
      const amendmentWaypoints = amendmentWaypointsBetween(
        amendmentIndex,
        connection.endpointA.room_number,
        endpointB.room_number,
      );
      const unchanged = scope !== undefined &&
        !scope.rooms.has(roomA.roomNumber) && !scope.rooms.has(roomB.roomNumber) &&
        !(amendmentWaypoints && amendmentWaypoints.length > 0) &&
        !routeCrossesCells(positionA, positionB, connection.routePoints, scope.cells);
      const obstructions = unchanged ? [] : directRoomObstructions(positionA, positionB, occupied);
      const desired = unchanged
        ? geometryOf(connection, endpointB)
        : this.#desiredConnectionGeometry(
          roomA.roomNumber,
          roomB.roomNumber,
          positionA,
          positionB,
          occupied,
          preferredStart,
          preferredEnd,
          connection.endpointA,
          endpointB,
          obstructions.length > 0,
          amendmentWaypoints,
        );
      const soleMember = members.length === 1 ? members[0] : undefined;
      const oneWayOriginRoom = soleMember &&
          ((soleMember.room.roomNumber === roomA.roomNumber &&
              soleMember.exit.toAreaId !== null &&
              sameAreaId(soleMember.exit.toAreaId, area.id) &&
              soleMember.exit.toRoomNumber === roomB.roomNumber) ||
            (soleMember.room.roomNumber === roomB.roomNumber &&
              soleMember.exit.toAreaId !== null &&
              sameAreaId(soleMember.exit.toAreaId, area.id) &&
              soleMember.exit.toRoomNumber === roomA.roomNumber))
        ? soleMember.room.roomNumber
        : undefined;
      proposals.push({
        key,
        connection,
        roomA,
        roomB,
        positionA,
        positionB,
        exitA,
        exitB,
        preferredStart,
        preferredEnd,
        obstructions,
        desired,
        amended: amendmentWaypoints !== undefined && amendmentWaypoints.length > 0,
        oneWayOriginRoom,
      });
    }

    const proposedByKey = new Map(proposals.map((proposal) => [proposal.key, proposal]));
    const portConnections: OneWayPortConnection[] = [];
    for (const connection of area.connections.values()) {
      const endpointB = connection.endpointB;
      if (!endpointB) continue;
      const key = connectionMirrorKey(connection.id);
      const proposal = proposedByKey.get(key);
      const endpointA = proposal?.desired.endpoint_a ?? connection.endpointA;
      const desiredEndpointB = proposal?.desired.endpoint_b ?? endpointB;
      const roomA = area.roomsByNumber.get(endpointA.room_number);
      const roomB = area.roomsByNumber.get(desiredEndpointB.room_number);
      if (!roomA || !roomB) continue;
      const positionA = byRoomNumber.get(roomA.roomNumber) ?? roomA.position;
      const positionB = byRoomNumber.get(roomB.roomNumber) ?? roomB.position;
      portConnections.push({
        key,
        endpointA,
        endpointB: desiredEndpointB,
        positionA,
        positionB,
        oneWayOriginRoom: proposal?.oneWayOriginRoom,
      });
    }
    const portLayouts = disambiguateOneWayArrivalPorts(portConnections);

    for (const proposal of proposals) {
      const {
        key,
        connection,
        roomA,
        roomB,
        positionA,
        positionB,
        exitA,
        exitB,
        preferredStart,
        preferredEnd,
        obstructions,
        desired,
        amended,
      } = proposal;
      const currentEndpointB = connection.endpointB;
      if (!currentEndpointB) continue;
      const ports = portLayouts.get(key);
      if (ports) {
        desired.endpoint_a = copyEndpoint(ports.endpointA);
        desired.endpoint_b = copyEndpoint(ports.endpointB);
      }
      const desiredSignature = geometrySignature(desired);
      if (connectionSignature(connection) === desiredSignature) {
        continue;
      }
      const portsChanged = connection.endpointA.port_offset !== desired.endpoint_a.port_offset ||
        currentEndpointB.port_offset !== desired.endpoint_b.port_offset;
      const before = geometryOf(connection, currentEndpointB);
      await writeGeometry(connection, desired);
      changes.push({
        connectionId: connection.id,
        roomA: {
          roomNumber: roomA.roomNumber,
          vnum: roomA.vnum,
          position: positionA,
          direction: exitA?.fromDirection,
        },
        roomB: {
          roomNumber: roomB.roomNumber,
          vnum: roomB.vnum,
          position: positionB,
          direction: exitB?.fromDirection,
        },
        preferredStart,
        preferredEnd,
        obstructions,
        reason: portsChanged
          ? "one-way-arrival-disambiguation"
          : amended
          ? "engine-route-amendment"
          : desired.route_points.length > 0
          ? "direct-segment-crosses-room"
          : obstructions.length > 0
          ? "no-orthogonal-route-found"
          : "direct-segment-clear",
        before,
        after: desired,
      });
    }
    if (changes.length > 0) {
      this.#logDecision({
        kind: "routing-decisions",
        area: { id: area.id, name: area.name },
        changes,
      });
    }
    return routeAfterCommit.size > 0;
  }

  /**
   * Routes, in an edit of their own, the Connections an edit just brought onto
   * one level: once that edit has committed, the host checks their routes
   * against their kind between rooms on one level.
   */
  async #routeConnectionsAfterCommit(
    area: AreaMirror,
    residentRooms: ReadonlyMap<RoomNumber, RoomMirror>,
    positions: ReadonlyMap<string, GridPosition>,
    runGeneration: number,
    routeAmendments?: readonly RouteAmendment[],
  ): Promise<void> {
    await this.#whileCurrentRun(
      runGeneration,
      () => this.#mutateArea(area.id, async (mutation) => {
        await this.#syncAreaConnectionRoutes(
          area,
          residentRooms,
          positions,
          runGeneration,
          mutation,
          routeAmendments,
        );
      }, `Route NukeFire connections brought onto one level in ${area.name}`),
    );
  }

  async #syncRoom(
    assignment: Assignment,
    mutation: AreaMutator,
    runGeneration: number,
  ): Promise<RoomMirror> {
    this.#assertCurrentRun(runGeneration);
    const source = assignment.source;
    const position = assignment.position;
    if (!position) throw new Error(`layout omitted room #${source.vnum}`);
    const { x, y, level } = position;
    const color = terrainColor(source.terrain);
    let room = assignment.room;
    const created = !room;

    if (!room) {
      const number = await this.#whileCurrentRun(
        runGeneration,
        () => mutation.createRoom({
          title: source.name,
          level,
          x,
          y,
          color,
          externalId: externalRoomId(source.vnum),
        }),
      );
      room = {
        areaId: assignment.area.id,
        roomNumber: number,
        vnum: source.vnum,
        externalId: externalRoomId(source.vnum),
        title: source.name,
        color,
        position: { ...position },
        layoutLocked: false,
        exits: [],
      };
      this.#registerRoom(assignment.area, room);
      assignment.room = room;
    }

    const updates: UpdateRoomParams = {};
    if (source.name && room.title !== source.name) updates.title = source.name;
    if (room.color !== color) updates.color = color;
    if (
      coordinateWriteAllowed(
        created,
        assignment.positionApplied === true,
        assignment.moveExisting === true,
      )
    ) {
      if (room.position.level !== level) updates.level = level;
      if (room.position.x !== x) updates.x = x;
      if (room.position.y !== y) updates.y = y;
    }
    if (Object.keys(updates).length > 0) {
      await this.#whileCurrentRun(
        runGeneration,
        () => mutation.updateRoom(room.roomNumber, updates),
      );
      if (updates.title !== undefined) room.title = updates.title;
      if (updates.color !== undefined) room.color = updates.color;
      room.position = roundedPosition(
        updates.x ?? room.position.x,
        updates.y ?? room.position.y,
        updates.level ?? room.position.level,
      );
    }

    if (room.zone !== String(source.zone)) {
      await this.#whileCurrentRun(
        runGeneration,
        () => mutation.setRoomProperty(room.roomNumber, NUKEFIRE_MARKS.zone, String(source.zone)),
      );
      room.zone = String(source.zone);
    }
    if (room.terrain !== source.terrain) {
      await this.#whileCurrentRun(
        runGeneration,
        () => mutation.setRoomProperty(room.roomNumber, ROOM_TERRAIN_PROPERTY, source.terrain),
      );
      room.terrain = source.terrain;
    }
    return room;
  }

  async #syncLinks(
    links: readonly NukeFireMapLink[],
    rooms: Map<number, RoomMirror>,
    runGeneration: number,
  ): Promise<boolean> {
    this.#assertCurrentRun(runGeneration);
    let changed = false;
    const processed = new Set<string>();
    const batchable = new Map<string, {
      link: NukeFireMapLink;
      from: RoomMirror;
      to: RoomMirror | undefined;
      mapped: MappedDirection;
    }[]>();
    const crossArea: {
      link: NukeFireMapLink;
      from: RoomMirror;
      to: RoomMirror | undefined;
      mapped: MappedDirection;
    }[] = [];
    for (const link of links) {
      if (!isUsableVnum(link.from) || !isUsableVnum(link.to)) continue;
      const mapped = mapDirection(link.direction);
      const key = `${link.from}>${link.to}:${mapped.command}`;
      if (processed.has(key)) continue;
      processed.add(key);
      if (link.bidirectional && mapped.reverseCommand) {
        processed.add(`${link.to}>${link.from}:${mapped.reverseCommand}`);
      }

      const from = rooms.get(link.from) ?? this.#roomsByVnum.get(link.from);
      if (!from) continue;
      const to = rooms.get(link.to) ?? this.#roomsByVnum.get(link.to);
      const work = { link, from, to, mapped };
      if (link.bidirectional && to && !sameAreaId(from.areaId, to.areaId)) {
        crossArea.push(work);
      } else {
        const key = areaIdKey(from.areaId);
        const group = batchable.get(key) ?? [];
        group.push(work);
        batchable.set(key, group);
      }
    }
    for (const group of batchable.values()) {
      const areaId = group[0].from.areaId;
      try {
        let routesNeedSync = false;
        await this.#whileCurrentRun(
          runGeneration,
          () => this.#mutateArea(areaId, async (mutation) => {
            for (const work of group) {
              routesNeedSync = await this.#syncLink(
                work.link,
                work.from,
                work.to,
                work.mapped,
                runGeneration,
                mutation,
              ) || routesNeedSync;
            }
          }, "Apply NukeFire map links"),
        );
        changed ||= routesNeedSync;
        const area = this.#areasById.get(areaIdKey(areaId));
        if (routesNeedSync && area) {
          // A Connection created earlier in one host mutation is not visible to
          // a later update operation in that same envelope. Route and port
          // geometry therefore gets its own committed-topology pass.
          const linkedRooms = new Set<RoomNumber>();
          for (const work of group) {
            linkedRooms.add(work.from.roomNumber);
            if (work.to) linkedRooms.add(work.to.roomNumber);
          }
          await this.#whileCurrentRun(
            runGeneration,
            () => this.#mutateArea(areaId, async (mutation) => {
              await this.#syncAreaConnectionRoutes(
                area,
                area.roomsByNumber,
                new Map([...area.roomsByNumber.values()].map((room) => [
                  residentId(room.roomNumber),
                  room.position,
                ])),
                runGeneration,
                mutation,
                undefined,
                { rooms: linkedRooms, cells: [] },
              );
            }, "Route NukeFire map links"),
          );
        }
      } catch (caught) {
        if (caught instanceof ObsoleteNukeFireMapperRunError) throw caught;
        // A failed response can follow a durable topology write; refresh after recovery.
        changed = true;
        // A drafted createLink cannot discover a host topology rejection until
        // submission. The failed batch has already rehydrated this area, so retry
        // each link against fresh mirrors and preserve the established traversal
        // fallback for unusual or duplicate topologies.
        let routesNeedSync = false;
        for (const work of group) {
          const from = this.#roomsByVnum.get(work.link.from);
          if (!from) continue;
          const to = this.#roomsByVnum.get(work.link.to);
          routesNeedSync = await this.#syncLink(
            work.link,
            from,
            to,
            work.mapped,
            runGeneration,
          ) || routesNeedSync;
        }
        let refreshedArea = this.#areasById.get(areaIdKey(areaId));
        try {
          // Direct createLink can commit and then fail while its return value is
          // crossing the script boundary. Re-read after all fallbacks so those
          // durable Connections, not the pre-submit mirror, drive routing.
          refreshedArea = this.#hydrateArea(mapper.getAreaById(areaId), true);
        } catch {
          // Keep the best mirror recovered by #mutateArea when the host read is
          // itself unavailable; the serialized topology lane will retry later.
        }
        if (refreshedArea) {
          // A failed response may still represent a committed topology
          // envelope; #mutateArea rehydrates that state before throwing. Run
          // the committed-topology route pass even when every retry is now a
          // no-op, because those existing Connections may still need ports.
          await this.#whileCurrentRun(
            runGeneration,
            () => this.#mutateArea(areaId, async (mutation) => {
              await this.#syncAreaConnectionRoutes(
                refreshedArea,
                refreshedArea.roomsByNumber,
                new Map([...refreshedArea.roomsByNumber.values()].map((room) => [
                  residentId(room.roomNumber),
                  room.position,
                ])),
                runGeneration,
                mutation,
              );
            }, routesNeedSync
              ? "Route retried NukeFire map links"
              : "Route committed NukeFire map links"),
          );
        }
      }
    }
    for (const work of crossArea) {
      changed = await this.#syncLink(work.link, work.from, work.to, work.mapped, runGeneration) || changed;
    }
    return changed;
  }

  /**
   * Room.Info reports a closed exit without its destination vnum. Preserve an
   * already known destination, or create the missing vertical stub so Up/Down
   * still exists in Smudgy's topology.
   */
  async #syncClosedVerticalExits(
    observations: readonly VerticalExitObservation[],
    room: RoomMirror | undefined,
    runGeneration: number,
  ): Promise<boolean> {
    this.#assertCurrentRun(runGeneration);
    if (!room) return false;
    const pending: {
      observation: VerticalExitObservation;
      existing?: ExitMirror;
    }[] = [];
    for (const observation of observations) {
      if (!observation.closed || observation.destination !== undefined) continue;
      const existing = matchingExit(room, observation.mapped);
      if (existing && doorIsClosed(existing.door)) continue;
      if (existing && !existing.id) {
        const source = mapper.getAreaById(room.areaId).room(room.roomNumber);
        const hostExit = source?.exits.find((exit) =>
          exit.from_direction === observation.mapped.direction
        );
        if (!hostExit) continue;
        existing.id = hostExit.id;
        existing.connectionId = hostExit.connection_id;
      }
      pending.push({ observation, existing });
    }
    if (pending.length === 0) return false;

    await this.#whileCurrentRun(
      runGeneration,
      () => this.#mutateArea(room.areaId, async (mutation) => {
        for (const { observation, existing } of pending) {
          if (!existing) {
            const fields: ExitArgs = {
              from_direction: observation.mapped.direction,
              door: reportedDoor(true, false, null),
              weight: 1,
              command: observation.mapped.command,
            };
            const id = await this.#whileCurrentRun(
              runGeneration,
              () => mutation.createRoomExit(room.roomNumber, fields),
            );
            room.exits.push(exitFromFields(fields, id));
            continue;
          }
          const door = reportedDoor(true, false, existing.door);
          await this.#whileCurrentRun(
            runGeneration,
            () => mutation.setRoomExit(
              room.roomNumber,
              existing.id as ExitId,
              { door },
            ),
          );
          existing.door = copyDoor(door);
        }
      }, `Apply NukeFire vertical exits for room ${room.roomNumber}`),
    );
    return true;
  }

  async #syncLink(
    link: Readonly<NukeFireMapLink>,
    from: RoomMirror,
    to: RoomMirror | undefined,
    mapped: MappedDirection,
    runGeneration: number,
    mutation?: AreaMutator,
  ): Promise<boolean> {
    this.#assertCurrentRun(runGeneration);
    const fromExit = matchingExit(from, mapped);
    const reverseMapped = mapped.opposite && mapped.reverseCommand
      ? {
          direction: mapped.opposite,
          command: mapped.reverseCommand,
          opposite: mapped.direction,
          reverseCommand: mapped.command,
        } satisfies MappedDirection
      : undefined;
    const reverseExit = link.bidirectional && to && reverseMapped
      ? matchingExit(to, reverseMapped)
      : undefined;

    if (!fromExit && !reverseExit && to && sameAreaId(from.areaId, to.areaId)) {
      try {
        await this.#createLocalLink(
          link,
          from,
          to,
          mapped,
          reverseMapped,
          runGeneration,
          mutation,
        );
        return true;
      } catch (caught) {
        if (caught instanceof ObsoleteNukeFireMapperRunError) throw caught;
        // An unusual/duplicate topology can reject atomic pairing. The
        // traversal fallback below still records the server-authoritative exits.
      }
    }

    let changed = await this.#ensureTraversal(
      from,
      to,
      mapped,
      fromExit,
      link.closed,
      link.locked,
      runGeneration,
      mutation,
    );
    if (link.bidirectional && to && reverseMapped) {
      changed = await this.#ensureTraversal(
        to,
        from,
        reverseMapped,
        reverseExit,
        link.closed,
        link.locked,
        runGeneration,
        mutation,
      ) || changed;
    }
    return changed;
  }

  async #createLocalLink(
    link: Readonly<NukeFireMapLink>,
    from: RoomMirror,
    to: RoomMirror,
    mapped: MappedDirection,
    reverse: MappedDirection | undefined,
    runGeneration: number,
    mutation?: AreaMutator,
  ): Promise<void> {
    this.#assertCurrentRun(runGeneration);
    const positionA = from.position;
    const positionB = to.position;
    const deltaX = positionB.x - positionA.x;
    const deltaY = positionB.y - positionA.y;
    const preferredStart = directionSide(mapped.direction, deltaX, deltaY) as RouteSide;
    const preferredEnd = directionSide(mapped.opposite ?? "Other", -deltaX, -deltaY) as RouteSide;
    const area = this.#areasById.get(areaIdKey(from.areaId));
    if (!area) throw new Error(`missing mirrored area for room #${from.vnum ?? from.roomNumber}`);
    const occupied = [...area.roomsByNumber.values()].map((room) => room.position);
    const geometry = this.#desiredConnectionGeometry(
      from.roomNumber,
      to.roomNumber,
      positionA,
      positionB,
      occupied,
      preferredStart,
      preferredEnd,
    );
    const traversals: LinkTraversalArgs[] = [
      {
        room_number: from.roomNumber,
        from_direction: mapped.direction,
        ...(mapped.opposite ? { to_direction: mapped.opposite } : {}),
        to_area_id: to.areaId,
        to_room_number: to.roomNumber,
        door: reportedDoor(link.closed, link.locked, null),
        weight: 1,
        command: mapped.command,
      },
    ];
    if (link.bidirectional && reverse) {
      traversals.push({
        room_number: to.roomNumber,
        from_direction: reverse.direction,
        to_direction: mapped.direction,
        to_area_id: from.areaId,
        to_room_number: from.roomNumber,
        door: reportedDoor(link.closed, link.locked, null),
        weight: 1,
        command: reverse.command,
      });
    }

    const connectionId = await this.#whileCurrentRun(
      runGeneration,
      () => mutation
        ? mutation.createLink({
          ...geometry,
          traversals,
        })
        : this.#directMutation(
          from.areaId,
          "createLink",
          `Create NukeFire link from room ${from.roomNumber}`,
          () => mapper.createLink(from.areaId, {
            ...geometry,
            traversals,
          }),
        ),
    );
    const canonicalGeometry = canonicalConnectionGeometry(geometry);
    area.connections.set(connectionMirrorKey(connectionId), {
      id: connectionId,
      endpointA: canonicalGeometry.endpoint_a,
      endpointB: canonicalGeometry.endpoint_b,
      kind: from.roomNumber === to.roomNumber
        ? "SelfLoop"
        : positionA.level === positionB.level
        ? "Internal"
        : "CrossLevel",
      routing: canonicalGeometry.routing,
      segmentShape: canonicalGeometry.segment_shape,
      corner: canonicalGeometry.corner,
      routePoints: canonicalGeometry.route_points,
    });
    from.exits.push(exitFromFields(traversals[0], undefined, connectionId));
    if (traversals[1]) {
      to.exits.push(exitFromFields(traversals[1], undefined, connectionId));
    }
  }

  async #ensureTraversal(
    from: RoomMirror,
    to: RoomMirror | undefined,
    mapped: MappedDirection,
    existing: ExitMirror | undefined,
    closed: boolean,
    locked: boolean,
    runGeneration: number,
    mutation?: AreaMutator,
  ): Promise<boolean> {
    this.#assertCurrentRun(runGeneration);
    const destination = to
      ? {
          ...(mapped.opposite ? { to_direction: mapped.opposite } : {}),
          to_area_id: to.areaId,
          to_room_number: to.roomNumber,
        }
      : {};
    const fields: ExitArgs = {
      from_direction: mapped.direction,
      ...destination,
      door: reportedDoor(closed, locked, existing?.door ?? null),
      weight: 1,
      command: mapped.command,
    };

    if (existing && exitMatchesFields(existing, fields)) return false;
    if (existing) {
      if (!existing.id) {
        // createLink returns its Connection id but not its traversal ids. Read
        // this one room only if a later door/topology change actually needs an
        // id; the common unchanged path above remains entirely VM-local.
        const source = mapper.getAreaById(from.areaId).room(from.roomNumber);
        const hostExit = source && (mapped.direction === "Special"
          ? source.exits.find((exit) =>
            exit.from_direction === "Special" && commandKey(exit.command) === mapped.command
          )
          : source.exits.find((exit) => exit.from_direction === mapped.direction));
        if (!hostExit) return false;
        existing.id = hostExit.id;
        existing.connectionId = hostExit.connection_id;
      }
      await this.#whileCurrentRun(
        runGeneration,
        () => mutation
          ? mutation.setRoomExit(from.roomNumber, existing.id as ExitId, fields)
          : this.#directMutation(
            from.areaId,
            "setRoomExit",
            `Update NukeFire ${mapped.command} exit from room ${from.roomNumber}`,
            () => mapper.setRoomExit(
              from.areaId,
              from.roomNumber,
              existing.id as ExitId,
              fields,
            ),
          ),
      );
      applyExitFields(existing, fields);
      return true;
    } else {
      const id = await this.#whileCurrentRun(
        runGeneration,
        () => mutation
          ? mutation.createRoomExit(from.roomNumber, fields)
          : this.#directMutation(
            from.areaId,
            "createRoomExit",
            `Create NukeFire ${mapped.command} exit from room ${from.roomNumber}`,
            () => mapper.createRoomExit(from.areaId, from.roomNumber, fields),
          ),
      );
      from.exits.push(exitFromFields(fields, id));
      return true;
    }
  }
}
