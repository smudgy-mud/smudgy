# Map Layout

Reusable integral-grid layout and reflow for Smudgy mappers.

The planner keeps cardinal/elevation rays, avoids route violations and
crossings, weighing a ray against eight of those (see
[Quality order](#quality-order)), then minimizes link slack and footprint.
Route violations include both rooms lying on a direct connection and occupied
cells which prevent an exit from leaving through its declared cardinal wall.
It never writes to the mapper; every operation returns a declarative patch.

## Stateless area planning

`planAreaChange` is the normal API for mappers which only reflow when topology
grows. It snapshots the Smudgy area into ordinary V8 data immediately before
planning and does not retain that model afterward. Ordinary CPU-intensive
planning runs through one lazily created FIFO Worker shared by this package
instance, so concurrent callers cannot accidentally stack compute-heavy jobs.

```ts
const result = await planAreaChange(areaId, {
  type: "add-room",
  from: currentRoomNumber,
  direction: "North",
  temporaryId: "$new",
});

await mapper.updateRooms(areaId, result.patch.moves.map((move) => [
  move.roomNumber!,
  move.to,
]));
```

An entire existing area can be reflowed without adding observations:

```ts
const result = await planAreaChange(areaId, {
  type: "reflow",
  anchor: currentRoomNumber,
});
```

Manual tools can opt into a bounded thorough search. It repeats each candidate
to a fixed point and compares the requested anchor, an unrestricted reflow,
rooms incident to remaining directional violations, their immediate neighbors,
and high-degree structural rooms. The winning layout is returned as one patch;
locked rooms remain fixed.

```ts
const result = await planAreaChange(areaId, {
  type: "reflow",
  anchor: currentRoomNumber,
}, {
  effort: "thorough",
});
```

`result.search` describes the anchors and planning passes considered. Thorough
search is opt-in so latency-sensitive automatic mapping retains the standard
single-pass behavior.

An explicit tool can instead stage the standard area plan through the same
whole-layout repair Worker used by low-level callers. Pass `constraintRepair`
in the third argument; the returned `constraintRepair` report distinguishes a
fixed point, deterministic ceiling, wall timeout, cancellation, and error.
This mode retains the same stale-snapshot validation as ordinary area planning.

Because Worker planning introduces a real asynchronous gap, `planAreaChange`
reloads the area before returning. It discards one stale result and retries
once; a second concurrent change rejects with `StaleLayoutSnapshotError`
instead of returning a patch for obsolete coordinates or topology. Room
movability callbacks always run while snapshotting in the caller realm. Trace
events are collected in the Worker and replayed in order after the accepted
result, so callbacks and other non-cloneable values never cross the boundary.
With `includeSnapshotKeys: true`, the result's `sourceSnapshotKey` and
`plannedSnapshotKey` canonically identify the validated source and proposed
final models, so later proof/metadata posts can reject a live area that no
longer matches the plan. Ordinary callers pay no allocation for these keys.

Two existing rooms can be connected while planning the reflow required by the
new topology:

```ts
const result = await planAreaChange(areaId, {
  type: "connect-rooms",
  from: currentRoomNumber,
  to: matchingRoomNumber,
  direction: "East",
});
```

## Retained models

High-frequency consumers may explicitly retain a model instead:

```ts
const workspace = createLayoutWorkspace(loadLayoutModel(areaId));
const result = await workspace.planAsync(change);
await apply(result.patch);
workspace.accept(result);
```

`planLayoutModelAsync` and `planIntegralLayoutAsync` use the same shared Worker
for host-independent models and low-level requests. Their synchronous
counterparts remain available for deterministic tests, decision-log replay,
and explicitly synchronous tools. A retained workspace also keeps its original
`plan` method; an async result cannot be accepted if another result changed the
workspace while its Worker request was in flight.

Async calls accept an optional `AbortSignal` and parent-side `timeoutMs`.
Canceling queued work does not disturb the active job. Canceling active work
terminates that Worker, rejects only that request, and resumes queued work on a
fresh Worker.

The shared Workers run one request at a time, so a long search delays every
request queued behind it. `createLayoutPlanner()` returns a planner whose
`planIntegral` takes the same arguments as `planIntegralLayoutAsync` but runs
on Workers of its own: a long background search neither waits behind the
shared queue nor holds it up. Its Workers start with its first request, and
`close()` ends them, rejecting any request still running.

```ts
const planner = createLayoutPlanner();
try {
  for (const request of requests) await planner.planIntegral(request, { onProgress });
} finally {
  planner.close();
}
```

## Live progress and deep repair

`layoutPlannerState` is a read-only, subscribable state object for the latest
operation in the current package realm. Its JSON-safe snapshot reports the
phase, elapsed time, input sizes, layouts considered, randomized restarts,
feasibility checks, hard-valid incumbents, distinct layouts and relation masks,
separator states/branches/cycle prunes, crossing work, and the
current/standard/best quality tuples. `firstIncumbentMs` records when geometric
repair first produced a complete hard-valid map.
Subscribers receive the retained value immediately and every subsequent
update.

`planIntegralLayoutAsync` also accepts `onProgress`. The callback receives the
same snapshot and includes an `improvement` plan whenever a new complete
best-so-far layout is found. Progress sinks are isolated from correctness: a
throwing observer cannot fail planning. Live streaming is demand-driven: an
integral job streams per-event telemetry only while a trace sink or
`onProgress` observer is attached, and unobserved jobs build and post no
events at all. Constraint-repair improvements always stream — they are how
anytime results flow.

Whole-layout constraint repair supports separate `maxDurationMs`,
`maxRestarts`, `maxLayouts`, `maxPolishTournaments`, `maxPolishPasses`,
`maxExtensionStates`, `maxLiveSearchNodes`, `maxMaskDiversifications`, and
`maxCrossingWork` controls.
Polish passes are one aggregate deterministic budget shared by the early preview
and all later multi-anchor tournaments; a pass ceiling can stop partway through
a tournament and reports `polishCutoff: "passes"` without claiming a fixed point.
Extension work counts
every deterministic precedence state built across canonical relation masks;
crossing work counts bridge-push macro expansions. Each work limit accepts
`Infinity`, continuing until that frontier exhausts, a perfect map is found, or
the request is cancelled. An infinite duration removes the wall-clock cutoff
while preserving `AbortSignal` cancellation. Complete-layout attempts use a
serialized dedicated-Worker lane, publish every complete strict improvement,
retain only a bounded polish frontier plus compact search bookkeeping, and cap
retained diagnostic coordinates so long searches do not grow an unbounded
trace result.

`layoutWorkerDiagnostics` publishes the latest map-free terminal accounting
from production workers: inspected/materialized work, progress volume,
termination and worker-retirement disposition, phase timings, and optional
Deno heap/RSS samples. Subscribers cannot affect planning.

Callers can attach a map-free `plannerContext` (source, area id/name, and a
stable context key) to asynchronous integral work. It is copied into every
`layoutPlannerState` snapshot for attribution and never crosses into the
Worker request.

Low-level automatic mappers can opt into a whole-layout constraint repair for
settled reflows. The Worker first runs the ordinary planner, then immediately
runs exactly one complete reflow pass anchored at the request center when present so a
strong complete incumbent can publish without letting a multi-anchor tournament
starve exact certification. It next certifies the
minimum-weight set of protected exits to relax with an exact
implicit-hitting-set search over the conflicts feasibility analysis reports.
`optimal: true` is the normal outcome, including optima that relax reciprocal
exits; seeded randomized restarts survive only as a deterministic fallback for
pathological conflict accumulation and report honestly when they run. For each
mask, deterministic complete-extension search branches over legal room and
route separations with cycle pruning, traversing best-first on incumbent
scores so finite state budgets reach better incumbents earlier. Per-state
coordinates minimize retained-relation slack on each axis after longest-path
ranks, so every candidate state is scored at its tightest geometry. Retained
rays, fixed rooms, integral coordinates, and collision freedom are hard
requirements. The extension frontier also has an independent live-node cap;
pruning a subtree reports exhaustion rather than an exact proof, so a generous
total-state budget cannot imply unbounded retained memory. Route obstructions
and crossings are soft quality terms. A collision-free soft incumbent is
freshly scored and published after a complete trace-silent compaction
transaction, or at the cheap compaction fixed point below when a deadline or a
cleanup failure cuts that transaction short. Geometry-guided equal-primary mask
swaps, bounded distinct-layout planning, multi-anchor fixed-point polish using
the remaining aggregate pass budget, and crossing repair then continue from the
best incumbent without losing it. Master masks are
compacted best-first, and any relaxation mask discovered by final unrestricted
polish is compacted and its newly retained layouts are polished in the same
bounded operation before a geometric fixed point can be reported.

A request polishes the whole map when its topology is fully resident: every
chart node it sends is also a resident, as when it sends none, and it lets
existing rooms move. A mapper's quiet polish that sends its local chart is
such a request, as is a reflow. Every layout a whole-map request publishes is
finished as described below. A reflow, which sends no chart, also packs axis
groups: its standard pass recursively packs mutually blocking room groups
along both planar axes from every directional orientation (axis-group
compaction), starting another full pass after every pass that gains until one
gains nothing, and the deep crossing repair heals its provisional winners the
same way. That packing can take minutes on a large map, so a polish that sends
its chart skips it everywhere, its constraint repair included, and keeps to
the cheap fixed point; `packsAxisGroups` says which a request gets. A request
with a chart node that is not a resident places new rooms and keeps its
low-latency path: its standard pass gets neither the packing nor the
finishing below.

Every other layout published as an improvement ends at the cheap compaction
fixed point, `compactIntegralLayoutPlan` with `axisGroupCompaction: false`:
globally empty rows and columns removed and unavoidable slack redistributed
evenly along maximal straight cardinal series, prioritizing reciprocal
connections, repeated until neither gains. That covers the quick crossing
repair and the plan of a whole-map request's standard pass, and every
request's constraint repair: its incumbents, early preview and polished
layouts, each under the admission of the stage that publishes it. A full
compaction ends with the same fixed point. The planner, the preview and polish
go on from the finished layout. The deep crossing repair goes on from its raw
transaction and publishes and returns each copy at the fixed point of a
trace-silent compaction instead, packing axis groups where the request allows,
so compacting a layout it streams the same way changes nothing; only a
compaction that cancellation cuts short leaves the cheap fixed point. A plan
whose quick crossing repair published an improvement receives one more
compaction for the same reason. A repaired plan ends at the
fixed point too, and finishing a finished layout leaves it as it is. No plan
ranks below a layout published before it, so a streamed layout never takes a
result's place.

Axis-group compaction can take far longer than the rest of a standard pass:
seconds to minutes on a few hundred rooms. Before it starts, a reflow whose
progress is observed publishes its layout as it stands, finished, as a
`preview` event that `planIntegralLayoutAsync` streams as an improvement, so a
map shows the reflow begin within moments and the compacted plan when it is
ready.

The spacing pass moves attached axis groups atomically and never increases any
public quality field, leaves the original envelope, moves a fixed/player room,
or violates a caller-supplied position constraint.

```ts
const result = await planIntegralLayoutAsync(request, {
  constraintRepair: {
    when: "settled-regression",
    maxDurationMs: 10_000,
  },
});
```

`when: "violation-regression"` also considers newly observed topology and is
appropriate for a mapper's final settled snapshot. `when: "defects"` repairs
any layout with a directional, routing or crossing defect, which suits a
mapper polishing whole maps over time. `when: "always"` is useful for explicit
whole-area reflows and still runs geometric polish when its input already has
no defect; every other mode returns such a layout as it is.
`result.constraintRepair` reports constraint optimality separately from
`geometricFixedPoint`, along with constraint and polish cutoff reasons,
first-incumbent latency, geometric work counts, and stage timings. Constraint
optimality describes the weighted relation master; a finite geometric cutoff
remains an anytime result, not a proof that no better complete extension
exists. The duration is a cooperative search ceiling: one complete polish
tournament may finish past a finite deadline. An infinite duration continues
tournaments until geometric fixed point, the optional deterministic tournament
ceiling, or caller cancellation. An exit whose vector the constraint search
cannot express, such as a diagonal or a constraint vector along several axes,
is left out of that search and still counts in every quality measure.

Every repair reports, whether or not it searched. `report.outcome` is
`"searched"` when the repair ran its constraint search and carried the result
through compaction, polish and crossing repair. Any other outcome returns the
standard plan as it was and names why:

| Outcome | Why the repair did not search |
| --- | --- |
| `locked` | The request forbids moving existing rooms. |
| `no-regression` | The plan does not regress the way `when` asks for. |
| `clean` | The plan has no directional, routing or crossing defect. |
| `no-constraints` | No exit is one the search can express. |
| `no-budget` | `maxDurationMs` is not positive. |
| `search-failed:analysis` | The search's first check found the exits the standard plan keeps contradictory, which only an inconsistent plan can cause, such as one that puts two rooms on one cell. |
| `search-failed:time` | The deadline passed before the search finished its first check. |
| `search-failed:work` | The search's check budget ran out before its first check. |
| `no-layout` | The search ran, but no compaction produced a layout that keeps the exits it retained. |

A report that did not search selects nothing and proves nothing: `selected`,
`constraintOptimal` and `geometricFixedPoint` are false, `polishCutoff` is
`"none"`, and its counts cover only the work done before the repair stopped.
Its `cutoff` is `"time"` when a deadline stopped it and names a work ceiling
when one did. Only a change to the map or to the request changes an outcome,
except for those a deadline cut, which depend on how fast the machine ran.
A requested repair whose plan carries no report never delivered one: a
parent-side deadline, a cancellation or a failed Worker resolves the retained
plan.

A flat exit also says that its rooms share a level. Where the exits
contradict that (an Up/Down exit between rooms that flat exits join, or a
cycle of Up/Down exits between such groups of rooms), or where the standard
plan already puts rooms that flat exits join on different levels, each of
those flat exits' shared level is a level relation the search may give up
like a ray. Giving it up costs what drawing those exits wrong costs, and of
equally costly choices the search gives up fewer level relations. A flat exit
and an Up/Down exit between the same two rooms are therefore an ordinary
conflict, resolved the cheapest way across the whole map, and
`report.relaxedLevelRelations` counts the level relations the result gives up.
Everywhere else a flat exit's shared level is never given up: that could only
separate rooms by level, and collisions are always resolved in the plane.

Constraint repair runs on a second, dedicated persistent Worker after the
ordinary result is available. It therefore cannot hold up the ordinary shared
FIFO queue, while a second FIFO ensures that concurrent callers never stack
CPU-heavy repair Workers. The repair Worker stays warm across successful
repairs, so sequential repairs pay isolate spawn and module compile once. It
is reclaimed — terminated, with the next repair starting a fresh Worker —
whenever active work is abandoned or the transport misbehaves: the hard
deadline's backstop, an abort or caller deadline during active repair, a
startup or postMessage failure, or malformed traffic. Every degraded outcome
still returns the retained ordinary result (or the best validated streamed
improvement); a hard deadline still kills a hung repair.

When a defect is permanent — every room a fix would need to move is immovable
— the planner cannot improve the geometry, so it proposes a `routeAmendments`
entry instead: per-link elbow waypoints that draw the link around the
obstruction or crossing, leaving and entering through the declared walls.
Amendments are advisory. They never move rooms, they never change the quality
tuple (which keeps scoring the straight segments honestly), and consumers
that ignore them behave exactly as before.

## Quality order

`compareLayoutQuality` is the one order by which the planner, placement, the
constraint repair and the crossing repair select, accept and publish layouts.
The planner's greedy searches may pass through layouts the order ranks lower
on their way to deeper repairs, and the planner returns the one it ranks
highest among those it keeps. The order ranks layouts first by a score, lower
first:

```text
8 × (cardinalRayViolations − levelViolations) + 17 × levelViolations
  + routingViolations + linkCrossings
```

The 8 is `DIRECTIONAL_VIOLATION_WEIGHT`. A layout that removes a directed
protected-ray violation is better only if that costs at most 8 more routing
violations and crossings combined, and a layout with one more such violation
is better when it has more than 8 fewer. Routing violations and crossings
weigh the same.

`levelViolations` counts the directional violations whose rooms sit on levels
their exit rules out: an exit on one level, such as a compass exit, joining
rooms on different levels; an Up exit whose room is not higher; a Down exit
whose room is not lower. Level 2 is above level 1, and an exit's own vector
decides, so a projected Up/Down exit drawn on one level is not mis-levelled.
Each weighs 17, `LEVEL_VIOLATION_WEIGHT`, in place of 8: one outweighs 16
routing violations and crossings combined, and two exits drawn wrong on their
own level. A link between two levels is never obstructed and never crosses
another, so without this weight moving rooms to another level would shed
crossings for the price of an ordinary directional violation. An Up exit that
climbs from beside its room is drawn wrong on its level only, and exits
without a direction to keep, such as a diagonal without a vector or In and
Out, are never mis-levelled.

Layouts with equal scores are ranked field by field. Mis-levelled exits come
first, then directional violations, then `reciprocalRayViolations`, which
prefers preserving exact two-way connections. After every defect comes the
map's size: `levelSlack`, the extra levels that exits drawn right climb or
descend (an Up exit to the room three levels up spans two more than it
needs), then footprint. `routingViolations` combines direct-link room
obstructions with blocked exit ports; `exitPortViolations` and
`reciprocalExitPortViolations` break ties in favor of routes which leave both
rooms through their declared walls before obstructions, crossings, footprint
and slack. Every field takes part, so the order is total: sorting any set of
layouts agrees with every pairwise comparison.

The constraint repair's exact search minimizes the exits it draws wrong,
weighted as its report describes; the quality order ranks the layouts that
search leads to.

## Joining map sections

`placeRigidBlocks` decides where whole sections of rooms land when they join
an existing map, for mappers that merge maps by moving each section with one
rigid offset. It returns one integer offset per section, and no section room
lands on a cell that a resident or an earlier section holds. Offsets come from
the search that anchors a new chart component: the offset each seam link
implies, nearby shifts of the eight whose seam links score best, and islands
past the map's edge. `compareLayoutQuality` chooses among them, so the offset
whose seam links score best, each mis-levelled one at its own weight, wins
unless another saves more blocked routes, exit ports and crossings than the
directions it gives up are worth, and all of those count before footprint and
slack. Offsets are compared best seam score first and nearest the best seam
first, and one whose seam scores worse only while it could still win: hundreds of them on
a map of a few dozen rooms, only a few on a map of thousands, which keeps
sections whose seam links all disagree fast to place. The section most
strongly linked to what is already placed goes first, so a chain of sections
aligns transitively; a section without a directional link becomes an island
east of the map. The offsets do not depend on the order of the request's
lists, and a section that no offset lands on free cells with safe-integer
coordinates is an error.

```ts
const offsets = placeRigidBlocks({
  residents: destinationRooms, // { id, position, movable }
  blocks: [{ id: sourceAreaId, rooms: sourceRooms }], // rooms: { id, relative }
  edges, // exits among all of those rooms
});
```

## Seam regions

Joining sections leaves its defects where they meet. `seamRegion` picks the
rooms a quick polish of those seams may move, so a mapper can plan the joined
map with every other room pinned and show that layout long before a polish of
the whole map finishes. The seeds are the rooms at either end of the links
that joined two sections, and the region holds:

- the seeds;
- the rooms on or next to a link between two seeds: within `SEAM_LINK_REACH`
  (1.32 cells, Chebyshev) of its segment, on its level;
- the rooms on or next to any link between two rooms of that set, so the
  polish can move the rooms that block those links;
- every room that shares its cell with another, since the planner cannot lay
  out two pinned rooms on one cell.

```ts
const region = seamRegion(positions, edges, seeds); // Map<id, GridPosition>, LayoutEdge[], ids
const residents = rooms.map((room) => ({
  ...room,
  movable: room.movable && region.has(room.id),
}));
```

The result is a set of room ids and depends only on its inputs as sets. Seeds
without a position are ignored, and a link between two levels has no segment,
so no room lies next to it. Plan a pinned request without `constraintRepair`:
the constraint search does not yet take pinned rooms as anchors, so a pinned
repair spends its budget without finding a layout.

## Elevation

Existing U/D links on different levels remain vertical. Same-level U/D links
are projected diagonally; their semantic directions remain Up/Down while each
physical connection receives a NE/NW or SE/SW constraint selected from local
path continuity. A new link may request `auto`, `levels`, or `projected`.
