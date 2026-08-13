// Ops are imported from the global op module "ext:core/ops" (deno's modern
// convention; its own extensions do the same, e.g. 40_process.ts imports
// op_create_worker from here). Deno.core.ops is built at bootstrap and does
// NOT include runtime-registered extension ops like ours, so importing from
// "ext:core/ops" (generated from the full op table) is the correct path.
// NOTE: extension source must be 7-bit ASCII (deno_core extensions.rs check).
import {
    op_smudgy_mapper_set_current_location,
    op_smudgy_mapper_get_current_location,
    op_smudgy_mapper_list_area_ids,
    op_smudgy_mapper_refresh_areas,
    op_smudgy_mapper_list_area_room_numbers,
    op_smudgy_mapper_list_rooms_by_title_and_description,
    op_smudgy_mapper_list_rooms_by_title_description_and_visible_exits,
    op_smudgy_mapper_create_area,
    op_smudgy_mapper_get_area_storage,
    op_smudgy_mapper_get_atlas_storage,
    op_smudgy_mapper_list_atlases,
    op_smudgy_mapper_create_atlas,
    op_smudgy_mapper_relocate_areas,
    op_smudgy_mapper_relocate_atlas,
    op_smudgy_mapper_delete_area,
    op_smudgy_mapper_get_area_is_ephemeral,
    op_smudgy_mapper_rename_area,
    op_smudgy_mapper_get_area_by_id,
    op_smudgy_mapper_get_area_name,
    op_smudgy_mapper_get_area_id,
    op_smudgy_mapper_get_area_uuid,
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
    // @ts-ignore - ext:core/ops is a deno virtual module with no type decls
} from "ext:core/ops";

// These declarations MIRROR the published author-facing contract in
// `core/src/models/script_typings/smudgy-mapper.d.ts` (the global ambient map types). The
// `mapper_ts_impl_conforms_to_contract` drift guard in `models/script_typings.rs` compiles
// this impl against that contract, so the two cannot silently diverge -- edit both together.
//
// An `AreaId`/`ExitId` is a 2-element `[hi, lo]` pair of a UUID's 64-bit halves as plain JS
// numbers (the ops serialize the `u64` pair to f64). It is an OPAQUE handle: pass it back to
// mapper methods unchanged; each half exceeds 2^53, so the numbers are not exact.
type AreaId = readonly [number, number];
type AtlasId = readonly [number, number];
type RoomNumber = number;
type ExitId = readonly [number, number];
type ConnectionId = readonly [number, number];
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
    atlas?: Atlas | AtlasId;
    /**
     * @deprecated Supported through Smudgy 0.5.x; removed in 0.6.0.
     * Use `storage: "session"` instead.
     */
    ephemeral?: boolean;
}

type MapStorage = "session" | "local" | "cloud";

interface MapDestination {
    storage: MapStorage;
    atlas?: Atlas | AtlasId;
}

interface CreateAtlasOptions {
    storage: "local" | "cloud";
}

interface MutateAreaOptions {
    description?: string;
}

/** An opaque id pair as the ops accept it: a 2-element array of numbers
 * (a UUID's 64-bit halves). */
function isIdPair(value: unknown): value is readonly [number, number] {
    return (
        Array.isArray(value) &&
        value.length === 2 &&
        typeof value[0] === "number" &&
        typeof value[1] === "number"
    );
}

/** Unwrap an atlas argument structurally. The contract `Atlas` type is an
 * interface, so callers may legitimately hold plain objects (a spread or a
 * JSON round-trip of a handle) rather than this module's class; anything
 * carrying a valid id pair is accepted, and anything else fails here with a
 * clear TypeError instead of an opaque serde error inside the op. */
function atlasIdOf(atlas: Atlas | AtlasId | undefined): AtlasId | undefined {
    if (atlas === undefined) return undefined;
    if (atlas instanceof Atlas) return atlas.id;
    if (isIdPair(atlas)) return atlas;
    const id = (atlas as Atlas).id;
    if (isIdPair(id)) return id;
    throw new TypeError(
        "expected an Atlas handle or an AtlasId [hi, lo] pair",
    );
}

function areaIdOf(area: Area | AreaId): AreaId {
    return area instanceof Area ? area.id : area;
}

function destinationForOp(destination: MapDestination) {
    return {
        storage: destination.storage,
        atlas_id: atlasIdOf(destination.atlas),
    };
}

const mapper = {
    /** Refresh every visible area from durable storage. Use this before a
     * presence-based package upsert that can run during startup or after a
     * mapping-owner handoff. Requires `mapper:read`. */
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
        op_smudgy_mapper_set_current_location(areaId, roomNumber);
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

    getAreaById(id: AreaId) {
        let area = op_smudgy_mapper_get_area_by_id(id);
        return new Area(area);
    },

    /** Collect related writes to one area and submit them in the fewest practical
     * ordered envelopes. The whole callback is validated and durably staged before
     * anything is published, so a locally invalid batch submits nothing even across
     * an envelope split. Each emitted envelope is atomic at the backend; if a later
     * envelope fails after earlier ones were acknowledged, the thrown Error carries
     * the acknowledged prefix as `committedOperations` (acknowledged envelopes are
     * never rolled back). Draft room numbers are reserved host-side for the life of
     * the callback (see AreaMutator.createRoom). */
    async mutateArea(
        area: Area | AreaId,
        callback: (mutation: AreaMutator) => void | Promise<void>,
        options?: MutateAreaOptions,
    ): Promise<OperationId[]> {
        // Always start from the current host snapshot. A script may retain an Area
        // wrapper across prior writes, including a now-stale next_room_number.
        const target = this.getAreaById(area instanceof Area ? area.id : area);
        const mutation = new AreaMutator(target);
        try {
            await callback(mutation);
            const outcome: { committed: OperationId[]; error: string | null } =
                await op_smudgy_mapper_mutate_area(
                    target.id,
                    mutation.finish(),
                    options?.description ?? "Scripted area mutation",
                );
            if (outcome.error !== null && outcome.error !== undefined) {
                const failure = new Error(outcome.error);
                (failure as any).committedOperations = outcome.committed;
                throw failure;
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

    renameArea(area: Area | AreaId, name: string): Promise<void> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_rename_area(areaId, name);
    },

    deleteArea(area: Area | AreaId): Promise<void> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_delete_area(areaId);
    },

    setRoomTitle(area: Area | AreaId, room: Room | RoomNumber, title: string): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_title(areaId, roomNumber, title);
    },

    setRoomDescription(area: Area | AreaId, room: Room | RoomNumber, description: string): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_description(areaId, roomNumber, description);
    },

    setRoomColor(area: Area | AreaId, room: Room | RoomNumber, color: string): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_color(areaId, roomNumber, color);
    },

    setRoomLevel(area: Area | AreaId, room: Room | RoomNumber, level: number): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_level(areaId, roomNumber, level);
    },

    setRoomX(area: Area | AreaId, room: Room | RoomNumber, x: number): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_x(areaId, roomNumber, x);
    },

    setRoomY(area: Area | AreaId, room: Room | RoomNumber, y: number): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_y(areaId, roomNumber, y);
    },

    setRoomProperty(area: Area | AreaId, room: Room | RoomNumber, name: string, value: string): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_property(areaId, roomNumber, name, value);
    },

    /** Set a custom data property on an area (the write counterpart of `area.data(key)`). Pass an
     * empty value to clear it. Requires the `mapper:write` capability. */
    setAreaProperty(area: Area | AreaId, name: string, value: string): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_set_area_property(areaId, name, value);
    },

    /** Add a case-insensitive tag to a room. The tag is normalized to UPPERCASE;
     * re-adding an existing tag is a no-op. Requires the `mapper:write` capability. */
    addRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_add_room_tag(areaId, roomNumber, tag);
    },

    /** Remove a tag from a room (case-insensitive). Requires `mapper:write`. */
    removeRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_remove_room_tag(areaId, roomNumber, tag);
    },

    /** The nearest reachable room carrying `tag` (case-insensitive) from `from`,
     * by the same weighted graph search as `getPathBetweenRooms` (the start room
     * counts if it carries the tag), or `undefined` if none is reachable. Path to
     * it with `getPathBetweenRooms`. Requires `mapper:read`. */
    findNearestRoomWithTag(from: Room, tag: string): Room | undefined {
        return this.findNearestRoomWithTags(from, { all: [tag] });
    },

    /** The nearest reachable room whose tags satisfy a conjunctive filter: has
     * every tag in `all` and none in `none` (all case-insensitive), or
     * `undefined` if none is reachable. The filter is evaluated in Rust during the
     * search, so it is cheap even over large maps. An empty filter returns
     * `undefined`. Requires `mapper:read`. */
    findNearestRoomWithTags(
        from: Room,
        filter: { all?: string[]; none?: string[] },
    ): Room | undefined {
        const ref = op_smudgy_mapper_find_nearest_room_with_tags(
            from.area_id,
            from.room_number,
            filter.all ?? [],
            filter.none ?? [],
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
        const areaId = area instanceof Area ? area.id : area;
        const ref = op_smudgy_mapper_find_nearest_room_in_area(
            from.area_id,
            from.room_number,
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
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_external_id(areaId, roomNumber, externalId);
    },

    createRoom(area: Area | AreaId, params: CreateRoomParams): Promise<RoomNumber> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_create_room(areaId, params);
    },

    /** Update multiple fields of an existing room in ONE cache update (one index rebuild)
     * instead of one per field. Only the fields present in `fields` change. */
    updateRoom(area: Area | AreaId, room: Room | RoomNumber, fields: UpdateRoomParams): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_update_room(areaId, roomNumber, fields);
    },

    /** Batch-update many rooms of one area in a single cache update. Each entry is a
     * `[roomNumber, fields]` pair; only the present fields of each change. */
    updateRooms(area: Area | AreaId, updates: [RoomNumber, UpdateRoomParams][]): Promise<OperationId[]> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_update_rooms(areaId, updates);
    },

    createRoomExit(area: Area | AreaId, room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_create_room_exit(areaId, roomNumber, exit);
    },
    /** Update an existing exit and resolve only after the map backend
     * acknowledges the exact mutation. Equal updates resolve to `null`
     * without sending a revision-bumping no-op. */
    setRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId, exit: ExitUpdates): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_set_room_exit(areaId, roomNumber, exitId, exit);
    },
    /** Merge `remove` into `keep` as one durable area mutation. The kept
     * room's metadata wins; traversal is deduplicated and rewired. Resolves
     * only after the backend acknowledges the exact operation. */
    mergeRooms(area: Area | AreaId, keep: Room | RoomNumber, remove: Room | RoomNumber): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const keepRoomNumber = keep instanceof Room ? keep.room_number : keep;
        const removeRoomNumber = remove instanceof Room ? remove.room_number : remove;
        return op_smudgy_mapper_merge_rooms(areaId, keepRoomNumber, removeRoomNumber);
    },
    deleteRoom(area: Area | AreaId, room: Room | RoomNumber): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_delete_room(areaId, roomNumber);
    },
    deleteRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_delete_room_exit(areaId, roomNumber, exitId);
    },
    /** Atomically create one Connection and its one or two member traversals. */
    createLink(area: Area | AreaId, link: LinkCreateArgs): Promise<ConnectionId> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_create_link(areaId, link);
    },
    /** Update shared Connection geometry or appearance. */
    setConnection(area: Area | AreaId, connectionId: ConnectionId, updates: ConnectionUpdates): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_set_connection(areaId, connectionId, updates);
    },
    /** Split one traversal out of a bidirectional Connection. */
    unlinkRoomExit(area: Area | AreaId, exitId: ExitId): Promise<ConnectionId> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_unlink_exit(areaId, exitId);
    },
    /** Merge reciprocal one-way Connections, preserving `keepConnectionId`'s route. */
    pairConnections(area: Area | AreaId, keepConnectionId: ConnectionId, mergeConnectionId: ConnectionId): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_pair_connections(areaId, keepConnectionId, mergeConnectionId);
    },
    /** Delete a Connection and all of its member traversals. */
    deleteLink(area: Area | AreaId, connectionId: ConnectionId): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_delete_link(areaId, connectionId);
    },
    /** Add a text label to an area; returns its new id. Requires `mapper:write`. */
    createLabel(area: Area | AreaId, label: LabelArgs): Promise<LabelId> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_create_label(areaId, label);
    },
    /** Add a graphical shape to an area; returns its new id. Requires `mapper:write`. */
    createShape(area: Area | AreaId, shape: ShapeArgs): Promise<ShapeId> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_create_shape(areaId, shape);
    },
    /** Delete a label from an area. Requires `mapper:write`. */
    deleteLabel(area: Area | AreaId, labelId: LabelId): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_delete_label(areaId, labelId);
    },
    /** Delete a shape from an area. Requires `mapper:write`. */
    deleteShape(area: Area | AreaId, shapeId: ShapeId): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_delete_shape(areaId, shapeId);
    },
    /** Update an existing label; only present fields change. Requires `mapper:write`. */
    setLabel(area: Area | AreaId, labelId: LabelId, updates: LabelUpdates): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_set_label(areaId, labelId, updates);
    },
    /** Update an existing shape; only present fields change. Requires `mapper:write`. */
    setShape(area: Area | AreaId, shapeId: ShapeId, updates: ShapeUpdates): Promise<OperationId | null> {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_set_shape(areaId, shapeId, updates);
    },
    /** Serialize an area to a portable JSON blob. Requires `mapper:read` and copy rights
     * (`can_copy`) on the area. */
    exportArea(area: Area | AreaId): Promise<AreaJson> {
        const areaId = area instanceof Area ? area.id : area;
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
    readonly from_direction: ExitDirection;
    readonly from_area_id: AreaId;
    readonly from_room_number: RoomNumber;
    readonly to_direction: ExitDirection | null;
    readonly to_area_id: AreaId | null;
    readonly to_room_number: RoomNumber | null;
    readonly is_hidden: boolean;
    readonly is_closed: boolean;
    readonly is_locked: boolean;
    readonly weight: number;
    readonly command: string | null;
}

// Fields accepted when creating an exit (`createRoomExit`); `from_direction` is required.
// Visual appearance (routing, dash, color, thickness) lives on the shared
// Connection, not the exit.
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

// Fields accepted when updating an exit (`setRoomExit`). Any omitted field is left unchanged.
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
    room_number: RoomNumber;
    side: RoomSide;
    port_offset: number;
    port_mode: PortMode;
}

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

interface LinkTraversalArgs extends ExitArgs {
    room_number: RoomNumber;
}

interface LinkCreateArgs extends ConnectionUpdates {
    endpoint_a: ConnectionEndpoint;
    endpoint_b?: ConnectionEndpoint;
    traversals: LinkTraversalArgs[];
}

type AreaBatchOperation =
    | { upsert_room: { room_number: RoomNumber; body: CreateRoomParams } }
    | { create_room: { room_number: RoomNumber; body: CreateRoomParams } }
    | { delete_room: { room_number: RoomNumber } }
    | { upsert_room_property: { room_number: RoomNumber; name: string; value: string } }
    | { upsert_area_property: { name: string; value: string } }
    | { add_room_tag: { room_number: RoomNumber; tag: string } }
    | { remove_room_tag: { room_number: RoomNumber; tag: string } }
    | { create_exit: { room_number: RoomNumber; id: ExitId; body: ExitArgs } }
    | { update_exit: { exit_id: ExitId; body: ExitUpdates } }
    | { delete_exit: { exit_id: ExitId } }
    | { create_link: { connection_id: ConnectionId; body: LinkCreateArgs } }
    | { update_connection: { connection_id: ConnectionId; body: ConnectionUpdates } };

function roomNumberInArea(areaId: AreaId, room: Room | RoomNumber): RoomNumber {
    if (!(room instanceof Room)) return room;
    if (room.area_id[0] !== areaId[0] || room.area_id[1] !== areaId[1]) {
        throw new TypeError("mutateArea cannot edit a room from another area");
    }
    return room.room_number;
}

/** A callback-scoped write collector. Its methods preserve the familiar async
 * mapper shape, but only record draft operations; the host is touched once the
 * callback completes. Draft room numbers are reserved against the host's live
 * allocator under a per-mutator token, so ambient creates cannot collide with
 * a draft; the reservation is released when the mutator finishes or aborts. */
class AreaMutator {
    readonly #areaId: AreaId;
    readonly #token: readonly [number, number];
    #operations: AreaBatchOperation[] = [];
    #open = true;

    constructor(area: Area) {
        this.#areaId = area.id;
        this.#token = op_smudgy_mapper_generate_id();
    }

    #record(operation: AreaBatchOperation): void {
        if (!this.#open) throw new TypeError("this mutateArea callback has finished");
        this.#operations.push(operation);
    }

    async createRoom(params: CreateRoomParams): Promise<RoomNumber> {
        if (!this.#open) throw new TypeError("this mutateArea callback has finished");
        const roomNumber: RoomNumber = op_smudgy_mapper_reserve_room_number(
            this.#areaId,
            this.#token,
        );
        // Create-only submission: if this number exists by submission time
        // (another client won the race), the envelope is refused with
        // `room_number_exists` and surfaces through mutateArea's thrown
        // error (committedOperations carries any acknowledged prefix),
        // never a silent merge into the other client's room.
        this.#record({
            create_room: { room_number: roomNumber, body: { ...params } },
        });
        return roomNumber;
    }

    async updateRoom(room: Room | RoomNumber, fields: UpdateRoomParams): Promise<void> {
        this.#record({
            upsert_room: {
                room_number: roomNumberInArea(this.#areaId, room),
                body: { ...fields },
            },
        });
    }

    async updateRooms(updates: [RoomNumber, UpdateRoomParams][]): Promise<void> {
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
                body: { ...exit },
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
        this.#record({ update_exit: { exit_id: exitId, body: { ...exit } } });
    }

    async deleteRoom(room: Room | RoomNumber): Promise<void> {
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
        this.#record({
            create_link: {
                connection_id: connectionId,
                body: { ...link, traversals: link.traversals.map((value) => ({ ...value })) },
            },
        });
        return connectionId;
    }

    async setConnection(connectionId: ConnectionId, updates: ConnectionUpdates): Promise<void> {
        this.#record({
            update_connection: { connection_id: connectionId, body: { ...updates } },
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

// A label/shape id: a 2-element `[hi, lo]` UUID pair, like `AreaId`/`ExitId`. Opaque.
type LabelId = readonly [number, number];
type ShapeId = readonly [number, number];

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
    readonly horizontal_alignment: LabelHorizontalAlign;
    readonly vertical_alignment: LabelVerticalAlign;
    readonly text: string;
    readonly color: string;
    readonly background_color: string;
    readonly font_size: number;
    readonly font_weight: number;
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
    horizontal_alignment?: LabelHorizontalAlign;
    vertical_alignment?: LabelVerticalAlign;
    color?: string;
    background_color?: string;
    font_size?: number;
    font_weight?: number;
}

// Fields accepted when updating a label (`setLabel`). Any omitted field is left unchanged.
interface LabelUpdates {
    x?: number;
    y?: number;
    width?: number;
    height?: number;
    text?: string;
    level?: number;
    horizontal_alignment?: LabelHorizontalAlign;
    vertical_alignment?: LabelVerticalAlign;
    color?: string;
    background_color?: string;
    font_size?: number;
    font_weight?: number;
}

// A graphical shape read back from an area (`area.shapes`). Mirrors the `Shape` contract interface.
interface Shape {
    readonly id: ShapeId;
    readonly level: number;
    readonly x: number;
    readonly y: number;
    readonly width: number;
    readonly height: number;
    readonly background_color: string | null;
    readonly stroke_color: string | null;
    readonly shape_type: ShapeKind;
    readonly border_radius: number;
    readonly stroke_width: number;
}

// Fields accepted when creating a shape (`createShape`); position and size are required,
// everything else defaults host-side (level 0, "Rectangle", radius 0).
interface ShapeArgs {
    x: number;
    y: number;
    width: number;
    height: number;
    level?: number;
    background_color?: string;
    stroke_color?: string;
    shape_type?: ShapeKind;
    border_radius?: number;
    stroke_width?: number;
}

// Fields accepted when updating a shape (`setShape`). Any omitted field is left unchanged.
interface ShapeUpdates {
    x?: number;
    y?: number;
    width?: number;
    height?: number;
    level?: number;
    background_color?: string;
    stroke_color?: string;
    shape_type?: ShapeKind;
    border_radius?: number;
    stroke_width?: number;
}

// A portable area JSON blob produced by `exportArea` and consumed by `importArea`/`importAreas`.
// Treat it as opaque: round-trip it (export -> store -> import) without introspecting its shape.
type AreaJson = Record<string, unknown>;
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

    /** The area id as its canonical hyphenated lowercase UUID string. */
    get uuid(): string {
        return op_smudgy_mapper_get_area_uuid(this.#obj);
    }

    get name(): string {
        return op_smudgy_mapper_get_area_name(this.#obj);
    }

    get room_numbers(): RoomNumber[] {
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

    get next_room_number(): RoomNumber {
        return op_smudgy_mapper_get_area_next_room_number(this.#obj);
    }

    room(roomNumber: number): Room | undefined {
        const room: Room | undefined = op_smudgy_mapper_get_area_room_by_number(this.#obj, roomNumber);
        return room && new Room(room);
    }

    data(key: string): string | undefined {
        return op_smudgy_mapper_get_area_property(this.#obj, key);
    }

    /** This area's text labels. */
    get labels(): Label[] {
        return op_smudgy_mapper_get_area_labels(this.#obj);
    }

    /** This area's graphical shapes. */
    get shapes(): Shape[] {
        return op_smudgy_mapper_get_area_shapes(this.#obj);
    }

    /** This area's shared link geometry and appearance records. */
    get connections(): Connection[] {
        return op_smudgy_mapper_get_area_connections(this.#obj);
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

    get room_number(): RoomNumber {
        return op_smudgy_mapper_get_room_number(this.#obj);
    }

    get area_id(): AreaId {
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
        return op_smudgy_mapper_get_room_exits(this.#obj);
    }

    data(key: string): string | undefined {
        return op_smudgy_mapper_get_room_property(this.#obj, key);
    }

    /** This room's tags, normalized to UPPERCASE and sorted. */
    get tags(): string[] {
        return op_smudgy_mapper_get_room_tags(this.#obj);
    }

    /** Whether this room carries `tag` (case-insensitive). */
    hasTag(tag: string): boolean {
        return op_smudgy_mapper_has_tag(this.#obj, tag);
    }

    /** Update multiple fields of this room in one cache update. Convenience over
     * `mapper.updateRoom(this.area_id, this.room_number, fields)`; only the present fields
     * change. */
    update(fields: UpdateRoomParams): Promise<OperationId | null> {
        return op_smudgy_mapper_update_room(this.area_id, this.room_number, fields);
    }

    toString() {
        return this.#obj.toString();
    }
}

// smudgy.ts loads before this extension and exposes a one-shot private registrar. Hand the
// public values to its lexical facade instead of publishing `mapper` or `Area` on globalThis.
const installMapper = (globalThis as any).__smudgy_install_mapper;
if (typeof installMapper !== "function") {
    throw new TypeError("smudgy mapper registrar is unavailable");
}
installMapper(mapper, Area);

// Drift-guard surface for `mapper_ts_impl_conforms_to_contract` (models/script_typings.rs):
// these TYPE-ONLY exports let the conformance test assert this runtime impl satisfies the
// published `smudgy-mapper.d.ts` contract (`Mapper`/`Area`/`Room`/`Exit`). They are fully
// erased -- the session reaches the API through the private handoff above, never these.
export type MapperImpl = typeof mapper;
export type AreaConstructorImpl = typeof Area;
export type AreaImpl = Area;
export type RoomImpl = Room;
export type ExitImpl = Exit;
export type ConnectionImpl = Connection;
