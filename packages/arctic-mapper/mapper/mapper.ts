import { mapper, sendRaw, echo } from 'smudgy:core';
import { get } from 'smudgy:params';

export enum RoomFlags {
    DT = 'DT',
    PEACE = 'PEACE',
    NO_MAGIC = 'NO_MAGIC',
    NO_RECALL = 'NO_RECALL',
    NO_LOCATE = 'NO_LOCATE',
    SPIN = 'SPIN',
    ORDERED = 'ORDERED',
    RECUPERATE = 'RECUPERATE',
    CLERIC_STUDY = 'CLERIC_STUDY',
    DRUID_STUDY = 'DRUID_STUDY',
    MAGE_STUDY = 'MAGE_STUDY',
    BARBARIAN_GUILD = 'BARBARIAN_GUILD',
    CLERIC_GUILD = 'CLERIC_GUILD',
    DARK_KNIGHT_GUILD = 'DARK_KNIGHT_GUILD',
    DRUID_GUILD = 'DRUID_GUILD',
    MAGE_GUILD = 'MAGE_GUILD',
    PALADIN_GUILD = 'PALADIN_GUILD',
    SCOUT_GUILD = 'SCOUT_GUILD',
    SHAMAN_GUILD = 'SHAMAN_GUILD',
    THIEF_GUILD = 'THIEF_GUILD',
    WARRIOR_GUILD = 'WARRIOR_GUILD',
    INN = 'INN',
    SHOP = 'SHOP',
    BANK = 'BANK',
    VAULT = 'VAULT',
    DUMP = 'DUMP',
    RANK = 'RANK',
    AGGRO = 'AGGRO',
    AGGRO_GOOD = 'AGGRO_GOOD',
    AGGRO_NONGOOD = 'AGGRO_NONGOOD',
    AGGRO_EVIL = 'AGGRO_EVIL',
    AGGRO_NONEVIL = 'AGGRO_NONEVIL',
    AGGRO_NEUTRAL = 'AGGRO_NEUTRAL',
    AGGRO_NONNEUTRAL = 'AGGRO_NONNEUTRAL',
    MERCHANT = 'MERCHANT',
    FLETCHER = 'FLETCHER',
    GENERAL_STORE = 'GENERAL_STORE',
    PORTAL = 'PORTAL',
    LIMIT_1 = 'LIMIT_1',
    LIMIT_2 = 'LIMIT_2',
    LIMIT_3 = 'LIMIT_3',
    LIMIT_4 = 'LIMIT_4',
    LIMIT_5 = 'LIMIT_5',
    LIMIT_6 = 'LIMIT_6',
    LIMIT_7 = 'LIMIT_7',
    LIMIT_8 = 'LIMIT_8',

    DONT_SPEEDWALK = 'DONT_SPEEDWALK',
}

export enum Direction {
    North = 'North',
    East = 'East',
    South = 'South',
    West = 'West',
    Up = 'Up',
    Down = 'Down',
};

export const SingleCharExit: Record<string, Direction> = {
    N: Direction.North,
    S: Direction.South,
    E: Direction.East,
    W: Direction.West,
    U: Direction.Up,
    D: Direction.Down,
};

export const OppositeDirection: Record<Direction, Direction> = {
    [Direction.North]: Direction.South,
    [Direction.East]: Direction.West,
    [Direction.South]: Direction.North,
    [Direction.West]: Direction.East,
    [Direction.Up]: Direction.Down,
    [Direction.Down]: Direction.Up,
}

export type MoveCommand = Direction | "look";

export const MoveCommands: Record<string, MoveCommand> = {
    n: Direction.North,
    no: Direction.North,
    nor: Direction.North,
    nort: Direction.North,
    north: Direction.North,
    e: Direction.East,
    ea: Direction.East,
    eas: Direction.East,
    east: Direction.East,
    s: Direction.South,
    sou: Direction.South,
    sout: Direction.South,
    south: Direction.South,
    w: Direction.West,
    we: Direction.West,
    wes: Direction.West,
    west: Direction.West,
    u: Direction.Up,
    up: Direction.Up,
    d: Direction.Down,
    ["do"]: Direction.Down,
    dow: Direction.Down,
    down: Direction.Down,
    l: "look",
    lo: "look",
    loo: "look",
    look: "look",
}

export const MoveCoordinates = {
    [Direction.North]: [0, -1, 0],
    [Direction.East]: [1, 0, 0],
    [Direction.South]: [0, 1, 0],
    [Direction.West]: [-1, 0, 0],
    [Direction.Up]: [0, 0, 1],
    [Direction.Down]: [0, 0, -1]
};

export const IsometricZMoveCoordinates = {
    [Direction.North]: [0, -1, 0],
    [Direction.East]: [1, 0, 0],
    [Direction.South]: [0, 1, 0],
    [Direction.West]: [-1, 0, 0],
    [Direction.Up]: [1, -1, 0],
    [Direction.Down]: [-1, 1, 0]
};

export enum ZMode {
    Auto,
    Normal,
    Isometric,
}

export enum Confidence {
    Full,
    Partial,
    None,
}

export enum State {
    Off,
    Following,
    Mapping,
    /** Following, plus movement commands open doors / use exit commands. */
    Active,
}

// --- Load-time configuration (smudgy:params) --------------------------------
// These read this package's configured options once at load. smudgy applies
// param changes on reload, so there are no live setters for them — `mode` keeps
// its existing `map off/follow/active` commands, but Z mode and text size are
// load-time only. Each falls back to its prior built-in default when unset or
// holding an unrecognized value.

/** A param's value as a string, or undefined when unset / not a string. */
function paramString(key: string): string | undefined {
    const value = get(key);
    return typeof value === "string" ? value : undefined;
}

/** The `mode` param (active/follow/off) -> the State the mapper starts in. */
const MODE_BY_PARAM: Record<string, State> = {
    active: State.Active,
    follow: State.Following,
    off: State.Off,
};
function configuredMode(): State {
    return MODE_BY_PARAM[paramString("mode") ?? ""] ?? State.Active;
}

/** The `zMode` param is a creation preference; existing links encode their own style. */
const ZMODE_BY_PARAM: Record<string, ZMode> = {
    auto: ZMode.Auto,
    normal: ZMode.Normal,
    levels: ZMode.Normal,
    isometric: ZMode.Isometric,
    projected: ZMode.Isometric,
};
function configuredZMode(): ZMode {
    return ZMODE_BY_PARAM[paramString("zMode") ?? ""] ?? ZMode.Auto;
}

/**
 * Named map-panel text sizes (pixels) for the `textSize` param. `medium` is the
 * prior built-in default (16 — Markdown's default, and the base the old
 * hard-coded flag size of 12 sat a quarter below).
 */
const TEXT_SIZES: Record<string, number> = {
    "x-small": 12,
    "small": 14,
    "medium": 16,
    "large": 20,
    "x-large": 24,
};
function configuredTextSize(): number {
    return TEXT_SIZES[paramString("textSize") ?? "small"] ?? TEXT_SIZES.medium;
}

/**
 * Side length (px) of the square map widget, from the numeric `widgetSize` param.
 * 350 is the default (the prior built-in size). A non-positive or non-numeric value
 * falls back to the default rather than collapsing the panel.
 */
const DEFAULT_WIDGET_SIZE = 225;
function configuredWidgetSize(): number {
    const value = get("widgetSize");
    return typeof value === "number" && value > 0 ? value : DEFAULT_WIDGET_SIZE;
}

export const DirectionLetter: Record<Direction, string> = {
    [Direction.North]: "n",
    [Direction.East]: "e",
    [Direction.South]: "s",
    [Direction.West]: "w",
    [Direction.Up]: "u",
    [Direction.Down]: "d",
};

/** Room property holding the command that opens/clears this room's exit (e.g. `open n`, `part brush`). */
export function openCommandProperty(direction: Direction): string {
    return `open_${DirectionLetter[direction]}_command`;
}

export function idsMatch(id1: AreaId | undefined | null, id2: AreaId | undefined | null) {
    if (!id1 && !id2) {
        return true;
    }

    if (!id1 || !id2) {
        return false;
    }

    return id1 === id2;
}

/**
 * Speedwalk from `from` (default: the mapper's current room) to `to` along the
 * mapped shortest path, one hop at a time. ArcticMUD movement lives here rather
 * than in the host so each MUD owns its own conventions: per hop we first send
 * the exit's open command (an explicit `open_<d>_command` property, or `open <d>`
 * when the exit is mapped closed), then the exit's `command` if set, else the
 * direction letter. Commands go out with `sendRaw` so the emitted directions do
 * NOT re-fire the movement-capture alias. Returns the number of hops walked.
 */
export function speedwalk(to: Room, from: Room | null = state.room): number {
    if (!from) {
        return 0;
    }

    const path = mapper.getPathBetweenRooms(
        from.area_id,
        from.room_number,
        to.area_id,
        to.room_number,
    );
    if (path.length < 2) {
        return 0;
    }

    // Split on ";" and raw-send each command (an open command may be compound,
    // e.g. "unlock n;open n").
    const sendCommands = (raw: string) => {
        for (const part of raw.split(";")) {
            const command = part.trim();
            if (command) {
                sendRaw(command);
            }
        }
    };

    let steps = 0;
    for (let i = 0; i < path.length - 1; i++) {
        const [stepAreaId, stepRoomNumber] = path[i];
        const [nextAreaId, nextRoomNumber] = path[i + 1];
        const room = mapper.getAreaById(stepAreaId).room(stepRoomNumber);
        if (!room) {
            continue;
        }
        const exit = room.exits.find((e) =>
            e.to_room_number === nextRoomNumber &&
            !!e.to_area_id && idsMatch(e.to_area_id, nextAreaId)
        );
        if (!exit) {
            continue;
        }

        const direction = exit.from_direction as Direction;
        const letter = DirectionLetter[direction] ?? exit.from_direction.toLowerCase();

        // Open a mapped-closed door before stepping through it.
        const openCommand = room.data(openCommandProperty(direction))
            || (exit.is_closed ? `open ${letter}` : null);
        if (openCommand) {
            sendCommands(openCommand);
        }

        // Move: an explicit exit command (e.g. "enter hole"), else the direction.
        sendCommands(exit.command && exit.command !== "" ? exit.command : letter);
        steps++;
    }

    return steps;
}

function findAreaByName(name: string): Area {
    return mapper.areas.find((a) =>
        a.name.toLowerCase() === name.toLowerCase()
    );
}

type AreaChangedListener = (area: Area | null) => void;
const areaChangedListeners: AreaChangedListener[] = [];

/** Register a callback fired whenever the current area changes. */
export function onAreaChanged(listener: AreaChangedListener) {
    areaChangedListeners.push(listener);
}

type RoomChangedListener = (room: Room | null) => void;
const roomChangedListeners: RoomChangedListener[] = [];

/** Register a callback fired whenever the current room changes. */
export function onRoomChanged(listener: RoomChangedListener) {
    roomChangedListeners.push(listener);
}

export class MapState {
    #area: Area | null = null;
    #room: Room | null = null;
    possibleRooms: Room[] = [];
    state: State = configuredMode();
    moveQueue: MoveCommand[] = [];
    lastMoveCommand = new Date();
    /** Exits from the most recent prompt; value is true when shown closed (parenthesized). Null when unknown. */
    seenExits: Map<Direction, boolean> | null = null;

    get area(): Area | null {
        return this.#area;
    }

    set area(value: Area | null) {
        const changed = value?.name !== this.#area?.name;
        this.#area = value;
        if (changed) {
            for (const listener of areaChangedListeners) {
                listener(value);
            }
        }
    }

    get room(): Room | null {
        return this.#room;
    }

    set room(value: Room | null) {
        // Fire only when we land in a *different* room (by area + number), not when
        // refreshRoomAndArea swaps in a fresh snapshot of the same room.
        const changed = value?.room_number !== this.#room?.room_number
            || !idsMatch(value?.area_id, this.#room?.area_id);
        this.#room = value;
        if (changed) {
            for (const listener of roomChangedListeners) {
                listener(value);
            }
        }
    }

    setCurrentRoom(room: Room) {
        this.area = mapper.getAreaById(room.area_id);
        this.room = this.area.room(room.room_number);
        mapper.setCurrentLocation(room.area_id, room.room_number);
    }

    setCurrentPossibleRooms(rooms: Room[]) {
        this.possibleRooms = rooms;
    }

    clearMoveQueue() {
        this.moveQueue = [];
    }

    captureMoveCommand(command: MoveCommand, prepend: boolean = false) {
        if (prepend) {
            this.moveQueue.unshift(command);
        } else {
            // if the last move command is over ten seconds old, clear the queue
            if (this.lastMoveCommand.getTime() + 10000 < new Date().getTime()) {
                this.clearMoveQueue();
            }
            this.moveQueue.push(command);
            this.lastMoveCommand = new Date();
        }
    }

    popMoveCommand(): MoveCommand | undefined {
        return this.moveQueue.shift();
    }

    refreshRoomAndArea() {
        if (this.area) {
            this.area = mapper.getAreaById(this.area.id);

            if (this.room) {
                this.room = this.area.room(this.room.room_number);
            }
        }
    }

    getRoomFromRoomTag(key: string): Room | null {
        if (/^\d+$/.test(key)) {
            return this.area?.room(parseInt(key)) ?? null;
        }

        const match = /^(.+)#(\d+)$/.exec(key);
        if (match) {
            const areaName = match[1];
            const roomNumber = parseInt(match[2]);
            return findAreaByName(areaName)?.room(roomNumber) ?? null;
        }

        return null;
    }
}

export class MapOptions {
    debug: boolean = false;
    debugRaw: boolean = false;
    /** Creation preference for new U/D links; Auto infers the nearby section. */
    zMode: ZMode = configuredZMode();
    /** Base text size (px) for the map panel's text/button/markdown widgets (the `textSize` param). */
    textSize: number = configuredTextSize();
    /** Side length (px) of the square map widget (the `widgetSize` param). */
    widgetSize: number = configuredWidgetSize();

    get moveCoordinates() {
        // Coordinate-only legacy operations have no topology to infer from.
        // Auto therefore uses levels; room creation goes through map-layout.
        return this.zMode === ZMode.Isometric ? IsometricZMoveCoordinates : MoveCoordinates;
    }
}

export const state = new MapState();
export const options = new MapOptions();

/** Echo a diagnostic line when `map debug` is on. */
export function debugLog(message: string) {
    if (options.debug) {
        echo(`[mapper] ${message}`);
    }
}

export const EXIT_MASK = {
    [Direction.North]: 0b100000,
    [Direction.East]: 0b010000,
    [Direction.South]: 0b001000,
    [Direction.West]: 0b000100,
    [Direction.Up]: 0b000010,
    [Direction.Down]: 0b000001,
}

export interface ExitObservation {
    direction: Direction;
    /** True when the exit appeared parenthesized in the prompt — a closed door. */
    closed: boolean;
}

/** Parse a prompt exit list like "(E)S" or "(ES)" — parenthesized exits are closed. */
export function parseRoomExitsDetailed(exits: string | undefined): ExitObservation[] {
    const result: ExitObservation[] = [];
    let inParens = false;
    for (const c of exits ?? "") {
        if (c === "(") {
            inParens = true;
        } else if (c === ")") {
            inParens = false;
        } else {
            const direction = SingleCharExit[c as keyof typeof SingleCharExit];
            if (direction) {
                result.push({ direction, closed: inParens });
            }
        }
    }
    return result;
}

export function parseRoomExits(exits: string | undefined): Direction[] {
    return parseRoomExitsDetailed(exits).map((e) => e.direction);
}

export async function linkRooms(from: Room, to: Room, direction: Direction, createReturnExit: boolean = true): Promise<ExitId[]> {
    const oppositeDirection = OppositeDirection[direction];

    const toRemoveFrom = from.exits.filter((e) =>
        e.from_direction === direction && e.to_room_number !== to.room_number
    );
    const toRemoveTo = to.exits.filter((e) =>
        e.from_direction === oppositeDirection &&
        e.to_room_number !== from.room_number
    );

    const fromExit = from.exits.find((e) =>
        e.from_direction === direction &&
        e.to_room_number === to.room_number
    );

    if (idsMatch(from.area_id, to.area_id)) {
        const ids: ExitId[] = [];
        await mapper.mutateArea(from.area_id, async (mutation) => {
            for (const exit of toRemoveFrom) {
                await mutation.deleteRoomExit(from.room_number, exit.id);
            }
            for (const exit of toRemoveTo) {
                await mutation.deleteRoomExit(to.room_number, exit.id);
            }
            ids.push(fromExit?.id ?? await mutation.createRoomExit(from.room_number, {
                from_direction: direction,
                to_direction: oppositeDirection,
                to_area_id: to.area_id,
                to_room_number: to.room_number,
            }));
            if (createReturnExit) {
                const toExit = to.exits.find((e) =>
                    e.from_direction === oppositeDirection &&
                    e.to_room_number === from.room_number
                );
                ids.push(toExit?.id ?? await mutation.createRoomExit(to.room_number, {
                    from_direction: oppositeDirection,
                    to_direction: direction,
                    to_area_id: from.area_id,
                    to_room_number: from.room_number,
                }));
            }
        }, { description: "Link Arctic rooms" });
        return ids;
    }

    for (const exit of toRemoveFrom) {
        await mapper.deleteRoomExit(from.area_id, from.room_number, exit.id);
    }
    for (const exit of toRemoveTo) {
        await mapper.deleteRoomExit(to.area_id, to.room_number, exit.id);
    }

    const fromExitPromise = fromExit ? Promise.resolve(fromExit.id) :
        mapper.createRoomExit(from.area_id, from.room_number, {
            from_direction: direction,
            to_direction: oppositeDirection,
            to_area_id: to.area_id,
            to_room_number: to.room_number,
        });

    if (!createReturnExit) {
        return [await fromExitPromise];
    }

    {
        const toExit = to.exits.find((e) =>
            e.from_direction === oppositeDirection &&
            e.to_room_number === from.room_number
        );

        const toExitPromise = toExit ? Promise.resolve(toExit.id) :
            mapper.createRoomExit(to.area_id, to.room_number, {
                from_direction: oppositeDirection,
                to_direction: direction,
                to_area_id: from.area_id,
                to_room_number: from.room_number,
            });

        return Promise.all([fromExitPromise, toExitPromise]);
    }
}

export function findRoomsAt(x: number, y: number, level: number): Room[] {
    const radius = 0.25;

    const rooms: Room[] = [];

    if (!state.area) {
        return rooms;
    }

    for (const roomNumber of state.area.room_numbers) {
        const room = state.area.room(roomNumber);
        const roomX = room.x;
        const roomY = room.y;
        const roomLevel = room.level;

        if (Math.abs(roomX - x) <= radius && Math.abs(roomY - y) <= radius && roomLevel === level) {
            rooms.push(room);
        }
    }

    return rooms;
}

export function findRoomsInRect(x1: number, y1: number, x2: number, y2: number, level: number): Room[] {
    const rooms: Room[] = [];

    if (!state.area) {
        return rooms;
    }

    const minX = Math.min(x1, x2);
    const maxX = Math.max(x1, x2);
    const minY = Math.min(y1, y2);
    const maxY = Math.max(y1, y2);

    for (const roomNumber of state.area.room_numbers) {
        const room = state.area.room(roomNumber);
        if (room.level === level && room.x >= minX && room.x <= maxX && room.y >= minY && room.y <= maxY) {
            rooms.push(room);
        }
    }

    return rooms;
}

function linesIntersect(xa1, ya1, xa2, ya2, xb1, yb1, xb2, yb2) {
    const det = (xa2 - xa1) * (yb2 - yb1) - (xb2 - xb1) * (ya2 - ya1);
    if (det === 0) {
        return false;
    } else {
        const lambda = ((yb2 - yb1) * (xb2 - xa1) + (xb1 - xb2) * (yb2 - ya1)) / det;
        const gamma = ((ya1 - ya2) * (xb2 - xa1) + (xa2 - xa1) * (yb2 - ya1)) / det;
        return (0 < lambda && lambda < 1) && (0 < gamma && gamma < 1);
    }
};

export function distanceBetweenRooms(a: Room, b: Room) {
    if (!a || !b) {
        return Number.MAX_VALUE;
    }
    return Math.sqrt(Math.abs(a.x - b.x) ** 2 + Math.abs(a.y - b.y) ** 2 + Math.abs(a.level - b.level) ** 2);
}

export function findRoomsWithExitsThrough(x1: number, y1: number, x2: number, y2: number, level: number): Room[] {
    const result = [];
    const seenRoomNumbers = new Set<number>();

    if (!state.area) {
        return result;
    }

    for (const roomNumber of state.area.room_numbers) {
        const room = state.area.room(roomNumber);
        if (room.level !== level) {
            continue;
        }

        for (const exit of room.exits) {
            if (typeof exit.to_room_number === "number" && exit.to_area_id && idsMatch(exit.to_area_id, exit.from_area_id)) {
                const to_room = state.area.room(exit.to_room_number);
                if (to_room.level !== level) {
                    continue;
                }

                const from_x = room.x;
                const from_y = room.y;
                const to_x = to_room.x;
                const to_y = to_room.y;

                if (linesIntersect(x1, y1, x2, y2, from_x, from_y, to_x, to_y)) {
                    if (!seenRoomNumbers.has(room.room_number)) {
                        result.push(room);
                        seenRoomNumbers.add(room.room_number);
                    }
                    if (!seenRoomNumbers.has(to_room.room_number)) {
                        result.push(to_room);
                        seenRoomNumbers.add(to_room.room_number);
                    }
                }
            }
        }
    }

    return result;
}


export function roomsImpactedByPush(room: Room, direction: Direction, found: Set<RoomNumber> = new Set()): RoomNumber[] {
    const recurseTo = [];

    found.add(room.room_number);

    // rooms impacted by the push are the rooms that are near where we are pushing to, and any rooms connected perpendicular to
    // the room itself or to any affected rooms

    const offset = options.moveCoordinates[direction];

    const exitsToFollow = direction === Direction.North ? [Direction.East, Direction.West, Direction.Up, Direction.Down] :
        direction === Direction.East ? [Direction.North, Direction.South, Direction.Up, Direction.Down] :
            direction === Direction.South ? [Direction.East, Direction.West, Direction.Up, Direction.Down] :
                direction === Direction.West ? [Direction.North, Direction.South, Direction.Up, Direction.Down] :
                    [];


    for (const exit of room.exits) {
        if (typeof exit.to_room_number === "number" && exitsToFollow.includes(exit.from_direction as Direction) && exit.to_area_id && idsMatch(exit.from_area_id, exit.to_area_id)) {
            if (!found.has(exit.to_room_number)) {
                const toRoom = state.area.room(exit.to_room_number);

                if (toRoom && toRoom.level === room.level) {
                    recurseTo.push(exit.to_room_number);

                    // if the exit will move across any rooms during the push, push those rooms too
                    const roomsToAdd = findRoomsInRect(room.x, room.y, toRoom.x + offset[0] * 1.5, toRoom.y + offset[1] * 1.5, room.level);
                    for (const room of roomsToAdd) {
                        if (!found.has(room.room_number)) {
                            recurseTo.push(room.room_number);
                        }
                    }
                }
            }
        }
    }

    const willCollideWith = findRoomsAt(room.x + offset[0], room.y + offset[1], room.level + offset[2]);

    for (const room of willCollideWith) {
        if (!found.has(room.room_number)) {
            recurseTo.push(room.room_number);
        }
    }

    const intersectX = room.x + offset[0] * 1.5;
    const intersectY = room.y + offset[1] * 1.5;
    const willIntersectWith = findRoomsWithExitsThrough(room.x, room.y, intersectX, intersectY, room.level);

    for (const room of willIntersectWith) {
        if (!found.has(room.room_number)) {
            recurseTo.push(room.room_number);
        }
    }

    for (const roomNumber of recurseTo) {
        found.add(roomNumber);
    }

    const foundThroughRecursion = recurseTo.flatMap((roomNumber) => roomsImpactedByPush(state.area.room(roomNumber), direction, found));

    for (const roomNumber of foundThroughRecursion) {
        found.add(roomNumber);
    }

    return [...found.values()];
}
