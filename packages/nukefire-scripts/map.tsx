// =============================================================================
//  Map pane — the smudgy MapView with a room header and a live GPS strip
// =============================================================================
//  Everything in this pane is fed by bindings (room name/area from Room.Info,
//  the GPS line from a small derived state), so the widget mounts once and
//  never rebuilds — the MapView keeps its zoom and pan across room changes.

import {
  createState,
  getSessions,
  getSettings,
  mapper,
  send,
  session,
  type BoundStateConsumer,
  type EventSubscription,
  type StateConsumer,
} from "smudgy:core";
import { room as mapRoomChanged } from "smudgy:events/map";
import {
  connected as sessionConnected,
  created as sessionCreated,
  destroyed as sessionDestroyed,
  disconnected as sessionDisconnected,
} from "smudgy:events/sessions";
import {
  Button,
  Checkbox,
  Column,
  Container,
  MapView,
  Row,
  Scrollable,
  Space,
  Text,
  createWidget,
  type MapStyleApplication,
} from "smudgy:widgets";
// Generated from index.ts for installed packages; the checked-in workspace
// typings describe the previous package version and do not know this export.
// @ts-ignore generated self-state module
import { sessionMap as generatedSessionMapConsumer } from "smudgy:state/kapusniak/nukefire-scripts";
import {
  layoutDiagnostics,
  layoutState,
} from "smudgy:state/kapusniak/nukefire-mapper";
import {
  nukefire,
  watchMessage,
  type CharGps,
} from "smudgy://kapusniak/nukefire-gmcp";
import { showLayoutState, widgetTextSize } from "./config.ts";
import {
  CURRENT_ROOM_STYLE,
  ROUTE_STYLE,
  currentRoomMapViewApply,
  gpsRouteRaw,
  mapViewRoute,
  type RouteRoom,
} from "./map-route.ts";
import { GPS_CLEAR, UI } from "./theme.ts";
import {
  TRACK_STYLE,
  UNVISITED_STYLE,
  VISITED_STORAGE_KEY,
  advanceTrack,
  clearTracks,
  expandTrackedExitRefs,
  markAreaUnvisited,
  parseVisitedRooms,
  rememberVisitedRoom,
  trackApplication,
  type MapExitRef,
  type MapLocation,
  type SessionMapSnapshot,
  type VisitedRooms,
} from "./map-activity.ts";
import { sessionMap } from "./index.ts";

const PANE = "Map";
const UNVISITED_OPACITY = 0.4;
const MAP_MENU_HEIGHT = 116;
const PLAYER_COLORS = [
  "#e76f51",
  "#2a9d8f",
  "#9b5de5",
  "#4cc9f0",
  "#f72585",
  "#90be6d",
  "#ff9f1c",
  "#577590",
] as const;

interface GpsView {
  line: string;
  color: string;
}

const gpsView = createState<GpsView>("gpsView");
gpsView.set({ line: "no route set", color: UI.faint });
const mapApply = createState<MapStyleApplication[]>("mapApply");
mapApply.set([]);
const mapControls = createState<{ menuHeight: number; tracksVisible: boolean }>("mapControls");
mapControls.set({ menuHeight: 0, tracksVisible: true });

let latestGps: Readonly<CharGps> | undefined;
let initialized = false;
let mounted = false;

interface DirectedMapState {
  view: BoundStateConsumer<SessionMapSnapshot>;
  subscription: EventSubscription;
}

const directedMapConsumer = generatedSessionMapConsumer as StateConsumer<SessionMapSnapshot>;
const directedMaps = new Map<number, DirectedMapState>();

function currentMappedRoom(): Room | undefined {
  const location = mapper.getCurrentLocation();
  if (location?.room !== undefined) {
    return mapper.getAreaById(location.area).room(location.room);
  }
  const vnum = nukefire.value?.Room?.Info?.num;
  return Number.isSafeInteger(vnum)
    ? mapper.findRoomByExternalId(String(vnum))
    : undefined;
}

function areaById(id: string): Area | undefined {
  return mapper.areas.find((area) => area.id === id);
}

function ownMapSnapshot(): SessionMapSnapshot | undefined {
  const location = sessionMap.value.location;
  const tracks = sessionMap.value.tracks;
  if (!location && !tracks) return undefined;
  return JSON.parse(JSON.stringify({ location, tracks: tracks ?? {} })) as SessionMapSnapshot;
}

function currentMapLocation(): MapLocation | undefined {
  const current = currentMappedRoom();
  if (!current) return undefined;
  try {
    return {
      areaId: mapper.getAreaById(current.area_id).id,
      roomNumber: current.room_number,
    };
  } catch {
    return undefined;
  }
}

function loadVisitedRooms(): VisitedRooms {
  return parseVisitedRooms(localStorage.getItem(VISITED_STORAGE_KEY));
}

function saveVisitedRooms(visited: VisitedRooms): void {
  localStorage.setItem(VISITED_STORAGE_KEY, JSON.stringify(visited));
}

function rememberLocation(location: MapLocation): void {
  const visited = loadVisitedRooms();
  const next = rememberVisitedRoom(visited, location);
  if (next !== visited) saveVisitedRooms(next);
}

function traversedExit(
  previous: MapLocation | undefined,
  next: MapLocation,
): { areaId: string; exit: MapExitRef } | undefined {
  if (!previous || previous.roomNumber === null || next.roomNumber === null) return undefined;
  const previousArea = areaById(previous.areaId);
  const nextArea = areaById(next.areaId);
  const previousRoom = previousArea?.room(previous.roomNumber);
  if (!previousArea || !nextArea || !previousRoom) return undefined;
  const exit = previousRoom.exits.find((candidate) =>
    candidate.to_area_id !== null &&
    candidate.to_room_number === next.roomNumber &&
    candidate.to_area_id === nextArea.id
  );
  return exit
    ? {
      areaId: previous.areaId,
      exit: {
        room: previous.roomNumber,
        direction: exit.from_direction,
        connectionKey: connectionKey(exit.connection_id),
      },
    }
    : undefined;
}

function recordLocation(location: MapLocation): void {
  const previous = ownMapSnapshot();
  rememberLocation(location);
  sessionMap.set(advanceTrack(previous, location, traversedExit(previous?.location, location)));
  if (mounted) refreshMapStyles();
}

function connectionKey(id: ConnectionId): string {
  return id;
}

/** Style every endpoint ref the renderer can use, including a one-way link's
 * synthetic endpoint where no stored traversal originates. */
function expandExitRefs(area: Area, refs: readonly MapExitRef[]): MapExitRef[] {
  const traversals = area.room_numbers.flatMap((roomNumber) =>
    (area.room(roomNumber)?.exits ?? []).map((exit) => ({
      connectionKey: connectionKey(exit.connection_id),
      room: roomNumber,
      direction: exit.from_direction,
      toRoom: exit.to_room_number,
      toDirection: exit.to_direction,
    }))
  );
  const connections = area.connections.map((connection) => ({
    key: connectionKey(connection.id),
    endpointA: {
      room: connection.endpoint_a.room_number,
      side: connection.endpoint_a.side,
    },
    endpointB: connection.endpoint_b
      ? {
        room: connection.endpoint_b.room_number,
        side: connection.endpoint_b.side,
      }
      : null,
  }));
  return expandTrackedExitRefs(refs, traversals, connections);
}

function unvisitedApplication(area: Area): MapStyleApplication | undefined {
  const visited = loadVisitedRooms();
  const isVisited = (areaId: string, roomNumber: number): boolean =>
    (visited[areaId] ?? []).includes(roomNumber);
  const rooms = area.room_numbers.filter((roomNumber) => !isVisited(area.id, roomNumber));
  const exits: MapExitRef[] = [];

  for (const roomNumber of area.room_numbers) {
    const sourceVisited = isVisited(area.id, roomNumber);
    for (const exit of area.room(roomNumber)?.exits ?? []) {
      let destinationVisited = true;
      if (exit.to_area_id !== null && exit.to_room_number !== null) {
        const destinationArea = mapper.areas.find((candidate) => candidate.id === exit.to_area_id);
        if (destinationArea) {
          destinationVisited = isVisited(destinationArea.id, exit.to_room_number);
        }
      }
      if (!sourceVisited || !destinationVisited) {
        exits.push({ room: roomNumber, direction: exit.from_direction });
      }
    }
  }

  return rooms.length > 0 || exits.length > 0
    ? { style: UNVISITED_STYLE, area: area.id, rooms, exits }
    : undefined;
}

function peerApplications(area: Area, currentRoom: number): MapStyleApplication[] {
  return getSessions()
    .filter((target) => target.id !== session.id && target.connected)
    .flatMap((target, index) => {
      const location = directedMaps.get(target.id)?.view.value?.location;
      return location?.areaId === area.id &&
          location.roomNumber !== null &&
          location.roomNumber !== currentRoom
        ? [{
          style: `other-player-${index}`,
          area: area.id,
          rooms: [location.roomNumber],
        }]
        : [];
    });
}

function gpsApplications(current: Room, area: Area): MapStyleApplication[] {
  const routeRaw = gpsRouteRaw(latestGps);
  if (!latestGps?.active || !routeRaw) return [];
  try {
    return mapViewRoute(
      current as RouteRoom,
      routeRaw,
      (areaId, roomNumber) =>
        mapper.getAreaById(areaId).room(roomNumber) as RouteRoom | undefined,
    ).map((application) => ({
      ...application,
      area: area.id,
      exits: expandExitRefs(area, application.exits ?? []),
    }));
  } catch {
    // The mapper can be between area snapshots while movement GMCP arrives.
    // Its subsequent map:room event retries against the settled topology.
    return [];
  }
}

function refreshMapStyles(): void {
  const current = currentMappedRoom();
  if (!current) {
    mapApply.set([]);
    return;
  }
  try {
    const area = mapper.getAreaById(current.area_id);
    const applications: MapStyleApplication[] = [];
    const unvisited = unvisitedApplication(area);
    if (unvisited) applications.push(unvisited);

    if (initialized && mapControls.value.tracksVisible) {
      const tracks = trackApplication(area.id, ownMapSnapshot());
      if (tracks) {
        applications.push({
          ...tracks,
          exits: expandExitRefs(area, tracks.exits ?? []),
        });
      }
    }
    applications.push(...peerApplications(area, current.room_number));
    applications.push(...currentRoomMapViewApply(current as RouteRoom));
    applications.push(...gpsApplications(current, area));
    mapApply.set(applications);
  } catch {
    mapApply.set([]);
  }
}

function updateGps(gps: Readonly<CharGps> | undefined): void {
  latestGps = gps;
  if (gps?.active) {
    gpsView.set({
      line: `→ ${gps.destination} · ${gps.steps} steps · next: ${gps.next || "?"}`,
      color: UI.gold,
    });
  } else {
    gpsView.set({ line: "no route set", color: UI.faint });
  }
  refreshMapStyles();
}

watchMessage("Char.GPS", updateGps);
// State watches are write-triggered rather than replaying retained state.
// Seed the GPS strip and route accent immediately when scripts reload after
// Char.GPS arrived, so a stationary player still sees the active route.
updateGps(nukefire.value?.Char?.GPS);

mapRoomChanged.on(({ areaId, roomNumber }) => recordLocation({ areaId, roomNumber }));

function syncDirectedMaps(): void {
  const sessions = getSessions();
  const liveIds = new Set(sessions.map((target) => target.id));
  for (const [sessionId, directed] of directedMaps) {
    if (liveIds.has(sessionId)) continue;
    directed.subscription.off();
    directedMaps.delete(sessionId);
  }
  for (const target of sessions) {
    if (target.id === session.id || directedMaps.has(target.id)) continue;
    const view = directedMapConsumer.from(target);
    const subscription = view.watch(() => {
      if (mounted) refreshMapStyles();
    });
    directedMaps.set(target.id, { view, subscription });
  }
  if (mounted) refreshMapStyles();
}

function releaseDirectedMaps(): void {
  for (const directed of directedMaps.values()) directed.subscription.off();
  directedMaps.clear();
}

for (const lifecycle of [
  sessionCreated,
  sessionConnected,
  sessionDisconnected,
  sessionDestroyed,
]) {
  lifecycle.on(() => {
    if (mounted) setTimeout(syncDirectedMaps, 0);
  });
}

export function initialize(): void {
  if (initialized) return;
  initialized = true;
  const location = currentMapLocation();
  if (location) recordLocation(location);
  else sessionMap.set(ownMapSnapshot() ?? { tracks: {} });
}

function mount(): void {
  createWidget(
    "nf-map",
    <Column width="fill" height="fill" padding={6} spacing={6}>
      <Row spacing={8}>
        <Text size={widgetTextSize(14)} color={UI.bright}>
          {nukefire.bind("Room.Info.name", { fallback: "NukeFire" })}
        </Text>
        <Space width="fill" />
        <Text size={widgetTextSize(11)} color={UI.dim}>
          {nukefire.bind("Room.Info.area", { fallback: "" })}
        </Text>
      </Row>
      <MapView
        defaultStyle={{
          crossAreaLabelVisibility: "hover",
          crossAreaLabelBackground: getSettings().palette?.background,
          
        }}
        styles={{
          [CURRENT_ROOM_STYLE]: {
            crossAreaLabelVisibility: "always",
          },
          [UNVISITED_STYLE]: {
            roomOpacity: UNVISITED_OPACITY,
            connectionOpacity: UNVISITED_OPACITY,
          },
          [TRACK_STYLE]: {
            roomOpacity: 1,
            roomStroke: "#6fa6c9",
            roomStrokeWidth: 1.5,
            connectionOpacity: 1,
            connectionColor: "#6fa6c9",
            connectionWidth: 1.5,
          },
          ...Object.fromEntries(PLAYER_COLORS.map((color, index) => [
            `other-player-${index}`,
            {
              roomOpacity: 1,
              roomStroke: color,
              roomStrokeWidth: 3,
            },
          ])),
          [ROUTE_STYLE]: {
            roomOpacity: 1,
            connectionColor: UI.gold,
            connectionWidth: 2,
            connectionOpacity: 1,
            roomStroke: UI.gold,
            roomStrokeWidth: 2,
            crossAreaLabelVisibility: "always",
          },
        }}
        apply={mapApply.bind()}
      />
      {showLayoutState &&
        <Container width="fill" background={UI.card}>
          <Column width="fill" padding={6} spacing={3}>
            <Row spacing={8}>
              <Text size={widgetTextSize(10)} color={UI.gold}>LAYOUT</Text>
              <Text size={widgetTextSize(10)} color={UI.text}>
                {layoutState.bind("context.areaName", { fallback: "" })}
              </Text>
              <Text size={widgetTextSize(10)} color={UI.bright}>
                {layoutState.bind("status", { fallback: "idle" })}
              </Text>
              <Text size={widgetTextSize(10)} color={UI.dim}>
                {layoutState.bind("phase", { fallback: "idle" })}
              </Text>
              <Space width="fill" />
              <Text size={widgetTextSize(10)} color={UI.dim}>
                {layoutState.bind("terminalReason", { fallback: "" })}
              </Text>
              <Text size={widgetTextSize(10)} color={UI.dim}>
                {layoutState.bind("elapsedMs", { fallback: 0 })} ms
              </Text>
            </Row>
            <Row spacing={10}>
              <Text size={widgetTextSize(9)} color={UI.text}>
                layouts {layoutState.bind("work.layoutsConsidered", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                compactions {layoutState.bind("work.compactionAttempts", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                restarts {layoutState.bind("work.restarts", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                checks {layoutState.bind("work.feasibilityChecks", { fallback: 0 })}
              </Text>
            </Row>
            <Row spacing={10}>
              <Text size={widgetTextSize(9)} color={UI.text}>
                crossing-pairs {layoutState.bind("work.crossingsConsidered", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                macros {layoutState.bind("work.macrosConsidered", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                pushes {layoutState.bind("work.pushClosures", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                depth {layoutState.bind("work.maxDepth", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                states {layoutState.bind("work.visitedStates", { fallback: 0 })}
              </Text>
            </Row>
            <Row spacing={10}>
              <Text size={widgetTextSize(9)} color={UI.dim}>WORKER</Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                inspected {layoutDiagnostics.bind("inspectedStates", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                peak-live {layoutDiagnostics.bind("peakLiveSearchNodes", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                materialized {layoutDiagnostics.bind("candidateMaterializations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                {layoutDiagnostics.bind("workerDisposition", { fallback: "" })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.dim}>
                heap {layoutDiagnostics.bind("memory.peak.heapUsed", { fallback: 0 })} B
              </Text>
              <Text size={widgetTextSize(9)} color={UI.dim}>
                rss {layoutDiagnostics.bind("memory.peak.rss", { fallback: 0 })} B
              </Text>
            </Row>
            <Row spacing={10}>
              <Text size={widgetTextSize(9)} color={UI.dim}>CURRENT</Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                rays {layoutState.bind("currentQuality.cardinalRayViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                reciprocal {layoutState.bind("currentQuality.reciprocalRayViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                routes {layoutState.bind("currentQuality.routingViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                rooms {layoutState.bind("currentQuality.roomObstructions", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                ports {layoutState.bind("currentQuality.exitPortViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                2way-ports {layoutState.bind("currentQuality.reciprocalExitPortViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.text}>
                crossings {layoutState.bind("currentQuality.linkCrossings", { fallback: 0 })}
              </Text>
            </Row>
            <Row spacing={10}>
              <Text size={widgetTextSize(9)} color={UI.gold}>BEST</Text>
              <Text size={widgetTextSize(9)} color={UI.bright}>
                rays {layoutState.bind("bestQuality.cardinalRayViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.bright}>
                reciprocal {layoutState.bind("bestQuality.reciprocalRayViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.bright}>
                routes {layoutState.bind("bestQuality.routingViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.bright}>
                rooms {layoutState.bind("bestQuality.roomObstructions", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.bright}>
                ports {layoutState.bind("bestQuality.exitPortViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.bright}>
                2way-ports {layoutState.bind("bestQuality.reciprocalExitPortViolations", { fallback: 0 })}
              </Text>
              <Text size={widgetTextSize(9)} color={UI.bright}>
                crossings {layoutState.bind("bestQuality.linkCrossings", { fallback: 0 })}
              </Text>
            </Row>
          </Column>
        </Container>}
      <Scrollable width="fill" height={mapControls.bind("menuHeight")} direction="vertical">
        <Container width="fill" background={UI.card}>
          <Column width="fill" padding={6} spacing={3}>
            <Checkbox
              checked={mapControls.bind("tracksVisible")}
              text_size={widgetTextSize(10)}
              onToggle={(checked) => {
                mapControls.value.tracksVisible = checked;
                refreshMapStyles();
              }}
            >show my tracks</Checkbox>
            <Button
              width="fill"
              variant="subtle"
              onPress={() => {
                sessionMap.set(clearTracks(ownMapSnapshot()));
                refreshMapStyles();
              }}
            >
              <Text size={widgetTextSize(10)} color={UI.dim}>clear my tracks</Text>
            </Button>
            <Container width="fill" height={1} background={UI.cardEdge}><Space height={1} /></Container>
            <Button
              width="fill"
              variant="subtle"
              onPress={() => {
                const location = ownMapSnapshot()?.location ?? currentMapLocation();
                if (location) {
                  saveVisitedRooms(markAreaUnvisited(loadVisitedRooms(), location.areaId));
                  refreshMapStyles();
                }
              }}
            >
              <Text size={widgetTextSize(10)} color={UI.warning}>
                mark all rooms unvisited in this area
              </Text>
            </Button>
          </Column>
        </Container>
      </Scrollable>
      <Row spacing={8}>
        <Text size={widgetTextSize(11)} color={UI.gold}>GPS</Text>
        <Text size={widgetTextSize(11)} color={gpsView.bind("color")}>{gpsView.bind("line")}</Text>
        <Space width="fill" />
        <Button variant="subtle" onPress={() => send(GPS_CLEAR)}>
          <Text size={widgetTextSize(10)} color={UI.dim}>clear</Text>
        </Button>
        <Button
          variant="subtle"
          onPress={() => {
            mapControls.value.menuHeight = mapControls.value.menuHeight === 0
              ? MAP_MENU_HEIGHT
              : 0;
          }}
        >
          <Text size={widgetTextSize(10)} color={UI.dim}>view ▾</Text>
        </Button>
      </Row>
    </Column>,
    { pane: PANE },
  );
}

export function open(): void {
  session.mainPane.split("right", { name: PANE, width: 400, terminal: false });
  mounted = true;
  syncDirectedMaps();
  refreshMapStyles();
  mount();
}

export function close(): void {
  mounted = false;
  releaseDirectedMaps();
  session.panes.get(PANE)?.close();
}
