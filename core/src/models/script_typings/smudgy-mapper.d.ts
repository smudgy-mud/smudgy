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

// ---- Names ------------------------------------------------------------------
//
// Every name in this API is camelCase.
//
// @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
// The snake_case names earlier versions used keep working until then, untyped: what the API
// returns answers to them (`room.room_number`, `room.area_id`, `area.room_numbers`,
// `area.next_room_number`, every `Exit` field such as `to_room_number` and `is_hidden`, and the
// `Label`, `Shape`, `Connection` and `ConnectionEndpoint` fields such as `background_color`,
// `shape_type`, `endpoint_a` and `port_offset`), and what it takes accepts them in the same
// objects (`ExitArgs`, `LabelArgs`, `ShapeArgs`, `LinkCreateArgs`, the updates). The first one
// a script uses draws a notice, once. A refusal's message carries its code's old spelling
// beside the new one (`roomNumberExists; formerly room_number_exists`), so a script matching
// the old code keeps matching.

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

/**
 * A Secret's identifier, spelled like {@link AreaId}. A Secret's own rooms read as
 * an area of their own under the same id ({@link Secret.area}).
 */
type SecretId = string & { readonly __id: "SecretId" };
/** What a call accepts wherever it takes a SecretId; see {@link AreaIdLike}. */
type SecretIdLike = SecretId | (string & { readonly __id?: undefined });

// ---- Places -----------------------------------------------------------------
//
// A cloud map's content lives in several **places**: the map's own content
// (`"map"`), each of its Secrets you can read (shared with their own readers), and
// your Private additions (`"private"`, only you see them). Each place keeps its own
// properties, tags and exits for the map's rooms, so map room 12 can carry the map's
// `notes` and a Secret's `notes` side by side; nothing merges two places' values. A
// Secret or your Private additions also keep rooms of their own, which live in that
// place.
//
// Each place numbers its own rooms, starting at 1: map room 3 and a Secret's own room
// 3 are different rooms, told apart by the area they belong to (a Secret's own rooms
// read as an area of their own). `area.nextRoomNumber` and `createRoom` number within
// the area's place alone. A room moved between places keeps its number unless the
// place it moves to already uses it; then it takes that place's next number. Room
// handles are snapshots and keep naming where the room was: the current location
// (and a location still being set) moves with the room, and `map:merged` says where
// each moved room went, so look the room up again from there.
//
// Every call that names no place works where the handle lives, as it always has: a
// map room's data is the map's, a Secret's own room's data is that Secret's. Reach
// another place through a view, `room.in(place)` or `area.in(place)`. A few reads
// cover every place at once: `room.tags` and `room.hasTag`, `room.combinedData` and
// `room.combinedTags`, and the searches (`findRoomsWithTag` and the rest, and
// `findNearestRoomWithTag(s)`), which take `{ in: place }` to narrow to one place.
// A place can be named by its keyword, a Secret handle, or a Secret's id, which can
// never be mistaken for a keyword. A Secret that does not exist and one you cannot
// read both throw the same "Secret not found".
//
// An installed package reaches places other than the map only with the `secrets`
// capability in its manifest's `permissions.smudgy`: `"read"`, `"write"` (implies
// read; writes also need `mapper: ["write"]`) and `"manage"` (implies read; creating,
// updating and deleting Secrets). The capability covers Secrets and Private additions
// alike.
//
// Without `secrets`, Secrets and Private additions are invisible: nothing a package
// sees differs from a map without them. Places, views, combined reads and searches
// cover the map alone; `room.exits` leaves out Secret/Private-owned doors and
// `area.connections` leaves out links anchored on Secret/Private rooms. Map-owned
// exits into an unreadable destination retain their content with an unknown target.
// A write naming a hidden attachment by id
// (`setRoomExit`, `deleteRoomExit`, `unlinkRoomExit`, `setConnection`,
// `pairConnections`, `deleteLink`, `mutateArea`) answers as an id naming nothing does; routes and the
// nearest-room searches never pass through them; a Secret's or Private additions' own
// area, and every room in it, answers every lookup and every write exactly as an id
// naming nothing does; and while the player stands in one of their rooms, the current
// location reads, and `map:room` announces, the map with no room, as an unmapped room
// of the map would; a click on one of their rooms in a map view never fires
// `map:click`. Naming a Secret or `"private"` in a view or a search throws the
// same capability error for every name, existing or not, before anything is looked up.

/** Who holds ownership authority over a Secret: the map's owner (`"owner"`), its
 *  recorded clan members (`"members"`), or its clan (`"clan"`). */
type SecretOwnership = "owner" | "members" | "clan";

/**
 * What you may do with a Secret: `"read"` it; `"add"`, `"edit"` and `"remove"` its
 * content; `"manageAccess"` (share it); `"copy"` (take it along when copying the
 * map); `"rename"` it (its name and color); `"delete"` it; `"manageOwnership"`.
 */
type SecretAction =
    | "read"
    | "add"
    | "edit"
    | "remove"
    | "manageAccess"
    | "copy"
    | "rename"
    | "delete"
    | "manageOwnership";

/** A new name, a new color, or both, for {@link Secret.update}. */
interface SecretUpdates {
    name?: string;
    /** `#rrggbb`, or `null` to let the palette pick. */
    color?: string | null;
}

/** A new Secret, for {@link SecretRegistry.create}. */
interface CreateSecretOptions {
    name: string;
    /** `#rrggbb`; omitted or `null`, the palette picks. */
    color?: string | null;
    /** Who owns it. Omitted or `"owner"`: an owner Secret, on a map you own.
     *  `"members"`: a Member-owned Clan Secret with you as its first owner (needs
     *  the clan's `secret.create_member_owned` on the map). `"clan"`: a Clan-owned
     *  Clan Secret, which you start as a Contributor of (needs `secret.create_clan_owned`). */
    ownership?: "owner" | "members" | "clan";
    /** The clan a Clan Secret belongs to: required on a user's map filed in a clan,
     *  and on a clan's own map that clan (the default). */
    clanId?: string;
}

/**
 * One of a cloud map's Secrets, as you can see it: never its owners, grants or
 * audience. A handle shows what was served the last time the Secret was read, and
 * every read of the same Secret refreshes the same handle, so two handles for one
 * Secret are `===`.
 */
interface Secret {
    readonly id: SecretId;
    readonly name: string;
    /** Its chosen color, `#rrggbb`, or `null` when the palette picks one. */
    readonly color: string | null;
    readonly ownership: SecretOwnership;
    /** The clan a Clan Secret (`"members"` or `"clan"` ownership) belongs to. */
    readonly clanId?: string;
    readonly actions: readonly SecretAction[];
    /** The Secret's own area: its own rooms, labels and shapes, under its id. */
    readonly area: Area;
    /** The map the Secret belongs to. */
    readonly mapId: AreaId;
    /** Rename and recolor it in one request (needs its `"rename"` action). Resolves
     *  once the server has it; the handle shows the result. */
    update(changes: SecretUpdates): Promise<void>;
    /** Delete it and everything in it (needs its `"delete"` action). */
    delete(): Promise<void>;
    toString(): string;
}

/** A map's Secrets ({@link Area.secrets}). Empty on a Secret's own area and on local
 *  and session maps. */
interface SecretRegistry {
    /** The map's Secrets you can read, in place order. */
    list(): Secret[];
    /** The Secret with this id on this map. One that does not exist, one you cannot
     *  read and one on another map all throw the same "Secret not found". */
    get(id: SecretIdLike): Secret;
    /** Whether {@link SecretRegistry.get} would find it; always `false` without the
     *  `secrets` capability. */
    exists(id: SecretIdLike): boolean;
    /** Create a Secret on this cloud map in one request, and resolve once the server
     *  has it: an owner Secret on a map you own, or with `ownership` a Clan Secret
     *  where a clan lets you make one. An installed package needs
     *  `secrets: ["manage"]`. */
    create(options: CreateSecretOptions): Promise<Secret>;
}

/** One of a map's places: its own content, your Private additions, or a Secret. */
type MapPlace = "map" | "private" | Secret;

/** A place as calls take it: a {@link MapPlace}, or a Secret's id. */
type MapPlaceLike = MapPlace | SecretIdLike;

/** Narrows a search to one place. Omitted, a search covers every place you read. */
interface PlaceOptions {
    in?: MapPlaceLike;
}

/** One place's value for a room's property ({@link Room.combinedData}). */
interface RoomPlaceData {
    readonly source: MapPlace;
    readonly data: string;
}

/** One place's value for one of a room's properties ({@link Room.combinedData}). */
interface RoomPlaceEntry extends RoomPlaceData {
    readonly key: string;
}

/** One place's tag on a room ({@link Room.combinedTags}). */
interface RoomPlaceTag {
    readonly source: MapPlace;
    readonly tag: string;
}

/**
 * One place's data on one room ({@link Room.in}). Writes go to that place alone:
 * on a map room, a Secret's or your Private additions' data for the room never
 * touches the map's, and the reverse.
 */
interface RoomView {
    readonly place: MapPlace;
    /** The value this place keeps for `key` on the room, or `undefined`. */
    data(key: string): string | undefined;
    /** This place's tags for the room, UPPERCASE and sorted. */
    readonly tags: string[];
    /** Whether this place tags the room with `tag` (case-insensitive). */
    hasTag(tag: string): boolean;
    /** The exits this place keeps on the room. */
    readonly exits: Exit[];
    setData(key: string, value: string): Promise<OperationId | null>;
    deleteData(key: string): Promise<OperationId | null>;
    /** Add a tag (normalized to UPPERCASE); adding one the place already keeps is a
     *  no-op that resolves to `null`. */
    addTag(tag: string): Promise<OperationId | null>;
    /** Remove a tag; removing one the place does not keep resolves to `null`. */
    removeTag(tag: string): Promise<OperationId | null>;
    /** Create an exit owned by this place on the readable room. Reading the
     *  destination is required; writing to its source is not. */
    createExit(exit: ExitArgs): Promise<ExitId>;
}

/** One place's data on one map ({@link Area.in}), and the searches narrowed to it. */
interface AreaView {
    readonly place: MapPlace;
    /** This place's own property `key` on the map (keyed by no room), or `undefined`. */
    data(key: string): string | undefined;
    setData(key: string, value: string): Promise<OperationId | null>;
    deleteData(key: string): Promise<OperationId | null>;
    findRoomsByProperty(name: string, value: string): Room[];
    findRoomsWithProperty(name: string): Room[];
    findRoomsWithTag(tag: string): Room[];
}

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
    readonly horizontalAlignment: LabelHorizontalAlign;
    readonly verticalAlignment: LabelVerticalAlign;
    readonly text: string;
    /** A CSS color string. */
    readonly color: string;
    /** A CSS color string for the background (`""` for none). */
    readonly backgroundColor: string;
    readonly fontSize: number;
    readonly fontWeight: number;
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
    horizontalAlignment?: LabelHorizontalAlign;
    verticalAlignment?: LabelVerticalAlign;
    /** A CSS color string for the text (default `"#ffffff"`). */
    color?: string;
    /** A CSS color string for the background; omit for none. */
    backgroundColor?: string;
    /** Text size in px (default 16). */
    fontSize?: number;
    /** Text weight (default 400). */
    fontWeight?: number;
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
    horizontalAlignment?: LabelHorizontalAlign;
    verticalAlignment?: LabelVerticalAlign;
    /** A CSS color string for the text. */
    color?: string;
    /** A CSS color string for the background. */
    backgroundColor?: string;
    fontSize?: number;
    fontWeight?: number;
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
    readonly backgroundColor: string | null;
    /** A CSS color string, or `null` for none. */
    readonly strokeColor: string | null;
    readonly shapeType: ShapeKind;
    readonly borderRadius: number;
    readonly strokeWidth: number;
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
    backgroundColor?: string;
    /** A CSS stroke color; omit for none. */
    strokeColor?: string;
    /** Shape kind (default `"Rectangle"`). */
    shapeType?: ShapeKind;
    /** Corner radius (default 0). */
    borderRadius?: number;
    /** Stroke width in px. */
    strokeWidth?: number;
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
    backgroundColor?: string;
    /** A CSS stroke color. */
    strokeColor?: string;
    shapeType?: ShapeKind;
    borderRadius?: number;
    strokeWidth?: number;
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
    /**
     * The one place the whole callback writes. Omitted, it writes the area's own
     * place. Named on a map, the callback writes that place's properties, tags and
     * exits on the map's rooms and its own area properties; creating, changing or
     * deleting rooms throws, because another place cannot change the map's rooms —
     * edit a Secret's own rooms through {@link Secret.area}.
     */
    in?: MapPlaceLike;
}

/**
 * One exit read back from a room (`room.exits`). Optional links are present but `null`
 * when unset (not omitted).
 */
interface Exit {
    readonly id: ExitId;
    /** The shared Connection this traversal belongs to. */
    readonly connectionId: ConnectionId;
    readonly fromDirection: ExitDirection;
    readonly fromAreaId: AreaId;
    readonly fromRoomNumber: RoomNumber;
    readonly toDirection: ExitDirection | null;
    /**
     * Where the room it leads to lives: a map, or a Secret's own area. An exit
     * into a room of another map's Secret names that Secret's own area (its id),
     * as {@link Secret.area} does, and `toRoomNumber` the room in the Secret's
     * own numbering. Only scripts that can read Secrets (`smudgy.secrets` read)
     * ever see such an exit.
     */
    readonly toAreaId: AreaId | null;
    readonly toRoomNumber: RoomNumber | null;
    readonly isHidden: boolean;
    /** The exit's door, or `null` for an exit without one. */
    readonly door: Door | null;
    /** Pathfinding cost. */
    readonly weight: number;
    /** The command sent to traverse this exit, or `null` to use `fromDirection`. */
    readonly command: string | null;
    /** The room it leads to, wherever it lives (a map's room or a Secret's own room),
     *  or `undefined` when it leads nowhere loaded. */
    readonly toRoom: Room | undefined;
    /** The place that keeps the exit: the room's own place, or, for a hidden door on a
     *  map room, the Secret or Private additions that keep it. */
    readonly place: MapPlace;
}

/** Whether a door is open, closed or locked. Locked implies closed. */
type DoorState = "open" | "closed" | "locked";

/** An exit's door. Each side of a link has its own. */
interface Door {
    readonly state: DoorState;
    /** The door's name ("gate", "bookshelf"), or `null`. */
    readonly name: string | null;
    /**
     * The command that opens the door ("pull lever"), or `null`. It differs
     * from the exit's `command`, which goes through the exit.
     */
    readonly opensWith: string | null;
}

/**
 * A door as exits take it. `name` (1 to 64 characters) and `opensWith` (1 to
 * 255) are omitted or `null` for none; an empty string is refused.
 */
interface DoorArgs {
    state: DoorState;
    name?: string | null;
    opensWith?: string | null;
}

/** Fields accepted when creating an exit (`mapper.createRoomExit`). Only
 *  `fromDirection` is required. Visual appearance (routing, dash, color,
 *  thickness) lives on the shared Connection, not the exit.
 *
 *  An exit's door says whether it is open, closed or locked: exits take no
 *  `isClosed` or `isLocked` (nor `is_closed`, `is_locked`). Passing one to
 *  `createRoomExit`, `setRoomExit`, `createLink` or a `mutateArea` callback
 *  throws a `TypeError` naming `door`, and nothing is written. */
interface ExitArgs {
    fromDirection: ExitDirection;
    toDirection?: ExitDirection;
    /**
     * The area the destination room lives in: a map, or a Secret's own area. A
     * Secret of another map is named by its own area (`secret.area.id`), with
     * `toRoomNumber` in its numbering; the exit stays in the room's place and
     * shows only to that Secret's readers. Naming one needs `smudgy.secrets`
     * read, and an exit into a Secret's room on the room's own map, which goes
     * into that Secret, `smudgy.secrets` write.
     */
    toAreaId?: AreaIdLike;
    toRoomNumber?: RoomNumber;
    isHidden?: boolean;
    /** The new exit's door; omitted or `null` for none. Use it in place of
     *  `isClosed` and `isLocked`, which throw: `door: { state: "locked" }`. */
    door?: DoorArgs | null;
    weight?: number;
    command?: string;
}

/** Fields accepted when updating an exit (`mapper.setRoomExit`). Any omitted field is
 *  left unchanged. As with {@link ExitArgs}, `isClosed` and `isLocked` throw a
 *  `TypeError` naming `door`. */
interface ExitUpdates {
    fromDirection?: ExitDirection;
    toDirection?: ExitDirection;
    /** As in {@link ExitArgs.toAreaId}. */
    toAreaId?: AreaIdLike;
    toRoomNumber?: RoomNumber;
    isHidden?: boolean;
    /** `null` removes the door, with its name and command; a door replaces it whole. */
    door?: DoorArgs | null;
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
    /** The room's place: "map", "private", or a Secret id. When omitted, the
     * containing area's own place. Reading or naming a Secret/Private anchor
     * requires the secrets Read capability; it does not change link ownership. */
    place?: "map" | "private" | SecretId;
    roomNumber: RoomNumber;
    side: RoomSide;
    /** Normalized position along the room wall, from 0 through 1. */
    portOffset: number;
    portMode: PortMode;
}

/** Shared topology, route, and appearance for one or two member Exits. */
interface Connection {
    readonly id: ConnectionId;
    readonly endpointA: ConnectionEndpoint;
    readonly endpointB: ConnectionEndpoint | null;
    readonly kind: ConnectionKind;
    readonly routing: ConnectionRouting;
    readonly segmentShape: ConnectionSegmentShape;
    readonly corner: ConnectionCorner;
    readonly routePoints: MapPoint[];
    readonly dash: ConnectionDash;
    readonly color: string;
    readonly thickness: number;
}

/** Geometry/appearance fields accepted by {@link Mapper.setConnection}. */
interface ConnectionUpdates {
    endpointA?: ConnectionEndpoint;
    endpointB?: ConnectionEndpoint;
    routing?: ConnectionRouting;
    segmentShape?: ConnectionSegmentShape;
    corner?: ConnectionCorner;
    routePoints?: MapPoint[];
    dash?: ConnectionDash;
    color?: string;
    thickness?: number;
}

/** One directed Exit to create as a member of a new Connection. */
interface LinkTraversalArgs extends ExitArgs {
    /** Room that owns this traversal. */
    roomNumber: RoomNumber;
}

/** One atomic link creation: Connection first, followed by one or two traversals. */
interface LinkCreateArgs extends ConnectionUpdates {
    endpointA: ConnectionEndpoint;
    endpointB?: ConnectionEndpoint;
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
     * rejected (`mutateArea` throws with `roomNumberExists` in the
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
    deleteRoomProperty(room: Room | RoomNumber, name: string): Promise<void>;
    deleteAreaProperty(name: string): Promise<void>;
    /** Add a tag to the room's own place, as {@link Mapper.addRoomTag} does. */
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
    readonly roomNumber: RoomNumber;
    /** Where the room lives: its map, or for a Secret's own room the Secret's area. */
    readonly areaId: AreaId;
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
    /** The room's exits, with the hidden doors the map's other places you read keep on it. */
    readonly exits: Exit[];
    /** Where the room lives: `"map"` for a map's room, or the Secret (or
     *  `"private"`) whose own room it is. */
    readonly place: MapPlace;
    /**
     * Read a custom property of the room's own place by key (or `undefined` if
     * unset). Other places' values never stand in: reach them with `in(place)` or
     * list them all with `combinedData`.
     *
     * @example
     * ```ts
     * const ordinary = room.data("notes");
     * const mine = room.in("private").data("notes");
     * const quest = room.in(questSecretId).data("notes");
     * ```
     */
    data(key: string): string | undefined;
    /**
     * One place's data on this room: `"map"`, `"private"` or any readable Secret
     * of the same map (a handle or its id). The data stays in that place even
     * when the room belongs to another source; reading it requires both sources.
     * A Secret that does not exist and one you cannot read both throw the same
     * "Secret not found".
     */
    in(place: MapPlaceLike): RoomView;
    /**
     * Every readable place's value for `key` on this room, side by side: the
     * room's own place first, then the map's places in {@link Area.places} order.
     * A place that keeps nothing for `key` is left out. Nothing is merged or ranked.
     */
    combinedData(key: string): RoomPlaceData[];
    /** Every readable place's properties on this room, each with its key, in the
     *  same order. */
    combinedData(): RoomPlaceEntry[];
    /** Each readable place's tags for this room, the room's own place first. */
    combinedTags(): RoomPlaceTag[];
    /** Every readable place's tags for this room together, normalized to UPPERCASE,
     *  without repeats, and sorted. Each tag still lives in one place
     *  ({@link Room.combinedTags} says which): writing this list back with
     *  `mapper.addRoomTag` copies every other place's tags into the room's own place,
     *  where everyone who reads that place sees them. To tag the room in one place,
     *  use `room.in(place).addTag(tag)`. */
    readonly tags: string[];
    /** Whether any readable place tags this room with `tag` (case-insensitive). */
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
    /**
     * For a Secret's own area, or your Private additions', the map it belongs to;
     * absent on a map. A Secret's rooms are rooms like any other: routes, the
     * current location and `getAreaById` all take its area id.
     */
    readonly mapId?: AreaId;
    /** Where the area lives: `"map"` for a map, or the Secret (or `"private"`)
     *  whose own area it is. */
    readonly place: MapPlace;
    /**
     * The map's places: `"map"`, each Secret you read in layer (color) order, then
     * `"private"` on a cloud map. A Secret's own area has its own place alone.
     */
    readonly places: MapPlace[];
    readonly roomNumbers: RoomNumber[];
    /**
     * Whether this is a session map: it lives only for this session and is
     * discarded when the session closes.
     * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
     * Use `storage === "session"` instead.
     */
    readonly isEphemeral: boolean;
    /** The area's actual storage tier. */
    readonly storage: MapStorage;
    /** The next unused room number in this area. Each place numbers its own rooms, so
     *  a Secret's own area starts at 1 whatever numbers the map uses. */
    readonly nextRoomNumber: RoomNumber;
    /** The room with this number, or `undefined`. */
    room(roomNumber: number): Room | undefined;
    /** Read a custom property of the area's own place by key (or `undefined` if unset). */
    data(key: string): string | undefined;
    /** One place's data on this map, and the searches narrowed to that place; see
     *  {@link Room.in}. */
    in(place: MapPlaceLike): AreaView;
    /** The map's Secrets. */
    readonly secrets: SecretRegistry;
    /**
     * This area's rooms whose `name` property is exactly `value` in any place
     * you read (or the one `options.in` names): a map room matched by a
     * Secret's data is the map's room, and a Secret's own room is the Secret's.
     * One indexed lookup per place, however many rooms the area has. An area
     * answers for itself even when you have turned its map off — naming it is
     * asking for it.
     */
    findRoomsByProperty(name: string, value: string, options?: PlaceOptions): Room[];
    /**
     * This area's rooms carrying a property called `name`, whatever its value —
     * "which rooms did I write this on at all" — like `findRoomsByProperty`.
     */
    findRoomsWithProperty(name: string, options?: PlaceOptions): Room[];
    /**
     * This area's rooms carrying `tag` (case-insensitive), like
     * `findRoomsByProperty`, in no particular order.
     */
    findRoomsWithTag(tag: string, options?: PlaceOptions): Room[];
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
     * The atlas to create the area in. The atlas determines the storage tier
     * when `storage` is omitted; when both are given they must match. When
     * omitted, a saved map goes in this server's default atlas for its
     * storage tier, which the player can change in the map editor. Until one
     * is set, the player's atlas named for the server in the cloud ("Loose
     * maps (Arctic)" on a server named Arctic), or "Loose maps" on this
     * device, becomes it, made the first time it is needed. A session map is
     * in no atlas. If no atlas can be had, the call rejects.
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
    /**
     * Area properties the new map starts with, as `area.data()` reads them:
     * at most 256, each value a string. Local and session maps are saved
     * together with their properties. A cloud map is created first and
     * receives them before the call resolves; if they cannot be saved, the
     * map is deleted again and the call rejects, so a rejected call leaves
     * no map behind. The one exception is a cloud map that cannot be deleted
     * either: it remains without the properties, is added to this server's
     * maps like any new map, and the rejection names it and its id.
     */
    properties?: Record<string, string>;
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
    /** Copy areas together, preserving links between members of the set. A copy
     *  carries the Secrets you may copy (`"copy"` in their actions), as Secrets of
     *  your own under new ids, and your Private additions; nothing of other
     *  Secrets, and no exit into another map's Secret. */
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
     *  location names an area without a specific room: somewhere unmapped. Without the
     *  `secrets` capability, a room of a Secret or of Private additions reads that way too. */
    getCurrentLocation(): { area: AreaId; room?: RoomNumber } | undefined;
    /** All active areas (areas marked inactive are excluded). */
    readonly areas: Area[];
    /**
     * The area with this id. A Secret's id names the Secret's own area: its own
     * rooms, labels and shapes, with `mapId` naming its map. Without the `secrets`
     * capability, a Secret's or Private additions' area is not found, as an unknown id is not.
     */
    getAreaById(id: AreaIdLike | SecretIdLike): Area;
    /**
     * One of a cloud map's Secrets, by an id you stored (`area.secrets` lists a
     * map's). A Secret that does not exist and one you cannot read both throw the
     * same "Secret not found".
     */
    getSecretById(id: SecretIdLike): Secret;
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
     * Every room whose `name` property is exactly `value` in any place you read
     * (the map's own data, each Secret's, your Private additions'), or in the
     * one `options.in` names: a map room matched by a Secret's data is the map's
     * room, and a Secret's own room is the Secret's. Name and value both match
     * exactly. Each place keeps an index, so this costs one lookup per place
     * however large the map is; there is no reason to walk the areas yourself.
     * Rooms of maps you have turned off are left out.
     */
    findRoomsByProperty(name: string, value: string, options?: PlaceOptions): Room[];
    /**
     * Every room carrying a property called `name`, whatever its value — "which
     * rooms did I write this on at all" — in any place you read or the one
     * `options.in` names. Indexed like `findRoomsByProperty`.
     */
    findRoomsWithProperty(name: string, options?: PlaceOptions): Room[];
    /**
     * Every room carrying `tag` (case-insensitive) in any place you read or the
     * one `options.in` names, in no particular order. Reach for
     * `findNearestRoomWithTag` when you want the closest one instead. Indexed
     * like `findRoomsByProperty`.
     */
    findRoomsWithTag(tag: string, options?: PlaceOptions): Room[];
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
    /** Set a custom property (string key/value) in the room's own place; reach
     *  another place with `room.in(place).setData`. */
    setRoomProperty(
        area: Area | AreaIdLike,
        room: Room | RoomNumber,
        name: string,
        value: string,
    ): Promise<OperationId | null>;
    /** Set a custom area property (string key/value) in the area's own place; the write
     *  counterpart of `area.data(key)`. Pass an empty value to clear it. */
    setAreaProperty(area: Area | AreaIdLike, name: string, value: string): Promise<OperationId | null>;
    /** Delete a property from the room's own place; no other place's value changes. */
    deleteRoomProperty(area: Area | AreaIdLike, room: Room | RoomNumber, name: string): Promise<OperationId | null>;
    /** Delete a property from the area's own place. */
    deleteAreaProperty(area: Area | AreaIdLike, name: string): Promise<OperationId | null>;
    /** Add a case-insensitive tag to the room's own place (normalized to UPPERCASE;
     *  re-adding is a no-op): on a map room, the map's tags, which everyone who reads
     *  the map sees. To tag it in a Secret or your Private additions, use
     *  `room.in(place).addTag(tag)`. */
    addRoomTag(area: Area | AreaIdLike, room: Room | RoomNumber, tag: string): Promise<OperationId | null>;
    /** Remove a tag from the room's own place (case-insensitive). */
    removeRoomTag(area: Area | AreaIdLike, room: Room | RoomNumber, tag: string): Promise<OperationId | null>;
    /**
     * The nearest reachable room carrying `tag` (case-insensitive) in any place you
     * read, or in the one `options.in` names, from `from`, by the same weighted search
     * as `getPathBetweenRooms`, which takes the hidden doors the map's places keep
     * (the start room counts if it carries the tag), or `undefined` if none is
     * reachable. Path to it with `getPathBetweenRooms`.
     */
    findNearestRoomWithTag(from: Room, tag: string, options?: PlaceOptions): Room | undefined;
    /**
     * The nearest reachable room that carries every tag in `all` and none of the
     * tags in `none` (all case-insensitive), where a room's tags are every place's
     * you read (or the one place `options.in` names), so one tag can come from the
     * map and another from a Secret; `undefined` if no such room is reachable or
     * the filter is empty. Used by multi-tag speedwalks like `\inn.peace` and
     * `\!peace.guild`.
     */
    findNearestRoomWithTags(
        from: Room,
        filter: { all?: string[]; none?: string[] },
        options?: PlaceOptions,
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
     * `roomNumberExists` instead of silently merging into that room.
     */
    createRoom(area: Area | AreaIdLike, params: CreateRoomParams): Promise<RoomNumber>;
    /** Update multiple fields of a room in one cache update; only present fields change. */
    updateRoom(area: Area | AreaIdLike, room: Room | RoomNumber, fields: UpdateRoomParams): Promise<OperationId | null>;
    /** Batch-update many rooms of one area in a single cache update. */
    updateRooms(area: Area | AreaIdLike, updates: [RoomNumber, UpdateRoomParams][]): Promise<OperationId[]>;
    /**
     * Create an exit on a room, in the room's own place, and return its new id.
     * Give a map room a hidden door that a Secret or your Private additions keep
     * with `room.in(place).createExit`.
     * Linking to a Secret room requires Secret Read, not Secret Write; the exit
     * stays in its chosen place. Its destination is hidden from readers who
     * cannot read that room.
     */
    createRoomExit(area: Area | AreaIdLike, room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId>;
    /**
     * Update an existing exit. Resolves after backend acknowledgement; equal
     * updates resolve to `null` without sending a mutation. An exit a Secret or
     * your Private additions keep on a map room is changed in that place, which
     * needs `secrets` write (with read alone, it throws the `secrets-write`
     * capability error).
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
     * leave maps unchanged (`mergeAreasInvalidTranslation` or
     * `mergeAreasRoomNumbersExhausted`). After an interrupted save, call
     * `refreshAreas()` before retrying: an error does not guarantee that the maps
     * were left unchanged.
     * All touched maps must use the same storage tier: local or session. Cloud maps
     * and links from a different tier are refused. Refusal messages explain the
     * reason and include a stable code that scripts can check:
     * - `mergeAreasNoSources`: no source areas were provided.
     * - `mergeAreasSameArea`: a source repeats or is the destination.
     * - `mergeAreasNoRooms`: an explicit room list is empty.
     * - `mergeAreasInvalidRooms`: a room list is not an array of 32-bit integers.
     * - `mergeAreasRoomNotFound`: a selected room is missing.
     * - `mergeAreasMixedTiers`: affected maps use different storage tiers.
     * - `mergeAreasUnsupportedStorage`: cloud merges are unsupported.
     * - `mergeAreasBusy`: pending edits or another operation prevent the merge.
     * - `mergeAreasSourceChanged`: maps or incoming links changed while waiting.
     * - `mergeAreasInvalidTranslation`: offsets are invalid or coordinates overflow.
     * - `mergeAreasRoomNumbersExhausted`: no representable room number remains.
     * Missing maps, invalid connections and storage failures also reject the call.
     * Requires `mapper:write`.
     */
    mergeAreas(into: Area | AreaIdLike, sources: (Area | AreaIdLike | MergeAreaSource)[]): Promise<MergedRoom[]>;
    /** Delete a room. */
    deleteRoom(area: Area | AreaIdLike, room: Room | RoomNumber): Promise<OperationId | null>;
    /** Delete an exit from a room; one a Secret or your Private additions keep
     *  on a map room is deleted from that place, which needs `secrets` write. */
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
     *  the area. Like a copy, it carries the Secrets you may copy (`"copy"` in
     *  their actions) and your Private additions, and nothing of other Secrets. */
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
