# nukefire-scripts

NukeFire command deck for Smudgy, built on
`smudgy://kapusniak/nukefire-gmcp`.

## Panels

| Panel | Purpose |
| --- | --- |
| **HUD** | Player and opponent vitals, status, and multi-session summaries. |
| **Affects** | Timed character and scanned-target effects. |
| **Comms** | Filterable channel feed with plain or full-ANSI rendering. |
| **Map** | Smudgy map with persistent visit history, a since-session-start trail, other-session locations, and the live GPS route accented in gold. |
| **Radar** | Interactive local BIGMAP view with exits, doors, and route overlays; disabled by default. |
| **Atlas** | Searchable GPS catalog; selecting a destination starts walking. |
| **Deck** | Context-sensitive service status and actions. |
| **Codex** | Searchable knowledge browser, disabled by default. |

## Configuration

Choose visible panels, chat rendering and font sizes in the package settings,
then reload the package. Chat uses Full ANSI and separates messages with empty
lines by default. Additional sessions use shared tabs by default, with Compact
or Wide vitals, and can instead be stacked on the right. The stacked layout
gives the central session a Wide vitals header and each right-column session a
Compact header. Both styles sit directly on the terminal-theme background
without an extra panel tint.

The Map dims unvisited rooms and their links to 40% opacity. It remembers
visits by the game's room numbers, so combining map sections keeps them. The
session trail and other-session outlines follow committed whole or partial
section merges, including room renumbering and duplicate-room joins. Numeric
game identities take precedence over old map addresses. Visits from before the
VNUM store are converted once; installations that already have that store
discard numeric legacy addresses because they may refer to a reused number.
The first conversion can only resolve surviving addresses: if a number was
already reused before conversion, the original game identity is unavailable.
The **view** menu can hide or clear the current session's trail and reset visit
history for the current area. Connected characters on the same map appear as distinct colored
room outlines; GPS styling takes precedence where it overlaps the trail.

Enable **Show live layout planner state below the map** to add a compact status
block beneath `MapView`. It shows the active phase and elapsed time, candidate
layouts/restarts/feasibility checks, and current-versus-best counts for ray,
reciprocal, route, room-obstruction, exit-port, reciprocal-port, and crossing
violations.

F1–F4 select and focus a session. Ctrl+F1–F4 magnifies stacked sessions or
selects the corresponding shared tab.

The primary session shows a dismissible first-run welcome with the same
multi-session controls. Reopen it at any time with `nf welcome`.

## Utilities

| Command | Action |
| --- | --- |
| `nf help` | Show the NukeFire Scripts utility and routing reference. |
| `nf find <name or VNUM>` | Find mapped rooms and show clickable routes from the current room. |
| `nf run <name, VNUM, or death> [commands]` | Walk to a mapped room, then run optional semicolon-separated commands. |
| `nf death` | Show the last death room and a clickable route back. |
| `nf path <FROM> <TO>` | Show a route between mapped rooms and publish it as `nfPath.commands`. |
| `nf welcome` | Reopen the welcome and multi-session guide. |
| `nf reflow` | Run a bounded, 30-second/8-pass reflow of the current area. |
| `nf reflow perfect` | Explicitly run the high-effort constraint search without an automatic wall deadline. |

The perfect form retains deterministic total-work and live-frontier ceilings,
then records its exact final geometry at a stronger effort tier. A zero-defect
result suppresses a redundant lower-budget automatic pass; non-perfect results
do not cross-certify the command's different anchor and lock set.

Both reflow commands send their validated plan to the mapping owner, which
rechecks the source and final room geometry, commits room moves with compatible
connection routing, and retains the engine's detours for fixed-room defects.
The command announces completion after the owner acknowledges the commit. A
missing acknowledgement reports an unknown result; inspect the map before
retrying. NukeFire Scripts 0.5.8 requires mapper 0.18.5 and Smudgy 0.5.8.

When **Show live layout planner state** is enabled, the panel also attributes
automatic work to its area, shows the terminal reason, and exposes map-free
Worker counts plus optional peak heap/RSS samples and retirement disposition.
Memory fields show `unavailable` when the Worker host supplies no sample.

Quote room names containing spaces in `run` and `path`. Routes use learned map
topology and special-exit commands, but intentionally do not open doors or
avoid hazardous rooms.

Examples:

- `nf find the temple of technology`
- `nf run 3001 look north;look south`
- `nf path "Central Plaza" "The Spaceport"`
