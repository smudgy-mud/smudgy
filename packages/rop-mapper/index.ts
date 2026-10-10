import {
    asId,
    asNumber,
    asTitle,
    asZone,
    canonicalDir,
    createAutoMapper,
    type DoorFix,
    type NeighborhoodFix,
    type NeighborhoodRoomFix,
    type RoomFix,
} from "smudgy://official/auto-mapper/engine.ts";
import { createAlias } from "smudgy:core";

const MAX_EXITS = 64;
const MAX_MAP_ROOMS = 512;

const SECTOR_TERRAIN: Record<number, string> = {
    0: "inside",
    1: "city",
    2: "field",
    3: "forest",
    4: "hills",
    5: "mountain",
    6: "water",
    7: "water_deep",
    8: "underwater",
    9: "air",
    10: "desert",
    11: "snow",
    12: "tropical",
    13: "field",
    14: "ice",
    15: "marsh",
};

const GRID_OFFSET: Record<string, [number, number]> = {
    n: [0, -1],
    e: [1, 0],
    s: [0, 1],
    w: [-1, 0],
};

function flag(value: unknown): boolean {
    return asNumber(value) === 1;
}

function exitDestination(value: unknown): string | null {
    if (value && typeof value === "object") {
        return asId((value as Record<string, unknown>).v);
    }
    return asId(value);
}

/** An exit's `door`, numbered as Mudlet numbers door states: 0 none, 1 open, 2 closed,
 *  3 locked. No door reports nothing, leaving a door the map already has as it is. */
function exitDoor(value: unknown): DoorFix | undefined {
    if (!value || typeof value !== "object") return undefined;
    const state = asNumber((value as Record<string, unknown>).door);
    if (state === null || state < 1) return undefined;
    return { state: state >= 3 ? "locked" : state >= 2 ? "closed" : "open" };
}

function adaptRoomInfo(info: unknown): RoomFix | null {
    if (!info || typeof info !== "object") return null;
    const fields = info as Record<string, unknown>;
    const id = asId(fields.num);
    const exits: Record<string, string | null> = {};
    const doors: Record<string, DoorFix> = {};
    const rawExits = fields.exits;
    if (rawExits && typeof rawExits === "object") {
        let count = 0;
        for (const [rawDirection, value] of Object.entries(rawExits as Record<string, unknown>)) {
            if (count >= MAX_EXITS) break;
            const direction = canonicalDir(rawDirection);
            if (!direction) continue;
            const destination = exitDestination(value);
            // RoP can report a room as its own destination; it is not a traversable link.
            if (destination !== null && destination === id) continue;
            exits[direction] = destination;
            const door = exitDoor(value);
            if (door) doors[direction] = door;
            count += 1;
        }
    }

    let terrain = typeof fields.terrain === "string" ? fields.terrain : null;
    if (flag(fields.mana)) {
        terrain = "mana";
    } else if (terrain === "inside" && flag(fields.indoors) && flag(fields.dark)) {
        terrain = "inside_dark";
    }

    return {
        id,
        name: flag(fields.blind) ? "(dark)" : asTitle(fields.name),
        zone: asZone(fields.zone),
        terrain,
        exits,
        doors,
        coords: null,
        unmappable: id === null,
    };
}

function adaptRoomMap(map: unknown): NeighborhoodFix | null {
    if (!map || typeof map !== "object") return null;
    const fields = map as Record<string, unknown>;
    const px = asNumber(fields.px);
    const py = asNumber(fields.py);
    if (px === null || py === null || !Array.isArray(fields.rooms)) return null;

    const rows = fields.rooms.slice(0, MAX_MAP_ROOMS);
    const grid = new Map<string, string>();
    for (const row of rows) {
        if (!row || typeof row !== "object") continue;
        const room = row as Record<string, unknown>;
        const x = asNumber(room.x);
        const y = asNumber(room.y);
        const id = asId(room.v);
        if (x !== null && y !== null && id !== null) {
            grid.set(`${x},${y}`, id);
        }
    }
    const centerId = grid.get(`${px},${py}`);
    if (!centerId) return null;

    const rooms: NeighborhoodRoomFix[] = [];
    for (const row of rows) {
        if (!row || typeof row !== "object") continue;
        const room = row as Record<string, unknown>;
        const x = asNumber(room.x);
        const y = asNumber(room.y);
        const id = asId(room.v);
        if (x === null || y === null || id === null) continue;

        const exits: Record<string, string | null> = {};
        const advertised = typeof room.e === "string" ? room.e.slice(0, MAX_EXITS) : "";
        for (const rawDirection of advertised) {
            const direction = canonicalDir(rawDirection);
            if (!direction) continue;
            const offset = GRID_OFFSET[rawDirection.toLowerCase()];
            exits[direction] = offset
                ? (grid.get(`${x + offset[0]},${y + offset[1]}`) ?? null)
                : null;
        }

        const sector = asNumber(room.s);
        let terrain = sector === null ? null : (SECTOR_TERRAIN[sector] ?? "field");
        if (sector === 0 && flag(room.i) && flag(room.d)) terrain = "inside_dark";
        rooms.push({
            id,
            terrain,
            offset: { x: x - px, y: y - py, z: 0 },
            exits,
        });
    }

    return { centerId, rooms };
}

const ropMapper = createAutoMapper({
    roomInfo: adaptRoomInfo,
    roomMap: adaptRoomMap,
    msdp: false,
    // RoP provides destination ids in Room.Info. Outgoing command inference would only
    // introduce ambiguity after a closed door or another refused movement.
    inferMovementFromCommands: false,
});

ropMapper.start();

createAlias(/^ropmap\s+upgrade(?:\s+(?<zone>.+))?$/i, ({ zone }: { zone?: string }) => {
    ropMapper.upgradeToCloud(zone);
});
