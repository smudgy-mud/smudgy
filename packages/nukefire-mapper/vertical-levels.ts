import type { LayoutEdge, LayoutNode } from "./layout.ts";

interface VerticalRelation {
  other: string;
  delta: 1 | -1;
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
 * game reported. Levels between established rooms are never changed here, and
 * a forced stack that would make two new rooms share a chart cell is
 * abandoned rather than risking an unplaceable rigid chart.
 */
export function stackVerticalTraversals(
  nodes: readonly LayoutNode[],
  edges: readonly LayoutEdge[],
  establishedLevels: ReadonlyMap<string, number>,
  centerId?: string,
): LayoutNode[] {
  const byId = new Map(nodes.map((node) => [node.id, node]));
  const forced = new Map<string, number>();

  // Anchor the durable mirror in chart space at one reference room so charts
  // whose z already matches the durable map are left untouched.
  const established = [...establishedLevels.keys()]
    .filter((id) => byId.has(id))
    .sort((a, b) => {
      if (a === centerId) return -1;
      if (b === centerId) return 1;
      return a.localeCompare(b);
    });
  if (established.length > 0) {
    const reference = established[0];
    const base = (byId.get(reference) as LayoutNode).relative.level;
    const referenceLevel = establishedLevels.get(reference) as number;
    for (const id of established) {
      forced.set(id, base + (establishedLevels.get(id) as number) - referenceLevel);
    }
  }

  const relations = new Map<string, VerticalRelation[]>();
  const relate = (id: string, other: string, delta: 1 | -1): void => {
    const values = relations.get(id) ?? [];
    values.push({ other, delta });
    relations.set(id, values);
  };
  for (const edge of edges) {
    if (edge.from === edge.to || !byId.has(edge.from) || !byId.has(edge.to)) continue;
    if (edge.direction === "Up") {
      relate(edge.from, edge.to, 1);
      relate(edge.to, edge.from, -1);
    } else if (edge.direction === "Down") {
      relate(edge.from, edge.to, -1);
      relate(edge.to, edge.from, 1);
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
  const roots = [...relations.keys()]
    .filter((id) => !forced.has(id))
    .sort((a, b) => {
      if (a === centerId) return -1;
      if (b === centerId) return 1;
      return a.localeCompare(b);
    });
  for (const root of roots) {
    if (forced.has(root)) continue;
    enqueue(root, (byId.get(root) as LayoutNode).relative.level);
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
