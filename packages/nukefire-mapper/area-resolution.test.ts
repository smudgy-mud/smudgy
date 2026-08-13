import assert from "node:assert/strict";
import test from "node:test";
import {
  areaForObservedRoom,
  findAreaByNukeFireId,
  findCompatibleAreaByName,
  isAdoptableStorage,
  NUKEFIRE_AREA_ID_PROPERTY,
  type NukeFireAreaCandidate,
} from "./area-resolution.ts";

interface Candidate extends NukeFireAreaCandidate {
  readonly id: string;
}

function candidate(
  id: string,
  name: string,
  storage: MapStorage,
  areaId?: number,
): Candidate {
  return {
    id,
    name,
    storage,
    data(key: string): string | undefined {
      return key === NUKEFIRE_AREA_ID_PROPERTY && areaId !== undefined
        ? String(areaId)
        : undefined;
    },
  };
}

test("area identity wins over duplicate display names", () => {
  const sameName = candidate("name-only", "Central Plaza", "local");
  const exact = candidate("exact", "Old Central Plaza", "local", 42);

  assert.equal(findAreaByNukeFireId([sameName, exact], "local", 42), exact);
});

test("area identity matching prefers the configured storage tier", () => {
  const cloud = candidate("cloud", "Central Plaza", "cloud", 42);
  const local = candidate("local", "Central Plaza", "local", 42);

  assert.equal(findAreaByNukeFireId([cloud, local], "local", 42), local);
});

test("an existing cloud area is not adopted when the configured storage is local", () => {
  const cloud = candidate("cloud", "Central Plaza", "cloud", 42);

  assert.equal(findAreaByNukeFireId([cloud], "local", 42), undefined);
  assert.equal(findCompatibleAreaByName([cloud], "local", 42, "Central Plaza"), undefined);
});

test("session areas are never adopted by a durable-storage mapper", () => {
  const session = candidate("session", "Central Plaza", "session", 42);

  assert.equal(findAreaByNukeFireId([session], "local", 42), undefined);
  assert.equal(findCompatibleAreaByName([session], "local", 42, "Central Plaza"), undefined);
});

test("a session-storage mapper only pairs with session areas", () => {
  assert.equal(isAdoptableStorage("session", "session"), true);
  assert.equal(isAdoptableStorage("local", "session"), false);
  assert.equal(isAdoptableStorage("cloud", "session"), false);
});

test("a cloud-configured mapper never adopts a local area", () => {
  const local = candidate("local", "Central Plaza", "local", 42);

  assert.equal(isAdoptableStorage("local", "cloud"), false);
  assert.equal(findAreaByNukeFireId([local], "cloud", 42), undefined);
  assert.equal(findCompatibleAreaByName([local], "cloud", 42, "Central Plaza"), undefined);
});

test("name fallback does not reuse an area tagged for another identity", () => {
  const wrong = candidate("wrong", "Central Plaza", "local", 41);
  const unclaimed = candidate("unclaimed", "central plaza", "local");

  assert.equal(findCompatibleAreaByName([wrong, unclaimed], "local", 42, "Central Plaza"), unclaimed);
});

test("a border or re-zoned vnum keeps its existing room's area", () => {
  const home = candidate("home", "Old Central Plaza", "local", 5);
  const zone = candidate("zone", "Central Plaza", "local", 6);

  // The server now reports the vnum under zone 6, but the room already lives
  // in the zone-5 area: re-creating it there would duplicate its externalId.
  assert.equal(areaForObservedRoom(zone, home), home);
  assert.equal(areaForObservedRoom(zone, undefined), zone);
});
