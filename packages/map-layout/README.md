# Map Layout

Reusable integral-grid layout and reflow for Smudgy mappers.

The planner protects cardinal/elevation rays first, then avoids rooms and
crossings, then minimizes link slack and footprint. It never writes to the
mapper; every operation returns a declarative patch.

## Stateless area planning

`planAreaChange` is the normal API for mappers which only reflow when topology
grows. It snapshots the Smudgy area into ordinary V8 data immediately before
planning and does not retain that model afterward. CPU-intensive planning runs
in one lazily created background Worker shared by this package instance.

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

Because Worker planning introduces a real asynchronous gap, `planAreaChange`
reloads the area before returning. It discards one stale result and retries
once; a second concurrent change rejects with `StaleLayoutSnapshotError`
instead of returning a patch for obsolete coordinates or topology. Room
movability callbacks always run while snapshotting in the caller realm. Trace
events are collected in the Worker and replayed in order after the accepted
result, so callbacks and other non-cloneable values never cross the boundary.

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

## Elevation

Existing U/D links on different levels remain vertical. Same-level U/D links
are projected diagonally; their semantic directions remain Up/Down while each
physical connection receives a NE/NW or SE/SW constraint selected from local
path continuity. A new link may request `auto`, `levels`, or `projected`.
