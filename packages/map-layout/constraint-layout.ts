import {
  compactIntegralLayoutPlan,
  compareLayoutQuality,
  computeIntegralRouteAmendments,
  DIRECTIONAL_VIOLATION_WEIGHT,
  directionalViolationEdges,
  LEVEL_VIOLATION_WEIGHT,
  levelViolationEdges,
  measureIntegralLayoutQuality,
  measureLayoutRoutingQuality,
  packsAxisGroups,
  planIntegralLayout,
  repairIntegralLayoutCrossingsDeep,
  withIntegralLayoutCandidateAdmission,
  type ConstraintRepairOptions,
  type ConstraintRepairOutcome,
  type ConstraintRepairReport,
  type ConstraintRepairWorkStats,
  type GridPosition,
  type IntegralLayoutPlan,
  type IntegralLayoutRequest,
  type LayoutDirection,
  type LayoutEdge,
  type LayoutQuality,
  type LayoutTraceEvent,
} from "./layout.ts";
import { planLayoutModel, type LayoutModel } from "./model.ts";
import {
  searchConstraintExtensions,
  type ConstraintExtensionAlternative,
  type ConstraintExtensionArc,
  type ConstraintExtensionDefect,
  type ConstraintExtensionInspection,
} from "./constraint-extension-search.ts";

const AXIS_NAMES = ["x", "y", "level"] as const;
const ORTHOGONAL_VECTORS: Partial<Record<LayoutDirection, readonly [number, number, number]>> = {
  North: [0, -1, 0],
  East: [1, 0, 0],
  South: [0, 1, 0],
  West: [-1, 0, 0],
  Up: [0, 0, 1],
  Down: [0, 0, -1],
};
const DEFAULT_MAX_DURATION_MS = 10_000;
const DEFAULT_MAX_RESTARTS = 10_000;
const RANDOM_SEED = 0x5EED1234;
const PROGRESS_INTERVAL_MS = 30;
const SEARCH_PROGRESS_CHECK_MASK = 0x0f;
const COMPACTION_PROGRESS_CHECK_MASK = 0x01;
const DEFAULT_MAX_EXTENSION_STATES = 16_384;
const DEFAULT_MAX_LIVE_SEARCH_NODES = 32_768;
const DEFAULT_MAX_MASK_DIVERSIFICATIONS = 256;
/**
 * Deterministic ceilings for the exact hitting-set master strategy. Each
 * iteration costs one feasibility check; the node budget is shared across all
 * of an operation's exact solves and their components. Both are far above
 * what realistic conflict structures need — hitting either one means conflict
 * accumulation went pathological, and the search falls back to seeded
 * randomized restarts.
 */
const MAX_HITTING_SET_ITERATIONS = 256;
const MAX_HITTING_SET_DFS_NODES = 262_144;
/**
 * A level equality no relation can give up (see `levelEqualityFrom`): it
 * joins no removable relation to a conflict.
 */
const HARD_LEVEL_EQUALITY = -1;

interface DenseConstraintGraph {
  ids: string[];
  indexById: Map<string, number>;
  positions: readonly Int32Array[];
  sourceEdges: LayoutEdge[];
  sourceIndexes: Int32Array;
  from: Int32Array;
  to: Int32Array;
  axis: Int8Array;
  sign: Int8Array;
  step: Int32Array;
  reciprocal: Uint8Array;
  /** Feasibility-equivalent source edges share one canonical relation. */
  groupOfEdge: Int32Array;
  /** Indexed by directional relation group, as are `groupFrom` through `groupStep`. */
  groupAxis: Int8Array;
  groupFrom: Int32Array;
  groupTo: Int32Array;
  groupStep: Int32Array;
  /**
   * Directed source-edge cost paid when this relation is relaxed. The weights
   * are indexed by relation: the directional groups, then the level relations.
   */
  groupSourceCount: Int32Array;
  /** Reciprocal directed source-edge cost paid when this relation is relaxed. */
  groupReciprocalCount: Int32Array;
  /**
   * 1 for a level relation and 0 for a directional group: the master
   * objective's third weight, so that of two equally costly relaxations the
   * search keeps rooms on their levels.
   */
  groupLevelCount: Int32Array;
  /**
   * The level relation of a flat directional group in a contested level class
   * (see `compileGraph`), or -1. The group's rays hold only while its level
   * relation does, so giving up the level relation also gives up the group.
   */
  groupLevelRelation: Int32Array;
  /** The flat directional group of each level relation, indexed from `groupCount`. */
  levelRelationGroup: Int32Array;
  /** Nodes whose topological component contains an authoritative level-crossing relation. */
  levelCrossingReachable: Uint8Array;
  /**
   * Node pairs that share one level whatever the mask: by default the
   * endpoints of every flat relation without a level relation; under fixed
   * levels, the rooms stored on one level. They carry no removable relation
   * into a conflict.
   */
  levelEqualityFrom: Int32Array;
  levelEqualityTo: Int32Array;
  /** Directional relation groups, indexed first in every relation mask. */
  groupCount: number;
  /**
   * The length of a relation mask: the directional groups, then one level
   * relation for each flat group of a contested level class.
   */
  relationCount: number;
  nodeCount: number;
  edgeCount: number;
  /** Lazily attached feasibility-check scratch arena; see `AnalysisScratch`. */
  scratch?: AnalysisScratch;
  /** Lazily attached candidate-admission scratch; see `AdmissionScratch`. */
  admission?: AdmissionScratch;
}

interface AxisGraph {
  head: Int32Array;
  to: Int32Array;
  next: Int32Array;
  sourceNode: Int32Array;
  targetNode: Int32Array;
  sourceRoot: Int32Array;
  targetRoot: Int32Array;
  edge: Int32Array;
  step: Int32Array;
  length: number;
}

interface FeasibleState {
  roots: readonly Int32Array[];
  graphs: readonly AxisGraph[];
}

interface AnalysisResult {
  conflict?: number[];
  state?: FeasibleState;
}

type ConstraintFailureReason =
  | "analysis"
  | "compaction"
  | "time"
  | "work";

type ConstraintResult<T> =
  | { ok: true; value: T }
  | { ok: false; reason: ConstraintFailureReason };

interface SearchRuntimeOptions {
  now?: () => number;
  maximumFeasibilityChecks?: number;
  /** Test seam: deterministic node budget shared by the exact hitting-set solves. */
  maximumHittingSetNodes?: number;
  progress?: (progress: ConstraintRepairWorkStats & {
    phase: "search" | "compaction" | "polish";
    restarts: number;
    feasibilityChecks: number;
    layoutsConsidered: number;
    compactionAttempts: number;
    elapsedMs: number;
    bestQuality?: Readonly<import("./layout.ts").LayoutQuality>;
  }) => void;
  /** Streams each feasible canonical mask before randomized exploration ends. */
  mask?: (
    removedGroups: Uint8Array,
    progress: { restarts: number; feasibilityChecks: number; elapsedMs: number },
  ) => boolean | void;
}

interface ConstraintSearchResult {
  /** Canonical relation-group mask for the best weighted master incumbent. */
  removed: Uint8Array;
  /** Best distinct feasible group masks retained for geometric diversification. */
  masks: Uint8Array[];
  score: RelaxationScore;
  lowerBound: number;
  optimal: boolean;
  cutoff: ConstraintRepairReport["cutoff"];
  restarts: number;
  feasibilityChecks: number;
  elapsedMs: number;
}

interface RepairRuntimeOptions extends SearchRuntimeOptions {
  search?: (
    graph: DenseConstraintGraph,
    options: ConstraintRepairOptions,
    standardPositions: ReadonlyMap<string, GridPosition>,
    runtime: SearchRuntimeOptions,
  ) => ConstraintResult<ConstraintSearchResult>;
  compact?: (
    graph: DenseConstraintGraph,
    removed: Uint8Array,
  ) => ConstraintResult<Map<string, GridPosition>>;
  /** Test seam for the compaction-only polish applied to publishable raw incumbents. */
  gravity?: typeof compactIntegralLayoutPlan;
  polish?: typeof polishConstraintLayoutToFixedPoint;
  /** Test-only collision injector for dense position-dedup hashes. */
  densePositionHash?: (
    graph: DenseConstraintGraph,
    positions: readonly (readonly [number, number, number])[],
  ) => ConstraintPositionHash | undefined;
  /** Test-only collision injector forwarded to the geometric compactor. */
  softDefectHash?: ConstraintCompactionRuntime["softDefectHash"];
}

class DenseUnionFind {
  parent: Int32Array;
  #rank: Uint8Array;

  constructor(size: number) {
    this.parent = new Int32Array(size);
    this.#rank = new Uint8Array(size);
    for (let index = 0; index < size; index += 1) this.parent[index] = index;
  }

  /** Restores the identity partition over the first `size` entries, growing capacity geometrically when needed. */
  reset(size: number): void {
    if (size > this.parent.length) {
      const capacity = Math.max(size, this.parent.length * 2);
      this.parent = new Int32Array(capacity);
      this.#rank = new Uint8Array(capacity);
    }
    for (let index = 0; index < size; index += 1) this.parent[index] = index;
    this.#rank.fill(0, 0, size);
  }

  find(value: number): number {
    let root = value;
    while (this.parent[root] !== root) root = this.parent[root];
    while (this.parent[value] !== value) {
      const next = this.parent[value];
      this.parent[value] = root;
      value = next;
    }
    return root;
  }

  union(a: number, b: number): void {
    let rootA = this.find(a);
    let rootB = this.find(b);
    if (rootA === rootB) return;
    if (this.#rank[rootA] < this.#rank[rootB]) [rootA, rootB] = [rootB, rootA];
    this.parent[rootB] = rootA;
    if (this.#rank[rootA] === this.#rank[rootB]) this.#rank[rootA] += 1;
  }
}

class DenseMinHeap {
  readonly #values: number[] = [];

  get length(): number {
    return this.#values.length;
  }

  clear(): void {
    this.#values.length = 0;
  }

  push(value: number): void {
    let index = this.#values.length;
    this.#values.push(value);
    while (index > 0) {
      const parent = (index - 1) >>> 1;
      if (this.#values[parent] <= value) break;
      this.#values[index] = this.#values[parent];
      index = parent;
    }
    this.#values[index] = value;
  }

  pop(): number | undefined {
    const first = this.#values[0];
    const last = this.#values.pop();
    if (last === undefined || this.#values.length === 0) return first;
    let index = 0;
    while (true) {
      const left = index * 2 + 1;
      if (left >= this.#values.length) break;
      const right = left + 1;
      const child = right < this.#values.length && this.#values[right] < this.#values[left]
        ? right
        : left;
      if (this.#values[child] >= last) break;
      this.#values[index] = this.#values[child];
      index = child;
    }
    this.#values[index] = last;
    return first;
  }
}

/**
 * Reusable typed-array battery for `analyzeConstraints`.
 *
 * One arena belongs to one compiled graph and lives on it for the duration of
 * a repair, so tens of thousands of feasibility checks share one set of
 * buffers instead of allocating ~50 arrays each. That sharing is sound
 * because analysis is synchronous, single-threaded, and never re-enters
 * itself on the same graph: `shouldStop` predicates observe clocks and
 * counters only, never start another analysis.
 *
 * Ownership rule: nothing handed out by the arena may escape an
 * `analyzeConstraints` call. The one escaping product — the `includeState`
 * feasible-state snapshot — is copied out at its return sites; conflict
 * results are freshly built plain arrays. Buffers are reset per check with
 * prefix fills or write-before-read discipline; capacity only grows,
 * geometrically, and is kept for later checks.
 */
class AnalysisScratch {
  #nodeCapacity = 0;
  #relationCapacity = 0;
  #equalityCapacity = 0;
  readonly unions = [new DenseUnionFind(0), new DenseUnionFind(0), new DenseUnionFind(0)];
  readonly equalityHead: Int32Array[] = [new Int32Array(0), new Int32Array(0), new Int32Array(0)];
  readonly equalityTo: Int32Array[] = [new Int32Array(0), new Int32Array(0), new Int32Array(0)];
  readonly equalityEdge: Int32Array[] = [new Int32Array(0), new Int32Array(0), new Int32Array(0)];
  readonly equalityNext: Int32Array[] = [new Int32Array(0), new Int32Array(0), new Int32Array(0)];
  readonly equalityLength = new Int32Array(3);
  readonly roots: Int32Array[] = [new Int32Array(0), new Int32Array(0), new Int32Array(0)];
  /** Indexed by relation; cleared lazily by `uniqueConflict`. */
  conflictMarker = new Uint8Array(0);
  pathQueue = new Int32Array(0);
  pathPreviousNode = new Int32Array(0);
  pathPreviousEdge = new Int32Array(0);
  /**
   * One set of per-axis relation-graph buffers serves all three axes in turn:
   * nothing reads an earlier axis's arrays after its iteration ends, because
   * the escaping state snapshot is sliced out at push time.
   */
  axisHead = new Int32Array(0);
  axisTo = new Int32Array(0);
  axisNext = new Int32Array(0);
  axisSourceNode = new Int32Array(0);
  axisTargetNode = new Int32Array(0);
  axisSourceRoot = new Int32Array(0);
  axisTargetRoot = new Int32Array(0);
  axisEdge = new Int32Array(0);
  axisStep = new Int32Array(0);
  color = new Uint8Array(0);
  parentArc = new Int32Array(0);
  readonly nodeStack: number[] = [];
  readonly arcStack: number[] = [];
  readonly triples = new Map<number, number>();

  /**
   * Room for `nodeCount` nodes, `relationCount` relations, and
   * `equalityCount` equalities along any one axis.
   */
  ensure(nodeCount: number, relationCount: number, equalityCount: number): void {
    if (nodeCount > this.#nodeCapacity) {
      const capacity = Math.max(nodeCount, this.#nodeCapacity * 2);
      this.#nodeCapacity = capacity;
      for (let axis = 0; axis < 3; axis += 1) {
        this.equalityHead[axis] = new Int32Array(capacity);
        this.roots[axis] = new Int32Array(capacity);
      }
      this.pathQueue = new Int32Array(capacity);
      this.pathPreviousNode = new Int32Array(capacity);
      this.pathPreviousEdge = new Int32Array(capacity);
      this.axisHead = new Int32Array(capacity);
      this.color = new Uint8Array(capacity);
      this.parentArc = new Int32Array(capacity);
    }
    if (equalityCount > this.#equalityCapacity) {
      const capacity = Math.max(equalityCount, this.#equalityCapacity * 2);
      this.#equalityCapacity = capacity;
      for (let axis = 0; axis < 3; axis += 1) {
        this.equalityTo[axis] = new Int32Array(capacity * 2);
        this.equalityEdge[axis] = new Int32Array(capacity * 2);
        this.equalityNext[axis] = new Int32Array(capacity * 2);
      }
    }
    if (relationCount > this.#relationCapacity) {
      const capacity = Math.max(relationCount, this.#relationCapacity * 2);
      this.#relationCapacity = capacity;
      this.conflictMarker = new Uint8Array(capacity);
      this.axisTo = new Int32Array(capacity);
      this.axisNext = new Int32Array(capacity);
      this.axisSourceNode = new Int32Array(capacity);
      this.axisTargetNode = new Int32Array(capacity);
      this.axisSourceRoot = new Int32Array(capacity);
      this.axisTargetRoot = new Int32Array(capacity);
      this.axisEdge = new Int32Array(capacity);
      this.axisStep = new Int32Array(capacity);
    }
  }
}

function success<T>(value: T): ConstraintResult<T> {
  return { ok: true, value };
}

function failure(reason: ConstraintFailureReason): ConstraintResult<never> {
  return { ok: false, reason };
}

function integralPosition(position: GridPosition): GridPosition {
  return {
    x: Math.round(position.x),
    y: Math.round(position.y),
    level: Math.round(position.level),
  };
}

function protectedVector(edge: LayoutEdge): readonly [number, number, number] | undefined {
  if (edge.constraintVector) {
    return [
      Math.round(edge.constraintVector.x),
      Math.round(edge.constraintVector.y),
      Math.round(edge.constraintVector.level),
    ];
  }
  return ORTHOGONAL_VECTORS[edge.direction];
}

function vectorKey(from: string, to: string, vector: readonly number[]): string {
  return `${from}\u0000${to}\u0000${vector.join(",")}`;
}

/**
 * Strongly connected components of a directed graph whose arcs leave vertex
 * `v` at `targets[head[v]]` up to `targets[head[v + 1]]`: the component of
 * every vertex, numbered from 0. Tarjan's algorithm, with an explicit stack.
 */
function stronglyConnectedComponents(
  vertexCount: number,
  head: Int32Array,
  targets: Int32Array,
): Int32Array {
  const order = new Int32Array(vertexCount).fill(-1);
  const low = new Int32Array(vertexCount);
  const component = new Int32Array(vertexCount).fill(-1);
  const onStack = new Uint8Array(vertexCount);
  const stack: number[] = [];
  const callVertex: number[] = [];
  const callArc: number[] = [];
  let visited = 0;
  let components = 0;
  for (let start = 0; start < vertexCount; start += 1) {
    if (order[start] >= 0) continue;
    order[start] = low[start] = visited++;
    stack.push(start);
    onStack[start] = 1;
    callVertex.push(start);
    callArc.push(head[start]);
    while (callVertex.length > 0) {
      const top = callVertex.length - 1;
      const vertex = callVertex[top];
      const arc = callArc[top];
      if (arc < head[vertex + 1]) {
        callArc[top] = arc + 1;
        const target = targets[arc];
        if (order[target] < 0) {
          order[target] = low[target] = visited++;
          stack.push(target);
          onStack[target] = 1;
          callVertex.push(target);
          callArc.push(head[target]);
        } else if (onStack[target]) {
          low[vertex] = Math.min(low[vertex], order[target]);
        }
        continue;
      }
      callVertex.pop();
      callArc.pop();
      if (callVertex.length > 0) {
        const parent = callVertex[callVertex.length - 1];
        low[parent] = Math.min(low[parent], low[vertex]);
      }
      if (low[vertex] !== order[vertex]) continue;
      for (;;) {
        const member = stack.pop() as number;
        onStack[member] = 0;
        component[member] = components;
        if (member === vertex) break;
      }
      components += 1;
    }
  }
  return component;
}

/**
 * The rooms of the default model's contested level classes, flagged per node.
 * A level class is the rooms that flat relations join, which those relations
 * put on one level. It is contested when the exits or the supplied layout
 * already say otherwise: an Up/Down relation joins two of its rooms, Up/Down
 * relations between classes close a cycle through it, or `levels` put its
 * rooms on more than one level.
 */
function contestedLevelClasses(
  nodeCount: number,
  groupAxes: readonly number[],
  groupFroms: readonly number[],
  groupTos: readonly number[],
  levels: Int32Array,
): Uint8Array {
  const classes = new DenseUnionFind(nodeCount);
  for (let group = 0; group < groupAxes.length; group += 1) {
    if (groupAxes[group] !== 2) classes.union(groupFroms[group], groupTos[group]);
  }
  const root = Int32Array.from({ length: nodeCount }, (_value, node) => classes.find(node));
  const contested = new Uint8Array(nodeCount);
  for (let node = 0; node < nodeCount; node += 1) {
    if (levels[node] !== levels[root[node]]) contested[root[node]] = 1;
  }
  // Up/Down relations between classes, as arcs from the lower class's root
  // to the upper one's, in compressed rows. One that leads a room to itself
  // contradicts itself whatever the levels, so it contests nothing.
  const head = new Int32Array(nodeCount + 1);
  for (let group = 0; group < groupAxes.length; group += 1) {
    if (groupAxes[group] !== 2 || groupFroms[group] === groupTos[group]) continue;
    const lower = root[groupFroms[group]];
    const upper = root[groupTos[group]];
    if (lower === upper) contested[lower] = 1;
    else head[lower + 1] += 1;
  }
  for (let node = 0; node < nodeCount; node += 1) head[node + 1] += head[node];
  const targets = new Int32Array(head[nodeCount]);
  const cursor = head.slice(0, nodeCount);
  for (let group = 0; group < groupAxes.length; group += 1) {
    if (groupAxes[group] !== 2 || groupFroms[group] === groupTos[group]) continue;
    const lower = root[groupFroms[group]];
    const upper = root[groupTos[group]];
    if (lower !== upper) targets[cursor[lower]++] = upper;
  }
  if (targets.length > 0) {
    const component = stronglyConnectedComponents(nodeCount, head, targets);
    const size = new Int32Array(nodeCount);
    for (let node = 0; node < nodeCount; node += 1) size[component[node]] += 1;
    for (let node = 0; node < nodeCount; node += 1) {
      if (size[component[node]] > 1) contested[node] = 1;
    }
  }
  return Uint8Array.from({ length: nodeCount }, (_value, node) => contested[root[node]]);
}

/**
 * Compile the protected rays along one axis. An edge whose vector the graph
 * cannot express (a diagonal, or a constraint vector along several axes or
 * longer than one step) is left out of the graph and only measured.
 *
 * A flat relation also says that its rooms share a level. In a contested
 * level class (see `contestedLevelClasses`) that shared level is a relation of
 * its own, a level relation the search may give up like a ray: giving it up
 * costs what drawing the flat relation's exits wrong costs, and frees its
 * rooms entirely. Everywhere else the shared level is hard. There no Up/Down
 * relation can order two of the class's rooms, so giving it up could only
 * separate rooms by level, and layout never resolves a collision that way.
 */
function compileGraph(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly LayoutEdge[],
): DenseConstraintGraph {
  const ids = [...positions.keys()].sort();
  const indexById = new Map(ids.map((id, index) => [id, index]));
  const densePositions = AXIS_NAMES.map((axisName) =>
    Int32Array.from(ids, (id) => Math.round((positions.get(id) as GridPosition)[axisName]))
  );
  const sourceEdges: LayoutEdge[] = [];
  const sourceIndexes: number[] = [];
  const vectors: (readonly [number, number, number])[] = [];
  for (let sourceIndex = 0; sourceIndex < edges.length; sourceIndex += 1) {
    const edge = edges[sourceIndex];
    if (!indexById.has(edge.from) || !indexById.has(edge.to)) continue;
    const vector = protectedVector(edge);
    if (!vector) continue;
    const nonzero = vector.filter((value) => value !== 0);
    if (nonzero.length !== 1 || Math.abs(nonzero[0]) !== 1) continue;
    sourceEdges.push(edge);
    sourceIndexes.push(sourceIndex);
    vectors.push(vector);
  }

  const reciprocalKeys = new Set<string>();
  for (let edge = 0; edge < sourceEdges.length; edge += 1) {
    reciprocalKeys.add(vectorKey(sourceEdges[edge].from, sourceEdges[edge].to, vectors[edge]));
  }
  const edgeCount = sourceEdges.length;
  const from = new Int32Array(edgeCount);
  const to = new Int32Array(edgeCount);
  const axis = new Int8Array(edgeCount);
  const sign = new Int8Array(edgeCount);
  const step = new Int32Array(edgeCount);
  const reciprocal = new Uint8Array(edgeCount);
  const groupKeys = new Map<string, number>();
  const groupIndexes: number[] = [];
  const groupAxes: number[] = [];
  const groupFroms: number[] = [];
  const groupTos: number[] = [];
  const groupSteps: number[] = [];
  const groupSourceCounts: number[] = [];
  const groupReciprocalCounts: number[] = [];
  for (let edge = 0; edge < edgeCount; edge += 1) {
    const vector = vectors[edge];
    const edgeAxis = vector.findIndex((value) => value !== 0);
    from[edge] = indexById.get(sourceEdges[edge].from) as number;
    to[edge] = indexById.get(sourceEdges[edge].to) as number;
    axis[edge] = edgeAxis;
    sign[edge] = Math.sign(vector[edgeAxis]);
    step[edge] = Math.abs(vector[edgeAxis]);
    const low = sign[edge] > 0 ? from[edge] : to[edge];
    const high = sign[edge] > 0 ? to[edge] : from[edge];
    const groupKey = `${edgeAxis}:${low}:${high}:${step[edge]}`;
    let group = groupKeys.get(groupKey);
    if (group === undefined) {
      group = groupKeys.size;
      groupKeys.set(groupKey, group);
      groupAxes.push(edgeAxis);
      groupFroms.push(low);
      groupTos.push(high);
      groupSteps.push(step[edge]);
      groupSourceCounts.push(0);
      groupReciprocalCounts.push(0);
    }
    groupIndexes.push(group);
    reciprocal[edge] = reciprocalKeys.has(vectorKey(
        sourceEdges[edge].to,
        sourceEdges[edge].from,
        vector.map((value) => -value),
      ))
      ? 1
      : 0;
    groupSourceCounts[group] += 1;
    groupReciprocalCounts[group] += reciprocal[edge];
  }
  const groupCount = groupKeys.size;
  const contested = contestedLevelClasses(
    ids.length,
    groupAxes,
    groupFroms,
    groupTos,
    densePositions[2],
  );
  // Constraint repair may adjust levels only in a component which actually
  // contains a level-crossing relation or a contested level class. Planar-only
  // and isolated components keep the authoritative levels established by the
  // standard planner.
  const topology = new DenseUnionFind(ids.length);
  for (let group = 0; group < groupCount; group += 1) {
    topology.union(groupFroms[group], groupTos[group]);
  }
  const levelCrossingRoots = new Uint8Array(ids.length);
  for (let group = 0; group < groupCount; group += 1) {
    if (groupAxes[group] === 2) levelCrossingRoots[topology.find(groupFroms[group])] = 1;
  }
  for (let node = 0; node < ids.length; node += 1) {
    if (contested[node]) levelCrossingRoots[topology.find(node)] = 1;
  }
  const levelCrossingReachable = Uint8Array.from(
    ids,
    (_id, node) => levelCrossingRoots[topology.find(node)],
  );
  // Levels are topology. A planar relation says that its endpoints share one
  // level: through its level relation in a contested level class, and as a
  // hard equality elsewhere.
  const groupLevelRelation = new Int32Array(groupCount).fill(-1);
  const levelRelationGroups: number[] = [];
  const levelEqualityFrom: number[] = [];
  const levelEqualityTo: number[] = [];
  for (let group = 0; group < groupCount; group += 1) {
    if (groupAxes[group] === 2) continue;
    if (contested[groupFroms[group]]) {
      groupLevelRelation[group] = groupCount + levelRelationGroups.length;
      levelRelationGroups.push(group);
    } else {
      levelEqualityFrom.push(groupFroms[group]);
      levelEqualityTo.push(groupTos[group]);
    }
  }
  // A level relation weighs what its flat group weighs: once its rooms sit
  // on different levels, every exit of the group is drawn wrong.
  const relationCount = groupCount + levelRelationGroups.length;
  const relationGroup = (relation: number): number =>
    relation < groupCount ? relation : levelRelationGroups[relation - groupCount];
  return {
    ids,
    indexById,
    positions: densePositions,
    sourceEdges,
    sourceIndexes: Int32Array.from(sourceIndexes),
    from,
    to,
    axis,
    sign,
    step,
    reciprocal,
    groupOfEdge: Int32Array.from(groupIndexes),
    groupAxis: Int8Array.from(groupAxes),
    groupFrom: Int32Array.from(groupFroms),
    groupTo: Int32Array.from(groupTos),
    groupStep: Int32Array.from(groupSteps),
    groupSourceCount: Int32Array.from(
      { length: relationCount },
      (_value, relation) => groupSourceCounts[relationGroup(relation)],
    ),
    groupReciprocalCount: Int32Array.from(
      { length: relationCount },
      (_value, relation) => groupReciprocalCounts[relationGroup(relation)],
    ),
    groupLevelCount: Int32Array.from(
      { length: relationCount },
      (_value, relation) => relation < groupCount ? 0 : 1,
    ),
    groupLevelRelation,
    levelRelationGroup: Int32Array.from(levelRelationGroups),
    levelCrossingReachable,
    levelEqualityFrom: Int32Array.from(levelEqualityFrom),
    levelEqualityTo: Int32Array.from(levelEqualityTo),
    groupCount,
    relationCount,
    nodeCount: ids.length,
    edgeCount,
  };
}

/**
 * Whether `mask` gives up directional group `group`: by relaxing the group
 * itself, or its level relation, without which none of its rays holds.
 */
function groupRelaxed(graph: DenseConstraintGraph, mask: Uint8Array, group: number): boolean {
  if (mask[group]) return true;
  const level = graph.groupLevelRelation[group];
  return level >= 0 && !!mask[level];
}

/**
 * `mask` without directional groups whose level relation it also gives up:
 * such a group is given up with its level relation and costs nothing more.
 * Every mask the search weighs has this form, so its score counts each exit
 * drawn wrong once.
 */
function canonicalRelationMask(graph: DenseConstraintGraph, mask: Uint8Array): Uint8Array {
  for (let relation = graph.groupCount; relation < graph.relationCount; relation += 1) {
    if (mask[relation]) mask[graph.levelRelationGroup[relation - graph.groupCount]] = 0;
  }
  return mask;
}

function analyzeConstraints(
  graph: DenseConstraintGraph,
  removedGroups: Uint8Array,
  includeState = false,
  shouldStop?: () => boolean,
): ConstraintResult<AnalysisResult> {
  const { nodeCount, groupCount } = graph;
  const scratch = graph.scratch ??= new AnalysisScratch();
  scratch.ensure(
    nodeCount,
    graph.relationCount,
    Math.max(groupCount, graph.levelEqualityFrom.length + graph.relationCount - groupCount),
  );
  let work = 0;
  const interrupted = (): boolean => {
    work += 1;
    return (work & 0x3ff) === 0 && shouldStop?.() === true;
  };
  const unions = scratch.unions;
  const equalityHead = scratch.equalityHead;
  for (let axis = 0; axis < 3; axis += 1) {
    unions[axis].reset(nodeCount);
    equalityHead[axis].fill(-1, 0, nodeCount);
  }
  const equalityTo = scratch.equalityTo;
  const equalityEdge = scratch.equalityEdge;
  const equalityNext = scratch.equalityNext;
  const equalityLength = scratch.equalityLength;
  equalityLength.fill(0);
  const addEquality = (axis: number, from: number, to: number, edge: number): void => {
    unions[axis].union(from, to);
    let cursor = equalityLength[axis]++;
    equalityTo[axis][cursor] = to;
    equalityEdge[axis][cursor] = edge;
    equalityNext[axis][cursor] = equalityHead[axis][from];
    equalityHead[axis][from] = cursor;
    cursor = equalityLength[axis]++;
    equalityTo[axis][cursor] = from;
    equalityEdge[axis][cursor] = edge;
    equalityNext[axis][cursor] = equalityHead[axis][to];
    equalityHead[axis][to] = cursor;
  };

  // Rooms that share a level (see `levelEqualityFrom`) share it under every
  // mask. Relaxing a directional x/y ray must not turn z into another
  // collision-avoidance axis. These hard equalities deliberately have no
  // removable relation in a conflict explanation; only the still-active
  // relations around them can be selected by the hitting-set master.
  for (let pair = 0; pair < graph.levelEqualityFrom.length; pair += 1) {
    if (interrupted()) return failure("time");
    addEquality(2, graph.levelEqualityFrom[pair], graph.levelEqualityTo[pair], HARD_LEVEL_EQUALITY);
  }
  // A level relation keeps its flat group's rooms on one level until the mask
  // gives it up.
  for (let relation = groupCount; relation < graph.relationCount; relation += 1) {
    if (interrupted()) return failure("time");
    if (removedGroups[relation]) continue;
    const group = graph.levelRelationGroup[relation - groupCount];
    addEquality(2, graph.groupFrom[group], graph.groupTo[group], relation);
  }
  // Search and analysis otherwise operate only on canonical relation groups.
  // Source-edge multiplicity survives exclusively as the exact objective weight.
  for (let group = 0; group < graph.groupCount; group += 1) {
    if (interrupted()) return failure("time");
    if (groupRelaxed(graph, removedGroups, group)) continue;
    for (let axis = 0; axis < 3; axis += 1) {
      if (graph.groupAxis[group] === axis) continue;
      if (axis === 2 && graph.groupAxis[group] !== 2) continue;
      addEquality(axis, graph.groupFrom[group], graph.groupTo[group], group);
    }
  }

  const roots = scratch.roots;
  for (let axis = 0; axis < 3; axis += 1) {
    for (let node = 0; node < nodeCount; node += 1) {
      if (interrupted()) return failure("time");
      roots[axis][node] = unions[axis].find(node);
    }
  }
  const marker = scratch.conflictMarker;
  const uniqueConflict = (edges: readonly number[]): number[] => {
    // Conflict members are relations, and at most one conflict is assembled
    // per check, so the marker is cleared lazily here.
    marker.fill(0, 0, graph.relationCount);
    const result: number[] = [];
    const add = (relation: number): void => {
      if (marker[relation]) return;
      marker[relation] = 1;
      result.push(relation);
    };
    for (const edge of edges) {
      add(edge);
      // A directional group's rays hold only while its level relation does,
      // so giving up either one breaks what the group contributes.
      if (edge < groupCount && graph.groupLevelRelation[edge] >= 0) {
        add(graph.groupLevelRelation[edge]);
      }
    }
    return result;
  };
  const equalityPath = (
    axis: number,
    start: number,
    finish: number,
  ): ConstraintResult<number[]> => {
    if (start === finish) return success([]);
    const queue = scratch.pathQueue;
    const previousNode = scratch.pathPreviousNode;
    // `queue` and `previousEdge` follow write-before-read discipline: entries
    // are read only for nodes discovered in this traversal.
    const previousEdge = scratch.pathPreviousEdge;
    previousNode.fill(-2, 0, nodeCount);
    let read = 0;
    let write = 0;
    queue[write++] = start;
    previousNode[start] = -1;
    while (read < write) {
      if (interrupted()) return failure("time");
      const node = queue[read++];
      for (let cursor = equalityHead[axis][node]; cursor !== -1; cursor = equalityNext[axis][cursor]) {
        const next = equalityTo[axis][cursor];
        if (previousNode[next] !== -2) continue;
        previousNode[next] = node;
        previousEdge[next] = equalityEdge[axis][cursor];
        if (next === finish) {
          const result: number[] = [];
          let current = finish;
          while (current !== start) {
            if (previousEdge[current] !== HARD_LEVEL_EQUALITY) {
              result.push(previousEdge[current]);
            }
            current = previousNode[current];
          }
          return success(result);
        }
        queue[write++] = next;
      }
    }
    return failure("analysis");
  };

  const axisGraphs: AxisGraph[] = [];
  for (let axis = 0; axis < 3; axis += 1) {
    const head = scratch.axisHead;
    const to = scratch.axisTo;
    const next = scratch.axisNext;
    const sourceNode = scratch.axisSourceNode;
    const targetNode = scratch.axisTargetNode;
    const sourceRoot = scratch.axisSourceRoot;
    const targetRoot = scratch.axisTargetRoot;
    const sourceEdge = scratch.axisEdge;
    const step = scratch.axisStep;
    head.fill(-1, 0, nodeCount);
    let length = 0;
    for (let group = 0; group < graph.groupCount; group += 1) {
      if (interrupted()) return failure("time");
      if (graph.groupAxis[group] !== axis || groupRelaxed(graph, removedGroups, group)) continue;
      const lowNode = graph.groupFrom[group];
      const highNode = graph.groupTo[group];
      const lowRoot = roots[axis][lowNode];
      const highRoot = roots[axis][highNode];
      if (lowRoot === highRoot) {
        const path = equalityPath(axis, lowNode, highNode);
        if (!path.ok) return path;
        return success({ conflict: uniqueConflict([group, ...path.value]) });
      }
      to[length] = highRoot;
      sourceNode[length] = lowNode;
      targetNode[length] = highNode;
      sourceRoot[length] = lowRoot;
      targetRoot[length] = highRoot;
      sourceEdge[length] = group;
      step[length] = graph.groupStep[group];
      next[length] = head[lowRoot];
      head[lowRoot] = length++;
    }

    const color = scratch.color;
    const parentArc = scratch.parentArc;
    color.fill(0, 0, nodeCount);
    parentArc.fill(-1, 0, nodeCount);
    const nodeStack = scratch.nodeStack;
    const arcStack = scratch.arcStack;
    let cycle: number[] | undefined;
    for (let root = 0; root < nodeCount && !cycle; root += 1) {
      if (roots[axis][root] !== root || color[root] !== 0) continue;
      nodeStack.length = 0;
      arcStack.length = 0;
      nodeStack.push(root);
      arcStack.push(head[root]);
      color[root] = 1;
      while (nodeStack.length > 0 && !cycle) {
        if (interrupted()) return failure("time");
        const depth = nodeStack.length - 1;
        const node = nodeStack[depth];
        const arc = arcStack[depth];
        if (arc === -1) {
          color[node] = 2;
          nodeStack.pop();
          arcStack.pop();
          continue;
        }
        arcStack[depth] = next[arc];
        const target = to[arc];
        if (color[target] === 0) {
          parentArc[target] = arc;
          color[target] = 1;
          nodeStack.push(target);
          arcStack.push(head[target]);
        } else if (color[target] === 1) {
          const path: number[] = [];
          let cursor = node;
          while (cursor !== target) {
            const parent = parentArc[cursor];
            if (parent === -1) return failure("analysis");
            path.push(parent);
            cursor = sourceRoot[parent];
          }
          path.reverse();
          path.push(arc);
          cycle = path;
        }
      }
    }
    if (cycle) {
      const conflict = cycle.map((arc) => sourceEdge[arc]);
      for (let index = 0; index < cycle.length; index += 1) {
        const before = cycle[index];
        const after = cycle[(index + 1) % cycle.length];
        const path = equalityPath(axis, targetNode[before], sourceNode[after]);
        if (!path.ok) return path;
        conflict.push(...path.value);
      }
      return success({ conflict: uniqueConflict(conflict) });
    }
    // Escape point: the feasible-state snapshot outlives this check (it seeds
    // an entire extension search), so it is copied out of the arena here. The
    // copies match the historical exact allocation sizes.
    if (includeState) {
      axisGraphs.push({
        head: head.slice(0, nodeCount),
        to: to.slice(0, groupCount),
        next: next.slice(0, groupCount),
        sourceNode: sourceNode.slice(0, groupCount),
        targetNode: targetNode.slice(0, groupCount),
        sourceRoot: sourceRoot.slice(0, groupCount),
        targetRoot: targetRoot.slice(0, groupCount),
        edge: sourceEdge.slice(0, groupCount),
        step: step.slice(0, groupCount),
        length,
      });
    }
  }

  const triples = scratch.triples;
  triples.clear();
  for (let node = 0; node < nodeCount; node += 1) {
    if (interrupted()) return failure("time");
    const key = roots[0][node] + nodeCount * (roots[1][node] + nodeCount * roots[2][node]);
    const previous = triples.get(key);
    if (previous !== undefined) {
      const conflict: number[] = [];
      for (let axis = 0; axis < 3; axis += 1) {
        const path = equalityPath(axis, previous, node);
        if (!path.ok) return path;
        conflict.push(...path.value);
      }
      return success({ conflict: uniqueConflict(conflict) });
    }
    triples.set(key, node);
  }
  if (!includeState) return success({});
  // Escape point: the retained state snapshot must survive later checks, so
  // the roots leave the arena as exact-size copies alongside the axis graphs.
  return success({
    state: {
      roots: [
        roots[0].slice(0, nodeCount),
        roots[1].slice(0, nodeCount),
        roots[2].slice(0, nodeCount),
      ],
      graphs: axisGraphs,
    },
  });
}

/**
 * Collect pairwise-disjoint conflict cores by repeatedly removing every group
 * of each discovered conflict and re-checking. Each core is a certificate that
 * at least one of its groups must be removed by any feasible mask, so the sum
 * of per-core minimum weights is a valid objective lower bound — and the cores
 * themselves seed the exact hitting-set strategy, which subsumes this bound.
 */
function collectDisjointConflictCores(
  graph: DenseConstraintGraph,
  check: (removed: Uint8Array, includeState?: boolean) => ConstraintResult<AnalysisResult>,
): ConstraintResult<{ cores: number[][]; lowerBound: number; complete: boolean }> {
  const excluded = new Uint8Array(graph.relationCount);
  const cores: number[][] = [];
  let lowerBound = 0;
  for (;;) {
    const checked = check(excluded);
    if (!checked.ok) {
      if (checked.reason === "time" || checked.reason === "work") {
        return success({ cores, lowerBound, complete: false });
      }
      return checked;
    }
    const conflict = checked.value.conflict;
    if (!conflict) return success({ cores, lowerBound, complete: true });
    if (conflict.length === 0) return failure("analysis");
    let minimumWeight = Number.POSITIVE_INFINITY;
    let changed = false;
    for (const group of conflict) {
      minimumWeight = Math.min(minimumWeight, graph.groupSourceCount[group]);
      if (!excluded[group]) changed = true;
      excluded[group] = 1;
    }
    if (!Number.isFinite(minimumWeight)) return failure("analysis");
    cores.push(conflict);
    lowerBound += minimumWeight;
    if (!changed) return failure("analysis");
  }
}

/**
 * The master objective of a relation mask, compared lexicographically: the
 * directed source edges it gives up, those of them with a reciprocal, and the
 * level relations it gives up.
 */
type RelaxationScore = readonly [number, number, number];

interface HittingSetSolution {
  /** Exact only when true; false means the node budget cut the solve. */
  complete: boolean;
  /** Present exactly when a hitting set strictly better than the incumbent exists. */
  mask?: Uint8Array;
  score?: RelaxationScore;
}

/** The relation weights an exact hitting-set solve reads. */
type HittingSetWeights = Pick<
  DenseConstraintGraph,
  "relationCount" | "groupSourceCount" | "groupReciprocalCount" | "groupLevelCount"
>;

/**
 * The exact solve the implicit hitting-set loop runs each iteration. Cores
 * that share no relation group form independent components: a set hits every
 * core exactly when its groups in each component hit that component's cores,
 * so the lexicographic minimum is the union of the components' minima, and
 * independent clusters cost the sum of their searches, not the product. Each
 * component is solved with the incumbent restricted to its groups as the
 * score to beat. The incumbent is feasible and every feasible mask hits every
 * core, so the restriction hits every core of the component, and the
 * component's minimum is the strictly better set the solve returns or else
 * the restriction itself. The summed minimum can beat the incumbent even when
 * no component improves, since the incumbent may relax groups in no core.
 * Components are solved in order of their smallest group and draw on one node
 * budget; an exhausted budget leaves the whole solve incomplete.
 */
function solveMinimumHittingSetByComponent(
  graph: HittingSetWeights,
  cores: readonly (readonly number[])[],
  incumbentMask: Uint8Array,
  incumbent: RelaxationScore,
  budget: { nodes: number },
): HittingSetSolution {
  const components = new DenseUnionFind(graph.relationCount);
  const present = new Uint8Array(graph.relationCount);
  for (const core of cores) {
    for (const group of core) {
      present[group] = 1;
      components.union(core[0], group);
    }
  }
  // Components are numbered by their smallest group, so their order depends
  // only on the cores.
  const componentOfRoot = new Int32Array(graph.relationCount).fill(-1);
  const componentGroups: number[][] = [];
  for (let group = 0; group < graph.relationCount; group += 1) {
    if (!present[group]) continue;
    const root = components.find(group);
    if (componentOfRoot[root] < 0) {
      componentOfRoot[root] = componentGroups.length;
      componentGroups.push([]);
    }
    componentGroups[componentOfRoot[root]].push(group);
  }
  if (componentGroups.length <= 1) return solveMinimumHittingSet(graph, cores, incumbent, budget);
  const componentCores = componentGroups.map((): (readonly number[])[] => []);
  for (const core of cores) componentCores[componentOfRoot[components.find(core[0])]].push(core);
  const mask = new Uint8Array(graph.relationCount);
  let primary = 0;
  let secondary = 0;
  let levels = 0;
  for (let component = 0; component < componentGroups.length; component += 1) {
    const groups = componentGroups[component];
    const restricted: [number, number, number] = [0, 0, 0];
    for (const group of groups) {
      if (!incumbentMask[group]) continue;
      restricted[0] += graph.groupSourceCount[group];
      restricted[1] += graph.groupReciprocalCount[group];
      restricted[2] += graph.groupLevelCount[group];
    }
    const solved = solveMinimumHittingSet(graph, componentCores[component], restricted, budget);
    if (!solved.complete) return { complete: false };
    const chosen = solved.mask ?? incumbentMask;
    for (const group of groups) {
      if (chosen[group]) mask[group] = 1;
    }
    const score = solved.score ?? restricted;
    primary += score[0];
    secondary += score[1];
    levels += score[2];
  }
  const score: RelaxationScore = [primary, secondary, levels];
  if (!betterScore(score, incumbent)) return { complete: true };
  return { complete: true, mask, score };
}

/**
 * The cores without every relation that another relation dominates. Relation
 * `a` is dominated by `b` when every core containing `a` also contains `b`,
 * and `b` weighs no more than `a` in any of the objective's three weights;
 * of equivalent relations, which lie in the same cores with every weight
 * equal, the lowest index survives. Swapping a dominated relation for a
 * dominator keeps a hitting set hitting every core and never worsens any
 * weight. Dominance is transitive and acyclic, so every dominated relation has
 * a surviving dominator: every reduced core keeps a relation, and the minimum
 * over the reduced cores is the minimum over the originals. Wide cores whose
 * relations each lie in that one core collapse to their cheapest relation.
 */
function removeDominatedGroups(
  graph: HittingSetWeights,
  cores: readonly (readonly number[])[],
): readonly (readonly number[])[] {
  // Dense slots for the groups the cores mention, each with the bitset of
  // cores containing it.
  const slotOfGroup = new Int32Array(graph.relationCount).fill(-1);
  const groups: number[] = [];
  for (const core of cores) {
    for (const group of core) {
      if (slotOfGroup[group] >= 0) continue;
      slotOfGroup[group] = groups.length;
      groups.push(group);
    }
  }
  const words = Math.ceil(cores.length / 32);
  const membership = new Uint32Array(groups.length * words);
  const coreCount = new Int32Array(groups.length);
  const firstCore = new Int32Array(groups.length).fill(-1);
  for (let index = 0; index < cores.length; index += 1) {
    const word = index >>> 5;
    const bit = 1 << (index & 31);
    for (const group of cores[index]) {
      const slot = slotOfGroup[group];
      if (membership[slot * words + word] & bit) continue;
      membership[slot * words + word] |= bit;
      coreCount[slot] += 1;
      if (firstCore[slot] < 0) firstCore[slot] = index;
    }
  }
  const dominated = new Uint8Array(groups.length);
  let reduced = false;
  for (let a = 0; a < groups.length; a += 1) {
    const groupA = groups[a];
    const weightA = graph.groupSourceCount[groupA];
    const reciprocalA = graph.groupReciprocalCount[groupA];
    const levelA = graph.groupLevelCount[groupA];
    // Every dominator of `a` lies in each core containing `a`, so the first
    // such core holds them all. A candidate already found dominated is
    // skipped: its surviving dominator also dominates `a` and lies here too.
    for (const groupB of cores[firstCore[a]]) {
      if (groupB === groupA) continue;
      const b = slotOfGroup[groupB];
      if (dominated[b] || coreCount[b] < coreCount[a]) continue;
      const weightB = graph.groupSourceCount[groupB];
      const reciprocalB = graph.groupReciprocalCount[groupB];
      const levelB = graph.groupLevelCount[groupB];
      if (weightB > weightA || reciprocalB > reciprocalA || levelB > levelA) continue;
      // With equal counts a superset is the same set of cores.
      if (weightB === weightA && reciprocalB === reciprocalA && levelB === levelA &&
        coreCount[b] === coreCount[a] && groupB > groupA) continue;
      let superset = true;
      for (let word = 0; word < words; word += 1) {
        if ((membership[a * words + word] & ~membership[b * words + word]) !== 0) {
          superset = false;
          break;
        }
      }
      if (!superset) continue;
      dominated[a] = 1;
      reduced = true;
      break;
    }
  }
  if (!reduced) return cores;
  return cores.map((core) => core.filter((group) => !dominated[slotOfGroup[group]]));
}

/**
 * Exact minimum-weight hitting set over conflict cores, via depth-first
 * branch and bound under a deterministic shared node budget. Weights are the
 * master objective — primary directed source edges, secondary reciprocal
 * source edges, then level relations, compared lexicographically (see
 * `RelaxationScore`). The incumbent to beat is the
 * score of a hitting set of every core: the best known feasible mask, or its
 * restriction to one component of the cores (a mask hitting no group of a
 * core keeps that core's contradiction intact). The solve therefore ends in
 * one of two proofs: a strictly better hitting set whose score is the exact
 * minimum over the cores, or the certificate that no hitting set beats the
 * incumbent — which, because every feasible mask is a hitting set, certifies
 * the incumbent as weight-optimal, reciprocal-positive optima included. The
 * search branches only over groups no other group dominates, which leaves
 * that minimum unchanged.
 */
function solveMinimumHittingSet(
  graph: HittingSetWeights,
  allCores: readonly (readonly number[])[],
  incumbent: RelaxationScore,
  budget: { nodes: number },
): HittingSetSolution {
  const cores = removeDominatedGroups(graph, allCores);
  const chosen = new Uint8Array(graph.relationCount);
  const banned = new Uint8Array(graph.relationCount);
  const marker = new Uint8Array(graph.relationCount);
  let bestScore: RelaxationScore | undefined;
  let bestMask: Uint8Array | undefined;
  let exhausted = false;

  // Admissible primary-weight bound on completing the current partial set:
  // greedily count still-unhit cores whose selectable groups are pairwise
  // disjoint, each contributing its cheapest selectable group. A core with no
  // selectable group at all is unhittable in this subtree.
  const remainingBound = (): number | undefined => {
    let bound = 0;
    marker.fill(0);
    for (const core of cores) {
      let hit = false;
      let overlaps = false;
      let minimum = Number.POSITIVE_INFINITY;
      for (const group of core) {
        if (chosen[group]) {
          hit = true;
          break;
        }
        if (banned[group]) continue;
        if (marker[group]) overlaps = true;
        minimum = Math.min(minimum, graph.groupSourceCount[group]);
      }
      if (hit) continue;
      if (!Number.isFinite(minimum)) return undefined;
      if (overlaps) continue;
      bound += minimum;
      for (const group of core) {
        if (!banned[group]) marker[group] = 1;
      }
    }
    return bound;
  };

  const descend = (primary: number, secondary: number, levels: number): void => {
    if (exhausted) return;
    if (budget.nodes <= 0) {
      exhausted = true;
      return;
    }
    budget.nodes -= 1;
    const bound = remainingBound();
    if (bound === undefined) return;
    const target = bestScore ?? incumbent;
    if (primary + bound > target[0] ||
      (primary + bound === target[0] &&
        (secondary > target[1] || secondary === target[1] && levels >= target[2]))) return;
    let branchCore: readonly number[] | undefined;
    for (const core of cores) {
      let hit = false;
      for (const group of core) {
        if (chosen[group]) {
          hit = true;
          break;
        }
      }
      if (!hit) {
        branchCore = core;
        break;
      }
    }
    if (!branchCore) {
      // Every core is hit, and the entry prune already guaranteed strict
      // lexicographic improvement over the running target.
      bestScore = [primary, secondary, levels];
      bestMask = chosen.slice();
      return;
    }
    // Standard symmetry breaking: after the branch containing a group is
    // exhausted, later branches of this core exclude it, so no group subset is
    // enumerated twice.
    const bannedHere: number[] = [];
    for (const group of branchCore) {
      if (banned[group]) continue;
      chosen[group] = 1;
      descend(
        primary + graph.groupSourceCount[group],
        secondary + graph.groupReciprocalCount[group],
        levels + graph.groupLevelCount[group],
      );
      chosen[group] = 0;
      if (exhausted) break;
      // Every completion below this node scores at least [primary + bound,
      // secondary, levels]; once the running best sits exactly on that floor,
      // no sibling branch can strictly beat it. On wide cores this collapses
      // the whole fan to its first satisfying branch.
      if (bestScore && bestScore[0] === primary + bound && bestScore[1] === secondary &&
        bestScore[2] === levels) break;
      banned[group] = 1;
      bannedHere.push(group);
    }
    for (const group of bannedHere) banned[group] = 0;
  };
  descend(0, 0, 0);
  if (exhausted) return { complete: false };
  if (!bestMask || !bestScore) return { complete: true };
  return { complete: true, mask: bestMask, score: bestScore };
}

function randomGenerator(seed: number): () => number {
  let state = seed >>> 0;
  return () => {
    state ^= state << 13;
    state ^= state >>> 17;
    state ^= state << 5;
    return (state >>> 0) / 0x1_0000_0000;
  };
}

function removedScore(graph: HittingSetWeights, removedGroups: Uint8Array): RelaxationScore {
  let total = 0;
  let reciprocal = 0;
  let levels = 0;
  for (let group = 0; group < graph.relationCount; group += 1) {
    if (!removedGroups[group]) continue;
    total += graph.groupSourceCount[group];
    reciprocal += graph.groupReciprocalCount[group];
    levels += graph.groupLevelCount[group];
  }
  return [total, reciprocal, levels];
}

function constraintMaskHash(mask: Uint8Array): number {
  let hash = 0x811c9dc5;
  for (let group = 0; group < mask.length; group += 1) {
    if (mask[group]) hash = Math.imul(hash ^ group, 0x01000193) >>> 0;
  }
  return hash;
}

function sameConstraintMask(left: Uint8Array, right: Uint8Array): boolean {
  if (left.length !== right.length) return false;
  for (let group = 0; group < left.length; group += 1) {
    if (left[group] !== right[group]) return false;
  }
  return true;
}

/** Per compiled edge: 1 when the mask gives up its group, directly or with its level relation. */
function expandGroupMask(graph: DenseConstraintGraph, removedGroups: Uint8Array): Uint8Array {
  return Uint8Array.from(graph.groupOfEdge, (group) => groupRelaxed(graph, removedGroups, group) ? 1 : 0);
}

/** Per compiled edge: 1 when the mask gives up the level relation of its group. */
function expandLevelRelationMask(graph: DenseConstraintGraph, removedGroups: Uint8Array): Uint8Array {
  return Uint8Array.from(graph.groupOfEdge, (group) => {
    const level = graph.groupLevelRelation[group];
    return level >= 0 && removedGroups[level] ? 1 : 0;
  });
}

/**
 * The relation mask giving up every group all of whose compiled edges
 * `removedSources` marks, and the level relation of every group one of whose
 * compiled edges `relaxedLevelSources` marks.
 */
function sourceMaskToGroupMask(
  graph: DenseConstraintGraph,
  removedSources: Uint8Array,
  relaxedLevelSources?: Uint8Array,
): Uint8Array {
  const removedCounts = new Int32Array(graph.groupCount);
  for (let edge = 0; edge < graph.edgeCount; edge += 1) {
    if (removedSources[edge]) removedCounts[graph.groupOfEdge[edge]] += 1;
  }
  const mask = new Uint8Array(graph.relationCount);
  for (let group = 0; group < graph.groupCount; group += 1) {
    if (removedCounts[group] === graph.groupSourceCount[group]) mask[group] = 1;
  }
  for (let edge = 0; edge < graph.edgeCount && relaxedLevelSources; edge += 1) {
    const level = graph.groupLevelRelation[graph.groupOfEdge[edge]];
    if (relaxedLevelSources[edge] && level >= 0) mask[level] = 1;
  }
  return mask;
}

/**
 * The source indexes of the directional groups among `relations`, and
 * separately those of the groups whose level relation is among them.
 */
function sourceIndexesForGroups(
  graph: DenseConstraintGraph,
  relations: readonly number[],
): { directional: number[]; level: number[] } {
  const selected = new Uint8Array(graph.relationCount);
  for (const relation of relations) selected[relation] = 1;
  const directional: number[] = [];
  const level: number[] = [];
  for (let edge = 0; edge < graph.edgeCount; edge += 1) {
    const group = graph.groupOfEdge[edge];
    if (selected[group]) directional.push(graph.sourceIndexes[edge]);
    const levelRelation = graph.groupLevelRelation[group];
    if (levelRelation >= 0 && selected[levelRelation]) level.push(graph.sourceIndexes[edge]);
  }
  return { directional, level };
}

/**
 * Infer the least canonical relaxation mask which admits the supplied rays.
 * Grouping makes reciprocal/duplicate source edges atomic, so one violated
 * member relaxes the complete feasibility-equivalent relation. A group with a
 * level relation whose rooms lie on different levels gives up that level
 * relation instead, which gives up the group with it.
 *
 * Each ray is read from the compiled unit vectors, as `directionalViolationEdges`
 * measures it: every compiled edge is one step along one axis, so it is on
 * its ray exactly when it advances a whole number of cells, at least one, in
 * its vector's direction and does not move along the other axes.
 */
function removedGroupsForPositions(
  graph: DenseConstraintGraph,
  positions: ReadonlyMap<string, GridPosition>,
): Uint8Array {
  const removed = new Uint8Array(graph.relationCount);
  for (let edge = 0; edge < graph.edgeCount; edge += 1) {
    const group = graph.groupOfEdge[edge];
    const level = graph.groupLevelRelation[group];
    if (removed[group] || (level >= 0 && removed[level])) continue;
    const from = positions.get(graph.ids[graph.from[edge]]);
    const to = positions.get(graph.ids[graph.to[edge]]);
    if (!from || !to) continue;
    if (level >= 0 && from.level !== to.level) {
      removed[level] = 1;
      continue;
    }
    const dx = to.x - from.x;
    const dy = to.y - from.y;
    const dl = to.level - from.level;
    const axis = graph.axis[edge];
    const along = (axis === 0 ? dx : axis === 1 ? dy : dl) * graph.sign[edge];
    const onRay = along >= 1 && Number.isInteger(along) &&
      (axis === 0 || dx === 0) && (axis === 1 || dy === 0) &&
      (axis === 2 || dl === 0);
    if (!onRay) removed[group] = 1;
  }
  return removed;
}

/**
 * `removedGroupsForPositions` stated through `directionalViolationEdges`. The
 * oracle its compiled reading is tested against; admission never calls it.
 */
function referenceRemovedGroupsForPositions(
  graph: DenseConstraintGraph,
  positions: ReadonlyMap<string, GridPosition>,
): Uint8Array {
  const violated = new Set(directionalViolationEdges(positions, graph.sourceEdges));
  const removed = new Uint8Array(graph.relationCount);
  for (let edge = 0; edge < graph.edgeCount; edge += 1) {
    if (violated.has(graph.sourceEdges[edge])) removed[graph.groupOfEdge[edge]] = 1;
  }
  for (let relation = graph.groupCount; relation < graph.relationCount; relation += 1) {
    const group = graph.levelRelationGroup[relation - graph.groupCount];
    const low = positions.get(graph.ids[graph.groupFrom[group]]);
    const high = positions.get(graph.ids[graph.groupTo[group]]);
    if (low && high && low.level !== high.level) removed[relation] = 1;
  }
  return canonicalRelationMask(graph, removed);
}

function betterScore(a: RelaxationScore, b: RelaxationScore | undefined): boolean {
  return !b || compareRelaxationScores(a, b) < 0;
}

/** Negative when `a` is the better (smaller) relaxation score, as a sort comparator. */
function compareRelaxationScores(a: RelaxationScore, b: RelaxationScore): number {
  return a[0] - b[0] || a[1] - b[1] || a[2] - b[2];
}

function emptyConstraintWorkStats(): ConstraintRepairWorkStats {
  return {
    rawIncumbents: 0,
    softIncumbents: 0,
    distinctLayouts: 0,
    maskDiversifications: 0,
    separatorStates: 0,
    separatorBranches: 0,
    separatorCyclePrunes: 0,
    peakLiveSearchNodes: 0,
    candidateMaterializations: 0,
  };
}

function constraintSearch(
  graph: DenseConstraintGraph,
  options: ConstraintRepairOptions,
  standardPositions: ReadonlyMap<string, GridPosition>,
  runtime: SearchRuntimeOptions = {},
): ConstraintResult<ConstraintSearchResult> {
  const now = runtime.now ?? (() => performance.now());
  const started = now();
  const requestedDuration = options.maxDurationMs ?? DEFAULT_MAX_DURATION_MS;
  const duration = Number.isFinite(requestedDuration)
    ? Math.max(0, requestedDuration)
    : requestedDuration === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : 0;
  const deadline = started + duration;
  const requestedRestarts = Math.floor(options.maxRestarts ?? DEFAULT_MAX_RESTARTS);
  const maximumRestarts = Number.isFinite(requestedRestarts)
    ? Math.max(1, requestedRestarts)
    : requestedRestarts === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : DEFAULT_MAX_RESTARTS;
  const requestedMasks = Math.floor(options.maxMaskDiversifications ?? DEFAULT_MAX_MASK_DIVERSIFICATIONS);
  const maximumMasks = Number.isFinite(requestedMasks)
    ? Math.max(1, requestedMasks)
    : requestedMasks === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : DEFAULT_MAX_MASK_DIVERSIFICATIONS;
  // The initial mask gives up every relation the standard plan breaks, level
  // relations included, so the plan itself shows that it is feasible.
  const initial = removedGroupsForPositions(graph, standardPositions);
  const initialScore = removedScore(graph, initial);
  const initialRemovedGroups = initial.reduce((total, value) => total + (value ? 1 : 0), 0);
  const maximumPerRestart = 2 * Math.min(graph.relationCount, initialRemovedGroups + 1) + 3;
  // Restoring a level relation takes up to two checks, one for its level and
  // one for its rays.
  const defaultMaximumChecks = Math.min(
    Number.MAX_SAFE_INTEGER,
    1 + graph.relationCount + 1 + initialRemovedGroups + initialScore[2] +
      maximumRestarts * maximumPerRestart,
  );
  const maximumFeasibilityChecks = runtime.maximumFeasibilityChecks === undefined
    ? defaultMaximumChecks
    : Math.max(1, Math.floor(runtime.maximumFeasibilityChecks));
  let feasibilityChecks = 0;
  let attemptedRestarts = 0;
  let lastProgressAt = started;
  let budgetCutoff: ConstraintRepairReport["cutoff"] | undefined;
  let stopRequested = false;
  const publishMask = (removed: Uint8Array): void => {
    if (runtime.mask?.(removed.slice(), {
      restarts: attemptedRestarts,
      feasibilityChecks,
      elapsedMs: Math.max(0, now() - started),
    }) === false) stopRequested = true;
  };
  const timeExpired = (): boolean => {
    if (now() < deadline) return false;
    budgetCutoff = "time";
    return true;
  };
  const check = (
    removed: Uint8Array,
    includeState = false,
  ): ConstraintResult<AnalysisResult> => {
    if (timeExpired()) return failure("time");
    if (feasibilityChecks >= maximumFeasibilityChecks) {
      budgetCutoff ??= "restarts";
      return failure("work");
    }
    feasibilityChecks += 1;
    if ((feasibilityChecks & SEARCH_PROGRESS_CHECK_MASK) === 0) {
      const progressAt = now();
      if (progressAt - lastProgressAt >= PROGRESS_INTERVAL_MS) {
        lastProgressAt = progressAt;
        runtime.progress?.({
          ...emptyConstraintWorkStats(),
          phase: "search",
          restarts: attemptedRestarts,
          feasibilityChecks,
          layoutsConsidered: 0,
          compactionAttempts: 0,
          elapsedMs: Math.max(0, progressAt - started),
        });
      }
    }
    const analyzed = analyzeConstraints(graph, removed, includeState, timeExpired);
    if (!analyzed.ok && analyzed.reason === "time") budgetCutoff = "time";
    return analyzed;
  };

  const initialCheck = check(initial);
  if (!initialCheck.ok) return initialCheck;
  if (initialCheck.value.conflict) return failure("analysis");
  publishMask(initial);
  let best = initial.slice();
  let bestScore = initialScore;
  interface RetainedMask {
    hash: number;
    removed: Uint8Array;
    score: RelaxationScore;
    ordinal: number;
  }
  const retainedMasks: RetainedMask[] = [];
  const retainedMaskBuckets = new Map<number, RetainedMask[]>();
  let nextMaskOrdinal = 0;
  const rememberMask = (removed: Uint8Array): void => {
    const hash = constraintMaskHash(removed);
    if (retainedMaskBuckets.get(hash)?.some((entry) =>
      sameConstraintMask(entry.removed, removed)
    )) return;
    const entry: RetainedMask = {
      hash,
      removed: removed.slice(),
      score: removedScore(graph, removed),
      ordinal: nextMaskOrdinal++,
    };
    const addEntry = (): void => {
      retainedMasks.push(entry);
      const bucket = retainedMaskBuckets.get(hash);
      if (bucket) bucket.push(entry);
      else retainedMaskBuckets.set(hash, [entry]);
    };
    if (retainedMasks.length < maximumMasks) {
      addEntry();
      return;
    }
    let worstIndex = -1;
    for (let index = 0; index < retainedMasks.length; index += 1) {
      const candidate = retainedMasks[index];
      const worst = worstIndex < 0 ? undefined : retainedMasks[worstIndex];
      if (!worst || betterScore(worst.score, candidate.score) ||
        compareRelaxationScores(candidate.score, worst.score) === 0 &&
          candidate.ordinal > worst.ordinal) worstIndex = index;
    }
    const worst = worstIndex < 0 ? undefined : retainedMasks[worstIndex];
    if (worst && betterScore(entry.score, worst.score)) {
      const worstBucket = retainedMaskBuckets.get(worst.hash) as RetainedMask[];
      const bucketIndex = worstBucket.indexOf(worst);
      if (bucketIndex >= 0) worstBucket.splice(bucketIndex, 1);
      if (worstBucket.length === 0) retainedMaskBuckets.delete(worst.hash);
      retainedMasks[worstIndex] = entry;
      const entryBucket = retainedMaskBuckets.get(hash);
      if (entryBucket) entryBucket.push(entry);
      else retainedMaskBuckets.set(hash, [entry]);
    }
  };
  rememberMask(initial);
  let lowerBound = 0;
  const finish = (optimal: boolean): ConstraintResult<ConstraintSearchResult> => {
    rememberMask(best);
    const masks = [...retainedMasks]
      .sort((a, b) =>
        (sameConstraintMask(a.removed, best) ? -1 : sameConstraintMask(b.removed, best) ? 1 : 0) ||
        compareRelaxationScores(a.score, b.score) || a.ordinal - b.ordinal
      )
      .map((entry) => entry.removed);
    return success({
      removed: best,
      masks,
      score: bestScore,
      lowerBound,
      optimal,
      cutoff: optimal ? "none" : budgetCutoff ?? "restarts",
      restarts: attemptedRestarts,
      feasibilityChecks,
      elapsedMs: Math.max(0, now() - started),
    });
  };
  // A feasible empty relaxation cannot be beaten: certify it before spending
  // any bound work, and independently of any downstream stop request — the
  // proof is already in hand.
  if (bestScore[0] === 0 && bestScore[1] === 0) return finish(true);
  // A streamed mask callback may already have found a perfect geometric
  // incumbent (or exhausted a caller-owned downstream budget). Do not spend
  // certification or restart work after it asks the master search to stop.
  if (stopRequested) return finish(false);
  const seeded = collectDisjointConflictCores(graph, check);
  if (!seeded.ok) {
    if (seeded.reason === "time" || seeded.reason === "work") return finish(false);
    return seeded;
  }
  const cores = seeded.value.cores;
  lowerBound = seeded.value.lowerBound;
  if (stopRequested) return finish(false);
  // The bound counts only the primary weight, so it certifies a score that
  // gives up no reciprocal exit and no level.
  const certifiedByBound = (): boolean =>
    bestScore[0] === lowerBound && bestScore[1] === 0 && bestScore[2] === 0;
  if (certifiedByBound()) return finish(true);
  if (!seeded.value.complete) return finish(false);

  // Restores one relaxed relation of a feasible `mask` when the mask stays
  // feasible, and reverts it otherwise. A level relation is restored in two
  // steps: its level first, with its group's rays still relaxed, then the
  // rays. The mask is feasible whenever this returns.
  const restore = (mask: Uint8Array, relation: number): ConstraintResult<void> => {
    const group = relation < graph.groupCount
      ? -1
      : graph.levelRelationGroup[relation - graph.groupCount];
    mask[relation] = 0;
    if (group >= 0) mask[group] = 1;
    const restored = check(mask);
    if (!restored.ok || restored.value.conflict) {
      mask[relation] = 1;
      if (group >= 0) mask[group] = 0;
      return restored.ok ? success(undefined) : restored;
    }
    if (group < 0) return success(undefined);
    mask[group] = 0;
    const rays = check(mask);
    if (!rays.ok || rays.value.conflict) mask[group] = 1;
    return rays.ok ? success(undefined) : rays;
  };

  // The initial feasible state removes every directionally-invalid canonical
  // relation. Restore each removed group once in a stable objective-aware
  // order before the exact hitting-set loop: the cheaper incumbent tightens
  // its branch and bound, and the anytime geometry lane receives a better
  // early mask. A failed restore is reverted; the incumbent remains feasible
  // throughout.
  const restorationOrder = Array.from(initial, (_value, group) => group)
    .filter((group) => initial[group] !== 0)
    .sort((a, b) => graph.groupReciprocalCount[b] - graph.groupReciprocalCount[a] || a - b);
  for (const group of restorationOrder) {
    const restored = restore(initial, group);
    if (!restored.ok) {
      if (restored.reason === "time" || restored.reason === "work") return finish(false);
      return restored;
    }
  }
  rememberMask(initial);
  publishMask(initial);
  const restoredScore = removedScore(graph, initial);
  if (betterScore(restoredScore, bestScore)) {
    best = initial.slice();
    bestScore = restoredScore;
    lastProgressAt = now();
    runtime.progress?.({
      ...emptyConstraintWorkStats(),
      phase: "search",
      restarts: attemptedRestarts,
      feasibilityChecks,
      layoutsConsidered: 0,
      compactionAttempts: 0,
      elapsedMs: Math.max(0, lastProgressAt - started),
      bestQuality: undefined,
    });
  }
  if (stopRequested) return finish(false);
  if (certifiedByBound()) return finish(true);

  // Primary strategy: MaxHS-style implicit hitting set. Alternate an exact
  // min-weight hitting set over the accumulated cores, solved per independent
  // component, with one feasibility check of that set as a removal mask. A
  // feasible exact minimum is optimal outright; an infeasible one contributes
  // a new core (none of whose groups it removed) and the iteration repeats.
  // Termination within the ceilings is the normal outcome; hitting a ceiling
  // falls back to randomized restarts with whatever bound the completed solves
  // proved.
  const hittingSetBudget = {
    nodes: runtime.maximumHittingSetNodes === undefined
      ? MAX_HITTING_SET_DFS_NODES
      : Math.max(0, Math.floor(runtime.maximumHittingSetNodes)),
  };
  for (let iteration = 0; iteration < MAX_HITTING_SET_ITERATIONS; iteration += 1) {
    const solved = solveMinimumHittingSetByComponent(
      graph,
      cores,
      best,
      bestScore,
      hittingSetBudget,
    );
    if (!solved.complete) break;
    if (!solved.mask || !solved.score) {
      // No hitting set beats the incumbent, and every feasible mask must hit
      // every accumulated core: the incumbent is weight-optimal.
      lowerBound = Math.max(lowerBound, bestScore[0]);
      return finish(true);
    }
    lowerBound = Math.max(lowerBound, solved.score[0]);
    const checked = check(solved.mask);
    if (!checked.ok) {
      if (checked.reason === "time" || checked.reason === "work") return finish(false);
      return checked;
    }
    const conflict = checked.value.conflict;
    if (!conflict) {
      best = solved.mask.slice();
      bestScore = solved.score;
      rememberMask(best);
      publishMask(best);
      lastProgressAt = now();
      runtime.progress?.({
        ...emptyConstraintWorkStats(),
        phase: "search",
        restarts: attemptedRestarts,
        feasibilityChecks,
        layoutsConsidered: 0,
        compactionAttempts: 0,
        elapsedMs: Math.max(0, lastProgressAt - started),
      });
      // The exact hitting-set minimum is itself feasible: weight-optimal
      // regardless of any downstream stop request.
      return finish(true);
    }
    if (conflict.length === 0) return failure("analysis");
    cores.push(conflict);
  }

  // Fallback: seeded randomized conflict-driven restarts, reachable only when
  // conflict accumulation exhausted a hitting-set ceiling.
  const random = randomGenerator(RANDOM_SEED);
  for (let restarts = 0; restarts < maximumRestarts; restarts += 1) {
    if (timeExpired()) return finish(false);
    attemptedRestarts += 1;
    const removed = new Uint8Array(graph.relationCount);
    let removedWeight = 0;
    let restartWork = 0;
    const maximumRestartWork = graph.relationCount * 6 + 16;
    const continueRestart = (): boolean => {
      restartWork += 1;
      if (restartWork > maximumRestartWork) {
        budgetCutoff ??= "restarts";
        return false;
      }
      return (restartWork & 0x3ff) !== 0 || !timeExpired();
    };
    for (;;) {
      if (!continueRestart()) return finish(false);
      const checked = check(removed);
      if (!checked.ok) {
        if (checked.reason === "time" || checked.reason === "work") return finish(false);
        return checked;
      }
      const conflict = checked.value.conflict;
      if (!conflict) break;
      if (conflict.length === 0) return failure("analysis");
      const oneWay = conflict.filter((group) => graph.groupReciprocalCount[group] === 0);
      const choices = oneWay.length > 0 && random() < 0.85 ? oneWay : conflict;
      const selected = choices[Math.floor(random() * choices.length)];
      if (!removed[selected]) {
        removed[selected] = 1;
        removedWeight += graph.groupSourceCount[selected];
      }
      if (removedWeight > bestScore[0]) break;
    }
    canonicalRelationMask(graph, removed);
    const order: number[] = [];
    for (let group = 0; group < graph.relationCount; group += 1) {
      if (!continueRestart()) return finish(false);
      if (removed[group]) order.push(group);
    }
    for (let index = order.length - 1; index > 0; index -= 1) {
      if (!continueRestart()) return finish(false);
      const swap = Math.floor(random() * (index + 1));
      [order[index], order[swap]] = [order[swap], order[index]];
    }
    for (const group of order) {
      if (!continueRestart()) return finish(false);
      const restored = restore(removed, group);
      if (!restored.ok) {
        if (restored.reason === "time" || restored.reason === "work") return finish(false);
        return restored;
      }
    }
    const finalCheck = check(removed);
    if (!finalCheck.ok) {
      if (finalCheck.reason === "time" || finalCheck.reason === "work") return finish(false);
      return finalCheck;
    }
    if (finalCheck.value.conflict) continue;
    rememberMask(removed);
    publishMask(removed);
    const score = removedScore(graph, removed);
    if (betterScore(score, bestScore)) {
      best = removed.slice();
      bestScore = score;
      lastProgressAt = now();
      runtime.progress?.({
        ...emptyConstraintWorkStats(),
        phase: "search",
        restarts: attemptedRestarts,
        feasibilityChecks,
        layoutsConsidered: 0,
        compactionAttempts: 0,
        elapsedMs: Math.max(0, now() - started),
      });
    }
    if (stopRequested) return finish(false);
    if (certifiedByBound()) break;
  }
  const optimal = certifiedByBound();
  if (!optimal && timeExpired()) budgetCutoff = "time";
  return finish(optimal);
}

type ConstraintSpatialKey = number | string;

const CONSTRAINT_CELL_AXIS_LIMIT = 1 << 20;
const CONSTRAINT_CELL_LEVEL_LIMIT = 1 << 9;
const CONSTRAINT_CELL_AXIS_SPAN = 1 << 21;

/** Keep constraint-search occupancy keys on the same packed numeric fast path as layout.ts. */
function constraintCellKeyAt(x: number, y: number, level: number): ConstraintSpatialKey {
  if (
    x >= -CONSTRAINT_CELL_AXIS_LIMIT && x < CONSTRAINT_CELL_AXIS_LIMIT &&
    y >= -CONSTRAINT_CELL_AXIS_LIMIT && y < CONSTRAINT_CELL_AXIS_LIMIT &&
    level >= -CONSTRAINT_CELL_LEVEL_LIMIT && level < CONSTRAINT_CELL_LEVEL_LIMIT &&
    Number.isInteger(x) && Number.isInteger(y) && Number.isInteger(level)
  ) {
    return ((level + CONSTRAINT_CELL_LEVEL_LIMIT) * CONSTRAINT_CELL_AXIS_SPAN +
      (x + CONSTRAINT_CELL_AXIS_LIMIT)) * CONSTRAINT_CELL_AXIS_SPAN +
      (y + CONSTRAINT_CELL_AXIS_LIMIT);
  }
  return `${level}:${x}:${y}`;
}

function constraintLaneKey(level: number, coordinate: number): ConstraintSpatialKey {
  if (
    coordinate >= -CONSTRAINT_CELL_AXIS_LIMIT && coordinate < CONSTRAINT_CELL_AXIS_LIMIT &&
    level >= -CONSTRAINT_CELL_LEVEL_LIMIT && level < CONSTRAINT_CELL_LEVEL_LIMIT &&
    Number.isInteger(coordinate) && Number.isInteger(level)
  ) {
    return (level + CONSTRAINT_CELL_LEVEL_LIMIT) * CONSTRAINT_CELL_AXIS_SPAN +
      (coordinate + CONSTRAINT_CELL_AXIS_LIMIT);
  }
  return `${level}:${coordinate}`;
}

function lowerBoundNumber(values: readonly number[], target: number): number {
  let low = 0;
  let high = values.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (values[middle] < target) low = middle + 1;
    else high = middle;
  }
  return low;
}

function upperBoundNumber(values: readonly number[], target: number): number {
  let low = 0;
  let high = values.length;
  while (low < high) {
    const middle = (low + high) >>> 1;
    if (values[middle] <= target) low = middle + 1;
    else high = middle;
  }
  return low;
}

function updateCrossingTree(
  tree: Int32Array,
  index: number,
  delta: number,
  node: number,
  left: number,
  right: number,
): void {
  tree[node] += delta;
  if (left === right) return;
  const middle = (left + right) >>> 1;
  if (index <= middle) updateCrossingTree(tree, index, delta, node * 2, left, middle);
  else updateCrossingTree(tree, index, delta, node * 2 + 1, middle + 1, right);
}

function collectCrossingTree(
  tree: Int32Array,
  active: readonly ReadonlySet<number>[],
  minimum: number,
  maximum: number,
  output: number[],
  node: number,
  left: number,
  right: number,
): void {
  if (tree[node] === 0 || right < minimum || left > maximum) return;
  if (left === right) {
    for (const edge of active[left]) output.push(edge);
    return;
  }
  const middle = (left + right) >>> 1;
  collectCrossingTree(tree, active, minimum, maximum, output, node * 2, left, middle);
  collectCrossingTree(tree, active, minimum, maximum, output, node * 2 + 1, middle + 1, right);
}

interface ConstraintPhysicalEdge {
  group: number;
  from: number;
  to: number;
  axis: number;
}

interface CrossingLevelScratch {
  horizontal: number[];
  vertical: number[];
  events: number[];
  yValues: number[];
  active: Set<number>[];
  tree: Int32Array;
}

/** Reusable strict-interior orthogonal crossing sweep for one fixed edge set. */
function createConstraintCrossingIndex(
  physicalEdges: readonly ConstraintPhysicalEdge[],
): {
  crossingsByFirst: number[][];
  build: (
    positions: readonly (readonly [number, number, number])[],
    shouldCancel: () => boolean,
  ) => boolean;
} {
  const crossingLevels = new Map<number, CrossingLevelScratch>();
  const crossingLevelPool: CrossingLevelScratch[] = [];
  const crossingsByFirst: number[][] = Array.from(
    { length: physicalEdges.length },
    () => [],
  );
  const crossingCandidates: number[] = [];
  const build = (
    positions: readonly (readonly [number, number, number])[],
    shouldCancel: () => boolean,
  ): boolean => {
    crossingLevels.clear();
    for (const crossings of crossingsByFirst) crossings.length = 0;
    let usedLevels = 0;
    const levelScratch = (level: number): CrossingLevelScratch => {
      let scratch = crossingLevels.get(level);
      if (scratch) return scratch;
      scratch = crossingLevelPool[usedLevels] ?? (crossingLevelPool[usedLevels] = {
        horizontal: [],
        vertical: [],
        events: [],
        yValues: [],
        active: [],
        tree: new Int32Array(0),
      });
      usedLevels += 1;
      scratch.horizontal.length = 0;
      scratch.vertical.length = 0;
      scratch.events.length = 0;
      scratch.yValues.length = 0;
      crossingLevels.set(level, scratch);
      return scratch;
    };
    for (let edge = 0; edge < physicalEdges.length; edge += 1) {
      if ((edge & 0x3f) === 0 && shouldCancel()) return false;
      const physical = physicalEdges[edge];
      const from = positions[physical.from];
      const to = positions[physical.to];
      if (from[2] !== to[2]) continue;
      const scratch = levelScratch(from[2]);
      if (physical.axis === 0) scratch.horizontal.push(edge);
      else scratch.vertical.push(edge);
    }
    for (const scratch of crossingLevels.values()) {
      if (shouldCancel()) return false;
      const { events, yValues, active } = scratch;
      for (const edge of scratch.horizontal) {
        const physical = physicalEdges[edge];
        yValues.push(positions[physical.from][1]);
        // End, query, start event order excludes endpoint intersections.
        events.push(edge * 3, edge * 3 + 2);
      }
      for (const edge of scratch.vertical) events.push(edge * 3 + 1);
      yValues.sort((left, right) => left - right);
      let uniqueY = 0;
      for (const value of yValues) {
        if (uniqueY === 0 || yValues[uniqueY - 1] !== value) yValues[uniqueY++] = value;
      }
      yValues.length = uniqueY;
      if (uniqueY === 0 || scratch.vertical.length === 0) continue;
      for (const bucket of active) bucket.clear();
      while (active.length < uniqueY) active.push(new Set());
      const requiredTreeLength = uniqueY * 4 + 4;
      if (scratch.tree.length < requiredTreeLength) {
        scratch.tree = new Int32Array(requiredTreeLength);
      } else {
        scratch.tree.fill(0, 0, requiredTreeLength);
      }
      const eventCoordinate = (code: number): number => {
        const edge = Math.floor(code / 3);
        const type = code % 3;
        const physical = physicalEdges[edge];
        const from = positions[physical.from];
        const to = positions[physical.to];
        if (type === 1) return from[0];
        return type === 0 ? Math.max(from[0], to[0]) : Math.min(from[0], to[0]);
      };
      events.sort((left, right) =>
        eventCoordinate(left) - eventCoordinate(right) ||
        left % 3 - right % 3 || Math.floor(left / 3) - Math.floor(right / 3)
      );
      for (let eventIndex = 0; eventIndex < events.length; eventIndex += 1) {
        if ((eventIndex & 0x3f) === 0 && shouldCancel()) return false;
        const code = events[eventIndex];
        const edge = Math.floor(code / 3);
        const type = code % 3;
        const physical = physicalEdges[edge];
        if (type !== 1) {
          const yIndex = lowerBoundNumber(yValues, positions[physical.from][1]);
          const bucket = active[yIndex];
          if (type === 0) {
            if (bucket.delete(edge)) {
              updateCrossingTree(scratch.tree, yIndex, -1, 1, 0, uniqueY - 1);
            }
          } else if (!bucket.has(edge)) {
            bucket.add(edge);
            updateCrossingTree(scratch.tree, yIndex, 1, 1, 0, uniqueY - 1);
          }
          continue;
        }
        const from = positions[physical.from];
        const to = positions[physical.to];
        const minimumY = Math.min(from[1], to[1]);
        const maximumY = Math.max(from[1], to[1]);
        const firstY = upperBoundNumber(yValues, minimumY);
        const lastY = lowerBoundNumber(yValues, maximumY) - 1;
        if (firstY > lastY) continue;
        crossingCandidates.length = 0;
        collectCrossingTree(
          scratch.tree,
          active,
          firstY,
          lastY,
          crossingCandidates,
          1,
          0,
          uniqueY - 1,
        );
        for (const horizontalEdge of crossingCandidates) {
          const horizontal = physicalEdges[horizontalEdge];
          if (horizontal.from === physical.from || horizontal.from === physical.to ||
            horizontal.to === physical.from || horizontal.to === physical.to) continue;
          const first = Math.min(horizontalEdge, edge);
          const second = Math.max(horizontalEdge, edge);
          crossingsByFirst[first].push(second);
        }
      }
    }
    for (const crossings of crossingsByFirst) crossings.sort((left, right) => left - right);
    return true;
  };
  return { crossingsByFirst, build };
}

function median(values: number[]): number {
  values.sort((a, b) => a - b);
  return values[Math.floor(values.length / 2)];
}

interface ConstraintCompactionRuntime {
  maximumStates?: number;
  maximumLiveSearchNodes?: number;
  shouldCancel?: () => boolean;
  score?: (positions: ReadonlyMap<string, GridPosition>) => LayoutQuality;
  onIncumbent?: (
    candidate: {
      /** Scratch view valid only during this callback. */
      dense?: readonly (readonly [number, number, number])[];
      /** Hash fused into the scratch-map update for this dense view. */
      hash?: ConstraintPositionHash;
      /** Stable snapshot, created lazily when invoked during this callback. */
      materialize: () => Map<string, GridPosition>;
    },
    quality: Readonly<LayoutQuality>,
  ) => void;
  onProgress?: () => void;
  /** Soft-defect relation groups eligible for an equal-primary mask swap. */
  onDiversification?: (relationGroups: readonly number[]) => void;
  onFinish?: (status: {
    completed: boolean;
    cancelled: boolean;
    exhausted: boolean;
  }) => void;
  workStats?: ConstraintRepairWorkStats;
  /** Test-only differential counters against the former brute-force defect scans. */
  verifySpatialIndexes?: {
    obstructionQueries: number;
    obstructionHits: number;
    crossingStates: number;
    crossingPairs: number;
  };
  /** Test-only collision injector; production uses both canonical hash lanes. */
  softDefectHash?: (
    kind: number,
    values: readonly number[],
    first: number,
    second: number,
    count: number,
  ) => string;
}

function hardValidCompactionPositions(
  graph: DenseConstraintGraph,
  removedGroups: Uint8Array,
  positions: readonly (readonly [number, number, number])[],
  fixedIds: ReadonlySet<string>,
): boolean {
  for (let node = 0; node < graph.nodeCount; node += 1) {
    if (!graph.levelCrossingReachable[node] && positions[node][2] !== graph.positions[2][node]) {
      return false;
    }
  }
  for (const id of fixedIds) {
    const node = graph.indexById.get(id);
    if (node === undefined) continue;
    if (positions[node][0] !== graph.positions[0][node] ||
      positions[node][1] !== graph.positions[1][node] ||
      positions[node][2] !== graph.positions[2][node]) return false;
  }
  for (let group = 0; group < graph.groupCount; group += 1) {
    const axis = graph.groupAxis[group];
    const low = positions[graph.groupFrom[group]];
    const high = positions[graph.groupTo[group]];
    const level = graph.groupLevelRelation[group];
    const levelRelaxed = level >= 0 && !!removedGroups[level];
    // A relaxed planar ray may move or reorder its endpoints in x/y, but it
    // never permits either endpoint to leave their shared map level; only a
    // relaxed level relation does, and it frees the relation entirely.
    if (axis !== 2 && !levelRelaxed && low[2] !== high[2]) return false;
    if (removedGroups[group] || levelRelaxed) continue;
    if (high[axis] - low[axis] < graph.groupStep[group]) return false;
    for (let perpendicular = 0; perpendicular < 3; perpendicular += 1) {
      if (perpendicular !== axis && low[perpendicular] !== high[perpendicular]) return false;
    }
  }
  return true;
}

/** Cells with |x|, |y| < 2^19 and |level| < 2^10 get a numeric key; any other cell a string key. */
const ADMISSION_AXIS_LIMIT = 2 ** 19;
const ADMISSION_LEVEL_LIMIT = 2 ** 10;

/**
 * Reusable buffers for candidate admission on one compiled graph. Admission
 * is synchronous and never re-enters itself, so one set serves every check:
 * the candidate's coordinates in node order, and an open-addressing table of
 * the numeric keys of the cells it occupies. A slot is filled only while it
 * carries the current check's stamp, so starting a check clears the table in
 * constant time.
 */
class AdmissionScratch {
  readonly xs: Float64Array;
  readonly ys: Float64Array;
  readonly levels: Float64Array;
  readonly #cells: Float64Array;
  readonly #stamps: Uint32Array;
  readonly #mask: number;
  #stamp = 0;

  constructor(nodeCount: number) {
    this.xs = new Float64Array(nodeCount);
    this.ys = new Float64Array(nodeCount);
    this.levels = new Float64Array(nodeCount);
    let capacity = 16;
    while (capacity < nodeCount * 2) capacity *= 2;
    this.#cells = new Float64Array(capacity);
    this.#stamps = new Uint32Array(capacity);
    this.#mask = capacity - 1;
  }

  clearCells(): void {
    this.#stamp += 1;
    if (this.#stamp > 0xffff_ffff) {
      this.#stamps.fill(0);
      this.#stamp = 1;
    }
  }

  /**
   * Occupy the cell at integral coordinates inside the numeric keys' range;
   * false when a room of this check already occupies it.
   */
  claimCell(x: number, y: number, level: number): boolean {
    // Highest key ~2^51, inside the safe-integer range.
    const key = ((x + ADMISSION_AXIS_LIMIT) * (2 * ADMISSION_AXIS_LIMIT) + y + ADMISSION_AXIS_LIMIT) *
        (2 * ADMISSION_LEVEL_LIMIT) + level + ADMISSION_LEVEL_LIMIT;
    let hash = Math.imul(x, 0x9e3779b1) ^ Math.imul(y, 0x85ebca77) ^ Math.imul(level, 0xc2b2ae3d);
    hash = Math.imul(hash ^ (hash >>> 15), 0x2c1b3c6d);
    let slot = (hash ^ (hash >>> 12)) & this.#mask;
    while (this.#stamps[slot] === this.#stamp) {
      if (this.#cells[slot] === key) return false;
      slot = (slot + 1) & this.#mask;
    }
    this.#stamps[slot] = this.#stamp;
    this.#cells[slot] = key;
    return true;
  }
}

/**
 * The rules every hard-valid candidate meets whatever its mask: each room of
 * the graph on a safe-integer cell of its own, levels kept outside
 * level-crossing components, and pinned rooms in place. Returns the graph's
 * admission scratch holding the
 * candidate's coordinates in node order, or undefined when a rule fails.
 */
function admittedCells(
  graph: DenseConstraintGraph,
  positions: ReadonlyMap<string, GridPosition>,
  fixedIds: ReadonlySet<string>,
): AdmissionScratch | undefined {
  if (positions.size !== graph.nodeCount) return undefined;
  const scratch = graph.admission ??= new AdmissionScratch(graph.nodeCount);
  const { xs, ys, levels } = scratch;
  const storedLevels = graph.positions[2];
  scratch.clearCells();
  let numericKeys = true;
  for (let node = 0; node < graph.nodeCount; node += 1) {
    const position = positions.get(graph.ids[node]);
    if (!position) return undefined;
    const { x, y, level } = position;
    if (!Number.isSafeInteger(x) || !Number.isSafeInteger(y) || !Number.isSafeInteger(level)) {
      return undefined;
    }
    if (!graph.levelCrossingReachable[node] && level !== storedLevels[node]) return undefined;
    xs[node] = x;
    ys[node] = y;
    levels[node] = level;
    if (!numericKeys) continue;
    if (Math.abs(x) < ADMISSION_AXIS_LIMIT && Math.abs(y) < ADMISSION_AXIS_LIMIT &&
      Math.abs(level) < ADMISSION_LEVEL_LIMIT) {
      if (!scratch.claimCell(x, y, level)) return undefined;
    } else {
      numericKeys = false;
    }
  }
  if (!numericKeys) {
    // A cell outside the numeric keys' range: key every room's cell as text.
    const occupied = new Set<string>();
    for (let node = 0; node < graph.nodeCount; node += 1) {
      const key = `${xs[node]},${ys[node]},${levels[node]}`;
      if (occupied.has(key)) return undefined;
      occupied.add(key);
    }
  }
  for (const id of fixedIds) {
    const node = graph.indexById.get(id);
    if (node === undefined) continue;
    if (xs[node] !== graph.positions[0][node] || ys[node] !== graph.positions[1][node] ||
      levels[node] !== storedLevels[node]) return undefined;
  }
  return scratch;
}

/**
 * Whether a complete candidate satisfies the hard relations `removedGroups`
 * keeps, with every room on a cell of its own: the verdict of
 * `hardValidCompactionPositions` for a candidate map.
 */
function hardValidLayoutPositions(
  graph: DenseConstraintGraph,
  removedGroups: Uint8Array,
  positions: ReadonlyMap<string, GridPosition>,
  fixedIds: ReadonlySet<string>,
): boolean {
  const cells = admittedCells(graph, positions, fixedIds);
  if (!cells) return false;
  const { xs, ys, levels } = cells;
  for (let group = 0; group < graph.groupCount; group += 1) {
    const axis = graph.groupAxis[group];
    const low = graph.groupFrom[group];
    const high = graph.groupTo[group];
    const level = graph.groupLevelRelation[group];
    const levelRelaxed = level >= 0 && !!removedGroups[level];
    // A relaxed planar ray still keeps its rooms on one level unless its level
    // relation is relaxed too.
    if (axis !== 2 && !levelRelaxed && levels[low] !== levels[high]) return false;
    if (removedGroups[group] || levelRelaxed) continue;
    const coordinates = axis === 0 ? xs : axis === 1 ? ys : levels;
    if (coordinates[high] - coordinates[low] < graph.groupStep[group]) return false;
    // A planar ray's level is the shared-level rule above.
    if (axis !== 0 && xs[low] !== xs[high]) return false;
    if (axis !== 1 && ys[low] !== ys[high]) return false;
  }
  return true;
}

/**
 * `hardValidLayoutPositions` under the least mask the candidate admits,
 * `removedGroupsForPositions(graph, positions)`. That mask keeps a relation
 * only when every edge of it is on its ray, which already spaces the
 * relation's rooms along its axis and lines them up across it, and gives up
 * the level relation of every flat relation whose rooms it finds on different
 * levels, so of the relation checks only the hard shared level of a planar
 * relation without a level relation remains.
 */
function acceptsAnyHardValidLayoutPositions(
  graph: DenseConstraintGraph,
  positions: ReadonlyMap<string, GridPosition>,
  fixedIds: ReadonlySet<string>,
): boolean {
  const cells = admittedCells(graph, positions, fixedIds);
  if (!cells) return false;
  const levels = cells.levels;
  for (let pair = 0; pair < graph.levelEqualityFrom.length; pair += 1) {
    if (levels[graph.levelEqualityFrom[pair]] !== levels[graph.levelEqualityTo[pair]]) return false;
  }
  return true;
}

/**
 * `hardValidLayoutPositions` stated with a string key and a tuple per room.
 * The oracle the allocation-free admission is tested against; admission never
 * calls it.
 */
function referenceHardValidLayoutPositions(
  graph: DenseConstraintGraph,
  removedGroups: Uint8Array,
  positions: ReadonlyMap<string, GridPosition>,
  fixedIds: ReadonlySet<string>,
): boolean {
  if (positions.size !== graph.nodeCount) return false;
  const occupied = new Set<string>();
  const dense: [number, number, number][] = [];
  for (let node = 0; node < graph.nodeCount; node += 1) {
    const position = positions.get(graph.ids[node]);
    if (!position || !Number.isSafeInteger(position.x) ||
      !Number.isSafeInteger(position.y) || !Number.isSafeInteger(position.level)) return false;
    const integral: [number, number, number] = [
      position.x,
      position.y,
      position.level,
    ];
    const key = integral.join(",");
    if (occupied.has(key)) return false;
    occupied.add(key);
    dense.push(integral);
  }
  return hardValidCompactionPositions(graph, removedGroups, dense, fixedIds);
}

/** Undefined reports a cancellation observed mid-check, never an admission verdict. */
function createConstraintSeparatorAdmission(nodeCount: number): (
  alternatives: readonly ConstraintExtensionAlternative[],
  outgoing: readonly (readonly (readonly number[])[])[],
  shouldCancel: () => boolean,
) => boolean | undefined {
  const reachSeen = new Int32Array(nodeCount);
  const reachStack: number[] = [];
  let reachStamp = 0;
  const pathExists = (
    outgoing: readonly (readonly number[])[],
    from: number,
    target: number,
    shouldCancel: () => boolean,
  ): boolean | undefined => {
    if (from === target) return true;
    reachStamp += 1;
    if (reachStamp === 0x7fffffff) {
      reachSeen.fill(0);
      reachStamp = 1;
    }
    const stamp = reachStamp;
    reachStack.length = 0;
    reachStack.push(from);
    reachSeen[from] = stamp;
    let work = 0;
    while (reachStack.length > 0) {
      if ((work++ & 0x3ff) === 0 && shouldCancel()) return undefined;
      const node = reachStack.pop() as number;
      for (const next of outgoing[node]) {
        if (next === target) return true;
        if (reachSeen[next] === stamp) continue;
        reachSeen[next] = stamp;
        reachStack.push(next);
      }
    }
    reachStack.length = 0;
    return false;
  };
  return (alternatives, outgoing, shouldCancel): boolean | undefined => {
    for (const alternative of alternatives) {
      // Production geometric defects currently use one atomic precedence arc.
      // Retain the generic loop so later multi-arc defects remain conservative.
      let changed = false;
      let admissible = true;
      for (const arc of alternative.arcs) {
        const reverse = pathExists(outgoing[arc.axis], arc.to, arc.from, shouldCancel);
        if (reverse === undefined) return undefined;
        if (reverse) {
          admissible = false;
          break;
        }
        const implied = pathExists(outgoing[arc.axis], arc.from, arc.to, shouldCancel);
        if (implied === undefined) return undefined;
        if (!implied) changed = true;
      }
      if (admissible && changed) return true;
    }
    return false;
  };
}

/** Distinct from an inadmissible defect: a cancellation observed during admission. */
const DEFECT_SCAN_CANCELLED = "cancelled";

function firstAdmissibleConstraintDefect<T>(
  candidates: Iterable<T>,
  admit: (candidate: T) => ConstraintExtensionDefect | typeof DEFECT_SCAN_CANCELLED | undefined,
): ConstraintExtensionDefect | typeof DEFECT_SCAN_CANCELLED | undefined {
  for (const candidate of candidates) {
    const defect = admit(candidate);
    if (defect) return defect;
  }
  return undefined;
}

function compactConstraints(
  graph: DenseConstraintGraph,
  removed: Uint8Array,
  fixedIds: ReadonlySet<string> = new Set(),
  runtime: ConstraintCompactionRuntime = {},
): ConstraintResult<Map<string, GridPosition>> {
  const analyzed = analyzeConstraints(graph, removed, true);
  if (!analyzed.ok) return analyzed;
  const state = analyzed.value.state;
  if (!state) return failure("compaction");

  // Difference-constraint ranks intentionally minimize slack. Multiple fixed
  // anchors can require different offsets inside one retained component (for
  // example A=0, B=5 with A→B). Preserve the supplied complete geometry as a
  // hard-valid incumbent before attempting that optional compaction.
  const suppliedPositions = new Map(graph.ids.map((id, node) => [id, {
    x: graph.positions[0][node],
    y: graph.positions[1][node],
    level: graph.positions[2][node],
  }]));
  const suppliedHardValid = hardValidLayoutPositions(
    graph,
    removed,
    suppliedPositions,
    fixedIds,
  );
  if (suppliedHardValid) {
    const suppliedQuality = runtime.score
      ? runtime.score(suppliedPositions)
      : measureIntegralLayoutQuality(suppliedPositions, graph.sourceEdges);
    runtime.onIncumbent?.({ materialize: () => suppliedPositions }, suppliedQuality);
  }

  // Scratch reused by every `buildCoordinates` call across the separator
  // states of this one extension search (single-threaded and synchronous, so
  // exactly one state is ever in flight). Ownership rule: nothing built from
  // these buffers may outlive the `inspect` call that received it — the next
  // state overwrites everything. Whatever escapes into a published candidate
  // is copied at the escape point: candidate positions are copied scalar-by-
  // scalar into a fresh Map, and defect/alternative objects carry only
  // numbers. The graph's node count is fixed for the life of this search, so
  // these capacities never grow.
  const scratchNodeCount = graph.nodeCount;
  const emptyNodeLists = (): number[][] =>
    Array.from({ length: scratchNodeCount }, () => [] as number[]);
  const scratchOutgoing: number[][][] = [emptyNodeLists(), emptyNodeLists(), emptyNodeLists()];
  const scratchExtensionOutgoing: number[][][] = [
    emptyNodeLists(),
    emptyNodeLists(),
    emptyNodeLists(),
  ];
  const scratchUndirected: number[][][] = [emptyNodeLists(), emptyNodeLists(), emptyNodeLists()];
  const baseIndegree = [0, 1, 2].map(() => new Int32Array(scratchNodeCount));
  const baseInWeight = [0, 1, 2].map(() => new Int32Array(scratchNodeCount));
  const baseOutWeight = [0, 1, 2].map(() => new Int32Array(scratchNodeCount));
  const baseFixedRoot = [0, 1, 2].map(() => new Uint8Array(scratchNodeCount));
  const baseTarget = [0, 1, 2].map(() => new Int32Array(scratchNodeCount));
  const rootLists: number[][] = [[], [], []];
  // The retained relation graph and every root-derived property are immutable
  // for this mask. Build them once; each separator state applies only the
  // suffix that differs from the preceding extension path.
  for (let axis = 0; axis < 3; axis += 1) {
    const roots = state.roots[axis];
    const base = state.graphs[axis];
    const outgoing = scratchOutgoing[axis];
    const undirected = scratchUndirected[axis];
    const targets = emptyNodeLists();
    for (let node = 0; node < scratchNodeCount; node += 1) {
      if (roots[node] === node) rootLists[axis].push(node);
      targets[roots[node]].push(graph.positions[axis][node]);
    }
    for (let arc = 0; arc < base.length; arc += 1) {
      const from = base.sourceRoot[arc];
      const to = base.targetRoot[arc];
      outgoing[from].push(to);
      undirected[from].push(to);
      undirected[to].push(from);
      baseIndegree[axis][to] += 1;
      const weight = graph.groupSourceCount[base.edge[arc]];
      baseOutWeight[axis][from] += weight;
      baseInWeight[axis][to] += weight;
    }
    for (const root of rootLists[axis]) baseTarget[axis][root] = median(targets[root]);
    for (const id of fixedIds) {
      const node = graph.indexById.get(id);
      if (node !== undefined) baseFixedRoot[axis][roots[node]] = 1;
    }
  }
  const appliedExtensionArcs: ConstraintExtensionArc[] = [];
  const currentIndegree = baseIndegree.map((values) => values.slice());
  // `indegree`, `coordinate`, and `fixedShift` follow write-before-read
  // discipline (roots are fully seeded before any read); `rank` needs its zero
  // fill and the two flag arrays need their clears.
  const scratchIndegree = new Int32Array(scratchNodeCount);
  const scratchRank = new Int32Array(scratchNodeCount);
  const scratchCoordinate = [
    new Int32Array(scratchNodeCount),
    new Int32Array(scratchNodeCount),
    new Int32Array(scratchNodeCount),
  ];
  const scratchFixedShift = new Int32Array(scratchNodeCount);
  const scratchHasFixedShift = new Uint8Array(scratchNodeCount);
  const scratchEntered = new Uint8Array(scratchNodeCount);
  const scratchReady = new DenseMinHeap();
  const scratchOrder: number[] = [];
  const scratchComponent: number[] = [];
  const scratchComponentQueue: number[] = [];
  const scratchShifts: number[] = [];
  const scratchPositions: [number, number, number][] = Array.from(
    { length: scratchNodeCount },
    () => [0, 0, 0],
  );
  const scratchPositionObjects: GridPosition[] = Array.from(
    { length: scratchNodeCount },
    () => ({ x: 0, y: 0, level: 0 }),
  );
  const scratchPositionMap = new Map<string, GridPosition>(
    graph.ids.map((id, node) => [id, scratchPositionObjects[node]]),
  );
  const updateScratchPositionMap = (
    positions: readonly (readonly [number, number, number])[],
  ): ConstraintPositionHash => {
    let hash = 0x811c9dc5;
    let second = 0x9e3779b9;
    for (let node = 0; node < scratchNodeCount; node += 1) {
      const target = scratchPositionObjects[node];
      target.x = positions[node][0];
      target.y = positions[node][1];
      target.level = positions[node][2];
      for (const value of positions[node]) {
        const low = value | 0;
        const high = Math.floor(value / 0x1_0000_0000);
        hash = Math.imul(hash ^ low, 0x01000193) >>> 0;
        hash = Math.imul(hash ^ high, 0x01000193) >>> 0;
        second = Math.imul(second ^ (low + 0x7f4a7c15), 0x85ebca6b) >>> 0;
        second = Math.imul(second ^ (high + 0x7f4a7c15), 0x85ebca6b) >>> 0;
      }
    }
    return { hash, second };
  };

  const buildCoordinates = (
    extensionArcs: readonly ConstraintExtensionArc[],
  ): ConstraintResult<{
    positions: [number, number, number][];
    outgoing: number[][][];
  }> => {
    let work = 0;
    const stopped = (): boolean =>
      (work++ & 0x3ff) === 0 && runtime.shouldCancel?.() === true;
    let common = 0;
    while (common < appliedExtensionArcs.length && common < extensionArcs.length) {
      const previous = appliedExtensionArcs[common];
      const next = extensionArcs[common];
      if (previous.axis !== next.axis || previous.from !== next.from || previous.to !== next.to) break;
      common += 1;
    }
    while (appliedExtensionArcs.length > common) {
      const arc = appliedExtensionArcs.pop() as ConstraintExtensionArc;
      scratchOutgoing[arc.axis][arc.from].pop();
      scratchExtensionOutgoing[arc.axis][arc.from].pop();
      scratchUndirected[arc.axis][arc.from].pop();
      scratchUndirected[arc.axis][arc.to].pop();
      currentIndegree[arc.axis][arc.to] -= 1;
    }
    for (let index = common; index < extensionArcs.length; index += 1) {
      if (stopped()) return failure("time");
      const arc = extensionArcs[index];
      scratchOutgoing[arc.axis][arc.from].push(arc.to);
      scratchExtensionOutgoing[arc.axis][arc.from].push(arc.to);
      scratchUndirected[arc.axis][arc.from].push(arc.to);
      scratchUndirected[arc.axis][arc.to].push(arc.from);
      currentIndegree[arc.axis][arc.to] += 1;
      appliedExtensionArcs.push(arc);
    }
    for (let axis = 0; axis < 3; axis += 1) {
      const roots: Int32Array = state.roots[axis];
      const base: AxisGraph = state.graphs[axis];
      const outgoing = scratchOutgoing[axis];
      const extensionOutgoing = scratchExtensionOutgoing[axis];
      const rootList = rootLists[axis];
      const indegree = scratchIndegree;
      indegree.set(currentIndegree[axis]);
      const ready = scratchReady;
      ready.clear();
      for (const root of rootList) if (indegree[root] === 0) ready.push(root);
      const order = scratchOrder;
      order.length = 0;
      while (ready.length > 0) {
        if (stopped()) return failure("time");
        const root = ready.pop() as number;
        order.push(root);
        for (const target of outgoing[root]) {
          indegree[target] -= 1;
          if (indegree[target] === 0) ready.push(target);
        }
      }
      if (order.length !== rootList.length) return failure("compaction");

      const rank = scratchRank;
      rank.fill(0);
      for (const root of order) {
        if (stopped()) return failure("time");
        for (let arc = base.head[root]; arc !== -1; arc = base.next[arc]) {
          rank[base.to[arc]] = Math.max(rank[base.to[arc]], rank[root] + base.step[arc]);
        }
        for (const to of extensionOutgoing[root]) {
          rank[to] = Math.max(rank[to], rank[root] + 1);
        }
      }
      // Longest-path ranks pin every root at its earliest feasible coordinate,
      // which strands slack on retained relations whose source chain has no
      // other support: a neighborhood attached only from behind sits flush
      // against zero while its successors are pushed ahead by longer chains.
      // One reverse-topological raise pass compresses exactly that slack: a
      // root whose weighted in-degree does not exceed its weighted out-degree
      // rises to its tightest outgoing bound. Weights are retained source-edge
      // multiplicities — extension separators weigh nothing but still bound
      // the move — so each move changes weighted retained slack by
      // delta * (in - out), never positive. Bounds only relax for roots still
      // to be processed (a raise loosens only its predecessors' bounds), so
      // one sweep reaches the per-root fixed point. Roots holding fixed rooms
      // never move: their ranks anchor the component shifts, and moving one
      // could break multi-anchor shift agreement that longest-path ranks
      // satisfied.
      const inWeight = baseInWeight[axis];
      const outWeight = baseOutWeight[axis];
      const fixedRoot = baseFixedRoot[axis];
      for (let index = order.length - 1; index >= 0; index -= 1) {
        if (stopped()) return failure("time");
        const root = order[index];
        if (fixedRoot[root] || inWeight[root] > outWeight[root]) continue;
        let bound = Number.MAX_SAFE_INTEGER;
        for (let arc = base.head[root]; arc !== -1; arc = base.next[arc]) {
          bound = Math.min(bound, rank[base.to[arc]] - base.step[arc]);
        }
        for (const to of extensionOutgoing[root]) {
          bound = Math.min(bound, rank[to] - 1);
        }
        if (bound !== Number.MAX_SAFE_INTEGER && bound > rank[root]) rank[root] = bound;
      }
      const undirected = scratchUndirected[axis];
      const target = baseTarget[axis];
      const coordinate = scratchCoordinate[axis];
      const fixedShift = scratchFixedShift;
      const hasFixedShift = scratchHasFixedShift;
      hasFixedShift.fill(0);
      for (let node = 0; node < graph.nodeCount; node += 1) {
        if (!fixedIds.has(graph.ids[node])) continue;
        const root = roots[node];
        const shift = graph.positions[axis][node] - rank[root];
        if (hasFixedShift[root] && fixedShift[root] !== shift) return failure("compaction");
        hasFixedShift[root] = 1;
        fixedShift[root] = shift;
      }
      const entered = scratchEntered;
      entered.fill(0);
      const component = scratchComponent;
      const queue = scratchComponentQueue;
      for (const start of rootList) {
        if (entered[start]) continue;
        component.length = 0;
        queue.length = 0;
        queue.push(start);
        let queueIndex = 0;
        entered[start] = 1;
        while (queueIndex < queue.length) {
          if (stopped()) return failure("time");
          const root = queue[queueIndex++];
          component.push(root);
          for (const neighbor of undirected[root]) {
            if (entered[neighbor]) continue;
            entered[neighbor] = 1;
            queue.push(neighbor);
          }
        }
        let anchoredShift: number | undefined;
        for (const root of component) {
          if (!hasFixedShift[root]) continue;
          if (anchoredShift !== undefined && anchoredShift !== fixedShift[root]) {
            return failure("compaction");
          }
          anchoredShift = fixedShift[root];
        }
        let shift: number;
        if (anchoredShift === undefined) {
          const shifts = scratchShifts;
          shifts.length = 0;
          for (const root of component) shifts.push(target[root] - rank[root]);
          shift = median(shifts);
        } else {
          shift = anchoredShift;
        }
        for (const root of component) coordinate[root] = rank[root] + shift;
      }
    }
    const positions = scratchPositions;
    for (let node = 0; node < graph.nodeCount; node += 1) {
      const triple = positions[node];
      triple[0] = scratchCoordinate[0][state.roots[0][node]];
      triple[1] = scratchCoordinate[1][state.roots[1][node]];
      triple[2] = scratchCoordinate[2][state.roots[2][node]];
    }
    return success({ positions, outgoing: scratchOutgoing });
  };

  const physicalEdges: { group: number; from: number; to: number; axis: number }[] = [];
  const physicalKeys = new Set<string>();
  for (let group = 0; group < graph.groupCount; group += 1) {
    if (graph.groupAxis[group] > 1 || groupRelaxed(graph, removed, group)) continue;
    const from = graph.groupFrom[group];
    const to = graph.groupTo[group];
    const low = Math.min(from, to);
    const high = Math.max(from, to);
    const key = `${low}:${high}:${graph.groupAxis[group]}`;
    if (physicalKeys.has(key)) continue;
    physicalKeys.add(key);
    physicalEdges.push({ group, from, to, axis: graph.groupAxis[group] });
  }

  const singleArc = (
    axis: number,
    from: number,
    to: number,
  ): ConstraintExtensionAlternative => ({
    arcs: [{ axis: axis as 0 | 1 | 2, from, to }],
  });
  const MAX_SOFT_DEFECT_SIGNATURES = 4_096;
  const MAX_SOFT_DEFECT_WITNESS_ARCS = 65_536;
  interface SoftDefectWitness {
    hash: string;
    kind: number;
    values: number[];
    arcs: ConstraintExtensionArc[];
  }
  const seenSoftDefects = new Map<string, SoftDefectWitness[]>();
  const softDefectOrder: (SoftDefectWitness | undefined)[] = Array.from(
    { length: MAX_SOFT_DEFECT_SIGNATURES },
  );
  let nextSoftDefectInsertion = 0;
  let retainedSoftDefectWitnesses = 0;
  let retainedSoftDefectArcs = 0;
  const softDefectSignature = (
    kind: number,
    values: readonly number[],
    extensionFingerprintFirst: number,
    extensionFingerprintSecond: number,
    extensionFingerprintCount: number,
  ): string => {
    let first = 0x811c9dc5;
    let second = 0x9e3779b9;
    const feed = (value: number): void => {
      first = Math.imul(first ^ value, 0x01000193) >>> 0;
      second = Math.imul(second ^ (value + 0x7f4a7c15), 0x85ebca6b) >>> 0;
    };
    feed(kind);
    for (const value of values) feed(value);
    feed(extensionFingerprintCount);
    feed(extensionFingerprintFirst);
    feed(extensionFingerprintSecond);
    return `${first.toString(16).padStart(8, "0")}${second.toString(16).padStart(8, "0")}`;
  };
  const sameSoftDefectWitness = (
    witness: SoftDefectWitness,
    kind: number,
    values: readonly number[],
    arcs: readonly ConstraintExtensionArc[],
  ): boolean => {
    if (witness.kind !== kind || witness.values.length !== values.length ||
      witness.arcs.length !== arcs.length) return false;
    for (let index = 0; index < values.length; index += 1) {
      if (witness.values[index] !== values[index]) return false;
    }
    // Active arcs are unique but traversal-order dependent. Exact unordered
    // comparison is paid only when the two-lane hash repeats; the common path
    // remains allocation-free.
    for (const arc of arcs) {
      if (!witness.arcs.some((known) => known.axis === arc.axis && known.from === arc.from &&
        known.to === arc.to)) return false;
    }
    return true;
  };
  const removeSoftDefectWitness = (witness: SoftDefectWitness): void => {
    const bucket = seenSoftDefects.get(witness.hash);
    if (bucket) {
      const index = bucket.indexOf(witness);
      if (index >= 0) bucket.splice(index, 1);
      if (bucket.length === 0) seenSoftDefects.delete(witness.hash);
    }
    retainedSoftDefectArcs -= witness.arcs.length;
  };
  const evictNextSoftDefectWitness = (): void => {
    if (retainedSoftDefectWitnesses === 0) return;
    while (!softDefectOrder[nextSoftDefectInsertion]) {
      nextSoftDefectInsertion =
        (nextSoftDefectInsertion + 1) % MAX_SOFT_DEFECT_SIGNATURES;
    }
    removeSoftDefectWitness(softDefectOrder[nextSoftDefectInsertion] as SoftDefectWitness);
    softDefectOrder[nextSoftDefectInsertion] = undefined;
    retainedSoftDefectWitnesses -= 1;
  };
  const rememberSoftDefect = (
    hash: string,
    kind: number,
    values: readonly number[],
    arcs: readonly ConstraintExtensionArc[],
  ): boolean => {
    const bucket = seenSoftDefects.get(hash);
    if (bucket?.some((witness) => sameSoftDefectWitness(witness, kind, values, arcs))) return false;
    // A single exceptionally deep state cannot make the witness cache large.
    // It remains explorable, but is deliberately not deduplicated.
    if (arcs.length > MAX_SOFT_DEFECT_WITNESS_ARCS) return true;
    while (retainedSoftDefectArcs + arcs.length > MAX_SOFT_DEFECT_WITNESS_ARCS &&
      retainedSoftDefectWitnesses > 0) evictNextSoftDefectWitness();
    if (softDefectOrder[nextSoftDefectInsertion]) evictNextSoftDefectWitness();
    const witness: SoftDefectWitness = {
      hash,
      kind,
      values: values.slice(),
      arcs: arcs.slice(),
    };
    const targetBucket = seenSoftDefects.get(hash) ?? [];
    targetBucket.push(witness);
    seenSoftDefects.set(hash, targetBucket);
    retainedSoftDefectArcs += witness.arcs.length;
    softDefectOrder[nextSoftDefectInsertion] = witness;
    retainedSoftDefectWitnesses += 1;
    nextSoftDefectInsertion =
      (nextSoftDefectInsertion + 1) % MAX_SOFT_DEFECT_SIGNATURES;
    return true;
  };
  const hasAdmissibleSeparator = createConstraintSeparatorAdmission(graph.nodeCount);
  // Per-state scan scratch, reset at each use inside `inspect` and never
  // escaping it: published alternatives are mapped into fresh objects at the
  // escape point, and blocker records hold only numbers.
  const scratchOccupied = new Map<ConstraintSpatialKey, number>();
  const scratchWeightedAlternatives: {
    penalty: number;
    axis: number;
    from: number;
    to: number;
  }[] = [];
  const scratchBlockers: { edge: number; node: number; atPort: boolean }[] = [];
  const scratchLanes: [
    Map<ConstraintSpatialKey, number[]>,
    Map<ConstraintSpatialKey, number[]>,
  ] = [new Map(), new Map()];
  const scratchLanePools: [number[][], number[][]] = [[], []];
  const buildSpatialLanes = (
    positions: readonly (readonly [number, number, number])[],
  ): void => {
    for (let axis = 0; axis < 2; axis += 1) {
      const lanes = scratchLanes[axis];
      const pool = scratchLanePools[axis];
      lanes.clear();
      let used = 0;
      const perpendicular = axis === 0 ? 1 : 0;
      for (let node = 0; node < scratchNodeCount; node += 1) {
        const position = positions[node];
        const key = constraintLaneKey(position[2], position[perpendicular]);
        let lane = lanes.get(key);
        if (!lane) {
          lane = pool[used] ?? (pool[used] = []);
          lane.length = 0;
          used += 1;
          lanes.set(key, lane);
        }
        lane.push(node);
      }
      for (const lane of lanes.values()) {
        lane.sort((left, right) =>
          positions[left][axis] - positions[right][axis] || left - right
        );
      }
    }
  };
  const firstLaneCoordinateAfter = (
    lane: readonly number[],
    positions: readonly (readonly [number, number, number])[],
    axis: number,
    coordinate: number,
  ): number => {
    let low = 0;
    let high = lane.length;
    while (low < high) {
      const middle = (low + high) >>> 1;
      if (positions[lane[middle]][axis] <= coordinate) low = middle + 1;
      else high = middle;
    }
    return low;
  };
  const crossingIndex = createConstraintCrossingIndex(physicalEdges);
  const crossingsByFirst = crossingIndex.crossingsByFirst;
  const inspect = ({
    extensionArcs,
    extensionFingerprintFirst,
    extensionFingerprintSecond,
    extensionFingerprintCount,
    shouldCancel,
  }: {
    extensionArcs: readonly ConstraintExtensionArc[];
    extensionFingerprintFirst: number;
    extensionFingerprintSecond: number;
    extensionFingerprintCount: number;
    shouldCancel: () => boolean;
  }): ConstraintExtensionInspection<Int32Array, LayoutQuality> => {
    const builtResult = buildCoordinates(extensionArcs);
    if (!builtResult.ok) {
      // A deadline observed while building geometry says nothing about this
      // state. Fabricating a conflict here would let a cut traversal drain the
      // stack and masquerade as an exhaustively completed search.
      if (builtResult.reason === "time") return { type: "cancelled" };
      return {
        type: "hard-conflict",
        conflict: { kind: builtResult.reason, alternatives: [] },
      };
    }
    const built = builtResult.value;
    const occupied = scratchOccupied;
    occupied.clear();
    let collision: readonly [number, number] | undefined;
    for (let node = 0; node < graph.nodeCount; node += 1) {
      const position = built.positions[node];
      const key = constraintCellKeyAt(position[0], position[1], position[2]);
      const previous = occupied.get(key);
      if (previous !== undefined) {
        collision = [previous, node];
        break;
      }
      occupied.set(key, node);
    }
    const weightedAlternatives = scratchWeightedAlternatives;
    weightedAlternatives.length = 0;
    if (collision) {
      // Levels express map topology. Ordinary room collisions must be healed
      // in the visible x/y plane rather than inventing a new floor.
      for (let axis = 0; axis < 2; axis += 1) {
        const roots = state.roots[axis];
        const a = roots[collision[0]];
        const b = roots[collision[1]];
        if (a === b) continue;
        const preferred: readonly [number, number] = graph.positions[axis][collision[0]] <=
            graph.positions[axis][collision[1]]
          ? [a, b]
          : [b, a];
        for (const [from, to] of [preferred, [preferred[1], preferred[0]]] as const) {
          weightedAlternatives.push({
            axis,
            from,
            to,
            penalty: graph.positions[axis][collision[0]] === graph.positions[axis][collision[1]] ? 1 : 0,
          });
        }
      }
      weightedAlternatives.sort((a, b) =>
        a.penalty - b.penalty || a.axis - b.axis || a.from - b.from || a.to - b.to
      );
      return {
        type: "hard-conflict",
        conflict: {
          kind: "collision",
          alternatives: weightedAlternatives.map(({ axis, from, to }) => singleArc(axis, from, to)),
        },
      };
    }
    if (!hardValidCompactionPositions(graph, removed, built.positions, fixedIds)) {
      return { type: "hard-conflict", conflict: { kind: "hard-validity", alternatives: [] } };
    }
    const positionHash = updateScratchPositionMap(built.positions);
    if (runtime.verifySpatialIndexes) {
      const referenceHash = denseConstraintPositionHash(graph, built.positions);
      if (!referenceHash || referenceHash.hash !== positionHash.hash ||
        referenceHash.second !== positionHash.second) {
        throw new Error("fused dense position hash diverged from the canonical hash");
      }
    }
    const quality = runtime.score
      ? runtime.score(scratchPositionMap)
      : measureIntegralLayoutQuality(scratchPositionMap, graph.sourceEdges);
    let materializedDense: Int32Array | undefined;
    const materializeDensePositions = (): Int32Array => {
      if (materializedDense) return materializedDense;
      materializedDense = new Int32Array(scratchNodeCount * 3);
      let cursor = 0;
      for (const position of built.positions) {
        materializedDense[cursor++] = position[0];
        materializedDense[cursor++] = position[1];
        materializedDense[cursor++] = position[2];
      }
      return materializedDense;
    };
    let materializedPositions: Map<string, GridPosition> | undefined;
    const materializePositions = (): Map<string, GridPosition> => {
      if (materializedPositions) return materializedPositions;
      const dense = materializedDense;
      materializedPositions = new Map(graph.ids.map((id, node) => dense
        ? [id, { x: dense[node * 3], y: dense[node * 3 + 1], level: dense[node * 3 + 2] }]
        : [id, {
          x: built.positions[node][0],
          y: built.positions[node][1],
          level: built.positions[node][2],
        }]
      ));
      if (runtime.workStats) {
        runtime.workStats.candidateMaterializations =
          (runtime.workStats.candidateMaterializations ?? 0) + 1;
      }
      return materializedPositions;
    };
    runtime.onIncumbent?.({
      dense: built.positions,
      hash: positionHash,
      materialize: materializePositions,
    }, quality);
    // The incumbent above is already published; a cancellation observed at any
    // later point of this inspection must surface as a cancellation so the
    // truncated defect scan can never pass for a fully explored state.
    if (shouldCancel()) return { type: "cancelled" };
    buildSpatialLanes(built.positions);

    const admissibleDefect = (
      defect: ConstraintExtensionDefect,
      signatureValues: readonly number[],
      kind: number,
    ): ConstraintExtensionDefect | typeof DEFECT_SCAN_CANCELLED | undefined => {
      const admissible = hasAdmissibleSeparator(defect.alternatives, built.outgoing, shouldCancel);
      if (admissible === undefined) return DEFECT_SCAN_CANCELLED;
      if (!admissible) return undefined;
      const signature = softDefectSignature(
        kind,
        signatureValues,
        extensionFingerprintFirst,
        extensionFingerprintSecond,
        extensionFingerprintCount,
      );
      const hash = runtime.softDefectHash?.(
        kind,
        signatureValues,
        extensionFingerprintFirst,
        extensionFingerprintSecond,
        extensionFingerprintCount,
      ) ?? signature;
      return rememberSoftDefect(hash, kind, signatureValues, extensionArcs) ? defect : undefined;
    };
    const obstructionDefect = (
      obstruction: { edge: number; node: number; atPort: boolean },
    ): ConstraintExtensionDefect => {
      weightedAlternatives.length = 0;
      const edgeAxis = graph.groupAxis[obstruction.edge];
      const perpendicular = edgeAxis === 0 ? 1 : 0;
      const edgeRoot = state.roots[perpendicular][graph.groupFrom[obstruction.edge]];
      const blockerRoot = state.roots[perpendicular][obstruction.node];
      if (edgeRoot !== blockerRoot) {
        const edgeOriginal = graph.positions[perpendicular][graph.groupFrom[obstruction.edge]];
        const blockerOriginal = graph.positions[perpendicular][obstruction.node];
        const preferred: readonly [number, number] = edgeOriginal <= blockerOriginal
          ? [edgeRoot, blockerRoot]
          : [blockerRoot, edgeRoot];
        for (const [from, to] of [preferred, [preferred[1], preferred[0]]] as const) {
          weightedAlternatives.push({
            axis: perpendicular,
            from,
            to,
            penalty: edgeOriginal === blockerOriginal ? 1 : 0,
          });
        }
      }
      const roots = state.roots[edgeAxis];
      const blocker = roots[obstruction.node];
      const fromNode = graph.groupFrom[obstruction.edge];
      const toNode = graph.groupTo[obstruction.edge];
      const lowNode = built.positions[fromNode][edgeAxis] <= built.positions[toNode][edgeAxis]
        ? fromNode
        : toNode;
      const highNode = lowNode === fromNode ? toNode : fromNode;
      for (const [from, to] of [
        [blocker, roots[lowNode]],
        [roots[highNode], blocker],
      ] as const) {
        weightedAlternatives.push({ axis: edgeAxis, from, to, penalty: 2 });
      }
      weightedAlternatives.sort((a, b) =>
        a.penalty - b.penalty || a.axis - b.axis || a.from - b.from || a.to - b.to
      );
      return {
        kind: "obstruction",
        relationGroups: [obstruction.edge],
        alternatives: weightedAlternatives.map(({ axis, from, to }) => singleArc(axis, from, to)),
      };
    };

    // Scan every stable obstruction candidate until one has a separator which
    // is neither already implied nor cycle-closing. An unrepairable first
    // obstruction must not hide a later repairable defect in the same layout.
    for (let group = 0; group < graph.groupCount; group += 1) {
      if ((group & 0x3f) === 0 && shouldCancel()) return { type: "cancelled" };
      if (graph.groupAxis[group] > 1 || groupRelaxed(graph, removed, group)) continue;
      const fromNode = graph.groupFrom[group];
      const toNode = graph.groupTo[group];
      const from = built.positions[fromNode];
      const to = built.positions[toNode];
      if (from[2] !== to[2]) continue;
      const axis = graph.groupAxis[group];
      const perpendicular = axis === 0 ? 1 : 0;
      const minimum = Math.min(from[axis], to[axis]);
      const maximum = Math.max(from[axis], to[axis]);
      const blockers = scratchBlockers;
      blockers.length = 0;
      const lane = scratchLanes[axis].get(constraintLaneKey(from[2], from[perpendicular])) ?? [];
      for (let laneIndex = firstLaneCoordinateAfter(
        lane,
        built.positions,
        axis,
        minimum,
      ); laneIndex < lane.length; laneIndex += 1) {
        const node = lane[laneIndex];
        const position = built.positions[node];
        if (position[axis] >= maximum) break;
        blockers.push({
          edge: group,
          node,
          atPort: Math.abs(position[axis] - from[axis]) === 1 ||
            Math.abs(position[axis] - to[axis]) === 1,
        });
      }
      blockers.sort((a, b) => Number(b.atPort) - Number(a.atPort) ||
        (a.atPort ? a.node - b.node : b.node - a.node));
      if (runtime.verifySpatialIndexes) {
        runtime.verifySpatialIndexes.obstructionQueries += 1;
        runtime.verifySpatialIndexes.obstructionHits += blockers.length;
        const reference: { edge: number; node: number; atPort: boolean }[] = [];
        for (let node = 0; node < graph.nodeCount; node += 1) {
          if (node === fromNode || node === toNode) continue;
          const position = built.positions[node];
          if (position[2] !== from[2] || position[perpendicular] !== from[perpendicular] ||
            position[axis] <= minimum || position[axis] >= maximum) continue;
          reference.push({
            edge: group,
            node,
            atPort: Math.abs(position[axis] - from[axis]) === 1 ||
              Math.abs(position[axis] - to[axis]) === 1,
          });
        }
        reference.sort((a, b) => Number(b.atPort) - Number(a.atPort) ||
          (a.atPort ? a.node - b.node : b.node - a.node));
        if (reference.length !== blockers.length || reference.some((value, index) => {
          const actual = blockers[index];
          return value.edge !== actual.edge || value.node !== actual.node ||
            value.atPort !== actual.atPort;
        })) throw new Error("constraint obstruction spatial index diverged from brute force");
      }
      const defect = firstAdmissibleConstraintDefect(blockers, (blocker) =>
        admissibleDefect(
          obstructionDefect(blocker),
          [blocker.edge, blocker.node, blocker.atPort ? 1 : 0],
          1,
        )
      );
      if (defect === DEFECT_SCAN_CANCELLED) return { type: "cancelled" };
      if (defect) {
        return {
          type: "candidate",
          materializeCandidate: materializeDensePositions,
          score: quality,
          softDefect: defect,
        };
      }
    }

    if (!crossingIndex.build(built.positions, shouldCancel)) return { type: "cancelled" };
    if (runtime.verifySpatialIndexes) {
      runtime.verifySpatialIndexes.crossingStates += 1;
      runtime.verifySpatialIndexes.crossingPairs += crossingsByFirst.reduce(
        (total, crossings) => total + crossings.length,
        0,
      );
      const reference = Array.from({ length: physicalEdges.length }, () => [] as number[]);
      for (let first = 0; first < physicalEdges.length; first += 1) {
        for (let second = first + 1; second < physicalEdges.length; second += 1) {
          const a = physicalEdges[first];
          const b = physicalEdges[second];
          if (a.axis === b.axis || a.from === b.from || a.from === b.to ||
            a.to === b.from || a.to === b.to) continue;
          const horizontal = a.axis === 0 ? a : b;
          const vertical = a.axis === 1 ? a : b;
          const hFrom = built.positions[horizontal.from];
          const hTo = built.positions[horizontal.to];
          const vFrom = built.positions[vertical.from];
          const vTo = built.positions[vertical.to];
          if (hFrom[2] !== hTo[2] || hFrom[2] !== vFrom[2] || hFrom[2] !== vTo[2]) continue;
          const minimumX = Math.min(hFrom[0], hTo[0]);
          const maximumX = Math.max(hFrom[0], hTo[0]);
          const minimumY = Math.min(vFrom[1], vTo[1]);
          const maximumY = Math.max(vFrom[1], vTo[1]);
          if (vFrom[0] > minimumX && vFrom[0] < maximumX &&
            hFrom[1] > minimumY && hFrom[1] < maximumY) reference[first].push(second);
        }
      }
      if (reference.some((expected, first) => {
        const actual = crossingsByFirst[first];
        return expected.length !== actual.length ||
          expected.some((second, index) => second !== actual[index]);
      })) throw new Error("constraint crossing sweep diverged from brute force ordering");
    }
    for (let first = 0; first < physicalEdges.length; first += 1) {
      if ((first & 0x3f) === 0 && shouldCancel()) return { type: "cancelled" };
      for (const second of crossingsByFirst[first]) {
        const a = physicalEdges[first];
        const b = physicalEdges[second];
        const horizontal = a.axis === 0 ? a : b;
        const vertical = a.axis === 1 ? a : b;
        const hFrom = built.positions[horizontal.from];
        const hTo = built.positions[horizontal.to];
        const vFrom = built.positions[vertical.from];
        const vTo = built.positions[vertical.to];
        if (hFrom[2] !== hTo[2] || hFrom[2] !== vFrom[2] || hFrom[2] !== vTo[2]) continue;
        const minimumX = Math.min(hFrom[0], hTo[0]);
        const maximumX = Math.max(hFrom[0], hTo[0]);
        const minimumY = Math.min(vFrom[1], vTo[1]);
        const maximumY = Math.max(vFrom[1], vTo[1]);
        if (vFrom[0] <= minimumX || vFrom[0] >= maximumX ||
          hFrom[1] <= minimumY || hFrom[1] >= maximumY) continue;
        weightedAlternatives.length = 0;
        const horizontalY = state.roots[1][horizontal.from];
        const verticalFromY = state.roots[1][vertical.from];
        const verticalToY = state.roots[1][vertical.to];
        const topY = built.positions[vertical.from][1] <= built.positions[vertical.to][1]
          ? verticalFromY
          : verticalToY;
        const bottomY = topY === verticalFromY ? verticalToY : verticalFromY;
        const verticalX = state.roots[0][vertical.from];
        const horizontalFromX = state.roots[0][horizontal.from];
        const horizontalToX = state.roots[0][horizontal.to];
        const leftX = built.positions[horizontal.from][0] <= built.positions[horizontal.to][0]
          ? horizontalFromX
          : horizontalToX;
        const rightX = leftX === horizontalFromX ? horizontalToX : horizontalFromX;
        for (const [axis, from, to] of [
          [1, horizontalY, topY],
          [1, bottomY, horizontalY],
          [0, verticalX, leftX],
          [0, rightX, verticalX],
        ] as const) {
          weightedAlternatives.push({ axis, from, to, penalty: 3 });
        }
        const defect = admissibleDefect({
          kind: "crossing",
          relationGroups: [horizontal.group, vertical.group],
          alternatives: weightedAlternatives.map(({ axis, from, to }) => singleArc(axis, from, to)),
        }, [horizontal.group, vertical.group], 2);
        if (defect === DEFECT_SCAN_CANCELLED) return { type: "cancelled" };
        if (defect) {
          return {
            type: "candidate",
            materializeCandidate: materializeDensePositions,
            score: quality,
            softDefect: defect,
          };
        }
      }
    }
    return { type: "candidate", materializeCandidate: materializeDensePositions, score: quality };
  };

  const baseArcs: ConstraintExtensionArc[] = [];
  for (let axis = 0; axis < 3; axis += 1) {
    const base = state.graphs[axis];
    for (let arc = 0; arc < base.length; arc += 1) {
      baseArcs.push({
        axis: axis as 0 | 1 | 2,
        from: base.sourceRoot[arc],
        to: base.targetRoot[arc],
      });
    }
  }
  const initialStats = runtime.workStats ? { ...runtime.workStats } : undefined;
  const updateWorkStats = (stats: {
    states: number;
    branches: number;
    cyclePrunes: number;
    peakLiveSearchNodes?: number;
  }): void => {
    if (!runtime.workStats || !initialStats) return;
    runtime.workStats.separatorStates = initialStats.separatorStates + stats.states;
    runtime.workStats.separatorBranches = initialStats.separatorBranches + stats.branches;
    runtime.workStats.separatorCyclePrunes = initialStats.separatorCyclePrunes + stats.cyclePrunes;
    runtime.workStats.peakLiveSearchNodes = Math.max(
      initialStats.peakLiveSearchNodes ?? 0,
      stats.peakLiveSearchNodes ?? 0,
    );
  };
  const result = searchConstraintExtensions<Int32Array, LayoutQuality>({
    axisNodeCounts: [graph.nodeCount, graph.nodeCount, graph.nodeCount],
    baseArcs,
    inspect: (context) => {
      if (runtime.workStats && initialStats) {
        runtime.workStats.separatorStates = initialStats.separatorStates + context.state;
        runtime.workStats.separatorBranches = initialStats.separatorBranches + context.branches;
        runtime.workStats.separatorCyclePrunes = initialStats.separatorCyclePrunes +
          context.cyclePrunes;
      }
      return inspect(context);
    },
    compareScores: compareLayoutQuality,
    maxExtensionStates: runtime.maximumStates,
    maxLiveSearchNodes: runtime.maximumLiveSearchNodes,
    shouldCancel: runtime.shouldCancel,
    progressIntervalStates: 16,
    snapshotDiversificationArcs: false,
    onProgress: (stats) => {
      updateWorkStats(stats);
      runtime.onProgress?.();
    },
    onEqualPrimaryDiversification: ({ reason, defect }) => {
      // Soft defects are heuristic geometry guidance: they may diversify the
      // next complete mask but can never prune this fixed-mask search or prove
      // infeasibility. Hard explanations remain confined to the generic core's
      // exhaustive root-conflict contract.
      if (reason === "soft-defect" && defect.relationGroups?.length) {
        runtime.onDiversification?.(defect.relationGroups);
      }
    },
  });
  updateWorkStats(result);
  runtime.onFinish?.({
    completed: result.completed,
    cancelled: result.cancelled,
    exhausted: result.exhausted,
  });
  if (result.best) {
    const positions = new Map<string, GridPosition>();
    for (let node = 0; node < graph.nodeCount; node += 1) {
      positions.set(graph.ids[node], {
        x: result.best[node * 3],
        y: result.best[node * 3 + 1],
        level: result.best[node * 3 + 2],
      });
    }
    if (runtime.workStats) {
      runtime.workStats.candidateMaterializations =
        (runtime.workStats.candidateMaterializations ?? 0) + 1;
    }
    return success(positions);
  }
  if (suppliedHardValid) return success(suppliedPositions);
  return failure(result.cancelled ? "time" : "compaction");
}

function samePosition(a: GridPosition, b: GridPosition): boolean {
  return a.x === b.x && a.y === b.y && a.level === b.level;
}

function recomputeMovedExisting(
  request: IntegralLayoutRequest,
  positions: ReadonlyMap<string, GridPosition>,
): ReadonlySet<string> {
  const result = new Set<string>();
  for (const resident of request.residents) {
    const after = positions.get(resident.id);
    if (after && !samePosition(integralPosition(resident.position), after)) result.add(resident.id);
  }
  return result;
}

/**
 * `plan` at the cheap compaction fixed point under `accepts`, the compaction
 * without axis groups that every layout the repair publishes, and the plan it
 * returns, end with. Rooms move as the request allows, and its residents'
 * stored positions break the compaction's ties, whatever nested planner pass
 * produced `plan`. Returns `plan` itself when nothing gains or finishing
 * fails, and otherwise a freshly measured plan that ranks at or above it;
 * `plan.quality` must be fresh.
 */
function finishedPlan(
  request: IntegralLayoutRequest,
  plan: IntegralLayoutPlan,
  accepts?: (positions: ReadonlyMap<string, GridPosition>) => boolean,
): IntegralLayoutPlan {
  try {
    const finished = compactIntegralLayoutPlan({ ...request, trace: undefined }, plan, {
      acceptsPositions: accepts,
      axisGroupCompaction: false,
    });
    if (finished === plan || (accepts && !accepts(finished.positions))) return plan;
    const quality = measureIntegralLayoutQuality(finished.positions, request.edges);
    if (compareLayoutQuality(quality, plan.quality) < 0) return plan;
    return {
      positions: new Map(finished.positions),
      movedExisting: recomputeMovedExisting(request, finished.positions),
      quality,
    };
  } catch {
    // Finishing is an aesthetic pass; it never retracts the plan it was given.
    return plan;
  }
}

/**
 * Two independent 32-bit FNV-style lanes fingerprint a complete position
 * assignment, the same technique the soft-defect signatures use. Coordinates
 * travel with the signature only for exact comparison against the retained
 * full-position holders (the winner and the polish frontier); the dedup
 * window stores nothing but the combined 64-bit key.
 */
interface ConstraintPositionSignature {
  hash: number;
  second: number;
  coordinates: Float64Array;
}

interface ConstraintPositionHash {
  hash: number;
  second: number;
}

function constraintPositionSignature(
  graph: DenseConstraintGraph,
  positions: ReadonlyMap<string, GridPosition>,
): ConstraintPositionSignature | undefined {
  if (positions.size !== graph.nodeCount) return undefined;
  const coordinates = new Float64Array(graph.nodeCount * 3);
  let hash = 0x811c9dc5;
  let second = 0x9e3779b9;
  let cursor = 0;
  for (const id of graph.ids) {
    const position = positions.get(id);
    if (!position || !Number.isSafeInteger(position.x) ||
      !Number.isSafeInteger(position.y) || !Number.isSafeInteger(position.level)) return undefined;
    for (const value of [position.x, position.y, position.level]) {
      coordinates[cursor++] = value;
      const low = value | 0;
      const high = Math.floor(value / 0x1_0000_0000);
      hash = Math.imul(hash ^ low, 0x01000193) >>> 0;
      hash = Math.imul(hash ^ high, 0x01000193) >>> 0;
      second = Math.imul(second ^ (low + 0x7f4a7c15), 0x85ebca6b) >>> 0;
      second = Math.imul(second ^ (high + 0x7f4a7c15), 0x85ebca6b) >>> 0;
    }
  }
  return { hash, second, coordinates };
}

function denseConstraintPositionSignature(
  graph: DenseConstraintGraph,
  positions: readonly (readonly [number, number, number])[],
  knownHash?: Readonly<ConstraintPositionHash>,
): ConstraintPositionSignature | undefined {
  if (positions.length !== graph.nodeCount) return undefined;
  const coordinates = new Float64Array(graph.nodeCount * 3);
  let cursor = 0;
  for (let node = 0; node < graph.nodeCount; node += 1) {
    for (const value of positions[node]) {
      if (!Number.isSafeInteger(value)) return undefined;
      coordinates[cursor++] = value;
    }
  }
  const hashed = knownHash ?? denseConstraintPositionHash(graph, positions);
  return hashed && { ...hashed, coordinates };
}

function denseConstraintPositionHash(
  graph: DenseConstraintGraph,
  positions: readonly (readonly [number, number, number])[],
): ConstraintPositionHash | undefined {
  if (positions.length !== graph.nodeCount) return undefined;
  let hash = 0x811c9dc5;
  let second = 0x9e3779b9;
  for (let node = 0; node < graph.nodeCount; node += 1) {
    for (const value of positions[node]) {
      if (!Number.isSafeInteger(value)) return undefined;
      const low = value | 0;
      const high = Math.floor(value / 0x1_0000_0000);
      hash = Math.imul(hash ^ low, 0x01000193) >>> 0;
      hash = Math.imul(hash ^ high, 0x01000193) >>> 0;
      second = Math.imul(second ^ (low + 0x7f4a7c15), 0x85ebca6b) >>> 0;
      second = Math.imul(second ^ (high + 0x7f4a7c15), 0x85ebca6b) >>> 0;
    }
  }
  return { hash, second };
}

function constraintPositionSignatureKey(signature: Readonly<ConstraintPositionHash>): string {
  return `${signature.hash.toString(16).padStart(8, "0")}${
    signature.second.toString(16).padStart(8, "0")}`;
}

function sameConstraintPositionSignature(
  left: ConstraintPositionSignature,
  right: ConstraintPositionSignature,
): boolean {
  if (left.hash !== right.hash || left.second !== right.second ||
    left.coordinates.length !== right.coordinates.length) return false;
  for (let index = 0; index < left.coordinates.length; index += 1) {
    if (left.coordinates[index] !== right.coordinates[index]) return false;
  }
  return true;
}

function beforeViolationCounts(
  request: IntegralLayoutRequest,
  standard: IntegralLayoutPlan,
): {
  beforeViolations: number;
  beforeRoutingViolations: number;
  beforeSettledViolations: number;
  standardSettledViolations: number;
  beforeSettledRoutingViolations: number;
  standardSettledRoutingViolations: number;
} {
  const residentIds = new Set(request.residents.map((resident) => resident.id));
  const settledEdges = request.edges.filter((edge) => residentIds.has(edge.from) && residentIds.has(edge.to));
  const before = new Map(request.residents.map((resident) => [resident.id, integralPosition(resident.position)]));
  return {
    beforeViolations: directionalViolationEdges(before, request.edges).length,
    beforeRoutingViolations: measureLayoutRoutingQuality(before, request.edges).routingViolations,
    beforeSettledViolations: directionalViolationEdges(before, settledEdges).length,
    standardSettledViolations: directionalViolationEdges(standard.positions, settledEdges).length,
    beforeSettledRoutingViolations: measureLayoutRoutingQuality(before, settledEdges).routingViolations,
    standardSettledRoutingViolations: measureLayoutRoutingQuality(
      standard.positions,
      settledEdges,
    ).routingViolations,
  };
}

/** Exits a plan draws wrong: directional violations, the mis-levelled ones among them, and routing violations. */
interface ExitViolations {
  directional: number;
  level: number;
  routing: number;
}

/**
 * Whether a plan draws exits worse than they were drawn before it, weighing a
 * directional violation, or a mis-levelled one, against routing violations
 * as the quality order does: a higher weighted count, or an equal one with
 * more mis-levelled, then directional, violations.
 */
function violationsRegress(after: ExitViolations, before: ExitViolations): boolean {
  const weighted = DIRECTIONAL_VIOLATION_WEIGHT *
      (after.directional - after.level - (before.directional - before.level)) +
    LEVEL_VIOLATION_WEIGHT * (after.level - before.level) + after.routing - before.routing;
  if (weighted !== 0) return weighted > 0;
  return after.level !== before.level ? after.level > before.level : after.directional > before.directional;
}

/**
 * The mis-levelled exits among `beforeViolationCounts`' directional
 * violations, for the regression gates alone: the report does not carry them.
 */
function beforeLevelViolationCounts(
  request: IntegralLayoutRequest,
  standard: IntegralLayoutPlan,
): { before: number; beforeSettled: number; standardSettled: number } {
  const residentIds = new Set(request.residents.map((resident) => resident.id));
  const settledEdges = request.edges.filter((edge) => residentIds.has(edge.from) && residentIds.has(edge.to));
  const before = new Map(request.residents.map((resident) => [resident.id, integralPosition(resident.position)]));
  return {
    before: levelViolationEdges(before, request.edges).length,
    beforeSettled: levelViolationEdges(before, settledEdges).length,
    standardSettled: levelViolationEdges(standard.positions, settledEdges).length,
  };
}

/**
 * The report of a repair that returns its standard plan without a searched
 * result. It selects and proves nothing, its final counts are the standard
 * plan's, and `work` carries whatever ran before the repair stopped.
 */
function unsearchedConstraintRepairReport(
  outcome: Exclude<ConstraintRepairOutcome, "searched">,
  options: ConstraintRepairOptions,
  standard: IntegralLayoutPlan,
  counts: ReturnType<typeof beforeViolationCounts>,
  work: Partial<ConstraintRepairReport> = {},
): ConstraintRepairReport {
  return {
    ...emptyConstraintWorkStats(),
    trigger: options.when,
    outcome,
    selected: false,
    constraintOptimal: false,
    optimal: false,
    cutoff: "none",
    lowerBound: 0,
    relaxedEdges: 0,
    reciprocalRelaxedEdges: 0,
    relaxedLevelRelations: 0,
    standardViolations: standard.quality.cardinalRayViolations,
    finalViolations: standard.quality.cardinalRayViolations,
    standardRoutingViolations: standard.quality.routingViolations,
    finalRoutingViolations: standard.quality.routingViolations,
    ...counts,
    restarts: 0,
    feasibilityChecks: 0,
    layoutsConsidered: 0,
    compactionAttempts: 0,
    extensionSearch: { completed: false, cancelled: false, exhausted: false },
    maskDiversification: { completed: false, exhausted: false },
    searchMs: 0,
    compactionMs: 0,
    polishTournaments: 0,
    polishPasses: 0,
    polishAnchorsTried: 0,
    polishImprovements: 0,
    geometricFixedPoint: false,
    polishCutoff: "none",
    polishMs: 0,
    crossingRepair: {
      completed: false,
      cancelled: false,
      exhausted: false,
      elapsedMs: 0,
      crossingsConsidered: 0,
      macrosConsidered: 0,
      pushClosures: 0,
      maxDepth: 0,
      visitedStates: 0,
    },
    ...work,
  };
}

interface ConstraintPolishProgress {
  repairStarted: number;
  deadline: number;
  maximumTournaments: number;
  maximumPasses: number;
  constraintLayoutsConsidered: number;
  compactionAttempts: number;
  restarts: number;
  feasibilityChecks: number;
  workStats: ConstraintRepairWorkStats;
  /** Reject optional polish maps which no longer satisfy the chosen hard mask. */
  acceptsPositions?: (positions: ReadonlyMap<string, GridPosition>) => boolean;
}

interface ConstraintPolishResult {
  plan: IntegralLayoutPlan;
  tournaments: number;
  passes: number;
  anchorsTried: number;
  improvements: number;
  fixedPoint: boolean;
  cutoff: "fixed-point" | "time" | "tournaments" | "passes" | "error";
  elapsedMs: number;
}

/** Private control-flow marker used to stop a finite nested planner cleanly. */
const POLISH_DEADLINE = Symbol("map-layout-polish-deadline");

/** Macro budget of the early lane's crossing repair. */
const EARLY_LANE_CROSSING_WORK = 64;

/** What the early lane reports while it runs, with the polish passes it has charged. */
interface EarlyLaneObserver {
  /** A strict improvement, as it lands. */
  improved(plan: IntegralLayoutPlan, passes: number): void;
  /** The best plan so far, once the planner pass completes and when the lane ends. */
  progressed(plan: IntegralLayoutPlan, passes: number): void;
}

/**
 * The repair's early anytime lane, which runs before the exact search: one
 * planner pass and then a deep crossing repair on `EARLY_LANE_CROSSING_WORK`
 * macros, both under `acceptsPositions` and both without axis-group
 * compaction, the planner's most expensive stage, which the crossing repair
 * would otherwise also run as a heal its macro budget does not bound. The
 * lane keeps its seed unless a stage strictly improves public quality, and
 * publishes and goes on from each improvement at the cheap compaction fixed
 * point.
 *
 * The planner pass counts as one polish pass once it completes. A finite
 * deadline stops the lane between candidates. When the planner admits no
 * candidate, not even the seed, it throws, and the lane goes on from the seed.
 */
function polishConstraintLayoutEarly(
  request: IntegralLayoutRequest,
  seed: IntegralLayoutPlan,
  residents: IntegralLayoutRequest["residents"],
  acceptsPositions: (positions: ReadonlyMap<string, GridPosition>) => boolean,
  deadline: number,
  now: () => number,
  observer: EarlyLaneObserver,
): ConstraintPolishResult {
  const started = now();
  const expired = (): boolean => Number.isFinite(deadline) && now() >= deadline;
  let best = seed;
  let passes = 0;
  let improvements = 0;
  let failed = false;
  const adopt = (positions: ReadonlyMap<string, GridPosition>): void => {
    if (!acceptsPositions(positions)) return;
    const quality = measureIntegralLayoutQuality(positions, request.edges);
    if (compareLayoutQuality(quality, best.quality) <= 0) return;
    // The lane goes on from the finished layout it publishes.
    best = finishedPlan(request, {
      positions: new Map(positions),
      movedExisting: recomputeMovedExisting(request, positions),
      quality,
    }, acceptsPositions);
    improvements += 1;
    observer.improved(best, passes);
  };
  const finish = (cutoff: ConstraintPolishResult["cutoff"]): ConstraintPolishResult => {
    observer.progressed(best, passes);
    return {
      plan: best,
      tournaments: 0,
      passes,
      anchorsTried: passes,
      improvements,
      fixedPoint: false,
      cutoff,
      elapsedMs: Math.max(0, now() - started),
    };
  };
  if (expired()) return finish("time");
  const admitsBeforeDeadline = Number.isFinite(deadline)
    ? (positions: ReadonlyMap<string, GridPosition>): boolean => {
      if (now() >= deadline) throw POLISH_DEADLINE;
      return acceptsPositions(positions);
    }
    : acceptsPositions;
  try {
    const planned = planIntegralLayout({
      residents,
      nodes: [],
      edges: request.edges,
      centerId: request.centerId,
      allowExistingMoves: true,
    }, { acceptsPositions: admitsBeforeDeadline, axisGroupCompaction: false });
    passes = 1;
    adopt(planned.positions);
    observer.progressed(best, passes);
  } catch (error) {
    if (error === POLISH_DEADLINE) return finish("time");
    failed = true;
  }
  try {
    if (best.quality.linkCrossings > 0 && !expired()) {
      adopt(repairIntegralLayoutCrossingsDeep({ ...request, trace: undefined }, best, {
        maximumWork: EARLY_LANE_CROSSING_WORK,
        shouldCancel: expired,
        acceptsPositions,
        axisGroupCompaction: false,
      }).plan.positions);
    }
  } catch {
    // Optional polish never discards the best plan it already holds.
    failed = true;
  }
  return finish(expired() ? "time" : failed ? "error" : "passes");
}

/** Code-unit id order keeps model and trace ordering identical across ICU locales. */
function compareRoomIds(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function layoutModelFromPlan(
  request: IntegralLayoutRequest,
  plan: IntegralLayoutPlan,
): LayoutModel {
  const movableById = new Map(request.residents.map((resident) => [resident.id, resident.movable]));
  return {
    rooms: [...plan.positions]
      .sort(([a], [b]) => compareRoomIds(a, b))
      .map(([id, position]) => ({
        id,
        position,
        movable: movableById.get(id) ?? true,
      })),
    edges: request.edges,
  };
}

/**
 * Apply the same multi-anchor fixed-point tournament as `nf reflow` to the
 * constraint winner. A complete tournament which finds no strict public
 * quality improvement is the geometric fixed-point proof for this heuristic.
 */
function polishConstraintLayoutToFixedPoint(
  request: IntegralLayoutRequest,
  seed: IntegralLayoutPlan,
  trace: ((event: LayoutTraceEvent) => void) | undefined,
  progress: ConstraintPolishProgress,
  now: () => number = () => performance.now(),
): ConstraintPolishResult {
  const started = now();
  let winner = seed;
  let tournaments = 0;
  let passes = 0;
  let anchorsTried = 0;
  let improvements = 0;

  const publishProgress = (): void => trace?.({
    type: "constraint-progress",
    stage: "constraint-repair",
    phase: "polish",
    restarts: progress.restarts,
    feasibilityChecks: progress.feasibilityChecks,
    layoutsConsidered: progress.constraintLayoutsConsidered + passes,
    compactionAttempts: progress.compactionAttempts,
    elapsedMs: Math.max(0, now() - progress.repairStarted),
    bestQuality: winner.quality,
    ...progress.workStats,
  });
  const finish = (
    cutoff: ConstraintPolishResult["cutoff"],
    fixedPoint: boolean,
  ): ConstraintPolishResult => ({
    plan: winner,
    tournaments,
    passes,
    anchorsTried,
    improvements,
    fixedPoint,
    cutoff,
    elapsedMs: Math.max(0, now() - started),
  });
  const adopt = (
    _quality: IntegralLayoutPlan["quality"],
    positions: ReadonlyMap<string, GridPosition>,
  ): void => {
    if (progress.acceptsPositions && !progress.acceptsPositions(positions)) return;
    const quality = measureIntegralLayoutQuality(positions, request.edges);
    if (compareLayoutQuality(quality, winner.quality) <= 0) return;
    // A nested pass pins its anchor and measures movement from its own seed;
    // the published winner is finished as the request moves rooms.
    winner = finishedPlan(request, {
      positions: new Map(positions),
      movedExisting: recomputeMovedExisting(request, positions),
      quality: { ...quality },
    }, progress.acceptsPositions);
    improvements += 1;
    trace?.({
      type: "constraint-improvement",
      stage: "constraint-repair",
      restarts: progress.restarts,
      feasibilityChecks: progress.feasibilityChecks,
      layoutsConsidered: progress.constraintLayoutsConsidered + passes,
      compactionAttempts: progress.compactionAttempts,
      ...progress.workStats,
      candidate: {
        quality: { ...winner.quality },
        movedExisting: [...winner.movedExisting].sort(),
        positions: [...winner.positions]
          .sort(([a], [b]) => compareRoomIds(a, b))
          .map(([id, position]) => ({ id, ...position })),
      },
    });
  };

  try {
    if (Number.isFinite(progress.deadline) && now() >= progress.deadline) {
      publishProgress();
      return finish("time", false);
    }
    for (;;) {
      // Charge only complete deterministic tournaments. Check before entering
      // the nested planner so a limit of N never begins tournament N + 1 and
      // the last complete best-so-far plan remains publishable.
      if (tournaments >= progress.maximumTournaments) {
        publishProgress();
        return finish("tournaments", false);
      }
      if (passes >= progress.maximumPasses) {
        publishProgress();
        return finish("passes", false);
      }
      const beforeTournament = winner;
      const passesBeforeTournament = passes;
      const planTournament = (): ReturnType<typeof planLayoutModel> => planLayoutModel(
        layoutModelFromPlan(request, beforeTournament),
        { type: "reflow", anchor: request.centerId },
        {
          effort: "thorough",
          allowExistingMoves: true,
          maxPlanningPasses: Number.isFinite(progress.maximumPasses)
            ? Math.max(1, progress.maximumPasses - passes)
            : Number.POSITIVE_INFINITY,
          trace: (event) => {
            // The nested planner only exposes synchronous trace callbacks, so
            // throw before inspecting/filtering an event once a finite polish
            // deadline has elapsed. The outer catch retains `winner`, which is
            // always a complete plan adopted before this sentinel was raised.
            if (Number.isFinite(progress.deadline) && now() >= progress.deadline) {
              throw POLISH_DEADLINE;
            }
            if (event.type !== "selection" || event.stage !== "final-selection") return;
            passes += 1;
            if (event.selected.positions) {
              adopt(
                event.selected.quality,
                new Map(event.selected.positions.map(({ id, x, y, level }) => [
                  id,
                  { x, y, level },
                ])),
              );
            }
            publishProgress();
          },
        },
      );
      const planned = progress.acceptsPositions
        ? withIntegralLayoutCandidateAdmission(progress.acceptsPositions, planTournament)
        : planTournament();
      if (Number.isFinite(progress.deadline) && now() >= progress.deadline) {
        throw POLISH_DEADLINE;
      }
      anchorsTried += planned.search?.anchorsTried.length ?? 1;
      const reportedPasses = planned.search?.planningPasses ?? 1;
      // Selection events normally charge every completed nested pass live.
      // Reconcile defensively in case a future planner omits those diagnostics.
      passes = Math.max(passes, passesBeforeTournament + reportedPasses);
      const plannedQuality = measureIntegralLayoutQuality(planned.positions, request.edges);
      // The returned tournament winner also carries its deterministic
      // movement-count tie-break, so prefer it when public quality ties the
      // best progressive candidate observed inside the tournament. It is
      // finished as a published winner is, so a tournament's winner is always
      // at the cheap compaction fixed point.
      if ((!progress.acceptsPositions || progress.acceptsPositions(planned.positions)) &&
        compareLayoutQuality(plannedQuality, winner.quality) >= 0) {
        winner = finishedPlan(request, {
          positions: new Map(planned.positions),
          movedExisting: recomputeMovedExisting(request, planned.positions),
          quality: plannedQuality,
        }, progress.acceptsPositions);
      }
      if (planned.search?.completed === false) {
        publishProgress();
        return finish("passes", false);
      }
      tournaments += 1;
      if (compareLayoutQuality(winner.quality, beforeTournament.quality) <= 0) {
        publishProgress();
        return {
          plan: winner,
          tournaments,
          passes,
          anchorsTried,
          improvements,
          fixedPoint: true,
          cutoff: "fixed-point",
          elapsedMs: Math.max(0, now() - started),
        };
      }
      // Infinity is the NukeFire deep-search policy. Finite callers poll
      // cooperatively before nested trace events and at planner boundaries,
      // retaining the last complete winner when their deadline elapses.
      if (now() >= progress.deadline) {
        publishProgress();
        return finish("time", false);
      }
    }
  } catch (error) {
    if (error === POLISH_DEADLINE) {
      try {
        publishProgress();
      } catch {
        // Progress observers are request-local and cannot change the cutoff.
      }
      return finish("time", false);
    }
    // Constraint repair already produced a valid complete layout. A failure in
    // optional geometric polish must not discard that accepted best-so-far.
    return {
      plan: winner,
      tournaments,
      passes,
      anchorsTried,
      improvements,
      fixedPoint: false,
      cutoff: "error",
      elapsedMs: Math.max(0, now() - started),
    };
  }
}

function repairIntegralLayoutConstraintsWithRuntime(
  request: IntegralLayoutRequest,
  standard: IntegralLayoutPlan,
  options: ConstraintRepairOptions,
  trace?: (event: LayoutTraceEvent) => void,
  runtime: RepairRuntimeOptions = {},
): IntegralLayoutPlan {
  const counts = beforeViolationCounts(request, standard);
  const {
    beforeViolations,
    beforeRoutingViolations,
    beforeSettledViolations,
    standardSettledViolations,
    beforeSettledRoutingViolations,
    standardSettledRoutingViolations,
  } = counts;
  // Every return without a searched result still reports why, both on the
  // plan and, like a searched report, to the trace.
  const unsearched = (
    outcome: Exclude<ConstraintRepairOutcome, "searched">,
    work?: Partial<ConstraintRepairReport>,
  ): IntegralLayoutPlan => {
    const report = unsearchedConstraintRepairReport(outcome, options, standard, counts, work);
    trace?.({ type: "constraint-repair", stage: "constraint-repair", report });
    return { ...standard, constraintRepair: report };
  };
  if (request.allowExistingMoves === false) return unsearched("locked");
  const levels = options.when === "settled-regression" || options.when === "violation-regression"
    ? beforeLevelViolationCounts(request, standard)
    : undefined;
  if (options.when === "settled-regression" && levels && !violationsRegress(
    {
      directional: standardSettledViolations,
      level: levels.standardSettled,
      routing: standardSettledRoutingViolations,
    },
    {
      directional: beforeSettledViolations,
      level: levels.beforeSettled,
      routing: beforeSettledRoutingViolations,
    },
  )) return unsearched("no-regression");
  if (options.when === "violation-regression" && levels && !violationsRegress(
    {
      directional: standard.quality.cardinalRayViolations,
      level: standard.quality.levelViolations ?? 0,
      routing: standard.quality.routingViolations,
    },
    { directional: beforeViolations, level: levels.before, routing: beforeRoutingViolations },
  )) return unsearched("no-regression");
  if (options.when !== "always" && standard.quality.cardinalRayViolations === 0 &&
    standard.quality.routingViolations === 0 && standard.quality.linkCrossings === 0) {
    return unsearched("clean");
  }
  const graph = compileGraph(standard.positions, request.edges);
  if (graph.edgeCount === 0) return unsearched("no-constraints");
  if ((options.maxDurationMs ?? DEFAULT_MAX_DURATION_MS) <= 0) return unsearched("no-budget");
  const now = runtime.now ?? (() => performance.now());
  const repairStarted = now();
  const requestedDuration = options.maxDurationMs ?? DEFAULT_MAX_DURATION_MS;
  const duration = Number.isFinite(requestedDuration)
    ? Math.max(0, requestedDuration)
    : requestedDuration === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : 0;
  const deadline = repairStarted + duration;
  const workStats = emptyConstraintWorkStats();
  // `layoutsConsidered` in progress is an operation-wide work counter. Keep
  // completed unrestricted-polish passes as an explicit offset so returning
  // to mask compaction after a dynamic-mask discovery cannot make it regress.
  let progressPolishPasses = 0;
  const emitProgress = (progress: Parameters<NonNullable<SearchRuntimeOptions["progress"]>>[0]): void => {
    const enriched = {
      ...progress,
      ...workStats,
      layoutsConsidered: progress.layoutsConsidered + progressPolishPasses,
      bestQuality: progress.bestQuality ?? winner.quality,
    };
    runtime.progress?.(enriched);
    trace?.({ type: "constraint-progress", stage: "constraint-repair", ...enriched });
  };
  const requestedLayouts = Math.floor(options.maxLayouts ?? 1);
  const maximumLayouts = Number.isFinite(requestedLayouts)
    ? Math.max(1, requestedLayouts)
    : requestedLayouts === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : 1;
  const requestedPolishTournaments = Math.floor(
    options.maxPolishTournaments ?? Number.POSITIVE_INFINITY,
  );
  const maximumPolishTournaments = Number.isFinite(requestedPolishTournaments)
    ? Math.max(0, requestedPolishTournaments)
    : Number.POSITIVE_INFINITY;
  const requestedPolishPasses = Math.floor(
    options.maxPolishPasses ?? Number.POSITIVE_INFINITY,
  );
  const maximumPolishPasses = Number.isFinite(requestedPolishPasses)
    ? Math.max(0, requestedPolishPasses)
    : requestedPolishPasses === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : 0;
  const requestedExtensionStates = Math.floor(
    options.maxExtensionStates ?? DEFAULT_MAX_EXTENSION_STATES,
  );
  const maximumExtensionStates = Number.isFinite(requestedExtensionStates)
    ? Math.max(0, requestedExtensionStates)
    : requestedExtensionStates === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : DEFAULT_MAX_EXTENSION_STATES;
  const requestedLiveSearchNodes = Math.floor(
    options.maxLiveSearchNodes ?? DEFAULT_MAX_LIVE_SEARCH_NODES,
  );
  const maximumLiveSearchNodes = Number.isFinite(requestedLiveSearchNodes)
    ? Math.max(1, requestedLiveSearchNodes)
    : requestedLiveSearchNodes === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : DEFAULT_MAX_LIVE_SEARCH_NODES;
  const requestedMaskDiversifications = Math.floor(
    options.maxMaskDiversifications ?? DEFAULT_MAX_MASK_DIVERSIFICATIONS,
  );
  const maximumMaskDiversifications = Number.isFinite(requestedMaskDiversifications)
    ? Math.max(1, requestedMaskDiversifications)
    : requestedMaskDiversifications === Number.POSITIVE_INFINITY
    ? Number.POSITIVE_INFINITY
    : DEFAULT_MAX_MASK_DIVERSIFICATIONS;
  let winner = standard;
  let winnerRemovedGroups = removedGroupsForPositions(graph, standard.positions);
  let winnerRelaxedScore: RelaxationScore = removedScore(graph, winnerRemovedGroups);
  let winnerSignature = constraintPositionSignature(graph, winner.positions) as ConstraintPositionSignature;
  let searchRestarts = 0;
  let searchFeasibilityChecks = 0;
  const movableById = new Map(request.residents.map((resident) => [resident.id, resident.movable]));
  const fixedIds = new Set(
    request.residents.filter((resident) => !resident.movable).map((resident) => resident.id),
  );
  let layoutsConsidered = 0;
  let layoutFrontierTruncated = false;
  let compactionAttempts = 0;
  let lastCompactionProgressAt = repairStarted;
  let compactionMs = 0;
  let timedOut = false;
  const perfect = (plan: IntegralLayoutPlan): boolean =>
    plan.quality.cardinalRayViolations === 0 &&
    plan.quality.routingViolations === 0 &&
    plan.quality.linkCrossings === 0;
  interface DistinctLayout {
    signature: ConstraintPositionSignature;
    plan: IntegralLayoutPlan;
    relaxedScore: RelaxationScore;
    removedGroups: Uint8Array;
    ordinal: number;
    polished: boolean;
  }
  const polishFrontier: DistinctLayout[] = [];
  const maximumSeenSignatures = Number.isFinite(maximumExtensionStates)
    ? Math.max(64, Math.min(2_048, maximumExtensionStates))
    : 2_048;
  // Hashes keep the common rejection path small. A separate exact-witness
  // ring covers repeated hashes without trusting them: it is capped at one
  // MiB of coordinate scalars, and an unwitnessed repeat is classified as
  // ambiguous rather than suppressing a potentially distinct layout.
  const seenSignatureKeys = new Set<string>();
  const seenSignatureOrder: string[] = [];
  let nextSignatureEviction = 0;
  const MAX_EXACT_POSITION_WITNESS_COORDINATES = 131_072;
  const exactPositionWitnessCapacity = Math.min(
    maximumSeenSignatures,
    Math.floor(MAX_EXACT_POSITION_WITNESS_COORDINATES / Math.max(1, graph.nodeCount * 3)),
  );
  const exactPositionWitnesses = new Map<string, ConstraintPositionSignature[]>();
  const exactPositionWitnessOrder: ConstraintPositionSignature[] = [];
  let nextExactPositionWitnessEviction = 0;
  let nextLayoutOrdinal = 0;
  const rememberPositionHash = (signature: Readonly<ConstraintPositionHash>): boolean => {
    const key = constraintPositionSignatureKey(signature);
    if (seenSignatureKeys.has(key)) return false;
    seenSignatureKeys.add(key);
    if (seenSignatureOrder.length < maximumSeenSignatures) {
      seenSignatureOrder.push(key);
    } else {
      seenSignatureKeys.delete(seenSignatureOrder[nextSignatureEviction]);
      seenSignatureOrder[nextSignatureEviction] = key;
      nextSignatureEviction = (nextSignatureEviction + 1) % maximumSeenSignatures;
    }
    return true;
  };
  const removeExactPositionWitness = (signature: ConstraintPositionSignature): void => {
    const key = constraintPositionSignatureKey(signature);
    const bucket = exactPositionWitnesses.get(key);
    if (!bucket) return;
    const index = bucket.indexOf(signature);
    if (index >= 0) bucket.splice(index, 1);
    if (bucket.length === 0) exactPositionWitnesses.delete(key);
  };
  const storeExactPositionWitness = (signature: ConstraintPositionSignature): void => {
    if (exactPositionWitnessCapacity === 0) return;
    const key = constraintPositionSignatureKey(signature);
    const bucket = exactPositionWitnesses.get(key) ?? [];
    bucket.push(signature);
    exactPositionWitnesses.set(key, bucket);
    if (exactPositionWitnessOrder.length < exactPositionWitnessCapacity) {
      exactPositionWitnessOrder.push(signature);
    } else {
      removeExactPositionWitness(exactPositionWitnessOrder[nextExactPositionWitnessEviction]);
      exactPositionWitnessOrder[nextExactPositionWitnessEviction] = signature;
      nextExactPositionWitnessEviction =
        (nextExactPositionWitnessEviction + 1) % exactPositionWitnessCapacity;
    }
  };
  const classifyExactPositionSignature = (
    signature: ConstraintPositionSignature,
  ): "new" | "duplicate" | "ambiguous" => {
    const key = constraintPositionSignatureKey(signature);
    const witnesses = exactPositionWitnesses.get(key);
    if (witnesses?.some((known) => sameConstraintPositionSignature(known, signature))) {
      return "duplicate";
    }
    const knownHash = seenSignatureKeys.has(key) || witnesses !== undefined;
    rememberPositionHash(signature);
    storeExactPositionWitness(signature);
    return knownHash ? "ambiguous" : "new";
  };
  const publishImprovement = (plan: IntegralLayoutPlan): void => trace?.({
    type: "constraint-improvement",
    stage: "constraint-repair",
    restarts: searchRestarts,
    feasibilityChecks: searchFeasibilityChecks,
    layoutsConsidered: layoutsConsidered + progressPolishPasses,
    compactionAttempts,
    ...workStats,
    candidate: {
      quality: { ...plan.quality },
      movedExisting: [...plan.movedExisting].sort(),
      positions: [...plan.positions]
        .sort(([a], [b]) => compareRoomIds(a, b))
        .map(([id, position]) => ({ id, ...position })),
    },
  });
  const retainForPolish = (entry: DistinctLayout): void => {
    polishFrontier.push(entry);
    polishFrontier.sort((a, b) =>
      compareLayoutQuality(b.plan.quality, a.plan.quality) || a.ordinal - b.ordinal
    );
    if (polishFrontier.length <= maximumLayouts) return;
    polishFrontier.pop();
    layoutFrontierTruncated = true;
  };
  const considerRawIncumbent = (
    candidate: {
      dense?: readonly (readonly [number, number, number])[];
      hash?: ConstraintPositionHash;
      materialize: () => Map<string, GridPosition>;
    },
    relaxedScore: RelaxationScore,
    removedGroups: Uint8Array,
    measuredQuality?: Readonly<LayoutQuality>,
  ): void => {
    let positions: Map<string, GridPosition> | undefined;
    const materialize = (): Map<string, GridPosition> => positions ??= candidate.materialize();
    if (!candidate.dense &&
      !hardValidLayoutPositions(graph, removedGroups, materialize(), fixedIds)) return;
    workStats.rawIncumbents += 1;
    workStats.firstIncumbentMs ??= Math.max(0, now() - repairStarted);
    const quality = measuredQuality ?? measureIntegralLayoutQuality(materialize(), request.edges);
    const canImproveWinner = compareLayoutQuality(quality, winner.quality) >= 0;
    const worstRetained = polishFrontier.at(-1);
    const canEnterPolishFrontier = polishFrontier.length < maximumLayouts ||
      !!worstRetained && compareLayoutQuality(quality, worstRetained.plan.quality) > 0;
    const denseHash = candidate.dense
      ? (runtime.densePositionHash?.(graph, candidate.dense) ??
        candidate.hash ?? denseConstraintPositionHash(graph, candidate.dense))
      : undefined;
    // Frontier quality only improves as search proceeds. Once full, a strictly
    // worse state can neither become the winner nor survive the bounded top-K.
    // Preserve distinct-layout reporting with its compact hash only.
    if (!canImproveWinner && !canEnterPolishFrontier) {
      const hash = denseHash ?? constraintPositionSignature(graph, materialize());
      if (!hash) return;
      if (rememberPositionHash(hash)) {
        workStats.distinctLayouts += 1;
        layoutFrontierTruncated = true;
        return;
      }
      // Pay for coordinates only on a repeated hash. A retained exact witness
      // proves a harmless duplicate; otherwise collision/eviction ambiguity
      // conservatively withholds the fixed-point classification.
      const exact = candidate.dense
        ? denseConstraintPositionSignature(graph, candidate.dense, hash)
        : constraintPositionSignature(graph, materialize());
      const witnesses = exact && exactPositionWitnesses.get(constraintPositionSignatureKey(hash));
      if (exact && witnesses?.some((known) => sameConstraintPositionSignature(known, exact))) return;
      layoutFrontierTruncated = true;
      return;
    }
    const signature = candidate.dense
      ? denseConstraintPositionSignature(graph, candidate.dense, denseHash)
      : constraintPositionSignature(graph, materialize());
    if (!signature) return;
    if (sameConstraintPositionSignature(signature, winnerSignature) &&
      betterScore(relaxedScore, winnerRelaxedScore)) {
      winnerRelaxedScore = relaxedScore;
      winnerRemovedGroups = removedGroups.slice();
    }
    const retained = polishFrontier.find((entry) =>
      sameConstraintPositionSignature(entry.signature, signature)
    );
    if (retained) {
      if (betterScore(relaxedScore, retained.relaxedScore)) {
        retained.relaxedScore = relaxedScore;
        retained.removedGroups = removedGroups.slice();
      }
      return;
    }
    const signatureDisposition = classifyExactPositionSignature(signature);
    if (signatureDisposition === "duplicate") return;
    if (signatureDisposition === "ambiguous") {
      // Winner/frontier exact comparisons above found no duplicate. Continue
      // admitting this candidate, but withhold fixed-point claims because an
      // evicted hash-only witness cannot prove which prior geometry collided.
      layoutFrontierTruncated = true;
    }
    workStats.distinctLayouts += 1;
    // Only admitted, distinct layouts pay for full Map retention, movement
    // reconstruction, and any stable trace ordering.
    const copied = new Map(materialize());
    const plan: IntegralLayoutPlan = {
      positions: copied,
      movedExisting: recomputeMovedExisting(request, copied),
      // The production compactor supplies a freshly measured complete public
      // score. Test seams and alternate compactors are scored here instead;
      // incremental/private scores never cross this boundary.
      quality: { ...quality },
    };
    retainForPolish({
      signature,
      plan,
      relaxedScore,
      removedGroups: removedGroups.slice(),
      ordinal: nextLayoutOrdinal++,
      polished: false,
    });
    if (compareLayoutQuality(plan.quality, winner.quality) > 0) {
      let published = plan;
      const acceptsCompactedPositions = (positions: ReadonlyMap<string, GridPosition>): boolean =>
        hardValidLayoutPositions(graph, removedGroups, positions, fixedIds);
      try {
        const compacted = (runtime.gravity ?? compactIntegralLayoutPlan)({
          ...request,
          trace: undefined,
        }, plan, {
          acceptsPositions: acceptsCompactedPositions,
          axisGroupCompaction: packsAxisGroups(request),
          shouldCancel: () => Number.isFinite(deadline) && now() >= deadline,
        });
        const compactedQuality = measureIntegralLayoutQuality(compacted.positions, request.edges);
        if (acceptsCompactedPositions(compacted.positions) &&
          compareLayoutQuality(compactedQuality, plan.quality) >= 0) {
          published = {
            positions: new Map(compacted.positions),
            movedExisting: recomputeMovedExisting(request, compacted.positions),
            quality: compactedQuality,
          };
        }
      } catch {
        // A compaction-only aesthetic pass must never retract a freshly scored,
        // hard-valid raw improvement. The ordinary frontier can still polish it.
      }
      // A full compaction ends at the cheap fixed point already; one the
      // deadline cut short, that failed, or that its mask did not admit
      // leaves the raw layout, which is finished here instead.
      published = finishedPlan(request, published, acceptsCompactedPositions);
      winner = published;
      winnerRelaxedScore = relaxedScore;
      winnerRemovedGroups = removedGroups.slice();
      winnerSignature = constraintPositionSignature(graph, winner.positions) as ConstraintPositionSignature;
      workStats.softIncumbents += 1;
      publishImprovement(winner);
    }
  };

  interface MaskQueueNode {
    mask: Uint8Array;
    next?: MaskQueueNode;
  }
  let geometryHead: MaskQueueNode | undefined;
  let geometryTail: MaskQueueNode | undefined;
  let searchHead: MaskQueueNode | undefined;
  let searchTail: MaskQueueNode | undefined;
  let pendingMasks = 0;
  let knownMaskCount = 0;
  const knownMaskBuckets = new Map<number, Uint8Array[]>();
  const processedMaskBuckets = new Map<number, Uint8Array[]>();
  const rememberProcessedMask = (mask: Uint8Array): void => {
    const hash = constraintMaskHash(mask);
    const bucket = processedMaskBuckets.get(hash);
    if (bucket) bucket.push(mask.slice());
    else processedMaskBuckets.set(hash, [mask.slice()]);
  };
  const wasProcessedMask = (mask: Uint8Array): boolean => {
    const hash = constraintMaskHash(mask);
    return processedMaskBuckets.get(hash)?.some((known) => sameConstraintMask(known, mask)) === true;
  };
  let extensionCompleted = true;
  let extensionExhausted = false;
  let extensionCancelled = false;
  let maskExhausted = false;
  let fatalCompactionFailure = false;
  const stageMask = (
    mask: Uint8Array,
    geometry: boolean,
    restageKnownUnprocessed = false,
  ): boolean => {
    const hash = constraintMaskHash(mask);
    const known = knownMaskBuckets.get(hash)?.some((candidate) =>
      sameConstraintMask(candidate, mask)
    ) === true;
    if (known) {
      // A perfect interim winner can clear queued lower-priority masks. If a
      // later unrestricted polish selects one of those masks, closure must be
      // able to put that already-counted mask back on the geometry queue.
      if (!restageKnownUnprocessed || wasProcessedMask(mask)) return true;
    } else {
      if (knownMaskCount >= maximumMaskDiversifications) {
        maskExhausted = true;
        return false;
      }
      const copied = mask.slice();
      const bucket = knownMaskBuckets.get(hash);
      if (bucket) bucket.push(copied);
      else knownMaskBuckets.set(hash, [copied]);
      knownMaskCount += 1;
    }
    const copied = mask.slice();
    const node: MaskQueueNode = { mask: copied };
    if (geometry) {
      if (geometryTail) geometryTail.next = node;
      else geometryHead = node;
      geometryTail = node;
    } else {
      if (searchTail) searchTail.next = node;
      else searchHead = node;
      searchTail = node;
    }
    pendingMasks += 1;
    return true;
  };
  const takeMask = (): Uint8Array | undefined => {
    const node = geometryHead ?? searchHead;
    if (!node) return undefined;
    if (geometryHead) {
      geometryHead = node.next;
      if (!geometryHead) geometryTail = undefined;
    } else {
      searchHead = node.next;
      if (!searchHead) searchTail = undefined;
    }
    pendingMasks -= 1;
    return node.mask;
  };
  const drainMasks = (): boolean => {
    while (pendingMasks > 0 && !fatalCompactionFailure) {
      // Even a zero state budget evaluates the supplied hard-valid geometry
      // for the first mask. The extension core then reports exhaustion without
      // building a separator state, preserving a truthful incumbent/report.
      if (workStats.separatorStates >= maximumExtensionStates && compactionAttempts > 0) {
        extensionCompleted = false;
        extensionExhausted = true;
        return false;
      }
      if (now() >= deadline) {
        timedOut = true;
        extensionCompleted = false;
        extensionCancelled = true;
        return false;
      }
      const removedGroups = takeMask() as Uint8Array;
      rememberProcessedMask(removedGroups);
      const relaxedScore = removedScore(graph, removedGroups);
      compactionAttempts += 1;
      workStats.maskDiversifications += 1;
      const compactStarted = now();
      const rawBefore = workStats.rawIncumbents;
      const enqueueEqualPrimarySwaps = (relationGroups: readonly number[]): void => {
        const defectGroups = [...new Set(relationGroups)]
          .filter((group) =>
            group >= 0 && group < graph.groupCount && !groupRelaxed(graph, removedGroups, group)
          )
          .sort((a, b) => a - b);
        const restoredGroups = Array.from(removedGroups, (_removed, group) => group)
          .filter((group) => removedGroups[group] !== 0);
        // A swap trades a directional group for another of equal weight; a
        // level relation, whose third weight differs, is never swapped out.
        for (const defect of defectGroups) {
          for (const restored of restoredGroups) {
            if (graph.groupSourceCount[defect] !== graph.groupSourceCount[restored] ||
              graph.groupReciprocalCount[defect] !== graph.groupReciprocalCount[restored] ||
              graph.groupLevelCount[defect] !== graph.groupLevelCount[restored]) continue;
            const mask = removedGroups.slice();
            mask[defect] = 1;
            mask[restored] = 0;
            stageMask(mask, true);
          }
        }
      };
      let compactionStatus: { completed: boolean; cancelled: boolean; exhausted: boolean } = {
        completed: true,
        cancelled: false,
        exhausted: false,
      };
      const compactedResult = runtime.compact
        ? runtime.compact(graph, removedGroups)
        : compactConstraints(graph, removedGroups, fixedIds, {
          maximumStates: maximumExtensionStates - workStats.separatorStates,
          maximumLiveSearchNodes,
          shouldCancel: () => Number.isFinite(deadline) && now() >= deadline,
          score: (positions) => measureIntegralLayoutQuality(positions, request.edges),
          workStats,
          onIncumbent: (candidate, quality) =>
            considerRawIncumbent(candidate, relaxedScore, removedGroups, quality),
          onDiversification: enqueueEqualPrimarySwaps,
          softDefectHash: runtime.softDefectHash,
          onFinish: (status) => {
            compactionStatus = status;
          },
          onProgress: () => {
            const progressAt = now();
            if (progressAt - lastCompactionProgressAt < PROGRESS_INTERVAL_MS) return;
            lastCompactionProgressAt = progressAt;
            emitProgress({
              ...workStats,
              phase: "compaction",
              restarts: searchRestarts,
              feasibilityChecks: searchFeasibilityChecks,
              layoutsConsidered,
              compactionAttempts,
              elapsedMs: Math.max(0, progressAt - repairStarted),
              bestQuality: winner.quality,
            });
          },
        });
      compactionMs += Math.max(0, now() - compactStarted);
      extensionCompleted &&= compactionStatus.completed;
      extensionExhausted ||= compactionStatus.exhausted;
      extensionCancelled ||= compactionStatus.cancelled;
      if (compactionStatus.cancelled && Number.isFinite(deadline) && now() >= deadline) {
        timedOut = true;
      }
      if (runtime.compact && compactedResult.ok) {
        considerRawIncumbent(
          { materialize: () => new Map(compactedResult.value) },
          relaxedScore,
          removedGroups,
        );
      }
      if (!compactedResult.ok && runtime.compact) {
        fatalCompactionFailure = true;
        extensionCompleted = false;
      }
      const progressAt = now();
      if ((compactionAttempts & COMPACTION_PROGRESS_CHECK_MASK) === 0 &&
        (workStats.rawIncumbents !== rawBefore ||
          progressAt - lastCompactionProgressAt >= PROGRESS_INTERVAL_MS)) {
        lastCompactionProgressAt = progressAt;
        emitProgress({
          ...workStats,
          phase: "compaction",
          restarts: searchRestarts,
          feasibilityChecks: searchFeasibilityChecks,
          layoutsConsidered,
          compactionAttempts,
          elapsedMs: Math.max(0, progressAt - repairStarted),
          bestQuality: winner.quality,
        });
      }
      if (perfect(winner)) {
        geometryHead = geometryTail = searchHead = searchTail = undefined;
        pendingMasks = 0;
        return false;
      }
    }
    return !fatalCompactionFailure && !extensionExhausted && !extensionCancelled && !maskExhausted &&
      !perfect(winner);
  };

  const acceptsAnyHardValidPositions = (
    positions: ReadonlyMap<string, GridPosition>,
  ): boolean => acceptsAnyHardValidLayoutPositions(graph, positions, fixedIds);
  let earlyPolished: ConstraintPolishResult = {
    plan: winner,
    tournaments: 0,
    passes: 0,
    anchorsTried: 0,
    improvements: 0,
    fixedPoint: false,
    cutoff: "tournaments",
    elapsedMs: 0,
  };
  let earlyWinnerRetained = false;
  let earlyWinnerMask: Uint8Array | undefined;
  let earlyFixedPointSignature: ConstraintPositionSignature | undefined;
  // One cheap planner pass and a small crossing repair give the anytime lane a
  // useful incumbent before the exact master search spends minutes certifying
  // masks. The pass is charged against the aggregate pass ceiling that every
  // later tournament shares, so it cannot starve MaxHS or add work.
  // Custom search seams retain their historical phase isolation in tests.
  if (!runtime.search && !runtime.compact && !runtime.polish &&
    maximumPolishTournaments > 0 && maximumPolishPasses > 0) {
    earlyPolished = polishConstraintLayoutEarly(
      request,
      winner,
      graph.ids.map((id) => ({
        id,
        position: winner.positions.get(id) as GridPosition,
        movable: movableById.get(id) ?? true,
      })),
      acceptsAnyHardValidPositions,
      deadline,
      now,
      {
        improved: (plan, passes) => {
          progressPolishPasses = passes;
          publishImprovement(plan);
        },
        progressed: (plan, passes) => trace?.({
          type: "constraint-progress",
          stage: "constraint-repair",
          phase: "polish",
          restarts: 0,
          feasibilityChecks: 0,
          layoutsConsidered: passes,
          compactionAttempts,
          elapsedMs: Math.max(0, now() - repairStarted),
          bestQuality: plan.quality,
          ...workStats,
        }),
      },
    );
    const earlyQuality = measureIntegralLayoutQuality(earlyPolished.plan.positions, request.edges);
    const earlyMask = removedGroupsForPositions(graph, earlyPolished.plan.positions);
    if (hardValidLayoutPositions(graph, earlyMask, earlyPolished.plan.positions, fixedIds) &&
      compareLayoutQuality(earlyQuality, winner.quality) > 0) {
      winner = {
        positions: new Map(earlyPolished.plan.positions),
        movedExisting: recomputeMovedExisting(request, earlyPolished.plan.positions),
        quality: earlyQuality,
      };
      winnerRemovedGroups = earlyMask;
      winnerRelaxedScore = removedScore(graph, earlyMask);
      winnerSignature = constraintPositionSignature(graph, winner.positions) as ConstraintPositionSignature;
      earlyWinnerRetained = true;
      earlyWinnerMask = earlyMask;
    }
    progressPolishPasses = earlyPolished.passes;
    const earlySignature = constraintPositionSignature(graph, earlyPolished.plan.positions);
    if (earlyPolished.fixedPoint && earlySignature &&
      sameConstraintPositionSignature(earlySignature, winnerSignature)) {
      earlyFixedPointSignature = earlySignature;
    }
    if (earlyPolished.cutoff === "time") timedOut = true;
  }

  const remainingSearchDuration = Number.isFinite(deadline)
    ? Math.max(0, deadline - now())
    : Number.POSITIVE_INFINITY;
  const searchOptions = remainingSearchDuration === options.maxDurationMs
    ? options
    : { ...options, maxDurationMs: remainingSearchDuration };
  const searchStarted = now();
  const searchedResult = (runtime.search ?? constraintSearch)(graph, searchOptions, standard.positions, {
    ...runtime,
    progress: (progress) => {
      searchRestarts = progress.restarts;
      searchFeasibilityChecks = progress.feasibilityChecks;
      emitProgress(progress);
    },
    mask: (mask, progress) => {
      searchRestarts = progress.restarts;
      searchFeasibilityChecks = progress.feasibilityChecks;
      // Master search owns proof/certification. Geometry is deliberately
      // deferred until it returns, so a difficult crude mask cannot consume
      // the entire extension budget before MaxHS discovers its best mask.
      return true;
    },
  });
  if (!searchedResult.ok && searchedResult.reason === "time") timedOut = true;
  // The early preview pass runs before the search; a repair that stops after
  // it still counts its work.
  const previewWork: Partial<ConstraintRepairReport> = {
    polishTournaments: earlyPolished.tournaments,
    polishPasses: earlyPolished.passes,
    polishAnchorsTried: earlyPolished.anchorsTried,
    polishImprovements: earlyPolished.improvements,
    polishMs: earlyPolished.elapsedMs,
  };
  // Production search converts time/work limits to a partial success, but an
  // unexpected late analysis failure must not retract a complete improvement
  // that a streamed feasible mask already published. Retain it with an
  // explicitly non-optimal search termination. A failure before any incumbent
  // returns the standard plan and reports why: a compaction failure as no
  // layout, and a spent check budget as the "restarts" ceiling a completed
  // search reports for it.
  if (!searchedResult.ok && !earlyWinnerRetained) {
    const reason = searchedResult.reason;
    return unsearched(reason === "compaction" ? "no-layout" : `search-failed:${reason}`, {
      ...workStats,
      ...previewWork,
      cutoff: timedOut ? "time" : reason === "work" ? "restarts" : "none",
      restarts: searchRestarts,
      feasibilityChecks: searchFeasibilityChecks,
      searchMs: Math.max(0, now() - searchStarted),
    });
  }
  const searched: ConstraintSearchResult = searchedResult.ok
    ? searchedResult.value
    : {
      removed: winnerRemovedGroups.slice(),
      masks: [],
      score: winnerRelaxedScore,
      lowerBound: 0,
      optimal: false,
      cutoff: timedOut ? "time" : "restarts",
      restarts: searchRestarts,
      feasibilityChecks: searchFeasibilityChecks,
      elapsedMs: Math.max(0, now() - searchStarted),
    };
  searchRestarts = searched.restarts;
  searchFeasibilityChecks = searched.feasibilityChecks;
  // `constraintSearch` returns masks best-first. Stage that certified order
  // before the early heuristic mask; encounter-order callbacks are intentionally
  // not retained, so the crude initial mask cannot starve the MaxHS incumbent.
  for (const mask of searched.masks) stageMask(mask, false);
  if (earlyWinnerMask) stageMask(earlyWinnerMask, false);
  drainMasks();
  if (workStats.distinctLayouts === 0 && !earlyWinnerRetained) {
    return unsearched("no-layout", {
      ...workStats,
      ...previewWork,
      cutoff: timedOut
        ? "time"
        : extensionExhausted
        ? "extensions"
        : maskExhausted
        ? "masks"
        : searched.cutoff,
      lowerBound: searched.lowerBound,
      restarts: searched.restarts,
      feasibilityChecks: searched.feasibilityChecks,
      compactionAttempts,
      extensionSearch: {
        completed: extensionCompleted && !extensionCancelled && !extensionExhausted &&
          pendingMasks === 0,
        cancelled: extensionCancelled,
        exhausted: extensionExhausted,
      },
      maskDiversification: {
        completed: !maskExhausted && !extensionCancelled && pendingMasks === 0,
        exhausted: maskExhausted,
      },
      searchMs: searched.elapsedMs,
      compactionMs,
    });
  }

  const polishRetainedLayouts = (): void => {
    for (;;) {
      const entry = polishFrontier.find((candidate) => !candidate.polished);
      if (!entry) return;
      const reachedLayoutLimit = layoutsConsidered >= maximumLayouts;
      const reachedDeadline = now() >= deadline;
      if (reachedLayoutLimit || reachedDeadline) {
        // Set only while an actual retained entry remains unprocessed;
        // merely consuming exactly maxLayouts entries is complete.
        layoutFrontierTruncated ||= reachedLayoutLimit;
        if (reachedDeadline) timedOut = true;
        return;
      }
      // Mark before planning because candidate publication can re-sort the
      // retained frontier. The object survives that stable reordering.
      entry.polished = true;
      layoutsConsidered += 1;
      lastCompactionProgressAt = now();
      emitProgress({
        ...workStats,
        phase: "polish",
        restarts: searched.restarts,
        feasibilityChecks: searched.feasibilityChecks,
        layoutsConsidered,
        compactionAttempts,
        elapsedMs: Math.max(0, lastCompactionProgressAt - repairStarted),
        bestQuality: winner.quality,
      });
      const acceptsPolishedPositions = (positions: ReadonlyMap<string, GridPosition>): boolean =>
        hardValidLayoutPositions(graph, entry.removedGroups, positions, fixedIds);
      const ordinaryPolishTrace = layoutsConsidered === 1 && trace
        ? (event: LayoutTraceEvent): void => {
          // Quick crossing events are relative to this one local planner seed,
          // not the operation-wide public frontier. The completed plan is
          // reconsidered below and, if globally better, published once as a
          // constraint improvement. Telemetry-only crossing progress is safe.
          if (event.type !== "crossing-repair") trace(event);
        }
        : undefined;
      const candidate = planIntegralLayout({
        residents: graph.ids.map((id) => ({
          id,
          position: entry.plan.positions.get(id) as GridPosition,
          movable: movableById.get(id) ?? true,
        })),
        nodes: [],
        edges: request.edges,
        centerId: request.centerId,
        allowExistingMoves: true,
        trace: ordinaryPolishTrace,
      }, { acceptsPositions: acceptsPolishedPositions });
      const candidateQuality = measureIntegralLayoutQuality(candidate.positions, request.edges);
      if (acceptsPolishedPositions(candidate.positions) &&
        compareLayoutQuality(candidateQuality, winner.quality) > 0) {
        // The nested pass measured movement from this entry's layout; the
        // published winner is finished from the request's own rooms.
        winner = finishedPlan(request, {
          positions: new Map(candidate.positions),
          movedExisting: recomputeMovedExisting(request, candidate.positions),
          quality: candidateQuality,
        }, acceptsPolishedPositions);
        winnerRelaxedScore = entry.relaxedScore;
        winnerRemovedGroups = entry.removedGroups.slice();
        winnerSignature = constraintPositionSignature(graph, winner.positions) as ConstraintPositionSignature;
        publishImprovement(winner);
      }
    }
  };
  polishRetainedLayouts();
  const polished: ConstraintPolishResult = {
    plan: winner,
    tournaments: earlyPolished.tournaments,
    passes: earlyPolished.passes,
    anchorsTried: earlyPolished.anchorsTried,
    improvements: earlyPolished.improvements,
    fixedPoint: false,
    cutoff: earlyPolished.cutoff,
    elapsedMs: earlyPolished.elapsedMs,
  };
  // Close over masks discovered by unrestricted polish in the same bounded
  // operation. A newly selected mask is compacted before a fixed-point claim;
  // if that compaction advances the winner, another tournament starts from the
  // new basin while sharing the one configured tournament budget.
  for (;;) {
    if (earlyFixedPointSignature &&
      sameConstraintPositionSignature(earlyFixedPointSignature, winnerSignature) &&
      wasProcessedMask(winnerRemovedGroups)) {
      polished.fixedPoint = true;
      polished.cutoff = "fixed-point";
      break;
    }
    const polishSeed = winner;
    const remainingPolishTournaments = Number.isFinite(maximumPolishTournaments)
      ? Math.max(0, maximumPolishTournaments - polished.tournaments)
      : Number.POSITIVE_INFINITY;
    const remainingPolishPasses = Number.isFinite(maximumPolishPasses)
      ? Math.max(0, maximumPolishPasses - polished.passes)
      : Number.POSITIVE_INFINITY;
    const phase = (runtime.polish ?? polishConstraintLayoutToFixedPoint)(
      request,
      winner,
      trace,
      {
        repairStarted,
        deadline,
        maximumTournaments: remainingPolishTournaments,
        maximumPasses: remainingPolishPasses,
        constraintLayoutsConsidered: layoutsConsidered + progressPolishPasses,
        compactionAttempts,
        restarts: searched.restarts,
        feasibilityChecks: searched.feasibilityChecks,
        workStats,
        acceptsPositions: acceptsAnyHardValidPositions,
      },
      now,
    );
    polished.tournaments += phase.tournaments;
    polished.passes += phase.passes;
    polished.anchorsTried += phase.anchorsTried;
    polished.improvements += phase.improvements;
    polished.elapsedMs += phase.elapsedMs;
    polished.cutoff = phase.cutoff;
    polished.fixedPoint = phase.fixedPoint;
    progressPolishPasses += phase.passes;

    const phaseQuality = measureIntegralLayoutQuality(phase.plan.positions, request.edges);
    const phaseMask = removedGroupsForPositions(graph, phase.plan.positions);
    if (hardValidLayoutPositions(graph, phaseMask, phase.plan.positions, fixedIds) &&
      compareLayoutQuality(phaseQuality, polishSeed.quality) >= 0) {
      winner = {
        positions: new Map(phase.plan.positions),
        movedExisting: recomputeMovedExisting(request, phase.plan.positions),
        quality: phaseQuality,
      };
      winnerRemovedGroups = phaseMask;
      winnerRelaxedScore = removedScore(graph, phaseMask);
    } else {
      winner = polishSeed;
      winnerRemovedGroups = removedGroupsForPositions(graph, winner.positions);
      winnerRelaxedScore = removedScore(graph, winnerRemovedGroups);
    }
    winnerSignature = constraintPositionSignature(graph, winner.positions) as ConstraintPositionSignature;

    if (wasProcessedMask(winnerRemovedGroups)) break;
    const beforeDrain = winnerSignature;
    if (!stageMask(winnerRemovedGroups, true, true)) {
      polished.fixedPoint = false;
      break;
    }
    drainMasks();
    // Mask closure can discover complete layouts after the initial frontier
    // sweep. Polish every newly retained basin before claiming a fixed point;
    // a finite layout ceiling is recorded as an explicit truncation instead.
    polishRetainedLayouts();
    if (!wasProcessedMask(winnerRemovedGroups)) {
      polished.fixedPoint = false;
      break;
    }
    const winnerChanged = !sameConstraintPositionSignature(beforeDrain, winnerSignature);
    if (!winnerChanged) break;
    if (Number.isFinite(deadline) && now() >= deadline) {
      timedOut = true;
      polished.fixedPoint = false;
      polished.cutoff = "time";
      break;
    }
    if (Number.isFinite(maximumPolishTournaments) &&
      polished.tournaments >= maximumPolishTournaments) {
      polished.fixedPoint = false;
      polished.cutoff = "tournaments";
      break;
    }
    if (Number.isFinite(maximumPolishPasses) && polished.passes >= maximumPolishPasses) {
      polished.fixedPoint = false;
      polished.cutoff = "passes";
      break;
    }
  }
  polished.plan = winner;
  // Nested polish can hit the same finite repair deadline while it is inside a
  // synchronous planner callback. Carry that cooperative cutoff into the public
  // repair report even when crossing repair has no work left to cancel.
  if (earlyPolished.cutoff === "time" || polished.cutoff === "time") timedOut = true;
  const crossingStarted = now();
  const downstreamTrace = request.trace ?? trace;
  const beforeCrossing = winner;
  let publishedCrossingWinner = winner;
  const crossingTrace = downstreamTrace
    ? (event: LayoutTraceEvent): void => {
      if (event.type === "crossing-repair") {
        const positions = event.after.positions && new Map(
          event.after.positions.map(({ id, x, y, level }) => [id, { x, y, level }]),
        );
        if (!positions || !hardValidLayoutPositions(
          graph,
          winnerRemovedGroups,
          positions,
          fixedIds,
        )) return;
        const quality = measureIntegralLayoutQuality(positions, request.edges);
        if (compareLayoutQuality(quality, event.after.quality) !== 0 ||
          compareLayoutQuality(quality, publishedCrossingWinner.quality) <= 0) return;
        publishedCrossingWinner = {
          positions,
          movedExisting: recomputeMovedExisting(request, positions),
          quality,
        };
      }
      downstreamTrace(event);
    }
    : undefined;
  const crossing = repairIntegralLayoutCrossingsDeep(
    crossingTrace ? { ...request, trace: crossingTrace } : request,
    winner,
    {
      maximumWork: options.maxCrossingWork,
      shouldCancel: () => Number.isFinite(deadline) && now() >= deadline,
      acceptsPositions: (positions) =>
        hardValidLayoutPositions(graph, winnerRemovedGroups, positions, fixedIds),
    },
  );
  const crossingMs = Math.max(0, now() - crossingStarted);
  const crossingHardValid = hardValidLayoutPositions(
    graph,
    winnerRemovedGroups,
    crossing.plan.positions,
    fixedIds,
  );
  const crossingQuality = measureIntegralLayoutQuality(crossing.plan.positions, request.edges);
  if (crossingHardValid && compareLayoutQuality(crossingQuality, winner.quality) >= 0) {
    winner = {
      ...crossing.plan,
      movedExisting: recomputeMovedExisting(request, crossing.plan.positions),
      quality: crossingQuality,
    };
    winnerSignature = constraintPositionSignature(graph, winner.positions) as ConstraintPositionSignature;
  }
  // Defensive monotonic fallback: a future crossing implementation must not
  // return below a complete hard-valid candidate it already published.
  if (compareLayoutQuality(publishedCrossingWinner.quality, winner.quality) > 0) {
    winner = publishedCrossingWinner;
    winnerSignature = constraintPositionSignature(graph, winner.positions) as ConstraintPositionSignature;
  }
  const crossingImproved = compareLayoutQuality(winner.quality, beforeCrossing.quality) > 0;
  // The result ends at the cheap compaction fixed point every layout published
  // before it reached. Finishing within the winner's mask keeps the relaxation
  // the report describes; a strict gain is new geometry no tournament has seen.
  const beforeFinishing = winner;
  winner = finishedPlan(
    request,
    winner,
    (positions) => hardValidLayoutPositions(graph, winnerRemovedGroups, positions, fixedIds),
  );
  const finishingImproved = compareLayoutQuality(winner.quality, beforeFinishing.quality) > 0;
  if (crossing.cancelled && Number.isFinite(deadline) && now() >= deadline) timedOut = true;
  const selected = compareLayoutQuality(winner.quality, standard.quality) > 0;
  // The level relations a mask gives up only break ties between masks that
  // give up equally many exits, so a winner relaxing the proven minimum of
  // exits is optimal whichever of those masks it keeps.
  const winnerConstraintOptimal = searched.optimal &&
    winnerRelaxedScore[0] === searched.score[0] &&
    winnerRelaxedScore[1] === searched.score[1];
  const extensionSearch = {
    completed: extensionCompleted && !extensionCancelled && !extensionExhausted && pendingMasks === 0,
    cancelled: extensionCancelled,
    exhausted: extensionExhausted,
  };
  const maskDiversification = {
    // A cancelled extension traversal may have withheld equal-primary swaps,
    // so a cut run can never claim the diversification frontier was drained.
    completed: !maskExhausted && !extensionCancelled && pendingMasks === 0,
    exhausted: maskExhausted,
  };
  const report: ConstraintRepairReport = {
    ...workStats,
    trigger: options.when,
    outcome: "searched",
    selected,
    constraintOptimal: winnerConstraintOptimal,
    optimal: winnerConstraintOptimal,
    cutoff: timedOut
      ? "time"
      : extensionSearch.exhausted
      ? "extensions"
      : maskDiversification.exhausted
      ? "masks"
      : layoutFrontierTruncated
      ? "layouts"
      : perfect(winner)
      ? "none"
      : searched.cutoff,
    lowerBound: searched.lowerBound,
    relaxedEdges: winnerRelaxedScore[0],
    reciprocalRelaxedEdges: winnerRelaxedScore[1],
    relaxedLevelRelations: winnerRelaxedScore[2],
    standardViolations: standard.quality.cardinalRayViolations,
    finalViolations: winner.quality.cardinalRayViolations,
    beforeViolations,
    standardRoutingViolations: standard.quality.routingViolations,
    finalRoutingViolations: winner.quality.routingViolations,
    beforeRoutingViolations,
    beforeSettledViolations,
    standardSettledViolations,
    beforeSettledRoutingViolations,
    standardSettledRoutingViolations,
    restarts: searched.restarts,
    feasibilityChecks: searched.feasibilityChecks,
    layoutsConsidered,
    compactionAttempts,
    extensionSearch,
    maskDiversification,
    searchMs: searched.elapsedMs,
    compactionMs,
    polishTournaments: polished.tournaments,
    polishPasses: polished.passes,
    polishAnchorsTried: polished.anchorsTried,
    polishImprovements: polished.improvements,
    // Never a proof for a run the deadline cut at any point: callers retire
    // durable retry state on this flag, so a false claim is unrecoverable.
    geometricFixedPoint: !timedOut && (searched.cutoff === "none" || perfect(winner)) &&
      !layoutFrontierTruncated && polished.fixedPoint && !crossingImproved && !finishingImproved &&
      extensionSearch.completed && maskDiversification.completed &&
      crossing.completed && !crossing.cancelled && !crossing.exhausted,
    polishCutoff: polished.cutoff,
    polishMs: polished.elapsedMs,
    crossingRepair: {
      completed: crossing.completed,
      cancelled: crossing.cancelled,
      exhausted: crossing.exhausted,
      elapsedMs: crossingMs,
      ...crossing.stats,
    },
  };
  trace?.({ type: "constraint-repair", stage: "constraint-repair", report });
  // Polish and crossing phases rebuild the winner without the standard plan's
  // route amendments, and permanent all-immovable defects are exactly the case
  // repair cannot move rooms for — recompute amendments for the final geometry.
  const routeAmendments = computeIntegralRouteAmendments(request, winner);
  const { routeAmendments: _stale, ...base } = winner;
  return {
    ...base,
    ...(routeAmendments ? { routeAmendments } : {}),
    constraintRepair: report,
  };
}

/**
 * Whole-layout constraint repair. Called only in the Worker after the standard
 * plan. Every return carries a `constraintRepair` report whose `outcome` says
 * whether the repair searched or why it returned the standard plan instead.
 */
export function repairIntegralLayoutConstraints(
  request: IntegralLayoutRequest,
  standard: IntegralLayoutPlan,
  options: ConstraintRepairOptions,
  trace?: (event: LayoutTraceEvent) => void,
): IntegralLayoutPlan {
  return repairIntegralLayoutConstraintsWithRuntime(request, standard, options, trace);
}

interface TestGraphOptions {
  /** Flat edges whose level relation a test mask gives up. */
  relaxedLevelSourceIndexes?: readonly number[];
}

/** A test's relation mask, from the source indexes it gives up and those whose level it gives up. */
function testRelationMask(
  graph: DenseConstraintGraph,
  removedSourceIndexes: readonly number[],
  relaxedLevelSourceIndexes: readonly number[] = [],
): Uint8Array {
  const removed = new Set(removedSourceIndexes);
  const relaxedLevels = new Set(relaxedLevelSourceIndexes);
  return sourceMaskToGroupMask(
    graph,
    Uint8Array.from(graph.sourceIndexes, (sourceIndex) => removed.has(sourceIndex) ? 1 : 0),
    Uint8Array.from(graph.sourceIndexes, (sourceIndex) => relaxedLevels.has(sourceIndex) ? 1 : 0),
  );
}

/** Direct-only seams for deterministic stress and failure-path tests; not re-exported by the package entry. */
export const constraintLayoutInternalsForTesting = {
  packedCellKey: constraintCellKeyAt,

  crossingPairs(
    positions: readonly (readonly [number, number, number])[],
    edges: readonly { from: number; to: number; axis: 0 | 1 }[],
  ): number[][] {
    const index = createConstraintCrossingIndex(edges.map((edge, group) => ({ ...edge, group })));
    if (!index.build(positions, () => false)) throw new Error("unexpected crossing cancellation");
    return index.crossingsByFirst.map((crossings) => crossings.slice());
  },

  polish(
    request: IntegralLayoutRequest,
    seed: IntegralLayoutPlan,
    trace?: (event: LayoutTraceEvent) => void,
    options: {
      now?: () => number;
      deadline?: number;
      maximumTournaments?: number;
      maximumPasses?: number;
    } = {},
  ) {
    const now = options.now ?? (() => performance.now());
    const repairStarted = now();
    return polishConstraintLayoutToFixedPoint(request, seed, trace, {
      repairStarted,
      deadline: options.deadline ?? Number.POSITIVE_INFINITY,
      maximumTournaments: options.maximumTournaments ?? Number.POSITIVE_INFINITY,
      maximumPasses: options.maximumPasses ?? Number.POSITIVE_INFINITY,
      constraintLayoutsConsidered: 0,
      compactionAttempts: 0,
      restarts: 0,
      feasibilityChecks: 0,
      workStats: emptyConstraintWorkStats(),
    }, now);
  },

  /**
   * One feasibility check. The mask gives up the relations of
   * `removedSourceIndexes` and the level relations of the flat edges in
   * `graphOptions.relaxedLevelSourceIndexes`; a conflict reports the source
   * indexes of its directional relations and, apart, of its level relations.
   */
  analyze(
    positions: ReadonlyMap<string, GridPosition>,
    edges: readonly LayoutEdge[],
    removedSourceIndexes: readonly number[] = [],
    graphOptions: TestGraphOptions = {},
  ) {
    const graph = compileGraph(positions, edges);
    const removedGroups = testRelationMask(
      graph,
      removedSourceIndexes,
      graphOptions.relaxedLevelSourceIndexes,
    );
    const analyzed = analyzeConstraints(graph, removedGroups);
    if (!analyzed.ok) return { ok: false as const, reason: analyzed.reason };
    const conflict = analyzed.value.conflict && sourceIndexesForGroups(graph, analyzed.value.conflict);
    return {
      ok: true as const,
      feasible: !analyzed.value.conflict,
      conflictSourceIndexes: conflict?.directional,
      conflictLevelSourceIndexes: conflict?.level,
    };
  },

  hardValid(
    positions: ReadonlyMap<string, GridPosition>,
    edges: readonly LayoutEdge[],
    candidatePositions: ReadonlyMap<string, GridPosition>,
    removedSourceIndexes: readonly number[] = [],
    fixedIds: readonly string[] = [],
    graphOptions: TestGraphOptions = {},
  ) {
    const graph = compileGraph(positions, edges);
    return hardValidLayoutPositions(
      graph,
      testRelationMask(graph, removedSourceIndexes, graphOptions.relaxedLevelSourceIndexes),
      candidatePositions,
      new Set(fixedIds),
    );
  },

  /**
   * The compiled graph's relations: its directional groups, its level
   * relations as the source indexes of the flat edges each one joins, and the
   * source indexes the graph leaves out.
   */
  relations(
    positions: ReadonlyMap<string, GridPosition>,
    edges: readonly LayoutEdge[],
  ) {
    const graph = compileGraph(positions, edges);
    const compiled = new Set(graph.sourceIndexes);
    return {
      groupCount: graph.groupCount,
      relationCount: graph.relationCount,
      levelRelations: Array.from(
        graph.levelRelationGroup,
        (group) => sourceIndexesForGroups(graph, [group]).directional,
      ),
      omittedSourceIndexes: edges.map((_edge, index) => index).filter((index) => !compiled.has(index)),
    };
  },

  /**
   * Candidate admission on one compiled graph, allocation-free and as its
   * reference statements, for comparing verdicts.
   */
  admission(
    positions: ReadonlyMap<string, GridPosition>,
    edges: readonly LayoutEdge[],
    fixedIds: readonly string[] = [],
  ) {
    const graph = compileGraph(positions, edges);
    const fixed = new Set(fixedIds);
    return {
      groupCount: graph.groupCount,
      relationCount: graph.relationCount,
      removedGroups: (candidate: ReadonlyMap<string, GridPosition>) =>
        removedGroupsForPositions(graph, candidate),
      referenceRemovedGroups: (candidate: ReadonlyMap<string, GridPosition>) =>
        referenceRemovedGroupsForPositions(graph, candidate),
      hardValid: (candidate: ReadonlyMap<string, GridPosition>, removedGroups: Uint8Array) =>
        hardValidLayoutPositions(graph, removedGroups, candidate, fixed),
      referenceHardValid: (candidate: ReadonlyMap<string, GridPosition>, removedGroups: Uint8Array) =>
        referenceHardValidLayoutPositions(graph, removedGroups, candidate, fixed),
      acceptsAny: (candidate: ReadonlyMap<string, GridPosition>) =>
        acceptsAnyHardValidLayoutPositions(graph, candidate, fixed),
    };
  },

  firstAdmissibleSeparator(
    nodeCount: number,
    baseArcs: readonly ConstraintExtensionArc[],
    defects: readonly ConstraintExtensionDefect[],
  ): number | undefined {
    const outgoing = [0, 1, 2].map(() =>
      Array.from({ length: nodeCount }, () => [] as number[])
    );
    for (const arc of baseArcs) outgoing[arc.axis][arc.from].push(arc.to);
    const hasAdmissibleSeparator = createConstraintSeparatorAdmission(nodeCount);
    const candidates = defects.map((defect, index) => ({ defect, index }));
    const selected = firstAdmissibleConstraintDefect(candidates, (candidate) =>
      hasAdmissibleSeparator(candidate.defect.alternatives, outgoing, () => false) === true
        ? candidate.defect
        : undefined
    );
    return selected && selected !== DEFECT_SCAN_CANCELLED ? defects.indexOf(selected) : undefined;
  },

  compact(
    positions: ReadonlyMap<string, GridPosition>,
    edges: readonly LayoutEdge[],
    options: {
      removedSourceIndexes?: readonly number[];
      fixedIds?: readonly string[];
      maximumStates?: number;
      maximumLiveSearchNodes?: number;
      /** Latches cooperative cancellation once this many incumbents published. */
      cancelAtIncumbent?: number;
      /** Raw cooperative predicate, observed alongside any incumbent latch. */
      shouldCancel?: () => boolean;
      /** Flat edges whose level relation the mask gives up. */
      relaxedLevelSourceIndexes?: readonly number[];
      /** Cross-check packed lane and crossing sweep results against brute force. */
      verifySpatialIndexes?: boolean;
      /** Force every soft-defect hash into one bucket to exercise exact witnesses. */
      forceSoftDefectHashCollision?: boolean;
    } = {},
  ) {
    const graph = compileGraph(positions, edges);
    const removedGroups = testRelationMask(
      graph,
      options.removedSourceIndexes ?? [],
      options.relaxedLevelSourceIndexes,
    );
    const incumbents: { positions: Map<string, GridPosition>; quality: LayoutQuality }[] = [];
    const workStats = emptyConstraintWorkStats();
    const spatialVerification = options.verifySpatialIndexes
      ? { obstructionQueries: 0, obstructionHits: 0, crossingStates: 0, crossingPairs: 0 }
      : undefined;
    let status = { completed: true, cancelled: false, exhausted: false };
    let cancelRequested = false;
    const compacted = compactConstraints(graph, removedGroups, new Set(options.fixedIds), {
      maximumStates: options.maximumStates,
      maximumLiveSearchNodes: options.maximumLiveSearchNodes,
      shouldCancel: options.cancelAtIncumbent === undefined && options.shouldCancel === undefined
        ? undefined
        : () => cancelRequested || options.shouldCancel?.() === true,
      score: (candidate) => measureIntegralLayoutQuality(candidate, edges),
      onIncumbent: (candidate, quality) => {
        incumbents.push({
          positions: new Map(candidate.materialize()),
          quality: { ...quality },
        });
        if (options.cancelAtIncumbent !== undefined &&
          incumbents.length >= options.cancelAtIncumbent) cancelRequested = true;
      },
      onFinish: (finished) => {
        status = finished;
      },
      workStats,
      verifySpatialIndexes: spatialVerification,
      softDefectHash: options.forceSoftDefectHashCollision ? () => "forced-collision" : undefined,
    });
    return compacted.ok
      ? {
        ok: true as const,
        positions: compacted.value,
        quality: measureIntegralLayoutQuality(compacted.value, edges),
        incumbents,
        status,
        workStats,
        spatialVerification,
      }
      : { ok: false as const, reason: compacted.reason, incumbents, status, workStats };
  },

  /**
   * One exact hitting-set solve as the search runs it, over bare relation
   * weights; `groupLevelCount`, the third weight, is zero when omitted.
   * `incumbentMask` must hit every core; `nodes` is the node budget, and the
   * result reports the nodes the solve spent.
   */
  hittingSet(
    weights: {
      groupSourceCount: Int32Array;
      groupReciprocalCount: Int32Array;
      groupLevelCount?: Int32Array;
    },
    cores: readonly (readonly number[])[],
    incumbentMask: Uint8Array,
    nodes = MAX_HITTING_SET_DFS_NODES,
  ) {
    const graph: HittingSetWeights = {
      relationCount: weights.groupSourceCount.length,
      groupSourceCount: weights.groupSourceCount,
      groupReciprocalCount: weights.groupReciprocalCount,
      groupLevelCount: weights.groupLevelCount ?? new Int32Array(weights.groupSourceCount.length),
    };
    const incumbent = removedScore(graph, incumbentMask);
    const budget = { nodes };
    const solved = solveMinimumHittingSetByComponent(graph, cores, incumbentMask, incumbent, budget);
    return { ...solved, incumbent, nodes: nodes - budget.nodes };
  },

  /**
   * The master search. `removed` marks each compiled edge whose relation the
   * best mask gives up, directly or through its level relation, and
   * `relaxedLevelSourceIndexes` lists the flat edges whose level relation it
   * gives up.
   */
  search(
    positions: ReadonlyMap<string, GridPosition>,
    edges: readonly LayoutEdge[],
    options: ConstraintRepairOptions,
    runtime: SearchRuntimeOptions = {},
  ) {
    const graph = compileGraph(positions, edges);
    const searched = constraintSearch(graph, options, positions, runtime);
    if (!searched.ok) return { ok: false as const, reason: searched.reason };
    const removedSources = expandGroupMask(graph, searched.value.removed);
    const relaxedLevels = expandLevelRelationMask(graph, searched.value.removed);
    const sourceIndexesOf = (mask: Uint8Array): number[] =>
      Array.from(mask, (value, edge) => value ? graph.sourceIndexes[edge] : -1)
        .filter((sourceIndex) => sourceIndex !== -1);
    return {
      ok: true as const,
      ...searched.value,
      removed: removedSources,
      masks: searched.value.masks.map((mask) => expandGroupMask(graph, mask)),
      removedSourceIndexes: sourceIndexesOf(removedSources),
      relaxedLevelSourceIndexes: sourceIndexesOf(relaxedLevels),
    };
  },

  repairWithFailure(
    request: IntegralLayoutRequest,
    standard: IntegralLayoutPlan,
    options: ConstraintRepairOptions,
    fail: "search" | "compaction",
    trace?: (event: LayoutTraceEvent) => void,
  ) {
    return repairIntegralLayoutConstraintsWithRuntime(request, standard, options, trace, {
      search: fail === "search" ? () => failure("analysis") : undefined,
      compact: fail === "compaction" ? () => failure("compaction") : undefined,
    });
  },

  repairWithGravity(
    request: IntegralLayoutRequest,
    standard: IntegralLayoutPlan,
    options: ConstraintRepairOptions,
    gravity: NonNullable<RepairRuntimeOptions["gravity"]>,
    trace?: (event: LayoutTraceEvent) => void,
    now?: () => number,
  ) {
    return repairIntegralLayoutConstraintsWithRuntime(request, standard, options, trace, {
      gravity,
      now,
    });
  },

  repairWithClock(
    request: IntegralLayoutRequest,
    standard: IntegralLayoutPlan,
    options: ConstraintRepairOptions,
    now: () => number,
    trace?: (event: LayoutTraceEvent) => void,
  ) {
    return repairIntegralLayoutConstraintsWithRuntime(request, standard, options, trace, { now });
  },

  repairWithHashCollisions(
    request: IntegralLayoutRequest,
    standard: IntegralLayoutPlan,
    options: ConstraintRepairOptions,
    trace?: (event: LayoutTraceEvent) => void,
  ) {
    return repairIntegralLayoutConstraintsWithRuntime(request, standard, options, trace, {
      densePositionHash: () => ({ hash: 0, second: 0 }),
      softDefectHash: () => "forced-collision",
    });
  },
};
