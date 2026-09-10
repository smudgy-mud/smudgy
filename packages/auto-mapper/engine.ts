// Injectable auto-mapper engine: protocol-driven mapping over normalized room observations.
// (docs/gmcp-mapping.md section 5.3). Known rooms are followed; unknown rooms are
// auto-created, one durable local area per server-reported zone. A saved map from this
// session or any earlier one (matched by name) is adopted and mapped into directly;
// `savemap cloud` remains available to move locally auto-mapped zones into cloud storage.
//
// Room identity is the server's own room id, bound as each room's externalId and resolved
// through mapper.findRoomByExternalId (O(1)). Ids are opaque strings end to end: hash ids
// and MSDP's stringly-typed vnums both work unchanged.
//
// Two placement/linking signals, in authority order:
//  1. The server's own exit destination ids (arrivalDirection) — never contradicted.
//  2. The player's observed movement commands (sys:send), the fallback every mapper in
//     the ecosystem runs on when exits carry no destination ids. Only recognized
//     direction tokens are kept; every other outgoing command is discarded unread.

import gmcp from "smudgy:state/gmcp";
import msdp from "smudgy:state/msdp";
import { mapper, createAlias, echo, gmcp as gmcpCtl } from "smudgy:core";
import { send as sysSend, connect as sysConnect, disconnect as sysDisconnect } from "smudgy:events/sys";
import { closed as gmcpClosed } from "smudgy:events/gmcp";
import {
    planAreaChange,
    type AreaChangePlan,
    type LayoutDirection,
} from "smudgy://kapusniak/map-layout";

// ---------------------------------------------------------------------------------------
// The normalized room fix every dialect reduces to.
// ---------------------------------------------------------------------------------------

export interface DoorFix {
    closed: boolean;
    locked: boolean;
}

export interface RoomFix {
    /** Server-global room id, or null when the server withheld identity (mazes, -1). */
    id: string | null;
    name: string;
    zone: string | null;
    terrain: string | null;
    /** Direction (canonical long form) or special-exit command -> destination id
     *  (null when the id was withheld). */
    exits: Record<string, string | null>;
    /** Optional authoritative door state by canonical exit direction. */
    doors?: Record<string, DoorFix>;
    coords: { x: number; y: number; z: number } | null;
    /** True when this room must never be drawn or edited (identity withheld, or a
     *  continent grid). A known unmappable room is still FOLLOWED. */
    unmappable: boolean;
}

export interface NeighborhoodRoomFix {
    id: string;
    name?: string;
    terrain?: string | null;
    /** Position relative to the neighborhood's center room, in map grid units. */
    offset: { x: number; y: number; z: number };
    exits: Record<string, string | null>;
}

export interface NeighborhoodFix {
    /** The room at offset (0, 0, 0), used to anchor relative server coordinates. */
    centerId: string;
    rooms: NeighborhoodRoomFix[];
}

export interface AutoMapperOptions {
    /** Normalize the game's Room.Info payload. Defaults to the generic IRE/Aardwolf adapter. */
    roomInfo?: (info: unknown) => RoomFix | null;
    /** Normalize an optional server-provided neighborhood such as RoP's Room.Map. */
    roomMap?: (map: unknown) => NeighborhoodFix | null;
    /** Enable the generic MSDP ROOM adapters. Defaults to true. */
    msdp?: boolean;
    /** Use outgoing direction commands only when the server cannot identify an arrival. */
    inferMovementFromCommands?: boolean;
    /** Ask the server to enable the GMCP Room module. Defaults to true. */
    enableRoomModule?: boolean;
}

export interface AutoMapper {
    start(): void;
    /** Move every bound session/local zone (or one named zone) into cloud storage. */
    upgradeToCloud(zone?: string): void;
}

// Map-unit spacing between adjacent rooms (both for server grids and walk inference).
// The map's grid pitch is one unit per room (Viewport::GRID_UNIT); anything larger
// draws auto-maps at double density relative to hand-built maps.
const GRID = 1.0;
// Collision nudges tried along the movement vector before giving up and stacking.
const MAX_NUDGES = 50;
const PROPOSED_ROOM_ID = "$auto-mapper:new-room";
const SERVER_COORDINATES_PROPERTY = "auto-mapper.server-coordinates";

// Adapter input bounds (docs/gmcp-mapping.md section 7): a hostile or buggy server must
// not mint unbounded ids/names or NaN-adjacent geometry. Over-limit ids read as withheld
// identity (truncating could collide two distinct rooms); over-limit coords fall back to
// walk inference; names are truncated.
const MAX_ID_LENGTH = 128;
const MAX_TITLE_LENGTH = 256;
const MAX_ZONE_LENGTH = 128;
const MAX_EXIT_COMMAND_LENGTH = 64;
const MAX_EXITS_PER_ROOM = 64;
const MAX_COORD = 100_000;
const MAX_LEVEL_COORD = 1_000;

// Movement observation: queued direction commands awaiting attribution to a room change.
const MOVE_QUEUE_MAX = 32;
const MOVE_STALE_MS = 15_000;

const DIRECTIONS: Record<string, string> = {
    n: "north", e: "east", s: "south", w: "west", u: "up", d: "down",
    ne: "northeast", nw: "northwest", se: "southeast", sw: "southwest",
    north: "north", east: "east", south: "south", west: "west", up: "up", down: "down",
    northeast: "northeast", northwest: "northwest", southeast: "southeast",
    southwest: "southwest", in: "in", out: "out",
};

const EXIT_DIRECTION: Record<string, ExitDirection> = {
    north: "North", east: "East", south: "South", west: "West", up: "Up", down: "Down",
    northeast: "Northeast", northwest: "Northwest", southeast: "Southeast",
    southwest: "Southwest", in: "In", out: "Out",
};

const REVERSE: Record<string, string> = {
    north: "south", south: "north", east: "west", west: "east",
    northeast: "southwest", southwest: "northeast",
    northwest: "southeast", southeast: "northwest",
    up: "down", down: "up", in: "out", out: "in",
};

const OFFSETS: Record<string, [number, number, number]> = {
    north: [0, -1, 0], south: [0, 1, 0], east: [1, 0, 0], west: [-1, 0, 0],
    northeast: [1, -1, 0], northwest: [-1, -1, 0], southeast: [1, 1, 0], southwest: [-1, 1, 0],
    up: [0, 0, 1], down: [0, 0, -1], in: [1, 1, 0], out: [-1, -1, 0],
};

// Unvisited placeholder rooms wear a neutral dark wash until their first real visit.
const PLACEHOLDER_COLOR = "#4a4a52";

// A light terrain -> room color wash so auto-maps read at a glance (MudForge parity).
const TERRAIN_COLORS: Record<string, string> = {
    city: "#8a8a8a", inside: "#b0a27a", road: "#b09a6a", field: "#7aa85a",
    forest: "#3a7a3a", hills: "#8a7a4a", mountain: "#7a6a5a", water: "#4a7aba",
    water_deep: "#244c96", underwater: "#172e66", river: "#4a7aba", ocean: "#2a5a9a",
    air: "#96dcff", desert: "#c0a860", snow: "#dcefff", tropical: "#42a85a",
    ice: "#b4e6ff", marsh: "#527844", underground: "#5a5a6a",
    inside_dark: "#502828", mana: "#b432dc",
};

export function canonicalDir(raw: string): string | null {
    return DIRECTIONS[raw.toLowerCase()] ?? null;
}

export function asId(value: unknown): string | null {
    if (typeof value === "number" && Number.isFinite(value)) return String(value);
    if (typeof value === "string" && value.length > 0 && value.length <= MAX_ID_LENGTH) {
        return value;
    }
    return null;
}

export function asNumber(value: unknown): number | null {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string") {
        const parsed = Number(value);
        if (Number.isFinite(parsed)) return parsed;
    }
    return null;
}

export function asTitle(value: unknown): string {
    return String(value ?? "").slice(0, MAX_TITLE_LENGTH);
}

export function asZone(value: unknown): string | null {
    return typeof value === "string" ? value.slice(0, MAX_ZONE_LENGTH) : null;
}

export function boundedCoords(x: number, y: number, z: number): RoomFix["coords"] {
    if (Math.abs(x) > MAX_COORD || Math.abs(y) > MAX_COORD || Math.abs(z) > MAX_LEVEL_COORD) {
        return null;
    }
    return { x, y, z };
}

/** Exit table -> canonical map. Compass keys normalize to the long direction; any other
 *  key is a special exit (a portal, "enter grate", "clockwise"), kept under its command
 *  and traversed by sending that command verbatim. Destination sentinels "-1" and "0"
 *  (the "exists but unidentified" spellings in the wild — Aardwolf mazes, the IRE id-0
 *  case Mudlet's mmp refuses) read as withheld ids. */
function adaptExits(raw: unknown): Record<string, string | null> {
    const exits: Record<string, string | null> = {};
    if (!raw || typeof raw !== "object") return exits;
    let count = 0;
    for (const [rawDir, dest] of Object.entries(raw as Record<string, unknown>)) {
        if (count >= MAX_EXITS_PER_ROOM) break;
        const dir = canonicalDir(rawDir) ?? rawDir.trim().slice(0, MAX_EXIT_COMMAND_LENGTH);
        if (!dir) continue;
        const destId = asId(dest);
        exits[dir] = destId === "-1" || destId === "0" ? null : destId;
        count += 1;
    }
    return exits;
}

// ---------------------------------------------------------------------------------------
// Dialect adapters (docs/gmcp-mapping.md sections 4/5.3).
// ---------------------------------------------------------------------------------------

/** GMCP Room.Info / room.info: the IRE and Aardwolf dialects plus a tolerant generic. */
function adaptGmcp(info: unknown): RoomFix | null {
    if (info === null || typeof info !== "object") return null;
    const fields = info as Record<string, unknown>;

    const id = asId(fields.num) ?? asId(fields.vnum) ?? asId(fields.id);
    // Aardwolf's explicit "don't map here" sentinel.
    const unmappable = id === null || id === "-1";

    const exits = adaptExits(fields.exits);
    const name = asTitle(fields.name);
    const zone = asZone(fields.zone) ?? asZone(fields.area);
    const terrain = typeof fields.terrain === "string" ? fields.terrain
        : typeof fields.environment === "string" ? fields.environment : null;

    let coords: RoomFix["coords"] = null;
    const coord = fields.coord as Record<string, unknown> | undefined;
    if (coord && typeof coord === "object") {
        // Aardwolf: { id, x, y, cont } is the ZONE's position on its continent map, not a
        // per-room coordinate — adjacent zone rooms all carry the same x/y (verified against
        // the golden's capture: 35200 and 35201 both say x:30,y:20). Never place by it;
        // zone rooms use walk inference. Its one load-bearing signal is cont == 1: the room
        // IS on a continent grid (where coord is per-room). Continent rooms are follow-only
        // (unmappable, but identity kept so a room from an imported overland map still
        // tracks) — creating rooms across a 1000x1000 grid belongs to a future grid regime.
        if (asNumber(coord.cont) === 1) {
            return {
                id: unmappable ? null : id, name, zone, terrain,
                exits, coords: null, unmappable: true,
            };
        }
    } else if (typeof fields.coords === "string") {
        // IRE: "area,x,y[,level]" — the 4th slot matches the level baked into the
        // Room.Info `map` image URL, so multi-floor areas separate by z instead of
        // stacking on one plane.
        const parts = fields.coords.split(",");
        const x = asNumber(parts[1]);
        const y = asNumber(parts[2]);
        const z = asNumber(parts[3]) ?? 0;
        if (x !== null && y !== null) coords = boundedCoords(x, y, z);
    }

    return {
        id: unmappable ? null : id,
        name,
        zone,
        terrain,
        exits,
        coords,
        unmappable,
    };
}

/** MSDP composite ROOM table (the Luminari shape) or the flat ROOM_* variables. */
function adaptMsdp(room: unknown): RoomFix | null {
    if (room === null || typeof room !== "object") return null;
    const fields = room as Record<string, unknown>;
    const id = asId(fields.VNUM);

    const exits = adaptExits(fields.EXITS);

    let coords: RoomFix["coords"] = null;
    const c = fields.COORDS as Record<string, unknown> | undefined;
    if (c && typeof c === "object") {
        const x = asNumber(c.X);
        const y = asNumber(c.Y);
        const z = asNumber(c.Z) ?? 0;
        // All-zero coords are Luminari's "no meaningful position" for zone rooms;
        // walk inference places those better than stacking everything at origin.
        if (x !== null && y !== null && (x !== 0 || y !== 0 || z !== 0)) {
            coords = boundedCoords(x, y, z);
        }
    }

    return {
        id,
        name: asTitle(fields.NAME),
        zone: asZone(fields.AREA),
        terrain: typeof fields.TERRAIN === "string" ? fields.TERRAIN : null,
        exits,
        coords,
        unmappable: id === null,
    };
}

function adaptMsdpFlat(): RoomFix | null {
    const v = msdp.value;
    if (!v) return null;
    const id = asId(v.ROOM_VNUM);
    if (id === null) return null;
    return {
        id,
        name: asTitle(v.ROOM_NAME),
        zone: asZone(v.AREA_NAME),
        terrain: typeof v.ROOM_TERRAIN === "string" ? v.ROOM_TERRAIN : null,
        exits: adaptExits(v.ROOM_EXITS),
        coords: null,
        unmappable: false,
    };
}

// ---------------------------------------------------------------------------------------
// Mapping state (session-scoped, like the areas it manages).
// ---------------------------------------------------------------------------------------

export function createAutoMapper(options: AutoMapperOptions = {}): AutoMapper {
const roomInfoAdapter = options.roomInfo ?? adaptGmcp;
const roomMapAdapter = options.roomMap;
const includeMsdp = options.msdp ?? true;
const inferMovement = options.inferMovementFromCommands ?? true;
const enableRoomModule = options.enableRoomModule ?? true;

// IMPORTANT: an `Area` handle wraps an immutable snapshot taken when the handle was
// minted — `area.room(n)` / `area.room_numbers` on a cached handle never see later
// writes. All session state therefore holds `AreaId`s and re-fetches a fresh handle
// (`mapper.getAreaById`) at every read.

/** Zone name (folded) -> the area collecting that zone's rooms. */
const zoneAreas = new Map<string, AreaId>();
/** Areas that refused a write (read-only shares, deleted mid-session): never re-adopt. */
const rejectedAreas: AreaId[] = [];
/** UNVISITED destination id -> the exits pointing at it: links to its placeholder room,
 *  or dangling stubs when a placeholder could not be minted. Re-targeted when the room
 *  is truly visited (materialization may rebuild it in a different zone's area). */
const pendingLinks = new Map<string, { areaId: AreaId; room: RoomNumber; exitId: ExitId; dir: string }[]>();
/** The room the character was in before the current fix. */
let lastRoom: { areaId: AreaId; room: RoomNumber; fix: RoomFix } | null = null;
/** Direction commands sent to the game, awaiting attribution to a room change. */
const moveQueue: { dir: string; at: number }[] = [];
/** Opt-in (`mapprune on`): remove compass exits the server stops reporting. */
let pruneStale = false;
/** Areas we already told the player we cannot update (one notice each). */
const warnedAreas = new Set<string>();
/** Serializes async handling so a fast walk can't interleave two creations.
 *
 *  Seeded on the maps-loaded barrier (`importAreasIfAbsent([])` waits for the session's
 *  initial area load and imports nothing): a fix processed against the pre-load atlas
 *  cannot resolve saved rooms, so a zone mapped last run would be redrawn from scratch —
 *  and an area created mid-load can be dropped by the wholesale reload. If the load
 *  itself fails, mapping proceeds against whatever is resident (the pre-barrier
 *  behavior). */
let queue: Promise<void> = Promise.resolve();
/** Luminari sends both the composite ROOM and the flat variables; composite wins. */
let sawCompositeRoom = false;
/** Neighborhoods received before their center room exists, latest update per center wins. */
const pendingNeighborhoods = new Map<string, NeighborhoodFix>();
/** The newest not-yet-started Room.Map task, for adjacent same-center coalescing. */
let coalescibleNeighborhood: { fix: NeighborhoodFix } | null = null;
let started = false;

const FALLBACK_ZONE = "Uncharted";

/** The zone's identity fold — also applied to AREA NAMES when adopting existing maps,
 *  so the fallback must fold to its own key. */
function zoneKey(zone: string | null): string {
    return (zone ?? FALLBACK_ZONE).trim().toLowerCase() || FALLBACK_ZONE.toLowerCase();
}

/** Ids are canonical UUID strings, so `===` is the whole comparison. */
function sameArea(a: AreaId, b: AreaId): boolean {
    return a === b;
}

function sameExit(a: ExitId, b: ExitId): boolean {
    return a === b;
}

async function zoneArea(zone: string | null): Promise<AreaId> {
    const key = zoneKey(zone);
    const bound = zoneAreas.get(key);
    if (bound) return bound;
    // Adopt an existing durable map of this zone before minting a fresh one. Matching is
    // by folded name — renaming a map is how a player detaches it from auto-mapping.
    // A legacy session map can survive a script-engine rebuild, so promote it immediately
    // rather than continuing to write ephemeral map data.
    let sessionArea: AreaId | undefined;
    for (const area of mapper.areas) {
        if (zoneKey(area.name) !== key) continue;
        if (rejectedAreas.some((rejected) => sameArea(rejected, area.id))) continue;
        if (area.storage !== "session") {
            zoneAreas.set(key, area.id);
            return area.id;
        }
        sessionArea ??= area.id;
    }
    if (sessionArea) {
        const [promoted] = await mapper.moveAreas([sessionArea], { storage: "local" });
        if (!promoted) throw new Error("could not promote the legacy session map");
        zoneAreas.set(key, promoted.id);
        return promoted.id;
    }
    const area = await mapper.createArea((zone ?? FALLBACK_ZONE).trim() || FALLBACK_ZONE, {
        storage: "local",
    });
    zoneAreas.set(key, area.id);
    return area.id;
}

// ---------------------------------------------------------------------------------------
// Movement observation (sys:send) — the placement/linking fallback for dialects whose
// exit tables carry no destination ids.
// ---------------------------------------------------------------------------------------

function observeSend({ command }: { command: string }) {
    // Recognized direction tokens only; anything else is dropped unread. Masked
    // (password) sends never reach sys:send at all.
    const dir = DIRECTIONS[command.trim().toLowerCase()];
    if (!dir) return;
    moveQueue.push({ dir, at: Date.now() });
    if (moveQueue.length > MOVE_QUEUE_MAX) moveQueue.shift();
}

/** The one movement this fix accounts for. Server exit ids are authoritative when they
 *  identify the arrival direction (the queue is then only kept aligned); otherwise the
 *  oldest fresh queued command is consumed — but only when the room actually changed,
 *  so a `look` re-report does not eat a queued step.
 *
 *  `fixAt` is when the fix ARRIVED, not when it is being processed: handling can lag
 *  arrival (the maps-loaded barrier, a burst of slow creations), and a command sent
 *  after a fix arrived cannot have caused it. */
function takeMovement(fix: RoomFix, idDir: string | null, fixAt: number): string | null {
    while (moveQueue.length > 0 && fixAt - moveQueue[0].at > MOVE_STALE_MS) moveQueue.shift();
    let eligibleCount = 0;
    while (eligibleCount < moveQueue.length && moveQueue[eligibleCount].at <= fixAt) {
        eligibleCount += 1;
    }
    if (idDir) {
        // The server's destination ids settle the whole outstanding prefix. Commands before
        // the matching one did not produce room changes (closed door, invalid direction);
        // retaining them would poison a later arrival.
        const matching = moveQueue
            .slice(0, eligibleCount)
            .findIndex((entry) => entry.dir === idDir);
        moveQueue.splice(0, matching >= 0 ? matching + 1 : eligibleCount);
        return idDir;
    }
    if (!inferMovement || eligibleCount === 0) return null;
    const moved = !lastRoom || fix.id === null || lastRoom.fix.id !== fix.id;
    if (!moved) {
        // Some games acknowledge a refused move by re-reporting the current room.
        moveQueue.shift();
        return null;
    }
    if (eligibleCount !== 1) {
        // More than one unacknowledged command could have caused this room report. Refuse
        // to invent an exit; authoritative room ids/coordinates can still place and follow it.
        moveQueue.splice(0, eligibleCount);
        return null;
    }
    return moveQueue.shift()!.dir;
}

/** The direction walked into `fix`, from the PREVIOUS room's exit table: the exit whose
 *  destination id is the new room's id. Server-authoritative. */
function arrivalDirection(fix: RoomFix): string | null {
    if (!lastRoom || fix.id === null) return null;
    for (const [dir, dest] of Object.entries(lastRoom.fix.exits)) {
        if (dest !== null && dest === fix.id) return dir;
    }
    return null;
}

// A failed movement produces no Room.Info; the explicit refusal drops the queued step
// so it cannot be attributed to the next real room change.
function movementFailed(value: unknown) {
    const dir = typeof value === "string" ? canonicalDir(value) : null;
    const index = dir ? moveQueue.findIndex((entry) => entry.dir === dir) : 0;
    if (index >= 0 && moveQueue.length > 0) moveQueue.splice(index, 1);
}

// Walk-inference state is connection-scoped: after a disconnect or a GMCP teardown the
// player can be anywhere, and placing the next room relative to pre-disconnect state
// draws fiction.
function resetWalkState() {
    lastRoom = null;
    moveQueue.length = 0;
}
// ---------------------------------------------------------------------------------------
// Placement.
// ---------------------------------------------------------------------------------------

function propertyIsTrue(value: string | null | undefined): boolean {
    return value?.trim().toLowerCase() === "true";
}

/** Respect map-layout's generic manual locks and keep authoritative server grids fixed. */
function isLayoutRoomMovable(room: Room): boolean {
    return !room.hasTag("LAYOUT_LOCKED")
        && !propertyIsTrue(room.data("layoutLocked"))
        && !propertyIsTrue(room.data(SERVER_COORDINATES_PROPERTY));
}

function layoutDirection(dir: string): LayoutDirection | null {
    return (EXIT_DIRECTION[dir] as LayoutDirection | undefined) ?? null;
}

async function applyLayoutMoves(
    areaId: AreaId,
    result: AreaChangePlan,
    mutation?: AreaMutator,
): Promise<void> {
    const updates: [RoomNumber, UpdateRoomParams][] = result.patch.moves.map((move) => {
        if (move.roomNumber === undefined) {
            throw new Error(`layout move ${move.id} has no Smudgy room number`);
        }
        return [move.roomNumber, {
            x: move.to.x,
            y: move.to.y,
            level: move.to.level,
        }];
    });
    if (updates.length === 0) return;
    if (mutation) await mutation.updateRooms(updates);
    else await mapper.updateRooms(areaId, updates);
}

async function planRoomAddition(
    areaId: AreaId,
    fromRoom: RoomNumber,
    dir: string,
): Promise<{ result: AreaChangePlan; position: { x: number; y: number; level: number } } | null> {
    const direction = layoutDirection(dir);
    if (!direction) return null;
    const result = await planAreaChange(areaId, {
        type: "add-room",
        from: fromRoom,
        direction,
        temporaryId: PROPOSED_ROOM_ID,
        createReturnEdge: false,
    }, { isRoomMovable: isLayoutRoomMovable });
    const placement = result.patch.placements.find((candidate) => candidate.id === PROPOSED_ROOM_ID);
    if (!placement) throw new Error("map-layout did not place the new auto-mapper room");
    return { result, position: placement.position };
}

/** Reflow movement-placed rooms before adding a newly discovered same-area connection. */
async function planRoomConnection(
    areaId: AreaId,
    fromRoom: RoomNumber,
    toRoom: RoomNumber,
    dir: string,
    mutation?: AreaMutator,
): Promise<void> {
    if (fromRoom === toRoom) return;
    const direction = layoutDirection(dir);
    if (!direction) return;
    const result = await planAreaChange(areaId, {
        type: "connect-rooms",
        from: fromRoom,
        to: toRoom,
        direction,
        createReturnEdge: false,
    }, { isRoomMovable: isLayoutRoomMovable });
    await applyLayoutMoves(areaId, result, mutation);
}

function occupied(area: Area, x: number, y: number, level: number): boolean {
    for (const number of area.room_numbers) {
        const room = area.room(number);
        if (room && room.level === level && Math.abs(room.x - x) < 0.5 && Math.abs(room.y - y) < 0.5) {
            return true;
        }
    }
    return false;
}

/** Placement: server coords when present, else previous room + the movement vector, with
 *  nudging on collision (docs/gmcp-mapping.md section 5.3). Reads go through a FRESH
 *  area handle — a cached one is a stale snapshot. */
function placement(areaId: AreaId, fix: RoomFix, direction: string | null): { x: number; y: number; level: number } {
    const area = mapper.getAreaById(areaId);
    if (fix.coords) {
        // Trust the server grid, but never stack two rooms on one cell — a duplicate
        // report nudges east like the walk path does.
        return {
            x: fix.coords.x * GRID,
            y: fix.coords.y * GRID,
            level: Math.round(fix.coords.z),
        };
    }
    const prev = lastRoom && sameArea(lastRoom.areaId, areaId) ? area.room(lastRoom.room) : null;
    const from = prev ? { x: prev.x, y: prev.y, level: prev.level } : { x: 0, y: 0, level: 0 };
    const [dx, dy, dz] = direction ? (OFFSETS[direction] ?? [1, 1, 0]) : lastRoom ? [1, 1, 0] : [0, 0, 0];
    let x = from.x + dx * GRID;
    let y = from.y + dy * GRID;
    const level = from.level + dz;
    for (let nudge = 0; nudge < MAX_NUDGES && occupied(area, x, y, level); nudge += 1) {
        x += (dx || 1) * GRID;
        y += dy * GRID;
    }
    return { x, y, level };
}

// ---------------------------------------------------------------------------------------
// Exits and links.
// ---------------------------------------------------------------------------------------

/** The room's exit in canonical direction (or special command) `dir`. */
function exitFor(room: Room, dir: string): Exit | undefined {
    const from_direction = EXIT_DIRECTION[dir] ?? "Special";
    return room.exits.find(
        (exit) =>
            exit.from_direction === from_direction &&
            (from_direction !== "Special" || (exit.command ?? "") === dir),
    );
}

function exitIsPending(exitId: ExitId): boolean {
    for (const waiters of pendingLinks.values()) {
        if (waiters.some((waiter) => sameExit(waiter.exitId, exitId))) return true;
    }
    return false;
}

function dropPendingExit(exitId: ExitId) {
    for (const [id, waiters] of [...pendingLinks]) {
        const kept = waiters.filter((waiter) => !sameExit(waiter.exitId, exitId));
        if (kept.length === 0) pendingLinks.delete(id);
        else if (kept.length !== waiters.length) pendingLinks.set(id, kept);
    }
}

/** Point an existing exit at a destination by DELETE + RECREATE, preserving its fields.
 *  Only a freshly CREATED exit runs the host's reciprocal auto-pair, which folds it onto
 *  the one-member Connection of an opposing exit between the same two rooms — that is
 *  what collapses a back-and-forth walk into one two-way link. An in-place
 *  `setRoomExit` update never re-pairs and leaves two parallel one-way Connections. */
async function relinkExit(
    areaId: AreaId,
    room: RoomNumber,
    exit: Exit,
    toAreaId: AreaId,
    toRoomNumber: RoomNumber,
    mutation?: AreaMutator,
): Promise<ExitId> {
    if (mutation) await mutation.deleteRoomExit(room, exit.id);
    else await mapper.deleteRoomExit(areaId, room, exit.id);
    const fields: ExitArgs = {
        from_direction: exit.from_direction,
        to_area_id: toAreaId,
        to_room_number: toRoomNumber,
        is_hidden: exit.is_hidden,
        is_closed: exit.is_closed,
        is_locked: exit.is_locked,
        weight: exit.weight,
        command: exit.command ?? undefined,
    };
    return mutation
        ? await mutation.createRoomExit(room, fields)
        : await mapper.createRoomExit(areaId, room, fields);
}

function trackPending(destId: string, areaId: AreaId, room: RoomNumber, exitId: ExitId, dir: string) {
    const waiters = pendingLinks.get(destId) ?? [];
    if (!waiters.some((waiter) => sameExit(waiter.exitId, exitId))) {
        waiters.push({ areaId, room, exitId, dir });
        pendingLinks.set(destId, waiters);
    }
}

type PendingLinkEffect =
    | { type: "drop"; exitId: ExitId }
    | {
        type: "track";
        destId: string;
        areaId: AreaId;
        room: RoomNumber;
        exitId: ExitId;
        dir: string;
    };

function recordPendingDrop(effects: PendingLinkEffect[] | undefined, exitId: ExitId) {
    if (effects) effects.push({ type: "drop", exitId });
    else dropPendingExit(exitId);
}

function recordPendingTrack(
    effects: PendingLinkEffect[] | undefined,
    destId: string,
    areaId: AreaId,
    room: RoomNumber,
    exitId: ExitId,
    dir: string,
) {
    if (effects) effects.push({ type: "track", destId, areaId, room, exitId, dir });
    else trackPending(destId, areaId, room, exitId, dir);
}

function applyPendingEffects(effects: PendingLinkEffect[]) {
    for (const effect of effects) {
        if (effect.type === "drop") dropPendingExit(effect.exitId);
        else {
            trackPending(
                effect.destId,
                effect.areaId,
                effect.room,
                effect.exitId,
                effect.dir,
            );
        }
    }
}

function isUnvisited(room: Room): boolean {
    return room.data("unvisited") === "true";
}

/** Mint an unvisited placeholder for a server-named neighbor, offset one step from the
 *  room that advertised it — every room the server has named is on the map before it is
 *  walked (the Mudlet pattern: mmp's makeroom-for-every-exit / hashonly placeholders).
 *  A placeholder carries only its identity (externalId), position guess, the `unvisited`
 *  marker, and a neutral wash; the first real visit materializes it. Returns undefined
 *  on failure so the caller can fall back to a dangling stub. */
async function createPlaceholder(
    fromAreaId: AreaId,
    fromRoom: RoomNumber,
    dir: string,
    destId: string,
): Promise<Room | undefined> {
    try {
        const area = mapper.getAreaById(fromAreaId);
        const from = area.room(fromRoom);
        if (!from) return undefined;
        const planned = await planRoomAddition(fromAreaId, fromRoom, dir);
        const fallback = (() => {
            const [dx, dy, dz] = OFFSETS[dir] ?? [1, 1, 0];
            let x = from.x + dx * GRID;
            let y = from.y + dy * GRID;
            const level = from.level + dz;
            for (let nudge = 0; nudge < MAX_NUDGES && occupied(area, x, y, level); nudge += 1) {
                x += (dx || 1) * GRID;
                y += dy * GRID;
            }
            return { x, y, level };
        })();
        const at = planned?.position ?? fallback;
        let number!: RoomNumber;
        await mapper.mutateArea(fromAreaId, async (mutation) => {
            if (planned) await applyLayoutMoves(fromAreaId, planned.result, mutation);
            number = await mutation.createRoom({
                externalId: destId,
                x: at.x,
                y: at.y,
                level: at.level,
                color: PLACEHOLDER_COLOR,
            });
            await mutation.setRoomProperty(number, "unvisited", "true");
        }, { description: "Create GMCP map placeholder" });
        return mapper.getAreaById(fromAreaId).room(number);
    } catch {
        return undefined;
    }
}

async function linkOrStub(
    areaId: AreaId,
    room: RoomNumber,
    dir: string,
    destId: string | null,
    door?: DoorFix,
    mutation?: AreaMutator,
    pendingEffects?: PendingLinkEffect[],
) {
    const from_direction = EXIT_DIRECTION[dir] ?? "Special";
    const command = dir;
    if (destId !== null) {
        // A named destination is always a room on the map: the mapped one, or a fresh
        // unvisited placeholder.
        let dest = mapper.findRoomByExternalId(destId);
        const alreadyMapped = dest !== undefined;
        if (!dest && mutation) {
            // A callback-scoped mutator cannot safely open a nested room-creation batch.
            // Abort this topology batch and let its caller retry the link individually.
            throw new Error(`destination ${destId} is unavailable for batched linking`);
        }
        dest ??= await createPlaceholder(areaId, room, dir, destId);
        if (dest) {
            if (alreadyMapped && sameArea(areaId, dest.area_id)) {
                await planRoomConnection(areaId, room, dest.room_number, dir, mutation);
            }
            const fields: ExitArgs = {
                from_direction,
                to_area_id: dest.area_id,
                to_room_number: dest.room_number,
                is_closed: door?.closed,
                is_locked: door?.locked,
                command,
                weight: 1,
            };
            const exitId = mutation
                ? await mutation.createRoomExit(room, fields)
                : await mapper.createRoomExit(areaId, room, fields);
            // Exits into unvisited rooms stay tracked: materialization may rebuild the
            // room in a different zone's area, and every tracked exit follows it there.
            if (isUnvisited(dest)) {
                recordPendingTrack(pendingEffects, destId, areaId, room, exitId, dir);
            }
            return;
        }
    }
    // No identity (or the placeholder could not be minted): a dangling stub until the
    // far room is discovered.
    const fields: ExitArgs = {
        from_direction,
        is_closed: door?.closed,
        is_locked: door?.locked,
        command,
        weight: 1,
    };
    const exitId = mutation
        ? await mutation.createRoomExit(room, fields)
        : await mapper.createRoomExit(areaId, room, fields);
    if (destId !== null) {
        recordPendingTrack(pendingEffects, destId, areaId, room, exitId, dir);
    }
}

/** Re-target every tracked exit waiting on `id` to `(areaId, room)`. A tracked exit is
 *  re-pointed when it is still dangling OR still points at `replacing` (the placeholder
 *  a relocation is retiring); an exit the user re-aimed by hand is left alone. Each
 *  waiter is independent: one unreachable exit (its room or area deleted mid-session)
 *  must not abort the rest, or poison the fix that discovered the room. */
async function resolvePending(
    id: string,
    areaId: AreaId,
    room: RoomNumber,
    replacing?: { areaId: AreaId; room: RoomNumber },
) {
    const waiters = pendingLinks.get(id);
    if (!waiters) return;
    pendingLinks.delete(id);
    for (const waiter of waiters) {
        try {
            const owner = mapper.getAreaById(waiter.areaId).room(waiter.room);
            const exit = owner?.exits.find((candidate) => sameExit(candidate.id, waiter.exitId));
            if (!exit) continue;
            const pointsAtReplaced = replacing !== undefined
                && exit.to_area_id !== null
                && exit.to_room_number !== null
                && sameArea(exit.to_area_id, replacing.areaId)
                && exit.to_room_number === replacing.room;
            if (exit.to_room_number !== null && !pointsAtReplaced) continue;
            if (sameArea(waiter.areaId, areaId)) {
                await planRoomConnection(waiter.areaId, waiter.room, room, waiter.dir);
            }
            await relinkExit(waiter.areaId, waiter.room, exit, areaId, room);
        } catch {
            // Skip the casualty; the remaining waiters still deserve their links.
        }
    }
}

/** Movement-evidence linking, for exits that carry no destination ids: the observed
 *  command proves `prev --dir--> dest`. Forward: upgrade prev's dangling stub, or add
 *  the exit the walk just proved. Reverse: consume dest's dangling opposite-direction
 *  stub if it advertised one — generic_mapper's one-shot stub rule; the recreation
 *  auto-pairs both traversals onto one Connection. Ids stay authoritative: stubs whose
 *  destination id is known (pending) are never overridden by inference. */
async function reconcileTraversal(
    prev: { areaId: AreaId; room: RoomNumber },
    dir: string,
    toAreaId: AreaId,
    toRoomNumber: RoomNumber,
    topologyPlanned = false,
) {
    if (sameArea(prev.areaId, toAreaId) && prev.room === toRoomNumber) return;
    const prevRoom = mapper.getAreaById(prev.areaId).room(prev.room);
    if (!prevRoom) return;
    const existing = exitFor(prevRoom, dir);
    if (existing) {
        if (existing.to_room_number === null && !exitIsPending(existing.id)) {
            if (!topologyPlanned && sameArea(prev.areaId, toAreaId)) {
                await planRoomConnection(prev.areaId, prev.room, toRoomNumber, dir);
            }
            await relinkExit(prev.areaId, prev.room, existing, toAreaId, toRoomNumber);
        }
    } else {
        if (!topologyPlanned && sameArea(prev.areaId, toAreaId)) {
            await planRoomConnection(prev.areaId, prev.room, toRoomNumber, dir);
        }
        await mapper.createRoomExit(prev.areaId, prev.room, {
            from_direction: EXIT_DIRECTION[dir] ?? "Special",
            to_area_id: toAreaId,
            to_room_number: toRoomNumber,
            command: dir,
            weight: 1,
        });
    }
    const reverse = REVERSE[dir];
    if (!reverse) return;
    const destRoom = mapper.getAreaById(toAreaId).room(toRoomNumber);
    const reverseExit = destRoom ? exitFor(destRoom, reverse) : undefined;
    if (reverseExit && reverseExit.to_room_number === null && !exitIsPending(reverseExit.id)) {
        await relinkExit(toAreaId, toRoomNumber, reverseExit, prev.areaId, prev.room);
    }
}

/** Reconcile one server-reported exit. A destination id outranks any link inferred from
 * outgoing commands, including a previously linked but different room. */
async function reconcileReportedExit(
    areaId: AreaId,
    roomNumber: RoomNumber,
    dir: string,
    destId: string | null,
    door?: DoorFix,
    mutation?: AreaMutator,
    planTopology = true,
    pendingEffects?: PendingLinkEffect[],
) {
    const owner = mapper.getAreaById(areaId).room(roomNumber);
    const existing = owner ? exitFor(owner, dir) : undefined;
    if (!existing) {
        await linkOrStub(
            areaId,
            roomNumber,
            dir,
            destId,
            door,
            mutation,
            pendingEffects,
        );
        return;
    }

    if (destId === null) {
        if (door && (existing.is_closed !== door.closed || existing.is_locked !== door.locked)) {
            const fields: ExitUpdates = {
                is_closed: door.closed,
                is_locked: door.locked,
            };
            if (mutation) await mutation.setRoomExit(roomNumber, existing.id, fields);
            else await mapper.setRoomExit(areaId, roomNumber, existing.id, fields);
        }
        return;
    }

    let destination = mapper.findRoomByExternalId(destId);
    const alreadyMapped = destination !== undefined;
    if (!destination && mutation) {
        throw new Error(`destination ${destId} is unavailable for batched reconciliation`);
    }
    destination ??= await createPlaceholder(areaId, roomNumber, dir, destId);
    if (!destination) {
        if (!exitIsPending(existing.id)) {
            trackPending(destId, areaId, roomNumber, existing.id, dir);
        }
        return;
    }

    const alreadyLinked = existing.to_area_id !== null
        && existing.to_room_number !== null
        && sameArea(existing.to_area_id, destination.area_id)
        && existing.to_room_number === destination.room_number;
    let exitId = existing.id;
    if (!alreadyLinked) {
        if (planTopology && alreadyMapped && sameArea(areaId, destination.area_id)) {
            await planRoomConnection(
                areaId,
                roomNumber,
                destination.room_number,
                dir,
                mutation,
            );
        }
        recordPendingDrop(pendingEffects, existing.id);
        exitId = await relinkExit(
            areaId,
            roomNumber,
            existing,
            destination.area_id,
            destination.room_number,
            mutation,
        );
    }
    if (door && (existing.is_closed !== door.closed || existing.is_locked !== door.locked)) {
        const fields: ExitUpdates = {
            is_closed: door.closed,
            is_locked: door.locked,
        };
        if (mutation) await mutation.setRoomExit(roomNumber, exitId, fields);
        else await mapper.setRoomExit(areaId, roomNumber, exitId, fields);
    }
    if (isUnvisited(destination)) {
        recordPendingTrack(
            pendingEffects,
            destId,
            areaId,
            roomNumber,
            exitId,
            dir,
        );
    }
}

/** Revisit reconciliation (the mmp pattern): every fix for a known room re-syncs it —
 *  title and terrain refresh, newly advertised exits appear (linked when their
 *  destination is known, stubs otherwise), destination ids that finally resolve
 *  upgrade, and lost pending registrations re-arm (module state does not survive a
 *  script-engine rebuild; the map does). Opt-in `mapprune`: compass exits the server
 *  stopped reporting are removed. */
async function reconcileKnownRoom(room: Room, fix: RoomFix) {
    const areaId = room.area_id;
    const roomNumber = room.room_number;
    await mapper.mutateArea(areaId, async (mutation) => {
        if (fix.name && fix.name !== room.title) {
            await mutation.updateRoom(roomNumber, { title: fix.name });
        }
        if (fix.coords) {
            const at = placement(areaId, fix, null);
            if (room.x !== at.x || room.y !== at.y || room.level !== at.level) {
                await mutation.updateRoom(roomNumber, at);
            }
            if (!propertyIsTrue(room.data(SERVER_COORDINATES_PROPERTY))) {
                await mutation.setRoomProperty(roomNumber, SERVER_COORDINATES_PROPERTY, "true");
            }
        }
        if (fix.terrain && room.data("terrain") !== fix.terrain) {
            await mutation.setRoomProperty(roomNumber, "terrain", fix.terrain);
            const color = TERRAIN_COLORS[fix.terrain.toLowerCase()];
            if (color) await mutation.setRoomColor(roomNumber, color);
        }
    }, { description: "Refresh GMCP room metadata" });
    for (const [dir, dest] of Object.entries(fix.exits)) {
        await reconcileReportedExit(areaId, roomNumber, dir, dest, fix.doors?.[dir]);
    }
    if (pruneStale) {
        for (const exit of room.exits) {
            if (exit.from_direction === "Special" || exit.from_direction === "Other") continue;
            const dir = exit.from_direction.toLowerCase();
            if (!(dir in fix.exits)) {
                await mapper.deleteRoomExit(areaId, roomNumber, exit.id);
                dropPendingExit(exit.id);
            }
        }
    }
}

/** First real visit to a placeholder: give it its real face. In place when the zone
 *  guess was right (server coords may re-place it); otherwise the room is REBUILT in
 *  the correct zone's area — every tracked inbound exit re-targets (and re-pairs), the
 *  placeholder is deleted (the host repairs anything untracked into a dangling stub),
 *  and the external id moves to the rebuilt room. Title, terrain, and exits arrive via
 *  the revisit reconciliation that follows either way. */
async function materialize(room: Room, fix: RoomFix, dir: string | null): Promise<Room> {
    const id = fix.id!;
    const target = await zoneArea(fix.zone);
    if (sameArea(target, room.area_id)) {
        await mapper.mutateArea(room.area_id, async (mutation) => {
            if (fix.coords) {
                const at = placement(room.area_id, fix, null);
                await mutation.updateRoom(room.room_number, { x: at.x, y: at.y, level: at.level });
                await mutation.setRoomProperty(
                    room.room_number,
                    SERVER_COORDINATES_PROPERTY,
                    "true",
                );
            }
            await mutation.setRoomProperty(room.room_number, "unvisited", "");
        }, { description: "Materialize GMCP map placeholder" });
        // Tracked exits already point at this room; they need no re-targeting.
        pendingLinks.delete(id);
        return mapper.findRoomByExternalId(id) ?? room;
    }
    const at = placement(target, fix, dir);
    let rebuilt!: RoomNumber;
    await mapper.mutateArea(target, async (mutation) => {
        rebuilt = await mutation.createRoom({
            title: fix.name,
            x: at.x,
            y: at.y,
            level: at.level,
        });
        if (fix.coords) {
            await mutation.setRoomProperty(rebuilt, SERVER_COORDINATES_PROPERTY, "true");
        }
    }, { description: "Relocate GMCP map placeholder" });
    await resolvePending(id, target, rebuilt, { areaId: room.area_id, room: room.room_number });
    await mapper.deleteRoom(room.area_id, room.room_number);
    await mapper.setRoomExternalId(target, rebuilt, id);
    return mapper.getAreaById(target).room(rebuilt) ?? room;
}

function warnUnwritable(areaId: AreaId, err: unknown) {
    const key = areaId;
    if (warnedAreas.has(key)) return;
    warnedAreas.add(key);
    echo(`[auto-mapper] cannot update this zone's map (${err}) - following only.`);
}

// ---------------------------------------------------------------------------------------
// The fix pipeline.
// ---------------------------------------------------------------------------------------

async function autoCreate(fix: RoomFix, placementDir: string | null, movementDir: string | null): Promise<void> {
    if (fix.id === null) return;
    let areaId = await zoneArea(fix.zone);

    let room: RoomNumber | null = null;
    let topologyPlanned = false;
    for (let attempt = 0; attempt < 2 && room === null; attempt += 1) {
        try {
            const source = !fix.coords && placementDir && lastRoom && sameArea(lastRoom.areaId, areaId)
                ? mapper.getAreaById(areaId).room(lastRoom.room)
                : null;
            const planned = source && placementDir
                ? await planRoomAddition(areaId, source.room_number, placementDir)
                : null;
            const at = planned?.position ?? placement(areaId, fix, placementDir);
            const params: CreateRoomParams = {
                title: fix.name,
                externalId: fix.id,
                x: at.x,
                y: at.y,
                level: at.level,
            };
            const color = fix.terrain ? TERRAIN_COLORS[fix.terrain.toLowerCase()] : undefined;
            if (color) params.color = color;
            await mapper.mutateArea(areaId, async (mutation) => {
                if (planned) await applyLayoutMoves(areaId, planned.result, mutation);
                room = await mutation.createRoom(params);
                if (fix.terrain) {
                    await mutation.setRoomProperty(room, "terrain", fix.terrain);
                }
                if (fix.coords) {
                    await mutation.setRoomProperty(room, SERVER_COORDINATES_PROPERTY, "true");
                }
            }, { description: "Create GMCP map room" });
            topologyPlanned = planned !== null;
        } catch (err) {
            // The draft number resolves before submission, so a failed submit
            // leaves `room` pointing at a room that never existed; clear it or
            // the retry is skipped and links/current-location bind the phantom.
            room = null;
            if (attempt === 1) throw err;
            // The bound map refused the write: an adopted map we cannot write (a
            // read-only share), or an area deleted mid-session. Detach it for good and
            // fall back to a fresh local area so mapping continues durably.
            rejectedAreas.push(areaId);
            zoneAreas.delete(zoneKey(fix.zone));
            echo(`[auto-mapper] the map for zone "${(fix.zone ?? FALLBACK_ZONE).trim() || FALLBACK_ZONE}" is not writable - continuing in a new local map.`);
            areaId = await zoneArea(fix.zone);
        }
    }
    if (room === null) return;
    // Every exit of the new room: a real link when the destination is mapped, a fresh
    // unvisited placeholder when only its id is known, a dangling stub when even the id
    // was withheld (docs/gmcp-mapping.md section 5.3). The exit we arrived THROUGH
    // needs no special case: the previous room already links here (via this room's
    // placeholder or a tracked stub), and resolvePending below re-targets it — creating
    // it again would double the edge.
    for (const [dir, dest] of Object.entries(fix.exits)) {
        await linkOrStub(areaId, room, dir, dest, fix.doors?.[dir]);
    }

    await resolvePending(fix.id, areaId, room);
    // When the walk was attributed by an observed command rather than exit ids, the
    // command is the only evidence connecting the previous room to this one.
    if (movementDir && lastRoom) {
        await reconcileTraversal(lastRoom, movementDir, areaId, room, topologyPlanned);
    }
    mapper.setCurrentLocation(areaId, room);
    lastRoom = { areaId, room, fix };
}

/** Materialize every room in a server-provided neighborhood without treating any of them
 * as visited. Relative server coordinates outrank earlier movement-based placement. */
async function handleNeighborhood(fix: NeighborhoodFix | null): Promise<void> {
    if (!fix) return;
    const center = mapper.findRoomByExternalId(fix.centerId);
    if (!center) {
        pendingNeighborhoods.set(fix.centerId, fix);
        return;
    }
    pendingNeighborhoods.delete(fix.centerId);

    const areaId = center.area_id;
    const centerX = center.x;
    const centerY = center.y;
    const centerLevel = center.level;
    const missingBefore = new Set(
        fix.rooms
            .filter((reported) => !mapper.findRoomByExternalId(reported.id))
            .map((reported) => reported.id),
    );
    const drafted = new Set<string>();
    const offAreaTerrain = new Map<string, {
        areaId: AreaId;
        roomNumber: RoomNumber;
        terrain: string;
        color?: string;
    }[]>();
    try {
        await mapper.mutateArea(areaId, async (mutation) => {
            for (const reported of fix.rooms) {
                const x = centerX + reported.offset.x * GRID;
                const y = centerY + reported.offset.y * GRID;
                const level = centerLevel + Math.round(reported.offset.z);
                const room = mapper.findRoomByExternalId(reported.id);
                if (room && !sameArea(room.area_id, areaId)) {
                    if (isUnvisited(room) && reported.terrain) {
                        const key = room.area_id;
                        const group = offAreaTerrain.get(key) ?? [];
                        group.push({
                            areaId: room.area_id,
                            roomNumber: room.room_number,
                            terrain: reported.terrain,
                            color: TERRAIN_COLORS[reported.terrain.toLowerCase()],
                        });
                        offAreaTerrain.set(key, group);
                    }
                    continue;
                }

                if (!room) {
                    if (drafted.has(reported.id)) continue;
                    drafted.add(reported.id);
                    const params: CreateRoomParams = {
                        externalId: reported.id,
                        title: reported.name ?? "",
                        x,
                        y,
                        level,
                        color: reported.terrain
                            ? TERRAIN_COLORS[reported.terrain.toLowerCase()]
                            : PLACEHOLDER_COLOR,
                    };
                    const roomNumber = await mutation.createRoom(params);
                    await mutation.setRoomProperty(roomNumber, "unvisited", "true");
                    await mutation.setRoomProperty(
                        roomNumber,
                        SERVER_COORDINATES_PROPERTY,
                        "true",
                    );
                    if (reported.terrain) {
                        await mutation.setRoomProperty(roomNumber, "terrain", reported.terrain);
                    }
                    continue;
                }

                if (room.level === level && (room.x !== x || room.y !== y)) {
                    await mutation.updateRoom(room.room_number, { x, y });
                }
                if (!propertyIsTrue(room.data(SERVER_COORDINATES_PROPERTY))) {
                    await mutation.setRoomProperty(
                        room.room_number,
                        SERVER_COORDINATES_PROPERTY,
                        "true",
                    );
                }
                if (isUnvisited(room) && reported.terrain) {
                    await mutation.setRoomProperty(room.room_number, "terrain", reported.terrain);
                    const color = TERRAIN_COLORS[reported.terrain.toLowerCase()];
                    if (color) await mutation.setRoomColor(room.room_number, color);
                }
            }
        }, { description: "Apply GMCP neighborhood rooms" });
    } catch (err) {
        warnUnwritable(areaId, err);
    }
    for (const group of offAreaTerrain.values()) {
        try {
            await mapper.mutateArea(group[0].areaId, async (mutation) => {
                for (const update of group) {
                    await mutation.setRoomProperty(update.roomNumber, "terrain", update.terrain);
                    if (update.color) await mutation.setRoomColor(update.roomNumber, update.color);
                }
            }, { description: "Refresh GMCP neighborhood terrain" });
        } catch (err) {
            warnUnwritable(group[0].areaId, err);
        }
    }

    // Resolve only identities that were absent before this update. Re-query the
    // host after acknowledgement so oversized best-effort batches are handled
    // correctly even if a later envelope failed.
    for (const id of missingBefore) {
        const room = mapper.findRoomByExternalId(id);
        if (room) {
            await resolvePending(id, room.area_id, room.room_number);
        }
    }

    // All rooms now exist, so same-update exits can link directly instead of becoming
    // stubs. Group topology by owning area: the host applies reciprocal CreateExit
    // operations sequentially inside one envelope, so they auto-pair onto one Connection
    // without one durable acknowledgement per advertised direction. A failed batch is
    // retried link-by-link against the refreshed host snapshot, preserving the generic
    // mapper's unusual/cross-area fallback behavior.
    const exitGroups = new Map<string, {
        areaId: AreaId;
        work: { roomNumber: RoomNumber; dir: string; dest: string | null }[];
    }>();
    for (const reported of fix.rooms) {
        const room = mapper.findRoomByExternalId(reported.id);
        if (!room) continue;
        for (const [dir, dest] of Object.entries(reported.exits)) {
            const key = room.area_id;
            const group = exitGroups.get(key) ?? { areaId: room.area_id, work: [] };
            group.work.push({ roomNumber: room.room_number, dir, dest });
            exitGroups.set(key, group);
        }
    }
    for (const group of exitGroups.values()) {
        const pendingEffects: PendingLinkEffect[] = [];
        try {
            await mapper.mutateArea(group.areaId, async (mutation) => {
                for (const item of group.work) {
                    // Room.Map coordinates are authoritative and all affected rooms were
                    // locked above, so per-edge map-layout planning would only repeat the
                    // same no-move analysis. The room batch has already placed them.
                    await reconcileReportedExit(
                        group.areaId,
                        item.roomNumber,
                        item.dir,
                        item.dest,
                        undefined,
                        mutation,
                        false,
                        pendingEffects,
                    );
                }
            }, { description: "Apply GMCP neighborhood exits" });
            applyPendingEffects(pendingEffects);
        } catch {
            for (const item of group.work) {
                try {
                    await reconcileReportedExit(
                        group.areaId,
                        item.roomNumber,
                        item.dir,
                        item.dest,
                    );
                } catch (err) {
                    warnUnwritable(group.areaId, err);
                }
            }
        }
    }
}

async function handleRoomFix(fix: RoomFix | null, fixAt: number): Promise<void> {
    if (!fix) return;
    const idDir = arrivalDirection(fix);
    const dir = takeMovement(fix, idDir, fixAt);
    const known = fix.id !== null ? mapper.findRoomByExternalId(fix.id) : undefined;
    if (known) {
        // Following is identity-only, so it works for unmappable rooms too: a continent
        // room from an imported overland map still tracks position. Drawing/editing is
        // what unmappable forbids.
        let room = known;
        if (!fix.unmappable && isUnvisited(known)) {
            // First real visit to a placeholder neighbor.
            try {
                room = await materialize(known, fix, dir);
            } catch (err) {
                warnUnwritable(known.area_id, err);
            }
        }
        mapper.setCurrentLocation(room.area_id, room.room_number);
        const from = lastRoom;
        lastRoom = { areaId: room.area_id, room: room.room_number, fix };
        if (!fix.unmappable) {
            try {
                await reconcileKnownRoom(room, fix);
                if (dir && !idDir && from && from.fix.id !== fix.id) {
                    await reconcileTraversal(from, dir, room.area_id, room.room_number);
                }
            } catch (err) {
                warnUnwritable(room.area_id, err);
            }
        }
        return;
    }
    if (fix.unmappable || fix.id === null) {
        // The server withheld identity (maze, -1) or forbade drawing: never guess.
        lastRoom = null;
        return;
    }
    // Before drawing new terrain, check whether this room is already mapped for
    // a different server. If so, the player is offered to show that map here
    // too; drawing a duplicate would produce a second copy of the same map (the
    // multiple-entries-per-game / lagging-sibling case). Defer to that offer.
    if (mapper.rescueRoomByExternalId(fix.id)) {
        lastRoom = null;
        return;
    }
    await autoCreate(fix, dir, idDir ? null : dir);
}

async function handleFix(fix: RoomFix | null, fixAt: number): Promise<void> {
    await handleRoomFix(fix, fixAt);
    if (fix?.id) {
        const pending = pendingNeighborhoods.get(fix.id);
        if (pending) await handleNeighborhood(pending);
    }
}

function appendQueue(task: () => Promise<void>) {
    queue = queue.then(task).catch((err) => {
        echo(`[auto-mapper] ${err}`);
    });
}

function enqueue(task: () => Promise<void>) {
    // Do not coalesce Room.Map snapshots across an intervening Room.Info, storage move,
    // or other ordered mapper task: arrival order can carry movement meaning.
    coalescibleNeighborhood = null;
    appendQueue(task);
}

function enqueueNeighborhood(fix: NeighborhoodFix | null) {
    if (!fix) return;
    if (coalescibleNeighborhood?.fix.centerId === fix.centerId) {
        // Same center and no intervening task: only the newest unprocessed geometry is
        // useful (door/overlay churn can republish the same neighborhood repeatedly).
        coalescibleNeighborhood.fix = fix;
        return;
    }
    const slot = { fix };
    coalescibleNeighborhood = slot;
    appendQueue(async () => {
        if (coalescibleNeighborhood === slot) coalescibleNeighborhood = null;
        await handleNeighborhood(slot.fix);
    });
}

// ---------------------------------------------------------------------------------------
// Wire-up.
// ---------------------------------------------------------------------------------------

// ---------------------------------------------------------------------------------------
// savemap: retain the legacy session-map promotion path and allow durable local maps to
// move to cloud storage. Runs THROUGH the fix queue: relocating an area out from under an
// in-flight room creation would race it.
// ---------------------------------------------------------------------------------------

async function doSavemap(storage: "local" | "cloud", zone: string | undefined) {
    const filter = zone ? zoneKey(zone) : null;
    // Local is already the auto-mapper's durable default. A cloud request can move local
    // zones, while the local form exists only to rescue a legacy session map.
    const chosen = [...zoneAreas.entries()].filter(([key, areaId]) => {
        if (filter !== null && key !== filter) return false;
        try {
            const current = mapper.getAreaById(areaId).storage;
            return current === "session" || (storage === "cloud" && current === "local");
        } catch {
            return false;
        }
    });
    if (chosen.length === 0) {
        echo(filter === null
            ? `[auto-mapper] all mapped zones are already saved${storage === "cloud" ? " to cloud" : " locally"}.`
            : `[auto-mapper] "${zone}" is already saved${storage === "cloud" ? " to cloud" : " locally"}.`);
        return;
    }
    const moved = await mapper.moveAreas(
        chosen.map(([, areaId]) => areaId),
        { storage },
    );
    const destinationIds = moved.map((area) => area.id);
    // Rebind so mapping continues seamlessly into the acknowledged durable copies.
    for (const [index, [key]] of chosen.entries()) {
        const destinationId = destinationIds[index];
        if (destinationId) zoneAreas.set(key, destinationId);
    }
    // Re-key tracked exits into the relocated copies: the deleted originals took their
    // exit ids with them, but relocation preserves room numbers, so each waiter's exit is
    // recoverable in the promoted area by (room, direction) — whether it links to an
    // in-set placeholder (relocation remapped it) or went dangling. A waiter that cannot
    // be recovered is dropped rather than left pointing into a dead area.
    for (const [id, waiters] of [...pendingLinks]) {
        const remapped = waiters.flatMap((waiter) => {
            const index = chosen.findIndex(([, areaId]) => sameArea(areaId, waiter.areaId));
            if (index === -1) return [waiter];
            const destinationId = destinationIds[index];
            if (!destinationId) return [];
            const owner = mapper.getAreaById(destinationId).room(waiter.room);
            const exit = owner ? exitFor(owner, waiter.dir) : undefined;
            return exit
                ? [{ areaId: destinationId, room: waiter.room, exitId: exit.id, dir: waiter.dir }]
                : [];
        });
        if (remapped.length === 0) pendingLinks.delete(id);
        else pendingLinks.set(id, remapped);
    }
    if (lastRoom && chosen.some(([, areaId]) => sameArea(areaId, lastRoom!.areaId))) {
        lastRoom = null;
    }
    echo(`[auto-mapper] moved ${destinationIds.length} map(s) to ${storage}.`);
}

function start() {
    if (started) return;
    started = true;
    queue = mapper.importAreasIfAbsent([]).then(
        () => {},
        () => {},
    );

    echo("[auto-mapper] active - mapping structured room data into durable local maps.");

    // Fixes are timestamped at ARRIVAL (the watch delivery), not at processing: handling
    // may lag on the maps-loaded barrier or a creation burst.
    gmcp.watch("Room.Info", (info: unknown) => {
        const at = Date.now();
        enqueue(() => handleFix(roomInfoAdapter(info), at));
    });
    if (roomMapAdapter) {
        gmcp.watch("Room.Map", (map: unknown) => {
            enqueueNeighborhood(roomMapAdapter(map));
        });
    }
    if (includeMsdp) {
        msdp.watch("ROOM", (room: unknown) => {
            sawCompositeRoom = true;
            const at = Date.now();
            enqueue(() => handleFix(adaptMsdp(room), at));
        });
        msdp.watch("ROOM_VNUM", () => {
            if (!sawCompositeRoom) {
                const at = Date.now();
                enqueue(() => handleFix(adaptMsdpFlat(), at));
            }
        });
    }

    if (inferMovement) {
        sysSend.on(observeSend);
        gmcp.onWrite("Room.WrongDir", movementFailed);
    }
    sysConnect.on(resetWalkState);
    sysDisconnect.on(resetWalkState);
    gmcpClosed.on(resetWalkState);

    if (enableRoomModule) gmcpCtl.enableModule("Room");

    // The first word parses as a tier when it is `local` or `cloud`, shadowing
    // zones literally named that; such a zone is reachable through the
    // two-word form (`savemap local cloud`).
    createAlias(/^savemap(?:\s+(?<args>.+))?$/, (matches: { args?: string }) => {
        const args = matches.args?.trim();
        const tier = args?.match(/^(local|cloud)(?:\s+(.*))?$/i);
        const storage = (tier?.[1]?.toLowerCase() ?? "local") as "local" | "cloud";
        const zone = tier ? tier[2]?.trim() || undefined : args;
        enqueue(() => doSavemap(storage, zone));
    });
    createAlias(/^mapprune(?:\s+(?<state>on|off))?$/, (matches: { state?: string }) => {
        if (matches.state) pruneStale = matches.state === "on";
        echo(pruneStale
            ? "[auto-mapper] pruning ON - compass exits the server stops reporting are removed from revisited rooms."
            : "[auto-mapper] pruning OFF - exits are never removed automatically (mapprune on to enable).");
    });
}

function upgradeToCloud(zone?: string) {
    enqueue(() => doSavemap("cloud", zone?.trim() || undefined));
}

return { start, upgradeToCloud };
}
