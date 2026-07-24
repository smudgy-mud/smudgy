# auto-mapper

Maps as you explore, from the room data your game already sends. Works with GMCP
(`Room.Info` — Aardwolf, IRE games, and most modern MUDs) and MSDP (`ROOM` or the flat
`ROOM_*` variables).

- **Rooms you've mapped are followed** — the map pane tracks your position, and speedwalks
  work over everything you've explored.
- **Rooms you haven't are drawn for you**, in *session maps*: one per game zone, kept only
  for this session, never written into your saved maps.
- **`savemap`** keeps what you've mapped (or `savemap <zone>` for one zone): the session
  map becomes a normal local map and mapping continues into it.
- Unexplored exits appear as stubs and connect themselves when you get there. Rooms placed
  by server coordinates when the game provides them, by your movement when it doesn't.
- Mazes and other rooms where the game withholds identity are left alone — the mapper
  never guesses.

Development notes (not user docs): this is the first-party reference consumer of the
ephemeral map tier, `externalId` room identity, and the dual-protocol room-data producers
(`docs/gmcp-mapping.md` §5.3). It runs sandboxed under
`interop:read + mapper:write + automations:aliases + session:echo + gmcp:send`; the e2e
coverage lives in `core/tests/auto_mapper_package.rs`.
