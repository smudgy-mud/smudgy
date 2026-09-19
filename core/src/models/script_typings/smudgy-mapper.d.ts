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
 * An area's identifier: its canonical lowercase hyphenated UUID string, the
 * same spelling the `map:room` event's `areaId` field delivers and MapView
 * apply-area scoping accepts.
 *
 * It is an ordinary string, so ordinary string rules apply — compare two ids
 * with `===`, use one as a `Map`/`Set` key, and put one through
 * `JSON.stringify`, which means ids can travel session-store writes, store
 * bindings and widget props like any other value.
 *
 * Treat the *contents* as opaque: take an id from one mapper call and pass it
 * back to another rather than parsing it.
 *
 * The type is branded, so an id smudgy hands you carries which *kind* of id it
 * is: assigning an {@link ExitId} to something declared `AreaId` is an error,
 * which is why storing ids in your own typed structures is worth doing. Calls
 * that take an id accept `AreaIdLike`, so a plain string read back from
 * JSON, a package parameter, or your own state needs no cast.
 *
 * @remarks An id used to be a 2-element `[hi, lo]` pair of the UUID's 64-bit
 * halves. That spelling is gone: mapper calls reject it. Code that compared
 * ids elementwise (`a[0] === b[0] && a[1] === b[1]`) or built a key from
 * `` `${id[0]}:${id[1]}` `` must switch to `a === b` and `id` respectively —
 * on a string those old forms compare *characters* and would report matches
 * that are not there.
 */
type AreaId = string & { readonly __id: "AreaId" };

/** An atlas (map folder) identifier, spelled like {@link AreaId}. */
type AtlasId = string & { readonly __id: "AtlasId" };

/**
 * What a call accepts wherever it takes an id: the branded id an API handed
 * you, or a plain string that spells one — from JSON, a package parameter,
 * your own state — with no cast. An id of a *different* kind is refused, so
 * passing an {@link ExitId} where an {@link AreaId} belongs is a type error
 * rather than a lookup that quietly finds nothing.
 */
type AreaIdLike = AreaId | (string & { readonly __id?: undefined });
/** What a call accepts wherever it takes an AtlasId; see {@link AreaIdLike}. */
type AtlasIdLike = AtlasId | (string & { readonly __id?: undefined });
/** What a call accepts wherever it takes an ExitId; see {@link AreaIdLike}. */
type ExitIdLike = ExitId | (string & { readonly __id?: undefined });
/** What a call accepts wherever it takes a ConnectionId; see {@link AreaIdLike}. */
type ConnectionIdLike = ConnectionId | (string & { readonly __id?: undefined });
/** What a call accepts wherever it takes an OperationId; see {@link AreaIdLike}. */
type OperationIdLike = OperationId | (string & { readonly __id?: undefined });
/** What a call accepts wherever it takes a LabelId; see {@link AreaIdLike}. */
type LabelIdLike = LabelId | (string & { readonly __id?: undefined });
/** What a call accepts wherever it takes a ShapeId; see {@link AreaIdLike}. */
type ShapeIdLike = ShapeId | (string & { readonly __id?: undefined });

/** Where a map is stored. Session maps disappear when the session closes. */
type MapStorage = "session" | "local" | "cloud";

/** A room number within an area (a 32-bit integer). */
type RoomNumber = number;

/** An exit's identifier, spelled like {@link AreaId}. */
type ExitId = string & { readonly __id: "ExitId" };
/** A Connection's identifier, spelled like {@link AreaId}. */
type ConnectionId = string & { readonly __id: "ConnectionId" };
/** A queued mapper mutation's identifier, spelled like {@link AreaId}. */
type OperationId = string & { readonly __id: "OperationId" };

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

/** A label's identifier, spelled like {@link AreaId}. */
type LabelId = string & { readonly __id: "LabelId" };
/** A shape's identifier, spelled like {@link AreaId}. */
type ShapeId = string & { readonly __id: "ShapeId" };

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
    /** The shared Connection this traversal belongs to. */
    readonly connection_id: ConnectionId;
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
    to_area_id?: AreaIdLike;
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
    to_area_id?: AreaIdLike;
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
    setRoomExit(room: Room | RoomNumber, exitId: ExitIdLike, exit: ExitUpdates): Promise<void>;
    deleteRoom(room: Room | RoomNumber): Promise<void>;
    deleteRoomExit(room: Room | RoomNumber, exitId: ExitIdLike): Promise<void>;
    createLink(link: LinkCreateArgs): Promise<ConnectionId>;
    setConnection(connectionId: ConnectionIdLike, updates: ConnectionUpdates): Promise<void>;
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
     * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
     * `id` is that string now — this is an alias for it.
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
    /**
     * This area's rooms whose `name` property is exactly `value`, as
     * `room.data(name)` reads it. One indexed lookup, however many rooms the
     * area has. An area answers for itself even when you have turned its map
     * off — naming it is asking for it.
     */
    findRoomsByProperty(name: string, value: string): Room[];
    /**
     * This area's rooms carrying a property called `name`, whatever its value —
     * "which rooms did I write this on at all".
     */
    findRoomsWithProperty(name: string): Room[];
    /**
     * This area's rooms carrying `tag` (case-insensitive), in no particular
     * order.
     */
    findRoomsWithTag(tag: string): Room[];
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
    atlas?: Atlas | AtlasIdLike;
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
    atlas?: Atlas | AtlasIdLike;
}

interface CreateAtlasOptions {
    storage: "local" | "cloud";
}

/**
 * Maps for the current session. Each session has its own current location.
 * Sessions sharing local maps see each other's changes automatically.
 * Cloud changes prompt sessions using the same service and account to refresh.
 */
interface Mapper {
    /**
     * Wait until this session's maps are ready for startup lookups or updates.
     * Loads maps if startup has not done so; later calls use loaded maps.
     * Requires `mapper:read`.
     */
    ready(): Promise<void>;
    /**
     * Reload maps, including changes made to local files outside the app.
     * Updated maps are available to this session when the call resolves.
     * Use `ready()` for startup checks. Requires `mapper:read`.
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
    copyAreas(areas: (Area | AreaIdLike)[], destination: MapDestination): Promise<Area[]>;
    /** Move areas together. Cross-tier moves copy completely before deleting sources. */
    moveAreas(areas: (Area | AreaIdLike)[], destination: MapDestination): Promise<Area[]>;
    copyArea(area: Area | AreaIdLike, destination: MapDestination): Promise<Area>;
    moveArea(area: Area | AreaIdLike, destination: MapDestination): Promise<Area>;
    /** Copy an atlas and all of its areas to another durable storage tier. */
    copyAtlas(atlas: Atlas | AtlasIdLike, storage: "local" | "cloud"): Promise<Atlas>;
    /** Move an atlas and all of its areas to another durable storage tier. */
    moveAtlas(atlas: Atlas | AtlasIdLike, storage: "local" | "cloud"): Promise<Atlas>;
    /** Set the current map location (the per-session "you are here" marker). */
    setCurrentLocation(areaId: AreaIdLike, roomNumber?: RoomNumber): void;
    /** The current map location, or `undefined` if none is set. `room` is absent when the
     *  location names an area without a specific room. */
    getCurrentLocation(): { area: AreaId; room?: RoomNumber } | undefined;
    /** All active areas (areas marked inactive are excluded). */
    readonly areas: Area[];
    getAreaById(id: AreaIdLike): Area;
    /**
     * Collect related writes to one area. Callback and validation failures submit
     * nothing and pass through unchanged. Large batches may save in several ordered
     * steps; each step is atomic, and saved steps are not rolled back.
     * Resolves once all steps are saved, returning their operation IDs in order.
     * Save failures throw `MutateAreaError` with the IDs confirmed saved so far.
     * Other edits may still be pending; retrying the whole callback can duplicate work.
     *
     * @example
     * ```ts
     * import { mapper, MutateAreaError } from "smudgy:core";
     * try {
     *     await mapper.mutateArea(area, m => m.setRoomTitle(1, "Town square"));
     * } catch (error) {
     *     if (error instanceof MutateAreaError) {
     *         console.log("Saved operation IDs:", error.committedOperations);
     *     }
     *     throw error;
     * }
     * ```
     */
    mutateArea(
        area: Area | AreaIdLike,
        callback: (mutation: AreaMutator) => void | Promise<void>,
        options?: MutateAreaOptions,
    ): Promise<OperationId[]>;
    /** The cheapest route between two rooms, as a list of `[areaId, roomNumber]`
     *  steps (each exit's `weight` is its cost). */
    getPathBetweenRooms(
        fromAreaId: AreaIdLike,
        fromRoomNumber: RoomNumber,
        toAreaId: AreaIdLike,
        toRoomNumber: RoomNumber,
    ): [AreaId, RoomNumber][];
    listRoomsByTitleAndDescription(title: string, description: string): (Room | undefined)[];
    listRoomsByTitleDescriptionAndVisibleExits(
        title: string,
        description: string,
        visibleExitDirections: string[],
    ): (Room | undefined)[];
    /**
     * Every room on the map whose `name` property is exactly `value`, as
     * `room.data(name)` reads it. Name and value both match exactly. The map
     * keeps an index for this, so it costs one lookup however large the map is;
     * there is no reason to walk the areas yourself. Rooms of maps you have
     * turned off are left out.
     */
    findRoomsByProperty(name: string, value: string): Room[];
    /**
     * Every room on the map carrying a property called `name`, whatever its
     * value — "which rooms did I write this on at all". Indexed like
     * `findRoomsByProperty`. Rooms of maps you have turned off are left out.
     */
    findRoomsWithProperty(name: string): Room[];
    /**
     * Every room on the map carrying `tag` (case-insensitive), in no particular
     * order. Reach for `findNearestRoomWithTag` when you want the closest one
     * instead: that walks the map, this reads an index. Rooms of maps you have
     * turned off are left out.
     */
    findRoomsWithTag(tag: string): Room[];
    /**
     * Every area whose `name` property is exactly `value`, as `area.data(name)`
     * reads it. Indexed like the room lookups. Maps you have turned off are
     * left out.
     */
    findAreasByProperty(name: string, value: string): Area[];
    /**
     * Every area carrying a property called `name`, whatever its value. Maps
     * you have turned off are left out.
     */
    findAreasWithProperty(name: string): Area[];
    /** Rename an area after the backend acknowledges the change. */
    renameArea(area: Area | AreaIdLike, name: string): Promise<void>;
    /** Delete an area and everything in it. */
    deleteArea(area: Area | AreaIdLike): Promise<void>;
    setRoomTitle(area: Area | AreaIdLike, room: Room | RoomNumber, title: string): Promise<OperationId | null>;
    setRoomDescription(area: Area | AreaIdLike, room: Room | RoomNumber, description: string): Promise<OperationId | null>;
    /** Set a room's color to a CSS color string. */
    setRoomColor(area: Area | AreaIdLike, room: Room | RoomNumber, color: string): Promise<OperationId | null>;
    setRoomLevel(area: Area | AreaIdLike, room: Room | RoomNumber, level: number): Promise<OperationId | null>;
    setRoomX(area: Area | AreaIdLike, room: Room | RoomNumber, x: number): Promise<OperationId | null>;
    setRoomY(area: Area | AreaIdLike, room: Room | RoomNumber, y: number): Promise<OperationId | null>;
    /** Set a custom room property (string key/value). */
    setRoomProperty(area: Area | AreaIdLike, room: Room | RoomNumber, name: string, value: string): Promise<OperationId | null>;
    /** Set a custom area property (string key/value); the write counterpart of `area.data(key)`.
     *  Pass an empty value to clear it. */
    setAreaProperty(area: Area | AreaIdLike, name: string, value: string): Promise<OperationId | null>;
    /** Add a case-insensitive tag to a room (normalized to UPPERCASE; re-adding is a no-op). */
    addRoomTag(area: Area | AreaIdLike, room: Room | RoomNumber, tag: string): Promise<OperationId | null>;
    /** Remove a tag from a room (case-insensitive). */
    removeRoomTag(area: Area | AreaIdLike, room: Room | RoomNumber, tag: string): Promise<OperationId | null>;
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
    findNearestRoomInArea(from: Room, area: Area | AreaIdLike): Room | undefined;
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
    setRoomExternalId(area: Area | AreaIdLike, room: Room | RoomNumber, externalId: string): Promise<OperationId | null>;
    /**
     * Create a room and return its new room number. The write is a
     * must-not-exist create: if the allocated number is taken by the time
     * the write lands (another client raced it in), it rejects with
     * `room_number_exists` instead of silently merging into that room.
     */
    createRoom(area: Area | AreaIdLike, params: CreateRoomParams): Promise<RoomNumber>;
    /** Update multiple fields of a room in one cache update; only present fields change. */
    updateRoom(area: Area | AreaIdLike, room: Room | RoomNumber, fields: UpdateRoomParams): Promise<OperationId | null>;
    /** Batch-update many rooms of one area in a single cache update. */
    updateRooms(area: Area | AreaIdLike, updates: [RoomNumber, UpdateRoomParams][]): Promise<OperationId[]>;
    /** Create an exit on a room and return its new id. */
    createRoomExit(area: Area | AreaIdLike, room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId>;
    /**
     * Update an existing exit. Resolves after backend acknowledgement; equal
     * updates resolve to `null` without sending a mutation.
     */
    setRoomExit(area: Area | AreaIdLike, room: Room | RoomNumber, exitId: ExitIdLike, exit: ExitUpdates): Promise<OperationId | null>;
    /**
     * Merge `remove` into `keep` in one durable mutation. The kept room's
     * metadata wins; traversal is deduplicated and rewired. Resolves after
     * backend acknowledgement.
     */
    mergeRooms(area: Area | AreaIdLike, keep: Room | RoomNumber, remove: Room | RoomNumber): Promise<OperationId | null>;
    /**
     * Fold areas into `into` in one operation. Whole sources move their
     * rooms, exits, labels, shapes and connections, then are deleted. A source
     * with `rooms` keeps its area, labels, shapes and area properties, even if all
     * its rooms move. The destination keeps its own area metadata and properties;
     * whole sources' area metadata and properties are discarded.
     * Offsets apply only to moved content. Exits in the same storage tier follow
     * moved rooms. Room numbers stay where free and unreserved, or are reassigned.
     * Resolves with each moved room's old address and new number, with the updated
     * maps available to this session. Invalid offsets or exhausted room numbers
     * leave maps unchanged (`merge_areas_invalid_translation` or
     * `merge_areas_room_numbers_exhausted`). After an interrupted save, call
     * `refreshAreas()` before retrying: an error does not guarantee that the maps
     * were left unchanged.
     * All touched maps must use the same storage tier: local or session. Cloud maps
     * and links from a different tier are refused. Refusal codes:
     * - `merge_areas_no_sources`: no source areas were provided.
     * - `merge_areas_same_area`: a source repeats or is the destination.
     * - `merge_areas_no_rooms`: an explicit room list is empty.
     * - `merge_areas_invalid_rooms`: a room list is not an array of 32-bit integers.
     * - `merge_areas_room_not_found`: a selected room is missing.
     * - `merge_areas_mixed_tiers`: affected maps use different storage tiers.
     * - `merge_areas_unsupported_storage`: cloud merges are unsupported.
     * - `merge_requires_full_projection`: an affected map hides secret content.
     * - `merge_areas_busy`: pending edits or another operation prevent the merge.
     * - `merge_areas_source_changed`: maps or incoming links changed while waiting.
     * - `merge_areas_invalid_translation`: offsets are invalid or coordinates overflow.
     * - `merge_areas_room_numbers_exhausted`: no representable room number remains.
     * Missing maps, invalid connections and storage failures also reject the call.
     * Requires `mapper:write`.
     */
    mergeAreas(into: Area | AreaIdLike, sources: (Area | AreaIdLike | MergeAreaSource)[]): Promise<MergedRoom[]>;
    /** Delete a room. */
    deleteRoom(area: Area | AreaIdLike, room: Room | RoomNumber): Promise<OperationId | null>;
    /** Delete an exit from a room. */
    deleteRoomExit(area: Area | AreaIdLike, room: Room | RoomNumber, exitId: ExitIdLike): Promise<OperationId | null>;
    /** Atomically create one Connection and its one or two traversals. */
    createLink(area: Area | AreaIdLike, link: LinkCreateArgs): Promise<ConnectionId>;
    /** Update shared Connection geometry or appearance. */
    setConnection(area: Area | AreaIdLike, connectionId: ConnectionIdLike, updates: ConnectionUpdates): Promise<OperationId | null>;
    /** Split one traversal out of a bidirectional Connection. */
    unlinkRoomExit(area: Area | AreaIdLike, exitId: ExitIdLike): Promise<ConnectionId>;
    /** Merge reciprocal one-way Connections, preserving the first one's route. */
    pairConnections(area: Area | AreaIdLike, keepConnectionId: ConnectionIdLike, mergeConnectionId: ConnectionIdLike): Promise<OperationId | null>;
    /** Delete a Connection and every member traversal. */
    deleteLink(area: Area | AreaIdLike, connectionId: ConnectionIdLike): Promise<OperationId | null>;
    /** Add a text label to an area and return its new id. */
    createLabel(area: Area | AreaIdLike, label: LabelArgs): Promise<LabelId>;
    /** Add a graphical shape to an area and return its new id. */
    createShape(area: Area | AreaIdLike, shape: ShapeArgs): Promise<ShapeId>;
    /** Delete a label from an area. */
    deleteLabel(area: Area | AreaIdLike, labelId: LabelIdLike): Promise<OperationId | null>;
    /** Delete a shape from an area. */
    deleteShape(area: Area | AreaIdLike, shapeId: ShapeIdLike): Promise<OperationId | null>;
    /** Update an existing label; only present fields change. */
    setLabel(area: Area | AreaIdLike, labelId: LabelIdLike, updates: LabelUpdates): Promise<OperationId | null>;
    /** Update an existing shape; only present fields change. */
    setShape(area: Area | AreaIdLike, shapeId: ShapeIdLike, updates: ShapeUpdates): Promise<OperationId | null>;
    /** Export an area as a portable {@link AreaJson}. Requires copy rights on
     *  the area. */
    exportArea(area: Area | AreaIdLike): Promise<AreaJson>;
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

/** One source of {@link Mapper.mergeAreas}: an area, optionally just some of
 * its rooms, and the rigid offset applied to what moves before it lands in
 * the destination. */
interface MergeAreaSource {
    /** The area to draw from. */
    area: Area | AreaIdLike;
    /** Move these rooms; keep the source, labels, shapes and area properties,
     * even if all rooms are listed. Must be nonempty. Omit to move everything
     * and delete the source. */
    rooms?: RoomNumber[];
    /** Added only to moved rooms, labels, shapes and route points. Omitted axes
     * are 0. Coordinates and their results must be finite; levels must fit signed 32-bit integers. */
    translate?: { x?: number; y?: number; level?: number };
}

/** One room moved by {@link Mapper.mergeAreas}. */
interface MergedRoom {
    /** The room's old address. Its source is deleted only when `rooms` was omitted. */
    readonly from: { area: AreaId; room: RoomNumber };
    /** The room's number in the destination area now. */
    readonly to: RoomNumber;
}
