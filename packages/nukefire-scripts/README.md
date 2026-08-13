# nukefire-scripts

NukeFire command deck for Smudgy, built on
`smudgy://kapusniak/nukefire-gmcp`.

## Panels

| Panel | Purpose |
| --- | --- |
| **HUD** | Player and opponent vitals, status, and multi-session summaries. |
| **Affects** | Timed character and scanned-target effects. |
| **Comms** | Filterable channel feed with plain or full-ANSI rendering. |
| **Map** | Smudgy map with the live GPS route accented in gold. |
| **Radar** | Interactive local BIGMAP view with exits, doors, and route overlays; disabled by default. |
| **Atlas** | Searchable GPS catalog; selecting a destination starts walking. |
| **Deck** | Context-sensitive service status and actions. |
| **Codex** | Searchable knowledge browser, disabled by default. |

## Configuration

Choose visible panels, chat rendering and font sizes in the package settings,
then reload the package. Chat uses Full ANSI by default. Additional sessions
use shared tabs by default, with Compact or Wide vitals, and can instead be
stacked on the right. The stacked layout gives the central session a Wide
vitals header and each right-column session a Compact header. Both styles sit
directly on the terminal-theme background without an extra panel tint.

F1–F4 select and focus a session. Ctrl+F1–F4 magnifies stacked sessions or
selects the corresponding shared tab.

The primary session shows a dismissible first-run welcome with the same
multi-session controls. Reopen it at any time with `nf welcome`.

## Utilities

| Command | Action |
| --- | --- |
| `nf help` | Show the NukeFire Scripts utility and routing reference. |
| `nf welcome` | Reopen the welcome and multi-session guide. |
| `nf reflow` | Thoroughly reflow the current area across multiple violation-prioritized anchors. |
