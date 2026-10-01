import assert from "node:assert/strict";
import test from "node:test";
import { AreaNames, type MapName } from "./area-names.ts";
import { NUKEFIRE_AREA_NAME_RULES } from "./nukefire-maps.ts";
import { mapForGlimpsedZone, planZone, type MapFacts } from "./zone-plan.ts";

const names = new AreaNames(NUKEFIRE_AREA_NAME_RULES);
const nameOf = (observed: string) => names.resolve(observed) as MapName;

/**
 * A map as the planner sees it: `zone` rooms of the planned zone, `other` rooms
 * of other zones, and `madeFor` the zone an older version made it for.
 */
function map(
  id: string,
  name: string,
  { zone = 0, other = 0, key, managed = true, madeFor }: {
    zone?: number;
    other?: number;
    key?: string;
    managed?: boolean;
    madeFor?: number;
  } = {},
): MapFacts {
  return {
    id: id as AreaId,
    name,
    key,
    nameKey: names.resolve(name)?.key,
    managed,
    placeholder: /^NukeFire Zone \d+$/.test(name),
    madeFor,
    roomCount: zone + other,
    zoneRooms: Array.from({ length: zone }, (_, index) => index + 1),
  };
}

test("a zone met for the first time gets a new map", () => {
  assert.deepEqual(planZone(77, nameOf("Mines of Kreel"), []), {
    kind: "settle",
    destination: { kind: "create" },
    sources: [],
  });
});

test("the family's largest map is adopted and the zone's own section folds in whole", () => {
  const plan = planZone(315, nameOf("The Deathlands"), [
    map("zone-316", "The Deathlands", { zone: 57, other: 469, madeFor: 316 }),
    map("zone-315", "The Deathlands", { zone: 21, madeFor: 315 }),
    map("zone-317", "The Deathlands", { other: 53, madeFor: 317 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "adopt", id: "zone-316", rename: false },
    sources: [{ id: "zone-315" }],
  });
});

test("a small section holding most of the zone does not become the survivor", () => {
  const plan = planZone(563, nameOf("Jurassic World"), [
    map("park", "Jurassic Park", { zone: 15, other: 318, madeFor: 560 }),
    map("world-563", "Jurassic World", { zone: 60, madeFor: 563 }),
    map("placeholder-563", "NukeFire Zone 563", { madeFor: 563 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "adopt", id: "park", rename: false },
    sources: [{ id: "world-563" }, { id: "placeholder-563" }],
  });
});

test("an adopted older spelling or placeholder takes the map's title; a name the player chose stays", () => {
  const spelling = planZone(702, nameOf("Vega Jane III"), [map("iii", "Vega Jane II", { zone: 16, madeFor: 701 })]);
  assert.deepEqual(spelling.kind === "settle" && spelling.destination, { kind: "adopt", id: "iii", rename: true });

  const placeholder = planZone(77, nameOf("Mines of Kreel"), [
    map("glimpsed", "NukeFire Zone 77", { zone: 9, madeFor: 77 }),
  ]);
  assert.deepEqual(placeholder.kind === "settle" && placeholder.destination, {
    kind: "adopt",
    id: "glimpsed",
    rename: true,
  });

  const chosen = planZone(315, nameOf("The Deathlands"), [map("mine", "My Deathlands", { zone: 40 })]);
  assert.deepEqual(chosen.kind === "settle" && chosen.destination, { kind: "adopt", id: "mine", rename: false });
});

test("once a map is the map for a name it stays the destination, and duplicates fold into the larger", () => {
  const plan = planZone(318, nameOf("The Deathlands"), [
    map("small-claim", "The Deathlands", { key: "the deathlands", other: 30 }),
    map("large-claim", "The Deathlands", { key: "the deathlands", zone: 5, other: 400 }),
    map("zone-318", "The Deathlands", { zone: 52, madeFor: 318 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "existing", id: "large-claim" },
    sources: [{ id: "small-claim" }, { id: "zone-318" }],
  });
});

test("a rule sending a zone to another map folds the zone's map into it on the next visit", () => {
  const rules = new AreaNames([{ name: "Forest around Hermit's Knob", map: "Eastern Forest", exactCase: false }]);
  const plan = planZone(901, rules.resolve("Forest around Hermit's Knob") as MapName, [
    { ...map("forest", "Eastern Forest", { key: "eastern forest", other: 120 }), nameKey: "eastern forest" },
    { ...map("knob", "Forest around Hermit's Knob", { key: "forest around hermit's knob", zone: 35 }), nameKey: "eastern forest" },
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "existing", id: "forest" },
    sources: [{ id: "knob" }],
  });
});

test("without that rule the zone's rooms move back out into their own map", () => {
  const plan = planZone(901, nameOf("Forest around Hermit's Knob"), [
    map("forest", "Eastern Forest", { key: "eastern forest", zone: 35, other: 120 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "create" },
    sources: [{ id: "forest", rooms: Array.from({ length: 35 }, (_, index) => index + 1) }],
  });
});

test("a zone's rooms inside another family's map move out, and that map keeps the rest", () => {
  const plan = planZone(602, nameOf("Night Lands"), [
    map("caldera", "Caldera", { key: "caldera", zone: 3, other: 88, madeFor: 601 }),
    map("night", "Night Lands", { zone: 100, madeFor: 602 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "adopt", id: "night", rename: false },
    sources: [{ id: "caldera", rooms: [1, 2, 3] }],
  });
});

test("a map an older version made for another zone stays that zone's map, whichever zone is visited first", () => {
  const plan = planZone(602, nameOf("Night Lands"), [
    map("caldera", "Caldera", { zone: 2, other: 88, madeFor: 601 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "create" },
    sources: [{ id: "caldera", rooms: [1, 2] }],
  });
});

test("empty maps older versions left for the zone are removed, and nothing else moves", () => {
  const plan = planZone(463, nameOf("SST - Federation Station"), [
    map("sst", "SST", { key: "sst", zone: 37, other: 227 }),
    map("station", "SST - Federation Station", { madeFor: 463 }),
    map("station-placeholder", "NukeFire Zone 463", { madeFor: 463 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "existing", id: "sst" },
    sources: [{ id: "station" }, { id: "station-placeholder" }],
  });
});

test("an empty map made for the zone is reused before a new one is created", () => {
  const plan = planZone(420, nameOf("Dungeon Crawler Carl"), [
    map("carl", "Dungeon Crawler Carl", { madeFor: 420 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "adopt", id: "carl", rename: false },
    sources: [],
  });
});

test("a zone arranged mostly in a map the player made stays as they arranged it", () => {
  const plan = planZone(250, nameOf("The Crater"), [
    map("theirs", "Crater Notes", { zone: 24, managed: false }),
    map("ours", "The Crater", { zone: 3 }),
  ]);
  assert.deepEqual(plan, { kind: "player", id: "theirs" });
});

test("maps the player made are never taken from", () => {
  const plan = planZone(250, nameOf("The Crater"), [
    map("theirs", "Crater Notes", { zone: 2, other: 10, managed: false }),
    map("ours", "The Crater", { zone: 22 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "adopt", id: "ours", rename: false },
    sources: [],
  });
});

test("ties go to the lowest map id, so the same maps always give the same plan", () => {
  const plan = planZone(610, nameOf("Night Lands and Caldera"), [
    map("b", "Night Lands and Caldera", { zone: 50 }),
    map("a", "Night Lands and Caldera", { other: 50 }),
  ]);
  assert.deepEqual(plan, {
    kind: "settle",
    destination: { kind: "adopt", id: "a", rename: false },
    sources: [{ id: "b" }],
  });
});

test("a glimpsed zone's new rooms join the map holding most of it, else its placeholder", () => {
  assert.equal(
    mapForGlimpsedZone(602, [
      map("a", "Caldera", { zone: 2, other: 40, madeFor: 601 }),
      map("b", "NukeFire Zone 602", { zone: 9, madeFor: 602 }),
    ]),
    "b",
  );
  assert.equal(mapForGlimpsedZone(602, [map("shell", "NukeFire Zone 602", { madeFor: 602 })]), "shell");
  assert.equal(mapForGlimpsedZone(602, [map("other", "Caldera", { other: 40, madeFor: 601 })]), undefined);
});
