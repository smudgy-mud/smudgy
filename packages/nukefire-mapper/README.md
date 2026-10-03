# NukeFire Mapper

`smudgy://kapusniak/nukefire-mapper` reads NukeFire's GMCP data and
automatically builds the map shown in Smudgy's map widget.

As you explore, it adds rooms, exits, terrain, doors, and vertical connections.
A room you reach by going up or down is always mapped one level above or below
the room you left, even when the game charts both on the same plane. Rooms a
zone re-entry charts with no connection to any mapped room keep the level
`Map.Local.plane` gives them, so a newly discovered floor is not merged into
level zero. Before reflow changes levels, it puts affected routed links into a
compatible display mode; generated routes are recomputed from the committed
room coordinates. Author-drawn waypoints remain protected.
The mapper also follows your current location and displays GPS routes. New
maps are saved in a local `Nukefire` atlas, survive restarts, and are not
synced to the cloud.
Automatic mapping only adopts local areas. Existing cloud maps are left
untouched; moving a NukeFire map to cloud storage is always a user action.

Movement into an already mapped room updates the player marker immediately.
New rooms and exits are placed promptly without moving established rooms; an
expensive full reflow is deferred until map updates have been quiet for 350 ms.
A newer update cancels an obsolete full reflow without dropping the distinct
room snapshots still waiting to be mapped.

Automatic quiet reflow is intentionally bounded: it has a 10-second wall
deadline and finite ceilings of 4,096 constraint restarts, two layouts, two
polish tournaments within three aggregate passes, 32,768 separator states, 64
mask variants, 512 crossing expansions, and at most 4,096 live separator-search
nodes. It stops earlier at a geometric fixed point, skips the repair when the
ordinary layout has no directional, route, or crossing violation, and fresh
movement cancels an obsolete search immediately.

Use `nf reflow perfect` or `nfmap tidy` when you deliberately want the
high-effort search. That explicit policy has no wall deadline. Its
deterministic work ceilings instead shrink smoothly as the area grows: each
budget scales with the reciprocal of residents × (residents + edges), so
neighboring map sizes always receive neighboring budgets, with no cliffs. The
smallest maps retain up to 4,194,304 separator states and 32,768 live-search
nodes; at the largest documented shape every budget rests on its floor —
32,768 states, 32,768 constraint restarts, two complete polish layouts, a
two-tournament ceiling within three aggregate planner passes, 4,096 live-search
nodes, 64 mask variants, and 512 crossing expansions. These limits bound
retained search state while remaining independent of machine speed. The final
result is recorded at a stronger effort tier. A genuinely zero-defect result
subsumes the lower automatic pass; a fixed point or ceiling under the
command's different anchor/lock set remains evidence but does not silence a
feasible automatic improvement. Both manual reflow commands send the mapping
owner the validated source and final snapshot keys, room moves, and advisory detours.
The owner rejects a changed source, checks room locks and final coordinates,
then commits the layout and records any perfect-effort settlement against the
live final model. The command announces completion after its correlated
acknowledgement; a missing receipt reports an unknown result.

The mapper stores `nukefire.layout.polish-pending` on an area whenever prompt
topology work or an unsettled quiet pass leaves possible polish work, and
returning to that area passively schedules one new quiet attempt for the
visit. Movement that stays inside the area — repeated GMCP chatter or walking
between its rooms — re-arms that attempt rather than forfeiting it, so the
search resumes at the next quiet window; only leaving the area defers the
work. A visit spends at most eight such resumptions without a committed
improvement; every durable improvement, new topology, or re-entry restores
the allowance. A settled outcome clears the hint. Combining map sections also
stores `nukefire.layout.polish-seams`, the rooms at the seams where they meet
(the newest 512), which the next quiet pass polishes first (see
[One map per area](#one-map-per-area)); a completed quiet pass clears it. The
decision log records each seam round (`layout-seam-round`).

To avoid repeating expensive deterministic work, the legacy-named
`nukefire.layout.polish-exhausted-fingerprint` stores versioned settlement
evidence: final geometry, a schema-independent search generation, achieved
automatic effort, exact policy identity, diagnostic entry/chart coverage, and
terminal reason. Already-perfect, fixed-point, and deterministic-ceiling
outcomes settle the current geometry area-wide, including a pass which improved
it before reaching its ceiling, and so does a pass that left the map as it was
because the repair could not search it: exits the search cannot express or that
contradict each other, or no layout that keeps them. Returning through another
entrance does not start the same automatic search again. New topology or
geometry, a newer planner generation, a larger automatic effort, or changed
work policy makes the area eligible again. Timeout, cancellation, error, and
incomplete outcomes remain retryable, but persist an exponential cooldown (15
minutes initially, capped at 24 hours) so rapid area visits and package
restarts cannot create perpetual work. Pre-v3 values are read safely but do
not suppress the first pass. When the repair could not search, the decision
log records why (`layout-polish-not-searched`) and whether the map is now
settled.

Every strict best-so-far layout is applied while that search continues. Map
writes are serialized and naturally coalesce to the newest pending candidate,
so a slow durable write cannot build an unbounded improvement backlog. The
first improvement of a search applies immediately; later progressive commits
wait at least 1.5 seconds after the previous one, so a fast-improving search
reads as improvement rather than churn, and the final plan always applies
without delay. Each commit revalidates the area snapshot, updates connection
routing, and recenters the MapView if the player's room moved. A newer
movement snapshot cancels any pending obsolete candidate while preserving an
already committed improvement.

The package publishes `layoutState` through
`smudgy:state/kapusniak/nukefire-mapper`. It mirrors map-layout's live phase,
work counters, and current-versus-best quality without exposing mutable
planner internals. The mirror republishes at most five times per second,
retaining only the newest snapshot between publishes, and always ends on the
planner's final state. Automatic snapshots include the area id/name and the
diagnostic entry/chart context key, and terminal snapshots distinguish fixed
point, deterministic ceiling, timeout, cancellation, and fallback.
Terminal map-free worker accounting is separately published as
`layoutDiagnostics` from the same state module, including progress volume,
worker disposition, stage timings, and optional Deno heap/RSS samples.

Generated connections use orthogonal paths around intervening rooms only when
the path can leave and enter through the walls declared by both exits. They
store those paths as diagonal-tolerant routes so later layout changes cannot
invalidate the map. Their turns are drawn with rounded corners. Generated
routes are stored as solver-produced `Automatic` routing; a route drawn by
hand — `Manual` routing in the map editor — is user-owned and is never
overwritten by route recomputation.

Reflow and tidy refresh the player's location after moving its room. A chart
that adds rooms or changes exits also refreshes the map display, including
link-only changes; repeating an unchanged chart does not trigger that refresh.

When the layout engine reports that a crossing or obstruction sits between
rooms it is not allowed to move — a user-locked neighborhood, for example —
the mapper adopts the engine's proposed detour and draws that connection
around the problem instead of through it. The detour is stored as an ordinary
`Automatic` route, so it recomputes like any generated route and never touches
a hand-drawn one.

When a one-way arrival shares its destination wall with another connection,
the mapper spreads AutoPinned arrival ports across the room wall so arrowheads
remain distinguishable. Reciprocal midpoint slots and manually positioned
ports stay fixed; the arrangement is deterministic and recenters when the
crowding disappears.

## One map per area

A NukeFire area often spans several numbered zones, and older maps may hold one
section per zone. The mapper keeps one map per area name. The first time in a
session that you stand in a zone, it gathers that zone's rooms into the map for
the area the game names, combining sections and moving stray rooms, and says
so: "Combined 4 map sections into The Deathlands." Rooms, exits, labels, shapes
and links move together; nothing is lost. Each section moves as one piece, its
levels lined up with the map's through the exits that join them. A zone you
have only seen at the edge of the local map keeps a "NukeFire Zone N" map until
you visit it.

Sections seldom fit together perfectly where they meet: an exit between them
may run crooked or cross another. Once the map is quiet, the mapper straightens
those seams first, moving only the rooms around them in a few quick passes,
and each better layout appears on the map as soon as it is found. It then
polishes the whole map as usual, starting from the map as the sections were
combined, so the quick pass never costs the finished layout anything; the map
changes again only when that polish finds a layout better than the one you
see. If you move a room yourself in the meantime, the polish works from the
map as you left it.

**Area name rules** in the package settings decide which names share a map.
Each row sends an area name the game shows to the map it belongs in; tick
**Match this exact capitalization** to tell two spellings apart. The table
starts with these rows and follows the package's defaults until you change it:

| Area name the game shows | Map it belongs in |
|---|---|
| Vega Jane II, Vega Jane III, Vega Jane IV | Vega Jane |
| Dread Dungeon 253, Dread Dungeon 254, Dread Dungeon 258 | Dread Dungeon |
| Monster Island II | Monster Island |
| Shadowspire II | Shadowspire |
| SST - Federation Station | SST |
| Jurassic Park II, Jurassic World | Jurassic Park |
| DARK PLEASURES (exact capitalization) | Darker Pleasures |

Reconnect after editing the rules. A changed rule takes effect the next time
you visit the zone: its rooms move into the map the rule names, and back out
again if you remove the rule, so you never need to delete a map. Deleting a
combined map would delete every zone's rooms in it. To rename a map, rename it
in the map editor. Maps you made yourself are never combined, split, renamed or
deleted: a zone you keep in one of your own maps stays there, and its new rooms
are added to it.

## Tidying every map

Type `nfmap tidy` to do for every map at once what visiting and resting do for
one area at a time. The mapper checks each zone its maps hold and gathers it
into its area's map, as a visit would. It then polishes each map in turn with
the high-effort search of `nf reflow perfect`, records each finished polish as
that command does, and says what it does as it goes:

```
[nfmap] Zone 1600 (Seam Flats): checking.
[nukefire-mapper] Combined 2 map sections into Seam Flats.
[nfmap] Polishing Seam Flats: 537 rooms, 1400 exits, now 95/376/233 (wrong exits/blocked routes/crossings).
[nfmap] Seam Flats 1:12 | constraint search | best 41/289/71 from 95/376/233 | 14 layouts, 52310 checks | 2 on the map
[nfmap] Polished Seam Flats in 4:05: 95/376/233 -> 12/40/9.
```

Each layout is scored by three counts: exits that point the wrong way, routes
that rooms block, and crossing connections. While a map is being polished, a
line every second gives the time so far, what the search is doing, the best
layout it has found against the map as it was, the work behind it, and how
many better layouts it has already put on the map. A zone that only a
"NukeFire Zone N" map holds waits for your visit, which tells the mapper the
area's name.

Type `nfmap stop` to stop. A map being polished keeps the best layout already
on it. You can play on while the tidy runs: new rooms are mapped as usual, and
a map that changes while it is being polished keeps what it has, until its
next polish. Only the session that maps NukeFire can tidy.

## Setup

Enable the package and connect to NukeFire; mapping starts automatically. For
the complete map and radar interface, also enable
`smudgy://kapusniak/nukefire-scripts`.

Disable `smudgy://official/auto-mapper` while using this package. Running both
mappers can cause conflicting room and exit updates.

For troubleshooting, enable **Log mapping decisions (debug)** in the package
parameters. The JSONL log records every mapper mutation before drafting, after
drafting, and after acknowledgement, including results, durations, failures,
and any partially committed operation IDs.
