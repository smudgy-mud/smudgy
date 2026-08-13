// =============================================================================
//  `nf` command-deck utilities
// =============================================================================

import { createAlias, echo, mapper, style } from "smudgy:core";
import { compareLayoutQuality, planAreaChange } from "smudgy://kapusniak/map-layout";
import * as welcome from "./welcome.tsx";

let reflowing = false;

function qualitySummary(quality: {
  cardinalRayViolations: number;
  roomObstructions: number;
  linkCrossings: number;
}): string {
  return `${quality.cardinalRayViolations} directional / ` +
    `${quality.roomObstructions} obstructed / ${quality.linkCrossings} crossing`;
}

export function showHelp(): void {
  echo("");
  echo(style.yellow`NukeFire Scripts utilities`);
  echo("  nf help     Show this help text");
  echo("  nf welcome  Open the welcome and multi-session guide");
  echo("  nf reflow   Thoroughly search and reflow the current area");
  echo("");
  echo("Session routing:");
  echo("  F1..F4 / Ctrl+F1..F4  Select or magnify a session");
  echo("  134 look              Send from sessions 1, 3, and 4");
  echo("  * look                Send from every session");
  echo("  -4 look               Send from every session except 4");
}

async function reflowCurrentArea(): Promise<void> {
  if (reflowing) {
    echo(style.warn`[nf] A map reflow is already running.`);
    return;
  }
  const location = mapper.getCurrentLocation();
  if (!location) {
    echo(style.warn`[nf] No mapped room is selected.`);
    return;
  }

  reflowing = true;
  try {
    echo("[nf] Searching violation-prioritized anchors for a better layout…");
    const result = await planAreaChange(location.area, {
      type: "reflow",
      anchor: location.room,
    }, {
      effort: "thorough",
      // Honor both map-layout's generic lock conventions and the property used
      // by nukefire-mapper's automatic planner.
      isRoomMovable: (room) =>
        !room.hasTag("LAYOUT_LOCKED") &&
        room.data("layoutLocked") !== "true" &&
        room.data("nukefire.layout.locked") !== "true",
    });
    const updates: [RoomNumber, UpdateRoomParams][] = result.patch.moves.map((move) => {
      if (move.roomNumber === undefined) {
        throw new Error(`layout move ${move.id} has no Smudgy room number`);
      }
      return [move.roomNumber, {
        x: move.to.x,
        y: move.to.y,
        level: move.to.level,
      }];
    });
    if (updates.length > 0) {
      await mapper.mutateArea(location.area, (mutation) => mutation.updateRooms(updates), {
        description: "Reflow NukeFire rooms",
      });
    }
    const search = result.search;
    const improvementText = search && compareLayoutQuality(result.quality, search.baselineQuality) > 0
      ? ` Improved the regular anchored score (${qualitySummary(search.baselineQuality)} → ` +
        `${qualitySummary(result.quality)}).`
      : "";
    const searchText = search
      ? ` Tried ${search.anchorsTried.length} anchors across ${search.planningPasses} passes; ` +
        `selected ${search.selectedAnchor === null ? "the unanchored result" : `room ${search.selectedAnchor} as anchor`}.`
      : "";
    echo(
      `[nf] Thorough reflow moved ${updates.length} room${updates.length === 1 ? "" : "s"}; ` +
        `${result.quality.cardinalRayViolations} directional violation${
          result.quality.cardinalRayViolations === 1 ? "" : "s"
        } remain.${improvementText}${searchText}`,
    );
  } catch (caught) {
    const message = caught instanceof Error ? caught.message : String(caught);
    echo(style.warn`[nf] Reflow failed: ${message}`);
  } finally {
    reflowing = false;
  }
}

createAlias(/^nf(?:\s+(?<args>.*))?$/i, ({ args }) => {
  const command = (args ?? "").trim().split(/\s+/, 1)[0]?.toLowerCase() || "help";
  switch (command) {
    case "help":
      showHelp();
      break;
    case "welcome":
      welcome.open();
      break;
    case "reflow":
      void reflowCurrentArea();
      break;
    default:
      echo(style.warn`[nf] Unknown utility “${command}”.`);
      showHelp();
      break;
  }
}, { name: "nf-utilities" });
