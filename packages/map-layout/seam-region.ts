/**
 * The seam region: the rooms a quick polish of freshly joined map sections
 * may move, while every other room stays where it is. Joining sections leaves
 * its defects where they meet, so polishing the rooms around those seams first
 * shows a much better map long before a polish of the whole map finishes.
 */

import type { GridPosition, LayoutEdge } from "./layout.ts";

/**
 * How near a link a room must lie to count as on or next to it: the
 * Chebyshev distance, in cells, from the center of the room's cell to the
 * link's segment. One cell takes in the cells the link passes through and
 * their neighbours; the rest takes in the cells a slanted link passes close to.
 */
export const SEAM_LINK_REACH = 1.32;

type Link = readonly [string, string];

/**
 * The rooms a seam polish may move, given where every room is, the exits
 * between them, and the seeds: the rooms at either end of the links that
 * joined two sections. The region holds
 *
 * - the seeds;
 * - the rooms whose cells lie on or next to a link between two seeds (within
 *   `SEAM_LINK_REACH` of its segment, on its level);
 * - the rooms on or next to any link between two rooms of that set, so the
 *   polish can move the rooms that block those links;
 * - every room that shares its cell with another: the planner cannot lay out
 *   a map in which two rooms it may not move collide.
 *
 * Positions are integral cells. Seeds without a position are ignored, and so
 * are exits to rooms without one and exits from a room to itself; a link
 * between two levels has no segment on either, so no room lies next to it.
 * The result depends only on its inputs as sets: neither the order of the
 * positions, of the edges nor of the seeds changes it.
 */
export function seamRegion(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly Pick<LayoutEdge, "from" | "to">[],
  seeds: Iterable<string>,
): Set<string> {
  const cells = new Map<string, string[]>();
  const byLevel = new Map<number, [string, GridPosition][]>();
  for (const [id, position] of positions) {
    const key = cellKey(position.x, position.y, position.level);
    const occupants = cells.get(key);
    if (occupants) occupants.push(id);
    else cells.set(key, [id]);
    const onLevel = byLevel.get(position.level);
    if (onLevel) onLevel.push([id, position]);
    else byLevel.set(position.level, [[id, position]]);
  }
  const links = physicalLinks(positions, edges);

  const region = new Set<string>();
  for (const seed of seeds) {
    if (positions.has(seed)) region.add(seed);
  }
  const addNear = (link: Link, into: Set<string>): void => {
    const [a, b] = link;
    const from = positions.get(a) as GridPosition;
    const to = positions.get(b) as GridPosition;
    if (from.level !== to.level) return;
    const minX = Math.min(from.x, to.x) - 1;
    const maxX = Math.max(from.x, to.x) + 1;
    const minY = Math.min(from.y, to.y) - 1;
    const maxY = Math.max(from.y, to.y) + 1;
    const onLevel = byLevel.get(from.level) ?? [];
    // Scan the link's box cell by cell, or the level's rooms when the box
    // holds more cells than the level holds rooms: a long slanted link
    // crosses a large box that is mostly empty.
    if ((maxX - minX + 1) * (maxY - minY + 1) <= onLevel.length) {
      for (let x = minX; x <= maxX; x += 1) {
        for (let y = minY; y <= maxY; y += 1) {
          for (const id of cells.get(cellKey(x, y, from.level)) ?? []) {
            if (id !== a && id !== b && segmentMeetsSquare(from, to, x, y, SEAM_LINK_REACH)) {
              into.add(id);
            }
          }
        }
      }
      return;
    }
    for (const [id, position] of onLevel) {
      if (id === a || id === b) continue;
      if (position.x < minX || position.x > maxX || position.y < minY || position.y > maxY) continue;
      if (segmentMeetsSquare(from, to, position.x, position.y, SEAM_LINK_REACH)) into.add(id);
    }
  };

  const seedSet = new Set(region);
  for (const link of links) {
    if (seedSet.has(link[0]) && seedSet.has(link[1])) addNear(link, region);
  }
  const core = new Set(region);
  for (const link of links) {
    if (core.has(link[0]) && core.has(link[1])) addNear(link, region);
  }
  for (const occupants of cells.values()) {
    if (occupants.length > 1) for (const id of occupants) region.add(id);
  }
  return region;
}

function cellKey(x: number, y: number, level: number): string {
  return `${x},${y},${level}`;
}

/** Each pair of distinct rooms an exit joins, once, whichever way the exits run. */
function physicalLinks(
  positions: ReadonlyMap<string, GridPosition>,
  edges: readonly Pick<LayoutEdge, "from" | "to">[],
): Link[] {
  const seen = new Set<string>();
  const links: Link[] = [];
  for (const edge of edges) {
    if (edge.from === edge.to || !positions.has(edge.from) || !positions.has(edge.to)) continue;
    const link: Link = edge.from < edge.to ? [edge.from, edge.to] : [edge.to, edge.from];
    const key = JSON.stringify(link);
    if (seen.has(key)) continue;
    seen.add(key);
    links.push(link);
  }
  return links;
}

/**
 * Whether the segment between two cell centers meets the axis-aligned square
 * of half-size `half` around (`cx`, `cy`): whether the Chebyshev distance
 * from that point to the segment is at most `half` (Liang–Barsky clipping).
 */
function segmentMeetsSquare(
  from: GridPosition,
  to: GridPosition,
  cx: number,
  cy: number,
  half: number,
): boolean {
  const dx = to.x - from.x;
  const dy = to.y - from.y;
  let enter = 0;
  let leave = 1;
  const clips: readonly (readonly [number, number])[] = [
    [-dx, from.x - (cx - half)],
    [dx, cx + half - from.x],
    [-dy, from.y - (cy - half)],
    [dy, cy + half - from.y],
  ];
  for (const [p, q] of clips) {
    if (p === 0) {
      if (q < 0) return false;
      continue;
    }
    const ratio = q / p;
    if (p < 0) enter = Math.max(enter, ratio);
    else leave = Math.min(leave, ratio);
    if (enter > leave) return false;
  }
  return true;
}
