/**
 * Realistic-map corpus: deterministic constructors for the map classes the
 * layout engine actually meets in the field, shared by
 * `realistic-quality.test.ts` (version-independent invariants),
 * `realistic-ratchet.test.ts` (the equal-or-better quality ratchet), and
 * `realistic-ratchet-update.mjs` (deliberate ratchet regeneration).
 *
 * Every constructor is seeded and pure: all randomness comes from a locally
 * seeded xorshift32, no wall-clock time is read, and repair budgets are
 * deterministic work counts under an infinite deadline. Rooms, edges, and
 * defects are shaped like real MUD areas — near-full wilderness grids with a
 * few misobserved exits, one-way-heavy mazes with infeasible observation
 * knots, stratified towers with projected verticals, high-degree hub plazas,
 * user-pinned neighborhoods under active re-charting, disconnected partial
 * charts, and truncated `Map.Local` charts that grow across visits.
 */

import {
  planIntegralLayout,
  type ConstraintRepairOptions,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
  type LayoutDirection,
  type LayoutEdge,
  type LayoutQuality,
  type LayoutResident,
} from "./layout.ts";
import { repairIntegralLayoutConstraints } from "./constraint-layout.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });

/** The engine's own xorshift32, reseeded per fixture for full determinism. */
function xorshift32(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
}

const REVERSE: Record<LayoutDirection, LayoutDirection> = {
  North: "South",
  East: "West",
  South: "North",
  West: "East",
  Northeast: "Southwest",
  Northwest: "Southeast",
  Southeast: "Northwest",
  Southwest: "Northeast",
  Up: "Down",
  Down: "Up",
  In: "Out",
  Out: "In",
  Special: "Special",
  Other: "Other",
};

function negate(value: GridPosition): GridPosition {
  return { x: -value.x || 0, y: -value.y || 0, level: -value.level || 0 };
}

function link(
  edges: LayoutEdge[],
  from: string,
  to: string,
  direction: LayoutDirection,
  constraintVector?: GridPosition,
): void {
  edges.push(
    constraintVector
      ? { from, to, direction, constraintVector }
      : { from, to, direction },
  );
}

function reciprocalLink(
  edges: LayoutEdge[],
  from: string,
  to: string,
  direction: LayoutDirection,
  constraintVector?: GridPosition,
): void {
  link(edges, from, to, direction, constraintVector);
  link(
    edges,
    to,
    from,
    REVERSE[direction],
    constraintVector ? negate(constraintVector) : undefined,
  );
}

/** Count undirected links and how many of them are one-way observations. */
export function linkReciprocity(edges: readonly LayoutEdge[]): {
  links: number;
  oneWay: number;
} {
  const pairs = new Map<string, number>();
  for (const edge of edges) {
    const key = edge.from < edge.to
      ? `${edge.from}|${edge.to}`
      : `${edge.to}|${edge.from}`;
    pairs.set(key, (pairs.get(key) ?? 0) + 1);
  }
  let oneWay = 0;
  for (const count of pairs.values()) if (count === 1) oneWay += 1;
  return { links: pairs.size, oneWay };
}

/**
 * The public quality tuple's fields in declaration order, the order the
 * engine compares them in once their weighted scores are equal, for ratchet
 * records and human-readable digests. The comparison itself always goes
 * through the engine's `compareLayoutQuality`.
 */
export const QUALITY_TUPLE_FIELDS: readonly (keyof LayoutQuality)[] = [
  "levelViolations",
  "cardinalRayViolations",
  "reciprocalRayViolations",
  "routingViolations",
  "exitPortViolations",
  "reciprocalExitPortViolations",
  "roomObstructions",
  "linkCrossings",
  "levelSlack",
  "footprintArea",
  "footprintPerimeter",
  "cardinalSlack",
];

/** The full tuple with optional fields normalized, in comparison order. */
export function normalizedQuality(quality: Readonly<LayoutQuality>): Record<string, number> {
  const result: Record<string, number> = {};
  for (const field of QUALITY_TUPLE_FIELDS) result[field] = quality[field] ?? 0;
  return result;
}

/** Fixed corpus seeds. Changing one is a fixture change and a ratchet regen. */
export const REALISTIC_SEEDS = {
  denseGrid: 0xD1A001,
  oneWayMaze: 0xD1A002,
  tower: 0xD1A003,
  hub: 0xD1A004,
  lockedCluster: 0xD1A005,
  disconnected: 0xD1A006,
  truncatedGrowth: 0xD1A007,
} as const;

// ---------------------------------------------------------------------------
// a. Dense wilderness grid
// ---------------------------------------------------------------------------

/**
 * A ~220-room wilderness: a 15-wide near-full 4-connectivity grid whose last
 * five cells are uncharted. Roughly 7% of grid passages are missing, ~3% were
 * observed one-way, and three deliberately misobserved exits declare a
 * vertical direction between horizontally adjacent rooms — the surrounding
 * mesh pins both endpoints, so those rays are permanently unsatisfiable.
 * Charted positions are the true grid with ~8% single-cell charting drift.
 */
export function denseGridArea(seed: number): IntegralLayoutRequest {
  const random = xorshift32(seed);
  const WIDTH = 15;
  const ROOMS = 220;
  const id = (index: number): string => `wild-${String(index).padStart(3, "0")}`;
  const column = (index: number): number => index % WIDTH;
  const row = (index: number): number => Math.floor(index / WIDTH);

  // Misobserved exits: three interior horizontal adjacencies whose declared
  // direction is perpendicular to the grid geometry.
  const misobserved = new Set<number>();
  while (misobserved.size < 3) {
    const candidate = 1 + Math.floor(random() * (ROOMS - 1));
    if (column(candidate) >= WIDTH - 1 || candidate + 1 >= ROOMS) continue;
    if (row(candidate) === 0) continue;
    misobserved.add(candidate);
  }

  const edges: LayoutEdge[] = [];
  const neighbors: number[][] = Array.from({ length: ROOMS }, () => []);
  const connect = (
    a: number,
    b: number,
    kind: "reciprocal" | "one-way",
    direction: LayoutDirection,
  ): void => {
    if (kind === "reciprocal") reciprocalLink(edges, id(a), id(b), direction);
    else link(edges, id(a), id(b), direction);
    neighbors[a].push(b);
    neighbors[b].push(a);
  };

  for (let index = 0; index < ROOMS; index += 1) {
    const east = column(index) < WIDTH - 1 && index + 1 < ROOMS ? index + 1 : undefined;
    const south = index + WIDTH < ROOMS ? index + WIDTH : undefined;
    for (const [neighbor, outward] of [[east, "East"], [south, "South"]] as const) {
      if (neighbor === undefined) continue;
      if (outward === "East" && misobserved.has(index)) {
        connect(index, neighbor, "reciprocal", random() < 0.5 ? "North" : "South");
        continue;
      }
      const roll = random();
      if (roll < 0.07) continue;
      if (roll < 0.10) {
        if (random() < 0.5) connect(index, neighbor, "one-way", outward);
        else connect(neighbor, index, "one-way", REVERSE[outward]);
        continue;
      }
      connect(index, neighbor, "reciprocal", outward);
    }
  }

  // The random passage removal may strand rooms; a real wilderness chart is
  // one component. Reconnect every stranded room to its lower-index grid
  // neighbor, which is connected by induction over ascending index order.
  const reachable: boolean[] = new Array(ROOMS).fill(false);
  const flood = (start: number): void => {
    const frontier = [start];
    reachable[start] = true;
    while (frontier.length > 0) {
      const current = frontier.pop() as number;
      for (const next of neighbors[current]) {
        if (!reachable[next]) {
          reachable[next] = true;
          frontier.push(next);
        }
      }
    }
  };
  flood(0);
  for (let index = 1; index < ROOMS; index += 1) {
    if (reachable[index]) continue;
    if (column(index) > 0) connect(index, index - 1, "reciprocal", "West");
    else connect(index, index - WIDTH, "reciprocal", "North");
    flood(index);
  }

  const positions: GridPosition[] = [];
  for (let index = 0; index < ROOMS; index += 1) {
    positions.push(at(column(index), row(index)));
  }
  for (let index = 1; index < ROOMS; index += 1) {
    if (random() < 0.08) {
      const horizontal = random() < 0.5;
      const sign = random() < 0.5 ? -1 : 1;
      positions[index] = horizontal
        ? at(positions[index].x + sign, positions[index].y)
        : at(positions[index].x, positions[index].y + sign);
    }
  }

  const residents: LayoutResident[] = positions.map((position, index) => ({
    id: id(index),
    position,
    movable: index !== 0,
  }));
  return { residents, nodes: [], edges, centerId: id(0), allowExistingMoves: true };
}

// ---------------------------------------------------------------------------
// b. One-way-heavy maze
// ---------------------------------------------------------------------------

/**
 * A 120-room maze (12x10) whose links are heavily one-way (at least 40% of
 * observed passages have no return exit), with ~18 extra adjacencies forming
 * cycles and three infeasible observation knots: two one-way chords declared
 * against the pinned direction of a reciprocal two-link corridor, and one
 * doubly-reciprocal knot where the same adjacent pair carries both an
 * East/West and a North/South reciprocal link — the relaxation search must
 * choose which reciprocal pair to break, which is exactly the
 * reciprocal-preference weighting's workload.
 */
export function oneWayMazeArea(seed: number): IntegralLayoutRequest {
  const random = xorshift32(seed);
  const WIDTH = 12;
  const HEIGHT = 10;
  const ROOMS = WIDTH * HEIGHT;
  const id = (index: number): string => `maze-${String(index).padStart(3, "0")}`;
  const indexOf = (column: number, row: number): number => row * WIDTH + column;

  interface PlannedLink {
    a: number;
    b: number;
    /** Direction of travel from `a` to `b` on the grid. */
    outward: LayoutDirection;
    forced?: "reciprocal" | { oneWayFrom: number; declared: LayoutDirection };
    /** A second reciprocal link on the same pair with a conflicting direction. */
    doubled?: LayoutDirection;
  }
  const planned = new Map<string, PlannedLink>();
  const pairKey = (a: number, b: number): string => a < b ? `${a}|${b}` : `${b}|${a}`;
  const plan = (a: number, b: number, outward: LayoutDirection): PlannedLink => {
    const key = pairKey(a, b);
    let entry = planned.get(key);
    if (!entry) {
      entry = { a, b, outward };
      planned.set(key, entry);
    }
    return entry;
  };

  // Spanning maze: every room joins through its West or North grid neighbor.
  for (let index = 1; index < ROOMS; index += 1) {
    const column = index % WIDTH;
    const row = Math.floor(index / WIDTH);
    const viaWest = column > 0 && (row === 0 || random() < 0.5);
    if (viaWest) plan(index - 1, index, "East");
    else plan(index - WIDTH, index, "South");
  }

  // Extra adjacencies close cycles.
  let extras = 0;
  while (extras < 18) {
    const index = Math.floor(random() * ROOMS);
    const column = index % WIDTH;
    const row = Math.floor(index / WIDTH);
    const east = column < WIDTH - 1 ? indexOf(column + 1, row) : undefined;
    const south = row < HEIGHT - 1 ? indexOf(column, row + 1) : undefined;
    const neighbor = random() < 0.5 ? east : south;
    if (neighbor === undefined) continue;
    const key = pairKey(index, neighbor);
    if (planned.has(key)) continue;
    plan(index, neighbor, neighbor === east ? "East" : "South");
    extras += 1;
  }

  // Knot 1: a horizontal corridor A-M-B pinned reciprocally East, plus a
  // one-way chord from A to B declared West.
  const knot1Row = 1 + Math.floor(random() * (HEIGHT - 2));
  const knot1Col = Math.floor(random() * (WIDTH - 3));
  const knot1A = indexOf(knot1Col, knot1Row);
  const knot1M = indexOf(knot1Col + 1, knot1Row);
  const knot1B = indexOf(knot1Col + 2, knot1Row);
  plan(knot1A, knot1M, "East").forced = "reciprocal";
  plan(knot1M, knot1B, "East").forced = "reciprocal";
  const chords: { from: number; to: number; declared: LayoutDirection }[] = [
    { from: knot1A, to: knot1B, declared: "West" },
  ];

  // Knot 2: the vertical variant, offset to a different quadrant.
  let knot2Row = 0;
  let knot2Col = 0;
  do {
    knot2Row = Math.floor(random() * (HEIGHT - 3));
    knot2Col = 1 + Math.floor(random() * (WIDTH - 2));
  } while (Math.abs(knot2Row - knot1Row) < 3 && Math.abs(knot2Col - knot1Col) < 4);
  const knot2A = indexOf(knot2Col, knot2Row);
  const knot2M = indexOf(knot2Col, knot2Row + 1);
  const knot2B = indexOf(knot2Col, knot2Row + 2);
  plan(knot2A, knot2M, "South").forced = "reciprocal";
  plan(knot2M, knot2B, "South").forced = "reciprocal";
  chords.push({ from: knot2A, to: knot2B, declared: "North" });

  // Knot 3: one adjacent pair carrying two mutually infeasible reciprocal
  // links (East/West and North/South).
  let knot3 = 0;
  do {
    knot3 = indexOf(
      Math.floor(random() * (WIDTH - 1)),
      Math.floor(random() * HEIGHT),
    );
  } while (
    [knot1A, knot1M, knot1B, knot2A, knot2M, knot2B].includes(knot3) ||
    [knot1A, knot1M, knot1B, knot2A, knot2M, knot2B].includes(knot3 + 1)
  );
  const knot3Link = plan(knot3, knot3 + 1, "East");
  knot3Link.forced = "reciprocal";
  knot3Link.doubled = "South";

  const edges: LayoutEdge[] = [];
  for (const entry of [...planned.values()]) {
    const from = id(entry.a);
    const to = id(entry.b);
    if (entry.forced === "reciprocal") {
      reciprocalLink(edges, from, to, entry.outward);
    } else if (random() < 0.55) {
      reciprocalLink(edges, from, to, entry.outward);
    } else if (random() < 0.5) {
      link(edges, from, to, entry.outward);
    } else {
      link(edges, to, from, REVERSE[entry.outward]);
    }
    if (entry.doubled) reciprocalLink(edges, from, to, entry.doubled);
  }
  for (const chord of chords) {
    link(edges, id(chord.from), id(chord.to), chord.declared);
  }

  const positions: GridPosition[] = [];
  for (let index = 0; index < ROOMS; index += 1) {
    positions.push(at(index % WIDTH, Math.floor(index / WIDTH)));
  }
  for (let count = 0; count < 10; count += 1) {
    const victim = 1 + Math.floor(random() * (ROOMS - 1));
    const horizontal = random() < 0.5;
    const sign = random() < 0.5 ? -1 : 1;
    positions[victim] = horizontal
      ? at(positions[victim].x + sign, positions[victim].y)
      : at(positions[victim].x, positions[victim].y + sign);
  }

  const residents: LayoutResident[] = positions.map((position, index) => ({
    id: id(index),
    position,
    movable: index !== 0,
  }));
  return { residents, nodes: [], edges, centerId: id(0), allowExistingMoves: true };
}

// ---------------------------------------------------------------------------
// c. Stratified tower
// ---------------------------------------------------------------------------

/**
 * A ten-level tower. Each level has a three-room east-west spine; the spine
 * ends stack level-to-level through reciprocal cross-level Up/Down rays. Each
 * level also charts two same-level projected verticals (Up/Down links whose
 * `constraintVector` is a same-level diagonal) — a loft off each spine end —
 * and a cardinal wing north and south of the spine center, occasionally one
 * room deeper. A few loft and wing rooms drift one cell off their charted
 * positions per level.
 */
export function towerArea(seed: number): IntegralLayoutRequest {
  const random = xorshift32(seed);
  const LEVELS = 10;
  const id = (level: number, name: string): string => `tower-${level}-${name}`;

  const residents: LayoutResident[] = [];
  const edges: LayoutEdge[] = [];
  const add = (level: number, name: string, x: number, y: number): string => {
    const roomId = id(level, name);
    residents.push({
      id: roomId,
      position: at(x, y, level),
      movable: !(level === 0 && name === "s1"),
    });
    return roomId;
  };

  for (let level = 0; level < LEVELS; level += 1) {
    const s0 = add(level, "s0", 0, 0);
    const s1 = add(level, "s1", 1, 0);
    const s2 = add(level, "s2", 2, 0);
    reciprocalLink(edges, s0, s1, "East");
    reciprocalLink(edges, s1, s2, "East");

    // Same-level projected verticals: a loft off each spine end.
    const loftWest = add(level, "loft-w", -1, -1);
    reciprocalLink(edges, s0, loftWest, "Up", at(-1, -1));
    const loftEast = add(level, "loft-e", 3, -1);
    reciprocalLink(edges, s2, loftEast, "Up", at(1, -1));

    // Cardinal wings off the spine center.
    const wingNorth = add(level, "w-n", 1, -1);
    reciprocalLink(edges, s1, wingNorth, "North");
    const wingSouth = add(level, "w-s", 1, 1);
    reciprocalLink(edges, s1, wingSouth, "South");
    if (random() < 0.5) {
      const wingSouth2 = add(level, "w-s2", 1, 2);
      reciprocalLink(edges, wingSouth, wingSouth2, "South");
    }
    if (random() < 0.35) {
      const wingNorth2 = add(level, "w-n2", 1, -2);
      reciprocalLink(edges, wingNorth, wingNorth2, "North");
    }

    // Cross-level stacks through both spine ends.
    if (level > 0) {
      reciprocalLink(edges, id(level - 1, "s0"), s0, "Up");
      reciprocalLink(edges, id(level - 1, "s2"), s2, "Up");
    }
  }

  // Charting drift: nudge one satellite room on roughly half the levels.
  const satellites = ["loft-w", "loft-e", "w-n", "w-s"];
  const byId = new Map(residents.map((resident) => [resident.id, resident]));
  for (let level = 0; level < LEVELS; level += 1) {
    if (random() >= 0.5) continue;
    const name = satellites[Math.floor(random() * satellites.length)];
    const victim = byId.get(id(level, name));
    if (!victim || !victim.movable) continue;
    const sign = random() < 0.5 ? -1 : 1;
    victim.position = at(victim.position.x + sign, victim.position.y, level);
  }

  return {
    residents,
    nodes: [],
    edges,
    centerId: id(0, "s1"),
    allowExistingMoves: true,
  };
}

// ---------------------------------------------------------------------------
// d. High-degree hub
// ---------------------------------------------------------------------------

/**
 * A plaza with nine exits: four reciprocal cardinal spokes of 4-8 rooms, three
 * one-way diagonal arrival spokes (their first room exits into the plaza with
 * no return), and two reciprocal "Other" links to short side chains. Spoke
 * rooms drift perpendicular to their bearing with probability 0.3, so several
 * initial spokes wobble across each other's lanes.
 */
export function hubArea(seed: number): IntegralLayoutRequest {
  const random = xorshift32(seed);
  const residents: LayoutResident[] = [
    { id: "plaza", position: at(0, 0), movable: false },
  ];
  const edges: LayoutEdge[] = [];
  const spokeLength = (): number => 4 + Math.floor(random() * 5);

  const addRoom = (roomId: string, x: number, y: number): void => {
    residents.push({ id: roomId, position: at(x, y), movable: true });
  };
  const jitter = (
    x: number,
    y: number,
    perpendicular: { x: number; y: number },
  ): { x: number; y: number } => {
    if (random() >= 0.3) return { x, y };
    const sign = random() < 0.5 ? -1 : 1;
    return { x: x + sign * perpendicular.x, y: y + sign * perpendicular.y };
  };

  // Four reciprocal cardinal spokes.
  const cardinalBearings: readonly {
    direction: LayoutDirection;
    step: { x: number; y: number };
  }[] = [
    { direction: "North", step: { x: 0, y: -1 } },
    { direction: "East", step: { x: 1, y: 0 } },
    { direction: "South", step: { x: 0, y: 1 } },
    { direction: "West", step: { x: -1, y: 0 } },
  ];
  for (const bearing of cardinalBearings) {
    const length = spokeLength();
    const prefix = `spoke-${bearing.direction.toLowerCase()}`;
    const perpendicular = { x: bearing.step.y, y: bearing.step.x };
    let previous = "plaza";
    for (let step = 1; step <= length; step += 1) {
      const roomId = `${prefix}-${step}`;
      const charted = jitter(bearing.step.x * step, bearing.step.y * step, perpendicular);
      addRoom(roomId, charted.x, charted.y);
      reciprocalLink(edges, previous, roomId, bearing.direction);
      previous = roomId;
    }
  }

  // Three one-way arrival spokes on diagonal bearings. The chain is charted
  // outward; its first room falls into the plaza with no return exit.
  const arrivalBearings: readonly {
    outward: LayoutDirection;
    step: { x: number; y: number };
  }[] = [
    { outward: "Northeast", step: { x: 1, y: -1 } },
    { outward: "Southwest", step: { x: -1, y: 1 } },
    { outward: "Southeast", step: { x: 1, y: 1 } },
  ];
  for (const bearing of arrivalBearings) {
    const length = spokeLength();
    const prefix = `arrival-${bearing.outward.toLowerCase()}`;
    const perpendicular = { x: -bearing.step.y, y: bearing.step.x };
    let previous: string | undefined;
    for (let step = 1; step <= length; step += 1) {
      const roomId = `${prefix}-${step}`;
      const charted = jitter(bearing.step.x * step, bearing.step.y * step, perpendicular);
      addRoom(roomId, charted.x, charted.y);
      if (previous) reciprocalLink(edges, previous, roomId, bearing.outward);
      previous = roomId;
    }
    // The arrival itself: one-way from the chain's innermost room.
    link(edges, `${prefix}-1`, "plaza", REVERSE[bearing.outward]);
  }

  // Two "Other" side chains, linked to the plaza non-directionally.
  const otherChains: readonly {
    name: string;
    start: { x: number; y: number };
    step: { x: number; y: number };
    direction: LayoutDirection;
  }[] = [
    { name: "other-a", start: { x: -2, y: -2 }, step: { x: -1, y: 0 }, direction: "West" },
    { name: "other-b", start: { x: 2, y: 3 }, step: { x: 0, y: 1 }, direction: "South" },
  ];
  for (const chain of otherChains) {
    const length = spokeLength();
    const perpendicular = { x: chain.step.y, y: chain.step.x };
    let previous: string | undefined;
    for (let step = 1; step <= length; step += 1) {
      const roomId = `${chain.name}-${step}`;
      const charted = jitter(
        chain.start.x + chain.step.x * (step - 1),
        chain.start.y + chain.step.y * (step - 1),
        perpendicular,
      );
      addRoom(roomId, charted.x, charted.y);
      if (previous) reciprocalLink(edges, previous, roomId, chain.direction);
      previous = roomId;
    }
    reciprocalLink(edges, "plaza", `${chain.name}-1`, "Other");
  }

  return {
    residents,
    nodes: [],
    edges,
    centerId: "plaza",
    allowExistingMoves: true,
  };
}

// ---------------------------------------------------------------------------
// e. User-locked cluster under active re-charting
// ---------------------------------------------------------------------------

/**
 * A ~150-room area whose contiguous 40-room core (an 8x5 city neighborhood)
 * is user-pinned (`movable: false`) at exact positions, surrounded by three
 * movable district bands under active re-charting: jittered positions, two
 * duplicated-observation collisions, three gross misplacements, and nine
 * links into the pinned core — four geometrically consistent, five declaring
 * directions the pinned core makes permanently unsatisfiable.
 */
export function lockedClusterArea(seed: number): IntegralLayoutRequest {
  const random = xorshift32(seed);
  interface Cell {
    id: string;
    x: number;
    y: number;
    movable: boolean;
  }
  const cells: Cell[] = [];
  const byPosition = new Map<string, Cell>();
  const place = (id: string, x: number, y: number, movable: boolean): void => {
    const cell = { id, x, y, movable };
    cells.push(cell);
    byPosition.set(`${x}|${y}`, cell);
  };

  // The pinned core: 8 wide, 5 tall, at the origin.
  for (let y = 0; y < 5; y += 1) {
    for (let x = 0; x < 8; x += 1) {
      place(`core-${String(y * 8 + x).padStart(2, "0")}`, x, y, false);
    }
  }
  // Movable district bands with a one-cell aisle around the core.
  let bandIndex = 0;
  const band = (x: number, y: number): void => {
    place(`band-${String(bandIndex).padStart(3, "0")}`, x, y, true);
    bandIndex += 1;
  };
  for (let y = -4; y <= -2; y += 1) for (let x = -2; x <= 9; x += 1) band(x, y);
  for (let y = 6; y <= 8; y += 1) for (let x = -2; x <= 9; x += 1) band(x, y);
  for (let x = -5; x <= -3; x += 1) for (let y = -4; y <= 8; y += 1) band(x, y);

  // Grid adjacency: dense inside the core, looser across the districts.
  const edges: LayoutEdge[] = [];
  for (const cell of cells) {
    for (const [dx, dy, outward] of [[1, 0, "East"], [0, 1, "South"]] as const) {
      const neighbor = byPosition.get(`${cell.x + dx}|${cell.y + dy}`);
      if (!neighbor) continue;
      const core = !cell.movable && !neighbor.movable;
      const roll = random();
      if (core) {
        if (roll < 0.05) continue;
        reciprocalLink(edges, cell.id, neighbor.id, outward);
        continue;
      }
      if (roll < 0.10) continue;
      if (roll < 0.15) {
        if (random() < 0.5) link(edges, cell.id, neighbor.id, outward);
        else link(edges, neighbor.id, cell.id, REVERSE[outward]);
        continue;
      }
      reciprocalLink(edges, cell.id, neighbor.id, outward);
    }
  }

  // Nine links into the pinned core across the aisles. The consistent four
  // declare the direction the geometry supports (satisfiable with one cell of
  // aisle slack); the violating five declare a direction the pinned core and
  // the band mesh together make permanently unsatisfiable.
  const northEntry = (x: number): [string, string] => [`${x}|-2`, `${x}|0`];
  const southEntry = (x: number): [string, string] => [`${x}|8`, `${x}|4`];
  const westEntry = (y: number): [string, string] => [`-3|${y}`, `0|${y}`];
  const entries: {
    from: string;
    to: string;
    declared: LayoutDirection;
    reciprocal: boolean;
  }[] = [];
  const pickEntry = (
    build: (coordinate: number) => [string, string],
    range: [number, number],
    declared: LayoutDirection,
    reciprocal: boolean,
  ): void => {
    for (;;) {
      const coordinate = range[0] + Math.floor(random() * (range[1] - range[0] + 1));
      const [fromKey, toKey] = build(coordinate);
      const from = byPosition.get(fromKey);
      const to = byPosition.get(toKey);
      if (!from || !to) continue;
      if (entries.some((entry) => entry.from === from.id && entry.to === to.id)) continue;
      entries.push({ from: from.id, to: to.id, declared, reciprocal });
      return;
    }
  };
  // Consistent entries: two from the north (South into the core), one from
  // the south (North), one from the west (East).
  pickEntry(northEntry, [0, 7], "South", true);
  pickEntry(northEntry, [0, 7], "South", false);
  pickEntry(southEntry, [0, 7], "North", true);
  pickEntry(westEntry, [0, 4], "East", true);
  // Violating entries: the declared ray points away from the pinned core.
  pickEntry(northEntry, [0, 7], "North", true);
  pickEntry(northEntry, [0, 7], "East", false);
  pickEntry(southEntry, [0, 7], "South", true);
  pickEntry(westEntry, [0, 4], "West", false);
  pickEntry(westEntry, [0, 4], "North", true);
  for (const entry of entries) {
    if (entry.reciprocal) reciprocalLink(edges, entry.from, entry.to, entry.declared);
    else link(edges, entry.from, entry.to, entry.declared);
  }

  // Active re-charting on the movable bands: jitter, two
  // duplicated-observation collisions, three gross misplacements.
  const positions = new Map(cells.map((cell) => [cell.id, at(cell.x, cell.y)]));
  const movableCells = cells.filter((cell) => cell.movable);
  for (const cell of movableCells) {
    if (random() < 0.10) {
      const horizontal = random() < 0.5;
      const sign = random() < 0.5 ? -1 : 1;
      const current = positions.get(cell.id) as GridPosition;
      positions.set(
        cell.id,
        horizontal ? at(current.x + sign, current.y) : at(current.x, current.y + sign),
      );
    }
  }
  for (let count = 0; count < 2; count += 1) {
    const victim = movableCells[Math.floor(random() * movableCells.length)];
    const target = movableCells[Math.floor(random() * movableCells.length)];
    if (victim === target) continue;
    positions.set(victim.id, { ...positions.get(target.id) as GridPosition });
  }
  for (let count = 0; count < 3; count += 1) {
    const victim = movableCells[Math.floor(random() * movableCells.length)];
    const distance = 6 + Math.floor(random() * 5);
    const signX = random() < 0.5 ? -1 : 1;
    const signY = random() < 0.5 ? -1 : 1;
    const current = positions.get(victim.id) as GridPosition;
    positions.set(victim.id, at(current.x + signX * distance, current.y + signY * distance));
  }

  const residents: LayoutResident[] = cells.map((cell) => ({
    id: cell.id,
    position: positions.get(cell.id) as GridPosition,
    movable: cell.movable,
  }));
  return {
    residents,
    nodes: [],
    edges,
    centerId: "core-00",
    allowExistingMoves: true,
  };
}

/**
 * Bounded deterministic budgets for the locked-cluster constraint repair.
 * Wall-clock never terminates the search (`maxDurationMs: Infinity`), so the
 * pipeline is bit-for-bit reproducible. Budgets are sized to finish in a few
 * seconds at 151 rooms while still exercising restarts, extension states,
 * mask diversification, polish, and crossing repair.
 */
export const LOCKED_CLUSTER_REPAIR_OPTIONS: ConstraintRepairOptions = {
  when: "always",
  maxDurationMs: Number.POSITIVE_INFINITY,
  maxRestarts: 8,
  maxLayouts: 2,
  maxExtensionStates: 160,
  maxMaskDiversifications: 2,
  maxPolishTournaments: 1,
  maxCrossingWork: 48,
};

// ---------------------------------------------------------------------------
// f. Disconnected components
// ---------------------------------------------------------------------------

/**
 * Two ~30-room components with no path between them — a 6x5 grid district and
 * a separately charted 5x6 district to the east — plus one isolated room with
 * no exits at all, the way a partially charted area holds fragments.
 */
export function disconnectedArea(seed: number): IntegralLayoutRequest {
  const random = xorshift32(seed);
  const residents: LayoutResident[] = [];
  const edges: LayoutEdge[] = [];

  const component = (
    prefix: string,
    width: number,
    height: number,
    originX: number,
    originY: number,
  ): void => {
    const id = (index: number): string => `${prefix}-${String(index).padStart(2, "0")}`;
    const count = width * height;
    for (let index = 0; index < count; index += 1) {
      residents.push({
        id: id(index),
        position: at(originX + (index % width), originY + Math.floor(index / width)),
        movable: !(prefix === "a" && index === 0),
      });
    }
    for (let index = 0; index < count; index += 1) {
      const column = index % width;
      const row = Math.floor(index / width);
      for (const [neighbor, outward] of [
        [column < width - 1 ? index + 1 : undefined, "East"],
        [row < height - 1 ? index + width : undefined, "South"],
      ] as const) {
        if (neighbor === undefined) continue;
        const roll = random();
        if (roll < 0.08) continue;
        if (roll < 0.13) {
          if (random() < 0.5) link(edges, id(index), id(neighbor), outward);
          else link(edges, id(neighbor), id(index), REVERSE[outward]);
          continue;
        }
        reciprocalLink(edges, id(index), id(neighbor), outward);
      }
    }
    // Reconnect any stranded room through its lower-index neighbor so each
    // component is internally one piece.
    const neighbors = new Map<string, string[]>();
    for (const edge of edges) {
      if (!edge.from.startsWith(`${prefix}-`)) continue;
      neighbors.set(edge.from, [...(neighbors.get(edge.from) ?? []), edge.to]);
      neighbors.set(edge.to, [...(neighbors.get(edge.to) ?? []), edge.from]);
    }
    const reachable = new Set<string>([id(0)]);
    const frontier = [id(0)];
    while (frontier.length > 0) {
      const current = frontier.pop() as string;
      for (const next of neighbors.get(current) ?? []) {
        if (!reachable.has(next)) {
          reachable.add(next);
          frontier.push(next);
        }
      }
    }
    for (let index = 1; index < count; index += 1) {
      if (reachable.has(id(index))) continue;
      const column = index % width;
      if (column > 0) reciprocalLink(edges, id(index), id(index - 1), "West");
      else reciprocalLink(edges, id(index), id(index - width), "North");
      reachable.add(id(index));
    }
  };

  component("a", 6, 5, 0, 0);
  component("b", 5, 6, 10, 0);
  residents.push({ id: "hermit", position: at(20, 10), movable: true });

  // Light charting jitter on a few movable rooms in each component.
  for (let count = 0; count < 6; count += 1) {
    const victim = residents[Math.floor(random() * (residents.length - 1))];
    if (!victim.movable) continue;
    const horizontal = random() < 0.5;
    const sign = random() < 0.5 ? -1 : 1;
    victim.position = horizontal
      ? at(victim.position.x + sign, victim.position.y)
      : at(victim.position.x, victim.position.y + sign);
  }

  return {
    residents,
    nodes: [],
    edges,
    centerId: "a-00",
    allowExistingMoves: true,
  };
}

// ---------------------------------------------------------------------------
// g. Truncated growth sequence
// ---------------------------------------------------------------------------

/**
 * Four progressively larger snapshots of the same ~100-room area, the way
 * truncated `Map.Local` charts arrive across visits: rooms enter in
 * exploration (breadth-first) order at sizes 30, 55, 80, and 100, each
 * snapshot carrying only the edges among rooms already seen. Later-discovered
 * rooms chart sloppier — 20% carry a one-cell error. Replay with
 * `replayTruncatedGrowth`, which plans each snapshot's growth against the
 * previous plan's positions.
 */
export function truncatedGrowthArea(seed: number): IntegralLayoutRequest[] {
  const random = xorshift32(seed);
  const WIDTH = 10;
  const ROOMS = 100;
  const id = (index: number): string => `grow-${String(index).padStart(3, "0")}`;

  // Spanning maze plus cycle extras, with the house edge mix: ~80% reciprocal
  // cardinal, ~10% one-way cardinal, ~10% reciprocal "Other".
  interface AreaLink {
    a: number;
    b: number;
    outward: LayoutDirection;
  }
  const links: AreaLink[] = [];
  const used = new Set<string>();
  const pairKey = (a: number, b: number): string => a < b ? `${a}|${b}` : `${b}|${a}`;
  const addLink = (a: number, b: number, outward: LayoutDirection): void => {
    links.push({ a, b, outward });
    used.add(pairKey(a, b));
  };
  for (let index = 1; index < ROOMS; index += 1) {
    const column = index % WIDTH;
    const row = Math.floor(index / WIDTH);
    const viaWest = column > 0 && (row === 0 || random() < 0.5);
    if (viaWest) addLink(index - 1, index, "East");
    else addLink(index - WIDTH, index, "South");
  }
  let extras = 0;
  while (extras < 20) {
    const index = Math.floor(random() * ROOMS);
    const column = index % WIDTH;
    const row = Math.floor(index / WIDTH);
    const east = column < WIDTH - 1 ? index + 1 : undefined;
    const south = row < WIDTH - 1 ? index + WIDTH : undefined;
    const neighbor = random() < 0.5 ? east : south;
    if (neighbor === undefined || neighbor >= ROOMS) continue;
    if (used.has(pairKey(index, neighbor))) continue;
    addLink(index, neighbor, neighbor === east ? "East" : "South");
    extras += 1;
  }
  const edges: LayoutEdge[] = [];
  for (const areaLink of links) {
    const from = id(areaLink.a);
    const to = id(areaLink.b);
    const roll = random();
    if (roll < 0.8) {
      reciprocalLink(edges, from, to, areaLink.outward);
    } else if (roll < 0.9) {
      if (random() < 0.5) link(edges, from, to, areaLink.outward);
      else link(edges, to, from, REVERSE[areaLink.outward]);
    } else {
      reciprocalLink(edges, from, to, "Other");
    }
  }

  // Exploration order: breadth-first from the entrance over undirected
  // adjacency, neighbors visited in ascending index order.
  const neighbors: number[][] = Array.from({ length: ROOMS }, () => []);
  for (const areaLink of links) {
    neighbors[areaLink.a].push(areaLink.b);
    neighbors[areaLink.b].push(areaLink.a);
  }
  for (const list of neighbors) list.sort((a, b) => a - b);
  const order: number[] = [0];
  const seen = new Set<number>([0]);
  for (let cursor = 0; cursor < order.length; cursor += 1) {
    for (const next of neighbors[order[cursor]]) {
      if (!seen.has(next)) {
        seen.add(next);
        order.push(next);
      }
    }
  }

  // Charted positions: the true grid; rooms discovered later carry more
  // charting error.
  const positions: GridPosition[] = [];
  for (let index = 0; index < ROOMS; index += 1) {
    positions.push(at(index % WIDTH, Math.floor(index / WIDTH)));
  }
  const discoveryRank = new Map<number, number>();
  order.forEach((roomIndex, rank) => discoveryRank.set(roomIndex, rank));
  for (let index = 1; index < ROOMS; index += 1) {
    const rank = discoveryRank.get(index) ?? 0;
    if (rank >= 55 && random() < 0.2) {
      const horizontal = random() < 0.5;
      const sign = random() < 0.5 ? -1 : 1;
      positions[index] = horizontal
        ? at(positions[index].x + sign, positions[index].y)
        : at(positions[index].x, positions[index].y + sign);
    }
  }

  const snapshots: IntegralLayoutRequest[] = [];
  for (const size of [30, 55, 80, 100]) {
    const included = new Set(order.slice(0, size).map(id));
    snapshots.push({
      residents: order.slice(0, size).map((index) => ({
        id: id(index),
        position: { ...positions[index] },
        movable: index !== 0,
      })),
      nodes: [],
      edges: edges.filter((edge) => included.has(edge.from) && included.has(edge.to)),
      centerId: id(0),
      allowExistingMoves: true,
    });
  }
  return snapshots;
}

export interface GrowthReplayStep {
  /** The snapshot request actually planned, with carried-forward positions. */
  request: IntegralLayoutRequest;
  plan: IntegralLayoutPlan;
}

/**
 * Replay a truncated-growth sequence: each snapshot plans against the
 * previous plan's positions, exactly as the mapper re-charts an area whose
 * durable rooms already sit where the last visit left them.
 */
export function replayTruncatedGrowth(
  snapshots: readonly IntegralLayoutRequest[],
): GrowthReplayStep[] {
  const steps: GrowthReplayStep[] = [];
  let carried: ReadonlyMap<string, GridPosition> | undefined;
  for (const snapshot of snapshots) {
    const request: IntegralLayoutRequest = {
      ...snapshot,
      residents: snapshot.residents.map((resident) => {
        const position = carried?.get(resident.id);
        return position ? { ...resident, position: { ...position } } : resident;
      }),
    };
    const plan = planIntegralLayout(request);
    steps.push({ request, plan });
    carried = plan.positions;
  }
  return steps;
}

// ---------------------------------------------------------------------------
// Ratchet scenarios
// ---------------------------------------------------------------------------

export interface RealisticScenario {
  name: string;
  /**
   * The layout this fixture exists to reach — recorded beside the ratchet
   * tuple so the gap stays visible in reports. Never asserted.
   */
  aspiration: string;
  rooms: number;
  edges: number;
  run: () => Readonly<LayoutQuality>;
}

function memo<T>(compute: () => T): () => T {
  let value: T | undefined;
  let ready = false;
  return () => {
    if (!ready) {
      value = compute();
      ready = true;
    }
    return value as T;
  };
}

/**
 * The complete fixture/pipeline list in a stable order. Shared work — the
 * locked-cluster standard plan that seeds its constraint repair, and the
 * growth replay whose steps are separate scenarios — is memoized so the
 * corpus runs each pipeline exactly once.
 */
export function realisticScenarios(): RealisticScenario[] {
  const scenarios: RealisticScenario[] = [];

  const denseGrid = denseGridArea(REALISTIC_SEEDS.denseGrid);
  scenarios.push({
    name: "dense-grid/reflow",
    aspiration:
      "only the three misobserved exits relaxed, zero routing violations, zero slack: the grid re-charts as a grid",
    rooms: denseGrid.residents.length,
    edges: denseGrid.edges.length,
    run: () => planIntegralLayout(denseGrid).quality,
  });

  const maze = oneWayMazeArea(REALISTIC_SEEDS.oneWayMaze);
  scenarios.push({
    name: "one-way-maze/reflow",
    aspiration:
      "only the three observation knots relaxed (breaking a non-reciprocal chord wherever one suffices) and zero link crossings",
    rooms: maze.residents.length,
    edges: maze.edges.length,
    run: () => planIntegralLayout(maze).quality,
  });

  const tower = towerArea(REALISTIC_SEEDS.tower);
  scenarios.push({
    name: "tower/reflow",
    aspiration:
      "zero violations of any kind and zero slack: stacks aligned across all ten levels, lofts on their projected diagonals",
    rooms: tower.residents.length,
    edges: tower.edges.length,
    run: () => planIntegralLayout(tower).quality,
  });

  const hub = hubArea(REALISTIC_SEEDS.hub);
  scenarios.push({
    name: "hub/reflow",
    aspiration:
      "all nine spokes untangled: cardinal spokes on their exact rays with zero slack, zero crossings among arrival and Other chains",
    rooms: hub.residents.length,
    edges: hub.edges.length,
    run: () => planIntegralLayout(hub).quality,
  });

  const lockedCluster = lockedClusterArea(REALISTIC_SEEDS.lockedCluster);
  const lockedClusterStandard = memo(() => planIntegralLayout(lockedCluster));
  scenarios.push({
    name: "locked-cluster/reflow",
    aspiration:
      "only the five deliberately violating core entries relaxed; the districts re-chart cleanly around the pinned core",
    rooms: lockedCluster.residents.length,
    edges: lockedCluster.edges.length,
    run: () => lockedClusterStandard().quality,
  });
  scenarios.push({
    name: "locked-cluster/constraint-repair",
    aspiration:
      "repair proves the five-entry relaxation minimal (constraintOptimal) and removes every crossing the reflow left",
    rooms: lockedCluster.residents.length,
    edges: lockedCluster.edges.length,
    run: () =>
      repairIntegralLayoutConstraints(
        lockedCluster,
        lockedClusterStandard(),
        LOCKED_CLUSTER_REPAIR_OPTIONS,
      ).quality,
  });

  const disconnected = disconnectedArea(REALISTIC_SEEDS.disconnected);
  scenarios.push({
    name: "disconnected/reflow",
    aspiration:
      "both components compact with disjoint bounding boxes, zero violations, zero slack; the isolated room stays out of both",
    rooms: disconnected.residents.length,
    edges: disconnected.edges.length,
    run: () => planIntegralLayout(disconnected).quality,
  });

  const growthSnapshots = truncatedGrowthArea(REALISTIC_SEEDS.truncatedGrowth);
  const growthReplay = memo(() => replayTruncatedGrowth(growthSnapshots));
  growthSnapshots.forEach((snapshot, index) => {
    scenarios.push({
      name: `truncated-growth/step-${index + 1}`,
      aspiration:
        "each visit's growth lands without regressing the settled chart; the final snapshot matches a from-scratch reflow",
      rooms: snapshot.residents.length,
      edges: snapshot.edges.length,
      run: () => growthReplay()[index].plan.quality,
    });
  });

  return scenarios;
}
