/**
 * Deterministic, fixed-mask search for precedence constraints which separate
 * otherwise invalid or visually poor geometry.
 *
 * The caller owns geometry construction and scoring. This module owns only the
 * reversible precedence DAG and the anytime traversal. Keeping that boundary
 * small lets the same search serve collision repair, corridor obstruction
 * repair, and crossing polish without retaining every generated layout.
 *
 * The traversal is best-first on a bounded frontier: every unexplored
 * alternative is an open node keyed by the score of its nearest ancestor
 * candidate, so under a finite state budget the incumbent stream follows the
 * most promising subtrees instead of being hostage to the first alternative's
 * subtree. When the live-node ceiling is reached, the beam deterministically
 * retains its best alternatives while always advancing one preferred child.
 * If the active path itself reaches the ceiling, it becomes the next search
 * root and its now-incompatible siblings are discarded. This preserves high
 * total-work budgets and anytime incumbents without retaining one heap node
 * (plus its parent chain) for every inspected state.
 * Chains which have not yet produced a hard-valid candidate are expanded
 * eagerly, deepest-first, so collision repair reaches its first complete
 * layout as fast as plain depth-first search. The reversible DAGs make this
 * affordable without re-materializing states: switching to another open node
 * rolls the DAGs back to the deepest common ancestor's checkpoint and
 * re-applies the remaining ancestor arcs, which preserves the strictly LIFO
 * undo-journal discipline the DAG implementations rely on. Arc application is
 * deferred to expansion time, so an open node costs one small record until it
 * is actually visited. Limited-discrepancy restarts were rejected because
 * every re-inspected state would be charged against `maxExtensionStates`,
 * burning the budget the reordering is meant to spend better.
 */

export type ConstraintExtensionAxis = 0 | 1 | 2;

/** A strict, unit-distance precedence: `from` must precede `to` on `axis`. */
export interface ConstraintExtensionArc {
  readonly axis: ConstraintExtensionAxis;
  readonly from: number;
  readonly to: number;
}

/** All arcs in an alternative are applied atomically. */
export interface ConstraintExtensionAlternative {
  readonly arcs: readonly ConstraintExtensionArc[];
}

/**
 * An explanation supplied by the caller for an immutable-base contradiction.
 * The search never manufactures relation groups or promotes a soft defect to
 * this type.
 */
export interface ConstraintExtensionHardExplanation {
  readonly scope: "base-relations";
  readonly relationGroups: readonly number[];
}

export interface ConstraintExtensionDefect {
  /** Diagnostic label such as `collision`, `obstruction`, or `crossing`. */
  readonly kind: string;
  /**
   * Stable, preferred-first ordering. Siblings are explored in exactly this
   * order; between defects the frontier interleaves best-first.
   */
  readonly alternatives: readonly ConstraintExtensionAlternative[];
  /** Optional relation groups useful to a caller-owned diversification pass. */
  readonly relationGroups?: readonly number[];
}

export interface ConstraintExtensionHardConflict extends ConstraintExtensionDefect {
  /**
   * Supply only when the conflict follows solely from immutable base groups.
   * It is returned only for a root conflict with no legal alternatives.
   */
  readonly explanation?: ConstraintExtensionHardExplanation;
}

export type ConstraintExtensionInspection<TCandidate, TScore> =
  | {
    readonly type: "hard-conflict";
    readonly conflict: ConstraintExtensionHardConflict;
  }
  | {
    /** A complete, hard-valid layout. Soft defects do not invalidate it. */
    readonly type: "candidate";
    /**
     * Eager value for callers whose candidate is already cheap to retain.
     * Expensive geometry callers may instead provide `materializeCandidate`;
     * the search invokes it synchronously, before the next inspection, only
     * when this state becomes the strict incumbent. A materializer may
     * therefore snapshot caller-owned inspection scratch on demand.
     */
    readonly candidate?: TCandidate;
    readonly materializeCandidate?: () => TCandidate;
    readonly score: TScore;
    /** One optional defect to branch on after accepting the candidate. */
    readonly softDefect?: ConstraintExtensionDefect;
  }
  | {
    /**
     * A deadline or abort observed while inspecting this state. The traversal
     * terminates as cancelled — never as a hard conflict or as a completed
     * search — even when this is the root or the final remaining state.
     */
    readonly type: "cancelled";
  };

export interface ConstraintExtensionInspectContext {
  /**
   * The currently active non-redundant extension arcs. This view is valid only
   * for the synchronous duration of `inspect`; copy it if it must be retained.
   */
  readonly extensionArcs: readonly ConstraintExtensionArc[];
  /**
   * Allocation-free, order-independent fingerprint of `extensionArcs`. These
   * lanes are maintained as arcs enter and leave the reversible search path,
   * so geometry callers need not clone and sort a deep path for every state.
   */
  readonly extensionFingerprintFirst: number;
  readonly extensionFingerprintSecond: number;
  readonly extensionFingerprintCount: number;
  /** One-based number of this inspected state. */
  readonly state: number;
  /** Root is depth zero. */
  readonly depth: number;
  /** Alternatives attempted before this state was inspected. */
  readonly branches: number;
  /** Cyclic alternatives pruned before this state was inspected. */
  readonly cyclePrunes: number;
  /** Deepest state reached, including this state. */
  readonly maxDepth: number;
  /** Also exposed so expensive geometry construction can stop cooperatively. */
  readonly shouldCancel: () => boolean;
}

export interface ConstraintExtensionWorkStats {
  readonly states: number;
  readonly branches: number;
  readonly cyclePrunes: number;
  readonly maxDepth: number;
  readonly candidateStates: number;
  readonly hardConflicts: number;
  readonly rawIncumbents: number;
  readonly softIncumbents: number;
  readonly noOpPrunes: number;
}

export interface ConstraintExtensionIncumbent<TCandidate, TScore> {
  readonly candidate: TCandidate;
  readonly score: TScore;
  readonly hasSoftDefect: boolean;
  readonly stats: ConstraintExtensionWorkStats;
}

export interface ConstraintExtensionDiversificationRequest<TCandidate, TScore> {
  readonly reason: "hard-conflict" | "soft-defect";
  readonly defect: ConstraintExtensionDefect;
  readonly candidate?: TCandidate;
  readonly score?: TScore;
  /** Stable snapshot: this callback may queue it for a later mask search. */
  readonly extensionArcs: readonly ConstraintExtensionArc[];
}

export interface ConstraintExtensionSearchOptions<TCandidate, TScore> {
  /** Dense node counts after the caller has collapsed equality components. */
  readonly axisNodeCounts: readonly [number, number, number];
  readonly baseArcs?: readonly ConstraintExtensionArc[];
  readonly inspect: (
    context: ConstraintExtensionInspectContext,
  ) => ConstraintExtensionInspection<TCandidate, TScore>;
  /** Return a positive number exactly when `left` is strictly better. */
  readonly compareScores: (left: TScore, right: TScore) => number;
  /** Maximum inspected states for this mask. Infinity is supported. */
  readonly maxExtensionStates?: number;
  /**
   * Maximum live search-node records, including open alternatives and the
   * parent metadata needed to switch between them. This is deliberately
   * independent of the much larger total-state budget. A bounded beam keeps
   * searching after pruning, but can no longer claim exhaustive completion.
   */
  readonly maxLiveSearchNodes?: number;
  readonly shouldCancel?: () => boolean;
  readonly onIncumbent?: (
    incumbent: ConstraintExtensionIncumbent<TCandidate, TScore>,
  ) => void;
  /** State-count based (never clock based), so runs remain reproducible. */
  readonly onProgress?: (stats: ConstraintExtensionWorkStats) => void;
  readonly progressIntervalStates?: number;
  /**
   * Seam for caller-owned, geometry-guided equal-primary relation-mask swaps.
   * This fixed-mask engine never starts or counts a mask diversification.
   */
  readonly onEqualPrimaryDiversification?: (
    request: ConstraintExtensionDiversificationRequest<TCandidate, TScore>,
  ) => void;
  /** Skip the active-arc snapshot when the diversification consumer needs only the defect. */
  readonly snapshotDiversificationArcs?: boolean;
}

export interface ConstraintExtensionSearchResult<TCandidate, TScore>
  extends ConstraintExtensionWorkStats {
  /** True only after the deterministic search space was fully traversed. */
  readonly completed: boolean;
  readonly cancelled: boolean;
  /** True when either the state or live-node ceiling cut the exact traversal. */
  readonly exhausted: boolean;
  /** Open subtrees discarded to stay within the live-node ceiling. */
  readonly frontierPrunes: number;
  /** Largest number of alternatives simultaneously held in the frontier. */
  readonly peakFrontierNodes: number;
  /**
   * Largest number of live search nodes, including dormant parent chains and
   * the materialized path. This is the deterministic retained-memory proxy.
   */
  readonly peakLiveSearchNodes: number;
  /** Constant-space retention: only the best strict incumbent is retained. */
  readonly best?: TCandidate;
  readonly bestScore?: TScore;
  /** True means proven; false can also mean cancellation or work exhaustion. */
  readonly hardInfeasible: boolean;
  readonly hardExplanation?: ConstraintExtensionHardExplanation;
}

type AddArcResult = "added" | "implied" | "cycle" | "cancelled";

interface ReversibleDag {
  checkpoint(): number;
  rollback(checkpoint: number): void;
  clearHistory(): void;
  add(from: number, to: number, shouldCancel: () => boolean): AddArcResult;
}

const DENSE_REACHABILITY_LIMIT = 1_024;
const GRAPH_CANCEL_CHECK_MASK = 0x3ff;
const DEFAULT_PROGRESS_INTERVAL_STATES = 256;
/** A few MiB of lightweight nodes rather than millions of retained objects. */
const DEFAULT_MAX_LIVE_SEARCH_NODES = 32_768;
const NEVER_CANCEL = (): boolean => false;

function extensionArcFingerprintFirst(arc: Readonly<ConstraintExtensionArc>): number {
  let first = 0x811c9dc5;
  first = Math.imul(first ^ arc.axis, 0x01000193) >>> 0;
  first = Math.imul(first ^ arc.from, 0x01000193) >>> 0;
  first = Math.imul(first ^ arc.to, 0x01000193) >>> 0;
  return first;
}

function extensionArcFingerprintSecond(arc: Readonly<ConstraintExtensionArc>): number {
  let second = 0x9e3779b9;
  second = Math.imul(second ^ (arc.axis + 0x7f4a7c15), 0x85ebca6b) >>> 0;
  second = Math.imul(second ^ (arc.from + 0x7f4a7c15), 0x85ebca6b) >>> 0;
  second = Math.imul(second ^ (arc.to + 0x7f4a7c15), 0x85ebca6b) >>> 0;
  return second;
}

class DenseReachabilityDag implements ReversibleDag {
  readonly #size: number;
  readonly #words: number;
  readonly #reach: Uint32Array;
  /** Pairs of flattened word index and its previous value. */
  readonly #undo: number[] = [];

  constructor(size: number) {
    this.#size = size;
    this.#words = (size + 31) >>> 5;
    this.#reach = new Uint32Array(size * this.#words);
    for (let node = 0; node < size; node += 1) {
      this.#reach[node * this.#words + (node >>> 5)] |= 1 << (node & 31);
    }
  }

  checkpoint(): number {
    return this.#undo.length;
  }

  rollback(checkpoint: number): void {
    while (this.#undo.length > checkpoint) {
      const previous = this.#undo.pop() as number;
      const index = this.#undo.pop() as number;
      this.#reach[index] = previous;
    }
  }

  clearHistory(): void {
    this.#undo.length = 0;
  }

  #hasPath(from: number, to: number): boolean {
    const word = this.#reach[from * this.#words + (to >>> 5)];
    return (word & (1 << (to & 31))) !== 0;
  }

  add(from: number, to: number, shouldCancel: () => boolean): AddArcResult {
    if (from === to || this.#hasPath(to, from)) return "cycle";
    if (this.#hasPath(from, to)) return "implied";

    const checkpoint = this.checkpoint();
    const successorOffset = to * this.#words;
    let work = 0;
    for (let predecessor = 0; predecessor < this.#size; predecessor += 1) {
      if (!this.#hasPath(predecessor, from)) continue;
      const predecessorOffset = predecessor * this.#words;
      for (let word = 0; word < this.#words; word += 1) {
        if ((work++ & GRAPH_CANCEL_CHECK_MASK) === 0 && shouldCancel()) {
          this.rollback(checkpoint);
          return "cancelled";
        }
        const index = predecessorOffset + word;
        const previous = this.#reach[index];
        const next = previous | this.#reach[successorOffset + word];
        if (next === previous) continue;
        this.#undo.push(index, previous);
        this.#reach[index] = next;
      }
    }
    return "added";
  }
}

class SparseReachabilityDag implements ReversibleDag {
  readonly #outgoing: number[][];
  readonly #seen: Int32Array;
  readonly #stack: number[] = [];
  /** Source node for each reversible appended edge. */
  readonly #undo: number[] = [];
  #stamp = 0;

  constructor(size: number) {
    this.#outgoing = Array.from({ length: size }, () => []);
    this.#seen = new Int32Array(size);
  }

  checkpoint(): number {
    return this.#undo.length;
  }

  rollback(checkpoint: number): void {
    while (this.#undo.length > checkpoint) {
      const from = this.#undo.pop() as number;
      this.#outgoing[from].pop();
    }
  }

  clearHistory(): void {
    this.#undo.length = 0;
  }

  #reachable(
    from: number,
    target: number,
    shouldCancel: () => boolean,
  ): boolean | undefined {
    this.#stamp += 1;
    if (this.#stamp === 0x7fffffff) {
      this.#seen.fill(0);
      this.#stamp = 1;
    }
    const stamp = this.#stamp;
    const stack = this.#stack;
    stack.length = 0;
    stack.push(from);
    this.#seen[from] = stamp;
    let work = 0;
    while (stack.length > 0) {
      if ((work++ & GRAPH_CANCEL_CHECK_MASK) === 0 && shouldCancel()) {
        stack.length = 0;
        return undefined;
      }
      const node = stack.pop() as number;
      if (node === target) {
        stack.length = 0;
        return true;
      }
      const outgoing = this.#outgoing[node];
      // Reverse push preserves the caller's original edge order in traversal.
      for (let index = outgoing.length - 1; index >= 0; index -= 1) {
        const next = outgoing[index];
        if (this.#seen[next] === stamp) continue;
        this.#seen[next] = stamp;
        stack.push(next);
      }
    }
    return false;
  }

  add(from: number, to: number, shouldCancel: () => boolean): AddArcResult {
    if (from === to) return "cycle";
    const reverse = this.#reachable(to, from, shouldCancel);
    if (reverse === undefined) return "cancelled";
    if (reverse) return "cycle";
    const forward = this.#reachable(from, to, shouldCancel);
    if (forward === undefined) return "cancelled";
    if (forward) return "implied";
    this.#outgoing[from].push(to);
    this.#undo.push(from);
    return "added";
  }
}

/** One unexplored alternative: a lightweight open node awaiting expansion. */
interface SearchNode<TScore> {
  /** The applied node whose inspection produced this alternative; root: none. */
  parent: SearchNode<TScore> | undefined;
  readonly arcs: readonly ConstraintExtensionArc[];
  readonly depth: number;
  /**
   * Score of the nearest ancestor candidate state — the frontier ordering
   * basis. Undefined only on chains that have not yet reached hard validity.
   */
  readonly scoreBasis: TScore | undefined;
  /** Creation order; the deterministic total tie-break. */
  readonly ordinal: number;
  /** Frontier/applied-path/child references tracked for a truthful live cap. */
  retainers: number;
  /** Positions in the frontier's paired best/worst heaps. */
  bestHeapIndex: number;
  worstHeapIndex: number;
}

/** One applied node on the currently materialized root-to-leaf path. */
interface AppliedFrame<TScore> {
  readonly node: SearchNode<TScore>;
  /** Per-axis DAG checkpoints taken before this node's arcs were applied. */
  readonly checkpoints: readonly [number, number, number];
  /** Active-arc count before this node's arcs were applied. */
  readonly arcLength: number;
}

/**
 * Paired deterministic heaps over open nodes. The best heap drives traversal;
 * the worst heap makes bounded-beam eviction logarithmic instead of scanning
 * the entire live frontier every time it is full.
 */
class SearchFrontier<TScore> {
  readonly #bestNodes: SearchNode<TScore>[] = [];
  readonly #worstNodes: SearchNode<TScore>[] = [];
  readonly #compare: (a: SearchNode<TScore>, b: SearchNode<TScore>) => number;

  constructor(compare: (a: SearchNode<TScore>, b: SearchNode<TScore>) => number) {
    this.#compare = compare;
  }

  get length(): number {
    return this.#bestNodes.length;
  }

  push(node: SearchNode<TScore>): void {
    this.#pushHeap(this.#bestNodes, node, "bestHeapIndex", this.#compare);
    this.#pushHeap(
      this.#worstNodes,
      node,
      "worstHeapIndex",
      (left, right) => this.#compare(right, left),
    );
  }

  peekWorst(): SearchNode<TScore> | undefined {
    return this.#worstNodes[0];
  }

  popBest(): SearchNode<TScore> | undefined {
    const node = this.#bestNodes[0];
    if (!node) return undefined;
    this.#removeHeap(
      this.#bestNodes,
      node.bestHeapIndex,
      "bestHeapIndex",
      this.#compare,
    );
    this.#removeHeap(
      this.#worstNodes,
      node.worstHeapIndex,
      "worstHeapIndex",
      (left, right) => this.#compare(right, left),
    );
    return node;
  }

  popWorst(): SearchNode<TScore> | undefined {
    const node = this.#worstNodes[0];
    if (!node) return undefined;
    this.#removeHeap(
      this.#worstNodes,
      node.worstHeapIndex,
      "worstHeapIndex",
      (left, right) => this.#compare(right, left),
    );
    this.#removeHeap(
      this.#bestNodes,
      node.bestHeapIndex,
      "bestHeapIndex",
      this.#compare,
    );
    return node;
  }

  clear(): SearchNode<TScore>[] {
    const nodes = this.#bestNodes.slice();
    this.#bestNodes.length = 0;
    this.#worstNodes.length = 0;
    for (const node of nodes) {
      node.bestHeapIndex = -1;
      node.worstHeapIndex = -1;
    }
    return nodes;
  }

  #pushHeap(
    nodes: SearchNode<TScore>[],
    node: SearchNode<TScore>,
    indexKey: "bestHeapIndex" | "worstHeapIndex",
    compare: (a: SearchNode<TScore>, b: SearchNode<TScore>) => number,
  ): void {
    let index = nodes.length;
    nodes.push(node);
    while (index > 0) {
      const parent = (index - 1) >>> 1;
      if (compare(nodes[parent], node) <= 0) break;
      nodes[index] = nodes[parent];
      nodes[index][indexKey] = index;
      index = parent;
    }
    nodes[index] = node;
    node[indexKey] = index;
  }

  #removeHeap(
    nodes: SearchNode<TScore>[],
    index: number,
    indexKey: "bestHeapIndex" | "worstHeapIndex",
    compare: (a: SearchNode<TScore>, b: SearchNode<TScore>) => number,
  ): void {
    const removed = nodes[index];
    const last = nodes.pop();
    removed[indexKey] = -1;
    if (last === undefined || last === removed) return;
    nodes[index] = last;
    last[indexKey] = index;

    const parent = index > 0 ? (index - 1) >>> 1 : -1;
    if (parent >= 0 && compare(nodes[parent], last) > 0) {
      let cursor = index;
      while (cursor > 0) {
        const nextParent = (cursor - 1) >>> 1;
        if (compare(nodes[nextParent], last) <= 0) break;
        nodes[cursor] = nodes[nextParent];
        nodes[cursor][indexKey] = cursor;
        cursor = nextParent;
      }
      nodes[cursor] = last;
      last[indexKey] = cursor;
      return;
    }

    for (;;) {
      const left = index * 2 + 1;
      if (left >= nodes.length) break;
      const right = left + 1;
      const child = right < nodes.length && compare(nodes[right], nodes[left]) < 0
        ? right
        : left;
      if (compare(nodes[child], last) >= 0) break;
      nodes[index] = nodes[child];
      nodes[index][indexKey] = index;
      index = child;
    }
    nodes[index] = last;
    last[indexKey] = index;
  }
}

function validateLimit(value: number, name: string, allowZero: boolean): void {
  if (value === Number.POSITIVE_INFINITY) return;
  if (!Number.isInteger(value) || value < (allowZero ? 0 : 1)) {
    throw new RangeError(`${name} must be ${allowZero ? "a non-negative" : "a positive"} integer or Infinity`);
  }
}

function validateAxisNodeCounts(
  counts: readonly [number, number, number],
): void {
  if (counts.length !== 3) throw new RangeError("axisNodeCounts must contain three axes");
  for (const count of counts) {
    if (!Number.isSafeInteger(count) || count < 0) {
      throw new RangeError("axisNodeCounts must contain non-negative safe integers");
    }
  }
}

function validateArc(
  arc: ConstraintExtensionArc,
  counts: readonly [number, number, number],
): void {
  if (arc.axis !== 0 && arc.axis !== 1 && arc.axis !== 2) {
    throw new RangeError(`invalid constraint-extension axis: ${String(arc.axis)}`);
  }
  const count = counts[arc.axis];
  if (!Number.isSafeInteger(arc.from) || arc.from < 0 || arc.from >= count
      || !Number.isSafeInteger(arc.to) || arc.to < 0 || arc.to >= count) {
    throw new RangeError(
      `constraint-extension arc ${arc.from}->${arc.to} is outside axis ${arc.axis} (size ${count})`,
    );
  }
}

/**
 * Exhaustively searches one caller-selected relation mask unless cancelled or
 * stopped by a state/live-node ceiling. Sibling alternatives are explored in
 * supplied order; across defects the frontier is best-first on ancestor
 * candidate scores, so finite budgets spend their states in the most
 * promising subtrees. A live-node cut keeps searching as a deterministic
 * bounded beam and reports `exhausted`, never a false proof of completion.
 */
export function searchConstraintExtensions<TCandidate, TScore>(
  options: ConstraintExtensionSearchOptions<TCandidate, TScore>,
): ConstraintExtensionSearchResult<TCandidate, TScore> {
  validateAxisNodeCounts(options.axisNodeCounts);
  const maximumStates = options.maxExtensionStates ?? Number.POSITIVE_INFINITY;
  const maximumLiveSearchNodes = options.maxLiveSearchNodes
    ?? DEFAULT_MAX_LIVE_SEARCH_NODES;
  const progressInterval = options.progressIntervalStates
    ?? DEFAULT_PROGRESS_INTERVAL_STATES;
  validateLimit(maximumStates, "maxExtensionStates", true);
  validateLimit(maximumLiveSearchNodes, "maxLiveSearchNodes", false);
  validateLimit(progressInterval, "progressIntervalStates", false);

  const shouldCancel = options.shouldCancel ?? NEVER_CANCEL;
  const dags = options.axisNodeCounts.map((count) => (
    count <= DENSE_REACHABILITY_LIMIT
      ? new DenseReachabilityDag(count)
      : new SparseReachabilityDag(count)
  )) as unknown as [ReversibleDag, ReversibleDag, ReversibleDag];

  let states = 0;
  let branches = 0;
  let cyclePrunes = 0;
  let maxDepth = 0;
  let candidateStates = 0;
  let hardConflicts = 0;
  let rawIncumbents = 0;
  let softIncumbents = 0;
  let noOpPrunes = 0;
  let frontierPrunes = 0;
  let liveSearchNodes = 0;
  let peakLiveSearchNodes = 0;
  let peakFrontierNodes = 0;
  let cancelled = false;
  let stateExhausted = false;
  let baseInfeasible = false;
  let hasBest = false;
  let best: TCandidate | undefined;
  let bestScore: TScore | undefined;
  let hardExplanation: ConstraintExtensionHardExplanation | undefined;
  let lastProgressState = -1;

  const stats = (): ConstraintExtensionWorkStats => ({
    states,
    branches,
    cyclePrunes,
    maxDepth,
    candidateStates,
    hardConflicts,
    rawIncumbents,
    softIncumbents,
    noOpPrunes,
  });
  const emitProgress = (force = false): void => {
    if (!options.onProgress) return;
    if (!force && states - lastProgressState < progressInterval) return;
    lastProgressState = states;
    options.onProgress(stats());
  };

  for (const arc of options.baseArcs ?? []) {
    if (shouldCancel()) {
      cancelled = true;
      break;
    }
    validateArc(arc, options.axisNodeCounts);
    const dag = dags[arc.axis];
    const result = dag.add(arc.from, arc.to, shouldCancel);
    if (result === "cancelled") {
      cancelled = true;
      break;
    }
    if (result === "cycle") {
      baseInfeasible = true;
      break;
    }
    dag.clearHistory();
  }

  const activeArcs: ConstraintExtensionArc[] = [];
  let extensionFingerprintFirst = 0;
  let extensionFingerprintSecond = 0;
  let extensionFingerprintCount = 0;
  const pushActiveArc = (arc: ConstraintExtensionArc): void => {
    const first = extensionArcFingerprintFirst(arc);
    const second = extensionArcFingerprintSecond(arc);
    activeArcs.push(arc);
    extensionFingerprintFirst ^= first;
    extensionFingerprintSecond = (extensionFingerprintSecond + second) >>> 0;
    extensionFingerprintCount += 1;
  };
  const truncateActiveArcs = (length: number): void => {
    while (activeArcs.length > length) {
      const arc = activeArcs.pop() as ConstraintExtensionArc;
      const first = extensionArcFingerprintFirst(arc);
      const second = extensionArcFingerprintSecond(arc);
      extensionFingerprintFirst ^= first;
      extensionFingerprintSecond = (extensionFingerprintSecond - second) >>> 0;
      extensionFingerprintCount -= 1;
    }
  };
  const appliedPath: AppliedFrame<TScore>[] = [];
  let nextOrdinal = 0;
  // Frontier order: chains without a candidate ancestor first (deepest first,
  // so collision chains punch through to hard validity depth-first), then by
  // ancestor candidate score, best first. Depth and creation order break the
  // remaining ties, which keeps siblings in their preferred-first supplied
  // order and makes the whole traversal deterministic.
  const compareFrontierNodes = (a: SearchNode<TScore>, b: SearchNode<TScore>): number => {
    const aScored = a.scoreBasis !== undefined;
    const bScored = b.scoreBasis !== undefined;
    if (aScored !== bScored) return aScored ? 1 : -1;
    if (aScored) {
      const scores = options.compareScores(a.scoreBasis as TScore, b.scoreBasis as TScore);
      if (scores !== 0) return -scores;
      if (a.depth !== b.depth) return a.depth - b.depth;
    } else if (a.depth !== b.depth) {
      return b.depth - a.depth;
    }
    return a.ordinal - b.ordinal;
  };
  const frontier = new SearchFrontier<TScore>(compareFrontierNodes);

  const retainNode = (node: SearchNode<TScore>): void => {
    node.retainers += 1;
  };
  // Iterative cascading avoids a call-stack overflow when a deep committed
  // beam path is released all at once.
  const releaseNode = (initial: SearchNode<TScore>): void => {
    let node: SearchNode<TScore> | undefined = initial;
    while (node) {
      node.retainers -= 1;
      if (node.retainers > 0) return;
      liveSearchNodes -= 1;
      const parent: SearchNode<TScore> | undefined = node.parent;
      node.parent = undefined;
      node = parent;
    }
  };
  const releaseAppliedPathFrom = (start: number): void => {
    for (let index = appliedPath.length - 1; index >= start; index -= 1) {
      releaseNode(appliedPath[index].node);
    }
    appliedPath.length = start;
  };
  const retainForFrontier = (node: SearchNode<TScore>): void => {
    node.retainers = 1;
    liveSearchNodes += 1;
    if (node.parent) retainNode(node.parent);
    if (liveSearchNodes > peakLiveSearchNodes) peakLiveSearchNodes = liveSearchNodes;
    frontier.push(node);
    if (frontier.length > peakFrontierNodes) peakFrontierNodes = frontier.length;
  };
  const discardWorstFrontierNode = (): boolean => {
    const discarded = frontier.popWorst();
    if (!discarded) return false;
    frontierPrunes += 1;
    releaseNode(discarded);
    return true;
  };
  const rebaseActivePath = (): void => {
    const discarded = frontier.clear();
    frontierPrunes += discarded.length;
    for (const node of discarded) releaseNode(node);
    // The active precedence state is now the immutable base for this bounded
    // beam epoch. Its arcs remain visible to the caller in `activeArcs`, while
    // old undo history and parent metadata can be released.
    for (const dag of dags) dag.clearHistory();
    releaseAppliedPathFrom(0);
  };
  const enqueueAlternatives = (
    depth: number,
    alternatives: readonly ConstraintExtensionAlternative[],
    scoreBasis: TScore | undefined,
  ): void => {
    let parent = appliedPath.length > 0
      ? appliedPath[appliedPath.length - 1].node
      : undefined;
    for (let index = 0; index < alternatives.length; index += 1) {
      const alternative = alternatives[index];
      const node: SearchNode<TScore> = {
        parent,
        arcs: alternative.arcs,
        depth,
        scoreBasis,
        ordinal: nextOrdinal++,
        retainers: 0,
        bestHeapIndex: -1,
        worstHeapIndex: -1,
      };
      if (liveSearchNodes >= maximumLiveSearchNodes) {
        const worst = frontier.peekWorst();
        const preferredChild = index === 0;
        const belongsInBeam = worst !== undefined &&
          compareFrontierNodes(node, worst) < 0;
        if ((preferredChild || belongsInBeam) && discardWorstFrontierNode()) {
          // The eviction releases at least its own frontier reference and may
          // also release a dormant parent chain.
        } else if (preferredChild && appliedPath.length > 0) {
          rebaseActivePath();
          parent = undefined;
          node.parent = undefined;
        } else {
          frontierPrunes += 1;
          continue;
        }
      }
      retainForFrontier(node);
    }
  };

  const inspectCurrent = (depth: number, inheritedBasis: TScore | undefined): void => {
    states += 1;
    if (depth > maxDepth) maxDepth = depth;
    const inspection = options.inspect({
      extensionArcs: activeArcs,
      extensionFingerprintFirst: extensionFingerprintFirst >>> 0,
      extensionFingerprintSecond,
      extensionFingerprintCount,
      state: states,
      depth,
      branches,
      cyclePrunes,
      maxDepth,
      shouldCancel,
    });
    // An inspection-reported cancellation is authoritative even when the
    // caller's predicate does not latch, so it can never be miscounted as a
    // hard conflict or drain the frontier into a spurious completion.
    if (inspection.type === "cancelled" || shouldCancel()) {
      cancelled = true;
      return;
    }

    if (inspection.type === "hard-conflict") {
      hardConflicts += 1;
      const conflict = inspection.conflict;
      // Hard geometry branches normally carry no relation-mask guidance.
      // Avoid copying a potentially deep active-arc path unless the caller
      // supplied relation groups which can actually drive diversification.
      if (options.onEqualPrimaryDiversification && conflict.relationGroups?.length) {
        options.onEqualPrimaryDiversification({
          reason: "hard-conflict",
          defect: conflict,
          extensionArcs: options.snapshotDiversificationArcs === false ? [] : activeArcs.slice(),
        });
      }
      if (depth === 0 && conflict.alternatives.length === 0
          && conflict.explanation?.scope === "base-relations") {
        hardExplanation = {
          scope: "base-relations",
          relationGroups: [...conflict.explanation.relationGroups],
        };
      }
      enqueueAlternatives(depth + 1, conflict.alternatives, inheritedBasis);
    } else {
      candidateStates += 1;
      const isBetter = !hasBest
        || options.compareScores(inspection.score, bestScore as TScore) > 0;
      let materialized: TCandidate | undefined;
      let hasMaterialized = false;
      const materialize = (): TCandidate => {
        if (hasMaterialized) return materialized as TCandidate;
        if ("candidate" in inspection) {
          materialized = inspection.candidate;
        } else if (inspection.materializeCandidate) {
          materialized = inspection.materializeCandidate();
        } else {
          throw new TypeError("candidate inspection must provide a candidate value or materializer");
        }
        hasMaterialized = true;
        return materialized as TCandidate;
      };
      if (isBetter) {
        hasBest = true;
        best = materialize();
        bestScore = inspection.score;
        rawIncumbents += 1;
        if (inspection.softDefect) softIncumbents += 1;
        options.onIncumbent?.({
          candidate: best,
          score: inspection.score,
          hasSoftDefect: inspection.softDefect !== undefined,
          stats: stats(),
        });
      }
      if (inspection.softDefect) {
        if (options.onEqualPrimaryDiversification) {
          options.onEqualPrimaryDiversification({
            reason: "soft-defect",
            defect: inspection.softDefect,
            candidate: hasMaterialized
              ? materialized
              : "candidate" in inspection
              ? inspection.candidate
              : undefined,
            score: inspection.score,
            extensionArcs: options.snapshotDiversificationArcs === false ? [] : activeArcs.slice(),
          });
        }
        enqueueAlternatives(depth + 1, inspection.softDefect.alternatives, inspection.score);
      }
    }
    emitProgress();
  };

  // Rebuild the materialized path down to the popped node's parent: roll the
  // DAGs back to the deepest common ancestor's checkpoint, then re-apply the
  // remaining ancestor arcs. Every re-applied ancestor was inspected under an
  // identical DAG state, so each arc reproduces its original added/implied
  // outcome and a cycle is impossible; only a cooperative cancellation can
  // interrupt. Rollbacks always target checkpoints of the current path, so
  // the DAG undo journals unwind strictly LIFO.
  const ancestorsScratch: SearchNode<TScore>[] = [];
  const switchToParentChain = (node: SearchNode<TScore>): boolean => {
    ancestorsScratch.length = 0;
    for (let cursor = node.parent; cursor; cursor = cursor.parent) ancestorsScratch.push(cursor);
    ancestorsScratch.reverse();
    let common = 0;
    while (common < appliedPath.length && common < ancestorsScratch.length &&
      appliedPath[common].node === ancestorsScratch[common]) common += 1;
    if (appliedPath.length > common) {
      const frame = appliedPath[common];
      for (let axis = 0; axis < 3; axis += 1) dags[axis].rollback(frame.checkpoints[axis]);
      truncateActiveArcs(frame.arcLength);
      releaseAppliedPathFrom(common);
    }
    for (let index = common; index < ancestorsScratch.length; index += 1) {
      const ancestor = ancestorsScratch[index];
      const checkpoints = dags.map((dag) => dag.checkpoint()) as [number, number, number];
      const arcLength = activeArcs.length;
      for (const arc of ancestor.arcs) {
        const result = dags[arc.axis].add(arc.from, arc.to, shouldCancel);
        if (result === "cancelled") {
          cancelled = true;
          ancestorsScratch.length = 0;
          return false;
        }
        if (result === "added") pushActiveArc(arc);
      }
      retainNode(ancestor);
      appliedPath.push({ node: ancestor, checkpoints, arcLength });
    }
    ancestorsScratch.length = 0;
    return true;
  };

  if (!cancelled && !baseInfeasible) {
    if (shouldCancel()) {
      cancelled = true;
    } else if (states >= maximumStates) {
      stateExhausted = true;
    } else {
      inspectCurrent(0, undefined);
    }
  }

  while (!cancelled && !stateExhausted && frontier.length > 0) {
    // The frontier reference becomes the current/applied reference until this
    // iteration either installs the frame or releases the rejected node.
    const node = frontier.popBest() as SearchNode<TScore>;
    if (shouldCancel()) {
      cancelled = true;
      releaseNode(node);
      break;
    }
    if (!switchToParentChain(node)) {
      releaseNode(node);
      break;
    }
    branches += 1;
    const checkpoints = dags.map((dag) => dag.checkpoint()) as [number, number, number];
    const arcLength = activeArcs.length;
    let alternativeResult: AddArcResult = "implied";
    for (const arc of node.arcs) {
      validateArc(arc, options.axisNodeCounts);
      const result = dags[arc.axis].add(arc.from, arc.to, shouldCancel);
      if (result === "cancelled" || result === "cycle") {
        alternativeResult = result;
        break;
      }
      if (result === "added") {
        alternativeResult = "added";
        pushActiveArc(arc);
      }
    }

    if (alternativeResult === "cancelled") {
      for (let axis = 0; axis < 3; axis += 1) dags[axis].rollback(checkpoints[axis]);
      truncateActiveArcs(arcLength);
      cancelled = true;
      releaseNode(node);
      break;
    }
    if (alternativeResult === "cycle") {
      for (let axis = 0; axis < 3; axis += 1) dags[axis].rollback(checkpoints[axis]);
      truncateActiveArcs(arcLength);
      cyclePrunes += 1;
      releaseNode(node);
      continue;
    }
    if (alternativeResult === "implied") {
      noOpPrunes += 1;
      releaseNode(node);
      continue;
    }

    appliedPath.push({ node, checkpoints, arcLength });
    if (shouldCancel()) {
      cancelled = true;
      break;
    }
    if (states >= maximumStates) {
      stateExhausted = true;
      break;
    }
    inspectCurrent(node.depth, node.scoreBasis);
  }

  // Cancellation dominates every completion claim, including base
  // infeasibility: a cut traversal proves nothing about the search space.
  const exhausted = stateExhausted || frontierPrunes > 0;
  const completed = !cancelled && (baseInfeasible || (!exhausted && frontier.length === 0));
  emitProgress(true);
  const result: ConstraintExtensionSearchResult<TCandidate, TScore> = {
    ...stats(),
    completed,
    cancelled,
    exhausted,
    frontierPrunes,
    peakFrontierNodes,
    peakLiveSearchNodes,
    hardInfeasible: completed && !hasBest,
  };
  // No search node escapes in the result. Releasing references explicitly
  // keeps repeated worker searches from relying on a later full GC cycle.
  for (const node of frontier.clear()) releaseNode(node);
  releaseAppliedPathFrom(0);
  if (hasBest) {
    return {
      ...result,
      best: best as TCandidate,
      bestScore: bestScore as TScore,
    };
  }
  if (hardExplanation) return { ...result, hardExplanation };
  return result;
}
