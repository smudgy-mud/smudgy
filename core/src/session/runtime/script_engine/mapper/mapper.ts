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
    op_smudgy_mapper_list_area_room_numbers,
    op_smudgy_mapper_list_rooms_by_title_and_description,
    op_smudgy_mapper_list_rooms_by_title_description_and_visible_exits,
    op_smudgy_mapper_create_area,
    op_smudgy_mapper_rename_area,
    op_smudgy_mapper_get_area_by_id,
    op_smudgy_mapper_get_area_name,
    op_smudgy_mapper_get_area_id,
    op_smudgy_mapper_get_area_room_by_number,
    op_smudgy_mapper_get_area_property,
    op_smudgy_mapper_get_area_next_room_number,
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
    op_smudgy_mapper_create_room_exit,
    op_smudgy_mapper_set_room_exit,
    op_smudgy_mapper_delete_room,
    op_smudgy_mapper_delete_room_exit,
    op_smudgy_mapper_get_area_labels,
    op_smudgy_mapper_get_area_shapes,
    op_smudgy_mapper_create_label,
    op_smudgy_mapper_create_shape,
    op_smudgy_mapper_set_label,
    op_smudgy_mapper_set_shape,
    op_smudgy_mapper_delete_label,
    op_smudgy_mapper_delete_shape,
    op_smudgy_mapper_import_areas,
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
type RoomNumber = number;
type ExitId = readonly [number, number];

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

/** How an exit is drawn on the map. */
type ExitStyle = "Normal" | "Dashed" | "Dotted" | "Meandering" | "Stub";

interface CreateRoomParams {
    title?: string;
    description?: string;
    level?: number;
    x?: number;
    y?: number;
    color?: string;
}

// The fields `updateRoom`/`updateRooms`/`Room.update` accept: the same set as creation, minus
// the auto-assigned room number. Any omitted field is left unchanged.
type UpdateRoomParams = CreateRoomParams;

const mapper = {
    async createArea(name: string) {
        const id = await op_smudgy_mapper_create_area(name);
        return new Area(id);
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

    renameArea(area: Area | AreaId, name: string) {
        const areaId = area instanceof Area ? area.id : area;
        op_smudgy_mapper_rename_area(areaId, name);
    },

    setRoomTitle(area: Area | AreaId, room: Room | RoomNumber, title: string) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_set_room_title(areaId, roomNumber, title);
    },

    setRoomDescription(area: Area | AreaId, room: Room | RoomNumber, description: string) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_set_room_description(areaId, roomNumber, description);
    },

    setRoomColor(area: Area | AreaId, room: Room | RoomNumber, color: string) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_set_room_color(areaId, roomNumber, color);
    },

    setRoomLevel(area: Area | AreaId, room: Room | RoomNumber, level: number) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_set_room_level(areaId, roomNumber, level);
    },

    setRoomX(area: Area | AreaId, room: Room | RoomNumber, x: number) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_set_room_x(areaId, roomNumber, x);
    },

    setRoomY(area: Area | AreaId, room: Room | RoomNumber, y: number) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_set_room_y(areaId, roomNumber, y);
    },

    setRoomProperty(area: Area | AreaId, room: Room | RoomNumber, name: string, value: string) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_set_room_property(areaId, roomNumber, name, value);
    },

    /** Set a custom data property on an area (the write counterpart of `area.data(key)`). Pass an
     * empty value to clear it. Requires the `mapper:write` capability. */
    setAreaProperty(area: Area | AreaId, name: string, value: string) {
        const areaId = area instanceof Area ? area.id : area;
        op_smudgy_mapper_set_area_property(areaId, name, value);
    },

    /** Add a case-insensitive tag to a room. The tag is normalized to UPPERCASE;
     * re-adding an existing tag is a no-op. Requires the `mapper:write` capability. */
    addRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_add_room_tag(areaId, roomNumber, tag);
    },

    /** Remove a tag from a room (case-insensitive). Requires `mapper:write`. */
    removeRoomTag(area: Area | AreaId, room: Room | RoomNumber, tag: string) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_remove_room_tag(areaId, roomNumber, tag);
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

    createRoom(area: Area | AreaId, params: CreateRoomParams): RoomNumber {
        const areaId = area instanceof Area ? area.id : area;
        return op_smudgy_mapper_create_room(areaId, params);
    },

    /** Update multiple fields of an existing room in ONE cache update (one index rebuild)
     * instead of one per field. Only the fields present in `fields` change. */
    updateRoom(area: Area | AreaId, room: Room | RoomNumber, fields: UpdateRoomParams) {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_update_room(areaId, roomNumber, fields);
    },

    /** Batch-update many rooms of one area in a single cache update. Each entry is a
     * `[roomNumber, fields]` pair; only the present fields of each change. */
    updateRooms(area: Area | AreaId, updates: [RoomNumber, UpdateRoomParams][]) {
        const areaId = area instanceof Area ? area.id : area;
        op_smudgy_mapper_update_rooms(areaId, updates);
    },

    createRoomExit(area: Area | AreaId, room: Room | RoomNumber, exit: ExitArgs): Promise<ExitId> {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        return op_smudgy_mapper_create_room_exit(areaId, roomNumber, exit);
    },
    setRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId, exit: ExitUpdates): void {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_set_room_exit(areaId, roomNumber, exitId, exit);
    },
    deleteRoom(area: Area | AreaId, room: Room | RoomNumber): void {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_delete_room(areaId, roomNumber);
    },
    deleteRoomExit(area: Area | AreaId, room: Room | RoomNumber, exitId: ExitId): void {
        const areaId = area instanceof Area ? area.id : area;
        const roomNumber = room instanceof Room ? room.room_number : room;
        op_smudgy_mapper_delete_room_exit(areaId, roomNumber, exitId);
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
    deleteLabel(area: Area | AreaId, labelId: LabelId): void {
        const areaId = area instanceof Area ? area.id : area;
        op_smudgy_mapper_delete_label(areaId, labelId);
    },
    /** Delete a shape from an area. Requires `mapper:write`. */
    deleteShape(area: Area | AreaId, shapeId: ShapeId): void {
        const areaId = area instanceof Area ? area.id : area;
        op_smudgy_mapper_delete_shape(areaId, shapeId);
    },
    /** Update an existing label; only present fields change. Requires `mapper:write`. */
    setLabel(area: Area | AreaId, labelId: LabelId, updates: LabelUpdates): void {
        const areaId = area instanceof Area ? area.id : area;
        op_smudgy_mapper_set_label(areaId, labelId, updates);
    },
    /** Update an existing shape; only present fields change. Requires `mapper:write`. */
    setShape(area: Area | AreaId, shapeId: ShapeId, updates: ShapeUpdates): void {
        const areaId = area instanceof Area ? area.id : area;
        op_smudgy_mapper_set_shape(areaId, shapeId, updates);
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
    readonly style: ExitStyle;
    readonly color: string | null;
}

// Fields accepted when creating an exit (`createRoomExit`); `from_direction` is required.
// `style`/`color` are accepted for parity but IGNORED on creation (the create op drops them) --
// set them afterward with `setRoomExit`.
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
    style?: ExitStyle;
    color?: string;
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
    style?: ExitStyle;
    color?: string;
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
class Area {
    #obj: any;

    constructor(obj: any) {
        this.#obj = obj;
    }

    get id(): AreaId {
        return op_smudgy_mapper_get_area_id(this.#obj);
    }

    get name(): string {
        return op_smudgy_mapper_get_area_name(this.#obj);
    }

    get room_numbers(): RoomNumber[] {
        return op_smudgy_mapper_list_area_room_numbers(this.#obj) || [];
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
    update(fields: UpdateRoomParams) {
        op_smudgy_mapper_update_room(this.area_id, this.room_number, fields);
    }

    toString() {
        return this.#obj.toString();
    }
}

Object.defineProperty(globalThis, "mapper", { value: mapper });
Object.defineProperty(globalThis, "Area", { value: Area });

// Drift-guard surface for `mapper_ts_impl_conforms_to_contract` (models/script_typings.rs):
// these TYPE-ONLY exports let the conformance test assert this runtime impl satisfies the
// published `smudgy-mapper.d.ts` contract (`Mapper`/`Area`/`Room`/`Exit`). They are fully
// erased -- the session reaches the API through the `globalThis` installs above, never these.
export type MapperImpl = typeof mapper;
export type AreaImpl = Area;
export type RoomImpl = Room;
export type ExitImpl = Exit;
