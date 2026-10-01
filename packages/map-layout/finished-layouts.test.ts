/**
 * What every layout the planner publishes, and every plan it returns, has been
 * through. A request whose chart nodes are all residents polishes the whole
 * map: it gets the whole-map compaction stages, and every layout it streams,
 * its final plan included, sits at the cheap compaction fixed point, which
 * `compactIntegralLayoutPlan` with `axisGroupCompaction: false` computes. A
 * request that places a new room keeps its low-latency path. No plan ranks
 * below a layout published before it.
 */

import assert from "node:assert/strict";
import test from "node:test";
import {
  compactIntegralLayoutPlan,
  compareLayoutQuality,
  measureIntegralLayoutQuality,
  planIntegralLayout,
  repairIntegralLayoutCrossingsDeep,
  type ConstraintRepairOptions,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
  type LayoutEdge,
  type LayoutTraceCandidate,
  type LayoutTraceEvent,
} from "./layout.ts";
import { repairIntegralLayoutConstraints } from "./constraint-layout.ts";
import {
  disconnectedArea,
  hubArea,
  lockedClusterArea,
  LOCKED_CLUSTER_REPAIR_OPTIONS,
  REALISTIC_SEEDS,
} from "./realistic-fixtures.ts";

const at = (x: number, y: number, level = 0): GridPosition => ({ x, y, level });

/**
 * The request the mapper's quiet polish sends: every room of the map as a
 * resident and, as chart nodes, the local chart around the player, the rooms
 * within `radius` cells of the center, each one already a resident.
 */
function quietPolish(request: IntegralLayoutRequest, radius = 6): IntegralLayoutRequest {
  const centerId = request.centerId ?? request.residents[0].id;
  const center = request.residents.find((resident) => resident.id === centerId)?.position as
    GridPosition;
  return {
    ...request,
    centerId,
    allowExistingMoves: true,
    nodes: request.residents
      .filter((resident) =>
        Math.max(
          Math.abs(resident.position.x - center.x),
          Math.abs(resident.position.y - center.y),
        ) <= radius
      )
      .map((resident) => ({
        id: resident.id,
        relative: {
          x: resident.position.x - center.x,
          y: resident.position.y - center.y,
          level: resident.position.level - center.level,
        },
      })),
  };
}

/**
 * The same request after the player steps through an unmapped door of a
 * charted room into the free cell beyond it: the charted room nearest the
 * center with a free cell beside it.
 */
function withNewRoom(request: IntegralLayoutRequest): IntegralLayoutRequest {
  const positions = new Map(request.residents.map((resident) => [resident.id, resident.position]));
  const occupied = new Set([...positions.values()].map(({ x, y, level }) => `${x},${y},${level}`));
  const doors = [
    { direction: "North", back: "South", offset: at(0, -1) },
    { direction: "East", back: "West", offset: at(1, 0) },
    { direction: "South", back: "North", offset: at(0, 1) },
    { direction: "West", back: "East", offset: at(-1, 0) },
  ] as const;
  const charted = [...request.nodes].sort((a, b) =>
    Math.abs(a.relative.x) + Math.abs(a.relative.y) - Math.abs(b.relative.x) - Math.abs(b.relative.y) ||
    (a.id < b.id ? -1 : a.id > b.id ? 1 : 0)
  );
  for (const node of charted) {
    const stored = positions.get(node.id) as GridPosition;
    const door = doors.find(({ offset }) =>
      !occupied.has(`${stored.x + offset.x},${stored.y + offset.y},${stored.level}`)
    );
    if (!door) continue;
    return {
      ...request,
      nodes: [...request.nodes, {
        id: "new-room",
        relative: {
          x: node.relative.x + door.offset.x,
          y: node.relative.y + door.offset.y,
          level: node.relative.level,
        },
      }],
      edges: [
        ...request.edges,
        { from: node.id, to: "new-room", direction: door.direction },
        { from: "new-room", to: node.id, direction: door.back },
      ],
    };
  }
  throw new Error("no charted room has a free cell beside it");
}

/** A 14-room bridge tree whose stored layout crosses six of its links. */
function sixCrossingCluster(): IntegralLayoutRequest {
  const cells = [
    [4, 4], [5, -2], [4, 1], [-3, -3], [4, 0], [3, -2], [3, 2],
    [1, 0], [6, -5], [-2, 2], [4, -6], [0, -6], [-1, -3], [-1, 4],
  ] as const;
  const parents = [0, 1, 0, 3, 4, 5, 2, 1, 4, 3, 9, 2, 5] as const;
  const id = (index: number): string => `cluster-${index}`;
  return {
    residents: cells.map(([x, y], index) => ({ id: id(index), position: at(x, y), movable: index !== 0 })),
    nodes: [],
    edges: parents.flatMap((parent, offset): LayoutEdge[] => [
      { from: id(parent), to: id(offset + 1), direction: "Other" },
      { from: id(offset + 1), to: id(parent), direction: "Other" },
    ]),
    centerId: id(0),
    allowExistingMoves: true,
  };
}

/** A 12-room bridge tree scattered across the map, rays and links alike out of place. */
function scatteredBridgeTree(): IntegralLayoutRequest {
  return bridgeTree([
    [-8, 11], [-5, 5], [10, 2], [-2, -3], [-7, -6], [9, -4],
    [-8, -3], [-8, 5], [8, 4], [1, -6], [5, -10], [-6, 0],
  ]);
}

/**
 * The same tree with every ray held and no route blocked, but three links
 * crossing, which only the deep crossing repair's nested transactions clear.
 */
function tangledBridgeTree(): IntegralLayoutRequest {
  return bridgeTree([
    [-8, 11], [-10, 11], [-10, 12], [-11, 11], [-8, 12], [-9, 12],
    [-8, 10], [-11, 8], [-10, 9], [-7, 7], [-11, 9], [-7, 9],
  ]);
}

function bridgeTree(cells: readonly (readonly [number, number])[]): IntegralLayoutRequest {
  const parents = [0, 1, 1, 0, 4, 0, 5, 7, 2, 3, 0] as const;
  const directions = [
    "West", "South", "West", "South", "West", "North", "Other", "Other", "Other", "Other", "Other",
  ] as const;
  const reverses = { North: "South", South: "North", West: "East", Other: "Other" } as const;
  return {
    residents: cells.map(([x, y], index) => ({
      id: `tree-${index}`,
      position: at(x, y),
      movable: index !== 0,
    })),
    nodes: [],
    edges: parents.flatMap((parent, offset): LayoutEdge[] => [
      { from: `tree-${parent}`, to: `tree-${offset + 1}`, direction: directions[offset] },
      { from: `tree-${offset + 1}`, to: `tree-${parent}`, direction: reverses[directions[offset]] },
    ]),
    centerId: "tree-0",
    allowExistingMoves: true,
  };
}

/** A 13-room tree of undirected links whose stored layout crosses ten of them. */
function crossedTree(): IntegralLayoutRequest {
  const cells = [
    [-7, -4], [-8, -4], [-2, 2], [-8, -2], [4, 3], [8, -5], [3, 3],
    [-2, 7], [2, -6], [-6, 8], [3, 5], [5, 7], [8, -8],
  ] as const;
  const parents = [0, 0, 0, 1, 1, 2, 4, 1, 6, 8, 5, 2] as const;
  return {
    residents: cells.map(([x, y], index) => ({
      id: `crossed-${index}`,
      position: at(x, y),
      movable: index !== 0,
    })),
    nodes: [],
    edges: parents.flatMap((parent, offset): LayoutEdge[] => [
      { from: `crossed-${parent}`, to: `crossed-${offset + 1}`, direction: "Other" },
      { from: `crossed-${offset + 1}`, to: `crossed-${parent}`, direction: "Other" },
    ]),
    centerId: "crossed-0",
    allowExistingMoves: true,
  };
}

/** A bounded, deterministic repair for the small fixtures. */
const SMALL_REPAIR: ConstraintRepairOptions = {
  when: "always",
  maxDurationMs: Number.POSITIVE_INFINITY,
  maxRestarts: 8,
  maxLayouts: 2,
  maxExtensionStates: 256,
  maxMaskDiversifications: 4,
  maxPolishTournaments: 1,
  maxPolishPasses: 4,
  maxCrossingWork: 200,
};

/** A repair that reaches its final crossing stage with crossings left to clear. */
const CROSSING_REPAIR: ConstraintRepairOptions = {
  when: "always",
  maxDurationMs: Number.POSITIVE_INFINITY,
  maxRestarts: 1,
  maxLayouts: 1,
  maxPolishTournaments: 1,
  maxCrossingWork: 200,
};

function plannedPositions(candidate: LayoutTraceCandidate): Map<string, GridPosition> {
  assert.ok(candidate.positions, "a published layout carries its positions");
  return new Map(candidate.positions.map(({ id, x, y, level }) => [id, { x, y, level }]));
}

function measuredPlan(
  request: IntegralLayoutRequest,
  positions: ReadonlyMap<string, GridPosition>,
): IntegralLayoutPlan {
  return {
    positions,
    movedExisting: new Set(),
    quality: measureIntegralLayoutQuality(positions, request.edges),
  };
}

/** Fail unless the cheap compaction fixed point leaves `positions` exactly where they are. */
function assertFinished(
  request: IntegralLayoutRequest,
  positions: ReadonlyMap<string, GridPosition>,
  label: string,
): void {
  const plan = measuredPlan(request, positions);
  assert.equal(
    compactIntegralLayoutPlan(request, plan, { axisGroupCompaction: false }),
    plan,
    `${label} is not at the cheap compaction fixed point`,
  );
}

interface Publication {
  source: string;
  positions: ReadonlyMap<string, GridPosition>;
  quality: IntegralLayoutPlan["quality"];
}

/**
 * The layouts a progress observer receives, as the Worker client forwards
 * them: crossing transactions and constraint improvements while each ranks
 * above the last layout forwarded, and the standard plan as it completes.
 */
class PublicationRecorder {
  readonly publications: Publication[] = [];
  #last: IntegralLayoutPlan["quality"] | undefined;

  readonly trace = (event: LayoutTraceEvent): void => {
    if (event.type === "crossing-repair") this.#offer(`crossing ${event.mode}`, event.after);
    if (event.type === "constraint-improvement") this.#offer("constraint", event.candidate);
    if (event.type === "preview") this.#offer("preview", event.candidate);
  };

  standard(plan: IntegralLayoutPlan): void {
    this.#last = plan.quality;
    this.publications.push({ source: "standard", positions: plan.positions, quality: plan.quality });
  }

  #offer(source: string, candidate: LayoutTraceCandidate): void {
    if (this.#last && compareLayoutQuality(candidate.quality, this.#last) <= 0) return;
    this.#last = candidate.quality;
    this.publications.push({ source, positions: plannedPositions(candidate), quality: candidate.quality });
  }
}

function assertNeverBelow(
  plan: IntegralLayoutPlan,
  publications: readonly Publication[],
  label: string,
): void {
  for (const [index, publication] of publications.entries()) {
    assert.ok(
      compareLayoutQuality(plan.quality, publication.quality) >= 0,
      `${label} ranks below publication ${index} (${publication.source})`,
    );
  }
}

const realisticQuietPolishes = (): [string, IntegralLayoutRequest][] => [
  ["hub", quietPolish(hubArea(REALISTIC_SEEDS.hub))],
  ["disconnected", quietPolish(disconnectedArea(REALISTIC_SEEDS.disconnected))],
  ["locked cluster", quietPolish(lockedClusterArea(REALISTIC_SEEDS.lockedCluster))],
  ["six-crossing cluster", quietPolish(sixCrossingCluster())],
];

test("a polish that sends its local chart gets the whole-map compaction a reflow gets", () => {
  for (const [label, request] of realisticQuietPolishes()) {
    assert.ok(request.nodes.length > 0, `${label}: the polish sends a chart`);
    const trace: LayoutTraceEvent[] = [];
    const charted = planIntegralLayout({ ...request, trace: (event) => trace.push(event) });
    const reflowed = planIntegralLayout({ ...request, nodes: [] });

    assert.ok(
      trace.some((event) => event.type === "axis-progress" && event.phase === "gravity"),
      `${label}: axis-group compaction ran`,
    );
    assert.equal(
      compareLayoutQuality(charted.quality, reflowed.quality),
      0,
      `${label}: the chart costs the polish none of the reflow's quality`,
    );
    assert.equal(
      compactIntegralLayoutPlan(request, charted),
      charted,
      `${label}: a whole-map compaction finds nothing left to gain`,
    );
  }
  // The stored cluster crosses six links; its whole-map compaction clears every one.
  const cluster = planIntegralLayout(quietPolish(sixCrossingCluster()));
  assert.equal(cluster.quality.linkCrossings, 0);
});

test("a whole-map polish previews its layout before the axis-group pass, and its plan never ranks below it", () => {
  for (const [label, request] of realisticQuietPolishes()) {
    const trace: LayoutTraceEvent[] = [];
    const plan = planIntegralLayout({ ...request, trace: (event) => trace.push(event) });
    const previews = trace.filter((event) => event.type === "preview");
    assert.equal(previews.length, 1, `${label}: one preview`);
    const preview = previews[0];
    assert.ok(preview.type === "preview");
    assert.ok(
      trace.indexOf(preview) < trace.findIndex((event) => event.type === "axis-progress"),
      `${label}: the preview comes before the axis-group pass`,
    );
    assertFinished(request, plannedPositions(preview.candidate), `${label}: the preview`);
    assert.ok(
      compareLayoutQuality(plan.quality, preview.candidate.quality) >= 0,
      `${label}: the plan ranks at or above its preview`,
    );
    // Without the axis-group pass there is nothing to wait for.
    const unpacked: LayoutTraceEvent[] = [];
    planIntegralLayout({ ...request, trace: (event) => unpacked.push(event) }, { axisGroupCompaction: false });
    assert.equal(unpacked.some((event) => event.type === "preview"), false, `${label}: no pass, no preview`);
  }
});

test("a request that places a new room keeps its low-latency path", () => {
  for (const [label, request] of realisticQuietPolishes()) {
    const trace: LayoutTraceEvent[] = [];
    const plan = planIntegralLayout({ ...withNewRoom(request), trace: (event) => trace.push(event) });
    assert.ok(plan.positions.has("new-room"), `${label}: the new room is placed`);
    assert.equal(
      trace.some((event) => event.type === "axis-progress" || event.type === "preview"),
      false,
      `${label}: neither axis-group compaction nor series spacing ran, and nothing was previewed`,
    );
  }
});

test("every layout a quiet polish publishes is finished, and its plans never rank below one", () => {
  const polishes: [string, IntegralLayoutRequest, ConstraintRepairOptions][] = [
    ["scattered bridge tree", quietPolish(scatteredBridgeTree()), SMALL_REPAIR],
    ["tangled bridge tree", quietPolish(tangledBridgeTree()), CROSSING_REPAIR],
    ["six-crossing cluster", quietPolish(sixCrossingCluster()), SMALL_REPAIR],
    ["locked cluster", quietPolish(lockedClusterArea(REALISTIC_SEEDS.lockedCluster)), LOCKED_CLUSTER_REPAIR_OPTIONS],
  ];
  const sources = new Set<string>();
  for (const [label, request, options] of polishes) {
    const recorder = new PublicationRecorder();
    const standard = planIntegralLayout({ ...request, trace: recorder.trace });
    assertNeverBelow(standard, recorder.publications, `${label}: the standard plan`);
    recorder.standard(standard);
    const repaired = repairIntegralLayoutConstraints(
      { ...request, trace: recorder.trace },
      standard,
      options,
      recorder.trace,
    );
    assertNeverBelow(repaired, recorder.publications, `${label}: the repaired plan`);
    for (const [index, publication] of recorder.publications.entries()) {
      sources.add(publication.source);
      assertFinished(request, publication.positions, `${label}: publication ${index} (${publication.source})`);
    }
    assertFinished(request, repaired.positions, `${label}: the repaired plan`);
  }
  // The stream these polishes publish covers each source the client forwards.
  assert.deepEqual(
    [...sources].sort(),
    ["constraint", "crossing deep", "crossing quick", "preview", "standard"],
  );
});

test("a deep crossing repair publishes compacted layouts and returns the last one", () => {
  let longestStream = 0;
  for (const request of [crossedTree(), scatteredBridgeTree()]) {
    const seed = measuredPlan(
      request,
      new Map(request.residents.map((resident) => [resident.id, resident.position])),
    );
    const published: ReadonlyMap<string, GridPosition>[] = [];
    let previous = seed.quality;
    const result = repairIntegralLayoutCrossingsDeep(request, seed, {
      maximumWork: 80,
      onProgress: (progress) => {
        if (progress.kind !== "improvement") return;
        assert.ok(compareLayoutQuality(progress.candidate.quality, previous) > 0);
        previous = progress.candidate.quality;
        published.push(plannedPositions(progress.candidate));
      },
    });
    assert.ok(published.length >= 1, "the repair publishes its improvement");
    longestStream = Math.max(longestStream, published.length);
    for (const [index, positions] of published.entries()) {
      // A deep publication is at the full compaction fixed point, which ends
      // at the cheap one.
      const plan = measuredPlan(request, positions);
      assert.equal(
        compactIntegralLayoutPlan(request, plan),
        plan,
        `deep publication ${index} is not at the compaction fixed point`,
      );
      assertFinished(request, positions, `deep publication ${index}`);
    }
    assert.deepEqual(
      [...result.plan.positions].sort(),
      [...(published.at(-1) as ReadonlyMap<string, GridPosition>)].sort(),
    );
  }
  assert.ok(longestStream >= 2, "a repair publishes a stream of layouts");
});
