//! The headless-benchable portion of map rendering: per-frame spatial queries
//! and the connection-pairing rebuild of the mapper's area cache.
//!
//! `MapView::draw` (`map_widget/src/map_view.rs`) rebuilds its canvas geometry
//! from scratch every frame — there is no `canvas::Cache` — so each frame pays
//! one padded viewport rectangle queried against `AreaCache`'s two R-trees
//! (`cloud/src/mapper/area_cache.rs`, `with_rooms_in` /
//! `with_room_connections_in`) **five times over each**: the current level
//! plus four ghost levels (±1, ±2). The query API carries no level parameter;
//! the draw path filters by level inside the closure, paying the full R-tree
//! yield at every level slot. Editing a room takes the other expensive path:
//! `Mapper::upsert_room` (`cloud/src/mapper.rs`) →
//! `AreaCache::rebuild_room_state` — the O(rooms × exits)
//! `build_room_connections` pairing pass plus two `RTree::bulk_load`s —
//! followed by `AtlasCache::insert_area` → `new_with_areas`
//! (`cloud/src/mapper/atlas_cache.rs`), which rebuilds the
//! room-identification lookup tables over every room. Both costs sit on
//! interactive paths (every rendered frame; every room edit while mapping),
//! so together they bound how large an area the map can display and edit
//! smoothly.
//!
//! Groups:
//!   - `spatial_query/{rooms,connections}/viewport_{small,medium,full}/{10k,50k}`:
//!     raw envelope queries over rectangles covering ~2% / ~20% / 100% of the
//!     area extent. Every yielded item is passed through `black_box`;
//!     `Throughput::Elements` counts items yielded (measured once in setup).
//!   - `frame_proxy/10k`: one "frame" = the draw loop's actual query pattern —
//!     the medium viewport queried once per level slot against each index
//!     (5 connection queries + 5 room queries), with the draw path's level
//!     filters in the closures. The honest per-frame query cost.
//!   - `rebuild/room_connections/{1k,10k,50k}`: one room edit through
//!     `Mapper::upsert_room`, the narrowest *public* seam that triggers the
//!     area's room-state rebuild (`AreaCache`'s constructors and mutators are
//!     crate-private). The measurement therefore also includes the
//!     `AtlasCache` lookup-table rebuild, the `ArcSwap` rcu, and one
//!     unbounded-channel enqueue (the fire-and-forget sync send) — i.e. the
//!     full shipped cost of a single room edit at scale.
//!
//! The corpus is the deterministic synthetic atlas generator from
//! `smudgy_bench::atlas` (surface grid + ~2% basement layer + cross-level
//! exits), loaded through the public `LocalBackend` →
//! `Mapper::load_all_areas` path — the same route `synthetic_atlas_cache`
//! takes, replicated here because the rebuild group needs the live `Mapper`
//! handle that helper drops.
//!
//! No log corpus is used, so `SMUDGY_BENCH_LINES` does not apply. The sanity
//! checks (spatial queries validated against ground-truth scans; the rebuild
//! seam validated by rev/title/connection-count observation) are skippable
//! via `SMUDGY_BENCH_SKIP_SANITY=1`.

use std::{hint::black_box, sync::Arc};

use criterion::{
    BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use smudgy_bench::atlas::synthetic_area;
use smudgy_cloud::{
    AreaId, LocalBackend, Mapper, MapperBackend, RoomNumber, RoomUpdates,
    mapper::{RoomKey, area_cache::AreaCache},
};

/// Mirrors `GHOST_LEVEL_SPREAD` in `map_widget/src/map_view.rs`: the draw
/// loop queries ghost levels at ±1..=±this around the current level.
const GHOST_LEVEL_SPREAD: i32 = 2;

/// Mirrors `MapView::SPATIAL_QUERY_PADDING` (`map_widget/src/map_view.rs`):
/// the visible region is padded by this much before querying the indices.
const SPATIAL_QUERY_PADDING: f32 = 1.0;

/// Fraction of the area extent the "medium" viewport covers; also the
/// rectangle `frame_proxy` uses, so the two stay comparable.
const MEDIUM_FRACTION: f32 = 0.20;

/// Viewport shapes benchmarked by the `spatial_query` group.
const VIEWPORTS: &[(&str, f32)] = &[
    ("viewport_small", 0.02),
    ("viewport_medium", MEDIUM_FRACTION),
    ("viewport_full", 1.0),
];

/// Query rectangle in map coordinates (the argument shape
/// `with_rooms_in`/`with_room_connections_in` take).
#[derive(Clone, Copy)]
struct Rect {
    min_x: f32,
    min_y: f32,
    max_x: f32,
    max_y: f32,
}

/// A synthetic area loaded through the shipped cache-build path, with the
/// live `Mapper` kept so the rebuild group can drive `upsert_room`.
struct LoadedArea {
    mapper: Mapper,
    area_id: AreaId,
    /// Immutable snapshot taken at load time; the query groups run against
    /// this even while the rebuild group swaps newer caches into the mapper.
    area: Arc<AreaCache>,
    /// Kept alive so the mapper's spawned sync task (and its channel
    /// receiver) survive: a dropped runtime closes the channel and reroutes
    /// every fire-and-forget write through the warn-and-count failure arm,
    /// changing what `upsert_room` costs. The runtime is never polled after
    /// setup, so the enqueued operations sit in the channel and no background
    /// work competes with the timed thread.
    _runtime: tokio::runtime::Runtime,
    _store_dir: tempfile::TempDir,
    _cache_dir: tempfile::TempDir,
}

/// Builds a one-area mapper holding `rooms` synthetic rooms: import into a
/// temp-dir `LocalBackend`, `Mapper::load_all_areas`, snapshot the loaded
/// `AreaCache` — the exact cache-build path a real session exercises.
fn load_area(label: &str, rooms: usize, area_index: u32) -> LoadedArea {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench tokio runtime");
    let (mapper, store_dir, cache_dir) = runtime.block_on(async {
        let store_dir = tempfile::tempdir().expect("temp local map store");
        let cache_dir = tempfile::tempdir().expect("temp mapper cache dir");
        let backend = LocalBackend::new(store_dir.path());
        backend
            .import_local_area(synthetic_area(area_index, rooms))
            .await
            .expect("import synthetic area");
        let mapper = Mapper::new(Arc::new(backend), cache_dir.path());
        let summary = mapper.load_all_areas().await.expect("load synthetic area");
        assert_eq!(summary.areas.len(), 1, "exactly one synthetic area loads");
        (mapper, store_dir, cache_dir)
    });

    let area = mapper
        .get_current_atlas()
        .areas()
        .next()
        .expect("the loaded area is in the atlas");
    assert_eq!(area.room_count(), rooms, "every room must survive the load");
    let area_id = *area.get_id();
    eprintln!(
        "  {label}: {} rooms / {} connections (area {area_id})",
        area.room_count(),
        area.get_room_connections().len()
    );

    LoadedArea {
        mapper,
        area_id,
        area,
        _runtime: runtime,
        _store_dir: store_dir,
        _cache_dir: cache_dir,
    }
}

/// Bounding box of every room in the area (level ignored: basement rooms sit
/// directly under their surface anchors, inside the surface extent).
fn area_extent(area: &AreaCache) -> Rect {
    let mut extent = Rect {
        min_x: f32::MAX,
        min_y: f32::MAX,
        max_x: f32::MIN,
        max_y: f32::MIN,
    };
    for room in area.get_rooms() {
        extent.min_x = extent.min_x.min(room.get_x());
        extent.min_y = extent.min_y.min(room.get_y());
        extent.max_x = extent.max_x.max(room.get_x());
        extent.max_y = extent.max_y.max(room.get_y());
    }
    extent
}

/// A viewport covering `fraction` of the extent's area, centered on the
/// extent and padded the way the draw path pads its visible region.
fn viewport(extent: Rect, fraction: f32) -> Rect {
    let scale = fraction.sqrt();
    let half_w = (extent.max_x - extent.min_x) * scale / 2.0;
    let half_h = (extent.max_y - extent.min_y) * scale / 2.0;
    let center_x = f32::midpoint(extent.min_x, extent.max_x);
    let center_y = f32::midpoint(extent.min_y, extent.max_y);
    Rect {
        min_x: center_x - half_w - SPATIAL_QUERY_PADDING,
        min_y: center_y - half_h - SPATIAL_QUERY_PADDING,
        max_x: center_x + half_w + SPATIAL_QUERY_PADDING,
        max_y: center_y + half_h + SPATIAL_QUERY_PADDING,
    }
}

fn count_rooms_in(area: &AreaCache, rect: Rect) -> u64 {
    let mut yielded: u64 = 0;
    area.with_rooms_in(rect.min_x, rect.min_y, rect.max_x, rect.max_y, |_| {
        yielded += 1;
    });
    yielded
}

fn count_connections_in(area: &AreaCache, rect: Rect) -> u64 {
    let mut yielded: u64 = 0;
    area.with_room_connections_in(rect.min_x, rect.min_y, rect.max_x, rect.max_y, |_| {
        yielded += 1;
    });
    yielded
}

/// One frame's worth of spatial queries, exactly as `MapView::draw` issues
/// them: per ghost distance (farthest first) and each of its ± levels, one
/// connection query then one room query with the level filter inside the
/// closure, then the current level's pair. Returns how many items survive
/// the filters (the items the draw path would actually paint).
fn frame_queries(area: &AreaCache, rect: Rect, level: i32) -> u64 {
    let mut painted: u64 = 0;
    for distance in (1..=GHOST_LEVEL_SPREAD).rev() {
        for delta in [-distance, distance] {
            let ghost_level = level + delta;
            area.with_room_connections_in(
                rect.min_x,
                rect.min_y,
                rect.max_x,
                rect.max_y,
                |connection| {
                    if connection.from_level == ghost_level {
                        black_box(connection);
                        painted += 1;
                    }
                },
            );
            area.with_rooms_in(rect.min_x, rect.min_y, rect.max_x, rect.max_y, |room| {
                if room.get_level() == ghost_level {
                    black_box(room);
                    painted += 1;
                }
            });
        }
    }
    area.with_room_connections_in(
        rect.min_x,
        rect.min_y,
        rect.max_x,
        rect.max_y,
        |connection| {
            if connection.from_level == level {
                black_box(connection);
                painted += 1;
            }
        },
    );
    area.with_rooms_in(rect.min_x, rect.min_y, rect.max_x, rect.max_y, |room| {
        if room.get_level() == level {
            black_box(room);
            painted += 1;
        }
    });
    painted
}

fn sanity_skipped() -> bool {
    // House convention across the suite: any set value skips, matching the
    // `is_err()` gates in the sibling benches.
    std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_ok()
}

/// The spatial indices really cover the area: a full-extent room query yields
/// exactly the area's room count, its level-0 subset matches a ground-truth
/// scan of the room list, and a full-extent connection query yields exactly
/// the built connection set (every connection envelope lies within the room
/// extent, so full coverage is exhaustive).
fn query_sanity(label: &str, area: &AreaCache) {
    let full = viewport(area_extent(area), 1.0);

    let yielded = count_rooms_in(area, full);
    assert_eq!(
        yielded,
        area.room_count() as u64,
        "{label}: full-viewport room query must yield every room"
    );

    let direct_level0 = area
        .get_rooms()
        .iter()
        .filter(|room| room.get_level() == 0)
        .count() as u64;
    let mut query_level0: u64 = 0;
    area.with_rooms_in(full.min_x, full.min_y, full.max_x, full.max_y, |room| {
        if room.get_level() == 0 {
            query_level0 += 1;
        }
    });
    assert_eq!(
        query_level0, direct_level0,
        "{label}: the query's level-0 subset must match a direct scan"
    );

    let connections = count_connections_in(area, full);
    assert_eq!(
        connections,
        area.get_room_connections().len() as u64,
        "{label}: full-viewport connection query must yield every connection"
    );
}

/// `Mapper::upsert_room` really swaps in a rebuilt area: the rev bumps, the
/// edit is visible, and the (freshly rebuilt) connection set is unchanged by
/// a title-only edit.
fn rebuild_sanity(loaded: &LoadedArea) {
    let before = loaded
        .mapper
        .get_current_atlas()
        .get_area(&loaded.area_id)
        .expect("area present before the edit");
    let rev = before.get_rev();
    let connections = before.get_room_connections().len();

    loaded.mapper.upsert_room(
        RoomKey::new(loaded.area_id, RoomNumber(1)),
        RoomUpdates {
            title: Some("Sanity Landmark".to_string()),
            ..RoomUpdates::default()
        },
    );

    let after = loaded
        .mapper
        .get_current_atlas()
        .get_area(&loaded.area_id)
        .expect("area present after the edit");
    assert_eq!(
        after.get_rev(),
        rev + 1,
        "upsert_room must swap in a rebuilt area"
    );
    assert_eq!(
        after
            .get_room(&RoomNumber(1))
            .expect("room 1 exists")
            .get_title(),
        "Sanity Landmark",
        "the edit must be visible in the swapped cache"
    );
    assert_eq!(
        after.get_room_connections().len(),
        connections,
        "a title-only edit rebuilds the connections without changing them"
    );
}

/// `spatial_query/{rooms,connections}/<viewport>/<size>`: one R-tree envelope
/// query per iteration, passing every yielded item through `black_box`.
/// Yield counts are measured in setup and reported as `Throughput::Elements`.
fn spatial_query_group(c: &mut Criterion, sizes: &[(&str, &LoadedArea)]) {
    let mut group = c.benchmark_group("spatial_query");
    group.sample_size(10);
    // Flat sampling: criterion's recommended mode for benches whose
    // iterations run long (the 50k full-viewport queries are ms-scale).
    group.sampling_mode(SamplingMode::Flat);

    for &(size_label, loaded) in sizes {
        let area = &loaded.area;
        let extent = area_extent(area);
        for &(vp_label, fraction) in VIEWPORTS {
            let rect = viewport(extent, fraction);
            let rooms_yielded = count_rooms_in(area, rect);
            let connections_yielded = count_connections_in(area, rect);
            eprintln!(
                "  {size_label}/{vp_label}: rect ({:.1},{:.1})..({:.1},{:.1}) yields {rooms_yielded} rooms / {connections_yielded} connections",
                rect.min_x, rect.min_y, rect.max_x, rect.max_y
            );

            group.throughput(Throughput::Elements(rooms_yielded));
            group.bench_function(
                BenchmarkId::new(format!("rooms/{vp_label}"), size_label),
                |b| {
                    b.iter(|| {
                        let mut yielded: u64 = 0;
                        area.with_rooms_in(
                            rect.min_x,
                            rect.min_y,
                            rect.max_x,
                            rect.max_y,
                            |room| {
                                black_box(room);
                                yielded += 1;
                            },
                        );
                        black_box(yielded);
                    });
                },
            );

            group.throughput(Throughput::Elements(connections_yielded));
            group.bench_function(
                BenchmarkId::new(format!("connections/{vp_label}"), size_label),
                |b| {
                    b.iter(|| {
                        let mut yielded: u64 = 0;
                        area.with_room_connections_in(
                            rect.min_x,
                            rect.min_y,
                            rect.max_x,
                            rect.max_y,
                            |connection| {
                                black_box(connection);
                                yielded += 1;
                            },
                        );
                        black_box(yielded);
                    });
                },
            );
        }
    }
    group.finish();
}

/// `frame_proxy/<size>`: the full 10-query frame pattern over the medium
/// viewport. Throughput counts items *yielded* across all level slots — the
/// R-tree work a frame pays — which exceeds the items painted (off-level
/// yields are filtered in the closures, exactly as the draw path does).
fn frame_proxy_group(c: &mut Criterion, size_label: &str, loaded: &LoadedArea) {
    let area = &loaded.area;
    let rect = viewport(area_extent(area), MEDIUM_FRACTION);
    let level_slots =
        u64::try_from(GHOST_LEVEL_SPREAD * 2 + 1).expect("level-slot count is positive");
    let yielded_per_frame =
        level_slots * (count_rooms_in(area, rect) + count_connections_in(area, rect));
    let painted_per_frame = frame_queries(area, rect, 0);
    eprintln!(
        "  frame_proxy/{size_label}: {yielded_per_frame} items yielded / {painted_per_frame} painted per frame ({level_slots} level slots)"
    );

    let mut group = c.benchmark_group("frame_proxy");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    group.throughput(Throughput::Elements(yielded_per_frame));
    group.bench_function(BenchmarkId::from_parameter(size_label), |b| {
        b.iter(|| black_box(frame_queries(area, rect, 0)));
    });
    group.finish();
}

/// `rebuild/room_connections/<size>`: one room edit per iteration through
/// `Mapper::upsert_room` — `AreaCache::upsert_room` → `rebuild_room_state`
/// (`build_room_connections` + two `RTree::bulk_load`s) plus the
/// `AtlasCache::insert_area` lookup-table rebuild, the rcu swap, and the sync
/// enqueue. Throughput counts the area's rooms, so criterion's elements/sec
/// exposes the per-room rebuild rate across the size curve. The enqueued sync
/// operations are never drained (the mapper's runtime is parked), so each
/// iteration adds one small op to the unbounded channel and no background
/// work runs.
fn rebuild_group(c: &mut Criterion, sizes: &[(&str, &LoadedArea)]) {
    let mut group = c.benchmark_group("rebuild");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);

    for &(size_label, loaded) in sizes {
        let key = RoomKey::new(loaded.area_id, RoomNumber(1));
        let updates = RoomUpdates {
            title: Some("A Renamed Landmark".to_string()),
            ..RoomUpdates::default()
        };
        group.throughput(Throughput::Elements(loaded.area.room_count() as u64));
        group.bench_function(BenchmarkId::new("room_connections", size_label), |b| {
            b.iter(|| {
                // `upsert_room` takes its arguments by value; these clones
                // (a 24-byte key and one small String) are nanoseconds
                // against the ms-scale rebuild being measured.
                loaded.mapper.upsert_room(key.clone(), updates.clone());
                black_box(loaded.mapper.get_current_atlas());
            });
        });
    }
    group.finish();
}

fn map_spatial(c: &mut Criterion) {
    eprintln!("building synthetic areas via LocalBackend -> Mapper::load_all_areas:");
    let small = load_area("1k", 1_000, 1);
    let mid = load_area("10k", 10_000, 10);
    let big = load_area("50k", 50_000, 50);

    if sanity_skipped() {
        eprintln!("SMUDGY_BENCH_SKIP_SANITY=1: skipping sanity checks");
    } else {
        query_sanity("10k", &mid.area);
        query_sanity("50k", &big.area);
        rebuild_sanity(&small);
    }

    spatial_query_group(c, &[("10k", &mid), ("50k", &big)]);
    frame_proxy_group(c, "10k", &mid);
    rebuild_group(c, &[("1k", &small), ("10k", &mid), ("50k", &big)]);
}

criterion_group!(benches, map_spatial);
criterion_main!(benches);
