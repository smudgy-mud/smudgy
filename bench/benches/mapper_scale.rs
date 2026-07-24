//! Mapper write cliffs and pathfinding at scale, driven through the real
//! `smudgy_cloud` public API over the synthetic atlases from
//! `smudgy_bench::atlas`.
//!
//! Reads are O(1) `ArcSwap` snapshots, but **every room write RCUs the whole
//! `AtlasCache`**: `Mapper::upsert_room` (cloud/src/mapper.rs ~891) replaces
//! the touched `AreaCache` (`rebuild_room_state`, `area_cache.rs` ~117:
//! `build_room_connections` O(rooms x exits) plus two `RTree` bulk loads) and
//! then `insert_area` -> `AtlasCache::new_with_areas` (`atlas_cache.rs` ~50)
//! rebuilds all four identification indices over every room of **every** area
//! plus `sort_owned_first` (~494). These groups quantify where that cliff
//! starts to hurt and how much the batch API amortizes it:
//!
//! - `atlas_build/cold/<size>`: `Mapper::set_disabled_areas(empty)`, whose
//!   body is literally `AtlasCache::new_with_areas(areas.clone(), disabled)`
//!   (`with_disabled_areas`, `atlas_cache.rs` ~220) — the identification-index
//!   rebuild over pre-built `AreaCache`s that *every* single-room write also
//!   pays via `insert_area`, and the startup index cost once areas are
//!   decoded. Excluded: per-area geometry builds (connections/RTrees) and the
//!   backend JSON round trip. The timed body includes dropping the previous
//!   cache generation, exactly as the RCU does in the app.
//! - `upsert_room/single/<size>`: the real `Mapper::upsert_room` on one
//!   existing room — per-area `rebuild_room_state` (~500-room zone, constant
//!   across sizes) + the atlas-level index rebuild (scales with total rooms).
//!   `upsert_room/single` minus `atlas_build/cold` isolates the area-local
//!   share.
//! - `upsert_rooms/batch_10k/{1,16,256}`: the batch amortizer
//!   (`Mapper::upsert_rooms`, one rebuild for N rooms) at the 10k atlas;
//!   throughput is rooms written/sec, so the amortization shows as rising
//!   Kelem/s with batch size.
//! - `pathfinding/*/{10k,50k}`: lazy Dijkstra over exit edges with a Vec
//!   alloc per expanded node (`atlas_cache.rs` `get_path_between_rooms` ~319,
//!   `find_nearest_room_with_predicate` ~370). Expected magnitudes:
//!   `nearest_tag_hit` explores only a small radius (~1% tag density) and is
//!   microseconds; `path_across` (corner to corner) explores nearly the whole
//!   grid; `nearest_tag_miss` (a tag that exists nowhere) floods the entire
//!   reachable graph — the worst case, at least as slow as `path_across`.
//!   These run on single-area atlases so the reachable component really is
//!   the full room count (the generator links rooms in-area only).
//! - `identification/by_title_and_description/10k`: the room-identification
//!   lookup including the `format!("{title}\r\n{description}")` key String it
//!   allocates per call (`atlas_cache.rs` ~258), over a query mix of
//!   shared-bucket (title+description held by many rooms) and unique-bucket
//!   pairs drawn from the atlas itself.
//!
//! Write benches keep the mapper's fire-and-forget sync queue on a parked
//! current-thread tokio runtime that is never driven after setup, so queued
//! `AreaSyncOperation`s accumulate unexecuted: the timed body is the cache
//! RCU plus one unbounded-channel push — the same cost the app's calling
//! thread pays before background sync picks the op up.
//!
//! Env vars: `SMUDGY_BENCH_LINES=n` scales BIG down (no log corpus here, so
//! the knob maps to rooms as `BIG = clamp(25 * n, 1_000, 50_000)`; SMALL/MED
//! stay fixed and BIG entries are skipped if the scaled size collides with
//! them). `SMUDGY_BENCH_SKIP_SANITY=1` skips the state-restoring sanity pass.

use std::{
    collections::{HashMap, HashSet},
    hint::black_box,
    sync::Arc,
    time::Instant,
};

use criterion::{
    BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use smudgy_bench::atlas::{synthetic_area, synthetic_atlas_cache};
use smudgy_cloud::{
    AreaId, AreaWithDetails, LocalBackend, Mapper, MapperBackend, RoomNumber, RoomUpdates,
    mapper::{AtlasCache, RoomKey},
};

const SMALL: usize = 1_000;
const MED: usize = 10_000;
const BIG_DEFAULT: usize = 50_000;
/// `SMUDGY_BENCH_LINES` -> rooms conversion for the BIG size (see module docs).
const ROOMS_PER_LINE: usize = 25;
/// Zones of ~500 rooms — a realistic large MUD area — so the per-area rebuild
/// cost stays constant while the atlas-level index rebuild scales.
const ROOMS_PER_AREA: usize = 500;
/// A tag absent from the generator's pool (INN/PEACE/SHOP/GUILD), so the
/// nearest-tag search must exhaust the reachable graph and return `None`.
const MISS_TAG: &str = "ferry";

/// BIG room count, honoring `SMUDGY_BENCH_LINES` (module docs give the mapping).
fn big_rooms() -> usize {
    std::env::var("SMUDGY_BENCH_LINES").map_or(BIG_DEFAULT, |v| {
        let lines: usize = v.parse().expect("SMUDGY_BENCH_LINES must be a number");
        lines
            .saturating_mul(ROOMS_PER_LINE)
            .clamp(SMALL, BIG_DEFAULT)
    })
}

/// `1_000` -> `"1k"`; non-multiples print raw so scaled sizes stay truthful.
fn size_label(rooms: usize) -> String {
    if rooms.is_multiple_of(1_000) {
        format!("{}k", rooms / 1_000)
    } else {
        rooms.to_string()
    }
}

fn number(index_1based: usize) -> RoomNumber {
    RoomNumber(i32::try_from(index_1based).expect("room number fits in i32"))
}

/// Grid coordinates are small exact integers; mirrors `coord` in
/// `bench/src/atlas.rs` (the sanity pass verifies the mirrored layout).
#[allow(clippy::cast_precision_loss)]
fn coord(v: usize) -> f32 {
    v as f32
}

/// Smallest grid side whose square holds `rooms` rooms; mirrors `grid_width`
/// in `bench/src/atlas.rs` (verified against real room coordinates by the
/// sanity pass, so a generator layout change fails loudly here).
fn grid_width(rooms: usize) -> usize {
    let mut width = 1;
    while width * width < rooms {
        width += 1;
    }
    width
}

/// A representative room edit (retitle + nudge): the payload
/// `Mapper::upsert_room` sends on every map-editor drag/rename. Titles cycle
/// through eight values so the by-title index reaches a steady state instead
/// of growing a fresh key per iteration.
fn room_edit(seq: u64) -> RoomUpdates {
    RoomUpdates {
        title: Some(format!("Bench Edit {}", seq % 8)),
        x: Some(0.25),
        y: Some(0.25),
        ..RoomUpdates::default()
    }
}

/// A live `Mapper` over a `LocalBackend`, plus everything that must stay
/// alive for it: the (never again driven) tokio runtime its sync worker was
/// spawned on and the temp dirs backing the store. Construction mirrors
/// `smudgy_bench::atlas::synthetic_atlas_cache` but keeps the mapper so the
/// write benches can drive the real `upsert_room`/`upsert_rooms` methods.
struct ScaledMapper {
    _runtime: tokio::runtime::Runtime,
    _store_dir: tempfile::TempDir,
    _cache_dir: tempfile::TempDir,
    mapper: Mapper,
    /// Synthetic-area ids in generation order (index 0 = the write target).
    area_ids: Vec<AreaId>,
    /// Room count of area 0 (its numbers are `1..=area0_rooms`).
    area0_rooms: usize,
    total_rooms: usize,
}

fn build_scaled_mapper(total_rooms: usize) -> ScaledMapper {
    let areas = (total_rooms / ROOMS_PER_AREA).max(1);
    let base = total_rooms / areas;
    let remainder = total_rooms % areas;
    let details: Vec<AreaWithDetails> = (0..areas)
        .map(|i| {
            let index = u32::try_from(i).expect("area count fits in u32");
            synthetic_area(index, base + usize::from(i < remainder))
        })
        .collect();
    let area_ids: Vec<AreaId> = details.iter().map(|d| d.area.id).collect();
    let area0_rooms = details[0].rooms.len();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench tokio runtime");
    let store_dir = tempfile::tempdir().expect("temp local map store");
    let cache_dir = tempfile::tempdir().expect("temp mapper cache dir");
    let mapper = runtime.block_on(async {
        let backend = LocalBackend::new(store_dir.path());
        for area in details {
            backend
                .import_local_area(area)
                .await
                .expect("import synthetic area");
        }
        let mapper = Mapper::new(Arc::new(backend), cache_dir.path());
        let summary = mapper.load_all_areas().await.expect("load synthetic areas");
        assert_eq!(summary.areas.len(), areas, "every synthetic area loads");
        mapper
    });

    ScaledMapper {
        _runtime: runtime,
        _store_dir: store_dir,
        _cache_dir: cache_dir,
        mapper,
        area_ids,
        area0_rooms,
        total_rooms,
    }
}

/// A single-area atlas (so the whole atlas is one reachable component) with
/// the precomputed keys the pathfinding benches query.
struct PathBench {
    atlas: Arc<AtlasCache>,
    rooms: usize,
    /// Surface-grid room count (`rooms - rooms/50` basement rooms hang below).
    surface: usize,
    width: usize,
    /// Top-left grid corner (room 1).
    from: RoomKey,
    /// Bottom-right-most surface room — the far corner for `path_across`.
    to: RoomKey,
    /// Grid-center start for the nearest-tag searches.
    center: RoomKey,
    /// Unit-weight grid shortest path = manhattan distance + 1 nodes.
    expected_path_len: usize,
    label: String,
}

fn build_path_bench(rooms: usize) -> PathBench {
    let atlas = synthetic_atlas_cache(rooms, 1);
    let area_id = *atlas
        .areas()
        .next()
        .expect("single-area atlas has its area")
        .get_id();

    let surface = rooms - rooms / 50;
    let width = grid_width(surface);
    let to_index = surface - 1;
    let (to_row, to_col) = (to_index / width, to_index % width);
    let center_index = (width / 2) * width + width / 2;

    PathBench {
        atlas,
        rooms,
        surface,
        width,
        from: RoomKey::new(area_id, number(1)),
        to: RoomKey::new(area_id, number(surface)),
        center: RoomKey::new(area_id, number(center_index + 1)),
        expected_path_len: to_row + to_col + 1,
        label: size_label(rooms),
    }
}

/// Query corpus for the identification group: up to 32 shared
/// `(title, description)` buckets (>= 2 rooms each — the generator's generic
/// description under a pool title) followed by up to 32 unique buckets,
/// drawn from the atlas itself and sorted for determinism. Returns the
/// queries and how many of them are the shared-bucket kind.
fn build_ident_queries(atlas: &AtlasCache) -> (Vec<(String, String)>, usize) {
    let areas: Vec<_> = atlas.areas().collect();
    let mut buckets: HashMap<(&str, &str), usize> = HashMap::new();
    for area in &areas {
        for room in area.get_rooms() {
            *buckets
                .entry((room.get_title(), room.get_description()))
                .or_insert(0) += 1;
        }
    }

    let mut multi = Vec::new();
    let mut single = Vec::new();
    for (key, count) in &buckets {
        if *count >= 2 {
            multi.push(*key);
        } else {
            single.push(*key);
        }
    }
    multi.sort_unstable();
    single.sort_unstable();
    multi.truncate(32);
    single.truncate(32);
    assert!(
        !multi.is_empty(),
        "the corpus must contain shared (title, description) buckets"
    );

    let multi_len = multi.len();
    let queries = multi
        .into_iter()
        .chain(single)
        .map(|(t, d)| (t.to_owned(), d.to_owned()))
        .collect();
    (queries, multi_len)
}

/// Validates the write paths measure the real thing, restoring every mutation
/// so the benches start from the generated state:
/// - `upsert_room` of a new room grows the atlas by exactly one and the new
///   room resolves by key *and* through the rebuilt by-title index;
/// - `upsert_rooms` (batch) lands its edits;
/// - `delete_room` restores the count;
/// - `set_disabled_areas` (the `atlas_build` body) really rebuilds the
///   identification tables (see [`sanity_disabled_rebuild`]).
fn sanity_write_paths(m: &ScaledMapper) {
    let area0 = m.area_ids[0];
    let atlas = m.mapper.get_current_atlas();
    let total_before: usize = atlas.areas().map(|a| a.room_count()).sum();
    assert_eq!(
        total_before, m.total_rooms,
        "generator delivered the requested room count"
    );

    let room1_key = RoomKey::new(area0, RoomNumber(1));
    let room1_title: String = atlas
        .get_room(&room1_key)
        .expect("room 1 exists in area 0")
        .get_title()
        .to_owned();

    // upsert of a NEW room: count + 1, resolvable by key and via the rebuilt
    // identification index.
    let new_key = RoomKey::new(area0, number(m.area0_rooms + 1));
    m.mapper.upsert_room(
        new_key.clone(),
        RoomUpdates {
            title: Some("Sanity Room".to_owned()),
            x: Some(-8.0),
            y: Some(-8.0),
            ..RoomUpdates::default()
        },
    );
    let atlas = m.mapper.get_current_atlas();
    let total_after: usize = atlas.areas().map(|a| a.room_count()).sum();
    assert_eq!(
        total_after,
        total_before + 1,
        "upserting a new room grows the atlas by exactly one"
    );
    assert_eq!(
        atlas
            .get_room(&new_key)
            .expect("the upserted room resolves by key")
            .get_title(),
        "Sanity Room"
    );
    assert_eq!(
        atlas.get_rooms_by_title("Sanity Room").len(),
        1,
        "the write really re-ran the by-title index build"
    );

    // Batch path lands edits too.
    m.mapper.upsert_rooms(
        area0,
        vec![(
            RoomNumber(1),
            RoomUpdates {
                title: Some("Sanity Batch".to_owned()),
                ..RoomUpdates::default()
            },
        )],
    );
    let atlas = m.mapper.get_current_atlas();
    assert_eq!(
        atlas
            .get_room(&room1_key)
            .expect("room 1 still resolves")
            .get_title(),
        "Sanity Batch"
    );

    // Restore the generated state.
    m.mapper.delete_room(new_key.clone());
    m.mapper.upsert_room(
        room1_key.clone(),
        RoomUpdates {
            title: Some(room1_title.clone()),
            ..RoomUpdates::default()
        },
    );
    let atlas = m.mapper.get_current_atlas();
    assert!(
        atlas.get_room(&new_key).is_none(),
        "delete removed the sanity room"
    );
    assert_eq!(
        atlas.areas().map(|a| a.room_count()).sum::<usize>(),
        total_before
    );
    assert_eq!(
        atlas.get_room(&room1_key).expect("room 1").get_title(),
        room1_title
    );

    sanity_disabled_rebuild(m, &room1_title);
}

/// `set_disabled_areas` — the `atlas_build` timed body — must rebuild the
/// identification tables, not just flip a flag: disabling area 0 drops its
/// rooms from the lookups, re-enabling restores them.
fn sanity_disabled_rebuild(m: &ScaledMapper, room1_title: &str) {
    let area0 = m.area_ids[0];
    let atlas = m.mapper.get_current_atlas();
    let hits_enabled = atlas.get_rooms_by_title(room1_title).len();
    m.mapper
        .set_disabled_areas(std::iter::once(area0).collect());
    let disabled_atlas = m.mapper.get_current_atlas();
    assert!(
        disabled_atlas
            .get_rooms_by_title(room1_title)
            .all(|(id, _)| id != area0),
        "disabling area 0 drops its rooms from the by-title table"
    );
    assert!(disabled_atlas.get_rooms_by_title(room1_title).len() < hits_enabled);
    m.mapper.set_disabled_areas(HashSet::new());
    assert_eq!(
        m.mapper
            .get_current_atlas()
            .get_rooms_by_title(room1_title)
            .len(),
        hits_enabled,
        "re-enabling restores the lookup tables"
    );
}

/// Validates the pathfinding fixtures: the mirrored grid layout matches the
/// generator's real coordinates, the corner-to-corner path is exactly the
/// manhattan-shortest one, the tag hit lands on a genuinely tagged room, and
/// the miss tag matches nothing.
fn sanity_pathfinding(pb: &PathBench) {
    let near = |a: f32, b: f32| (a - b).abs() < 0.25;

    let from_room = pb.atlas.get_room(&pb.from).expect("corner room 1");
    assert!(
        near(from_room.get_x(), 0.0) && near(from_room.get_y(), 0.0),
        "room 1 sits at the grid origin"
    );
    let to_index = pb.surface - 1;
    let to_room = pb.atlas.get_room(&pb.to).expect("far corner room");
    assert!(
        near(to_room.get_x(), coord(to_index % pb.width))
            && near(to_room.get_y(), coord(to_index / pb.width)),
        "mirrored grid layout drifted from bench/src/atlas.rs"
    );
    let center_room = pb.atlas.get_room(&pb.center).expect("grid-center room");
    assert!(
        near(center_room.get_x(), coord(pb.width / 2))
            && near(center_room.get_y(), coord(pb.width / 2)),
        "center room sits mid-grid"
    );

    let path = pb
        .atlas
        .get_path_between_rooms(&pb.from, &pb.to)
        .expect("a corner-to-corner path exists");
    assert!(path.len() > 1);
    assert_eq!(
        path.len(),
        pb.expected_path_len,
        "unit-weight grid shortest path is manhattan distance + 1 nodes"
    );

    let hit = pb
        .atlas
        .find_nearest_room_with_tag(&pb.center, "inn")
        .expect("an INN room is reachable from the grid center");
    assert!(
        pb.atlas
            .get_room(&hit)
            .expect("the hit resolves")
            .get_tags()
            .contains("INN"),
        "nearest-tag hit really carries the tag"
    );

    assert!(
        pb.atlas
            .find_nearest_room_with_tag(&pb.center, MISS_TAG)
            .is_none(),
        "the miss tag must match nothing (full-exploration worst case)"
    );
}

/// Validates the identification queries: shared buckets resolve to >= 2
/// rooms, unique buckets to exactly 1.
fn sanity_identification(atlas: &AtlasCache, queries: &[(String, String)], multi_len: usize) {
    for (i, (title, description)) in queries.iter().enumerate() {
        let hits = atlas
            .get_rooms_by_title_and_description(title, description)
            .len();
        if i < multi_len {
            assert!(
                hits >= 2,
                "shared bucket ({title:?}) resolves several rooms"
            );
        } else {
            assert_eq!(hits, 1, "unique bucket ({title:?}) resolves one room");
        }
    }
}

#[allow(clippy::too_many_lines)]
fn mapper_scale(c: &mut Criterion) {
    let big = big_rooms();
    eprintln!(
        "sizes: SMALL={SMALL} MED={MED} BIG={big} rooms (~{ROOMS_PER_AREA}-room areas; \
         BIG = clamp(25 x SMUDGY_BENCH_LINES, {SMALL}, {BIG_DEFAULT}) when the env var is set)"
    );

    let build_timed = |rooms: usize| {
        let start = Instant::now();
        let m = build_scaled_mapper(rooms);
        eprintln!(
            "  built {} mapper: {} areas / {} rooms in {:.1}s",
            size_label(rooms),
            m.area_ids.len(),
            m.total_rooms,
            start.elapsed().as_secs_f32()
        );
        m
    };
    let small_mapper = build_timed(SMALL);
    let med_mapper = build_timed(MED);
    // A BIG that collides with a fixed size would duplicate benchmark ids;
    // skip it (the fixed-size entry already covers that scale).
    let big_mapper = (big != SMALL && big != MED).then(|| build_timed(big));

    // Snapshot for the identification group: an Arc pinned before any write
    // bench runs, so later mutations (which swap fresh caches into the
    // mapper) cannot touch what this group measures.
    let ident_atlas = med_mapper.mapper.get_current_atlas();
    let (ident_queries, ident_multi) = build_ident_queries(&ident_atlas);
    eprintln!(
        "identification queries: {} shared-bucket + {} unique-bucket",
        ident_multi,
        ident_queries.len() - ident_multi
    );

    let build_path_timed = |rooms: usize| {
        let start = Instant::now();
        let pb = build_path_bench(rooms);
        eprintln!(
            "  built single-area path atlas: {} rooms ({} surface, width {}) in {:.1}s",
            pb.rooms,
            pb.surface,
            pb.width,
            start.elapsed().as_secs_f32()
        );
        pb
    };
    let path_med = build_path_timed(MED);
    let path_big = (big != MED).then(|| build_path_timed(big));

    if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
        sanity_write_paths(&small_mapper);
        sanity_write_paths(&med_mapper);
        if let Some(m) = &big_mapper {
            sanity_write_paths(m);
        }
        sanity_pathfinding(&path_med);
        if let Some(pb) = &path_big {
            sanity_pathfinding(pb);
        }
        sanity_identification(&ident_atlas, &ident_queries, ident_multi);
        eprintln!("sanity: write round-trips, path shapes, and lookup buckets all verified");
    }

    let mut write_targets: Vec<&ScaledMapper> = vec![&small_mapper, &med_mapper];
    if let Some(m) = &big_mapper {
        write_targets.push(m);
    }

    // -- Group 1: the atlas-level identification-index rebuild -------------
    let mut group = c.benchmark_group("atlas_build");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for &m in &write_targets {
        group.throughput(Throughput::Elements(m.total_rooms as u64));
        group.bench_function(BenchmarkId::new("cold", size_label(m.total_rooms)), |b| {
            b.iter(|| m.mapper.set_disabled_areas(HashSet::new()));
        });
    }
    group.finish();

    // -- Group 2: the full single-room write (the RCU cliff) ---------------
    let mut group = c.benchmark_group("upsert_room");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for &m in &write_targets {
        let key = RoomKey::new(m.area_ids[0], RoomNumber(1));
        let mut seq: u64 = 0;
        group.throughput(Throughput::Elements(1));
        group.bench_function(BenchmarkId::new("single", size_label(m.total_rooms)), |b| {
            b.iter_batched(
                || {
                    seq += 1;
                    (key.clone(), room_edit(seq))
                },
                |(key, updates)| m.mapper.upsert_room(key, updates),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();

    // -- Group 3: the batch amortization curve at the 10k atlas ------------
    let mut group = c.benchmark_group("upsert_rooms");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    let batch_area = med_mapper.area_ids[0];
    for &batch in &[1_usize, 16, 256] {
        assert!(
            batch <= med_mapper.area0_rooms,
            "batches edit existing rooms of area 0"
        );
        let mut seq: u64 = 0;
        group.throughput(Throughput::Elements(batch as u64));
        group.bench_function(BenchmarkId::new("batch_10k", batch), |b| {
            b.iter_batched(
                || {
                    seq += 1;
                    (1..=batch)
                        .map(|n| (number(n), room_edit(seq)))
                        .collect::<Vec<_>>()
                },
                |updates| med_mapper.mapper.upsert_rooms(batch_area, updates),
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();

    // -- Group 4: Dijkstra exploration shapes ------------------------------
    // No throughput annotation: the three shapes explore wildly different
    // node counts, so a shared elements/sec rate would mislead.
    let mut group = c.benchmark_group("pathfinding");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    let mut path_benches: Vec<&PathBench> = vec![&path_med];
    if let Some(pb) = &path_big {
        path_benches.push(pb);
    }
    for &pb in &path_benches {
        group.bench_function(BenchmarkId::new("path_across", &pb.label), |b| {
            b.iter(|| black_box(pb.atlas.get_path_between_rooms(&pb.from, &pb.to)));
        });
        group.bench_function(BenchmarkId::new("nearest_tag_hit", &pb.label), |b| {
            b.iter(|| black_box(pb.atlas.find_nearest_room_with_tag(&pb.center, "inn")));
        });
        group.bench_function(BenchmarkId::new("nearest_tag_miss", &pb.label), |b| {
            b.iter(|| black_box(pb.atlas.find_nearest_room_with_tag(&pb.center, MISS_TAG)));
        });
    }
    group.finish();

    // -- Group 5: identification lookups (per-call format! key included) ---
    let mut group = c.benchmark_group("identification");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Elements(ident_queries.len() as u64));
    group.bench_function(BenchmarkId::new("by_title_and_description", "10k"), |b| {
        b.iter(|| {
            let mut matches = 0_usize;
            for (title, description) in &ident_queries {
                for hit in ident_atlas.get_rooms_by_title_and_description(title, description) {
                    black_box(&hit);
                    matches += 1;
                }
            }
            black_box(matches)
        });
    });
    group.finish();
}

criterion_group!(benches, mapper_scale);
criterion_main!(benches);
