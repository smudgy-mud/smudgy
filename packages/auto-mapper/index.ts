// auto-mapper: protocol-driven mapping over GMCP Room.Info and MSDP ROOM
// (docs/gmcp-mapping.md section 5.3). Known rooms are followed; unknown rooms are
// auto-created in session (ephemeral) areas, one per server-reported zone, so nothing a
// server sends can ever touch a saved map. `savemap` promotes the session areas to local
// maps and keeps mapping into them.
//
// Room identity is the server's own room id, bound as each room's externalId and resolved
// through mapper.findRoomByExternalId (O(1)). Ids are opaque strings end to end: hash ids
// and MSDP's stringly-typed vnums both work unchanged.

import gmcp from "smudgy:state/gmcp";
import msdp from "smudgy:state/msdp";
import { mapper, createAlias, echo, gmcp as gmcpCtl } from "smudgy:core";

// ---------------------------------------------------------------------------------------
// The normalized room fix every dialect reduces to.
// ---------------------------------------------------------------------------------------

interface RoomFix {
    /** Server-global room id, or null when the server withheld identity (mazes, -1). */
    id: string | null;
    name: string;
    zone: string | null;
    terrain: string | null;
    /** Canonical long direction -> destination id (null when the id was withheld). */
    exits: Record<string, string | null>;
    coords: { x: number; y: number; z: number } | null;
    /** True when this room must not be followed or created (identity withheld). */
    unmappable: boolean;
}

// Map-unit spacing between adjacent rooms (both for server grids and walk inference).
const GRID = 2.0;
// Collision nudges tried along the movement vector before giving up and stacking.
const MAX_NUDGES = 50;

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

const OFFSETS: Record<string, [number, number, number]> = {
    north: [0, -1, 0], south: [0, 1, 0], east: [1, 0, 0], west: [-1, 0, 0],
    northeast: [1, -1, 0], northwest: [-1, -1, 0], southeast: [1, 1, 0], southwest: [-1, 1, 0],
    up: [0, 0, 1], down: [0, 0, -1], in: [1, 1, 0], out: [-1, -1, 0],
};

// A light terrain -> room color wash so auto-maps read at a glance (MudForge parity).
const TERRAIN_COLORS: Record<string, string> = {
    city: "#8a8a8a", inside: "#b0a27a", road: "#b09a6a", field: "#7aa85a",
    forest: "#3a7a3a", hills: "#8a7a4a", mountain: "#7a6a5a", water: "#4a7aba",
    river: "#4a7aba", ocean: "#2a5a9a", desert: "#c0a860", underground: "#5a5a6a",
};

function canonicalDir(raw: string): string | null {
    return DIRECTIONS[raw.toLowerCase()] ?? null;
}

function asId(value: unknown): string | null {
    if (typeof value === "number" && Number.isFinite(value)) return String(value);
    if (typeof value === "string" && value.length > 0) return value;
    return null;
}

function asNumber(value: unknown): number | null {
    if (typeof value === "number" && Number.isFinite(value)) return value;
    if (typeof value === "string") {
        const parsed = Number(value);
        if (Number.isFinite(parsed)) return parsed;
    }
    return null;
}

// ---------------------------------------------------------------------------------------
// Dialect adapters (docs/gmcp-mapping.md sections 4/5.3).
// ---------------------------------------------------------------------------------------

/** GMCP Room.Info / room.info: the IRE and Aardwolf dialects plus a tolerant generic. */
function adaptGmcp(info: unknown): RoomFix | null {
    if (info === null || typeof info !== "object") return null;
    const fields = info as Record<string, unknown>;

    const id = asId(fields.num);
    // Aardwolf's explicit "don't map here" sentinel.
    const unmappable = id === null || id === "-1";

    const exits: Record<string, string | null> = {};
    if (fields.exits && typeof fields.exits === "object") {
        for (const [rawDir, dest] of Object.entries(fields.exits as Record<string, unknown>)) {
            const dir = canonicalDir(rawDir);
            if (dir) exits[dir] = asId(dest);
        }
    }

    let coords: RoomFix["coords"] = null;
    const coord = fields.coord as Record<string, unknown> | undefined;
    if (coord && typeof coord === "object") {
        // Aardwolf: { id, x, y, cont } is the ZONE's position on its continent map, not a
        // per-room coordinate — adjacent zone rooms all carry the same x/y (verified against
        // the golden's capture: 35200 and 35201 both say x:30,y:20). Never place by it;
        // zone rooms use walk inference. Its one load-bearing signal is cont == 1: the room
        // IS on a continent grid (where coord is per-room), which is follow-only — creating
        // rooms across a 1000x1000 overland grid belongs to a future grid regime.
        if (asNumber(coord.cont) === 1) {
            return {
                id: unmappable ? null : id, name: String(fields.name ?? ""),
                zone: typeof fields.zone === "string" ? fields.zone : null,
                terrain: typeof fields.terrain === "string" ? fields.terrain : null,
                exits, coords: null, unmappable: true,
            };
        }
    } else if (typeof fields.coords === "string") {
        // IRE: "area,x,y[,building]".
        const parts = fields.coords.split(",");
        const x = asNumber(parts[1]);
        const y = asNumber(parts[2]);
        if (x !== null && y !== null) coords = { x, y, z: 0 };
    }

    const zone = typeof fields.zone === "string" ? fields.zone
        : typeof fields.area === "string" ? fields.area : null;
    const terrain = typeof fields.terrain === "string" ? fields.terrain
        : typeof fields.environment === "string" ? fields.environment : null;

    return {
        id: unmappable ? null : id,
        name: String(fields.name ?? ""),
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

    const exits: Record<string, string | null> = {};
    if (fields.EXITS && typeof fields.EXITS === "object") {
        for (const [rawDir, dest] of Object.entries(fields.EXITS as Record<string, unknown>)) {
            const dir = canonicalDir(rawDir);
            if (dir) exits[dir] = asId(dest);
        }
    }

    let coords: RoomFix["coords"] = null;
    const c = fields.COORDS as Record<string, unknown> | undefined;
    if (c && typeof c === "object") {
        const x = asNumber(c.X);
        const y = asNumber(c.Y);
        const z = asNumber(c.Z) ?? 0;
        // All-zero coords are Luminari's "no meaningful position" for zone rooms;
        // walk inference places those better than stacking everything at origin.
        if (x !== null && y !== null && (x !== 0 || y !== 0 || z !== 0)) coords = { x, y, z };
    }

    return {
        id,
        name: String(fields.NAME ?? ""),
        zone: typeof fields.AREA === "string" ? fields.AREA : null,
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
    const exits: Record<string, string | null> = {};
    if (v.ROOM_EXITS && typeof v.ROOM_EXITS === "object") {
        for (const [rawDir, dest] of Object.entries(v.ROOM_EXITS as Record<string, unknown>)) {
            const dir = canonicalDir(rawDir);
            if (dir) exits[dir] = asId(dest);
        }
    }
    return {
        id,
        name: String(v.ROOM_NAME ?? ""),
        zone: typeof v.AREA_NAME === "string" ? v.AREA_NAME : null,
        terrain: typeof v.ROOM_TERRAIN === "string" ? v.ROOM_TERRAIN : null,
        exits,
        coords: null,
        unmappable: false,
    };
}

// ---------------------------------------------------------------------------------------
// Mapping state (session-scoped, like the areas it manages).
// ---------------------------------------------------------------------------------------

// IMPORTANT: an `Area` handle wraps an immutable snapshot taken when the handle was
// minted — `area.room(n)` / `area.room_numbers` on a cached handle never see later
// writes. All session state therefore holds `AreaId`s and re-fetches a fresh handle
// (`mapper.getAreaById`) at every read.

/** Zone name (folded) -> the area collecting that zone's rooms. */
const zoneAreas = new Map<string, AreaId>();
/** Unseen destination id -> stub exits waiting to be linked when it appears. */
const pendingLinks = new Map<string, { areaId: AreaId; room: RoomNumber; exitId: ExitId }[]>();
/** The room the character was in before the current fix. */
let lastRoom: { areaId: AreaId; room: RoomNumber; fix: RoomFix } | null = null;
/** Serializes async handling so a fast walk can't interleave two creations. */
let queue: Promise<void> = Promise.resolve();
/** Luminari sends both the composite ROOM and the flat variables; composite wins. */
let sawCompositeRoom = false;

const FALLBACK_ZONE = "Uncharted";

function zoneKey(zone: string | null): string {
    return (zone ?? FALLBACK_ZONE).trim().toLowerCase() || FALLBACK_ZONE;
}

/** AreaIds are opaque `[hi, lo]` pairs; compare by value, never by reference. */
function sameArea(a: AreaId, b: AreaId): boolean {
    return a[0] === b[0] && a[1] === b[1];
}

async function zoneArea(zone: string | null): Promise<AreaId> {
    const key = zoneKey(zone);
    let areaId = zoneAreas.get(key);
    if (!areaId) {
        const area = await mapper.createArea((zone ?? FALLBACK_ZONE).trim() || FALLBACK_ZONE, {
            ephemeral: true,
        });
        areaId = area.id;
        zoneAreas.set(key, areaId);
    }
    return areaId;
}

/** The direction walked into `fix`: the exit of the previous room whose destination id is
 *  the new room's id. Server-authoritative — no command sniffing needed. */
function arrivalDirection(fix: RoomFix): string | null {
    if (!lastRoom || fix.id === null) return null;
    for (const [dir, dest] of Object.entries(lastRoom.fix.exits)) {
        if (dest !== null && dest === fix.id) return dir;
    }
    return null;
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
 *  nudging along the vector on collision (docs/gmcp-mapping.md section 5.3). Reads go
 *  through a FRESH area handle — a cached one is a stale snapshot. */
function placement(areaId: AreaId, fix: RoomFix, direction: string | null): { x: number; y: number; level: number } {
    if (fix.coords) {
        return { x: fix.coords.x * GRID, y: fix.coords.y * GRID, level: Math.round(fix.coords.z) };
    }
    const area = mapper.getAreaById(areaId);
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

async function linkOrStub(areaId: AreaId, room: RoomNumber, dir: string, destId: string | null) {
    const from_direction = EXIT_DIRECTION[dir] ?? "Special";
    const command = dir;
    if (destId !== null) {
        const dest = mapper.findRoomByExternalId(destId);
        if (dest) {
            await mapper.createRoomExit(areaId, room, {
                from_direction,
                to_area_id: dest.area_id,
                to_room_number: dest.room_number,
                command,
                weight: 1,
            });
            return;
        }
    }
    // A destination-less exit is a dangling stub on the map until (and
    // unless) its far room is discovered.
    const exitId = await mapper.createRoomExit(areaId, room, {
        from_direction,
        command,
        weight: 1,
    });
    if (destId !== null) {
        const waiters = pendingLinks.get(destId) ?? [];
        waiters.push({ areaId, room, exitId });
        pendingLinks.set(destId, waiters);
    }
}

/** Upgrade every stub that was waiting for `id` to a real link at `(areaId, room)`. */
function resolvePending(id: string, areaId: AreaId, room: RoomNumber) {
    const waiters = pendingLinks.get(id);
    if (!waiters) return;
    pendingLinks.delete(id);
    for (const waiter of waiters) {
        mapper.setRoomExit(waiter.areaId, waiter.room, waiter.exitId, {
            to_area_id: areaId,
            to_room_number: room,
        });
    }
}

async function autoCreate(fix: RoomFix): Promise<void> {
    if (fix.id === null) return;
    const areaId = await zoneArea(fix.zone);
    const direction = arrivalDirection(fix);
    const at = placement(areaId, fix, direction);

    const params: CreateRoomParams = {
        title: fix.name,
        externalId: fix.id,
        x: at.x,
        y: at.y,
        level: at.level,
    };
    const color = fix.terrain ? TERRAIN_COLORS[fix.terrain.toLowerCase()] : undefined;
    if (color) params.color = color;
    const room = mapper.createRoom(areaId, params);
    if (fix.terrain) mapper.setRoomProperty(areaId, room, "terrain", fix.terrain);

    // Every exit of the new room: a real link when the destination is already known
    // (including the one we just came from), else a stub that upgrades when its id
    // finally appears (docs/gmcp-mapping.md section 5.3). The exit we arrived
    // THROUGH needs no special case: the previous room minted it as a stub naming this
    // room's id, and resolvePending below upgrades that stub — creating it again here
    // would double the edge.
    for (const [dir, dest] of Object.entries(fix.exits)) {
        await linkOrStub(areaId, room, dir, dest);
    }

    resolvePending(fix.id, areaId, room);
    mapper.setCurrentLocation(areaId, room);
    lastRoom = { areaId, room, fix };
}

async function handleFix(fix: RoomFix | null): Promise<void> {
    if (!fix) return;
    if (fix.unmappable || fix.id === null) {
        // The server withheld identity (maze, -1, continent grid): never guess.
        lastRoom = null;
        return;
    }
    const known = mapper.findRoomByExternalId(fix.id);
    if (known) {
        mapper.setCurrentLocation(known.area_id, known.room_number);
        lastRoom = { areaId: known.area_id, room: known.room_number, fix };
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
    await autoCreate(fix);
}

function enqueue(fix: RoomFix | null) {
    queue = queue.then(() => handleFix(fix)).catch((err) => {
        echo(`[auto-mapper] ${err}`);
    });
}

// ---------------------------------------------------------------------------------------
// Wire-up.
// ---------------------------------------------------------------------------------------

echo("[auto-mapper] active - mapping GMCP/MSDP room data into session maps (savemap to keep).");

gmcp.watch("Room.Info", (info: unknown) => enqueue(adaptGmcp(info)));
msdp.watch("ROOM", (room: unknown) => {
    sawCompositeRoom = true;
    enqueue(adaptMsdp(room));
});
msdp.watch("ROOM_VNUM", () => {
    if (!sawCompositeRoom) enqueue(adaptMsdpFlat());
});

// A failed movement produces no Room.Info; nothing to unwind — but a server that says so
// explicitly confirms we should not infer a move happened.
gmcp.onWrite("Room.WrongDir", () => {});

// The Room module is not in the host's blind-enable baseline on every game; ask for it.
gmcpCtl.enableModule("Room");

// ---------------------------------------------------------------------------------------
// savemap: promote session areas to local maps and keep mapping into them.
// ---------------------------------------------------------------------------------------

createAlias(/^savemap(?:\s+(?<zone>.+))?$/, async (matches: { zone?: string }) => {
    const filter = matches.zone ? zoneKey(matches.zone) : null;
    const chosen = [...zoneAreas.entries()].filter(([key]) => filter === null || key === filter);
    if (chosen.length === 0) {
        echo(filter === null
            ? "[auto-mapper] nothing mapped this session yet."
            : `[auto-mapper] no session map for "${matches.zone}".`);
        return;
    }
    const exports = [];
    for (const [, areaId] of chosen) {
        exports.push(await mapper.exportArea(areaId));
    }
    const importedIds = await mapper.importAreas(exports);
    // Rebind so mapping continues seamlessly into the saved copies, and drop the session
    // originals so each room id resolves to exactly one room again.
    chosen.forEach(([key, areaId], index) => {
        const importedId = importedIds[index];
        if (importedId) zoneAreas.set(key, importedId);
        mapper.deleteArea(areaId);
    });
    if (lastRoom && chosen.some(([, areaId]) => sameArea(areaId, lastRoom!.areaId))) {
        lastRoom = null;
    }
    echo(`[auto-mapper] saved ${importedIds.length} map(s).`);
});

