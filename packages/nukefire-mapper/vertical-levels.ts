import type { GridPosition, LayoutEdge, LayoutNode } from "./layout.ts";

interface VerticalRelation {
  other: string;
  delta: number;
}

/**
 * Rewrite a player-relative chart so vertical traversals always span levels.
 *
 * NukeFire's Map.Local may flow an up/down destination on its source's z
 * plane. Established rooms keep their durable placement: their chart levels
 * are rewritten to mirror durable level differences, so a reflow can re-embed
 * the chart without pulling an existing stack back onto one plane. Rooms new
 * to the map are then walked outward from those seeds along Up/Down edges and
 * forced one level above or below the room that reached them, whatever z the
 * game reported. An established endpoint does not have to be present in the
 * chart: Room.Info vertical exits normally cross from the current Map.Local
 * plane to a durable resident which Map.Local cannot include. Once that seam
 * fixes one chart node's level, ordinary chart edges carry the same level
 * through the rest of that observed plane. Their raw Map.Local z values are
 * not authoritative: only Up/Down traversals may change a room's level.
 * Levels between established rooms are never changed here, and a forced
 * stack that would make two new rooms share a chart cell is abandoned rather
 * than risking an unplaceable rigid chart. When a returning chart has no
 * durable seam at all, `unanchoredLevel` places its center on Map.Local's
 * server-reported plane instead of silently merging it into level zero.
 */
export function stackVerticalTraversals(
  nodes: readonly LayoutNode[],
  edges: readonly LayoutEdge[],
  establishedLevels: ReadonlyMap<string, number>,
  centerId?: string,
  unanchoredLevel = 0,
): LayoutNode[] {
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const forced = new Map<string, number>();

  const edgeIds = new Set<string>();
  for (const edge of edges) {
    edgeIds.add(edge.from);
    edgeIds.add(edge.to);
  }

  // Anchor the durable mirror in chart space at one reference room so charts
  // whose z already matches the durable map are left untouched. When the
  // chart contains no established room, use durable levels directly; the
  // layout seam will translate x/y while preserving these absolute levels.
  const established = [...establishedLevels.keys()]
    .filter((id) => byId.has(id) || edgeIds.has(id))
    .sort((a, b) => {
      if (a === centerId) return -1;
      if (b === centerId) return 1;
      return a.localeCompare(b);
    });
  const chartEstablished = established.filter((id) => byId.has(id));
  if (chartEstablished.length > 0) {
    const reference = chartEstablished[0];
    const base = (byId.get(reference) as LayoutNode).relative.level;
    const referenceLevel = establishedLevels.get(reference) as number;
    for (const id of established) {
      forced.set(id, base + (establishedLevels.get(id) as number) - referenceLevel);
    }
  } else {
    for (const id of established) {
      forced.set(id, establishedLevels.get(id) as number);
    }
  }

  const relations = new Map<string, VerticalRelation[]>();
  const relate = (id: string, other: string, delta: number): void => {
    const values = relations.get(id) ?? [];
    values.push({ other, delta });
    relations.set(id, values);
  };
  for (const edge of edges) {
    if (edge.from === edge.to) continue;
    const fromKnown = byId.has(edge.from) || establishedLevels.has(edge.from);
    const toKnown = byId.has(edge.to) || establishedLevels.has(edge.to);
    if (!fromKnown || !toKnown) continue;
    if (edge.direction === "Up") {
      relate(edge.from, edge.to, 1);
      relate(edge.to, edge.from, -1);
    } else if (edge.direction === "Down") {
      relate(edge.from, edge.to, -1);
      relate(edge.to, edge.from, 1);
    } else if (byId.has(edge.from) && byId.has(edge.to)) {
      // Map.Local's ordinary chart edges never cross a level boundary. Treat
      // their z observations as noise and carry one plane through the whole
      // connected chart; only explicit Up/Down traversals introduce +/-1.
      relate(edge.from, edge.to, 0);
      relate(edge.to, edge.from, 0);
    }
  }
  for (const values of relations.values()) {
    values.sort((a, b) => a.other.localeCompare(b.other) || a.delta - b.delta);
  }

  // Breadth-first assignment keeps every new room's level one step from the
  // room that reached it; a room claimed by two conflicting vertical paths
  // deterministically keeps its first (nearest-seed) assignment.
  const queue = established.filter((id) => relations.has(id));
  const enqueue = (id: string, level: number): void => {
    if (forced.has(id)) return;
    forced.set(id, level);
    queue.push(id);
  };
  const drain = (): void => {
    while (queue.length > 0) {
      const id = queue.shift() as string;
      const level = forced.get(id) as number;
      for (const relation of relations.get(id) ?? []) {
        if (establishedLevels.has(relation.other)) continue;
        enqueue(relation.other, level + relation.delta);
      }
    }
  };
  drain();
  const roots = [...new Set([...nodes.map((node) => node.id), ...relations.keys()])]
    .filter((id) => !forced.has(id))
    .sort((a, b) => {
      if (a === centerId) return -1;
      if (b === centerId) return 1;
      return a.localeCompare(b);
    });
  for (const root of roots) {
    if (forced.has(root)) continue;
    const relativeLevel = byId.get(root)?.relative.level ?? 0;
    enqueue(root, unanchoredLevel + relativeLevel);
    drain();
  }

  const rewritten = (applies: (id: string) => boolean): LayoutNode[] =>
    nodes.map((node) => {
      const level = forced.get(node.id);
      return level === undefined || level === node.relative.level || !applies(node.id)
        ? node
        : { ...node, relative: { ...node.relative, level } };
    });

  const result = rewritten(() => true);
  const cells = new Set<string>();
  for (const node of result) {
    if (establishedLevels.has(node.id)) continue;
    const { x, y, level } = node.relative;
    const cell = `${level}:${x}:${y}`;
    if (cells.has(cell)) return rewritten((id) => establishedLevels.has(id));
    cells.add(cell);
  }
  return result;
}

/**
 * The general layout engine packs a chart component with no durable seam onto
 * a zero-based level stack. Restore the absolute server-plane levels prepared
 * above after that collision-free x/y placement. Components touching any
 * durable room keep the planner's anchored levels. If restoring a component
 * would create a cell collision, retain the validated planner result.
 */
export function restoreUnanchoredChartLevels(
  positions: ReadonlyMap<string, GridPosition>,
  nodes: readonly LayoutNode[],
  edges: readonly LayoutEdge[],
  establishedLevels: ReadonlyMap<string, number>,
  centerId?: string,
): ReadonlyMap<string, GridPosition> {
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const nodeIds = new Set(byId.keys());
  const adjacency = new Map<string, Set<string>>(
    nodes.map((node) => [node.id, new Set<string>()]),
  );
  const seamNodes = new Set<string>();
  for (const edge of edges) {
    const fromNode = nodeIds.has(edge.from);
    const toNode = nodeIds.has(edge.to);
    if (fromNode && toNode) {
      adjacency.get(edge.from)?.add(edge.to);
      adjacency.get(edge.to)?.add(edge.from);
    } else if (fromNode && establishedLevels.has(edge.to)) {
      seamNodes.add(edge.from);
    } else if (toNode && establishedLevels.has(edge.from)) {
      seamNodes.add(edge.to);
    }
  }

  const restored = new Map(positions);
  const visited = new Set<string>();
  let changed = false;
  const roots = [...nodeIds].sort((a, b) => {
    if (a === centerId) return -1;
    if (b === centerId) return 1;
    return a.localeCompare(b);
  });
  for (const root of roots) {
    if (visited.has(root)) continue;
    const component: string[] = [];
    const queue = [root];
    visited.add(root);
    while (queue.length > 0) {
      const id = queue.shift() as string;
      component.push(id);
      for (const other of adjacency.get(id) ?? []) {
        if (visited.has(other)) continue;
        visited.add(other);
        queue.push(other);
      }
    }
    if (component.some((id) => establishedLevels.has(id) || seamNodes.has(id))) continue;
    component.sort((a, b) => {
      if (a === centerId) return -1;
      if (b === centerId) return 1;
      return a.localeCompare(b);
    });
    const reference = component.find((id) => restored.has(id) && byId.has(id));
    if (!reference) continue;
    const offset = (byId.get(reference) as LayoutNode).relative.level -
      (restored.get(reference) as GridPosition).level;
    if (offset === 0) continue;
    for (const id of component) {
      const position = restored.get(id);
      if (position) restored.set(id, { ...position, level: position.level + offset });
    }
    changed = true;
  }
  if (!changed) return positions;

  const cells = new Set<string>();
  for (const position of restored.values()) {
    const cell = `${position.level}:${position.x}:${position.y}`;
    if (cells.has(cell)) return positions;
    cells.add(cell);
  }
  return restored;
}
