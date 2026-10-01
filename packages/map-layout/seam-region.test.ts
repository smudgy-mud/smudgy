import assert from "node:assert/strict";
import test from "node:test";
import type { GridPosition, LayoutEdge } from "./layout.ts";
import { SEAM_LINK_REACH, seamRegion } from "./seam-region.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });
const rooms = (entries: Record<string, GridPosition>): Map<string, GridPosition> =>
  new Map(Object.entries(entries));
/** A two-way exit between two rooms; the helper reads only which rooms it joins. */
const link = (from: string, to: string): LayoutEdge[] => [
  { from, to, direction: "East" },
  { from: to, to: from, direction: "West" },
];
const sorted = (ids: Iterable<string>): string[] => [...ids].sort();

test("a seam link reaches 1.32 cells from its segment", () => {
  assert.equal(SEAM_LINK_REACH, 1.32);
});

test("the region holds the seeds and the rooms on or next to a link between two seeds", () => {
  const positions = rooms({
    a: at(0, 0),
    b: at(4, 0),
    on: at(2, 0),
    above: at(2, -1),
    below: at(2, 1),
    corner: at(-1, 1),
    beyond: at(5, 0),
    far: at(6, 0),
    twoAway: at(2, 2),
  });
  const region = seamRegion(positions, link("a", "b"), ["a", "b"]);
  assert.deepEqual(sorted(region), ["a", "above", "b", "below", "beyond", "corner", "on"]);
});

test("a slanted link takes in the cells within 1.32 of its segment and no further", () => {
  // From (0,0) to (3,1): (1,2) lies 1.25 from the segment, (0,2) lies 1.5.
  const positions = rooms({
    a: at(0, 0),
    b: at(3, 1),
    near: at(1, 2),
    farther: at(0, 2),
    pastEnd: at(4, 0),
  });
  const region = seamRegion(positions, link("a", "b"), ["a", "b"]);
  assert.deepEqual(sorted(region), ["a", "b", "near", "pastEnd"]);
});

test("rooms on another level, and rooms beside a link between levels, stay out", () => {
  const positions = rooms({
    a: at(0, 0),
    b: at(2, 0),
    overhead: at(1, 0, 1),
    lower: at(0, 5),
    upper: at(0, 5, 1),
    besideLower: at(1, 5),
  });
  const edges = [...link("a", "b"), ...link("lower", "upper")];
  const region = seamRegion(positions, edges, ["a", "b", "lower", "upper"]);
  assert.deepEqual(sorted(region), ["a", "b", "lower", "upper"]);
});

test("rooms blocking a link between two rooms of the set join it; links leaving the set do not count", () => {
  const positions = rooms({
    s: at(0, 0),
    t: at(0, 1),
    // Next to the seam link s–t, so part of the set.
    c: at(1, 2),
    // Next only to the link c–s, which joins two rooms of the set.
    g: at(2, 3),
    // On the link c–d, which leaves the set.
    h: at(3, 2),
    d: at(5, 2),
  });
  const edges = [...link("s", "t"), ...link("c", "s"), ...link("c", "d")];
  const region = seamRegion(positions, edges, ["s", "t"]);
  assert.deepEqual(sorted(region), ["c", "g", "s", "t"]);
});

test("the second step considers links of the first step's set only", () => {
  // g joins through c–s; g–k is a link between the set and a room that only
  // the second step added, so the room on it stays out.
  const positions = rooms({
    s: at(0, 0),
    t: at(0, 1),
    c: at(1, 2),
    g: at(2, 3),
    onGk: at(4, 3),
    k: at(6, 3),
  });
  const edges = [...link("s", "t"), ...link("c", "s"), ...link("g", "k")];
  assert.deepEqual(sorted(seamRegion(positions, edges, ["s", "t"])), ["c", "g", "s", "t"]);
});

test("every room that shares its cell with another joins, wherever it is", () => {
  const positions = rooms({
    a: at(0, 0),
    b: at(1, 0),
    p: at(10, 10),
    q: at(10, 10),
    otherLevel: at(10, 10, 1),
    p2: at(20, 20, 2),
    q2: at(20, 20, 2),
    alone: at(30, 30),
  });
  const region = seamRegion(positions, link("a", "b"), ["a", "b"]);
  assert.deepEqual(sorted(region), ["a", "b", "p", "p2", "q", "q2"]);
});

test("without seeds the region is only the rooms that collide", () => {
  const edges = link("a", "b");
  assert.deepEqual(
    sorted(seamRegion(rooms({ a: at(0, 0), b: at(2, 0), m: at(1, 0) }), edges, [])),
    [],
  );
  assert.deepEqual(
    sorted(seamRegion(rooms({ a: at(0, 0), b: at(2, 0), m: at(2, 0) }), edges, [])),
    ["b", "m"],
  );
});

test("seeds without a room, exits to unknown rooms and exits to the same room are ignored", () => {
  const positions = rooms({ a: at(0, 0), beside: at(1, 0) });
  const edges: LayoutEdge[] = [
    { from: "a", to: "a", direction: "Up" },
    { from: "a", to: "ghost", direction: "East" },
    { from: "ghost", to: "a", direction: "West" },
  ];
  assert.deepEqual(sorted(seamRegion(positions, edges, ["ghost", "a"])), ["a"]);
});

test("scanning a link's cells and scanning its level's rooms find the same rooms", () => {
  // The same slanted seed link on two levels: level 0 is a full grid, so the
  // helper scans the link's box; level 1 holds a few rooms, so it scans them.
  const positions = new Map<string, GridPosition>();
  for (let x = 0; x < 10; x += 1) {
    for (let y = 0; y < 10; y += 1) positions.set(`g${x}.${y}`, at(x, y));
  }
  const probes = [[0, 0], [9, 3], [3, 2], [4, 3], [5, 0], [6, 3], [2, 3], [7, 4], [10, 4], [9, 5]];
  for (const [x, y] of probes) positions.set(`p${x}.${y}`, at(x, y, 1));
  const edges = [...link("g0.0", "g9.3"), ...link("p0.0", "p9.3")];
  const region = seamRegion(positions, edges, ["g0.0", "g9.3", "p0.0", "p9.3"]);
  for (const [x, y] of probes.filter(([x, y]) => x < 10 && y < 10)) {
    assert.equal(region.has(`p${x}.${y}`), region.has(`g${x}.${y}`), `probe (${x}, ${y})`);
  }
  assert.equal(region.has("p10.4"), true, "a cell one past the far end is next to it");
  assert.equal(region.has("p9.5"), false, "two cells below the far end are not");
});

test("the region does not depend on the order of its inputs", () => {
  const positions = new Map<string, GridPosition>();
  const edges: LayoutEdge[] = [];
  let state = 7;
  const next = (limit: number): number => {
    state = (state * 1_103_515_245 + 12_345) % 2_147_483_648;
    return state % limit;
  };
  for (let index = 0; index < 60; index += 1) {
    positions.set(`r${index}`, at(next(12), next(12), next(2)));
  }
  for (let index = 0; index < 80; index += 1) {
    edges.push(...link(`r${next(60)}`, `r${next(60)}`));
  }
  const seeds = ["r1", "r5", "r9", "r13", "r21", "r34", "r55"];
  const expected = sorted(seamRegion(positions, edges, seeds));
  assert.ok(expected.length > seeds.length, "the fixture reaches past its seeds");

  const reversedPositions = new Map([...positions].reverse());
  const reversedEdges = [...edges].reverse();
  const reversedSeeds = [...seeds].reverse();
  assert.deepEqual(sorted(seamRegion(reversedPositions, reversedEdges, reversedSeeds)), expected);
  const doubled = [...edges, ...edges.map((edge) => ({ ...edge, from: edge.to, to: edge.from }))];
  assert.deepEqual(sorted(seamRegion(positions, doubled, new Set(seeds))), expected);
});
