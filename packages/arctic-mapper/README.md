# ArcticMUD Auto-Mapper for smudgy

Smudgy support for mapping rooms in ArcticMUD

## Modes

| Command | Mode |
|---|---|
| `map off` | Mapper does nothing |
| `map follow` | Tracks your position on the existing map; never modifies it |
| `map active` | Like follow, but `n/e/s/w/u/d` consult the map: closed doors are opened first, and exits with a movement command send that instead |
| `map record` | Like follow, but unknown rooms are created and linked as you move |

Movement is captured from the `n/s/e/w/u/d` command aliases (abbreviations
supported) and from `You follow <name> <direction>.` messages, then matched
against the next room the MUD sends. Failed moves ("Alas, you cannot go that
way...") pop the pending move so the queue stays in sync.

## Map & Notes panes

The mapper's UI lives in two panes split off the right of the session when the
package loads: **Map** (the live map, filling the pane) stacked above **Notes**
(area/room headings, notes, flags, and the notes editor). Resize either with
the dividers or drag them elsewhere in the grid; both persist across script
reloads.

## Settings

Configured in the package's options (prompted at install; editable any time and
applied on reload):

| Option | Choices | Effect |
|---|---|---|
| **Mapper mode on load** | Active / Follow / Off | Which mode the mapper starts in. Active and Follow match `map active` / `map follow`; you can still switch live with the `map` commands above. (`map record` stays a live-only mode.) |
| **Map panel text size** | Extra small / Small / Medium / Large / Extra large | Base text size for the map panel's headings, room/area notes (Markdown), button labels, and the notes editor. Flags render a quarter smaller. Medium (16 px) is the prior built-in default. |
| **Map panel widget size (px)** | Any number | The initial size of the map/notes panes: the panes' width and the Map pane's height, in pixels (the map starts out square, then simply fills its pane as you move the dividers). Defaults to 350 (the prior built-in size); a non-positive value falls back to the default. |
| **Vertical movement display** | Auto / Normal / Isometric | Creation preference for new `up`/`down` links. **Auto** infers the nearest mapped section. **Normal** uses separate map levels. **Isometric** keeps the rooms on one level and lets the layout pass choose NE/NW or SE/SW from the surrounding geometry. Existing links retain the style encoded by their endpoint levels. |

## Command reference

```
map help                         Show help
map area                         List areas
map area create <name>           Create a durable area (cloud when signed in, otherwise local)
map select <area> [room]         Select the current area and room
map rooms                        List rooms in the selected area
map move <direction>             Move the selection along an existing exit
map path <from> <to>             Path between two rooms (tags: <number> or <area>#<number>)
map push <direction>             Shift the selected room and everything connected, one step
map reflow                       Thoroughly search and reflow the current area
map z [auto|levels|projected]    Show or change the live U/D creation preference
map refresh                      Re-look and re-capture the current room
map debug [on|off]               Toggle diagnostic logging of capture/tracking decisions

map room move <direction>        Same as map move
map room color <color>           Set the room color
map room link <dir> <tag> [oneway]   Link the selected room to another
map room unlink <direction>      Delete exits in a direction
map room shift <direction>       Move the selected room one step on the grid
map room delete                  Delete the selected room
map room show                    Print title/description/coords/exits
map room flag set|clear <flag>   Set or clear a room flag (e.g. SPIN)
map room automerge               Merge the nearest duplicate room into this one

map exit <dir>                   Show the exit's commands and flags
map exit <dir> open <cmd>        Command sent before moving (e.g. "open n", "part brush")
map exit <dir> command <cmd>     Command sent instead of the direction (e.g. "enter hole")
map exit <dir> clear open|command    Remove the open/movement command
map exit <dir> closed|hidden|locked [true|false]   Set exit flags
```

When recording creates a room, the mapper snapshots the area and runs the
standard integral-grid layout pass. Correct directional lines are protected
before crossings, link length, or compactness are considered. `map reflow`
adds the bounded thorough tournament: it compares the selected room, an
unanchored result, rooms around remaining directional violations, and other
structural anchors before applying one winning patch. The snapshot exists
only for that operation; ordinary movement does not maintain or recompute a
shadow map.

Before creating a look-alike room, recording checks the current area for an
exact title/description match with an unconnected exit back toward the room
you left. If it finds one, the map panel asks whether to rearrange and connect
to that room or keep the rooms separate and create a new one.

## Speedwalks

`>` walks you to the nearest room matching what follows, along the cheapest
mapped route (closed doors are opened on the way, movement commands used):

```
>inn              Nearest room flagged INN
>inn.peace        Nearest room flagged BOTH INN and PEACE
>!peace.guild     Nearest GUILD room that is NOT PEACE
>dr               Tags expand by prefix within the current area (e.g. DRUID_GUILD)
>@sol             Nearest room of the first area whose NAME starts with "sol"
```

Tag speedwalks (`>inn`) match room flags set with `map room flag set <FLAG>`.
Area speedwalks (`>@sol`) match area names instead — case-insensitive, exact
name beating a longer prefix — and take no tag filter.

Three ways to traverse an exit, and how to model each:

| You'd normally type | Model it as |
|---|---|
| `n` | Nothing — a plain exit |
| `open n` then `n` | `map exit n closed true` (default open command is `open n`) |
| `part brush` then `n` | `map exit n open part brush` |
| `enter hole` (no direction works) | `map exit n command enter hole` |

In `map active` mode, typing `n` (or any abbreviation) checks the mapped
exit: the open command goes out first when the exit is flagged closed or has
an explicit open command, then the direction — or the exit's movement command
in its place. Multi-step commands can be separated with `;`. The move is
still recorded as a step in that direction, so tracking works as usual.
Anything sent through aliases/speedwalks that types directions benefits too;
raw sends bypass it.

Live door state is read from the prompt: in `Exits:(E)S>`, the parenthesized
E exit is currently closed. Active mode uses this to skip the open command
when the prompt shows the door already open, and to open doors the map
doesn't know about; hidden exits never appear in the prompt, so their open
commands are always sent. `map record` and `map refresh` flag parenthesized
exits as closed when creating them.

Open commands are stored as room properties named `open_<d>_command`
(e.g. `open_n_command`); movement commands live on the exit itself.

The `SPIN` flag matters to following: rooms flagged `SPIN` let the movement
tracker consider every exit, not just the direction you moved.  Generally,
if you _land_ in a seemingly-random room, you should flag the room you
came from as `SPIN`, not the room you ended up in.
