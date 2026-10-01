import assert from "node:assert/strict";
import test from "node:test";
import {
  compareLayoutQuality,
  DIRECTIONAL_VIOLATION_WEIGHT,
  directionalViolationEdges,
  measureIntegralLayoutQuality,
  placeRigidBlocks,
  type GridPosition,
  type LayoutDirection,
  type LayoutEdge,
  type LayoutNode,
  type LayoutResident,
  type RigidBlock,
  type RigidBlockPlacementRequest,
} from "./layout.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });
const node = (id: string, x: number, y: number, level = 0): LayoutNode => ({
  id,
  relative: at(x, y, level),
});
const resident = (id: string, x: number, y: number, level = 0): LayoutResident => ({
  id,
  position: at(x, y, level),
  movable: true,
});
const edge = (from: string, to: string, direction: LayoutDirection): LayoutEdge => ({
  from,
  to,
  direction,
});
const REVERSE: Partial<Record<LayoutDirection, LayoutDirection>> = {
  North: "South",
  East: "West",
  South: "North",
  West: "East",
  Up: "Down",
  Down: "Up",
};
/** A two-way link: `direction` from `from` and its reverse back. */
const link = (from: string, to: string, direction: LayoutDirection): LayoutEdge[] => [
  edge(from, to, direction),
  edge(to, from, REVERSE[direction] as LayoutDirection),
];

const cell = (position: GridPosition): string => `${position.level}:${position.x}:${position.y}`;
const moved = (position: GridPosition, offset: GridPosition): GridPosition =>
  at(position.x + offset.x, position.y + offset.y, position.level + offset.level);

/** Residents where they are and every block room at `relative + offset`. */
function finalPositions(
  request: RigidBlockPlacementRequest,
  offsets: ReadonlyMap<string, GridPosition>,
): Map<string, GridPosition> {
  const result = new Map(request.residents.map((room) => [room.id, room.position]));
  for (const block of request.blocks) {
    const offset = offsets.get(block.id);
    if (!offset) continue;
    for (const room of block.rooms) result.set(room.id, moved(room.relative, offset));
  }
  return result;
}

/**
 * Places the request and checks what every placement promises: each block
 * with rooms has an integral offset, and none of its rooms lands on a cell a
 * resident or a room of another block holds.
 */
function place(request: RigidBlockPlacementRequest): Map<string, GridPosition> {
  return checkedPlacement(request, placeRigidBlocks(request));
}

function checkedPlacement(
  request: RigidBlockPlacementRequest,
  offsets: Map<string, GridPosition>,
): Map<string, GridPosition> {
  const owners = new Map(request.residents.map((room) => [cell(room.position), `resident ${room.id}`]));
  for (const block of request.blocks) {
    const offset = offsets.get(block.id);
    if (block.rooms.length === 0) {
      assert.equal(offset, undefined, `empty block ${block.id} has no offset`);
      continue;
    }
    assert.ok(offset, `block ${block.id} has an offset`);
    assert.ok(
      Number.isInteger(offset.x) && Number.isInteger(offset.y) && Number.isInteger(offset.level),
      `block ${block.id} has an integral offset`,
    );
    for (const room of block.rooms) {
      const key = cell(moved(room.relative, offset));
      const owner = owners.get(key);
      assert.ok(
        owner === undefined || owner === `block ${block.id}`,
        `room ${room.id} of block ${block.id} lands on ${key}, held by ${owner}`,
      );
      owners.set(key, `block ${block.id}`);
    }
  }
  assert.deepEqual(
    [...offsets.keys()],
    request.blocks.filter((block) => block.rooms.length > 0).map((block) => block.id),
    "one offset per block with rooms, in request order",
  );
  return offsets;
}

/** The `seams` whose direction the placed rooms do not satisfy. */
function violatedSeams(
  request: RigidBlockPlacementRequest,
  offsets: ReadonlyMap<string, GridPosition>,
  seams: readonly LayoutEdge[],
): LayoutEdge[] {
  return directionalViolationEdges(finalPositions(request, offsets), seams);
}

/** The public quality of the whole map with one block at `offset`. */
function qualityAt(request: RigidBlockPlacementRequest, blockId: string, offset: GridPosition) {
  const known = new Set([
    ...request.residents.map((room) => room.id),
    ...request.blocks.flatMap((block) => block.rooms.map((room) => room.id)),
  ]);
  return measureIntegralLayoutQuality(
    finalPositions(request, new Map([[blockId, offset]])),
    request.edges.filter((edge) => known.has(edge.from) && known.has(edge.to)),
  );
}

test("seam links that agree place the block exactly", () => {
  const seams = [
    ...link("b0", "r0", "East"),
    ...link("b1", "r1", "East"),
    ...link("b2", "r2", "East"),
  ];
  const request: RigidBlockPlacementRequest = {
    residents: [resident("r0", 0, 0), resident("r1", 0, 1), resident("r2", 0, 2), resident("r3", 1, 0)],
    blocks: [{
      id: "west",
      rooms: [node("b0", 5, 5), node("b1", 5, 6), node("b2", 5, 7), node("b3", 4, 6)],
    }],
    edges: [
      ...link("r0", "r3", "East"),
      ...link("r0", "r1", "South"),
      ...link("r1", "r2", "South"),
      ...link("b0", "b1", "South"),
      ...link("b1", "b2", "South"),
      ...link("b3", "b1", "East"),
      ...seams,
    ],
  };

  const offsets = place(request);

  assert.deepEqual(offsets.get("west"), at(-6, -5));
  assert.deepEqual(violatedSeams(request, offsets, seams), []);
});

test("links that disagree choose the admissible offset with the most satisfied directions", () => {
  // Listed first, the lone link to r3 implies (-1, 4); the two links to r0
  // and r1 agree on (-1, 0). Both offsets are free, but at (-1, 0) the lone
  // link leaves b2 eastward straight into r2, a blocked exit port. One step
  // west keeps both agreeing links on their rays with every port clear, and
  // the quality order weighs blocked ports before slack and footprint.
  const minority = edge("b2", "r3", "East");
  const majority = [edge("b0", "r0", "East"), edge("b1", "r1", "East")];
  const request: RigidBlockPlacementRequest = {
    residents: [resident("r0", 0, 0), resident("r1", 0, 1), resident("r2", 0, 2), resident("r3", 0, 6)],
    blocks: [{ id: "west", rooms: [node("b0", 0, 0), node("b1", 0, 1), node("b2", 0, 2)] }],
    edges: [
      minority,
      ...majority,
      ...link("r0", "r1", "South"),
      ...link("r1", "r2", "South"),
      ...link("b0", "b1", "South"),
      ...link("b1", "b2", "South"),
    ],
  };

  const offsets = place(request);

  assert.deepEqual(offsets.get("west"), at(-2, 0));
  assert.deepEqual(violatedSeams(request, offsets, [minority, ...majority]), [minority]);
  assert.ok(compareLayoutQuality(qualityAt(request, "west", at(-2, 0)), qualityAt(request, "west", at(-1, 0))) > 0);
});

test("an agreed offset that collides shifts to the nearest admissible cell that keeps most links", () => {
  // Both seam links agree on (-2, 0), where b2 would land on the blocker.
  // Sliding the block west keeps both East links on their rays; sliding it
  // any other way breaks them.
  const seams = [edge("b0", "r0", "East"), edge("b1", "r1", "East")];
  const request: RigidBlockPlacementRequest = {
    residents: [resident("r0", 0, 0), resident("r1", 0, 1), resident("blocker", -2, 2)],
    blocks: [{ id: "west", rooms: [node("b0", 1, 0), node("b1", 1, 1), node("b2", 0, 2)] }],
    edges: [...link("r0", "r1", "South"), ...link("b0", "b1", "South"), ...seams],
  };
  assert.deepEqual(moved(at(0, 2), at(-2, 0)), at(-2, 2), "the agreed offset collides");

  const offsets = place(request);

  assert.deepEqual(offsets.get("west"), at(-3, 0));
  assert.deepEqual(violatedSeams(request, offsets, seams), []);
});

test("an Up/Down seam yields a level offset", () => {
  const seams = link("b0", "r0", "Up");
  const request: RigidBlockPlacementRequest = {
    residents: [resident("r0", 3, 4, 2), resident("r1", 4, 4, 2)],
    blocks: [{ id: "cellar", rooms: [node("b0", 0, 0), node("b1", 0, 1)] }],
    edges: [...link("r0", "r1", "East"), ...link("b0", "b1", "South"), ...seams],
  };

  const offsets = place(request);

  assert.deepEqual(offsets.get("cellar"), at(3, 4, 1));
  assert.deepEqual(violatedSeams(request, offsets, seams), []);
});

test("placement draws two exits wrong on their level before it puts one on the wrong level", () => {
  // The Up seams agree on (10, 0, 1), directly below their rooms, where the
  // West seam joins rooms on different levels: one mis-levelled exit, 17.
  // The West seam alone implies (21, 0, 0), where both Up exits still climb
  // but from beside their rooms: two exits drawn wrong on their level, 16.
  // The blockers hold the one cell row that could satisfy all three.
  const west = edge("b0", "r0", "West");
  const ups = [edge("b0", "u0", "Up"), edge("b1", "u1", "Up")];
  const request: RigidBlockPlacementRequest = {
    residents: [
      resident("r0", 20, 0),
      resident("u0", 10, 0, 2),
      resident("u1", 11, 0, 2),
      resident("blocker0", 10, 0),
      resident("blocker1", 11, 0),
    ],
    blocks: [{ id: "block", rooms: [node("b0", 0, 0), node("b1", 1, 0)] }],
    edges: [...link("u0", "u1", "East"), ...link("b0", "b1", "East"), west, ...ups],
  };
  const levelShifted = qualityAt(request, "block", at(10, 0, 1));
  const onItsLevel = qualityAt(request, "block", at(21, 0, 0));
  assert.deepEqual([levelShifted.cardinalRayViolations, levelShifted.levelViolations], [1, 1]);
  assert.deepEqual([onItsLevel.cardinalRayViolations, onItsLevel.levelViolations], [2, 0]);
  assert.ok(compareLayoutQuality(onItsLevel, levelShifted) > 0);

  const offsets = place(request);

  assert.deepEqual(offsets.get("block"), at(21, 0, 0));
  assert.deepEqual(violatedSeams(request, offsets, [west, ...ups]), ups);
});

test("a chain of blocks aligns transitively", () => {
  // "a-end" sorts first but links only to "z-middle", so it waits for it.
  const request: RigidBlockPlacementRequest = {
    residents: [resident("r0", 0, 0)],
    blocks: [
      { id: "a-end", rooms: [node("e0", 100, 100), node("e1", 100, 101)] },
      { id: "z-middle", rooms: [node("m0", 10, 10), node("m1", 11, 10)] },
    ],
    edges: [
      ...link("e0", "e1", "South"),
      ...link("m0", "m1", "East"),
      ...link("r0", "m0", "East"),
      ...link("m1", "e0", "East"),
    ],
  };

  const offsets = place(request);

  assert.deepEqual(offsets.get("z-middle"), at(-9, -10));
  assert.deepEqual(offsets.get("a-end"), at(-97, -100));
  const positions = finalPositions(request, offsets);
  assert.deepEqual(positions.get("m1"), at(2, 0));
  assert.deepEqual(positions.get("e0"), at(3, 0));
  assert.deepEqual(directionalViolationEdges(positions, request.edges), []);
});

test("with nothing placed yet the first block keeps its coordinates and the rest join it", () => {
  const request: RigidBlockPlacementRequest = {
    residents: [],
    blocks: [
      { id: "b", rooms: [node("b0", 40, 40)] },
      { id: "a", rooms: [node("a0", 7, 3), node("a1", 8, 3)] },
    ],
    edges: [...link("a0", "a1", "East"), ...link("a1", "b0", "East")],
  };

  const offsets = place(request);

  assert.deepEqual(offsets.get("a"), at(0, 0));
  assert.deepEqual(offsets.get("b"), at(-31, -37));
});

test("an unlinked block goes to an island east of the map", () => {
  // The map spans x 0..4 and y 2..5; the block keeps its two levels. An edge
  // naming an unknown room links the block to nothing.
  const request: RigidBlockPlacementRequest = {
    residents: [resident("r0", 0, 2), resident("r1", 4, 2), resident("r2", 4, 5), resident("r3", 2, 5)],
    blocks: [{
      id: "loose",
      rooms: [node("l0", 10, 10, 1), node("l1", 11, 10, 1), node("l2", 10, 11, -1)],
    }],
    edges: [...link("l0", "l1", "East"), edge("l0", "ghost", "West"), ...link("r0", "r1", "East")],
  };

  const offsets = place(request);

  assert.deepEqual(offsets.get("loose"), at(-2, -8));
  const positions = finalPositions(request, offsets);
  assert.deepEqual(positions.get("l0"), at(8, 2, 1), "four columns east of the map, top-aligned");
  assert.deepEqual(positions.get("l2"), at(8, 3, -1));
});

test("empty blocks are omitted", () => {
  const request: RigidBlockPlacementRequest = {
    residents: [resident("r0", 0, 0)],
    blocks: [
      { id: "empty", rooms: [] },
      { id: "full", rooms: [node("f0", 0, 0)] },
    ],
    edges: link("r0", "f0", "North"),
  };

  const offsets = place(request);

  assert.deepEqual([...offsets], [["full", at(0, -1)]]);
});

test("rooms of one block that share a cell keep sharing it", () => {
  // The link's own offset (-6, -5) puts the shared cell on r0's West exit
  // port, where s1 is a room the exit does not lead to. One more step west
  // keeps the link on its ray and the port clear.
  const request: RigidBlockPlacementRequest = {
    residents: [resident("r0", 0, 0)],
    blocks: [{ id: "stacked", rooms: [node("s0", 5, 5), node("s1", 5, 5), node("s2", 4, 5)] }],
    edges: link("r0", "s0", "West"),
  };

  const offsets = place(request);

  assert.deepEqual(offsets.get("stacked"), at(-7, -5));
  const positions = finalPositions(request, offsets);
  assert.deepEqual(positions.get("s1"), positions.get("s0"));
  assert.ok(compareLayoutQuality(qualityAt(request, "stacked", at(-7, -5)), qualityAt(request, "stacked", at(-6, -5))) > 0);
});

test("repeated room and block ids are rejected", () => {
  assert.throws(() => placeRigidBlocks({
    residents: [resident("r0", 0, 0)],
    blocks: [{ id: "a", rooms: [node("r0", 0, 0)] }],
    edges: [],
  }), /layout id r0 appears more than once/);
  assert.throws(() => placeRigidBlocks({
    residents: [],
    blocks: [{ id: "a", rooms: [] }, { id: "a", rooms: [] }],
    edges: [],
  }), /rigid block a appears more than once/);
});

test("a block room keeps off a resident's exit port when a slide keeps the same directions", () => {
  // R's East exit leads to S, which is charted south of it, so that exit is
  // violated wherever the block goes. The block's one room links West to R,
  // and that link's own offset (1, 0) is R's East exit port. Sliding one more
  // cell east keeps the West link on its ray and the port clear.
  const request: RigidBlockPlacementRequest = {
    residents: [resident("R", 0, 0), resident("S", 0, 3)],
    blocks: [{ id: "B", rooms: [node("b", 0, 0)] }],
    edges: [edge("R", "S", "East"), edge("b", "R", "West")],
  };

  const offsets = place(request);

  assert.deepEqual(offsets.get("B"), at(2, 0));
  const chosen = qualityAt(request, "B", at(2, 0));
  const onPort = qualityAt(request, "B", at(1, 0));
  assert.equal(chosen.cardinalRayViolations, onPort.cardinalRayViolations);
  assert.equal(onPort.exitPortViolations, 1);
  assert.equal(chosen.exitPortViolations, 0);
  assert.ok(compareLayoutQuality(chosen, onPort) > 0);
});

/** Thirty residents in a 6 x 5 grid at x = `baseX`, and a 3 x 3 block with two West seam links. */
function farRequest(baseX: number): RigidBlockPlacementRequest {
  const residents: LayoutResident[] = [];
  for (let index = 0; index < 30; index += 1) {
    residents.push(resident(`r${index}`, baseX + (index % 6), Math.floor(index / 6)));
  }
  const rooms: LayoutNode[] = [];
  for (let index = 0; index < 9; index += 1) rooms.push(node(`b${index}`, index % 3, Math.floor(index / 3)));
  return {
    residents,
    blocks: [{ id: "B", rooms }],
    edges: [edge("b0", "r3", "West"), edge("b4", "r10", "West")],
  };
}

test("far coordinates place exactly while they stay safe integers", () => {
  for (const baseX of [1_000_000, 2 ** 40]) {
    const request = farRequest(baseX);

    const offsets = place(request);

    assert.deepEqual(violatedSeams(request, offsets, request.edges), [], `residents at x = ${baseX}`);
  }
  // At 2^53 the residents' own coordinates are already rounded, but offsets
  // that keep every block room on a safe integer remain, and one of them wins.
  const request = farRequest(2 ** 53);
  const offset = place(request).get("B") as GridPosition;
  for (const room of request.blocks[0].rooms) {
    const position = moved(room.relative, offset);
    assert.ok(
      [position.x, position.y, position.level].every(Number.isSafeInteger),
      `${room.id} lands on ${cell(position)}`,
    );
  }
});

test("a block no offset places on exact unoccupied cells is a clear error", () => {
  // At 2^55 adjacent cells round together, so every candidate would merge
  // block rooms or land them on residents.
  assert.throws(
    () => placeRigidBlocks(farRequest(2 ** 55)),
    (error: unknown) =>
      error instanceof Error && !(error instanceof TypeError) &&
      error.message === "could not find a collision-free offset for rigid block B",
  );
});

// ---------------------------------------------------------------------------
// Realistic sizes
// ---------------------------------------------------------------------------

/** The engine's own xorshift32. */
function xorshift32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
}

interface Wilderness {
  rooms: { id: string; position: GridPosition }[];
  edges: LayoutEdge[];
  /** Room id at grid column/row, as charted. */
  id(column: number, row: number): string;
}

/**
 * A width x height wilderness chart at `origin`: most cardinal passages are
 * two-way, some are missing, and a few rooms were charted one cell off, which
 * is what bends real seams.
 */
function wilderness(
  prefix: string,
  width: number,
  height: number,
  origin: GridPosition,
  random: () => number,
): Wilderness {
  const id = (column: number, row: number): string => `${prefix}-${column}-${row}`;
  const rooms: Wilderness["rooms"] = [];
  const taken = new Set<string>();
  for (let row = 0; row < height; row += 1) {
    for (let column = 0; column < width; column += 1) {
      const position = at(origin.x + column, origin.y + row, origin.level);
      rooms.push({ id: id(column, row), position });
      taken.add(cell(position));
    }
  }
  for (const room of rooms) {
    if (random() >= 0.05) continue;
    const drifted = random() < 0.5
      ? at(room.position.x + (random() < 0.5 ? -1 : 1), room.position.y, room.position.level)
      : at(room.position.x, room.position.y + (random() < 0.5 ? -1 : 1), room.position.level);
    if (taken.has(cell(drifted))) continue;
    taken.delete(cell(room.position));
    taken.add(cell(drifted));
    room.position = drifted;
  }
  const edges: LayoutEdge[] = [];
  for (let row = 0; row < height; row += 1) {
    for (let column = 0; column < width; column += 1) {
      if (column + 1 < width && random() >= 0.08) edges.push(...link(id(column, row), id(column + 1, row), "East"));
      if (row + 1 < height && random() >= 0.08) edges.push(...link(id(column, row), id(column, row + 1), "South"));
    }
  }
  return { rooms, edges, id };
}

/**
 * A 1,000-room map with a ragged west edge, a 500-room section whose seam
 * collides there, a 200-room section on its north edge, a 64-room section
 * reachable only through the big one, and 30 unlinked rooms.
 */
function realisticRequest(seed: number): {
  request: RigidBlockPlacementRequest;
  westSeams: LayoutEdge[];
  westConsensus: GridPosition;
} {
  const random = xorshift32(seed);
  const map = wilderness("d", 40, 25, at(0, 0), random);
  const residents: LayoutResident[] = map.rooms.map((room) => ({ ...room, movable: true }));
  const edges = [...map.edges];
  for (const row of [3, 9, 14]) {
    residents.push(resident(`spur-${row}-1`, -1, row), resident(`spur-${row}-2`, -2, row));
    edges.push(...link(map.id(0, row), `spur-${row}-1`, "West"), ...link(`spur-${row}-1`, `spur-${row}-2`, "West"));
  }

  const west = wilderness("w", 25, 20, at(200, 300), random);
  const westSeams: LayoutEdge[] = [];
  for (let row = 0; row < 20; row += 1) {
    if (random() < 0.9) westSeams.push(...link(west.id(24, row), map.id(0, row), "East"));
  }
  const north = wilderness("n", 20, 10, at(500, 500), random);
  const northSeams: LayoutEdge[] = [];
  for (let column = 0; column < 20; column += 1) {
    if (random() < 0.8) northSeams.push(...link(map.id(column + 5, 0), north.id(column, 9), "North"));
  }
  const tail = wilderness("t", 8, 8, at(-50, 700), random);
  const tailSeams: LayoutEdge[] = [];
  for (let row = 0; row < 8; row += 1) tailSeams.push(...link(west.id(0, row + 4), tail.id(7, row), "West"));
  const loose = wilderness("l", 6, 5, at(0, 0, 3), random);

  const block = (id: string, section: Wilderness): RigidBlock => ({
    id,
    rooms: section.rooms.map((room) => ({ id: room.id, relative: room.position })),
  });
  return {
    request: {
      residents,
      blocks: [block("loose", loose), block("north", north), block("tail", tail), block("west", west)],
      edges: [
        ...edges,
        ...west.edges,
        ...north.edges,
        ...tail.edges,
        ...loose.edges,
        ...westSeams,
        ...northSeams,
        ...tailSeams,
      ],
    },
    westSeams,
    westConsensus: at(-225, -300),
  };
}

test("a 500-room section joins a 1,000-room map along its seam without overlap", () => {
  const { request, westSeams, westConsensus } = realisticRequest(0x5eed_b10c);
  assert.equal(request.residents.length, 1_006);
  assert.equal(request.blocks.find((block) => block.id === "west")?.rooms.length, 500);

  const offsets = place(request);

  // The agreed offset collides with the spurs. Every pure westward slide of
  // it keeps the East seam links it satisfied, so the chosen offset
  // satisfies at least as many seam links as any admissible slide.
  const occupied = new Set(request.residents.map((room) => cell(room.position)));
  const westRooms = request.blocks.find((block) => block.id === "west")?.rooms ?? [];
  const admissible = (offset: GridPosition): boolean =>
    westRooms.every((room) => !occupied.has(cell(moved(room.relative, offset))));
  const satisfied = (offset: GridPosition): number => {
    const positions = new Map(request.residents.map((room) => [room.id, room.position]));
    for (const room of westRooms) positions.set(room.id, moved(room.relative, offset));
    return westSeams.length - directionalViolationEdges(positions, westSeams).length;
  };
  assert.equal(admissible(westConsensus), false, "the agreed offset collides");
  let bestSlide = 0;
  for (let distance = 1; distance <= 12; distance += 1) {
    const slide = at(westConsensus.x - distance, westConsensus.y, westConsensus.level);
    if (admissible(slide)) bestSlide = Math.max(bestSlide, satisfied(slide));
  }
  const chosen = offsets.get("west") as GridPosition;
  assert.ok(bestSlide > westSeams.length / 2, "a slide keeps most seam links");
  assert.ok(
    satisfied(chosen) >= bestSlide,
    `offset ${cell(chosen)} satisfies ${satisfied(chosen)} seam links; a slide keeps ${bestSlide}`,
  );

  // The tail links only to the west section and lands against it.
  const tailSeams = request.edges.filter((edge) => edge.from.startsWith("t-") !== edge.to.startsWith("t-"));
  assert.deepEqual(directionalViolationEdges(finalPositions(request, offsets), tailSeams), []);
});

test("the same request always yields the same offsets", () => {
  const first = place(realisticRequest(0x0dd_ba11).request);
  const second = place(structuredClone(realisticRequest(0x0dd_ba11).request));

  assert.deepEqual([...second], [...first]);
});

/**
 * A 26 x 25 map and `count` square sections charted at `spacing` times its
 * scale, each with `seams` West links from its west column to scattered rows
 * of the map's east column, so nearly every link implies an offset of its own.
 */
function disagreeingSections(
  count: number,
  side: number,
  seams: number,
  spacing: number,
): RigidBlockPlacementRequest {
  const residents: LayoutResident[] = [];
  const edges: LayoutEdge[] = [];
  const mapId = (x: number, y: number): string => `r${x}-${y}`;
  for (let y = 0; y < 25; y += 1) {
    for (let x = 0; x < 26; x += 1) {
      residents.push(resident(mapId(x, y), x, y));
      if (x + 1 < 26) edges.push(...link(mapId(x, y), mapId(x + 1, y), "East"));
      if (y + 1 < 25) edges.push(...link(mapId(x, y), mapId(x, y + 1), "South"));
    }
  }
  const blocks: RigidBlock[] = [];
  for (let section = 0; section < count; section += 1) {
    const roomId = (x: number, y: number): string => `s${section}-${x}-${y}`;
    const rooms: LayoutNode[] = [];
    for (let y = 0; y < side; y += 1) {
      for (let x = 0; x < side; x += 1) {
        rooms.push(node(roomId(x, y), x * spacing, y * spacing));
        if (x + 1 < side) edges.push(edge(roomId(x, y), roomId(x + 1, y), "East"));
        if (y + 1 < side) edges.push(edge(roomId(x, y), roomId(x, y + 1), "South"));
      }
    }
    for (let seam = 0; seam < seams; seam += 1) {
      const row = seam % side;
      edges.push(edge(roomId(0, row), mapId(25, (row * 3 + section * 5 + seam) % 25), "West"));
    }
    blocks.push({ id: `section${section}`, rooms });
  }
  return { residents, blocks, edges };
}

test("sections whose seam links all disagree place in bounded time", () => {
  const request = disagreeingSections(3, 22, 100, 3);
  assert.equal(request.residents.length, 650);
  const timed = (): { offsets: Map<string, GridPosition>; elapsed: number } => {
    const started = performance.now();
    const offsets = placeRigidBlocks(request);
    return { offsets, elapsed: performance.now() - started };
  };

  const first = timed();
  const second = timed();

  checkedPlacement(request, first.offsets);
  assert.deepEqual([...second.offsets], [...first.offsets]);
  // The faster of two runs sheds a busy machine's pauses, and the ceiling
  // leaves slow machines a wide margin over the usual tens of milliseconds;
  // a search that grew with every disagreeing link would take seconds.
  const elapsed = Math.min(first.elapsed, second.elapsed);
  assert.ok(elapsed < 1_000, `placement took ${elapsed.toFixed(0)} ms`);
});

// ---------------------------------------------------------------------------
// Random requests
// ---------------------------------------------------------------------------

const RANDOM_DIRECTIONS: readonly LayoutDirection[] = [
  "North", "East", "South", "West", "Up", "Down",
  "Northeast", "Northwest", "Southeast", "Southwest",
];

/**
 * Up to 40 residents and four blocks of up to 12 rooms, crowded into a few
 * cells on three levels, with up to twice as many exits as rooms: random
 * ends and directions, some with constraint vectors, some to unknown rooms.
 */
function randomRequest(seed: number): RigidBlockPlacementRequest {
  const random = xorshift32(seed);
  const int = (low: number, high: number): number => low + Math.floor(random() * (high - low + 1));
  const residents: LayoutResident[] = [];
  const residentCount = int(0, 40);
  for (let index = 0; index < residentCount; index += 1) {
    residents.push(resident(`r${index}`, int(-6, 6), int(-6, 6), int(-1, 1)));
  }
  const blocks: RigidBlock[] = [];
  const blockCount = int(1, 4);
  for (let block = 0; block < blockCount; block += 1) {
    const rooms: LayoutNode[] = [];
    const roomCount = int(0, 12);
    for (let index = 0; index < roomCount; index += 1) {
      rooms.push(node(`b${block}r${index}`, int(-4, 4), int(-4, 4), int(-1, 1)));
    }
    blocks.push({ id: `block${block}`, rooms });
  }
  const ids = [
    ...residents.map((room) => room.id),
    ...blocks.flatMap((block) => block.rooms.map((room) => room.id)),
  ];
  const edges: LayoutEdge[] = [];
  const edgeCount = ids.length === 0 ? 0 : int(0, ids.length * 2);
  for (let index = 0; index < edgeCount; index += 1) {
    const exit = edge(
      ids[int(0, ids.length - 1)],
      ids[int(0, ids.length - 1)],
      RANDOM_DIRECTIONS[int(0, RANDOM_DIRECTIONS.length - 1)],
    );
    if (random() < 0.1) exit.constraintVector = at(int(-1, 1), int(-1, 1));
    edges.push(exit);
  }
  if (random() < 0.3) edges.push(edge("ghost", ids[0] ?? "ghost2", "East"));
  return { residents, blocks, edges };
}

test("random crowded requests place without overlap whatever order their lists come in", () => {
  const byBlock = (offsets: ReadonlyMap<string, GridPosition>): [string, GridPosition][] =>
    [...offsets].sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0));
  for (let seed = 1; seed <= 200; seed += 1) {
    const request = randomRequest(seed);

    const offsets = place(request);

    const reversed: RigidBlockPlacementRequest = {
      residents: [...request.residents].reverse(),
      blocks: [...request.blocks].reverse().map((block) => ({ id: block.id, rooms: [...block.rooms].reverse() })),
      edges: [...request.edges].reverse(),
    };
    assert.deepEqual(byBlock(placeRigidBlocks(reversed)), byBlock(offsets), `seed ${seed}`);
  }
});

const PROTECTED: Partial<Record<LayoutDirection, GridPosition>> = {
  North: at(0, -1),
  East: at(1, 0),
  South: at(0, 1),
  West: at(-1, 0),
  Up: at(0, 0, 1),
  Down: at(0, 0, -1),
};

/** The distinct offsets that the block's seam links to residents imply. */
function seamOrigins(request: RigidBlockPlacementRequest, block: RigidBlock): GridPosition[] {
  const relative = new Map(block.rooms.map((room) => [room.id, room.relative]));
  const placed = new Map(request.residents.map((room) => [room.id, room.position]));
  const origins = new Map<string, GridPosition>();
  for (const exit of request.edges) {
    const vector = exit.constraintVector
      ? at(Math.round(exit.constraintVector.x), Math.round(exit.constraintVector.y), Math.round(exit.constraintVector.level))
      : PROTECTED[exit.direction];
    if (!vector) continue;
    const leaving = relative.get(exit.from);
    const entered = placed.get(exit.to);
    if (leaving && entered) {
      const origin = at(entered.x - vector.x - leaving.x, entered.y - vector.y - leaving.y, entered.level - vector.level - leaving.level);
      origins.set(cell(origin), origin);
    }
    const left = placed.get(exit.from);
    const entering = relative.get(exit.to);
    if (left && entering) {
      const origin = at(left.x + vector.x - entering.x, left.y + vector.y - entering.y, left.level + vector.level - entering.level);
      origins.set(cell(origin), origin);
    }
  }
  return [...origins.values()];
}

test("with at most eight seam origins no fitting offset near one beats the chosen offset", () => {
  // Small maps compare hundreds of candidates, and below the origin limit
  // every origin's neighborhood is searched, so no fitting offset near an
  // origin may beat the chosen one, whatever directions it satisfies.
  let compared = 0;
  for (let seed = 1; seed <= 40; seed += 1) {
    const whole = randomRequest(seed * 7919);
    const block = whole.blocks.find((candidate) => candidate.rooms.length > 0);
    if (!block || whole.residents.length === 0) continue;
    const request = { ...whole, blocks: [block] };
    const origins = seamOrigins(request, block);
    if (origins.length === 0 || origins.length > 8) continue;
    const chosen = place(request).get(block.id) as GridPosition;
    const best = qualityAt(request, block.id, chosen);
    const occupied = new Set(request.residents.map((room) => cell(room.position)));
    for (const origin of origins) {
      for (let dx = -12; dx <= 12; dx += 1) {
        for (let dy = Math.abs(dx) - 12; dy <= 12 - Math.abs(dx); dy += 1) {
          const offset = moved(origin, at(dx, dy));
          if (block.rooms.some((room) => occupied.has(cell(moved(room.relative, offset))))) continue;
          const directions = directionalViolationEdges(
            finalPositions(request, new Map([[block.id, offset]])),
            request.edges,
          ).length;
          // Directions alone that outweigh the chosen offset's whole score lose.
          const score = DIRECTIONAL_VIOLATION_WEIGHT * best.cardinalRayViolations +
            best.routingViolations + best.linkCrossings;
          if (DIRECTIONAL_VIOLATION_WEIGHT * directions > score) continue;
          assert.ok(
            compareLayoutQuality(qualityAt(request, block.id, offset), best) <= 0,
            `seed ${seed * 7919}: ${cell(offset)} beats ${cell(chosen)}`,
          );
        }
      }
    }
    compared += 1;
  }
  assert.ok(compared >= 10, `${compared} requests compared`);
});

// ---------------------------------------------------------------------------
// Real geometry
// ---------------------------------------------------------------------------

/**
 * A placement pattern reduced from a player's map store: titles, VNUMs and
 * every property dropped, rooms renumbered, and each map translated so its
 * smallest x and y are 0. Rooms read `id@x,y,level` and exits `from>to:D`,
 * with D the initial of the exit's direction. The block is `B1`.
 */
interface RealFixture {
  residents: string;
  block: string;
  exits: string;
  /** The offset most seam links imply. */
  consensus: GridPosition;
  /** The most seam links an implied offset carries among those that land without overlap. */
  cleanSeamLinks: number;
}

const INITIALS: Readonly<Record<string, LayoutDirection>> = {
  N: "North",
  E: "East",
  S: "South",
  W: "West",
  U: "Up",
  D: "Down",
};

function realRequest(fixture: RealFixture): RigidBlockPlacementRequest {
  const tokens = (text: string): string[] => text.trim().split(/\s+/);
  const rooms = (text: string): LayoutNode[] =>
    tokens(text).map((token) => {
      const [id, coordinates] = token.split("@");
      const [x, y, level] = coordinates.split(",").map(Number);
      return node(id, x, y, level);
    });
  return {
    residents: rooms(fixture.residents).map((room) => ({ id: room.id, position: room.relative, movable: true })),
    blocks: [{ id: "B1", rooms: rooms(fixture.block) }],
    edges: tokens(fixture.exits).map((token) => {
      const [from, rest] = token.split(">");
      const [to, initial] = rest.split(":");
      return edge(from, to, INITIALS[initial]);
    }),
  };
}

/**
 * Places a real fixture and checks its offset against everything the
 * fixture shows is achievable: it satisfies at least as many seam links as
 * the best implied offset that lands without overlap, and at least as many
 * as any admissible planar shift within twelve cells of the consensus.
 */
function placeReal(fixture: RealFixture): {
  offset: GridPosition;
  satisfied: number;
  consensusAdmissible: boolean;
} {
  const request = realRequest(fixture);
  const offset = place(request).get("B1") as GridPosition;
  const rooms = request.blocks[0].rooms;
  const inBlock = new Set(rooms.map((room) => room.id));
  const seams = request.edges.filter((edge) => inBlock.has(edge.from) !== inBlock.has(edge.to));
  const occupied = new Set(request.residents.map((room) => cell(room.position)));
  const admissible = (candidate: GridPosition): boolean =>
    rooms.every((room) => !occupied.has(cell(moved(room.relative, candidate))));
  const satisfied = (candidate: GridPosition): number =>
    seams.length - violatedSeams(request, new Map([["B1", candidate]]), seams).length;

  const achieved = satisfied(offset);
  assert.ok(
    achieved >= fixture.cleanSeamLinks,
    `offset ${cell(offset)} satisfies ${achieved} seam links; a clean implied offset carries ${fixture.cleanSeamLinks}`,
  );
  for (let dx = -12; dx <= 12; dx += 1) {
    for (let dy = Math.abs(dx) - 12; dy <= 12 - Math.abs(dx); dy += 1) {
      const nearby = moved(fixture.consensus, at(dx, dy));
      if (!admissible(nearby)) continue;
      assert.ok(
        achieved >= satisfied(nearby),
        `offset ${cell(offset)} satisfies ${achieved} seam links; ${cell(nearby)} satisfies ${satisfied(nearby)}`,
      );
    }
  }
  return { offset, satisfied: achieved, consensusAdmissible: admissible(fixture.consensus) };
}

test("real seam links that agree place the block at their offset", () => {
  const { offset, satisfied } = placeReal(seamAgrees());

  assert.deepEqual(offset, at(0, 5));
  assert.equal(satisfied, 4);
});

test("real seam links that disagree keep at least the links of the one clean offset", () => {
  const fixture = seamDisagrees();

  const { satisfied, consensusAdmissible } = placeReal(fixture);

  assert.equal(consensusAdmissible, false);
  assert.ok(satisfied >= 4, `${satisfied} seam links satisfied`);
});

test("a real agreed offset that collides gives way to an admissible offset nearby", () => {
  const fixture = seamCollides();

  const { offset, consensusAdmissible } = placeReal(fixture);

  assert.equal(consensusAdmissible, false);
  const distance = Math.abs(offset.x - fixture.consensus.x) + Math.abs(offset.y - fixture.consensus.y) +
    Math.abs(offset.level - fixture.consensus.level);
  assert.ok(distance > 0 && distance <= 12, `offset ${cell(offset)} is ${distance} cells from the consensus`);
});

test("a real Up/Down seam moves the block down a level", () => {
  const { offset, satisfied } = placeReal(seamVertical());

  assert.deepEqual(offset, at(-2, 5, -1));
  assert.equal(satisfied, 2);
});

/** Seam links agree on one offset, which lands without overlap. */
function seamAgrees(): RealFixture {
  return {
    residents: `
      r1@0,0,0 r2@1,0,0 r3@2,0,0 r4@3,0,0 r5@4,0,0 r6@5,0,0 r7@6,0,0 r8@0,1,0 r9@1,1,0 r10@2,1,0
      r11@3,1,0 r12@4,1,0 r13@5,1,0 r14@6,1,0 r15@0,2,0 r16@1,2,0 r17@2,2,0 r18@3,2,0 r19@4,2,0
      r20@5,2,0 r21@6,2,0 r22@3,3,0 r23@5,3,0 r24@5,4,0 r25@5,5,0 r26@3,6,0 r27@4,6,0 r28@5,6,0
      r29@3,7,0 r30@4,7,0 r31@5,7,0 r32@3,8,0 r33@4,8,0 r34@5,8,0 r35@3,9,0 r36@4,9,0 r37@5,9,0
      r38@3,10,0 r39@4,10,0 r40@5,10,0 r41@5,11,0 r42@5,12,0 r43@5,13,0 r44@5,14,0 r45@4,15,0
      r46@5,15,0
    `,
    block: `
      b1@0,0,0 b2@1,0,0 b3@2,0,0 b4@0,1,0 b5@2,1,0 b6@0,2,0 b7@2,2,0 b8@0,3,0 b9@1,3,0 b10@2,3,0
    `,
    exits: `
      r1>r2:E r1>r8:S r2>r1:W r2>r3:E r2>r9:S r3>r2:W r3>r4:E r3>r10:S r4>r3:W r4>r5:E r4>r11:S
      r5>r4:W r5>r6:E r5>r12:S r6>r5:W r6>r7:E r6>r13:S r7>r6:W r7>r14:S r8>r1:N r8>r9:E r8>r15:S
      r9>r2:N r9>r8:W r9>r10:E r9>r16:S r10>r3:N r10>r9:W r10>r11:E r10>r17:S r11>r4:N r11>r10:W
      r11>r12:E r11>r18:S r12>r5:N r12>r11:W r12>r13:E r12>r19:S r13>r6:N r13>r12:W r13>r14:E
      r13>r20:S r14>r7:N r14>r13:W r14>r21:S r15>r8:N r15>r16:E r16>r9:N r16>r15:W r16>r17:E
      r17>r10:N r17>r16:W r17>r18:E r18>r11:N r18>r17:W r18>r19:E r19>r12:N r19>r18:W r19>r20:E
      r20>r13:N r20>r19:W r20>r21:E r20>r23:S r21>r14:N r21>r20:W r23>r20:N r23>r24:S r24>r23:N
      r24>r25:S r25>r24:N r25>r28:S r26>r27:E r26>r29:S r27>r26:W r27>r28:E r27>r30:S r28>r25:N
      r28>r27:W r28>r31:S r29>r26:N r29>r30:E r29>r32:S r29>b7:W r30>r27:N r30>r29:W r30>r31:E
      r30>r33:S r31>r28:N r31>r30:W r31>r34:S r32>r29:N r32>r33:E r32>r35:S r32>b10:W r33>r30:N
      r33>r32:W r33>r34:E r33>r36:S r34>r31:N r34>r33:W r34>r37:S r35>r32:N r35>r36:E r35>r38:S
      r36>r33:N r36>r35:W r36>r37:E r36>r39:S r37>r34:N r37>r36:W r37>r40:S r38>r35:N r38>r39:E
      r39>r36:N r39>r38:W r39>r40:E r40>r37:N r40>r39:W r40>r41:S r41>r40:N r41>r42:S r42>r41:N
      r42>r43:S r43>r42:N r43>r44:S r44>r43:N r44>r46:S r45>r46:E r46>r44:N r46>r45:W b1>b4:S
      b3>b5:S b4>b1:N b4>b6:S b5>b3:N b5>b7:S b6>b4:N b7>r29:E b7>b5:N b7>b10:S b8>b9:E b9>b8:W
      b9>b10:E b10>r32:E b10>b7:N b10>b9:W
    `,
    consensus: at(0, 5),
    cleanSeamLinks: 4,
  };
}

/** Seam links split between six offsets; the only clean one carries four links. */
function seamDisagrees(): RealFixture {
  return {
    residents: `
      r1@3,0,0 r2@4,0,0 r3@3,1,0 r4@4,1,0 r5@2,2,0 r6@3,2,0 r7@4,2,0 r8@5,2,0 r9@6,2,0 r10@3,3,0
      r11@4,3,0 r12@5,3,0 r13@6,3,0 r14@0,4,0 r15@1,4,0 r16@2,4,0 r17@3,4,0 r18@4,4,0 r19@5,4,0
      r20@6,4,0 r21@1,5,0 r22@2,5,0 r23@3,5,0 r24@4,5,0 r25@5,5,0 r26@6,5,0 r27@0,6,0 r28@1,6,0
      r29@2,6,0 r30@3,6,0 r31@4,6,0 r32@5,6,0 r33@0,7,0 r34@1,7,0 r35@2,7,0 r36@3,7,0 r37@4,7,0
      r38@5,7,0 r39@6,7,0 r40@0,8,0 r41@1,8,0 r42@2,8,0 r43@3,8,0 r44@4,8,0 r45@5,8,0 r46@6,8,0
      r47@0,9,0 r48@1,9,0 r49@2,9,0 r50@3,9,0 r51@5,9,0 r52@6,9,0 r53@0,10,0 r54@1,10,0 r55@0,11,0
      r56@1,11,0 r57@2,11,0 r58@0,12,0 r59@1,12,0 r60@2,12,0
    `,
    block: `
      b1@0,0,0 b2@1,0,0 b3@2,0,0 b4@0,1,0 b5@2,1,0 b6@0,2,0 b7@2,2,0 b8@0,3,0 b9@1,3,0 b10@2,3,0
    `,
    exits: `
      r1>r2:E r1>r3:S r1>b9:N r2>r1:W r2>r4:S r2>b10:N r3>r1:N r3>r4:E r4>r2:N r4>r3:W r4>r7:S
      r5>r6:E r6>r5:W r6>r7:E r6>r10:S r7>r4:N r7>r6:W r7>r8:E r7>r11:S r8>r7:W r8>r9:E r8>r12:S
      r9>r8:W r9>r13:S r10>r6:N r10>r11:E r10>r17:S r11>r7:N r11>r10:W r11>r12:E r11>r18:S r12>r8:N
      r12>r11:W r12>r13:E r12>r19:S r13>r9:N r13>r12:W r13>r20:S r14>r15:E r15>r14:W r15>r21:S
      r16>r17:E r16>r22:S r17>r10:N r17>r16:W r17>r18:E r17>r23:S r18>r11:N r18>r17:W r18>r19:E
      r18>r24:S r19>r12:N r19>r18:W r19>r20:E r19>r25:S r20>r13:N r20>r19:W r20>r26:S r21>r15:N
      r21>r28:S r22>r16:N r22>r23:E r23>r17:N r23>r22:W r23>r24:E r24>r18:N r24>r23:W r24>r25:E
      r24>r31:S r25>r19:N r25>r24:W r25>r26:E r26>r20:N r26>r25:W r27>r33:S r27>b1:E r28>r21:N
      r28>r34:S r28>b2:W r29>r30:E r30>r29:W r30>r31:E r31>r24:N r31>r30:W r31>r32:E r32>r31:W
      r33>r27:N r33>r40:S r33>b4:E r34>r28:N r34>r41:S r35>r36:E r35>r42:S r36>r35:W r36>r37:E
      r36>r43:S r37>r36:W r37>r38:E r37>r44:S r38>r37:W r38>r39:E r38>r45:S r39>r38:W r39>r46:S
      r40>r33:N r40>b6:E r41>r34:N r41>r42:E r41>r48:S r42>r35:N r42>r41:W r42>r43:E r42>r49:S
      r43>r36:N r43>r42:W r43>r44:E r43>r50:S r44>r37:N r44>r43:W r44>r45:E r44>b3:S r45>r38:N
      r45>r44:W r45>r46:E r45>r51:S r46>r39:N r46>r45:W r46>r52:S r47>r48:E r47>r53:S r47>b1:W
      r48>r41:N r48>r47:W r48>r54:S r49>r42:N r49>r50:E r50>r43:N r50>r49:W r50>b3:E r51>r45:N
      r51>r52:E r51>b3:W r52>r46:N r52>r51:W r53>r47:N r53>r54:E r53>r55:S r53>b4:W r54>r48:N
      r54>r53:W r54>r56:S r55>r53:N r55>r56:E r55>r58:S r55>b6:W r56>r54:N r56>r55:W r56>r57:E
      r56>r59:S r57>r56:W r57>r60:S r58>r55:N r58>r59:E r59>r56:N r59>r58:W r59>r60:E r59>b8:S
      r60>r57:N r60>r59:W r60>b7:E r60>b9:S b1>r27:W b1>r47:E b1>b4:S b2>r28:E b3>r44:N b3>r50:W
      b3>r51:E b3>b5:S b4>r33:W b4>r53:E b4>b1:N b4>b6:S b5>b3:N b5>b7:S b6>r40:W b6>r55:E b6>b4:N
      b7>r60:W b7>b5:N b7>b10:S b8>r59:N b8>b9:E b9>r1:S b9>r60:N b9>b8:W b9>b10:E b10>r2:S b10>b7:N
      b10>b9:W
    `,
    consensus: at(-1, 9),
    cleanSeamLinks: 4,
  };
}

/** Seam links agree, but their offset puts a room of a detached part on a resident. */
function seamCollides(): RealFixture {
  return {
    residents: `
      r1@7,0,-1 r2@8,0,-1 r3@7,1,-1 r4@7,2,-1 r5@7,2,0 r6@0,12,0 r7@2,12,0 r8@3,12,0 r9@0,13,0
      r10@2,13,0 r11@3,13,0 r12@1,14,0 r13@2,14,0 r14@2,15,0 r15@3,15,0 r16@2,16,0
    `,
    block: `
      b1@3,0,0 b2@4,0,0 b3@3,1,0 b4@4,1,0 b5@2,2,0 b6@3,2,0 b7@4,2,0 b8@5,2,0 b9@6,2,0 b10@3,3,0
      b11@4,3,0 b12@5,3,0 b13@6,3,0 b14@0,4,0 b15@1,4,0 b16@2,4,0 b17@3,4,0 b18@4,4,0 b19@5,4,0
      b20@6,4,0 b21@1,5,0 b22@2,5,0 b23@3,5,0 b24@4,5,0 b25@5,5,0 b26@6,5,0 b27@0,6,0 b28@1,6,0
      b29@2,6,0 b30@3,6,0 b31@4,6,0 b32@5,6,0 b33@0,7,0 b34@1,7,0 b35@2,7,0 b36@3,7,0 b37@4,7,0
      b38@5,7,0 b39@6,7,0 b40@0,8,0 b41@1,8,0 b42@2,8,0 b43@3,8,0 b44@4,8,0 b45@5,8,0 b46@6,8,0
      b47@0,9,0 b48@1,9,0 b49@2,9,0 b50@3,9,0 b51@5,9,0 b52@6,9,0 b53@0,10,0 b54@1,10,0 b55@0,11,0
      b56@1,11,0 b57@2,11,0 b58@0,12,0 b59@1,12,0 b60@2,12,0
    `,
    exits: `
      r1>r2:E r1>r3:S r2>r1:W r3>r1:N r3>r4:S r4>r3:N r5>r4:D r6>r3:E r6>r9:S r7>r2:N r7>r3:W
      r7>r8:E r7>r10:S r8>r7:W r8>r11:S r8>b1:E r9>r4:E r9>r6:N r10>r4:W r10>r7:N r10>r11:E
      r10>r13:S r11>r8:N r11>r10:W r11>b3:E r12>r4:N r12>r13:E r13>r10:N r13>r12:W r13>r14:S
      r14>r13:N r14>r15:E r14>r16:S r15>r14:W r15>b5:N r15>b10:E r15>b16:S r16>r14:N b1>r8:W b1>b2:E
      b1>b3:S b2>b1:W b2>b4:S b3>r11:W b3>b1:N b3>b4:E b4>b2:N b4>b3:W b4>b7:S b5>r15:S b5>b6:E
      b6>b5:W b6>b7:E b6>b10:S b7>b4:N b7>b6:W b7>b8:E b7>b11:S b8>b7:W b8>b9:E b8>b12:S b9>b8:W
      b9>b13:S b10>r15:W b10>b6:N b10>b11:E b10>b17:S b11>b7:N b11>b10:W b11>b12:E b11>b18:S
      b12>b8:N b12>b11:W b12>b13:E b12>b19:S b13>b9:N b13>b12:W b13>b20:S b14>b15:E b15>b14:W
      b15>b21:S b16>r15:N b16>b17:E b16>b22:S b17>b10:N b17>b16:W b17>b18:E b17>b23:S b18>b11:N
      b18>b17:W b18>b19:E b18>b24:S b19>b12:N b19>b18:W b19>b20:E b19>b25:S b20>b13:N b20>b19:W
      b20>b26:S b21>b15:N b21>b28:S b22>b16:N b22>b23:E b23>b17:N b23>b22:W b23>b24:E b24>b18:N
      b24>b23:W b24>b25:E b24>b31:S b25>b19:N b25>b24:W b25>b26:E b26>b20:N b26>b25:W b27>b33:S
      b28>b21:N b28>b34:S b29>b30:E b30>b29:W b30>b31:E b31>b24:N b31>b30:W b31>b32:E b32>b31:W
      b33>b27:N b33>b40:S b34>b28:N b34>b41:S b35>b36:E b35>b42:S b36>b35:W b36>b37:E b36>b43:S
      b37>b36:W b37>b38:E b37>b44:S b38>b37:W b38>b39:E b38>b45:S b39>b38:W b39>b46:S b40>b33:N
      b41>b34:N b41>b42:E b41>b48:S b42>b35:N b42>b41:W b42>b43:E b42>b49:S b43>b36:N b43>b42:W
      b43>b44:E b43>b50:S b44>b37:N b44>b43:W b44>b45:E b45>b38:N b45>b44:W b45>b46:E b45>b51:S
      b46>b39:N b46>b45:W b46>b52:S b47>b48:E b47>b53:S b48>b41:N b48>b47:W b48>b54:S b49>b42:N
      b49>b50:E b50>b43:N b50>b49:W b51>b45:N b51>b52:E b52>b46:N b52>b51:W b53>b47:N b53>b54:E
      b53>b55:S b54>b48:N b54>b53:W b54>b56:S b55>b53:N b55>b56:E b55>b58:S b56>b54:N b56>b55:W
      b56>b57:E b56>b59:S b57>b56:W b57>b60:S b58>b55:N b58>b59:E b59>b56:N b59>b58:W b59>b60:E
      b60>b57:N b60>b59:W
    `,
    consensus: at(1, 12),
    cleanSeamLinks: 0,
  };
}

/** An Up/Down seam implies an offset one level down. */
function seamVertical(): RealFixture {
  return {
    residents: `
      r1@1,0,-1 r2@2,0,-1 r3@4,0,-1 r4@5,0,-1 r5@2,1,-1 r6@3,1,-1 r7@4,1,-1 r8@13,6,-1 r9@14,6,-1
      r10@15,6,-1 r11@16,6,-1 r12@2,1,0 r13@3,1,0 r14@4,1,0 r15@0,2,0 r16@1,2,0 r17@2,2,0 r18@3,2,0
      r19@4,2,0 r20@5,2,0 r21@6,2,0 r22@7,2,0 r23@0,3,0 r24@1,3,0 r25@2,3,0 r26@3,3,0 r27@4,3,0
      r28@5,3,0 r29@6,3,0 r30@1,4,0 r31@2,4,0 r32@3,4,0 r33@4,4,0 r34@5,4,0 r35@19,4,0 r36@1,5,0
      r37@5,5,0 r38@6,5,0 r39@7,5,0 r40@8,5,0 r41@9,5,0 r42@10,5,0 r43@17,5,0 r44@18,5,0 r45@19,5,0
      r46@20,5,0 r47@21,5,0 r48@22,5,0 r49@10,6,0 r50@11,6,0 r51@12,6,0 r52@13,6,0 r53@16,6,0
      r54@17,6,0 r55@18,6,0 r56@19,6,0 r57@20,6,0 r58@21,6,0 r59@19,5,1
    `,
    block: `
      b1@2,0,0 b2@3,0,0 b3@4,0,0 b4@1,1,0 b5@2,1,0 b6@3,1,0 b7@4,1,0 b8@2,2,0 b9@3,2,0 b10@4,2,0
      b11@0,3,0 b12@1,3,0 b13@2,3,0 b14@3,3,0 b15@1,4,0 b16@2,4,0 b17@3,4,0 b18@1,5,0 b19@2,5,0
    `,
    exits: `
      r1>r2:E r2>r1:W r2>r5:S r3>r4:E r3>r7:S r4>r3:W r5>r2:N r5>r6:E r6>r5:W r6>r7:E r6>r13:U
      r7>r3:N r7>r6:W r8>r9:E r8>r52:U r9>r8:W r9>r10:E r10>r9:W r10>r11:E r11>r10:W r11>r53:U
      r12>r13:E r12>r17:S r13>r6:D r13>r12:W r13>r14:E r14>r13:W r14>r19:S r15>r16:E r16>r15:W
      r16>r17:E r17>r12:N r17>r16:W r17>r18:E r17>r25:S r18>r17:W r18>r19:E r18>r26:S r19>r14:N
      r19>r18:W r19>r20:E r19>r27:S r20>r19:W r20>r21:E r21>r20:W r21>r22:E r22>r21:W r23>r24:E
      r24>r23:W r24>r25:E r25>r17:N r25>r24:W r25>r26:E r25>r31:S r26>r18:N r26>r25:W r26>r27:E
      r26>r32:S r27>r19:N r27>r26:W r27>r28:E r27>r33:S r28>r27:W r28>r29:E r29>r28:W r30>r31:E
      r30>r36:S r31>r25:N r31>r30:W r31>r32:E r32>r26:N r32>r31:W r32>r33:E r33>r27:N r33>r32:W
      r33>r34:E r34>r33:W r34>r37:S r35>r45:S r36>r30:N r36>b2:D r37>r34:N r37>r38:E r38>r37:W
      r38>r39:E r39>r38:W r39>r40:E r40>r39:W r40>r41:E r41>r40:W r41>r42:E r42>r41:W r42>r49:S
      r43>r44:E r43>r54:S r44>r43:W r44>r45:E r44>r55:S r45>r35:N r45>r44:W r45>r46:E r45>r56:S
      r45>r59:U r46>r45:W r46>r47:E r46>r57:S r47>r46:W r47>r48:E r47>r58:S r48>r47:W r49>r42:N
      r49>r50:E r50>r49:W r50>r51:E r51>r50:W r51>r52:E r52>r8:D r52>r51:W r53>r11:D r53>r54:E
      r54>r43:N r54>r53:W r54>r55:E r55>r44:N r55>r54:W r55>r56:E r56>r45:N r56>r55:W r56>r57:E
      r57>r46:N r57>r56:W r57>r58:E r58>r47:N r58>r57:W r59>r45:D b1>b5:S b2>r36:U b2>b6:S b3>b7:S
      b4>b5:E b5>b1:N b5>b4:W b5>b6:E b5>b8:S b6>b2:N b6>b5:W b6>b7:E b6>b9:S b7>b3:N b7>b6:W
      b7>b10:S b8>b5:N b8>b13:S b9>b6:N b10>b7:N b11>b12:E b12>b11:W b12>b13:E b12>b15:S b13>b8:N
      b13>b12:W b13>b14:E b13>b16:S b14>b13:W b15>b12:N b15>b16:E b15>b18:S b16>b13:N b16>b15:W
      b16>b17:E b16>b19:S b17>b16:W b18>b15:N b18>b19:E b19>b16:N b19>b18:W
    `,
    consensus: at(-2, 5, -1),
    cleanSeamLinks: 2,
  };
}
