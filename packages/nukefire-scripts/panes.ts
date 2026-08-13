// Primary-session pane orchestration. Visibility is configured through package
// params; there is intentionally no in-session NF/nfui control strip.

import { createState, createTimer, session } from "smudgy:core";
import * as affects from "./affects.tsx";
import * as atlas from "./atlas.tsx";
import * as codex from "./codex.tsx";
import * as comms from "./comms.tsx";
import { panelVisibility, sessionLayout, type PanelKey } from "./config.ts";
import * as deck from "./deck.tsx";
import * as hud from "./hud.tsx";
import * as map from "./map.tsx";
import { getMagnifiedSession, startMultiSessionSupport } from "./multi.ts";
import * as radar from "./radar.tsx";
import * as vitals from "./vitals.tsx";
import * as welcome from "./welcome.tsx";

interface Panel {
  key: PanelKey;
  module: { open(): void; close(): void };
}

// Parents precede children. Deck is split beneath Affects; Atlas joins Map's
// tab group before Comms and Radar are split beneath it. Closing runs in
// reverse order.
const PANELS: readonly Panel[] = [
  { key: "hud", module: hud },
  { key: "affects", module: affects },
  { key: "deck", module: deck },
  { key: "map", module: map },
  { key: "atlas", module: atlas },
  { key: "comms", module: comms },
  { key: "radar", module: radar },
  { key: "codex", module: codex },
];

export const panesOpen = createState<Record<PanelKey, boolean>>("panesOpen");
panesOpen.set({ ...panelVisibility });

let primaryRole: boolean | undefined;
let displayedSessionId: number | undefined;

/** Keep the full and compact HUD variants attached to their visual roles. */
function syncHudForDisplay(force = false): void {
  if (!panelVisibility.hud) {
    hud.close();
    hud.closeMini();
    vitals.close();
    return;
  }

  if (sessionLayout === "tabbed") {
    hud.close();
    hud.closeMini();
    if (primaryRole) {
      vitals.open();
    } else {
      vitals.close();
    }
    return;
  }

  vitals.close();

  const displayed = getMagnifiedSession() ?? session;
  if (!force && displayed.id === displayedSessionId) return;
  displayedSessionId = displayed.id;

  if (displayed.id === session.id) {
    hud.closeMini();
    hud.open();
  } else {
    hud.close();
    hud.openMini();
  }
}

function applySessionRole(isPrimary: boolean): void {
  const previousRole = primaryRole;
  primaryRole = isPrimary;

  if (!isPrimary) {
    welcome.close();
    for (const panel of [...PANELS].reverse()) panel.module.close();
    syncHudForDisplay(true);
    return;
  }

  // A promoted stacked secondary should once again follow the user's normal
  // terminal font. Do not touch the original primary's existing preference.
  if (previousRole === false && sessionLayout === "stacked-right") {
    session.mainPane.setFontSize(null);
  }
  for (const panel of PANELS) {
    if (panel.key !== "hud" && panelVisibility[panel.key]) panel.module.open();
  }
  syncHudForDisplay(true);
  welcome.showFirstRun();
}

let started = false;

/** Start pane election after index.ts has initialized package-owned state. */
export function startPanes(): void {
  if (started) return;
  started = true;
  startMultiSessionSupport(applySessionRole);

  // Pane swaps are cross-session operations, while each package runtime owns
  // its own HUD widget. Poll only for the stacked layout where Ctrl+F1..F4 can
  // move a different session into this runtime's visual slot.
  if (sessionLayout === "stacked-right") {
    createTimer({ intervalMs: 200, repeat: true }, () => syncHudForDisplay());
  }
}
