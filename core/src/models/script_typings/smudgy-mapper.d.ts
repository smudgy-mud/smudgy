// =============================================================================
//  smudgy mapper -- TypeScript declarations  (GENERATED -- DO NOT EDIT)
// =============================================================================
//  smudgy writes and overwrites this file every time a session starts. It teaches
//  VS Code (and any TypeScript-aware editor) about the `mapper` API.
//
//  `mapper` is reachable two ways, both typed by the declarations below:
//    - `import { mapper } from "smudgy:core"` (a named export) and the default
//      export's `.mapper` member -- both typed `Mapper` by smudgy-core.d.ts, which
//      references the global `Mapper` interface declared here;
//    - the ambient global `mapper` (and `Area`), installed by the mapper runtime.
//      The `mapper` value global is deprecated (removal tracked in TODO.md);
//      import it from `smudgy:core` instead.
//
//  These are GLOBAL ambient declarations (no `declare module`), so the names
//  (`Mapper`, `Area`, `Room`, `Exit`, `AreaId`, ...) are visible both to bare
//  `mapper.*` usage and to smudgy-core.d.ts's `mapper` member.
//
//  Edits here are lost on the next launch.
// =============================================================================

// ---- Identifiers ------------------------------------------------------------

/**
 * An area's identifier. Treat it as **opaque**: take it from one mapper call and
 * pass it back to another, unchanged. **Careful:** this is not the same as the
 * UUID **string** the `map:room` event delivers; the two are not
 * interchangeable.
 */
type AreaId = readonly [number, number];

/** A room number within an area (a 32-bit integer). */
type RoomNumber = number;

/** An exit's identifier: a 2-element `[hi, lo]` pair, like {@link AreaId}. Opaque. */
type ExitId = readonly [number, number];
/** A Connection's identifier, opaque like {@link ExitId}. */
type ConnectionId = readonly [number, number];

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
    update(fields: UpdateRoomParams): void;
    toString(): string;
}

// ---- Areas ------------------------------------------------------------------

/**
 * A map area. You get areas from the mapper (`mapper.areas`,
 * `mapper.getAreaById`), never by constructing one; the global `Area` class
 * exists so checks like `area instanceof Area` work.
 */
declare class Area {
    private constructor();
    readonly id: AreaId;
    readonly name: string;
    readonly room_numbers: RoomNumber[];
    /**
     * Whether this is a session map: it lives only for this session and is
     * discarded when the session closes. Save it with `mapper.exportArea` +
     * `mapper.importAreas` to keep it.
     */
    readonly isEphemeral: boolean;
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
     * Create a session map: it lives only for this session, is never saved
     * or synced, and is discarded when the session closes. Use this for maps
     * built automatically from server data.
     */
    ephemeral?: boolean;
}

/**
 * The map API for the current session. Each session has its own current
 * location; changes to persistent areas sync to the cloud in the background.
 */
interface Mapper {
    /** Create a new area and return its handle. */
    createArea(name: string, options?: CreateAreaOptions): Promise<Area>;
    /** Set the current map location (the per-session "you are here" marker). */
    setCurrentLocation(areaId: AreaId, roomNumber?: RoomNumber): void;
    /** The current map location, or `undefined` if none is set. `room` is absent when the
     *  location names an area without a specific room. */
    getCurrentLocation(): { area: AreaId; room?: RoomNumber } | undefined;
    /** All active areas (areas marked inactive are excluded). */
    readonly areas: Area[];
    getAreaById(id: AreaId): Area;
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
    renameArea(area: Area | AreaId, name: string): void;
    /** Delete an area and everything in it. */
    deleteArea(area: Area | AreaId): void;
    setRoomTitle(area: Area | AreaId, room: Room | RoomNumber, title: string): void;
    setRoomDescription(area: Area | AreaId, room: Room | RoomNumber, description: string): void;
    /** Set a room's color to a CSS color string. */
    setRoomColor(area: Area | AreaId, room: Room | RoomNumber, color: string): void;
    setRoomLevel(area: Area | AreaId, room: Room | RoomNumber, level: number): void;
    setRoomX(area: Area | AreaId, room: Room | RoomNumber, x: number): void;
    setRoomY(area: Area | AreaId, room: Room | RoomNumber, y: number): void;
    /** Set a custom room property (string key/value). */
    setRoomProperty(area: Area | AreaId, room: Room | RoomNumber, name: string, value: string): void;
    /** Set a custom area property (string key/value); the write counterpart of `area.data(key)`.
     *  Pass an empty value to clear it. */
    setAreaProperty(area: Area | AreaId, name: string, value: string): void;
    /** Add a case-insensitive tag to a room (normalized to UPPERCASE; re-adding is a no-op). */
    addRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string): void;
    /** Remove a tag from a room (case-insensitive). */
    removeRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string): void;
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
    setRoomExternalId(area: Area | AreaId, room: Room | RoomNumber, externalId: string): void;
    /** Create a room and return its new room number. */
    createRoom(area: Area | AreaId, params: CreateRoomParams): RoomNumber;
    /** Update multiple fields of a room in one cache update; only present fields change. */
    updateRoom(area: Area | AreaId, room: Room | RoomNumber, fields: UpdateRoomParams): void;
    /** Batch-update many rooms of one area in a single cache update. */
    updateRooms(area: Area | AreaId, updates: [RoomNumber, UpdateRoomParams][]): void;
    /** Create an exit on a room and return its new id. */
    createRoomExit(area: Area | AreaId, room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId>;
    /** Update an existing exit. Returns nothing. */
    setRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId, exit: ExitUpdates): void;
    /** Delete a room. */
    deleteRoom(area: Area | AreaId, room: Room | RoomNumber): void;
    /** Delete an exit from a room. */
    deleteRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId): void;
    /** Atomically create one Connection and its one or two traversals. */
    createLink(area: Area | AreaId, link: LinkCreateArgs): ConnectionId;
    /** Update shared Connection geometry or appearance. */
    setConnection(area: Area | AreaId, connectionId: ConnectionId, updates: ConnectionUpdates): void;
    /** Split one traversal out of a bidirectional Connection. */
    unlinkRoomExit(area: Area | AreaId, exitId: ExitId): ConnectionId;
    /** Merge reciprocal one-way Connections, preserving the first one's route. */
    pairConnections(area: Area | AreaId, keepConnectionId: ConnectionId, mergeConnectionId: ConnectionId): void;
    /** Delete a Connection and every member traversal. */
    deleteLink(area: Area | AreaId, connectionId: ConnectionId): void;
    /** Add a text label to an area and return its new id. */
    createLabel(area: Area | AreaId, label: LabelArgs): Promise<LabelId>;
    /** Add a graphical shape to an area and return its new id. */
    createShape(area: Area | AreaId, shape: ShapeArgs): Promise<ShapeId>;
    /** Delete a label from an area. */
    deleteLabel(area: Area | AreaId, labelId: LabelId): void;
    /** Delete a shape from an area. */
    deleteShape(area: Area | AreaId, shapeId: ShapeId): void;
    /** Update an existing label; only present fields change. */
    setLabel(area: Area | AreaId, labelId: LabelId, updates: LabelUpdates): void;
    /** Update an existing shape; only present fields change. */
    setShape(area: Area | AreaId, shapeId: ShapeId, updates: ShapeUpdates): void;
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

/**
 * The current session's map API, as a global. Deprecated: import `mapper`
 * from `smudgy:core` instead.
 *
 * @deprecated Import `mapper` from `smudgy:core`.
 */
declare const mapper: Mapper;
