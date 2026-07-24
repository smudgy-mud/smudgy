# smudgy_bench

Criterion benchmarks over smudgy's hot paths. The support crate is a workspace
default-member; criterion targets still run explicitly:

```powershell
cargo bench -p smudgy_bench                          # everything (slow)
cargo bench -p smudgy_bench --bench interop_ops      # one suite
cargo bench -p smudgy_bench --bench trigger_churn -- churn_packet   # one group
cargo bench -p smudgy_bench -- --quick               # fast indicative pass
```

On Linux, the small deterministic CPU suite uses
[Gungraun](https://gungraun.github.io/gungraun/) (the maintained successor to
iai-callgrind) and Valgrind:

```powershell
cargo install --locked --version 0.19.4 gungraun-runner
cargo bench -p smudgy_bench --features callgrind --bench ingest_callgrind
```

Env vars honored where a bench says so: `SMUDGY_BENCH_LINES=n` (truncate-only
corpus/pass cap), `SMUDGY_BENCH_SKIP_SANITY=1` (skip the warmup assertions
that prove the measured machinery does what the bench claims).

## Continuous benchmark history

The public repository's trusted `Benchmarks` workflow runs this full suite
after pushes to `main`, weekly, for releases, and on manual dispatch. Its
canonical platform is one ephemeral **AWS `m8a.2xlarge` On-Demand** runner in
a fixed `us-west-2` AZ, using a pinned Ubuntu 24.04 AMI and Rust 1.97.1. The
workflow verifies the EC2 instance type, AMI ID, physical-core count, and
toolchain before measuring.

Fresh Criterion estimates are converted from `target/criterion` into the
custom-smaller-is-better input for
[`benchmark-action/github-action-benchmark`](https://github.com/benchmark-action/github-action-benchmark).
The dashboards record the appropriate Criterion slope/mean point estimate in a
fixed `ns/iter` unit and retain its 95% confidence interval:

- [`benchmarks`](https://smudgy-mud.github.io/smudgy/benchmarks/) is the
  `main` trend (pushes, weekly samples, and manual full runs).
- [`benchmarks/releases`](https://smudgy-mud.github.io/smudgy/benchmarks/releases/)
  is deliberately separate, so each comparison is against the previous
  release rather than the latest weekly sample.

A 25% slowdown in longitudinal history is a screening alert, not a merge gate.
PR comparisons use the replicated, targeted method below instead of applying
that history threshold to a single A/B pair.

The runner group is restricted to the exact benchmark workflow on `main`, and
the workflow deliberately has no pull-request trigger. A manual `smoke` mode
verifies provisioning and the pinned platform without running or publishing the
suite.

Maintainers can request a same-machine PR comparison:

```powershell
gh workflow run benchmark.yml --repo smudgy-mud/smudgy --ref main `
  --field mode=pr --field pr_number=123
```

The resolver accepts only open PRs targeting `main`. Same-repository PRs require
a write-authorized manual dispatcher; fork PRs additionally require the
dispatcher to match the `BENCHMARK_FORK_ACTOR` repository variable. Manually
dispatching a fork comparison authorizes that reviewed fork's build scripts to
run on the ephemeral self-hosted instance. The candidate is checked out through
the base repository's pull ref and must match the immutable SHA resolved before
the M8a job was requested.

The ephemeral runner selects PR suites through the trusted
`.github/benchmark-scope.json` map. A changed path can have direct coverage,
partial coverage with an explicit limitation, no performance relevance, or a
coverage gap. A gap is reported as a gap; it is never presented as evidence
that performance is unchanged.

To keep eight full observations inside the runner budget, a PR run selects at
most five product Criterion targets plus `runner_control`, in the priority order
declared by the map. If a broad change maps to more, the report names every
omitted target and marks coverage partial. The canonical main/weekly/release
run remains uncapped.

For selected Criterion targets, both revisions are compiled before measurement
and pinned to CPUs 2-7. The runner executes two balanced blocks, `ABBA` then
`BAAB`, where A is current `main` and B is the PR. Each block independently
uses the geometric mean of its two observations per revision. A wall-clock
change is called **confirmed** only when both blocks cross +/-5% in the same
direction and each arm's two observations within every block stay within 5%
of one another. Any product cell with internally unstable replicates is
**inconclusive**, regardless of its aggregate delta. A threshold crossing in
only one block, or opposite-direction crossings between blocks, is likewise
inconclusive rather than stable.

`runner_control` travels with every targeted Criterion comparison. Its
revision-independent integer and memory cells must remain within +/-3% in both
blocks. The workflow also requires the performance CPU governor when the
kernel exposes one, pins the benchmark cpuset, and records load, frequency,
temperature, steal-time, process, CPU, AMI, kernel, and toolchain provenance.
Failed controls, excess steal, or concurrent runnable work invalidate the
whole wall-clock result. The one-minute load average is retained as diagnostic
context because it decays too slowly after compilation to be a reliable gate.

CPU-only paths with a mapped Gungraun target also receive a same-run
Callgrind comparison. Instruction counts are deterministic enough to remain
useful on a virtualized runner, but they do not stand in for wall-clock
latency, readiness scheduling, TLS, allocator contention, or network behavior.
The PR report keeps those two kinds of evidence separate.

PR measurements and their environment telemetry are retained as workflow
artifacts but never written to Pages history. The bot updates its existing PR
comment in place. The comment is explicitly a review aid rather than a merge
gate; complete JSON, logs, and Gungraun profiles stay in the artifact.

Publishing a GitHub release dispatches release mode on the trusted `main`
workflow. A maintainer can backfill an existing version tag similarly:

```powershell
gh workflow run benchmark.yml --repo smudgy-mud/smudgy --ref main `
  --field mode=release --field release_ref=v0.4.2
```

## Baselines — the before/after discipline

Every optimization claim against these benches should be a criterion
comparison, not eyeballed numbers. Before starting a change:

```powershell
cargo bench -p smudgy_bench -- --save-baseline pre-<change>
```

then during/after the work:

```powershell
cargo bench -p smudgy_bench -- --baseline pre-<change>
```

Run baselines on a quiet machine; the live-session benches are µs-scale and
Windows timer jitter is real. For allocation-sensitive changes, the
`identity_tax` bench also prints **exact allocs/call** via the counting
global allocator (`src/alloc.rs`) — a deterministic figure that survives a
noisy machine.

Two hard-won caveats on trusting stored baselines (both bit during the
pre-GMCP hardening pass):

- **Stored baselines age with machine state.** Cells dominated by
  turn-round-trip latency (`set_per_turn64` is ~65 sequential cross-thread
  wakes) swing ±45% across machine states and ±5% across processes, so a
  comparison against a weeks-old tag can manufacture a "regression" whose
  boundary falls at whatever change landed when the machine state shifted.
  A criterion delta against a stored tag is a *screening* signal; before
  acting on one, confirm it with a same-day A/B — a git worktree at the
  baseline commit, both arms full-run, back-to-back.
- **`--quick` misses allocator/cache effects.** Deferred-deallocation
  costs only manifest under sustained load: a change can read as "no
  change" at `--quick` and as a real per-turn regression in a full run.
  Gate perf-sensitive landings on full runs of the affected cells.

The JS-driven suites are pinned to the **public scripting surface** (handles,
`line.*`, `echo`), never op names or signatures, precisely so the identical
bench runs unchanged on both sides of an op-layer rework.

## Suite map

Matching & dispatch:
- `trigger_matching` — matcher *strategies* in isolation (the `PatternSet`
  tiering rationale).
- `trigger_engine` — the real engine: steady-state corpus scans
  (`engine_scan`) and the dirty-flag `PatternSet` rebuild stall
  (`engine_build`).
- `script_dispatch` — live harness sessions: per-line fire tax + capture
  marshalling with empty JS bodies.
- `trigger_verbs` — same shape, bodies exercising the per-fire verbs
  (`gag`, `echo(line.text)`, `highlight`). Deltas vs its `empty` control.
- `trigger_churn` — the create/delete vs enable/disable trade:
  `churn_residue` (disabled-vs-absent scan cost per matcher tier) and
  `churn_packet` (the mid-packet rebuild stall a mode-switch handler causes,
  incl. a 4-sandbox variant).

Interop (`docs/interop.md`; these four are the measurement plan for the
op-layer identity-interning and read-path work):
- `identity_tax` — pure-Rust per-call identity re-derivation on the interop
  ops (creator/producer/path parse + fold), with exact alloc counts. The
  predicted ceiling for interning.
- `interop_ops` — the same costs end-to-end through the public JS surface,
  user-module vs sandboxed-package producers. The realized number interning
  must move.
- `interop_read` — `.value`/`.previousValue` leaf reads, proxy-walk depth,
  key enumeration, and explicit whole-tree materialization over
  published-tree sizes: the read path's O(answer) guarantee, plus the honest
  cost of explicit capture. (The retired `.current` cells live in the
  `pre-p4` baseline.)
- `interop_delivery` — emit fanout × payload size, both watch cadences, and
  a cross-isolate scheme-consumer cell, through live V8 delivery.
- `catalogue` — the runtime catalogue's host-side costs in isolation: the
  per-sample recording at the emit/post choke points (with vs without a
  subscribed store tab — deferred parsing's before/after) and the snapshot
  build against store size (shared `Node` roots keep it flat) and against
  catalogue entry count up to the per-producer budget (the snapshot's
  O(entries) half: per-entry shape render + sample-ring collection).

Store (below the op layer):
- `store_fanout` — `SessionStore` write→flush→fanout with no engine; the
  floor `interop_ops/…/set128` is compared against (its `J128/W0` cell).

Ingest & display:
- `ingest` — socket-bytes → `StyledLine` glue.
- `ingest_callgrind` — deterministic instruction counts for the synchronous
  telnet/VT/runtime-channel ingest CPU path; Linux + Valgrind only.
- `terminal_buffer` — display-side buffer work.
- `runner_control` — short integer/memory wall-clock controls used to reject a
  noisy PR comparison; not a product-performance benchmark.

Mapper:
- `mapper_scale`, `map_spatial` — map-domain scaling.
- `gmcp_automap` — the auto-mapper movement step (ephemeral-tier room
  creation + `external_id` follow) at 10k/100k loaded rooms, 1M grid behind
  `SMUDGY_BENCH_AUTOMAP_BIG=1`; the area-scoped rebuild's regression gate.

## Shared infrastructure (`src/`)

- `session.rs` — live spawned-session harness (hermetic smudgy home, module
  + sandboxed-package install, barrier-marker drains). Used by the
  interop/verbs/churn suites; `script_dispatch` keeps its own copy so its
  long-lived baselines stay bit-for-bit comparable.
- `alloc.rs` — the counting global allocator (opt-in per bench target).
- `lib.rs` — corpora loaders + `REGEX_TRIGGERS`.

## Public synthetic fixtures

All committed benchmark corpora are deterministic synthetic data. They contain
no player logs, private game output, or names learned from private inputs.
`generate_fixtures.py` builds both item-name corpora and the long session log
from the neutral vocabulary embedded in that script. The long log deliberately
meets or exceeds the line and byte size of the corpus it replaces so ingest,
terminal, and trigger benchmarks retain comparable workload pressure.

Regenerate or verify the committed files with:

```powershell
python bench/generate_fixtures.py
python bench/generate_fixtures.py --check
```

Adding a case for a newly-hot path is ~20 lines against `session.rs`: write
a module that registers a `ZZ…`-line trigger doing the work K times and
echoing a done marker, feed + drain in `iter_custom`, and assert the
observable effect once during warmup.

## Measured record: area-scoped identification-index rebuild (2026-07-18)

The `AtlasCache` identification tables (four lookup tables + both
`external_id` indexes) moved to persistent maps (`imbl`) maintained
incrementally per touched area; only exclusion-axis changes rebuild from
scratch. Clean before/after on the dev profile:

- `gmcp_automap automap_step/create_room`: 6.4 ms @10k / 120 ms @100k
  (super-linear) → **2.4 ms / 2.0 ms, and 7.6 ms @1M** — flat in total
  loaded rooms; the residual is the touched area's connection/R-tree
  rebuild + backend write.
- `follow/find_room_by_external_id`: 82 ns → ~112–128 ns, scale-flat
  through 1M (persistent-map probe constant).
- `mapper_scale`: batch upserts −47…−54%; identification cell unchanged
  (~16 µs); routing within noise (probe: `get_room` 23→38 ns,
  `path_across/50k` 18→18.5 ms); accepted cost: single-room upsert into a
  single-area-50k atlas +19% (815 µs) from the per-area diff scan.

## Measured record: pre-GMCP store hardening (2026-07)

The six-phase store/op/catalogue hardening pass (complete 2026-07-11) these
interop suites were built to measure. The semantics of the changes are
documented in `docs/interop.md`; the full plan is in git history
(`docs/interop-pre-gmcp-plan.md`, deleted 2026-07-17). This section is the
permanent record of the measured numbers.

Motivating measurements (all pre-pass):

- Reads were O(published tree), not O(answer): a `.current.hp` read on a
  ~1 MiB tree cost **6.5 ms**; a `.value` proxy leaf read through three fat
  intermediates cost **750 µs** (`interop_read/value_leaf/depth4`);
  `Object.keys` at 32 KiB cost **266 µs** (`interop_read/keys_32k`).
- Every write re-derived constant identity: a package-producer `set` paid
  **11 allocs / 418 B ≈ 0.7 µs of a ~2.5 µs op** (user-module ~0.2 of
  ~1.9 µs) — 10–28 % of each write (`identity_tax`, exact alloc counts).
- Every dirty widget binding took an O(subtree) `Value` clone per flush,
  and a subscribed store tab rebuilt the full catalogue snapshot on the
  session thread per dirty drain (`store_fanout` floor).
- The catalogue had no entry-count bound (a memory hazard, not a bench cell).

Per-phase before/after (criterion comparisons against per-phase baselines):

- **Phase 1 — tagged read path** (tagged get + keys ops; only leaves and
  arrays cross the V8 boundary, objects return deeper proxies with zero
  data crossed): `interop_read/value_leaf/depth4` **750 µs → 2.4 µs**;
  `keys_32k` **266 µs → single-digit µs**.
- **Phase 2 — op identity interning** (creator/producer/root-path/event
  identity interned once at handle construction, home-gate verdict cached):
  `identity_tax` package `set` **11 → ≤2 allocs/call**;
  `interop_ops/{user,package}/set128` moved by the interned share;
  `interop_delivery/emit_fanout/*` dropped the per-subscriber payload clone
  (one shared `Arc<str>`). `store_fanout` unmoved, as required (it sits
  below the op layer).
- **Phase 3 — persistent `Node` tree** (structural sharing, transient-
  then-freeze writes via `Arc::make_mut`, binding cells become
  `ArcSwap<Node>` bumps): `store_fanout` `flush_*/J128/W0` **54 µs → 5 µs**
  (head freeze replaces journal replay); coalesced fanout **−26…−73 %** at
  W>0; per-write **−2…−26 %** (a first-run +2–3 % on the per-write W64
  cells was machine noise — the same-day rerun read −2.4/−7.8/−2.5 % for
  J1/J16/J128). Unpredicted bonus: `interop_ops/user/set128`
  **190 µs → 87 µs** and `package/set128` **228 µs → 111 µs** — the
  per-turn projection accelerator's per-write deep `Value` clone became an
  O(1) shallow head apply, and per-number `to_string` usage allocations
  dropped off the write path.
- **Phase 4 — `value`/`previousValue`, `.current` retired**: the 6.5 ms
  innocent-looking property read is gone by construction. `interop_read`'s
  `current_leaf` cells were re-pointed at `value` leaf reads at
  1 KiB/1 MiB (the flatness proof) plus `materialize_32k` — explicit
  whole-tree capture prices *slower per element* than `.current`'s single
  O(tree) op did, by design (the cost of capture is now visible at an
  explicit spelling). The retired cells live in the `pre-p4` baseline.
- **Phase 5 — catalogue budgets, deferred sample parsing, Arc roots**: the
  `catalogue` target is the record — per-sample recording cost with vs
  without a subscribed store tab (deferred parsing's before/after), and
  snapshot build flat against store size (shared `Node` roots) and linear
  only in entry count up to the per-producer budget.
- **Deferral datum** (why host-pinned lazy payload views stayed deferred):
  measured whole-delivery cost is **~2 µs at 64 B and ~9 µs at 16 KiB**
  (`interop_delivery`), so eager by-value beats per-hop op crossings at
  realistic payload sizes; lazy views only earn their keep for large,
  sparsely-read snapshot deliveries.
