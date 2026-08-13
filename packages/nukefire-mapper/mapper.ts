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
  planIntegralLayout,
  type GridPosition,
  type LayoutDirection,
  type LayoutEdge,
  type LayoutNode,
  type LayoutResident,
  type LayoutTraceEvent,
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
  directRoomObstructions,
  routeAroundRooms,
  routeEndSide,
  routeStartSide,
  routeTurnPoints,
  type RouteSide,
} from "./routing.ts";
import {
  verticalExitObservations,
  verticalMapLinks,
  type VerticalExitObservation,
} from "./room-info.ts";
import { stackVerticalTraversals } from "./vertical-levels.ts";
import { reflowPolicy } from "./reflow-policy.ts";

const AREA_SOURCE_PROPERTY = "nukefire.mapper";
const ROOM_ZONE_PROPERTY = "nukefire.zone";
const ROOM_TERRAIN_PROPERTY = "terrain";
const ROOM_LAYOUT_LOCK_PROPERTY = "nukefire.layout.locked";
const SOURCE_NAME = "NukeFire.Map.Local";

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
}

interface Assignment {
  source: NukeFireMapRoom;
  area: AreaMirror;
  room?: RoomMirror;
  position?: GridPosition;
  positionApplied?: boolean;
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
  routePoints: MapPoint[];
}

interface AreaMirror {
  id: AreaId;
  name: string;
  storage: MapStorage;
  zone?: string;
  source?: string;
  roomsByNumber: Map<RoomNumber, RoomMirror>;
  connections: Map<string, ConnectionMirror>;
}

interface DesiredConnectionGeometry {
  endpoint_a: ConnectionEndpoint;
  endpoint_b: ConnectionEndpoint;
  routing: ConnectionRouting;
  segment_shape: ConnectionSegmentShape;
  route_points: MapPoint[];
}

function clone<T>(value: Readonly<T>): T {
  return JSON.parse(JSON.stringify(value)) as T;
}

function areaKey(area: { id: AreaId }): string {
  return areaIdKey(area.id);
}

function areaIdKey(areaId: AreaId): string {
  return `${areaId[0]}:${areaId[1]}`;
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

function exitFromFields(fields: ExitArgs, id?: ExitId): ExitMirror {
  return {
    id,
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
    routePoints: connection.route_points.map((point) => ({ x: point.x, y: point.y })),
  };
}

function connectionMirrorKey(id: ConnectionId): string {
  return `${id[0]}:${id[1]}`;
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
  #lastRoomInfo: RoomInfo | undefined;
  #lastSnapshot: NukeFireMapLocal | undefined;
  readonly #pending: NukeFireMapLocal[] = [];
  /** Traversals already allowed to trigger one expensive geometry reflow. */
  readonly #plannedTopology = new Set<string>();
  /** Areas which grew while newer movement snapshots were already queued. */
  readonly #deferredReflowAreas = new Set<string>();
  /** Numeric Room.Info vertical exits waiting for their destination room. */
  readonly #pendingVerticalLinks = new Map<string, NukeFireMapLink>();
  #running = false;
  #started = false;
  #currentLocation = "";
  #lastError = "";
  #lastDecisionLogError = "";

  constructor(options: NukeFireMapperOptions = {}) {
    this.#options = {
      areaPrefix: options.areaPrefix ?? "NukeFire Zone",
      storage: options.storage ?? (options.ephemeral ? "session" : "local"),
      updateCoordinates: options.updateCoordinates ?? true,
      decisionLogFile: options.decisionLogFile ?? DEFAULT_DECISION_LOG_FILE,
    };
    this.#decisionLogger = new MappingDecisionLogger(this.#options.decisionLogFile, (error) => {
      if (error === this.#lastDecisionLogError) return;
      this.#lastDecisionLogError = error;
      echo(`[nukefire-mapper] ${error}`);
    });
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
    try {
      await mapper.mutateArea(areaId, callback, { description });
    } catch (error) {
      try {
        this.#hydrateArea(mapper.getAreaById(areaId), true);
      } catch {
        // Preserve the original mutation failure if recovery itself cannot read.
      }
      throw error;
    }
  }

  start(): void {
    if (this.#started) return;
    this.#started = true;

    this.#logDecision({
      kind: "session-start",
      options: { ...this.#options },
    });
    if (this.#decisionLogger.path) {
      echo(`[nukefire-mapper] mapping decisions: ${this.#decisionLogger.path}`);
    }

    this.#subscriptions.push(
      watchMessage("Room.Info", (info) => {
        this.#lastRoomInfo = info ? clone(info) : undefined;
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
    // replay. Seed from the current tree when this package starts late.
    const currentRoom = nukefire.value?.Room?.Info;
    if (currentRoom && !this.#lastRoomInfo) this.#lastRoomInfo = clone(currentRoom);
    const current = nukefire.value?.NukeFire?.Map?.Local;
    if (current) {
      const stable = clone(current);
      this.#lastSnapshot = stable;
      this.#enqueue(stable);
    }
  }

  stop(): void {
    for (const subscription of this.#subscriptions.splice(0)) subscription.off();
    this.#pending.length = 0;
    this.#zoneAreas.clear();
    this.#areasById.clear();
    this.#roomsByVnum.clear();
    this.#plannedTopology.clear();
    this.#deferredReflowAreas.clear();
    this.#pendingVerticalLinks.clear();
    this.#currentLocation = "";
    this.#started = false;
  }

  #enqueue(snapshot: NukeFireMapLocal): void {
    const stable = snapshot;
    const last = this.#pending.at(-1);
    if (last?.center === stable.center) {
      // GPS overlays and door changes can republish the same neighborhood.
      // Only the newest unprocessed version of one center is useful.
      this.#pending[this.#pending.length - 1] = stable;
    } else {
      // Preserve movement through distinct centers: an intermediate local grid
      // may contain rooms that the next one no longer exposes.
      this.#pending.push(stable);
    }
    if (!this.#running) void this.#drain();
  }

  async #drain(): Promise<void> {
    this.#running = true;
    try {
      while (this.#pending.length > 0 && this.#started) {
        const snapshot = this.#pending.shift();
        if (!snapshot) continue;
        try {
          // Ingest intermediate movement snapshots without repeatedly moving
          // the entire atlas. The newest queued snapshot performs the one
          // deferred full reflow for every area it contains.
          await this.#syncSnapshot(snapshot, this.#pending.length === 0);
          this.#lastError = "";
        } catch (caught) {
          const message = caught instanceof Error ? caught.message : String(caught);
          if (message !== this.#lastError) {
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
        }
      }
    } finally {
      this.#running = false;
      if (this.#pending.length > 0 && this.#started) void this.#drain();
    }
  }

  async #syncSnapshot(snapshot: NukeFireMapLocal, allowExistingReflow: boolean): Promise<void> {
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
    // it chose the other storage mode, scan matching areas once and mirror them.
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
      areaByZone.set(zone, await this.#resolveArea(zone, knownAreaByZone.get(zone), preferredName));
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

    const planningStartedAt = performance.now();
    const planning = await this.#planAssignments(
      assignments,
      snapshot,
      centerSource,
      links,
      allowExistingReflow,
    );
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
      const area = group[0].area;
      await this.#mutateArea(area.id, async (mutation) => {
        for (const assignment of group) {
          const mapped = await this.#syncRoom(assignment, mutation);
          rooms.set(assignment.source.vnum, mapped);
        }
      }, `Apply NukeFire rooms for ${area.name}`);
    }
    const roomsFinishedAt = performance.now();

    const current = rooms.get(snapshot.center);
    if (current) {
      const key = `${areaIdKey(current.areaId)}:${current.roomNumber}`;
      if (key !== this.#currentLocation) {
        mapper.setCurrentLocation(current.areaId, current.roomNumber);
        this.#currentLocation = key;
      }
    }

    await this.#syncLinks(links, rooms);
    for (const [key, link] of this.#pendingVerticalLinks) {
      const from = rooms.get(link.from) ?? this.#roomsByVnum.get(link.from);
      const to = rooms.get(link.to) ?? this.#roomsByVnum.get(link.to);
      const mapped = mapDirection(link.direction);
      if (to && exitLeadsTo(from && matchingExit(from, mapped), to)) {
        this.#pendingVerticalLinks.delete(key);
      }
    }
    await this.#syncClosedVerticalExits(verticalExits, rooms.get(snapshot.center));

    const finishedAt = performance.now();
    if (planning.plannedAreas > 0 || finishedAt - startedAt >= 100) {
      this.#logDecision({
        kind: "mapping-performance",
        center: snapshot.center,
        rooms: assignments.length,
        links: links.length,
        allowExistingReflow,
        queuedSnapshots: this.#pending.length,
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
  ): Promise<AreaMirror> {
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
      const source = await mapper.createArea(
        preferredName || `${this.#options.areaPrefix} ${zone}`,
        { storage: this.#options.storage },
      );
      area = this.#registerArea({
        id: source.id,
        name: source.name,
        storage: source.storage,
        roomsByNumber: new Map(),
        connections: new Map(),
      });
    }

    this.#zoneAreas.set(zone, area);
    if (!area.zone || area.source !== SOURCE_NAME) {
      await this.#mutateArea(area.id, async (mutation) => {
        if (!area.zone) {
          await mutation.setAreaProperty(NUKEFIRE_AREA_ID_PROPERTY, areaId);
        }
        if (area.source !== SOURCE_NAME) {
          await mutation.setAreaProperty(AREA_SOURCE_PROPERTY, SOURCE_NAME);
        }
      }, `Bind NukeFire zone ${areaId}`);
      area.zone = areaId;
      area.source = SOURCE_NAME;
    }

    const placeholder = `${this.#options.areaPrefix} ${zone}`;
    if (preferredName && area.name === placeholder && area.name !== preferredName) {
      await mapper.renameArea(area.id, preferredName);
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
  ): Promise<AssignmentPlanStats> {
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
      const deferredReflow = this.#deferredReflowAreas.has(areaKey(area));
      const policy = reflowPolicy(
        topologyGrowth,
        deferredReflow,
        allowExistingReflow,
        this.#options.updateCoordinates,
      );
      const { runPlanner, moveExisting } = policy;
      if (topologyGrowth) stats.topologyGrowthAreas += 1;

      let plan: Pick<ReturnType<typeof planIntegralLayout>, "positions" | "movedExisting">;
      if (!runPlanner) {
        // The common walking path is intentionally O(residents): no edge graph,
        // candidate generation, scoring, or routing search is needed.
        plan = {
          positions: new Map(residents.map((resident) => [resident.id, resident.position])),
          movedExisting: new Set<string>(),
        };
      } else {
        stats.plannedAreas += 1;
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
        for (const assignment of group) {
          if (!assignment.room) continue;
          establishedLevels.set(
            assignmentIds.get(assignment.source.vnum) as string,
            assignment.room.position.level,
          );
        }
        // NukeFire may flow an up/down destination on its source's z plane;
        // this mapper always stacks vertical traversals across map levels.
        const nodes = stackVerticalTraversals(chartNodes, edges, establishedLevels, centerId);
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
        try {
          const plannerStartedAt = performance.now();
          const planned = planIntegralLayout({
            nodes,
            residents,
            edges,
            centerId,
            allowExistingMoves: moveExisting,
            trace: trace ? (event) => trace.push(event) : undefined,
          });
          stats.plannerMs += performance.now() - plannerStartedAt;
          plan = planned;
          if (diagnosticContext) {
            this.#logDecision({
              kind: "layout-decision",
              ...diagnosticContext,
              trace,
              result: {
                quality: planned.quality,
                movedExisting: [...planned.movedExisting].sort(),
                positions: serializedPositions(planned.positions),
              },
            });
          }
        } catch (caught) {
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
        }
        if (policy.deferExistingReflow) {
          this.#deferredReflowAreas.add(areaKey(area));
        } else {
          this.#deferredReflowAreas.delete(areaKey(area));
        }
      }

      for (const assignment of group) {
        const id = assignmentIds.get(assignment.source.vnum) as string;
        const position = plan.positions.get(id);
        if (!position) throw new Error(`layout omitted room #${assignment.source.vnum}`);
        assignment.position = position;
      }

      const updates: [RoomNumber, UpdateRoomParams][] = [];
      for (const id of plan.movedExisting) {
        const room = roomById.get(id);
        const position = plan.positions.get(id);
        if (!room || !position) continue;
        updates.push([room.roomNumber, {
          x: position.x,
          y: position.y,
          level: position.level,
        }]);
      }
      if (topologyGrowth || updates.length > 0) {
        const batchStartedAt = performance.now();
        await this.#mutateArea(area.id, async (mutation) => {
          if (updates.length > 0) {
            const coordinateWriteStartedAt = performance.now();
            await mutation.updateRooms(updates);
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
          await this.#syncAreaConnectionRoutes(area, residentRooms, plan.positions, mutation);
          stats.routeWriteMs += performance.now() - routeWriteStartedAt;
        }, `Reflow NukeFire area ${area.name}`);
        stats.batchCommitMs += performance.now() - batchStartedAt;
      }

      for (const key of introducedTopology) this.#plannedTopology.add(key);

      for (const assignment of group) {
        const id = assignmentIds.get(assignment.source.vnum) as string;
        assignment.positionApplied = assignment.room !== undefined && plan.movedExisting.has(id);
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
    const obstructed = knownObstructed ??
      directRoomObstructions(positionA, positionB, occupied).length > 0;
    if (obstructed) {
      const path = routeAroundRooms(positionA, positionB, occupied, preferredStart, preferredEnd);
      if (path) {
        return {
          endpoint_a: { ...baseA, side: routeStartSide(path) ?? preferredStart },
          endpoint_b: { ...baseB, side: routeEndSide(path) ?? preferredEnd },
          routing: "Manual",
          segment_shape: "Orthogonal",
          route_points: routeTurnPoints(path),
        };
      }
    }
    return {
      endpoint_a: { ...baseA, side: preferredStart },
      endpoint_b: { ...baseB, side: preferredEnd },
      routing: "Automatic",
      segment_shape: "Direct",
      route_points: [],
    };
  }

  async #syncAreaConnectionRoutes(
    area: AreaMirror,
    residentRooms: ReadonlyMap<RoomNumber, RoomMirror>,
    positions: ReadonlyMap<string, GridPosition>,
    mutation?: AreaMutator,
  ): Promise<void> {
    const byRoomNumber = new Map<RoomNumber, GridPosition>();
    for (const number of residentRooms.keys()) {
      const position = positions.get(residentId(number));
      if (position) byRoomNumber.set(number, position);
    }
    // Include planned rooms which have not been created yet. They can obstruct
    // an older connection during the same topology-growth transaction.
    const occupied = [...positions.values()];
    const changes: unknown[] = [];

    for (const connection of area.connections.values()) {
      const endpointB = connection.endpointB;
      if (!endpointB) continue;
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
      const exitA = roomA.exits.find((exit) =>
        exit.toAreaId && sameAreaId(exit.toAreaId, area.id) &&
        exit.toRoomNumber === roomB.roomNumber
      );
      const exitB = roomB.exits.find((exit) =>
        exit.toAreaId && sameAreaId(exit.toAreaId, area.id) &&
        exit.toRoomNumber === roomA.roomNumber
      );
      const preferredStart = directionSide(exitA?.fromDirection ?? "Other", deltaX, deltaY) as RouteSide;
      const preferredEnd = directionSide(exitB?.fromDirection ?? "Other", -deltaX, -deltaY) as RouteSide;
      const obstructions = directRoomObstructions(positionA, positionB, occupied);
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
      );
      const desiredSignature = geometrySignature(desired);
      if (connectionSignature(connection) === desiredSignature) {
        continue;
      }
      const before = {
        endpoint_a: copyEndpoint(connection.endpointA),
        endpoint_b: copyEndpoint(endpointB),
        routing: connection.routing,
        segment_shape: connection.segmentShape,
        route_points: connection.routePoints.map((point) => ({ ...point })),
      };
      if (mutation) await mutation.setConnection(connection.id, desired);
      else await mapper.setConnection(area.id, connection.id, desired);
      connection.endpointA = copyEndpoint(desired.endpoint_a);
      connection.endpointB = copyEndpoint(desired.endpoint_b);
      connection.routing = desired.routing;
      connection.segmentShape = desired.segment_shape;
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
        reason: desired.routing === "Manual"
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

  async #syncRoom(assignment: Assignment, mutation: AreaMutator): Promise<RoomMirror> {
    const source = assignment.source;
    const position = assignment.position;
    if (!position) throw new Error(`layout omitted room #${source.vnum}`);
    const { x, y, level } = position;
    const color = terrainColor(source.terrain);
    let room = assignment.room;
    const created = !room;

    if (!room) {
      const number = await mutation.createRoom({
        title: source.name,
        level,
        x,
        y,
        color,
        externalId: externalRoomId(source.vnum),
      });
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
    if (!assignment.positionApplied && (this.#options.updateCoordinates || created)) {
      if (room.position.level !== level) updates.level = level;
      if (room.position.x !== x) updates.x = x;
      if (room.position.y !== y) updates.y = y;
    }
    if (Object.keys(updates).length > 0) {
      await mutation.updateRoom(room.roomNumber, updates);
      if (updates.title !== undefined) room.title = updates.title;
      if (updates.color !== undefined) room.color = updates.color;
      room.position = roundedPosition(
        updates.x ?? room.position.x,
        updates.y ?? room.position.y,
        updates.level ?? room.position.level,
      );
    }

    if (room.zone !== String(source.zone)) {
      await mutation.setRoomProperty(room.roomNumber, ROOM_ZONE_PROPERTY, String(source.zone));
      room.zone = String(source.zone);
    }
    if (room.terrain !== source.terrain) {
      await mutation.setRoomProperty(room.roomNumber, ROOM_TERRAIN_PROPERTY, source.terrain);
      room.terrain = source.terrain;
    }
    return room;
  }

  async #syncLinks(links: readonly NukeFireMapLink[], rooms: Map<number, RoomMirror>): Promise<void> {
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
        await this.#mutateArea(areaId, async (mutation) => {
          for (const work of group) {
            await this.#syncLink(work.link, work.from, work.to, work.mapped, mutation);
          }
        }, "Apply NukeFire map links");
      } catch {
        // A drafted createLink cannot discover a host topology rejection until
        // submission. The failed batch has already rehydrated this area, so retry
        // each link against fresh mirrors and preserve the established traversal
        // fallback for unusual or duplicate topologies.
        for (const work of group) {
          const from = this.#roomsByVnum.get(work.link.from);
          if (!from) continue;
          const to = this.#roomsByVnum.get(work.link.to);
          await this.#syncLink(work.link, from, to, work.mapped);
        }
      }
    }
    for (const work of crossArea) {
      await this.#syncLink(work.link, work.from, work.to, work.mapped);
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
  ): Promise<void> {
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
      }
      pending.push({ observation, existing });
    }
    if (pending.length === 0) return;

    await this.#mutateArea(room.areaId, async (mutation) => {
      for (const { observation, existing } of pending) {
        if (!existing) {
          const fields: ExitArgs = {
            from_direction: observation.mapped.direction,
            is_closed: true,
            is_locked: false,
            weight: 1,
            command: observation.mapped.command,
          };
          const id = await mutation.createRoomExit(room.roomNumber, fields);
          room.exits.push(exitFromFields(fields, id));
          continue;
        }
        await mutation.setRoomExit(room.roomNumber, existing.id as ExitId, { is_closed: true });
        existing.closed = true;
      }
    }, `Apply NukeFire vertical exits for room ${room.roomNumber}`);
  }

  async #syncLink(
    link: Readonly<NukeFireMapLink>,
    from: RoomMirror,
    to: RoomMirror | undefined,
    mapped: MappedDirection,
    mutation?: AreaMutator,
  ): Promise<void> {
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
        await this.#createLocalLink(link, from, to, mapped, reverseMapped, mutation);
        return;
      } catch {
        // An unusual/duplicate topology can reject atomic pairing. The
        // traversal fallback below still records the server-authoritative exits.
      }
    }

    await this.#ensureTraversal(from, to, mapped, fromExit, link.closed, link.locked, mutation);
    if (link.bidirectional && to && reverseMapped) {
      await this.#ensureTraversal(to, from, reverseMapped, reverseExit, link.closed, link.locked, mutation);
    }
  }

  async #createLocalLink(
    link: Readonly<NukeFireMapLink>,
    from: RoomMirror,
    to: RoomMirror,
    mapped: MappedDirection,
    reverse: MappedDirection | undefined,
    mutation?: AreaMutator,
  ): Promise<void> {
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

    const connectionId = mutation
      ? await mutation.createLink({
        ...geometry,
        traversals,
      })
      : await mapper.createLink(from.areaId, {
        ...geometry,
        traversals,
      });
    area.connections.set(connectionMirrorKey(connectionId), {
      id: connectionId,
      endpointA: copyEndpoint(geometry.endpoint_a),
      endpointB: copyEndpoint(geometry.endpoint_b),
      routing: geometry.routing,
      segmentShape: geometry.segment_shape,
      routePoints: geometry.route_points.map((point) => ({ ...point })),
    });
    from.exits.push(exitFromFields(traversals[0]));
    if (traversals[1]) to.exits.push(exitFromFields(traversals[1]));
  }

  async #ensureTraversal(
    from: RoomMirror,
    to: RoomMirror | undefined,
    mapped: MappedDirection,
    existing: ExitMirror | undefined,
    closed: boolean,
    locked: boolean,
    mutation?: AreaMutator,
  ): Promise<void> {
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

    if (existing && exitMatchesFields(existing, fields)) return;
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
        if (!hostExit) return;
        existing.id = hostExit.id;
      }
      if (mutation) await mutation.setRoomExit(from.roomNumber, existing.id, fields);
      else await mapper.setRoomExit(from.areaId, from.roomNumber, existing.id, fields);
      applyExitFields(existing, fields);
    } else {
      const id = mutation
        ? await mutation.createRoomExit(from.roomNumber, fields)
        : await mapper.createRoomExit(from.areaId, from.roomNumber, fields);
      from.exits.push(exitFromFields(fields, id));
    }
  }
}
