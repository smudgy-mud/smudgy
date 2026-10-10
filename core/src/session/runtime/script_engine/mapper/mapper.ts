// Ops are captured from `Deno.core.ops` at module-eval time -- NOT imported
// from "ext:core/ops": that synthetic module's exports are frozen into the V8
// startup snapshot (which bakes only the deno_runtime base set), so the ops of
// this extension -- initialized per isolate OUTSIDE the snapshot -- never
// appear there. `Deno.core.ops` is (re)bound with the full op table at every
// runtime init, snapshot or not (see smudgy.ts for the long form).
// NOTE: extension source must be 7-bit ASCII (deno_core extensions.rs check).
const {
    op_smudgy_mapper_set_current_location,
    op_smudgy_mapper_get_current_location,
    op_smudgy_mapper_list_area_ids,
    op_smudgy_mapper_refresh_areas,
    op_smudgy_mapper_ready,
    op_smudgy_mapper_list_area_room_numbers,
    op_smudgy_mapper_list_rooms_by_title_and_description,
    op_smudgy_mapper_list_rooms_by_title_description_and_visible_exits,
    op_smudgy_mapper_find_rooms_by_property,
    op_smudgy_mapper_find_rooms_with_property,
    op_smudgy_mapper_find_rooms_with_tag,
    op_smudgy_mapper_find_areas_by_property,
    op_smudgy_mapper_find_areas_with_property,
    op_smudgy_mapper_find_area_rooms_by_property,
    op_smudgy_mapper_find_area_rooms_with_property,
    op_smudgy_mapper_find_area_rooms_with_tag,
    op_smudgy_mapper_create_area,
    op_smudgy_mapper_get_area_storage,
    op_smudgy_mapper_get_atlas_storage,
    op_smudgy_mapper_list_atlases,
    op_smudgy_mapper_create_atlas,
    op_smudgy_mapper_relocate_areas,
    op_smudgy_mapper_relocate_atlas,
    op_smudgy_mapper_delete_area,
    op_smudgy_mapper_get_area_is_ephemeral,
    op_smudgy_mapper_get_area_map_id,
    op_smudgy_mapper_rename_area,
    op_smudgy_mapper_get_area_by_id,
    op_smudgy_mapper_get_area_name,
    op_smudgy_mapper_get_area_id,
    op_smudgy_mapper_warn_area_uuid_once,
    op_smudgy_mapper_warn_snake_case_once,
    op_smudgy_mapper_get_area_room_by_number,
    op_smudgy_mapper_get_area_property,
    op_smudgy_mapper_get_area_next_room_number,
    op_smudgy_mapper_reserve_room_number,
    op_smudgy_mapper_release_room_reservations,
    op_smudgy_mapper_get_room_number,
    op_smudgy_mapper_get_room_area_id,
    op_smudgy_mapper_get_room_title,
    op_smudgy_mapper_get_room_description,
    op_smudgy_mapper_get_room_level,
    op_smudgy_mapper_get_room_x,
    op_smudgy_mapper_get_room_y,
    op_smudgy_mapper_get_room_color,
    op_smudgy_mapper_get_room_property,
    op_smudgy_mapper_get_room_tags,
    op_smudgy_mapper_has_tag,
    op_smudgy_mapper_add_room_tag,
    op_smudgy_mapper_remove_room_tag,
    op_smudgy_mapper_find_nearest_room_with_tags,
    op_smudgy_mapper_find_nearest_room_in_area,
    op_smudgy_mapper_get_room_external_id,
    op_smudgy_mapper_set_room_external_id,
    op_smudgy_mapper_find_room_by_external_id,
    op_smudgy_mapper_rescue_room_by_external_id,
    op_smudgy_mapper_get_room_exits,
    op_smudgy_mapper_set_room_title,
    op_smudgy_mapper_set_room_description,
    op_smudgy_mapper_set_room_color,
    op_smudgy_mapper_set_room_level,
    op_smudgy_mapper_set_room_x,
    op_smudgy_mapper_set_room_y,
    op_smudgy_mapper_set_room_property,
    op_smudgy_mapper_set_area_property,
    op_smudgy_mapper_create_room,
    op_smudgy_mapper_update_room,
    op_smudgy_mapper_update_rooms,
    op_smudgy_mapper_generate_id,
    op_smudgy_mapper_mutate_area,
    op_smudgy_mapper_create_room_exit,
    op_smudgy_mapper_set_room_exit,
    op_smudgy_mapper_merge_rooms,
    op_smudgy_mapper_merge_areas,
    op_smudgy_mapper_delete_room,
    op_smudgy_mapper_delete_room_exit,
    op_smudgy_mapper_get_area_labels,
    op_smudgy_mapper_get_area_shapes,
    op_smudgy_mapper_get_area_connections,
    op_smudgy_mapper_create_link,
    op_smudgy_mapper_set_connection,
    op_smudgy_mapper_unlink_exit,
    op_smudgy_mapper_pair_connections,
    op_smudgy_mapper_delete_link,
    op_smudgy_mapper_create_label,
    op_smudgy_mapper_create_shape,
    op_smudgy_mapper_set_label,
    op_smudgy_mapper_set_shape,
    op_smudgy_mapper_delete_label,
    op_smudgy_mapper_delete_shape,
    op_smudgy_mapper_import_areas,
    op_smudgy_mapper_import_areas_if_absent,
    op_smudgy_mapper_export_area,
    op_smudgy_mapper_get_path_between_rooms,
    op_smudgy_mapper_room_place,
    op_smudgy_mapper_area_place,
    op_smudgy_mapper_resolve_place,
    op_smudgy_mapper_list_area_places,
    op_smudgy_mapper_check_place,
    op_smudgy_mapper_room_place_data,
    op_smudgy_mapper_room_place_tags,
    op_smudgy_mapper_room_place_has_tag,
    op_smudgy_mapper_room_place_exits,
    op_smudgy_mapper_area_place_data,
    op_smudgy_mapper_room_combined_data,
    op_smudgy_mapper_room_combined_tags,
    op_smudgy_mapper_list_area_secrets,
    op_smudgy_mapper_get_area_secret,
    op_smudgy_mapper_area_secret_exists,
    op_smudgy_mapper_get_secret,
    op_smudgy_mapper_create_secret,
    op_smudgy_mapper_update_secret,
    op_smudgy_mapper_delete_secret,
    // (untyped: Deno.core is deno's private bootstrap namespace, no type decls)
} = (globalThis as any).Deno.core.ops;

// These declarations MIRROR the published author-facing contract in
// `core/src/models/script_typings/smudgy-mapper.d.ts` (the global ambient map types). The
// `mapper_ts_impl_conforms_to_contract` drift guard in `models/script_typings.rs` compiles
// this impl against that contract, so the two cannot silently diverge -- edit both together.
//
// Every map identity is its UUID's canonical lowercase hyphenated string. Ordinary string
// rules apply: compare with `===`, use one as a `Map`/`Set` key, and put one through
// `JSON.stringify` -- so ids ride the session store, store bindings and widget props like any
// other value. The brand is a type-only marker (nothing exists at runtime) that keeps an
// `ExitId` from satisfying an `AreaId` annotation. The contract's parameters take `<Id>Like`,
// which accepts that brand or an unbranded string -- so a plain string from JSON or a package
// parameter needs no cast, while a differently branded id is refused. The
// public methods here declare the branded type and rely on TypeScript's bivariant method
// parameters to satisfy that wider contract -- `normalizeId` is what actually validates.
type AreaId = string & { readonly __id: "AreaId" };
type AtlasId = string & { readonly __id: "AtlasId" };
type RoomNumber = number;
type ExitId = string & { readonly __id: "ExitId" };
type ConnectionId = string & { readonly __id: "ConnectionId" };
type OperationId = string & { readonly __id: "OperationId" };
type AreaIdLike = AreaId | (string & { readonly __id?: undefined });
type AtlasIdLike = AtlasId | (string & { readonly __id?: undefined });
type ExitIdLike = ExitId | (string & { readonly __id?: undefined });
type ConnectionIdLike = ConnectionId | (string & { readonly __id?: undefined });
type LabelIdLike = LabelId | (string & { readonly __id?: undefined });
type ShapeIdLike = ShapeId | (string & { readonly __id?: undefined });
type SecretId = string & { readonly __id: "SecretId" };
type SecretIdLike = SecretId | (string & { readonly __id?: undefined });

/** Who holds ownership authority over a Secret: the map's owner, its recorded clan members,
 * or its clan. */
type SecretOwnership = "owner" | "members" | "clan";

/** What the caller may do with a Secret. */
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

/** A Secret as the host serves it. */
interface SecretSnapshot {
    readonly id: SecretId;
    readonly mapId: AreaId;
    readonly name: string;
    readonly color: string | null;
    readonly ownership: SecretOwnership;
    readonly clanId?: string;
    readonly actions: readonly SecretAction[];
}

interface SecretUpdates {
    name?: string;
    color?: string | null;
}

interface CreateSecretOptions {
    name: string;
    color?: string | null;
    ownership?: "owner" | "members" | "clan";
    clanId?: string;
}

/** One of a map's places: its own content, Private additions, or a Secret. */
type MapPlace = "map" | "private" | Secret;
/** A place as calls take it: a place, or a Secret's id. */
type MapPlaceLike = MapPlace | SecretIdLike;

interface PlaceOptions {
    in?: MapPlaceLike;
}

/** One place's value for a property of a room (`room.combinedData(key)`). */
interface RoomPlaceData {
    readonly source: MapPlace;
    readonly data: string;
}

/** One place's value for one of a room's properties (`room.combinedData()`). */
interface RoomPlaceEntry extends RoomPlaceData {
    readonly key: string;
}

/** One place's tag on a room (`room.combinedTags()`). */
interface RoomPlaceTag {
    readonly source: MapPlace;
    readonly tag: string;
}

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

interface CreateRoomParams {
    title?: string;
    description?: string;
    level?: number;
    x?: number;
    y?: number;
    color?: string;
    externalId?: string;
}

// The fields `updateRoom`/`updateRooms`/`Room.update` accept: the same set as creation, minus
// the auto-assigned room number. Any omitted field is left unchanged.
type UpdateRoomParams = CreateRoomParams;

interface CreateAreaOptions {
    /** Omitted storage selects the default durable tier: cloud when signed
     * in, local otherwise (or the atlas's tier when `atlas` is given). */
    storage?: MapStorage;
    atlas?: Atlas | AtlasIdLike;
    /**
     * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
     * Use `storage: "session"` instead.
     */
    ephemeral?: boolean;
    /** Area properties the new map starts with, as `area.data()` reads them (at most 256). */
    properties?: Record<string, string>;
}

type MapStorage = "session" | "local" | "cloud";

interface MapDestination {
    storage: MapStorage;
    atlas?: Atlas | AtlasIdLike;
}

interface CreateAtlasOptions {
    storage: "local" | "cloud";
}

interface MutateAreaOptions {
    description?: string;
    in?: MapPlaceLike;
}

class MutateAreaError extends Error {
    readonly committedOperations: readonly OperationId[];

    constructor(message: string, committedOperations: readonly OperationId[]) {
        super(message);
        this.name = "MutateAreaError";
        this.committedOperations = committedOperations;
    }
}

/** Accept an id argument and hand back the string the ops take. Anything else is the
 * caller's mistake, and fails here with a clear TypeError rather than an opaque serde
 * error inside the op. */
function normalizeId<T extends string>(value: unknown, what: string): T {
    if (typeof value === "string") return value as T;
    throw new TypeError(`expected ${what} as a canonical UUID string, got ${typeof value}`);
}

// Every name in this API is camelCase. Through Smudgy 0.5.x the snake_case names earlier
// versions used keep working: what the API returns answers to them through hidden getters,
// what it takes accepts them, and the first one a script uses draws one notice per isolate.
// They go at 0.6 with the other 0.5 shims (the gate in mapper_api.rs).

type Renames = readonly (readonly [string, string])[];

const EXIT_ARGS_RENAMES: Renames = [
    ["from_direction", "fromDirection"],
    ["to_direction", "toDirection"],
    ["to_area_id", "toAreaId"],
    ["to_room_number", "toRoomNumber"],
    ["is_hidden", "isHidden"],
];

/**
 * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
 * Each snake_case name, by the kind of object that carries it, with the camelCase name that
 * replaced it.
 */
const SNAKE_CASE_NAMES = {
    room: [["room_number", "roomNumber"], ["area_id", "areaId"]],
    area: [["room_numbers", "roomNumbers"], ["next_room_number", "nextRoomNumber"]],
    exit: [
        ["connection_id", "connectionId"],
        ["from_area_id", "fromAreaId"],
        ["from_room_number", "fromRoomNumber"],
        ...EXIT_ARGS_RENAMES,
    ],
    exitArgs: EXIT_ARGS_RENAMES,
    traversal: [["room_number", "roomNumber"], ...EXIT_ARGS_RENAMES],
    endpoint: [["room_number", "roomNumber"], ["port_offset", "portOffset"], ["port_mode", "portMode"]],
    connection: [
        ["endpoint_a", "endpointA"],
        ["endpoint_b", "endpointB"],
        ["segment_shape", "segmentShape"],
        ["route_points", "routePoints"],
    ],
    label: [
        ["horizontal_alignment", "horizontalAlignment"],
        ["vertical_alignment", "verticalAlignment"],
        ["background_color", "backgroundColor"],
        ["font_size", "fontSize"],
        ["font_weight", "fontWeight"],
    ],
    shape: [
        ["background_color", "backgroundColor"],
        ["stroke_color", "strokeColor"],
        ["shape_type", "shapeType"],
        ["border_radius", "borderRadius"],
        ["stroke_width", "strokeWidth"],
    ],
} satisfies Record<string, Renames>;

let snakeCaseWarned = false;

/** Report the first snake_case name a script uses; the host latches it per isolate too. */
function warnSnakeCase(old: string, now: string): void {
    if (snakeCaseWarned) return;
    snakeCaseWarned = true;
    op_smudgy_mapper_warn_snake_case_once(old, now);
}

/** Give `target` a hidden getter for each snake_case name, reading its camelCase name. */
function defineSnakeCaseAliases(target: object, renames: Renames): void {
    for (const [old, now] of renames) {
        Object.defineProperty(target, old, {
            get(this: Record<string, unknown>) {
                warnSnakeCase(old, now);
                return this[now];
            },
            enumerable: false,
            configurable: true,
        });
    }
}

function aliasPrototype(renames: Renames): object {
    const prototype = {};
    defineSnakeCaseAliases(prototype, renames);
    return prototype;
}

const LABEL_PROTOTYPE = aliasPrototype(SNAKE_CASE_NAMES.label);
const SHAPE_PROTOTYPE = aliasPrototype(SNAKE_CASE_NAMES.shape);
const ENDPOINT_PROTOTYPE = aliasPrototype(SNAKE_CASE_NAMES.endpoint);
const CONNECTION_PROTOTYPE = aliasPrototype(SNAKE_CASE_NAMES.connection);

/** `value`'s own fields on an object that also answers to their snake_case names. Only the
 * camelCase fields are enumerable, so `JSON.stringify` and spreads carry those alone. */
function withSnakeCaseAliases<T extends object>(value: T, prototype: object): T {
    return Object.assign(Object.create(prototype), value);
}

/** `input` with each snake_case field moved to its camelCase name; a field given under both
 * names keeps the camelCase one. An input without snake_case fields comes back as it is. */
function fromSnakeCase<T>(input: T, renames: Renames): T {
    if (input === null || typeof input !== "object") return input;
    let out: Record<string, unknown> | undefined;
    for (const [old, now] of renames) {
        if (!Object.prototype.hasOwnProperty.call(input, old)) continue;
        warnSnakeCase(old, now);
        out ??= { ...(input as Record<string, unknown>) };
        if (out[now] === undefined) out[now] = out[old];
        delete out[old];
    }
    return (out ?? input) as T;
}

/** The door flags exits no longer take, in both spellings. An exit's door says whether it is
 * open, closed or locked. */
const DOOR_FLAGS = ["isClosed", "isLocked", "is_closed", "is_locked"] as const;

/** Refuses exit fields naming a door flag (any value but `undefined`), naming `door` as the
 * field that replaces it, so a script written for the flags fails where it passes them
 * instead of writing an exit without its door. */
function refuseDoorFlags(exit: unknown) {
    if (exit === null || typeof exit !== "object") return;
    const fields = exit as Record<string, unknown>;
    const flag = DOOR_FLAGS.find((name) => fields[name] !== undefined);
    if (flag === undefined) return;
    throw new TypeError(
        `Exits take no \`${flag}\`: give the exit a \`door\` instead, ` +
            `e.g. door: { state: "closed" } or door: { state: "locked" }; ` +
            `door: null for no door.`,
    );
}

function exitArgsIn<T extends ExitUpdates>(exit: T): T {
    refuseDoorFlags(exit);
    return fromSnakeCase(exit, SNAKE_CASE_NAMES.exitArgs);
}

/** Connection fields, with their endpoints and a link's traversals, in camelCase. */
function connectionIn<T extends ConnectionUpdates & { traversals?: LinkTraversalArgs[] }>(updates: T): T {
    const out = fromSnakeCase(updates, SNAKE_CASE_NAMES.connection);
    if (out === null || typeof out !== "object") return out;
    return {
        ...out,
        endpointA: fromSnakeCase(out.endpointA, SNAKE_CASE_NAMES.endpoint),
        endpointB: fromSnakeCase(out.endpointB, SNAKE_CASE_NAMES.endpoint),
        traversals: Array.isArray(out.traversals)
            ? out.traversals.map((traversal) => {
                refuseDoorFlags(traversal);
                return fromSnakeCase(traversal, SNAKE_CASE_NAMES.traversal);
            })
            : out.traversals,
    } as T;
}

/** A refusal's message as the host words one: the reason, then its code, with the code's
 * snake_case spelling beside it through 0.5.x. */
function refusal(message: string, code: string, formerly: string): string {
    return `${message} (${code}; formerly ${formerly})`;
}

function connectionOut(connection: Connection): Connection {
    return withSnakeCaseAliases(
        {
            ...connection,
            endpointA: withSnakeCaseAliases(connection.endpointA, ENDPOINT_PROTOTYPE),
            endpointB: connection.endpointB === null
                ? null
                : withSnakeCaseAliases(connection.endpointB, ENDPOINT_PROTOTYPE),
        },
        CONNECTION_PROTOTYPE,
    );
}

/** A room argument's number. */
function roomNumberOf(room: Room | RoomNumber): RoomNumber {
    return room instanceof Room ? room.roomNumber : room;
}

/** Unwrap an atlas argument structurally. The contract `Atlas` type is an
 * interface, so callers may legitimately hold plain objects (a spread or a
 * JSON round-trip of a handle) rather than this module's class; anything
 * carrying a usable id is accepted. */
function atlasIdOf(atlas: Atlas | AtlasIdLike | undefined): AtlasId | undefined {
    if (atlas === undefined) return undefined;
    if (atlas instanceof Atlas) return atlas.id;
    const what = "an Atlas handle or an AtlasId";
    return typeof atlas === "string"
        ? normalizeId<AtlasId>(atlas, what)
        : normalizeId<AtlasId>((atlas as Atlas)?.id, what);
}

/** Unwrap an area argument, structurally like `atlasIdOf`: a handle that came
 * back through JSON is a plain object rather than this module's class, and
 * anything carrying a usable id is accepted. */
function areaIdOf(area: Area | AreaId): AreaId {
    if (area instanceof Area) return area.id;
    const what = "an Area handle or an AreaId";
    return typeof area === "string"
        ? normalizeId<AreaId>(area, what)
        : normalizeId<AreaId>((area as Area)?.id, what);
}

/** The key the host names a place by: "map", "private", or a Secret's id. A Secret's id is a
 * UUID, so it can never be mistaken for a keyword; the host checks that it names a Secret. */
function placeKey(place: MapPlaceLike): string {
    if (typeof place === "string") return place;
    if (place instanceof Secret) return place.id;
    if (typeof place === "object" && place !== null && typeof (place as Secret).id === "string") {
        return (place as Secret).id;
    }
    throw new TypeError(
        `expected a place ("map", "private", a Secret or a Secret's id), got ${typeof place}`,
    );
}

/** The key of an options object's `in`, or "" (every place, or the handle's own) without one. */
function optionalPlaceKey(options: PlaceOptions | undefined): string {
    return options?.in === undefined ? "" : placeKey(options.in);
}

/** A place the host served: a keyword, or a Secret. */
function placeFrom(served: string | SecretSnapshot): MapPlace {
    return typeof served === "string" ? (served as "map" | "private") : Secret.from(served);
}

/** The place a host key names. Anything but the map needs the `secrets` capability. */
function placeOf(key: string): MapPlace {
    return key === "map" ? "map" : placeFrom(op_smudgy_mapper_resolve_place(key));
}

/** Resolves place keys, each once per call. */
function placeCache(): (key: string) => MapPlace {
    const places = new Map<string, MapPlace>();
    return (key) => {
        let place = places.get(key);
        if (place === undefined) {
            place = placeOf(key);
            places.set(key, place);
        }
        return place;
    };
}

/** Write one operation into a place of a map (`place` "" is the area's own), through the same
 * path as `mutateArea`. */
async function writeInPlace(
    area: Area | AreaId,
    place: string,
    operation: AreaBatchOperation,
    description: string,
): Promise<OperationId | null> {
    const outcome: { committed: OperationId[]; error: string | null } =
        await op_smudgy_mapper_mutate_area(areaIdOf(area), place, [operation], description);
    if (outcome.error !== null && outcome.error !== undefined) throw new Error(outcome.error);
    return outcome.committed[0] ?? null;
}

/** One of a cloud map's Secrets. A handle shows what the caller's projection serves about it
 * (never its owners, grants or audience) as of the last time it was read; every read of the
 * same Secret refreshes the same handle, so handles compare with `===`. */
class Secret {
    static readonly #handles = new Map<string, Secret>();

    readonly id: SecretId;
    #snapshot: SecretSnapshot;

    private constructor(snapshot: SecretSnapshot) {
        this.id = snapshot.id;
        this.#snapshot = snapshot;
    }

    /** The handle for a Secret the host served, refreshed to what it served. */
    static from(snapshot: SecretSnapshot): Secret {
        const served = Object.freeze({ ...snapshot, actions: Object.freeze([...snapshot.actions]) });
        const known = Secret.#handles.get(served.id);
        if (known !== undefined) {
            known.#snapshot = served;
            return known;
        }
        const handle = new Secret(served);
        Secret.#handles.set(served.id, handle);
        return handle;
    }

    static #forget(id: string): void {
        Secret.#handles.delete(id);
    }

    get name(): string {
        return this.#snapshot.name;
    }

    /** Its chosen color, `#rrggbb`, or `null` when the palette picks one. */
    get color(): string | null {
        return this.#snapshot.color;
    }

    get ownership(): SecretOwnership {
        return this.#snapshot.ownership;
    }

    /** The clan a Clan Secret belongs to. */
    get clanId(): string | undefined {
        return this.#snapshot.clanId;
    }

    /** What the caller may do with it. */
    get actions(): readonly SecretAction[] {
        return this.#snapshot.actions;
    }

    /** The map the Secret belongs to. */
    get mapId(): AreaId {
        return this.#snapshot.mapId;
    }

    /** The Secret's own area: its own rooms, labels and shapes. */
    get area(): Area {
        return mapper.getAreaById(this.id);
    }

    /** Rename and recolor in one request; the handle shows the result. */
    async update(changes: SecretUpdates): Promise<void> {
        if (changes === null || typeof changes !== "object") {
            throw new TypeError("expected the Secret's changes as { name?, color? }");
        }
        const name = changes.name;
        if (name !== undefined && typeof name !== "string") {
            throw new TypeError(`expected the Secret's name as a string, got ${typeof name}`);
        }
        const setColor = changes.color !== undefined;
        if (setColor && changes.color !== null && typeof changes.color !== "string") {
            throw new TypeError(`expected the Secret's color as "#rrggbb" or null, got ${typeof changes.color}`);
        }
        if (name === undefined && !setColor) {
            throw new TypeError("expected a new name, a new color or both");
        }
        Secret.from(await op_smudgy_mapper_update_secret(this.id, name, setColor, changes.color ?? null));
    }

    /** Delete the Secret and everything in it. */
    async delete(): Promise<void> {
        await op_smudgy_mapper_delete_secret(this.id);
        Secret.#forget(this.id);
    }

    toString(): string {
        return this.#snapshot.name;
    }
}

/** A map's Secrets (`area.secrets`). */
class SecretRegistry {
    readonly #area: Area;
    readonly #obj: unknown;

    constructor(area: Area, obj: unknown) {
        this.#area = area;
        this.#obj = obj;
    }

    /** The map's Secrets the caller reads, in place order. */
    list(): Secret[] {
        return op_smudgy_mapper_list_area_secrets(this.#obj).map((secret: SecretSnapshot) =>
            Secret.from(secret)
        );
    }

    /** The Secret with this id on this map; "Secret not found" alike for one that does not
     * exist, one the caller cannot read and one on another map. */
    get(id: SecretIdLike): Secret {
        return Secret.from(op_smudgy_mapper_get_area_secret(this.#obj, normalizeId<SecretId>(id, "a SecretId")));
    }

    /** Whether `get(id)` would find it. */
    exists(id: SecretIdLike): boolean {
        return op_smudgy_mapper_area_secret_exists(this.#obj, normalizeId<SecretId>(id, "a SecretId"));
    }

    /** Create a Secret on this map in one request, and resolve once the server has it: an owner
     * Secret, or with `ownership` a Clan Secret. */
    async create(options: CreateSecretOptions): Promise<Secret> {
        if (options === null || typeof options !== "object" || typeof options.name !== "string") {
            throw new TypeError("expected the new Secret as { name, color?, ownership?, clanId? }");
        }
        const color = options.color ?? null;
        if (color !== null && typeof color !== "string") {
            throw new TypeError(`expected the Secret's color as "#rrggbb" or null, got ${typeof color}`);
        }
        const ownership = options.ownership ?? null;
        if (ownership !== null && ownership !== "owner" && ownership !== "members" && ownership !== "clan") {
            throw new TypeError(`expected the Secret's ownership as "owner", "members" or "clan", got ${String(ownership)}`);
        }
        const clanId = options.clanId ?? null;
        if (clanId !== null && typeof clanId !== "string") {
            throw new TypeError(`expected clanId as a string, got ${typeof clanId}`);
        }
        if ((ownership === null || ownership === "owner") && clanId !== null) {
            throw new TypeError("an owner Secret takes no clanId");
        }
        return Secret.from(
            await op_smudgy_mapper_create_secret(this.#area.id, options.name, color, ownership, clanId),
        );
    }
}

/** One place's data on one room (`room.in(place)`). */
class RoomView {
    readonly #room: Room;
    readonly #obj: unknown;
    readonly #key: string;
    readonly #own: boolean;

    constructor(room: Room, obj: unknown, key: string) {
        this.#room = room;
        this.#obj = obj;
        this.#key = key;
        this.#own = key === op_smudgy_mapper_room_place(obj);
    }

    get place(): MapPlace {
        return placeOf(this.#key);
    }

    data(key: string): string | undefined {
        return op_smudgy_mapper_room_place_data(this.#obj, this.#key, key) ?? undefined;
    }

    get tags(): string[] {
        return op_smudgy_mapper_room_place_tags(this.#obj, this.#key);
    }

    hasTag(tag: string): boolean {
        return op_smudgy_mapper_room_place_has_tag(this.#obj, this.#key, String(tag));
    }

    get exits(): Exit[] {
        return op_smudgy_mapper_room_place_exits(this.#obj, this.#key).map((exit: ExitWire) => new MapExit(exit));
    }

    #attachmentAddress(): { mapId: AreaId; room_number: RoomNumber; room_source: string } {
        const area = mapper.getAreaById(this.#room.areaId);
        if (!area) throw new Error("The room's map is no longer available");
        return {
            mapId: area.mapId ?? area.id,
            room_number: this.#room.roomNumber,
            room_source: op_smudgy_mapper_room_place(this.#obj),
        };
    }

    setData(key: string, value: string): Promise<OperationId | null> {
        const room = this.#room;
        if (this.#own) return mapper.setRoomProperty(room.areaId, room, key, value);
        const { mapId, ...anchor } = this.#attachmentAddress();
        return writeInPlace(mapId, this.#key, {
            upsert_room_property: { ...anchor, name: key, value },
        }, "Scripted room property");
    }

    deleteData(key: string): Promise<OperationId | null> {
        const room = this.#room;
        if (this.#own) return mapper.deleteRoomProperty(room.areaId, room, key);
        const { mapId, ...anchor } = this.#attachmentAddress();
        return writeInPlace(mapId, this.#key, {
            delete_room_property: { ...anchor, name: key },
        }, "Scripted room property");
    }

    async addTag(tag: string): Promise<OperationId | null> {
        if (this.hasTag(tag)) return null;
        const room = this.#room;
        if (this.#own) return mapper.addRoomTag(room.areaId, room, tag);
        const { mapId, ...anchor } = this.#attachmentAddress();
        return writeInPlace(mapId, this.#key, {
            add_room_tag: { ...anchor, tag },
        }, "Scripted room tag");
    }

    async removeTag(tag: string): Promise<OperationId | null> {
        if (!this.hasTag(tag)) return null;
        const room = this.#room;
        if (this.#own) return mapper.removeRoomTag(room.areaId, room, tag);
        const { mapId, ...anchor } = this.#attachmentAddress();
        return writeInPlace(mapId, this.#key, {
            remove_room_tag: { ...anchor, tag },
        }, "Scripted room tag");
    }

    async createExit(exit: ExitArgs): Promise<ExitId> {
        const room = this.#room;
        if (this.#own) return mapper.createRoomExit(room.areaId, room, exit);
        const id: ExitId = op_smudgy_mapper_generate_id();
        const { mapId, ...anchor } = this.#attachmentAddress();
        await writeInPlace(mapId, this.#key, {
            create_exit: { ...anchor, id, body: { ...exitArgsIn(exit) } },
        }, "Scripted room exit");
        return id;
    }
}

/** One place's data on one map (`area.in(place)`), and the searches narrowed to it. */
class AreaView {
    readonly #area: Area;
    readonly #obj: unknown;
    readonly #key: string;
    readonly #own: boolean;

    constructor(area: Area, obj: unknown, key: string) {
        this.#area = area;
        this.#obj = obj;
        this.#key = key;
        this.#own = key === op_smudgy_mapper_area_place(obj);
    }

    get place(): MapPlace {
        return placeOf(this.#key);
    }

    data(key: string): string | undefined {
        return op_smudgy_mapper_area_place_data(this.#obj, this.#key, key) ?? undefined;
    }

    setData(key: string, value: string): Promise<OperationId | null> {
        if (this.#own) return mapper.setAreaProperty(this.#area, key, value);
        return writeInPlace(this.#area.id, this.#key, {
            upsert_area_property: { name: key, value },
        }, "Scripted area property");
    }

    deleteData(key: string): Promise<OperationId | null> {
        return writeInPlace(this.#area.id, this.#own ? "" : this.#key, {
            delete_area_property: { name: key },
        }, "Scripted area property");
    }

    findRoomsByProperty(name: string, value: string): Room[] {
        return this.#area.findRoomsByProperty(name, value, { in: this.#key });
    }

    findRoomsWithProperty(name: string): Room[] {
        return this.#area.findRoomsWithProperty(name, { in: this.#key });
    }

    findRoomsWithTag(tag: string): Room[] {
        return this.#area.findRoomsWithTag(tag, { in: this.#key });
    }
}

/** Resolves room references returned by the indexed lookups. A reference
 * whose room has gone between the lookup and here is dropped rather than
 * surfaced as a hole, so the result is always a list of real rooms. */
function hydrateRooms(refs: [AreaId, RoomNumber][]): Room[] {
    // Hits cluster by area, so the area handle is resolved once per area
    // rather than once per room. Ids are strings, so they key a Map directly.
    const areas = new Map<AreaId, Area>();
    const rooms: Room[] = [];
    for (const [areaId, roomNumber] of refs) {
        let area = areas.get(areaId);
        if (!area) {
            area = mapper.getAreaById(areaId);
            areas.set(areaId, area);
        }
        const room = area.room(roomNumber);
        if (room) rooms.push(room);
    }
    return rooms;
}

/** Check `createArea`'s initial properties before anything is created. Every own value must
 * be a string: an `undefined` value is refused rather than dropped, so a map never starts
 * without a property its caller meant to give it. */
function propertiesForOp(properties: Record<string, string> | undefined) {
    if (properties === undefined) return undefined;
    if (typeof properties !== "object" || properties === null || Array.isArray(properties)) {
        throw new TypeError("expected createArea properties as an object of strings");
    }
    for (const [name, value] of Object.entries(properties)) {
        if (typeof value !== "string") {
            throw new TypeError(
                `expected createArea property ${JSON.stringify(name)} as a string, got ${typeof value}`,
            );
        }
    }
    return properties;
}

function destinationForOp(destination: MapDestination) {
    return {
        storage: destination.storage,
        atlas_id: atlasIdOf(destination.atlas),
    };
}

/** Maps for the current session. Sessions sharing local maps see each other's changes
 * automatically. Cloud changes prompt sessions using the same service and account to refresh. */
const mapper = {
    /** Wait until this session's maps are ready for startup lookups or updates.
     * Loads maps if startup has not done so; later calls use loaded maps.
     * Requires `mapper:read`. */
    ready(): Promise<void> {
        return op_smudgy_mapper_ready();
    },

    /** Reload maps, including changes made to local files outside the app.
     * Updated maps are available to this session when the call resolves.
     * Use `ready()` for startup checks. Requires `mapper:read`. */
    refreshAreas(): Promise<void> {
        return op_smudgy_mapper_refresh_areas();
    },

    async createArea(name: string, options?: CreateAreaOptions) {
        // The deprecated ephemeral flag is forwarded only when the caller
        // actually supplied it, so the runtime can tell "flag passed" from
        // the fully supported storage-less default.
        const id = await op_smudgy_mapper_create_area(name, {
            storage: options?.storage,
            atlas_id: atlasIdOf(options?.atlas),
            ephemeral: options?.ephemeral,
            properties: propertiesForOp(options?.properties),
        });
        return new Area(id);
    },

    async listAtlases(): Promise<Atlas[]> {
        const atlases = await op_smudgy_mapper_list_atlases();
        return atlases.map((atlas: { id: AtlasId; name: string }) =>
            new Atlas(atlas.id, atlas.name)
        );
    },

    async createAtlas(name: string, options: CreateAtlasOptions): Promise<Atlas> {
        const atlas = await op_smudgy_mapper_create_atlas(name, options.storage);
        return new Atlas(atlas.id, atlas.name);
    },

    async copyAreas(areas: (Area | AreaId)[], destination: MapDestination): Promise<Area[]> {
        const ids = await op_smudgy_mapper_relocate_areas(
            areas.map(areaIdOf),
            destinationForOp(destination),
            false,
        );
        return ids.map((id: AreaId) => this.getAreaById(id));
    },

    async moveAreas(areas: (Area | AreaId)[], destination: MapDestination): Promise<Area[]> {
        const ids = await op_smudgy_mapper_relocate_areas(
            areas.map(areaIdOf),
            destinationForOp(destination),
            true,
        );
        return ids.map((id: AreaId) => this.getAreaById(id));
    },

    async copyArea(area: Area | AreaId, destination: MapDestination): Promise<Area> {
        return (await this.copyAreas([area], destination))[0];
    },

    async moveArea(area: Area | AreaId, destination: MapDestination): Promise<Area> {
        return (await this.moveAreas([area], destination))[0];
    },

    async copyAtlas(atlas: Atlas | AtlasId, storage: "local" | "cloud"): Promise<Atlas> {
        const copied = await op_smudgy_mapper_relocate_atlas(atlasIdOf(atlas), storage, false);
        return new Atlas(copied.id, copied.name);
    },

    async moveAtlas(atlas: Atlas | AtlasId, storage: "local" | "cloud"): Promise<Atlas> {
        const moved = await op_smudgy_mapper_relocate_atlas(atlasIdOf(atlas), storage, true);
        return new Atlas(moved.id, moved.name);
    },

    setCurrentLocation(areaId: AreaId, roomNumber?: RoomNumber) {
        op_smudgy_mapper_set_current_location(normalizeId<AreaId>(areaId, "an AreaId"), roomNumber);
    },

    /** The session's current mapper location (the last `setCurrentLocation`), or `undefined`
     * if none has been set. Current-session only: this reads this session's own UI marker, not
     * shared map data, so it is not addressable per-session. `room` is `undefined` when the
     * location names an area without a specific room. */
    getCurrentLocation(): { area: AreaId, room?: RoomNumber } | undefined {
        const location = op_smudgy_mapper_get_current_location();
        if (!location) return undefined;
        const [area, room] = location;
        return { area, room: room === null ? undefined : room };
    },

    /** Active areas only; areas marked inactive are excluded (use
     * `getAreaById` to reach one explicitly). */
    get areas(): Area[] {
        return op_smudgy_mapper_list_area_ids().map((id: AreaId) => new Area(op_smudgy_mapper_get_area_by_id(id)));
    },

    getAreaById(id: AreaId | SecretId) {
        let area = op_smudgy_mapper_get_area_by_id(normalizeId<AreaId>(id, "an AreaId"));
        return new Area(area);
    },

    /** One of a cloud map's Secrets, by an id you stored, or "Secret not found" alike for one
     * that does not exist and one you cannot read. */
    getSecretById(id: SecretIdLike): Secret {
        return Secret.from(op_smudgy_mapper_get_secret(normalizeId<SecretId>(id, "a SecretId")));
    },

    /** Collect related writes to one area. Callback and validation failures submit
     * nothing and pass through unchanged. Large batches may save in several ordered
     * steps; each step is atomic, and saved steps are not rolled back. Resolves once
     * all steps are saved, returning their operation IDs in order. Save failures throw
     * MutateAreaError with the IDs confirmed saved so far; other edits may still be
     * pending. See the public declaration for the error-handling example. */
    async mutateArea(
        area: Area | AreaId,
        callback: (mutation: AreaMutator) => void | Promise<void>,
        options?: MutateAreaOptions,
    ): Promise<OperationId[]> {
        // Always start from the current host snapshot. A script may retain an Area
        // wrapper across prior writes, including a now-stale nextRoomNumber.
        const target = this.getAreaById(areaIdOf(area));
        const place = optionalPlaceKey(options);
        // A place other than the map, written through the map, keeps data on the map's
        // rooms; its own rooms are edited through its own area.
        const onMapRooms = place !== "" && place !== "map" && target.mapId === undefined;
        const mutation = new AreaMutator(target, onMapRooms);
        try {
            await callback(mutation);
            const outcome: { committed: OperationId[]; error: string | null } =
                await op_smudgy_mapper_mutate_area(
                    target.id,
                    place,
                    mutation.finish(),
                    options?.description ?? "Scripted area mutation",
                );
            if (outcome.error !== null && outcome.error !== undefined) {
                throw new MutateAreaError(outcome.error, outcome.committed);
            }
            return outcome.committed;
        } catch (error) {
            mutation.abort();
            throw error;
        } finally {
            mutation.release();
        }
    },

    getPathBetweenRooms(fromAreaId: AreaId, fromRoomNumber: RoomNumber, toAreaId: AreaId, toRoomNumber: RoomNumber): [AreaId, RoomNumber][] {
        return op_smudgy_mapper_get_path_between_rooms(fromAreaId, fromRoomNumber, toAreaId, toRoomNumber);
    },

    listRoomsByTitleAndDescription(title: string, description: string) {
        return op_smudgy_mapper_list_rooms_by_title_and_description(title, description).map(
            ([areaId, roomNumber]: [AreaId, RoomNumber]) => this.getAreaById(areaId).room(roomNumber)
        );
    },

    listRoomsByTitleDescriptionAndVisibleExits(title: string, description: string, visibleExitDirections: string[]) {
        return op_smudgy_mapper_list_rooms_by_title_description_and_visible_exits(title, description, visibleExitDirections).map(
            ([areaId, roomNumber]: [AreaId, RoomNumber]) => this.getAreaById(areaId).room(roomNumber)
        );
    },

    /** Every room whose `name` property is exactly `value` in any place you read (the map's
     * own data, a Secret's, your Private additions'), or in the one `options.in` names. Name
     * and value both match exactly. One indexed lookup per place, however large the map is.
     * Rooms of maps you have turned off are left out. Requires `mapper:read`. */
    findRoomsByProperty(name: string, value: string, options?: PlaceOptions): Room[] {
        return hydrateRooms(op_smudgy_mapper_find_rooms_by_property(name, value, optionalPlaceKey(options)));
    },

    /** Every room carrying a property called `name`, whatever its value, in any place you
     * read or the one `options.in` names. Indexed like `findRoomsByProperty`. Rooms of maps
     * you have turned off are left out. Requires `mapper:read`. */
    findRoomsWithProperty(name: string, options?: PlaceOptions): Room[] {
        return hydrateRooms(op_smudgy_mapper_find_rooms_with_property(name, optionalPlaceKey(options)));
    },

    /** Every room carrying `tag` (case-insensitive) in any place you read or the one
     * `options.in` names, in no particular order. Reach for `findNearestRoomWithTag` when you
     * want the closest one instead. Rooms of maps you have turned off are left out. Requires
     * `mapper:read`. */
    findRoomsWithTag(tag: string, options?: PlaceOptions): Room[] {
        return hydrateRooms(op_smudgy_mapper_find_rooms_with_tag(tag, optionalPlaceKey(options)));
    },

    /** Every area whose `name` property is exactly `value`, as `area.data(name)`
     * reads it. Indexed like the room lookups. Maps you have turned off are left
     * out. Requires `mapper:read`. */
    findAreasByProperty(name: string, value: string): Area[] {
        return op_smudgy_mapper_find_areas_by_property(name, value).map((areaId: AreaId) =>
            this.getAreaById(areaId)
        );
    },

    /** Every area carrying a property called `name`, whatever its value. Maps
     * you have turned off are left out. Requires `mapper:read`. */
    findAreasWithProperty(name: string): Area[] {
        return op_smudgy_mapper_find_areas_with_property(name).map((areaId: AreaId) =>
            this.getAreaById(areaId)
        );
    },

    renameArea(area: Area | AreaId, name: string): Promise<void> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_rename_area(areaId, name);
    },

    deleteArea(area: Area | AreaId): Promise<void> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_delete_area(areaId);
    },

    setRoomTitle(area: Area | AreaId, room: Room | RoomNumber, title: string): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_title(areaId, roomNumber, title);
    },

    setRoomDescription(area: Area | AreaId, room: Room | RoomNumber, description: string): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_description(areaId, roomNumber, description);
    },

    setRoomColor(area: Area | AreaId, room: Room | RoomNumber, color: string): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_color(areaId, roomNumber, color);
    },

    setRoomLevel(area: Area | AreaId, room: Room | RoomNumber, level: number): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_level(areaId, roomNumber, level);
    },

    setRoomX(area: Area | AreaId, room: Room | RoomNumber, x: number): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_x(areaId, roomNumber, x);
    },

    setRoomY(area: Area | AreaId, room: Room | RoomNumber, y: number): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_y(areaId, roomNumber, y);
    },

    /** Set a custom property on a room, in the room's own place. Requires `mapper:write`. */
    setRoomProperty(
        area: Area | AreaId,
        room: Room | RoomNumber,
        name: string,
        value: string,
    ): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_property(areaId, roomNumber, name, value);
    },

    /** Set a custom data property on an area (the write counterpart of `area.data(key)`). Pass an
     * empty value to clear it. Requires the `mapper:write` capability. */
    setAreaProperty(area: Area | AreaId, name: string, value: string): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_set_area_property(areaId, name, value);
    },

    /** Delete a room property from the room's own place. */
    deleteRoomProperty(area: Area | AreaId, room: Room | RoomNumber, name: string): Promise<OperationId | null> {
        const roomNumber = roomNumberOf(room);
        return writeInPlace(area, "", {
            delete_room_property: { room_number: roomNumber, name },
        }, "Scripted room property");
    },

    /** Delete an area property from the area's own place. */
    deleteAreaProperty(area: Area | AreaId, name: string): Promise<OperationId | null> {
        return writeInPlace(area, "", {
            delete_area_property: { name },
        }, "Scripted area property");
    },

    /** Add a case-insensitive tag to a room, in the room's own place (on a map room, the map's
     * tags, which everyone who reads the map sees; `room.in(place).addTag` tags it in another
     * place). The tag is normalized to UPPERCASE; re-adding an existing tag is a no-op. Requires
     * the `mapper:write` capability. */
    addRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_add_room_tag(areaId, roomNumber, tag);
    },

    /** Remove a tag from a room's own place (case-insensitive). Requires `mapper:write`. */
    removeRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_remove_room_tag(areaId, roomNumber, tag);
    },

    /** The nearest reachable room carrying `tag` (case-insensitive) in any place you read, or
     * in the one `options.in` names, from `from`, by the same weighted graph search as
     * `getPathBetweenRooms`, which takes the hidden doors you read (the start room counts if
     * it carries the tag), or `undefined` if none is reachable. Path to it with
     * `getPathBetweenRooms`. Requires `mapper:read`. */
    findNearestRoomWithTag(from: Room, tag: string, options?: PlaceOptions): Room | undefined {
        return this.findNearestRoomWithTags(from, { all: [tag] }, options);
    },

    /** The nearest reachable room whose tags satisfy a conjunctive filter: has every tag in
     * `all` and none in `none` (all case-insensitive), where a room's tags are every place's
     * you read (or the one place `options.in` names), or `undefined` if none is reachable.
     * The candidates come from indexed lookups, so the search is cheap even over large maps.
     * An empty filter returns `undefined`. Requires `mapper:read`. */
    findNearestRoomWithTags(
        from: Room,
        filter: { all?: string[]; none?: string[] },
        options?: PlaceOptions,
    ): Room | undefined {
        const ref = op_smudgy_mapper_find_nearest_room_with_tags(
            from.areaId,
            from.roomNumber,
            filter.all ?? [],
            filter.none ?? [],
            optionalPlaceKey(options),
        );
        if (!ref) return undefined;
        const [areaId, roomNumber] = ref;
        return this.getAreaById(areaId).room(roomNumber);
    },

    /** The nearest reachable room belonging to `area` from `from`, by the same
     * weighted graph search as `getPathBetweenRooms` (`from` itself counts if it
     * is already in the area, and naming the area reaches it even when it is
     * marked inactive), or `undefined` if no room of the area is reachable. Path
     * to it with `getPathBetweenRooms`. Requires `mapper:read`. */
    findNearestRoomInArea(from: Room, area: Area | AreaId): Room | undefined {
        const areaId = areaIdOf(area);
        const ref = op_smudgy_mapper_find_nearest_room_in_area(
            from.areaId,
            from.roomNumber,
            areaId,
        );
        if (!ref) return undefined;
        const [refAreaId, roomNumber] = ref;
        return this.getAreaById(refAreaId).room(roomNumber);
    },

    /** The room bound to a server-global room id (a GMCP/MSDP room identity),
     * or `undefined` if no loaded room carries it. Best-effort when the same
     * id is bound in several areas. Requires `mapper:read`. */
    findRoomByExternalId(externalId: string): Room | undefined {
        const ref = op_smudgy_mapper_find_room_by_external_id(externalId);
        if (!ref) return undefined;
        const [refAreaId, roomNumber] = ref;
        return this.getAreaById(refAreaId).room(roomNumber);
    },

    /** Reports whether a room with this server-global id is already mapped for a
     * different server. When it is, the player is offered the chance to show
     * that map here too, and this returns `true`, so a caller drawing a map as
     * it explores knows the room is accounted for and should not recreate it.
     * Returns `false` when the id belongs to no other server's map. Requires
     * `mapper:read`. */
    rescueRoomByExternalId(externalId: string): boolean {
        return op_smudgy_mapper_rescue_room_by_external_id(externalId);
    },

    /** Bind (or, with an empty string, clear) a room's server-global room id.
     * Requires `mapper:write`. */
    setRoomExternalId(area: Area | AreaId, room: Room | RoomNumber, externalId: string): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_external_id(areaId, roomNumber, externalId);
    },

    createRoom(area: Area | AreaId, params: CreateRoomParams): Promise<RoomNumber> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_create_room(areaId, params);
    },

    /** Update multiple fields of an existing room in ONE cache update (one index rebuild)
     * instead of one per field. Only the fields present in `fields` change. */
    updateRoom(area: Area | AreaId, room: Room | RoomNumber, fields: UpdateRoomParams): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_update_room(areaId, roomNumber, fields);
    },

    /** Batch-update many rooms of one area in a single cache update. Each entry is a
     * `[roomNumber, fields]` pair; only the present fields of each change. */
    updateRooms(area: Area | AreaId, updates: [RoomNumber, UpdateRoomParams][]): Promise<OperationId[]> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_update_rooms(areaId, updates);
    },

    /** Create an exit on a room, in the room's own place. */
    createRoomExit(area: Area | AreaId, room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_create_room_exit(areaId, roomNumber, exitArgsIn(exit));
    },
    /** Update an existing exit and resolve only after the map backend
     * acknowledges the exact mutation. Equal updates resolve to `null`
     * without sending a revision-bumping no-op. */
    setRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId, exit: ExitUpdates): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_set_room_exit(areaId, roomNumber, exitId, exitArgsIn(exit));
    },
    /** Merge `remove` into `keep` as one durable area mutation. The kept
     * room's metadata wins; traversal is deduplicated and rewired. Resolves
     * only after the backend acknowledges the exact operation. */
    mergeRooms(area: Area | AreaId, keep: Room | RoomNumber, remove: Room | RoomNumber): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const keepRoomNumber = roomNumberOf(keep);
        const removeRoomNumber = roomNumberOf(remove);
        return op_smudgy_mapper_merge_rooms(areaId, keepRoomNumber, removeRoomNumber);
    },
    /** Fold areas into `into` as one durable transaction. Whole sources move their content
     * and are deleted; a source with `rooms` keeps its labels, shapes and area properties,
     * even if every room moves. The destination keeps its own area metadata and properties.
     * Offsets apply only to moved content; exits in the same storage tier follow moved rooms.
     * Resolves with each moved room's old address and new number, with the updated maps
     * available to this session. Invalid offsets or exhausted room numbers leave maps unchanged.
     * After an interrupted save, call `refreshAreas()` before retrying: an error does not
     * guarantee that the maps were left unchanged.
     * Local and session maps only. Requires `mapper:write`. */
    mergeAreas(into: Area | AreaId, sources: (Area | AreaId | MergeAreaSource)[]): Promise<MergedRoom[]> {
        const intoId = areaIdOf(into);
        const entries = sources.map((source) => {
            // A bare handle or id carries no room list and no offset; only a source entry can.
            const entry: MergeAreaSource =
                typeof source === "object" && source !== null && "area" in source
                    ? source
                    : { area: source };
            const wire: { area: AreaId; rooms?: RoomNumber[]; translate?: MergeAreaSource["translate"] } = {
                area: areaIdOf(entry.area),
            };
            const integer = (value: unknown): value is number =>
                typeof value === "number" && Number.isInteger(value) &&
                value >= -2147483648 && value <= 2147483647;
            if (entry.rooms !== undefined) {
                if (!Array.isArray(entry.rooms) ||
                    Array.from(entry.rooms).some((room) => !integer(room))) {
                    throw new TypeError(refusal("Rooms must be an array of 32-bit integers.", "mergeAreasInvalidRooms", "merge_areas_invalid_rooms"));
                }
                wire.rooms = entry.rooms;
            }
            if (entry.translate !== undefined) {
                const offset = entry.translate;
                if (offset === null || typeof offset !== "object" ||
                    [offset.x, offset.y].some((value) => value !== undefined &&
                        (typeof value !== "number" || !Number.isFinite(value) || Math.abs(value) > 3.4028234663852886e38)) ||
                    (offset.level !== undefined && !integer(offset.level))) {
                    throw new TypeError(refusal("A translation needs finite coordinates and a 32-bit integer level.", "mergeAreasInvalidTranslation", "merge_areas_invalid_translation"));
                }
                wire.translate = offset;
            }
            return wire;
        });
        return op_smudgy_mapper_merge_areas(intoId, entries);
    },
    deleteRoom(area: Area | AreaId, room: Room | RoomNumber): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_delete_room(areaId, roomNumber);
    },
    deleteRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        const roomNumber = roomNumberOf(room);
        return op_smudgy_mapper_delete_room_exit(areaId, roomNumber, exitId);
    },
    /** Atomically create one Connection and its one or two member traversals. */
    createLink(area: Area | AreaId, link: LinkCreateArgs): Promise<ConnectionId> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_create_link(areaId, connectionIn(link));
    },
    /** Update shared Connection geometry or appearance. */
    setConnection(area: Area | AreaId, connectionId: ConnectionId, updates: ConnectionUpdates): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_set_connection(areaId, connectionId, connectionIn(updates));
    },
    /** Split one traversal out of a bidirectional Connection. */
    unlinkRoomExit(area: Area | AreaId, exitId: ExitId): Promise<ConnectionId> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_unlink_exit(areaId, exitId);
    },
    /** Merge reciprocal one-way Connections, preserving `keepConnectionId`'s route. */
    pairConnections(area: Area | AreaId, keepConnectionId: ConnectionId, mergeConnectionId: ConnectionId): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_pair_connections(areaId, keepConnectionId, mergeConnectionId);
    },
    /** Delete a Connection and all of its member traversals. */
    deleteLink(area: Area | AreaId, connectionId: ConnectionId): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_delete_link(areaId, connectionId);
    },
    /** Add a text label to an area; returns its new id. Requires `mapper:write`. */
    createLabel(area: Area | AreaId, label: LabelArgs): Promise<LabelId> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_create_label(areaId, fromSnakeCase(label, SNAKE_CASE_NAMES.label));
    },
    /** Add a graphical shape to an area; returns its new id. Requires `mapper:write`. */
    createShape(area: Area | AreaId, shape: ShapeArgs): Promise<ShapeId> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_create_shape(areaId, fromSnakeCase(shape, SNAKE_CASE_NAMES.shape));
    },
    /** Delete a label from an area. Requires `mapper:write`. */
    deleteLabel(area: Area | AreaId, labelId: LabelId): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_delete_label(areaId, labelId);
    },
    /** Delete a shape from an area. Requires `mapper:write`. */
    deleteShape(area: Area | AreaId, shapeId: ShapeId): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_delete_shape(areaId, shapeId);
    },
    /** Update an existing label; only present fields change. Requires `mapper:write`. */
    setLabel(area: Area | AreaId, labelId: LabelId, updates: LabelUpdates): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_set_label(areaId, labelId, fromSnakeCase(updates, SNAKE_CASE_NAMES.label));
    },
    /** Update an existing shape; only present fields change. Requires `mapper:write`. */
    setShape(area: Area | AreaId, shapeId: ShapeId, updates: ShapeUpdates): Promise<OperationId | null> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_set_shape(areaId, shapeId, fromSnakeCase(updates, SNAKE_CASE_NAMES.shape));
    },
    /** Serialize an area to a portable JSON blob. Requires `mapper:read` and copy rights
     * (`can_copy`) on the area. */
    exportArea(area: Area | AreaId): Promise<AreaJson> {
        const areaId = areaIdOf(area);
        return op_smudgy_mapper_export_area(areaId);
    },
    /** Import portable area JSON as new LOCAL areas (fresh ids); cross-area exits within the set
     * are remapped, and exits pointing OUTSIDE the set are dropped (left unlinked). Returns the
     * new area ids. One-shot fast path. Requires `mapper:write`. */
    importAreas(areas: AreaJson[]): Promise<AreaId[]> {
        return op_smudgy_mapper_import_areas(areas);
    },
    /** Import one area JSON as a new local area; returns its id. Requires `mapper:write`. */
    async importArea(area: AreaJson): Promise<AreaId> {
        const [id] = await op_smudgy_mapper_import_areas([area]);
        return id;
    },
    /** Import portable area JSON, skipping (by name) every map already resident in the mapper --
     * including maps assigned to other servers and deactivated maps. Waits for the session's maps
     * to finish loading, so it is safe to call from package top-level code. Returns the imported
     * ids and the skipped names. Requires `mapper:write`. */
    importAreasIfAbsent(areas: AreaJson[]): Promise<{ added: AreaId[]; skipped: string[] }> {
        return op_smudgy_mapper_import_areas_if_absent(areas);
    }
};
// One exit read back from a room (`room.exits`). Optional links are present but `null` when
// unset (not omitted). Mirrors the `Exit` interface in the published contract.
interface Exit {
    readonly id: ExitId;
    /** The shared Connection this traversal belongs to. */
    readonly connectionId: ConnectionId;
    readonly fromDirection: ExitDirection;
    readonly fromAreaId: AreaId;
    readonly fromRoomNumber: RoomNumber;
    readonly toDirection: ExitDirection | null;
    readonly toAreaId: AreaId | null;
    readonly toRoomNumber: RoomNumber | null;
    readonly isHidden: boolean;
    readonly door: Door | null;
    readonly weight: number;
    readonly command: string | null;
    readonly toRoom: Room | undefined;
    readonly place: MapPlace;
}

// A door's state. Locked implies closed; an exit without a door has `door: null`.
type DoorState = "open" | "closed" | "locked";

// An exit's door: its state, its name, and the command that opens it (distinct from the
// exit's `command`, the one that goes through it).
interface Door {
    readonly state: DoorState;
    readonly name: string | null;
    readonly opensWith: string | null;
}

// A door as `createRoomExit` and `setRoomExit` take it; omitted or `null` name and
// `opensWith` are none.
interface DoorArgs {
    state: DoorState;
    name?: string | null;
    opensWith?: string | null;
}

/** An exit as the host serves it, with the key of the place that keeps it. */
type ExitWire = Omit<Exit, "toRoom" | "place"> & { readonly place: string };

/** An exit as `room.exits` returns it. */
class MapExit implements Exit {
    readonly id: ExitId;
    readonly connectionId: ConnectionId;
    readonly fromDirection: ExitDirection;
    readonly fromAreaId: AreaId;
    readonly fromRoomNumber: RoomNumber;
    readonly toDirection: ExitDirection | null;
    readonly toAreaId: AreaId | null;
    readonly toRoomNumber: RoomNumber | null;
    readonly isHidden: boolean;
    readonly door: Door | null;
    readonly weight: number;
    readonly command: string | null;
    readonly #place: string;

    constructor(exit: ExitWire) {
        this.id = exit.id;
        this.connectionId = exit.connectionId;
        this.fromDirection = exit.fromDirection;
        this.fromAreaId = exit.fromAreaId;
        this.fromRoomNumber = exit.fromRoomNumber;
        this.toDirection = exit.toDirection;
        this.toAreaId = exit.toAreaId;
        this.toRoomNumber = exit.toRoomNumber;
        this.isHidden = exit.isHidden;
        this.door = exit.door === null ? null : Object.freeze({ ...exit.door });
        this.weight = exit.weight;
        this.command = exit.command;
        this.#place = exit.place;
    }

    /** The room the exit leads to, wherever it lives: a map's room, or a Secret's own room. */
    get toRoom(): Room | undefined {
        if (this.toAreaId === null || this.toRoomNumber === null) return undefined;
        let area: Area;
        try {
            area = mapper.getAreaById(this.toAreaId);
        } catch {
            return undefined;
        }
        return area.room(this.toRoomNumber);
    }

    /** The place that keeps the exit. */
    get place(): MapPlace {
        return placeOf(this.#place);
    }
}
defineSnakeCaseAliases(MapExit.prototype, SNAKE_CASE_NAMES.exit);

// Fields accepted when creating an exit (`createRoomExit`); `fromDirection` is required.
// Visual appearance (routing, dash, color, thickness) lives on the shared
// Connection, not the exit.
interface ExitArgs {
    fromDirection: ExitDirection;
    toDirection?: ExitDirection;
    toAreaId?: AreaIdLike;
    toRoomNumber?: RoomNumber;
    isHidden?: boolean;
    // The new exit's door; omitted or `null` for none. Exits take no `isClosed` or
    // `isLocked` (nor `is_closed`, `is_locked`): passing one throws a TypeError naming `door`.
    door?: DoorArgs | null;
    weight?: number;
    command?: string;
}

// Fields accepted when updating an exit (`setRoomExit`). Any omitted field is left unchanged.
interface ExitUpdates {
    fromDirection?: ExitDirection;
    toDirection?: ExitDirection;
    toAreaId?: AreaIdLike;
    toRoomNumber?: RoomNumber;
    isHidden?: boolean;
    // `null` removes the door with its name and command; a door replaces it whole. As in
    // `ExitArgs`, a door flag (`isClosed`, `isLocked`) throws a TypeError naming `door`.
    door?: DoorArgs | null;
    weight?: number;
    command?: string;
}

type RoomSide = "North" | "East" | "South" | "West";
type PortMode = "AutoPinned" | "Manual";
type ConnectionKind = "Internal" | "SelfLoop" | "Dangling" | "External" | "CrossLevel";
type ConnectionRouting = "Stub" | "Simple" | "Manual" | "Automatic";
type ConnectionSegmentShape = "Direct" | "Orthogonal";
type ConnectionCorner = "Sharp" | "Rounded";
type ConnectionDash = "Solid" | "Dashed" | "Dotted";

interface MapPoint {
    x: number;
    y: number;
}

interface ConnectionEndpoint {
    place?: "map" | "private" | SecretId;
    roomNumber: RoomNumber;
    side: RoomSide;
    portOffset: number;
    portMode: PortMode;
}

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

interface LinkTraversalArgs extends ExitArgs {
    roomNumber: RoomNumber;
}

interface LinkCreateArgs extends ConnectionUpdates {
    endpointA: ConnectionEndpoint;
    endpointB?: ConnectionEndpoint;
    traversals: LinkTraversalArgs[];
}

// The host's batch wire: operation tags and their own fields keep the wire spelling, and
// each `body` carries the script-facing camelCase fields.
type AreaBatchOperation =
    | { upsert_room: { room_number: RoomNumber; body: CreateRoomParams } }
    | { create_room: { room_number: RoomNumber; body: CreateRoomParams } }
    | { delete_room: { room_number: RoomNumber } }
    | { upsert_room_property: { room_number: RoomNumber; room_source?: string; name: string; value: string } }
    | { upsert_area_property: { name: string; value: string } }
    | { delete_room_property: { room_number: RoomNumber; room_source?: string; name: string } }
    | { delete_area_property: { name: string } }
    | { add_room_tag: { room_number: RoomNumber; room_source?: string; tag: string } }
    | { remove_room_tag: { room_number: RoomNumber; room_source?: string; tag: string } }
    | { create_exit: { room_number: RoomNumber; room_source?: string; id: ExitId; body: ExitArgs } }
    | { update_exit: { exit_id: ExitId; body: ExitUpdates } }
    | { delete_exit: { exit_id: ExitId } }
    | { create_link: { connection_id: ConnectionId; body: LinkCreateArgs } }
    | { update_connection: { connection_id: ConnectionId; body: ConnectionUpdates } };

function roomNumberInArea(areaId: AreaId, room: Room | RoomNumber): RoomNumber {
    if (!(room instanceof Room)) return room;
    if (room.areaId !== areaId) {
        throw new TypeError("mutateArea cannot edit a room from another area");
    }
    return room.roomNumber;
}

/** A callback-scoped write collector. Its methods preserve the familiar async
 * mapper shape, but only record draft operations; the host is touched once the
 * callback completes. Draft room numbers are reserved against the host's live
 * allocator under a per-mutator token, so ambient creates cannot collide with
 * a draft; the reservation is released when the mutator finishes or aborts. */
class AreaMutator {
    readonly #areaId: AreaId;
    readonly #token: OperationId;
    readonly #onMapRooms: boolean;
    #operations: AreaBatchOperation[] = [];
    #open = true;

    constructor(area: Area, onMapRooms = false) {
        this.#areaId = area.id;
        this.#token = op_smudgy_mapper_generate_id();
        this.#onMapRooms = onMapRooms;
    }

    #record(operation: AreaBatchOperation): void {
        if (!this.#open) throw new TypeError("this mutateArea callback has finished");
        this.#operations.push(operation);
    }

    /** A place written through its map keeps properties, tags and exits on the map's rooms
     * and cannot change the rooms themselves. */
    #roomFields(): void {
        if (this.#onMapRooms) {
            throw new TypeError(
                "a mutateArea place keeps data on the map's rooms and cannot create, change or delete them; edit a Secret's own rooms through secret.area",
            );
        }
    }

    async createRoom(params: CreateRoomParams): Promise<RoomNumber> {
        if (!this.#open) throw new TypeError("this mutateArea callback has finished");
        this.#roomFields();
        const roomNumber: RoomNumber = op_smudgy_mapper_reserve_room_number(
            this.#areaId,
            this.#token,
        );
        // Create-only submission: if this number exists by submission time
        // (another client won the race), the envelope is refused with
        // `roomNumberExists` and surfaces through mutateArea's thrown
        // error (committedOperations carries any acknowledged prefix),
        // never a silent merge into the other client's room.
        this.#record({
            create_room: { room_number: roomNumber, body: { ...params } },
        });
        return roomNumber;
    }

    async updateRoom(room: Room | RoomNumber, fields: UpdateRoomParams): Promise<void> {
        this.#roomFields();
        this.#record({
            upsert_room: {
                room_number: roomNumberInArea(this.#areaId, room),
                body: { ...fields },
            },
        });
    }

    async updateRooms(updates: [RoomNumber, UpdateRoomParams][]): Promise<void> {
        this.#roomFields();
        for (const [roomNumber, fields] of updates) {
            this.#record({
                upsert_room: { room_number: roomNumber, body: { ...fields } },
            });
        }
    }

    setRoomTitle(room: Room | RoomNumber, title: string): Promise<void> {
        return this.updateRoom(room, { title });
    }

    setRoomDescription(room: Room | RoomNumber, description: string): Promise<void> {
        return this.updateRoom(room, { description });
    }

    setRoomColor(room: Room | RoomNumber, color: string): Promise<void> {
        return this.updateRoom(room, { color });
    }

    setRoomLevel(room: Room | RoomNumber, level: number): Promise<void> {
        return this.updateRoom(room, { level });
    }

    setRoomX(room: Room | RoomNumber, x: number): Promise<void> {
        return this.updateRoom(room, { x });
    }

    setRoomY(room: Room | RoomNumber, y: number): Promise<void> {
        return this.updateRoom(room, { y });
    }

    setRoomExternalId(room: Room | RoomNumber, externalId: string): Promise<void> {
        return this.updateRoom(room, { externalId });
    }

    async setRoomProperty(room: Room | RoomNumber, name: string, value: string): Promise<void> {
        this.#record({
            upsert_room_property: {
                room_number: roomNumberInArea(this.#areaId, room),
                name,
                value,
            },
        });
    }

    async setAreaProperty(name: string, value: string): Promise<void> {
        this.#record({ upsert_area_property: { name, value } });
    }

    async deleteRoomProperty(room: Room | RoomNumber, name: string): Promise<void> {
        this.#record({
            delete_room_property: {
                room_number: roomNumberInArea(this.#areaId, room),
                name,
            },
        });
    }

    async deleteAreaProperty(name: string): Promise<void> {
        this.#record({ delete_area_property: { name } });
    }

    async addRoomTag(room: Room | RoomNumber, tag: string): Promise<void> {
        this.#record({
            add_room_tag: {
                room_number: roomNumberInArea(this.#areaId, room),
                tag,
            },
        });
    }

    async removeRoomTag(room: Room | RoomNumber, tag: string): Promise<void> {
        this.#record({
            remove_room_tag: {
                room_number: roomNumberInArea(this.#areaId, room),
                tag,
            },
        });
    }

    async createRoomExit(room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId> {
        const id: ExitId = op_smudgy_mapper_generate_id();
        this.#record({
            create_exit: {
                room_number: roomNumberInArea(this.#areaId, room),
                id,
                body: { ...exitArgsIn(exit) },
            },
        });
        return id;
    }

    async setRoomExit(
        room: Room | RoomNumber,
        exitId: ExitId,
        exit: ExitUpdates,
    ): Promise<void> {
        roomNumberInArea(this.#areaId, room);
        this.#record({ update_exit: { exit_id: exitId, body: { ...exitArgsIn(exit) } } });
    }

    async deleteRoom(room: Room | RoomNumber): Promise<void> {
        this.#roomFields();
        this.#record({
            delete_room: { room_number: roomNumberInArea(this.#areaId, room) },
        });
    }

    async deleteRoomExit(room: Room | RoomNumber, exitId: ExitId): Promise<void> {
        roomNumberInArea(this.#areaId, room);
        this.#record({ delete_exit: { exit_id: exitId } });
    }

    async createLink(link: LinkCreateArgs): Promise<ConnectionId> {
        const connectionId: ConnectionId = op_smudgy_mapper_generate_id();
        const body = connectionIn(link);
        this.#record({
            create_link: {
                connection_id: connectionId,
                body: { ...body, traversals: body.traversals.map((value) => ({ ...value })) },
            },
        });
        return connectionId;
    }

    async setConnection(connectionId: ConnectionId, updates: ConnectionUpdates): Promise<void> {
        this.#record({
            update_connection: { connection_id: connectionId, body: { ...connectionIn(updates) } },
        });
    }

    finish(): AreaBatchOperation[] {
        if (!this.#open) throw new TypeError("this mutateArea callback has finished");
        this.#open = false;
        return this.#operations;
    }

    abort(): void {
        this.#open = false;
        this.#operations = [];
    }

    /** Return this mutator's reserved room numbers to the allocator.
     * Idempotent; committed drafts already occupy their numbers by the time
     * this runs, so releasing after submission frees nothing in use. */
    release(): void {
        op_smudgy_mapper_release_room_reservations(this.#areaId, this.#token);
    }
}

// A label/shape id: a canonical UUID string, like `AreaId`/`ExitId`.
type LabelId = string & { readonly __id: "LabelId" };
type ShapeId = string & { readonly __id: "ShapeId" };

// Text alignment of a label; a shape's kind. These mirror the cloud enums' variant names.
type LabelHorizontalAlign = "Left" | "Center" | "Right";
type LabelVerticalAlign = "Top" | "Center" | "Bottom";
type ShapeKind = "Rectangle" | "RoundedRectangle";

// A text label read back from an area (`area.labels`). Mirrors the `Label` contract interface.
interface Label {
    readonly id: LabelId;
    readonly level: number;
    readonly x: number;
    readonly y: number;
    readonly width: number;
    readonly height: number;
    readonly horizontalAlignment: LabelHorizontalAlign;
    readonly verticalAlignment: LabelVerticalAlign;
    readonly text: string;
    readonly color: string;
    readonly backgroundColor: string;
    readonly fontSize: number;
    readonly fontWeight: number;
}

// Fields accepted when creating a label (`createLabel`); position, size, and `text` are
// required, everything else defaults host-side (level 0, Center/Center, "#ffffff", 16, 400).
interface LabelArgs {
    x: number;
    y: number;
    width: number;
    height: number;
    text: string;
    level?: number;
    horizontalAlignment?: LabelHorizontalAlign;
    verticalAlignment?: LabelVerticalAlign;
    color?: string;
    backgroundColor?: string;
    fontSize?: number;
    fontWeight?: number;
}

// Fields accepted when updating a label (`setLabel`). Any omitted field is left unchanged.
interface LabelUpdates {
    x?: number;
    y?: number;
    width?: number;
    height?: number;
    text?: string;
    level?: number;
    horizontalAlignment?: LabelHorizontalAlign;
    verticalAlignment?: LabelVerticalAlign;
    color?: string;
    backgroundColor?: string;
    fontSize?: number;
    fontWeight?: number;
}

// A graphical shape read back from an area (`area.shapes`). Mirrors the `Shape` contract interface.
interface Shape {
    readonly id: ShapeId;
    readonly level: number;
    readonly x: number;
    readonly y: number;
    readonly width: number;
    readonly height: number;
    readonly backgroundColor: string | null;
    readonly strokeColor: string | null;
    readonly shapeType: ShapeKind;
    readonly borderRadius: number;
    readonly strokeWidth: number;
}

// Fields accepted when creating a shape (`createShape`); position and size are required,
// everything else defaults host-side (level 0, "Rectangle", radius 0).
interface ShapeArgs {
    x: number;
    y: number;
    width: number;
    height: number;
    level?: number;
    backgroundColor?: string;
    strokeColor?: string;
    shapeType?: ShapeKind;
    borderRadius?: number;
    strokeWidth?: number;
}

// Fields accepted when updating a shape (`setShape`). Any omitted field is left unchanged.
interface ShapeUpdates {
    x?: number;
    y?: number;
    width?: number;
    height?: number;
    level?: number;
    backgroundColor?: string;
    strokeColor?: string;
    shapeType?: ShapeKind;
    borderRadius?: number;
    strokeWidth?: number;
}

// A portable area JSON blob produced by `exportArea` and consumed by `importArea`/`importAreas`.
// Treat it as opaque: round-trip it (export -> store -> import) without introspecting its shape.
type AreaJson = Record<string, unknown>;

// One source of `mergeAreas` and one entry of its result. Mirror the `MergeAreaSource` and
// `MergedRoom` interfaces in the published contract.
interface MergeAreaSource {
    area: Area | AreaId;
    /** Nonempty list: move these rooms but keep the source, labels, shapes and area properties,
     * even if all rooms are listed. Omit to move everything and delete the source. */
    rooms?: RoomNumber[];
    /** Added only to moved content; omitted axes are 0. Coordinates/results must be finite
     * and levels must fit signed 32-bit integers. */
    translate?: { x?: number; y?: number; level?: number };
}
interface MergedRoom {
    readonly from: { area: AreaId; room: RoomNumber };
    readonly to: RoomNumber;
}
class Atlas {
    constructor(
        readonly id: AtlasId,
        readonly name: string,
    ) {}

    /** Live tier read. `moveAtlas` replaces the atlas with a new id, so the
     * old source handle becomes invalid; use the handle returned by the move. */
    get storage(): MapStorage {
        return op_smudgy_mapper_get_atlas_storage(this.id);
    }

    toString() {
        return this.name;
    }
}

class Area {
    #obj: any;

    constructor(obj: any) {
        this.#obj = obj;
    }

    get id(): AreaId {
        return op_smudgy_mapper_get_area_id(this.#obj);
    }

    /**
     * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
     * `id` is that string now -- this is an alias for it, and reading it
     * reports the replacement once per isolate.
     */
    get uuid(): string {
        op_smudgy_mapper_warn_area_uuid_once();
        return this.id;
    }

    get name(): string {
        return op_smudgy_mapper_get_area_name(this.#obj);
    }

    /** For a Secret's area (or your Private additions'), the map it belongs to;
     * `undefined` for a map. */
    get mapId(): AreaId | undefined {
        return op_smudgy_mapper_get_area_map_id(this.#obj) ?? undefined;
    }

    get roomNumbers(): RoomNumber[] {
        return op_smudgy_mapper_list_area_room_numbers(this.#obj) || [];
    }

    /**
     * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
     * Use `storage === "session"` instead.
     */
    get isEphemeral(): boolean {
        return op_smudgy_mapper_get_area_is_ephemeral(this.#obj) === true;
    }

    get storage(): MapStorage {
        return op_smudgy_mapper_get_area_storage(this.#obj);
    }

    get nextRoomNumber(): RoomNumber {
        return op_smudgy_mapper_get_area_next_room_number(this.#obj);
    }

    room(roomNumber: number): Room | undefined {
        const room: Room | undefined = op_smudgy_mapper_get_area_room_by_number(this.#obj, roomNumber);
        return room && new Room(room);
    }

    /** Read a custom property of the area's own place (or `undefined` if unset). */
    data(key: string): string | undefined {
        return op_smudgy_mapper_get_area_property(this.#obj, key);
    }

    /** Where this area lives: `"map"` for a map, the Secret or `"private"` for one of
     * their own areas. */
    get place(): MapPlace {
        return placeOf(op_smudgy_mapper_area_place(this.#obj));
    }

    /** The map's places: `"map"`, each Secret you read in layer order, then `"private"` on a
     * cloud map. A Secret's or Private additions' own area has its own place alone. */
    get places(): MapPlace[] {
        return op_smudgy_mapper_list_area_places(this.#obj).map(placeFrom);
    }

    /** One place's data on this map, and the searches narrowed to that place. */
    in(place: MapPlaceLike): AreaView {
        const key = placeKey(place);
        op_smudgy_mapper_check_place(this.id, key, false);
        return new AreaView(this, this.#obj, key);
    }

    /** The map's Secrets. */
    get secrets(): SecretRegistry {
        return new SecretRegistry(this, this.#obj);
    }

    /** This area's rooms whose `name` property is exactly `value`, in any place you read or
     * the one `options.in` names. One indexed lookup per place, however many rooms the area
     * has. An area answers for itself even when you have turned its map off -- naming it is
     * asking for it. */
    findRoomsByProperty(name: string, value: string, options?: PlaceOptions): Room[] {
        return hydrateRooms(
            op_smudgy_mapper_find_area_rooms_by_property(this.#obj, name, value, optionalPlaceKey(options)),
        );
    }

    /** This area's rooms carrying a property called `name`, whatever its value, in any
     * place you read or the one `options.in` names. */
    findRoomsWithProperty(name: string, options?: PlaceOptions): Room[] {
        return hydrateRooms(
            op_smudgy_mapper_find_area_rooms_with_property(this.#obj, name, optionalPlaceKey(options)),
        );
    }

    /** This area's rooms carrying `tag` (case-insensitive) in any place you read or the one
     * `options.in` names, in no particular order. */
    findRoomsWithTag(tag: string, options?: PlaceOptions): Room[] {
        return hydrateRooms(
            op_smudgy_mapper_find_area_rooms_with_tag(this.#obj, tag, optionalPlaceKey(options)),
        );
    }

    /** This area's text labels. */
    get labels(): Label[] {
        return op_smudgy_mapper_get_area_labels(this.#obj).map((label: Label) =>
            withSnakeCaseAliases(label, LABEL_PROTOTYPE)
        );
    }

    /** This area's graphical shapes. */
    get shapes(): Shape[] {
        return op_smudgy_mapper_get_area_shapes(this.#obj).map((shape: Shape) =>
            withSnakeCaseAliases(shape, SHAPE_PROTOTYPE)
        );
    }

    /** This area's shared link geometry and appearance records. */
    get connections(): Connection[] {
        return op_smudgy_mapper_get_area_connections(this.#obj).map(connectionOut);
    }

    toString() {
        return this.#obj.toString();
    }
}

class Room {
    #obj: any;

    constructor(obj: any) {
        this.#obj = obj;
    }

    get roomNumber(): RoomNumber {
        return op_smudgy_mapper_get_room_number(this.#obj);
    }

    get areaId(): AreaId {
        return op_smudgy_mapper_get_room_area_id(this.#obj);
    }

    get title(): string {
        return op_smudgy_mapper_get_room_title(this.#obj);
    }

    get externalId(): string | undefined {
        return op_smudgy_mapper_get_room_external_id(this.#obj) ?? undefined;
    }

    get description(): string {
        return op_smudgy_mapper_get_room_description(this.#obj);
    }

    get level(): number {
        return op_smudgy_mapper_get_room_level(this.#obj);
    }

    get x(): number {
        return op_smudgy_mapper_get_room_x(this.#obj);
    }

    get y(): number {
        return op_smudgy_mapper_get_room_y(this.#obj);
    }

    get color(): string {
        return op_smudgy_mapper_get_room_color(this.#obj);
    }

    get exits(): Exit[] {
        return op_smudgy_mapper_get_room_exits(this.#obj).map((exit: ExitWire) => new MapExit(exit));
    }

    /** Read a custom property of the room's own place (or `undefined` if unset). */
    data(key: string): string | undefined {
        return op_smudgy_mapper_get_room_property(this.#obj, key);
    }

    /** Where this room lives: `"map"` for a map's room, the Secret or `"private"` for one of
     * their own rooms. */
    get place(): MapPlace {
        return placeOf(op_smudgy_mapper_room_place(this.#obj));
    }

    /** One place's data on this room. */
    in(place: MapPlaceLike): RoomView {
        const key = placeKey(place);
        op_smudgy_mapper_check_place(this.areaId, key, true);
        return new RoomView(this, this.#obj, key);
    }

    /** Every readable place's value for `key` on this room (every property, with its key, when
     * `key` is omitted): the room's own place first, then the map's places in order. */
    combinedData(key: string): RoomPlaceData[];
    combinedData(): RoomPlaceEntry[];
    combinedData(key?: string): RoomPlaceData[] | RoomPlaceEntry[] {
        const served: { place: string; key: string; data: string }[] =
            op_smudgy_mapper_room_combined_data(this.#obj, key ?? null);
        const place = placeCache();
        if (key === undefined) {
            return served.map((entry) =>
                Object.freeze({ source: place(entry.place), key: entry.key, data: entry.data })
            );
        }
        return served.map((entry) => Object.freeze({ source: place(entry.place), data: entry.data }));
    }

    /** Each readable place's tags for this room, the room's own place first. */
    combinedTags(): RoomPlaceTag[] {
        const served: [string, string][] = op_smudgy_mapper_room_combined_tags(this.#obj);
        const place = placeCache();
        return served.map(([key, tag]) => Object.freeze({ source: place(key), tag }));
    }

    /** Every readable place's tags for this room together, normalized to UPPERCASE and
     * sorted. Each tag still lives in one place (`combinedTags()` says which): writing this
     * list back with `mapper.addRoomTag` copies every other place's tags into the room's own
     * place. To tag the room in one place, use `room.in(place).addTag(tag)`. */
    get tags(): string[] {
        return op_smudgy_mapper_get_room_tags(this.#obj);
    }

    /** Whether any readable place tags this room with `tag` (case-insensitive). */
    hasTag(tag: string): boolean {
        return op_smudgy_mapper_has_tag(this.#obj, String(tag));
    }

    /** Update multiple fields of this room in one cache update. Convenience over
     * `mapper.updateRoom(this.areaId, this.roomNumber, fields)`; only the present fields
     * change. */
    update(fields: UpdateRoomParams): Promise<OperationId | null> {
        return op_smudgy_mapper_update_room(this.areaId, this.roomNumber, fields);
    }

    toString() {
        return this.#obj.toString();
    }
}

defineSnakeCaseAliases(Area.prototype, SNAKE_CASE_NAMES.area);
defineSnakeCaseAliases(Room.prototype, SNAKE_CASE_NAMES.room);

// smudgy.ts loads before this extension and exposes a one-shot private registrar. Hand the
// public values to its lexical facade without publishing them on globalThis.
const installMapper = (globalThis as any).__smudgy_install_mapper;
if (typeof installMapper !== "function") {
    throw new TypeError("smudgy mapper registrar is unavailable");
}
installMapper(mapper, Area, MutateAreaError);

// Drift-guard surface for `mapper_ts_impl_conforms_to_contract` (models/script_typings.rs):
// these TYPE-ONLY exports let the conformance test assert this runtime impl satisfies the
// published `smudgy-mapper.d.ts` contract (`Mapper`/`Area`/`Room`/`Exit`). They are fully
// erased -- the session reaches the API through the private handoff above, never these.
export type MapperImpl = typeof mapper;
export type AreaConstructorImpl = typeof Area;
export type MutateAreaErrorConstructorImpl = typeof MutateAreaError;
export type AreaImpl = Area;
export type RoomImpl = Room;
export type ExitImpl = Exit;
export type ConnectionImpl = Connection;
export type SecretImpl = Secret;
export type SecretRegistryImpl = SecretRegistry;
export type RoomViewImpl = RoomView;
export type AreaViewImpl = AreaView;
