import { echo, mapper, type EventSubscription } from "smudgy:core";
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
  planIntegralLayoutAsync,
  type GridPosition,
  type IntegralLayoutPlan,
  type LayoutDirection,
  type LayoutEdge,
  type LayoutNode,
  type LayoutPlannerProgress,
  type LayoutQuality,
  type LayoutResident,
  type LayoutTraceEvent,
  type RouteAmendment,
} from "./layout.ts";
import {
  DEFAULT_DECISION_LOG_FILE,
  MappingDecisionLogger,
  type DecisionLogRecord,
} from "./decision-log.ts";
import {
  areaForObservedRoom,
  findAreaByNukeFireId,
  findCompatibleAreaByName,
  isAdoptableStorage,
  NUKEFIRE_AREA_ID_PROPERTY,
} from "./area-resolution.ts";
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
  type RouteSide,
} from "./routing.ts";
import {
  verticalExitObservations,
  verticalMapLinks,
  type VerticalExitObservation,
} from "./room-info.ts";
import { stackVerticalTraversals } from "./vertical-levels.ts";
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
import { nukeFireConstraintRepairPolicy } from "./constraint-policy.ts";
import {
  CurrentLocationFreshness,
  type CurrentLocationObservation,
} from "./current-location-freshness.ts";
import {
  disambiguateOneWayArrivalPorts,
  routedEndpointSide,
  routeIsManuallyAuthored,
  type OneWayPortConnection,
} from "./connection-ports.ts";
import {
  AREA_POLISH_EXHAUSTED_FINGERPRINT_PROPERTY,
  AREA_POLISH_PENDING_PROPERTY,
  AreaPolishEntryTracker,
  areaPolishMemo,
  areaPolishPending,
  createAreaPolishPlanningContext,
  MAX_FRUITLESS_QUIET_RESUMES,
  polishRetrySuppressed,
  QuietPolishClaims,
  QuietResumeBudget,
  reduceAreaPolishMemo,
  reduceAreaPolishState,
  type AreaPolishMemo,
  type AreaPolishEvent,
  type AreaPolishPlanningContext,
} from "./polish-state.ts";
import {
  coordinateWriteAllowed,
  reconcilableResidentIds,
} from "./coordinate-write-policy.ts";

const AREA_SOURCE_PROPERTY = "nukefire.mapper";
const ROOM_ZONE_PROPERTY = "nukefire.zone";
const ROOM_TERRAIN_PROPERTY = "terrain";
const ROOM_LAYOUT_LOCK_PROPERTY = "nukefire.layout.locked";
const SOURCE_NAME = "NukeFire.Map.Local";

/**
 * Floor between progressive durable applies of one quiet search. Improvements
 * can arrive far faster than durable writes and viewport recentering are
 * worth watching; the first improvement of a search still applies immediately
 * and the final plan is never paced, so the anytime contract stays visible
 * without write amplification.
 */
export const PROGRESSIVE_APPLY_FLOOR_MS = 1_500;

export interface NukeFireMapperOptions {
  /** Prefix used when Room.Info has not supplied the zone's display name. */
  areaPrefix?: string;
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
  /** Search far beyond the ordinary reflow until perfect, exhausted, or superseded. Default true. */
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
  closed: boolean;
  locked: boolean;
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
  routing: ConnectionRouting;
  segmentShape: ConnectionSegmentShape;
  corner: ConnectionCorner;
  routePoints: MapPoint[];
}

interface AreaMirror {
  id: AreaId;
  name: string;
  storage: MapStorage;
  zone?: string;
  source?: string;
  polishPending: boolean;
  /** Bounded exact contexts which already completed fruitlessly on this geometry. */
  polishMemo: AreaPolishMemo | undefined;
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

class StaleNukeFireLayoutPlanError extends Error {
  constructor(
    area: AreaMirror,
    phase: "before Worker planning" | "after Worker planning" | "before applying layout",
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
    closed: exit.is_closed,
    locked: exit.is_locked,
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
    closed: fields.is_closed ?? false,
    locked: fields.is_locked ?? false,
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
    exit.closed === (fields.is_closed ?? false) &&
    exit.locked === (fields.is_locked ?? false) &&
    exit.weight === (fields.weight ?? 1) &&
    commandKey(exit.command) === commandKey(fields.command ?? null);
}

function applyExitFields(exit: ExitMirror, fields: ExitArgs): void {
  exit.fromDirection = fields.from_direction;
  exit.toDirection = fields.to_direction ?? null;
  exit.toAreaId = fields.to_area_id ?? null;
  exit.toRoomNumber = fields.to_room_number ?? null;
  exit.hidden = fields.is_hidden ?? false;
  exit.closed = fields.is_closed ?? false;
  exit.locked = fields.is_locked ?? false;
  exit.weight = fields.weight ?? 1;
  exit.command = fields.command ?? null;
}

function copyConnection(connection: Connection): ConnectionMirror {
  return {
    id: connection.id,
    endpointA: copyEndpoint(connection.endpoint_a),
    endpointB: connection.endpoint_b ? copyEndpoint(connection.endpoint_b) : null,
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

/**
 * Reconciles NukeFire's authoritative local map snapshots into Smudgy areas.
 * Calls are serialized because mapper mutations acknowledge asynchronously.
 */
export class NukeFireMapper {
  readonly #options: Required<Omit<NukeFireMapperOptions, "ephemeral" | "storage">> & {
    storage: MapStorage;
  };
  readonly #decisionLogger: MappingDecisionLogger;
  readonly #subscriptions: EventSubscription[] = [];
  readonly #zoneAreas = new Map<number, AreaMirror>();
  readonly #areasById = new Map<string, AreaMirror>();
  readonly #roomsByVnum = new Map<number, RoomMirror>();
  readonly #latencyLanes: SnapshotLatencyLanes<NukeFireMapLocal>;
  readonly #currentLocationFreshness = new CurrentLocationFreshness();
  readonly #polishEntries = new AreaPolishEntryTracker();
  readonly #quietPolishClaims = new QuietPolishClaims<NukeFireMapLocal>(
    (aborted, incoming) => this.#snapshotsShareArea(aborted, incoming),
  );
  readonly #quietResumeBudget = new QuietResumeBudget();
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
  #areaRefresh: Promise<void> | undefined;
  #runGeneration = 0;
  #started = false;
  #currentLocation = "";
  #lastError = "";
  #lastDecisionLogError = "";
  #mutationSequence = 0;

  constructor(options: NukeFireMapperOptions = {}) {
    this.#options = {
      areaPrefix: options.areaPrefix ?? "NukeFire Zone",
      storage: options.storage ?? (options.ephemeral ? "session" : "local"),
      updateCoordinates: options.updateCoordinates ?? true,
      decisionLogFile: options.decisionLogFile ?? DEFAULT_DECISION_LOG_FILE,
      searchForPerfectLayouts: options.searchForPerfectLayouts ?? true,
    };
    this.#decisionLogger = new MappingDecisionLogger(this.#options.decisionLogFile, (error) => {
      if (error === this.#lastDecisionLogError) return;
      this.#lastDecisionLogError = error;
      echo(`[nukefire-mapper] ${error}`);
    });
    this.#latencyLanes = new SnapshotLatencyLanes({
      snapshotKey: (snapshot) => snapshot.center,
      followCurrent: (snapshot) => this.#observeSnapshotCurrentRoom(snapshot),
      runTopology: (snapshot) => this.#runSnapshotLane(snapshot, false),
      runFullReflow: (snapshot, signal) => {
        const currentAreaKey = this.#polishEntries.currentAreaKey;
        const snapshotRoom = this.#roomsByVnum.get(snapshot.center);
        if (
          !currentAreaKey || !snapshotRoom ||
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

  #registerRoom(area: AreaMirror, room: RoomMirror): void {
    area.roomsByNumber.set(room.roomNumber, room);
    if (room.vnum !== undefined) this.#roomsByVnum.set(room.vnum, room);
  }

  #registerArea(area: AreaMirror): AreaMirror {
    this.#areasById.set(areaIdKey(area.id), area);
    const zone = Number(area.zone);
    if (Number.isSafeInteger(zone)) this.#zoneAreas.set(zone, area);
    return area;
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
      for (const [zone, area] of this.#zoneAreas) {
        if (area === known) this.#zoneAreas.delete(zone);
      }
    }

    const area: AreaMirror = {
      id,
      name: source.name,
      storage: source.storage,
      zone: source.data(NUKEFIRE_AREA_ID_PROPERTY),
      source: source.data(AREA_SOURCE_PROPERTY),
      polishPending: areaPolishPending(source.data(AREA_POLISH_PENDING_PROPERTY)),
      polishMemo: areaPolishMemo(
        source.data(AREA_POLISH_EXHAUSTED_FINGERPRINT_PROPERTY),
      ),
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
        zone: room.data(ROOM_ZONE_PROPERTY),
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
    runGeneration: number,
  ): Promise<void> {
    const transition = reduceAreaPolishState(area.polishPending, event);
    const memoTransition = reduceAreaPolishMemo(area.polishMemo, event);
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
    if (writes.length > 0) {
      await this.#whileCurrentRun(
        runGeneration,
        () => this.#mutateArea(
          area.id,
          async (mutation) => {
            for (const [name, value] of writes) {
              await mutation.setAreaProperty(name, value);
            }
          },
          `${transition.pending ? "Mark" : "Clear"} passive NukeFire layout polish for ${area.name}`,
        ),
      );
    }
    area.polishPending = transition.pending;
    area.polishMemo = memoTransition.memo;
    this.#logDecision({
      kind: "layout-polish-state",
      area: { id: area.id, name: area.name },
      event: event.kind,
      pending: transition.pending,
      exhaustedMemo: memoTransition.memo?.kind === "contexts",
      exhaustedContexts: memoTransition.memo?.kind === "contexts"
        ? memoTransition.memo.contextKeys.length
        : 0,
      propertyChanged: writes.length > 0,
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

  async #refreshAreaProjection(): Promise<void> {
    const refreshable = mapper as Mapper & { refreshAreas?: () => Promise<void> };
    if (typeof refreshable.refreshAreas === "function") {
      await refreshable.refreshAreas();
    } else {
      // Older 0.5.3 builds lack refreshAreas, but this empty presence-checked
      // import still supplies their initial-load barrier. Ownership handoff
      // refreshes become fully authoritative once the new op is available.
      await mapper.importAreasIfAbsent([]);
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
        area.polishPending,
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
    return room && area && isAdoptableStorage(area.storage, this.#options.storage)
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
      if (isAdoptableStorage(hostArea.storage, this.#options.storage)) {
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
      if (!isAdoptableStorage(hostArea.storage, this.#options.storage)) continue;
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
  ): void {
    this.#currentLocationFreshness.publishIfCurrent(
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

  async #reloadAreaMirrors(runGeneration: number): Promise<void> {
    await this.#whileCurrentRun(runGeneration, () => this.#refreshAreaProjection());
    this.#zoneAreas.clear();
    this.#areasById.clear();
    this.#roomsByVnum.clear();
    for (const area of mapper.areas) this.#hydrateArea(area);
  }

  #assertLivePlanningFingerprint(
    area: AreaMirror,
    expected: string,
    phase: "before Worker planning" | "after Worker planning" | "before applying layout",
  ): void {
    let actual: string;
    try {
      actual = livePlanningFingerprint(mapper.getAreaById(area.id));
    } catch {
      throw new StaleNukeFireLayoutPlanError(area, phase);
    }
    if (actual !== expected) throw new StaleNukeFireLayoutPlanError(area, phase);
  }

  async #ensureFreshAreas(): Promise<void> {
    if (this.#areasReady) return;
    const generation = this.#runGeneration;
    const refresh = this.#areaRefresh ??= (
      this.#refreshAreaProjection()
    );
    try {
      await refresh;
      if (this.#runGeneration === generation && !this.#areasReady) {
        this.#areasReady = true;
        this.#followCurrentRoomAfterRefresh(generation);
      }
    } finally {
      if (this.#areaRefresh === refresh) this.#areaRefresh = undefined;
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

    // A package can start before the session's initial durable-map load, and
    // a successor session can inherit mapping ownership with an older cache.
    // Refresh before any presence-based zone resolution to avoid duplicates.
    const initialRefresh = this.#ensureFreshAreas();
    void initialRefresh.catch((caught) => {
      const message = caught instanceof Error ? caught.message : String(caught);
      echo(`[nukefire-mapper] failed to refresh existing maps: ${message}`);
    });

    // The atlas is part of mapper initialization, rather than a side effect of
    // receiving the first Map.Local snapshot. Let the initial refresh settle
    // first so an older catalogue publication cannot hide the new atlas.
    // Area creation below awaits this same in-flight upsert, so startup and
    // mapping cannot create duplicates.
    void afterAreaRefresh(
      initialRefresh,
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
    this.#started = false;
    this.#runGeneration += 1;
    this.#latencyLanes.stop();
    this.#zoneAreas.clear();
    this.#areasById.clear();
    this.#roomsByVnum.clear();
    this.#plannedTopology.clear();
    this.#reconciledPortAreas.clear();
    this.#deferredReflowAreas.clear();
    this.#polishEntries.clear();
    this.#quietResumeBudget.clear();
    this.#pendingVerticalLinks.clear();
    this.#currentLocationFreshness.clear();
    this.#currentLocation = "";
    this.#areasReady = false;
    this.#areaRefresh = undefined;
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
        await this.#reloadAreaMirrors(runGeneration);
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
    await this.#whileCurrentRun(runGeneration, () => this.#ensureFreshAreas());
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
      if (room && area && isAdoptableStorage(area.storage, this.#options.storage)) {
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
        if (cached && cachedArea && isAdoptableStorage(cachedArea.storage, this.#options.storage)) {
          existing.set(source.vnum, cached);
          continue;
        }
        const hostRoom = mapper.findRoomByExternalId(externalRoomId(source.vnum));
        if (!hostRoom) continue;
        const hostArea = mapper.getAreaById(hostRoom.area_id);
        if (!isAdoptableStorage(hostArea.storage, this.#options.storage)) continue;
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
        if (!isAdoptableStorage(hostArea.storage, this.#options.storage)) continue;
        const area = this.#hydrateArea(hostArea);
        for (const room of area.roomsByNumber.values()) {
          if (room.vnum !== undefined && wanted.has(room.vnum) && !existing.has(room.vnum)) {
            existing.set(room.vnum, room);
          }
        }
      }
    }

    const knownAreaByZone = new Map<number, AreaMirror>();
    const currentExisting = existing.get(snapshot.center);
    if (currentExisting) {
      const area = this.#areasById.get(areaIdKey(currentExisting.areaId));
      if (area) knownAreaByZone.set(centerSource.zone, area);
    }
    for (const source of sources) {
      const room = existing.get(source.vnum);
      if (room && !knownAreaByZone.has(source.zone)) {
        const area = this.#areasById.get(areaIdKey(room.areaId));
        if (area) knownAreaByZone.set(source.zone, area);
      }
    }

    const preferredCenterName = this.#lastRoomInfo?.num === snapshot.center
      ? this.#lastRoomInfo.area.trim()
      : "";
    const areaByZone = new Map<number, AreaMirror>();
    for (const zone of new Set(sources.map((room) => room.zone))) {
      const preferredName = zone === centerSource.zone ? preferredCenterName : "";
      const area = await this.#resolveArea(
        zone,
        knownAreaByZone.get(zone),
        preferredName,
        runGeneration,
      );
      assertNotAborted(signal);
      areaByZone.set(zone, area);
    }

    const assignments: Assignment[] = sources.map((source) => {
      const indexedRoom = existing.get(source.vnum);
      const area = areaForObservedRoom(
        areaByZone.get(source.zone),
        indexedRoom && this.#areasById.get(areaIdKey(indexedRoom.areaId)),
      );
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
    await this.#syncLinks(links, rooms, runGeneration);
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
    await this.#syncClosedVerticalExits(verticalExits, rooms.get(snapshot.center), runGeneration);
    assertNotAborted(signal);
    this.#assertCurrentRun(runGeneration);

    if (currentWasRepositioned && current) {
      // SetPlayerLocation also derives the MapView translation from the room's
      // current coordinates. Repeat the otherwise-deduplicated notification
      // only after every reflow write has committed so the viewport follows a
      // player room that the layout moved.
      if (currentObservation) {
        this.#setCurrentRoom(current, currentObservation, runGeneration, true);
      }
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

  async #resolveArea(
    zone: number,
    known: AreaMirror | undefined,
    preferredName: string,
    runGeneration: number,
  ): Promise<AreaMirror> {
    this.#assertCurrentRun(runGeneration);
    const areaId = String(zone);
    const exact = findAreaByNukeFireId(mapper.areas, this.#options.storage, areaId);
    let area = exact ? this.#hydrateArea(exact) : undefined;
    if (!area) {
      const cached = this.#zoneAreas.get(zone);
      if (
        cached && isAdoptableStorage(cached.storage, this.#options.storage) &&
        (!cached.zone || cached.zone === areaId)
      ) {
        area = cached;
      }
    }
    if (
      !area && known && isAdoptableStorage(known.storage, this.#options.storage) &&
      (!known.zone || known.zone === areaId)
    ) {
      area = known;
    }
    if (!area && preferredName) {
      const source = findCompatibleAreaByName(
        mapper.areas,
        this.#options.storage,
        areaId,
        preferredName,
      );
      if (source) area = this.#hydrateArea(source);
    }
    if (!area) {
      // Re-read durable storage at the decision boundary too. Another mapper
      // instance may have created this zone since our ownership-start refresh.
      await this.#whileCurrentRun(runGeneration, () => this.#refreshAreaProjection());
      const refreshedExact = findAreaByNukeFireId(
        mapper.areas,
        this.#options.storage,
        areaId,
      );
      if (refreshedExact) area = this.#hydrateArea(refreshedExact);
      if (!area && preferredName) {
        const refreshedByName = findCompatibleAreaByName(
          mapper.areas,
          this.#options.storage,
          areaId,
          preferredName,
        );
        if (refreshedByName) area = this.#hydrateArea(refreshedByName);
      }
    }
    if (!area) {
      const atlas = await this.#ensureLocalAtlas(runGeneration);
      const areaName = preferredName || `${this.#options.areaPrefix} ${zone}`;
      const source = await this.#whileCurrentRun(
        runGeneration,
        () => this.#directMutation(
          undefined,
          "createArea",
          `Create NukeFire area ${areaName}`,
          () => mapper.createArea(areaName, { storage: this.#options.storage, atlas }),
          (created) => ({
            areaId: created.id,
            name: created.name,
            storage: created.storage,
          }),
        ),
      );
      area = this.#registerArea({
        id: source.id,
        name: source.name,
        storage: source.storage,
        polishPending: false,
        polishMemo: undefined,
        roomsByNumber: new Map(),
        connections: new Map(),
      });
    }

    this.#zoneAreas.set(zone, area);
    if (!area.zone || area.source !== SOURCE_NAME) {
      await this.#whileCurrentRun(
        runGeneration,
        () => this.#mutateArea(area.id, async (mutation) => {
          if (!area.zone) {
            await this.#whileCurrentRun(
              runGeneration,
              () => mutation.setAreaProperty(NUKEFIRE_AREA_ID_PROPERTY, areaId),
            );
          }
          if (area.source !== SOURCE_NAME) {
            await this.#whileCurrentRun(
              runGeneration,
              () => mutation.setAreaProperty(AREA_SOURCE_PROPERTY, SOURCE_NAME),
            );
          }
        }, `Bind NukeFire zone ${areaId}`),
      );
      area.zone = areaId;
      area.source = SOURCE_NAME;
    }

    const placeholder = `${this.#options.areaPrefix} ${zone}`;
    if (preferredName && area.name === placeholder && area.name !== preferredName) {
      await this.#whileCurrentRun(
        runGeneration,
        () => this.#directMutation(
          area.id,
          "renameArea",
          `Rename NukeFire area ${area.name} to ${preferredName}`,
          () => mapper.renameArea(area.id, preferredName),
        ),
      );
      area.name = preferredName;
    }
    return area;
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
        await this.#persistAreaPolishState(area, { kind: "topology-deferred" }, runGeneration);
        assertNotAborted(signal);
        // Growth is fresh evidence that polish can gain ground here; the
        // fruitless-resume allowance starts over with it.
        this.#quietResumeBudget.reset(areaKey(area));
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
        const nodes = stackVerticalTraversals(chartNodes, edges, establishedLevels, centerId);
        const constraintRepairPolicy = nukeFireConstraintRepairPolicy(
          this.#options.searchForPerfectLayouts,
          {
            residentCount: residents.length,
            edgeCount: edges.length,
          },
        );
        const startingFingerprint = mirrorPlanningFingerprint(area);
        const candidatePolishContext = createAreaPolishPlanningContext({
          geometryFingerprint: startingFingerprint,
          centerId,
          nodes,
          edges,
          searchForPerfectLayouts: this.#options.searchForPerfectLayouts,
          policy: constraintRepairPolicy,
        });
        // A cached mirror cannot authorize suppression: editor/package writes
        // may have changed live geometry without touching NukeFire's snapshot.
        this.#assertLivePlanningFingerprint(area, startingFingerprint, "before Worker planning");
        if (moveExisting && polishRetrySuppressed(area.polishMemo, candidatePolishContext)) {
          this.#polishEntries.consumeRetry(areaKey(area));
          this.#deferredReflowAreas.delete(areaKey(area));
          runPlanner = false;
          for (const assignment of group) assignment.planned = false;
          this.#logDecision({
            kind: "layout-polish-retry-skipped",
            area: { id: area.id, name: area.name },
            vnum: snapshot.center,
            memoContexts: area.polishMemo?.kind === "contexts"
              ? area.polishMemo.contextKeys.length
              : 0,
          });
        } else {
          polishContext = candidatePolishContext;
          stats.plannedAreas += 1;
          if (moveExisting) {
            // An attempt belongs to this exact entry context, not to every
            // subsequent room movement. Consume it only after the full key is
            // known not to be memoized, immediately before cancelable work.
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
              residents,
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
              await this.#whileCurrentRun(
                runGeneration,
                () => this.#mutateArea(area.id, async (mutation) => {
                  assertNotAborted(planningSignal);
                  const coordinateWriteStartedAt = performance.now();
                  await this.#whileCurrentRun(
                    runGeneration,
                    () => mutation.updateRooms(updates),
                  );
                  assertNotAborted(planningSignal);
                  stats.coordinateWriteMs += performance.now() - coordinateWriteStartedAt;
                  stats.movedRooms += updates.length;
                  for (const [number, fields] of updates) {
                    const room = residentRooms.get(number);
                    if (!room) continue;
                    room.position = roundedPosition(
                      fields.x ?? room.position.x,
                      fields.y ?? room.position.y,
                      fields.level ?? room.position.level,
                    );
                  }
                  const routeWriteStartedAt = performance.now();
                  await this.#syncAreaConnectionRoutes(
                    area,
                    residentRooms,
                    candidate.positions,
                    runGeneration,
                    mutation,
                    candidate.routeAmendments,
                  );
                  assertNotAborted(planningSignal);
                  stats.routeWriteMs += performance.now() - routeWriteStartedAt;
                }, `Apply progressive NukeFire reflow for ${area.name}`),
              );
              for (const update of reconciled) appliedPositionIds.add(update.id);
              // The transaction can move a newer current room which belongs to
              // this area even when this plan's snapshot center is stale.
              this.#refreshMovedCurrentRoom(area, reconciled, runGeneration);
              assertNotAborted(planningSignal);
              stats.batchCommitMs += performance.now() - batchStartedAt;
              expectedFingerprint = mirrorPlanningFingerprint(area);
              this.#logDecision({
                kind: "layout-progress-applied",
                area: { id: area.id, name: area.name },
                quality: candidate.quality,
                movedRooms: updates.length,
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
          try {
            const runWorker = async (): Promise<IntegralLayoutPlan> => {
              assertNotAborted(planningSignal);
              const plannerStartedAt = performance.now();
              let planned: IntegralLayoutPlan;
              try {
                planned = await this.#whileCurrentRun(
                  runGeneration,
                  () => planIntegralLayoutAsync({
                    nodes,
                    residents,
                    edges,
                    centerId,
                    allowExistingMoves: moveExisting,
                    trace: trace ? (event) => trace.push(event) : undefined,
                  }, moveExisting
                    ? {
                      signal: planningSignal,
                      onProgress: publishImprovement,
                      constraintRepair: constraintRepairPolicy,
                    }
                    : undefined),
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

            const planned = await runWorker();
            assertNotAborted(signal);
            plan = planned;
            if (diagnosticContext && planned) {
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
          } catch (caught) {
            if (caught instanceof ObsoleteNukeFireMapperRunError) throw caught;
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
        await this.#whileCurrentRun(
          runGeneration,
          () => this.#mutateArea(area.id, async (mutation) => {
            if (updates.length > 0) {
              const coordinateWriteStartedAt = performance.now();
              await this.#whileCurrentRun(
                runGeneration,
                () => mutation.updateRooms(updates),
              );
              assertNotAborted(signal);
              stats.coordinateWriteMs += performance.now() - coordinateWriteStartedAt;
              stats.movedRooms += updates.length;
              for (const [number, fields] of updates) {
                const room = residentRooms.get(number);
                if (!room) continue;
                room.position = roundedPosition(
                  fields.x ?? room.position.x,
                  fields.y ?? room.position.y,
                  fields.level ?? room.position.level,
                );
              }
            }
            const routeWriteStartedAt = performance.now();
            await this.#syncAreaConnectionRoutes(
              area,
              residentRooms,
              plan.positions,
              runGeneration,
              mutation,
              plan.routeAmendments,
            );
            assertNotAborted(signal);
            stats.routeWriteMs += performance.now() - routeWriteStartedAt;
          }, `Reflow NukeFire area ${area.name}`),
        );
        for (const update of reconciled) appliedPositionIds.add(update.id);
        this.#refreshMovedCurrentRoom(area, reconciled, runGeneration);
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
        // the completed tournament.
        const completedContext = polishContext === undefined || !improved
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
        // so the fruitless-resume allowance starts over.
        this.#quietPolishClaims.discharge(snapshot, areaKey(area));
        this.#quietResumeBudget.reset(areaKey(area));
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

  async #syncAreaConnectionRoutes(
    area: AreaMirror,
    residentRooms: ReadonlyMap<RoomNumber, RoomMirror>,
    positions: ReadonlyMap<string, GridPosition>,
    runGeneration: number,
    mutation?: AreaMutator,
    routeAmendments?: readonly RouteAmendment[],
  ): Promise<void> {
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
      // recomputation proposes nothing for it, so its geometry survives every
      // commit while its endpoints still reserve their wall slots below.
      if (routeIsManuallyAuthored(connection.routing)) continue;
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
      const obstructions = directRoomObstructions(positionA, positionB, occupied);
      // A matching engine amendment supplies the generated route directly,
      // oriented from this connection's endpoint A toward endpoint B. Manual
      // connections never reach this point, so an amendment can never touch
      // an author-drawn route.
      const amendmentWaypoints = amendmentWaypointsBetween(
        amendmentIndex,
        connection.endpointA.room_number,
        endpointB.room_number,
      );
      const desired = this.#desiredConnectionGeometry(
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
      const before = {
        endpoint_a: copyEndpoint(connection.endpointA),
        endpoint_b: copyEndpoint(currentEndpointB),
        routing: connection.routing,
        segment_shape: connection.segmentShape,
        corner: connection.corner,
        route_points: connection.routePoints.map((point) => ({ ...point })),
      };
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
        () => mutation.setRoomProperty(room.roomNumber, ROOM_ZONE_PROPERTY, String(source.zone)),
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
  ): Promise<void> {
    this.#assertCurrentRun(runGeneration);
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
        const area = this.#areasById.get(areaIdKey(areaId));
        if (routesNeedSync && area) {
          // A Connection created earlier in one host mutation is not visible to
          // a later update operation in that same envelope. Route and port
          // geometry therefore gets its own committed-topology pass.
          await this.#whileCurrentRun(
            runGeneration,
            () => this.#mutateArea(areaId, (mutation) =>
              this.#syncAreaConnectionRoutes(
                area,
                area.roomsByNumber,
                new Map([...area.roomsByNumber.values()].map((room) => [
                  residentId(room.roomNumber),
                  room.position,
                ])),
                runGeneration,
                mutation,
              ), "Route NukeFire map links"),
          );
        }
      } catch (caught) {
        if (caught instanceof ObsoleteNukeFireMapperRunError) throw caught;
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
            () => this.#mutateArea(areaId, (mutation) =>
              this.#syncAreaConnectionRoutes(
                refreshedArea,
                refreshedArea.roomsByNumber,
                new Map([...refreshedArea.roomsByNumber.values()].map((room) => [
                  residentId(room.roomNumber),
                  room.position,
                ])),
                runGeneration,
                mutation,
              ), routesNeedSync
                ? "Route retried NukeFire map links"
                : "Route committed NukeFire map links"),
          );
        }
      }
    }
    for (const work of crossArea) {
      await this.#syncLink(work.link, work.from, work.to, work.mapped, runGeneration);
    }
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
  ): Promise<void> {
    this.#assertCurrentRun(runGeneration);
    if (!room) return;
    const pending: {
      observation: VerticalExitObservation;
      existing?: ExitMirror;
    }[] = [];
    for (const observation of observations) {
      if (!observation.closed || observation.destination !== undefined) continue;
      const existing = matchingExit(room, observation.mapped);
      if (existing?.closed) continue;
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
    if (pending.length === 0) return;

    await this.#whileCurrentRun(
      runGeneration,
      () => this.#mutateArea(room.areaId, async (mutation) => {
        for (const { observation, existing } of pending) {
          if (!existing) {
            const fields: ExitArgs = {
              from_direction: observation.mapped.direction,
              is_closed: true,
              is_locked: false,
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
          await this.#whileCurrentRun(
            runGeneration,
            () => mutation.setRoomExit(
              room.roomNumber,
              existing.id as ExitId,
              { is_closed: true },
            ),
          );
          existing.closed = true;
        }
      }, `Apply NukeFire vertical exits for room ${room.roomNumber}`),
    );
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
        is_closed: link.closed,
        is_locked: link.locked,
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
        is_closed: link.closed,
        is_locked: link.locked,
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
      is_closed: closed,
      is_locked: locked,
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
