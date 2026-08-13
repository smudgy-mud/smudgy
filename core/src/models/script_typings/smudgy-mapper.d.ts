// =============================================================================
//  smudgy mapper -- TypeScript declarations  (GENERATED -- DO NOT EDIT)
// =============================================================================
//  smudgy writes and overwrites this file every time a session starts. It teaches
//  VS Code (and any TypeScript-aware editor) about the `mapper` API.
//
//  Import the runtime values from `smudgy:core`: `mapper` is the current session's map
//  API, and `Area` is its runtime constructor for optional `instanceof` checks. The
//  declarations below supply the global ambient map TYPES that those exports reference.
//
//  These are GLOBAL ambient declarations (no `declare module`), so the names
//  (`Mapper`, `Area`, `Room`, `Exit`, `AreaId`, ...) remain visible without imports and
//  are also referenced by smudgy-core.d.ts's module exports.
//
//  Edits here are lost on the next launch.
// =============================================================================

// ---- Identifiers ------------------------------------------------------------

/**
 * An area's identifier. Treat it as **opaque**: take it from one mapper call and
 * pass it back to another, unchanged. **Careful:** this is not the same as the
 * UUID **string** the `map:room` event delivers; mapper calls accept only the
 * pair. Real ids carry `BigInt` halves (see {@link ConnectionId}),
 * which `JSON.stringify` rejects — so mapper-issued ids cannot travel
 * session-store writes or store bindings. Where an area scope must ride JSON —
 * store-bound MapView `apply` arrays — use the UUID string spelling instead:
 * `MapStyleApplication.area` in `smudgy:widgets` accepts either form.
 */
type AreaId = readonly [number, number];

/** An atlas (map folder) identifier, opaque like {@link AreaId}. */
type AtlasId = readonly [number, number];

/** Where a map is stored. Session maps disappear when the session closes. */
type MapStorage = "session" | "local" | "cloud";

/** A room number within an area (a 32-bit integer). */
type RoomNumber = number;

/** An exit's identifier: a 2-element `[hi, lo]` pair, like {@link AreaId}. Opaque. */
type ExitId = readonly [number, number];
/**
 * A Connection's identifier, opaque like {@link ExitId}.
 *
 * **Careful:** the `[hi, lo]` halves are 64-bit UUID halves. Values beyond
 * `Number.MAX_SAFE_INTEGER` (essentially always for real ids) are delivered
 * as `BigInt`, so despite this type's spelling the halves are `bigint` at
 * runtime. `JSON.stringify` throws on `BigInt`, which means these ids cannot
 * travel session-store writes or store bindings, and coercing a half with
 * `Number()` rounds it and will silently never match. Pass ids straight back
 * to mapper calls; for widget-facing selection use room + direction exit
 * refs instead (see `MapExitRef` in `smudgy:widgets`).
 */
type ConnectionId = readonly [number, number];
/** A queued mapper mutation's identifier, opaque like {@link ExitId}. */
type OperationId = readonly [number, number];

/** A compass/special exit direction (the canonical PascalCase names). */
type ExitDirection =
    | "North"
    | "East"
    | "South"
    | "West"
    | "Up"
    | "Down"
    | "Northeast"
    | "Northwest"
    | "Southeast"
    | "Southwest"
    | "In"
    | "Out"
    | "Special"
    | "Other";

// ---- Labels + shapes --------------------------------------------------------

/** A label's identifier: a 2-element `[hi, lo]` UUID pair, like {@link AreaId}. Opaque. */
type LabelId = readonly [number, number];
/** A shape's identifier: a 2-element `[hi, lo]` UUID pair, like {@link AreaId}. Opaque. */
type ShapeId = readonly [number, number];

/** Horizontal alignment of a label's text. */
type LabelHorizontalAlign = "Left" | "Center" | "Right";
/** Vertical alignment of a label's text. */
type LabelVerticalAlign = "Top" | "Center" | "Bottom";
/** A shape's kind. */
type ShapeKind = "Rectangle" | "RoundedRectangle";

/** A text label read back from an area (`area.labels`). */
interface Label {
    readonly id: LabelId;
    /** Map level / z-layer. */
    readonly level: number;
    readonly x: number;
    readonly y: number;
    readonly width: number;
    readonly height: number;
    readonly horizontal_alignment: LabelHorizontalAlign;
    readonly vertical_alignment: LabelVerticalAlign;
    readonly text: string;
    /** A CSS color string. */
    readonly color: string;
    /** A CSS color string for the background (`""` for none). */
    readonly background_color: string;
    readonly font_size: number;
    readonly font_weight: number;
}

/** Fields accepted when creating a label (`mapper.createLabel`). Position, size, and `text` are
 *  required; any omitted field takes its default. */
interface LabelArgs {
    x: number;
    y: number;
    width: number;
    height: number;
    text: string;
    /** Map level / z-layer (default 0). */
    level?: number;
    /** Text alignment (defaults: Center / Center). */
    horizontal_alignment?: LabelHorizontalAlign;
    vertical_alignment?: LabelVerticalAlign;
    /** A CSS color string for the text (default `"#ffffff"`). */
    color?: string;
    /** A CSS color string for the background; omit for none. */
    background_color?: string;
    /** Text size in px (default 16). */
    font_size?: number;
    /** Text weight (default 400). */
    font_weight?: number;
}

/** Fields accepted when updating a label (`mapper.setLabel`). Any omitted field is left
 *  unchanged. */
interface LabelUpdates {
    x?: number;
    y?: number;
    width?: number;
    height?: number;
    text?: string;
    /** Map level / z-layer. */
    level?: number;
    horizontal_alignment?: LabelHorizontalAlign;
    vertical_alignment?: LabelVerticalAlign;
    /** A CSS color string for the text. */
    color?: string;
    /** A CSS color string for the background. */
    background_color?: string;
    font_size?: number;
    font_weight?: number;
}

/** A graphical shape read back from an area (`area.shapes`). */
interface Shape {
    readonly id: ShapeId;
    /** Map level / z-layer. */
    readonly level: number;
    readonly x: number;
    readonly y: number;
    readonly width: number;
    readonly height: number;
    /** A CSS color string, or `null` for none. */
    readonly background_color: string | null;
    /** A CSS color string, or `null` for none. */
    readonly stroke_color: string | null;
    readonly shape_type: ShapeKind;
    readonly border_radius: number;
    readonly stroke_width: number;
}

/** Fields accepted when creating a shape (`mapper.createShape`). Position and size are required;
 *  any omitted field takes its default. */
interface ShapeArgs {
    x: number;
    y: number;
    width: number;
    height: number;
    /** Map level / z-layer (default 0). */
    level?: number;
    /** A CSS fill color; omit for none. */
    background_color?: string;
    /** A CSS stroke color; omit for none. */
    stroke_color?: string;
    /** Shape kind (default `"Rectangle"`). */
    shape_type?: ShapeKind;
    /** Corner radius (default 0). */
    border_radius?: number;
    /** Stroke width in px. */
    stroke_width?: number;
}

/** Fields accepted when updating a shape (`mapper.setShape`). Any omitted field is left
 *  unchanged. */
interface ShapeUpdates {
    x?: number;
    y?: number;
    width?: number;
    height?: number;
    /** Map level / z-layer. */
    level?: number;
    /** A CSS fill color. */
    background_color?: string;
    /** A CSS stroke color. */
    stroke_color?: string;
    shape_type?: ShapeKind;
    border_radius?: number;
    stroke_width?: number;
}

/** A portable area export, produced by {@link Mapper.exportArea} and consumed by
 *  {@link Mapper.importArea}/{@link Mapper.importAreas}. Treat it as **opaque**:
 *  export it, store it, import it back, but do not depend on its internal shape. */
type AreaJson = Record<string, unknown>;

// ---- Rooms ------------------------------------------------------------------

/** Fields accepted when creating a room (`mapper.createRoom`). Any omitted field
 *  takes its default. */
interface CreateRoomParams {
    title?: string;
    description?: string;
    /** Map level / z-layer. */
    level?: number;
    x?: number;
    y?: number;
    /** A CSS color string. */
    color?: string;
    /**
     * The server's own id for this room (the room number games send over
     * GMCP or MSDP). An empty string clears an existing binding.
     */
    externalId?: string;
}

/** Fields accepted when updating a room (`mapper.updateRoom`/`Room.update`): the same
 *  set as creation. Any omitted field is left unchanged. */
type UpdateRoomParams = CreateRoomParams;

interface MutateAreaOptions {
    /** Description shown by save/conflict diagnostics. */
    description?: string;
}

/**
 * One exit read back from a room (`room.exits`). Optional links are present but `null`
 * when unset (not omitted).
 */
interface Exit {
    readonly id: ExitId;
    readonly from_direction: ExitDirection;
    readonly from_area_id: AreaId;
    readonly from_room_number: RoomNumber;
    readonly to_direction: ExitDirection | null;
    readonly to_area_id: AreaId | null;
    readonly to_room_number: RoomNumber | null;
    readonly is_hidden: boolean;
    readonly is_closed: boolean;
    readonly is_locked: boolean;
    /** Pathfinding cost. */
    readonly weight: number;
    /** The command sent to traverse this exit, or `null` to use `from_direction`. */
    readonly command: string | null;
}

/** Fields accepted when creating an exit (`mapper.createRoomExit`). Only
 *  `from_direction` is required. Visual appearance (routing, dash, color,
 *  thickness) lives on the shared Connection, not the exit. */
interface ExitArgs {
    from_direction: ExitDirection;
    to_direction?: ExitDirection;
    to_area_id?: AreaId;
    to_room_number?: RoomNumber;
    is_hidden?: boolean;
    is_closed?: boolean;
    is_locked?: boolean;
    weight?: number;
    command?: string;
}

/** Fields accepted when updating an exit (`mapper.setRoomExit`). Any omitted field is
 *  left unchanged. */
interface ExitUpdates {
    from_direction?: ExitDirection;
    to_direction?: ExitDirection;
    to_area_id?: AreaId;
    to_room_number?: RoomNumber;
    is_hidden?: boolean;
    is_closed?: boolean;
    is_locked?: boolean;
    weight?: number;
    command?: string;
}

// ---- Connections ------------------------------------------------------------

/** One of the four walls where a Connection attaches to a room. */
type RoomSide = "North" | "East" | "South" | "West";
/** Whether a port follows automatic wall redistribution or keeps an author-selected offset. */
type PortMode = "AutoPinned" | "Manual";
/** The topology represented by a Connection. */
type ConnectionKind = "Internal" | "SelfLoop" | "Dangling" | "External" | "CrossLevel";
/** How a Connection's centerline is produced and stored. */
type ConnectionRouting = "Stub" | "Simple" | "Manual" | "Automatic";
/** Whether routed segments may be diagonal or must remain axis-aligned. */
type ConnectionSegmentShape = "Direct" | "Orthogonal";
/** How turns between Connection segments are drawn. */
type ConnectionCorner = "Sharp" | "Rounded";
/** The repeating stroke pattern used to draw a Connection. */
type ConnectionDash = "Solid" | "Dashed" | "Dotted";

/** One interior Connection centerline vertex in area coordinates. */
interface MapPoint {
    x: number;
    y: number;
}

/** A Connection's wall attachment on one room. */
interface ConnectionEndpoint {
    room_number: RoomNumber;
    side: RoomSide;
    /** Normalized position along the room wall, from 0 through 1. */
    port_offset: number;
    port_mode: PortMode;
}

/** Shared topology, route, and appearance for one or two member Exits. */
interface Connection {
    readonly id: ConnectionId;
    readonly endpoint_a: ConnectionEndpoint;
    readonly endpoint_b: ConnectionEndpoint | null;
    readonly kind: ConnectionKind;
    readonly routing: ConnectionRouting;
    readonly segment_shape: ConnectionSegmentShape;
    readonly corner: ConnectionCorner;
    readonly route_points: MapPoint[];
    readonly dash: ConnectionDash;
    readonly color: string;
    readonly thickness: number;
}

/** Geometry/appearance fields accepted by {@link Mapper.setConnection}. */
interface ConnectionUpdates {
    endpoint_a?: ConnectionEndpoint;
    endpoint_b?: ConnectionEndpoint;
    routing?: ConnectionRouting;
    segment_shape?: ConnectionSegmentShape;
    corner?: ConnectionCorner;
    route_points?: MapPoint[];
    dash?: ConnectionDash;
    color?: string;
    thickness?: number;
}

/** One directed Exit to create as a member of a new Connection. */
interface LinkTraversalArgs extends ExitArgs {
    /** Room that owns this traversal. */
    room_number: RoomNumber;
}

/** One atomic link creation: Connection first, followed by one or two traversals. */
interface LinkCreateArgs extends ConnectionUpdates {
    endpoint_a: ConnectionEndpoint;
    endpoint_b?: ConnectionEndpoint;
    traversals: LinkTraversalArgs[];
}

/** Callback-scoped collector used by {@link Mapper.mutateArea}. Calls update a
 * callback-local draft and are submitted only after the callback completes. */
interface AreaMutator {
    /**
     * Draft a room under a number reserved from the live allocator: ambient
     * creators in this client (`mapper.createRoom`, the map editor, other
     * open mutators) skip reserved numbers, so a create landing while the
     * callback is open cannot collide with the draft. The number is
     * provisional (the room exists only once the mutation commits), and the
     * reservation is released when the callback finishes or aborts, so an
     * aborted draft's numbers become available again.
     *
     * The draft submits as a must-not-exist create: if the number is taken
     * by submission time (another client raced it in), the mutation is
     * rejected (`mutateArea` throws with `room_number_exists` in the
     * message) rather than silently merging two logical rooms.
     */
    createRoom(params: CreateRoomParams): Promise<RoomNumber>;
    updateRoom(room: Room | RoomNumber, fields: UpdateRoomParams): Promise<void>;
    updateRooms(updates: [RoomNumber, UpdateRoomParams][]): Promise<void>;
    setRoomTitle(room: Room | RoomNumber, title: string): Promise<void>;
    setRoomDescription(room: Room | RoomNumber, description: string): Promise<void>;
    setRoomColor(room: Room | RoomNumber, color: string): Promise<void>;
    setRoomLevel(room: Room | RoomNumber, level: number): Promise<void>;
    setRoomX(room: Room | RoomNumber, x: number): Promise<void>;
    setRoomY(room: Room | RoomNumber, y: number): Promise<void>;
    setRoomExternalId(room: Room | RoomNumber, externalId: string): Promise<void>;
    setRoomProperty(room: Room | RoomNumber, name: string, value: string): Promise<void>;
    setAreaProperty(name: string, value: string): Promise<void>;
    addRoomTag(room: Room | RoomNumber, tag: string): Promise<void>;
    removeRoomTag(room: Room | RoomNumber, tag: string): Promise<void>;
    createRoomExit(room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId>;
    setRoomExit(room: Room | RoomNumber, exitId: ExitId, exit: ExitUpdates): Promise<void>;
    deleteRoom(room: Room | RoomNumber): Promise<void>;
    deleteRoomExit(room: Room | RoomNumber, exitId: ExitId): Promise<void>;
    createLink(link: LinkCreateArgs): Promise<ConnectionId>;
    setConnection(connectionId: ConnectionId, updates: ConnectionUpdates): Promise<void>;
}

/** A room read from the map. Obtain one via `area.room(n)` or the `listRooms*` helpers. */
interface Room {
    readonly room_number: RoomNumber;
    readonly area_id: AreaId;
    readonly title: string;
    /**
     * The server's own id for this room (the room number games send over
     * GMCP or MSDP), or `undefined` if none is bound. Bind one at creation
     * (`externalId` in the room fields) or with `mapper.setRoomExternalId`.
     */
    readonly externalId: string | undefined;
    readonly description: string;
    readonly level: number;
    readonly x: number;
    readonly y: number;
    /** A CSS color string. */
    readonly color: string;
    readonly exits: Exit[];
    /** Read a custom room property by key (or `undefined` if unset). */
    data(key: string): string | undefined;
    /** This room's tags, normalized to UPPERCASE and sorted. */
    readonly tags: string[];
    /** Whether this room carries `tag` (case-insensitive). */
    hasTag(tag: string): boolean;
    /** Update multiple fields of this room in one cache update; only present fields change. */
    update(fields: UpdateRoomParams): Promise<OperationId | null>;
    toString(): string;
}

// ---- Areas ------------------------------------------------------------------

/**
 * A map area. You get areas from the mapper (`mapper.areas`,
 * `mapper.getAreaById`), never by constructing one. For a runtime check, import
 * the constructor: `import { Area } from "smudgy:core"`.
 */
interface Area {
    readonly id: AreaId;
    /**
     * The area id as its canonical hyphenated lowercase UUID string: the
     * JSON-safe spelling of `id`, as carried by the `map:room` event's
     * `areaId` field and accepted by MapView apply-area scoping.
     */
    readonly uuid: string;
    readonly name: string;
    readonly room_numbers: RoomNumber[];
    /**
     * Whether this is a session map: it lives only for this session and is
     * discarded when the session closes.
     * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
     * Use `storage === "session"` instead.
     */
    readonly isEphemeral: boolean;
    /** The area's actual storage tier. */
    readonly storage: MapStorage;
    /** The next unused room number in this area. */
    readonly next_room_number: RoomNumber;
    /** The room with this number, or `undefined`. */
    room(roomNumber: number): Room | undefined;
    /** Read a custom area property by key (or `undefined` if unset). */
    data(key: string): string | undefined;
    /** This area's text labels. */
    readonly labels: Label[];
    /** This area's graphical shapes. */
    readonly shapes: Shape[];
    /** This area's shared link geometry and appearance records. */
    readonly connections: Connection[];
    toString(): string;
}

// ---- The mapper -------------------------------------------------------------

/** Options for {@link Mapper.createArea}. */
interface CreateAreaOptions {
    /**
     * The authoritative storage tier. When omitted, the area is durable in
     * the default tier: cloud when signed in, local otherwise (or the
     * atlas's tier when `atlas` is given).
     */
    storage?: MapStorage;
    /**
     * Optionally create the area inside this atlas. The atlas determines the
     * storage tier when `storage` is omitted; when both are given they must
     * match.
     */
    atlas?: Atlas | AtlasId;
    /**
     * Create a session map: it lives only for this session, is never saved
     * or synced, and is discarded when the session closes. Mutually
     * exclusive with `storage`, which wins if both are supplied.
     * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
     * Use `storage: "session"` instead.
     */
    ephemeral?: boolean;
}

/** An atlas (map folder). Session storage does not support atlases. */
interface Atlas {
    readonly id: AtlasId;
    readonly name: string;
    /**
     * The atlas's live tier. Moving an atlas creates a new id and invalidates
     * the source handle; use the `Atlas` returned by `moveAtlas` afterward.
     */
    readonly storage: MapStorage;
    toString(): string;
}

/** A destination used by map copy and move operations. */
interface MapDestination {
    storage: MapStorage;
    /** Omit to leave the area loose (outside an atlas). */
    atlas?: Atlas | AtlasId;
}

interface CreateAtlasOptions {
    storage: "local" | "cloud";
}

/**
 * The map API for the current session. Each session has its own current
 * location; changes to persistent areas sync to the cloud in the background.
 */
interface Mapper {
    /**
     * Refresh every visible area from durable storage. Package entry points
     * should await this before a presence-based upsert that can run during
     * startup or after mapping ownership moves between sessions.
     */
    refreshAreas(): Promise<void>;
    /**
     * Create a new area and return its handle. Without an explicit `storage`
     * (or an `atlas` to inherit a tier from), the area is durable in the
     * default tier: cloud when signed in, local otherwise.
     */
    createArea(name: string, options?: CreateAreaOptions): Promise<Area>;
    /** List local and cloud atlases. */
    listAtlases(): Promise<Atlas[]>;
    /** Create a durable atlas in an explicit storage tier. */
    createAtlas(name: string, options: CreateAtlasOptions): Promise<Atlas>;
    /** Copy areas together, preserving links between members of the set. */
    copyAreas(areas: (Area | AreaId)[], destination: MapDestination): Promise<Area[]>;
    /** Move areas together. Cross-tier moves copy completely before deleting sources. */
    moveAreas(areas: (Area | AreaId)[], destination: MapDestination): Promise<Area[]>;
    copyArea(area: Area | AreaId, destination: MapDestination): Promise<Area>;
    moveArea(area: Area | AreaId, destination: MapDestination): Promise<Area>;
    /** Copy an atlas and all of its areas to another durable storage tier. */
    copyAtlas(atlas: Atlas | AtlasId, storage: "local" | "cloud"): Promise<Atlas>;
    /** Move an atlas and all of its areas to another durable storage tier. */
    moveAtlas(atlas: Atlas | AtlasId, storage: "local" | "cloud"): Promise<Atlas>;
    /** Set the current map location (the per-session "you are here" marker). */
    setCurrentLocation(areaId: AreaId, roomNumber?: RoomNumber): void;
    /** The current map location, or `undefined` if none is set. `room` is absent when the
     *  location names an area without a specific room. */
    getCurrentLocation(): { area: AreaId; room?: RoomNumber } | undefined;
    /** All active areas (areas marked inactive are excluded). */
    readonly areas: Area[];
    getAreaById(id: AreaId): Area;
    /**
     * Collect related writes to one area and submit them in the fewest practical ordered
     * envelopes. The whole callback is validated and durably staged before anything is
     * published, so a locally invalid batch submits nothing, even when oversized work is
     * split into several envelopes. Each envelope is atomic at the backend; acknowledged
     * envelopes are never rolled back, so if a later envelope fails after earlier ones
     * were accepted, the thrown `Error` carries the acknowledged prefix on its
     * `committedOperations` property (an `OperationId[]`). If the callback throws,
     * nothing is submitted.
     */
    mutateArea(
        area: Area | AreaId,
        callback: (mutation: AreaMutator) => void | Promise<void>,
        options?: MutateAreaOptions,
    ): Promise<OperationId[]>;
    /** The cheapest route between two rooms, as a list of `[areaId, roomNumber]`
     *  steps (each exit's `weight` is its cost). */
    getPathBetweenRooms(
        fromAreaId: AreaId,
        fromRoomNumber: RoomNumber,
        toAreaId: AreaId,
        toRoomNumber: RoomNumber,
    ): [AreaId, RoomNumber][];
    listRoomsByTitleAndDescription(title: string, description: string): (Room | undefined)[];
    listRoomsByTitleDescriptionAndVisibleExits(
        title: string,
        description: string,
        visibleExitDirections: string[],
    ): (Room | undefined)[];
    /** Rename an area after the backend acknowledges the change. */
    renameArea(area: Area | AreaId, name: string): Promise<void>;
    /** Delete an area and everything in it. */
    deleteArea(area: Area | AreaId): Promise<void>;
    setRoomTitle(area: Area | AreaId, room: Room | RoomNumber, title: string): Promise<OperationId | null>;
    setRoomDescription(area: Area | AreaId, room: Room | RoomNumber, description: string): Promise<OperationId | null>;
    /** Set a room's color to a CSS color string. */
    setRoomColor(area: Area | AreaId, room: Room | RoomNumber, color: string): Promise<OperationId | null>;
    setRoomLevel(area: Area | AreaId, room: Room | RoomNumber, level: number): Promise<OperationId | null>;
    setRoomX(area: Area | AreaId, room: Room | RoomNumber, x: number): Promise<OperationId | null>;
    setRoomY(area: Area | AreaId, room: Room | RoomNumber, y: number): Promise<OperationId | null>;
    /** Set a custom room property (string key/value). */
    setRoomProperty(area: Area | AreaId, room: Room | RoomNumber, name: string, value: string): Promise<OperationId | null>;
    /** Set a custom area property (string key/value); the write counterpart of `area.data(key)`.
     *  Pass an empty value to clear it. */
    setAreaProperty(area: Area | AreaId, name: string, value: string): Promise<OperationId | null>;
    /** Add a case-insensitive tag to a room (normalized to UPPERCASE; re-adding is a no-op). */
    addRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string): Promise<OperationId | null>;
    /** Remove a tag from a room (case-insensitive). */
    removeRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string): Promise<OperationId | null>;
    /**
     * The nearest reachable room carrying `tag` (case-insensitive) from `from`, by the same
     * weighted search as `getPathBetweenRooms` (the start room counts if it carries the tag),
     * or `undefined` if none is reachable. Path to it with `getPathBetweenRooms`.
     */
    findNearestRoomWithTag(from: Room, tag: string): Room | undefined;
    /**
     * The nearest reachable room that carries every tag in `all` and none of the
     * tags in `none` (all case-insensitive); `undefined` if no such room is
     * reachable or the filter is empty. Used by multi-tag speedwalks like
     * `\inn.peace` and `\!peace.guild`.
     */
    findNearestRoomWithTags(
        from: Room,
        filter: { all?: string[]; none?: string[] },
    ): Room | undefined;
    /**
     * The nearest reachable room belonging to `area` from `from`, by the same
     * weighted search as `getPathBetweenRooms` (`from` itself counts if it is
     * already in the area, and naming the area reaches it even when it is marked
     * inactive), or `undefined` if no room of the area is reachable. Path to it
     * with `getPathBetweenRooms`.
     */
    findNearestRoomInArea(from: Room, area: Area | AreaId): Room | undefined;
    /**
     * The room bound to a server-global room id (the room number games send
     * over GMCP or MSDP), or `undefined` if no loaded room carries it. When
     * the same id is bound in more than one area, one match is returned
     * (rooms in your own maps win over shared ones).
     */
    findRoomByExternalId(externalId: string): Room | undefined;
    /**
     * Reports whether a room with this server-global id is already mapped for a
     * different server. When it is, the player is offered the chance to show
     * that map here too, and this returns `true`, so a map drawn as you explore
     * knows the room is accounted for and need not be recreated. Returns `false`
     * when the id belongs to no other server's map.
     */
    rescueRoomByExternalId(externalId: string): boolean;
    /** Bind (or, with an empty string, clear) a room's server-global room id. */
    setRoomExternalId(area: Area | AreaId, room: Room | RoomNumber, externalId: string): Promise<OperationId | null>;
    /**
     * Create a room and return its new room number. The write is a
     * must-not-exist create: if the allocated number is taken by the time
     * the write lands (another client raced it in), it rejects with
     * `room_number_exists` instead of silently merging into that room.
     */
    createRoom(area: Area | AreaId, params: CreateRoomParams): Promise<RoomNumber>;
    /** Update multiple fields of a room in one cache update; only present fields change. */
    updateRoom(area: Area | AreaId, room: Room | RoomNumber, fields: UpdateRoomParams): Promise<OperationId | null>;
    /** Batch-update many rooms of one area in a single cache update. */
    updateRooms(area: Area | AreaId, updates: [RoomNumber, UpdateRoomParams][]): Promise<OperationId[]>;
    /** Create an exit on a room and return its new id. */
    createRoomExit(area: Area | AreaId, room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId>;
    /**
     * Update an existing exit. Resolves after backend acknowledgement; equal
     * updates resolve to `null` without sending a mutation.
     */
    setRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId, exit: ExitUpdates): Promise<OperationId | null>;
    /**
     * Merge `remove` into `keep` in one durable mutation. The kept room's
     * metadata wins; traversal is deduplicated and rewired. Resolves after
     * backend acknowledgement.
     */
    mergeRooms(area: Area | AreaId, keep: Room | RoomNumber, remove: Room | RoomNumber): Promise<OperationId | null>;
    /** Delete a room. */
    deleteRoom(area: Area | AreaId, room: Room | RoomNumber): Promise<OperationId | null>;
    /** Delete an exit from a room. */
    deleteRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId): Promise<OperationId | null>;
    /** Atomically create one Connection and its one or two traversals. */
    createLink(area: Area | AreaId, link: LinkCreateArgs): Promise<ConnectionId>;
    /** Update shared Connection geometry or appearance. */
    setConnection(area: Area | AreaId, connectionId: ConnectionId, updates: ConnectionUpdates): Promise<OperationId | null>;
    /** Split one traversal out of a bidirectional Connection. */
    unlinkRoomExit(area: Area | AreaId, exitId: ExitId): Promise<ConnectionId>;
    /** Merge reciprocal one-way Connections, preserving the first one's route. */
    pairConnections(area: Area | AreaId, keepConnectionId: ConnectionId, mergeConnectionId: ConnectionId): Promise<OperationId | null>;
    /** Delete a Connection and every member traversal. */
    deleteLink(area: Area | AreaId, connectionId: ConnectionId): Promise<OperationId | null>;
    /** Add a text label to an area and return its new id. */
    createLabel(area: Area | AreaId, label: LabelArgs): Promise<LabelId>;
    /** Add a graphical shape to an area and return its new id. */
    createShape(area: Area | AreaId, shape: ShapeArgs): Promise<ShapeId>;
    /** Delete a label from an area. */
    deleteLabel(area: Area | AreaId, labelId: LabelId): Promise<OperationId | null>;
    /** Delete a shape from an area. */
    deleteShape(area: Area | AreaId, shapeId: ShapeId): Promise<OperationId | null>;
    /** Update an existing label; only present fields change. */
    setLabel(area: Area | AreaId, labelId: LabelId, updates: LabelUpdates): Promise<OperationId | null>;
    /** Update an existing shape; only present fields change. */
    setShape(area: Area | AreaId, shapeId: ShapeId, updates: ShapeUpdates): Promise<OperationId | null>;
    /** Export an area as a portable {@link AreaJson}. Requires copy rights on
     *  the area. */
    exportArea(area: Area | AreaId): Promise<AreaJson>;
    /** Import exported areas as new **local** areas (fresh ids). Exits between
     *  areas in the set are relinked to the new copies; exits pointing
     *  **outside** the set are kept but left unlinked. Returns the new area
     *  ids. Prefer this one-call form for multi-area imports. */
    importAreas(areas: AreaJson[]): Promise<AreaId[]>;
    /** Import one exported area as a new local area; returns its id. */
    importArea(area: AreaJson): Promise<AreaId>;
    /** Import exported areas, skipping any whose **name** is already resident
     *  in the mapper. Shared maps, deactivated maps, and maps assigned to
     *  other server entries count too. Waits for the session's maps to finish
     *  loading first, so it is safe to call as a package starts, on every
     *  start, without creating duplicates. Returns the ids of the areas
     *  imported and the names skipped. */
    importAreasIfAbsent(areas: AreaJson[]): Promise<AreasImportedIfAbsent>;
}

/** The outcome of {@link Mapper.importAreasIfAbsent}. */
interface AreasImportedIfAbsent {
    /** Ids of the areas imported by this call. */
    readonly added: AreaId[];
    /** Names skipped because a resident map already has that name. */
    readonly skipped: string[];
}
