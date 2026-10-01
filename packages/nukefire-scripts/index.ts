// =============================================================================
//  nukefire-scripts — the NukeFire command deck
// =============================================================================
//  Panels and widgets over smudgy://kapusniak/nukefire-gmcp:
//
//    HUD      per-session strip, or aggregate pane above shared session tabs
//    Affects  live effects with local countdowns + a combat target scanner
//    Comms    dropdown-filtered channel traffic in a compact native terminal
//    Map      smudgy MapView with a bound room header and live GPS strip
//    Radar    the server's BIGMAP grid on a Canvas: terrain cells, route in
//             gold, closed doors dashed red, pulsing you-are-here marker
//    Atlas    the GPS destination catalog, filterable, click-to-travel
//    Deck     NukeFire.Context service cards with confirmable actions
//    Codex    a Knowledge-console search browser (`codex <query>`)
//
//  The oldest same-server session owns these panels. Package params choose
//  which panels load and whether later sessions stack right or share tabs.

import { createState } from "smudgy:core";
import "./commands.ts";
import "./session-routing.ts";
import { startPanes } from "./panes.ts";
import * as map from "./map.tsx";
import * as vitals from "./vitals.tsx";
import type { SessionMapSnapshot } from "./map-activity.ts";
import type { VitalsSnapshot } from "./vitals.tsx";

/** Retained per-session vitals, read cross-session through `.from(session)`. */
export const sessionVitals = createState<VitalsSnapshot>("sessionVitals");
/** Retained location and since-session-start trail, read across sessions. */
export const sessionMap = createState<SessionMapSnapshot>("sessionMap");

// Static imports evaluate first; initialize only after the exported state
// handle exists, then elect the primary and build its panes.
vitals.initialize();
map.initialize();
startPanes();

// Shared state for other packages (smudgy:state/kapusniak/nukefire-scripts):
export { panesOpen } from "./panes.ts";
export { hudMeta } from "./hud.tsx";
export { radarScene } from "./radar.tsx";
export { lastDeath, nfPath } from "./commands.ts";
