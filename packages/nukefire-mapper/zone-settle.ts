/**
 * Settling a zone: gathering every room of a zone the player stands in into
 * the one map for its area name. `zone-plan.ts` decides; this module reads the
 * facts from the mapper and carries the decision out, one mapper call a step.
 * The move itself is a single `mergeAreas`, so there is nothing to recover:
 * whatever an interruption leaves, the next visit plans again from the map.
 */

import { mapper } from "smudgy:core";
import {
  placeRigidBlocks,
  type GridPosition,
  type LayoutDirection,
  type LayoutEdge,
} from "smudgy://kapusniak/map-layout/layout.ts";
import type { AreaNames, MapName } from "./area-names.ts";
import type { DecisionLogRecord } from "./decision-log.ts";
import { seamSeeds } from "./seam-preview.ts";
import { mapForGlimpsedZone, planZone, type Destination, type MapFacts, type Source } from "./zone-plan.ts";

/** The properties and names that mark a mapper's maps and rooms. */
export interface ZoneMarks {
  /** On a map: the area name it is the map for, as its folded key. */
  readonly areaKey: string;
  /** On a map: the property, and its value, present on every map the mapper made. */
  readonly managed: { readonly property: string; readonly value: string };
  /** On a room: its zone. On a placeholder or an older per-zone map: the zone it was made for. */
  readonly zone: string;
  /** The name of the map a zone's rooms wait in until the game names the zone's area. */
  placeholderName(zone: number): string;
  /** The zone a placeholder was made for, read from its name. */
  placeholderZone(name: string): number | undefined;
}

/** What settling needs from the mapper that runs it. */
export interface ZoneContext {
  readonly names: AreaNames;
  readonly marks: ZoneMarks;
  readonly storage: MapStorage;
  /** The atlas new maps are made in. */
  atlas(): Promise<Atlas | undefined>;
  /** Makes one mapper call that changes maps, recorded in the decision log like every other. */
  mutation<T>(
    area: AreaId | undefined,
    api: string,
    description: string,
    call: () => Promise<T>,
    summarize?: (result: T) => unknown,
  ): Promise<T>;
  /**
   * Asks the mapper's layout polish to revisit a map: its seams are new
   * geometry. `seams` are the rooms at them, which the polish previews first.
   */
  polish(area: AreaId, seams: readonly RoomNumber[]): Promise<void>;
  log(record: DecisionLogRecord): void;
  notice(text: string): void;
}

export type Settlement =
  /** The zone's rooms are in `into`; `changed` maps gained or lost rooms, `deleted` maps are gone. */
  | { readonly kind: "settled"; readonly into: AreaId; readonly changed: readonly AreaId[]; readonly deleted: readonly AreaId[] }
  /** Most of the zone is in a map the player made, and stays there. */
  | { readonly kind: "player"; readonly map: AreaId }
  /** The mapper was busy with the maps involved; the next visit tries again. */
  | { readonly kind: "retry" };

/**
 * Every map in the mapper's storage tier, as settling `zone` sees it. A room
 * is in the zone when it says so or when the chart shows it there, so rooms
 * stored without their zone, as in a map the player imported, count too:
 * `charted` holds the external ids of the chart's rooms in the zone.
 */
export function readZoneFacts(zone: number, context: ZoneContext, charted: readonly string[] = []): MapFacts[] {
  const { marks } = context;
  const zoneRooms = new Map<string, Set<RoomNumber>>();
  const add = (room: Room) => {
    const rooms = zoneRooms.get(room.area_id) ?? new Set<RoomNumber>();
    zoneRooms.set(room.area_id, rooms.add(room.room_number));
  };
  mapper.findRoomsByProperty(marks.zone, String(zone)).forEach(add);
  for (const externalId of charted) {
    const room = mapper.findRoomByExternalId(externalId);
    if (room) add(room);
  }
  return mapper.areas.filter((area) => area.storage === context.storage).map((area) => ({
    id: area.id,
    name: area.name,
    key: area.data(marks.areaKey) || undefined,
    nameKey: context.names.resolve(area.name)?.key,
    managed: area.data(marks.managed.property) === marks.managed.value,
    placeholder: marks.placeholderZone(area.name) !== undefined,
    madeFor: madeFor(area, marks),
    roomCount: area.room_numbers.length,
    zoneRooms: [...zoneRooms.get(area.id) ?? []],
  }));
}

/** Gathers every room of `zone`, whose area the game calls `name`, into that area's one map. */
export async function settleZone(
  zone: number,
  name: MapName,
  context: ZoneContext,
  charted: readonly string[] = [],
): Promise<Settlement> {
  const plan = planZone(zone, name, readZoneFacts(zone, context, charted));
  if (plan.kind === "player") {
    context.log({ kind: "zone-in-player-map", zone, key: name.key, map: plan.id });
    return { kind: "player", map: plan.id };
  }

  const into = await ensureMap(plan.destination, name, context);
  // Each source with the offset that places it, as it is merged and logged.
  const sources = plan.sources.length > 0 ? placeSources(into, plan.sources) : [];
  let moved: readonly MergedRoom[] = [];
  let seams: readonly RoomNumber[] = [];
  if (sources.length > 0) {
    try {
      moved = await context.mutation(
        into,
        "mergeAreas",
        `Gather zone ${zone} into ${name.display}`,
        () => mapper.mergeAreas(into, sources),
        (rooms) => ({ moved: rooms.length }),
      );
    } catch (error) {
      const reason = String(error);
      if (!/\bmerge_/.test(reason)) throw error;
      context.log({ kind: "zone-merge-refused", zone, into, sources, reason });
      // Busy maps free up; anything else is left as it is until the next run.
      return /\bmerge_areas_(busy|source_changed)\b/.test(reason)
        ? { kind: "retry" }
        : { kind: "settled", into, changed: [into], deleted: [] };
    }
    await joinDuplicateRooms(into, moved, context);
    if (moved.length > 0) {
      seams = seamsOf(into, moved);
      await context.polish(into, seams);
    }
    announce(name, plan.sources, moved, context);
  }

  context.log({
    kind: "zone-settled",
    zone,
    key: name.key,
    into,
    destination: plan.destination.kind,
    sources,
    moved: moved.length,
    seams: seams.length,
  });
  return {
    kind: "settled",
    into,
    changed: [into, ...plan.sources.filter((source) => source.rooms).map((source) => source.id)],
    deleted: plan.sources.filter((source) => !source.rooms).map((source) => source.id),
  };
}

/** The map new rooms of a glimpsed zone go to: the one holding most of it, else its placeholder. */
export async function glimpsedZoneMap(
  zone: number,
  context: ZoneContext,
  charted: readonly string[] = [],
): Promise<AreaId> {
  const existing = mapForGlimpsedZone(zone, readZoneFacts(zone, context, charted));
  if (existing) return existing;
  const { marks } = context;
  const name = marks.placeholderName(zone);
  const atlas = await context.atlas();
  const placeholder = await context.mutation(
    undefined,
    "createArea",
    `Create map ${name}`,
    () => mapper.createArea(name, {
      storage: context.storage,
      atlas,
      properties: { [marks.zone]: String(zone), [marks.managed.property]: marks.managed.value },
    }),
    describeMap,
  );
  return placeholder.id;
}

/** The zone a placeholder or an older version's per-zone map was made for. */
function madeFor(area: Area, marks: ZoneMarks): number | undefined {
  const tied = area.data(marks.zone)?.trim();
  if (tied) return /^\d+$/.test(tied) ? Number(tied) : undefined;
  return marks.placeholderZone(area.name);
}

const describeMap = (area: Area) => ({ areaId: area.id, name: area.name, storage: area.storage });

async function ensureMap(destination: Destination, name: MapName, context: ZoneContext): Promise<AreaId> {
  switch (destination.kind) {
    case "existing":
      return destination.id;
    case "adopt": {
      const { id } = destination;
      await context.mutation(id, "setAreaProperty", `Bind map to ${name.display}`, () =>
        mapper.setAreaProperty(id, context.marks.areaKey, name.key));
      if (destination.rename) {
        await context.mutation(id, "renameArea", `Rename map to ${name.display}`, () =>
          mapper.renameArea(id, name.display));
      }
      return id;
    }
    case "create": {
      const { marks } = context;
      const atlas = await context.atlas();
      const created = await context.mutation(
        undefined,
        "createArea",
        `Create map ${name.display}`,
        () => mapper.createArea(name.display, {
          storage: context.storage,
          atlas,
          properties: { [marks.areaKey]: name.key, [marks.managed.property]: marks.managed.value },
        }),
        describeMap,
      );
      return created.id;
    }
  }
}

const roomsOf = (area: Area): Room[] => area.room_numbers.flatMap((number) => area.room(number) ?? []);
const layoutId = (room: Room): string => `${room.area_id}:${room.room_number}`;
const at = (room: Room): GridPosition => ({ x: Math.round(room.x), y: Math.round(room.y), level: room.level });

/** Each source with the offset map-layout chose to join its moving rooms to the destination. */
function placeSources(into: AreaId, sources: readonly Source[]): MergeAreaSource[] {
  const residents = roomsOf(mapper.getAreaById(into));
  const blocks = sources.map((source) => {
    const area = mapper.getAreaById(source.id);
    const rooms = source.rooms ? source.rooms.flatMap((number) => area.room(number) ?? []) : roomsOf(area);
    return { id: source.id, rooms };
  });
  const edges: LayoutEdge[] = [...residents, ...blocks.flatMap((block) => block.rooms)].flatMap((room) =>
    room.exits.flatMap((exit) =>
      exit.to_area_id === null || exit.to_room_number === null ? [] : [{
        from: layoutId(room),
        to: `${exit.to_area_id}:${exit.to_room_number}`,
        direction: exit.from_direction as LayoutDirection,
      }]
    )
  );
  const offsets = placeRigidBlocks({
    residents: residents.map((room) => ({ id: layoutId(room), position: at(room), movable: false })),
    blocks: blocks.map((block) => ({
      id: block.id,
      rooms: block.rooms.map((room) => ({ id: layoutId(room), relative: at(room) })),
    })),
    edges,
  });
  return sources.map((source) => ({ area: source.id, rooms: source.rooms && [...source.rooms], translate: offsets.get(source.id) }));
}

/** The rooms at the seams the move left in `into`, as they are once duplicates are joined. */
function seamsOf(into: AreaId, moved: readonly MergedRoom[]): RoomNumber[] {
  const arrivedFrom = new Map(moved.map((room) => [room.to, String(room.from.area)]));
  const rooms = roomsOf(mapper.getAreaById(into)).map((room) => ({
    roomNumber: room.room_number,
    exitsTo: room.exits.flatMap((exit) =>
      exit.to_room_number !== null && exit.to_area_id === into ? [exit.to_room_number] : []
    ),
  }));
  return seamSeeds(rooms, arrivedFrom);
}

/** Joins a room the move left twice in one map (the same VNUM), keeping the destination's own room. */
async function joinDuplicateRooms(into: AreaId, moved: readonly MergedRoom[], context: ZoneContext): Promise<void> {
  const arrived = new Set(moved.map((room) => room.to));
  const byVnum = new Map<string, RoomNumber[]>();
  for (const room of roomsOf(mapper.getAreaById(into))) {
    if (room.externalId) byVnum.set(room.externalId, [...byVnum.get(room.externalId) ?? [], room.room_number]);
  }
  for (const [vnum, numbers] of byVnum) {
    const keep = numbers.find((number) => !arrived.has(number)) ?? numbers[0];
    for (const remove of numbers.filter((number) => number !== keep)) {
      // A room linked to other maps cannot be joined yet; both copies stay.
      await context.mutation(into, "mergeRooms", `Join duplicate room ${vnum}`, () =>
        mapper.mergeRooms(into, keep, remove)).catch((error) =>
          context.log({ kind: "duplicate-room-kept", into, vnum, keep, remove, reason: String(error) })
        );
    }
  }
}

/** One line for the player: sections combined, else rooms moved. Removing empty maps goes unsaid. */
function announce(name: MapName, sources: readonly Source[], moved: readonly MergedRoom[], context: ZoneContext): void {
  const whole = new Set(sources.filter((source) => !source.rooms).map((source) => source.id));
  const sections = new Set(moved.filter((room) => whole.has(room.from.area)).map((room) => room.from.area)).size;
  if (sections > 0) context.notice(`Combined ${sections + 1} map sections into ${name.display}.`);
  else if (moved.length === 1) context.notice(`Moved 1 room into ${name.display}.`);
  else if (moved.length > 0) context.notice(`Moved ${moved.length} rooms into ${name.display}.`);
}
