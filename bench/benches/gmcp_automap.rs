//! Auto-map write costs at scale (`docs/gmcp-mapping.md` §6): the per-movement work
//! the `auto-mapper` package generates, driven through the real production path — a
//! `Mapper` over a `CompositeBackend` whose ephemeral tier receives the creations, so the
//! timed body includes the ephemeral-cap check and the tier routing, exactly as a session
//! pays them.
//!
//! - `automap_step/create_room/<size>`: `Mapper::upsert_room` of a **new** room carrying
//!   an `external_id` binding, into an ephemeral area, with `<size>` rooms already loaded.
//!   This is the §6 blocker check: the RCU rebuild is O(loaded rooms), so this cell is
//!   what gates the area-scoped-rebuild optimization. Each iteration creates a fresh room
//!   (the atlas grows by a few hundred rooms over a run — <1% at these sizes; treat the
//!   number as an upper bound at `<size>`).
//! - `follow/find_room_by_external_id/<size>`: the follow-mode hot path — one reverse-
//!   index probe. Expected O(1) and scale-flat; the cell exists to prove it stays that
//!   way (the alternative, a property scan, is the O(all rooms) hazard the field was
//!   built to remove).
//!
//! Sizes: 10k and 100k by default. The plan's 1M procedural-grid corpus is behind
//! `SMUDGY_BENCH_AUTOMAP_BIG=1` — building it takes minutes and ~GBs of RAM, which is
//! too heavy for the default bench sweep but exactly the datapoint the §6 decision needs.

use std::{hint::black_box, sync::Arc, time::Instant};

use criterion::{
    BatchSize, BenchmarkId, Criterion, SamplingMode, Throughput, criterion_group, criterion_main,
};
use smudgy_bench::atlas::synthetic_area;
use smudgy_cloud::{
    AreaId, AreaWithDetails, CloudMapper, CompositeBackend, LocalBackend, Mapper, MapperBackend,
    RoomNumber, RoomUpdates, mapper::RoomKey,
};

const MED: usize = 10_000;
const BIG: usize = 100_000;
const HUGE: usize = 1_000_000;
const ROOMS_PER_AREA: usize = 500;
/// External ids seeded onto area 0 for the follow-lookup cell.
const SEEDED_IDS: usize = 500;

fn size_label(rooms: usize) -> String {
    if rooms.is_multiple_of(1_000_000) {
        format!("{}m", rooms / 1_000_000)
    } else {
        format!("{}k", rooms / 1_000)
    }
}

struct AutomapBench {
    _runtime: tokio::runtime::Runtime,
    _store_dir: tempfile::TempDir,
    _cache_dir: tempfile::TempDir,
    mapper: Mapper,
    /// The ephemeral area auto-map creations land in.
    session_area: AreaId,
    /// First unused room number in the ephemeral area (grows during the run).
    next_room: i32,
    total_rooms: usize,
}

/// Corpus + mapper construction mirroring a real session: synthetic areas imported into
/// the local tier, fanned through a composite with a dead cloud, one ephemeral area
/// created for the auto-map writes.
fn build(total_rooms: usize) -> AutomapBench {
    let areas = (total_rooms / ROOMS_PER_AREA).max(1);
    let base = total_rooms / areas;
    let remainder = total_rooms % areas;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench tokio runtime");
    let store_dir = tempfile::tempdir().expect("temp local map store");
    let cache_dir = tempfile::tempdir().expect("temp mapper cache dir");

    let (mapper, session_area) = runtime.block_on(async {
        let local = LocalBackend::new(store_dir.path());
        for i in 0..areas {
            let index = u32::try_from(i).expect("area count fits in u32");
            let mut details: AreaWithDetails =
                synthetic_area(index, base + usize::from(i < remainder));
            // Seed external ids onto area 0 so the follow cell has bindings to probe.
            if i == 0 {
                for (n, room) in details.rooms.iter_mut().enumerate().take(SEEDED_IDS) {
                    room.external_id = Some(format!("ext-{n}"));
                }
            }
            local
                .import_local_area(details)
                .await
                .expect("import synthetic area");
        }
        let cloud = CloudMapper::new("http://127.0.0.1:0".to_string(), "bench".to_string());
        let backend: Arc<dyn MapperBackend + Send + Sync> =
            Arc::new(CompositeBackend::new(Arc::new(local), Arc::new(cloud)));
        let mapper = Mapper::new(backend, cache_dir.path());
        mapper.load_all_areas().await.expect("load synthetic areas");
        let session_area = mapper
            .create_area_ephemeral("bench session map".to_string())
            .await
            .expect("create ephemeral area");
        (mapper, session_area)
    });

    AutomapBench {
        _runtime: runtime,
        _store_dir: store_dir,
        _cache_dir: cache_dir,
        mapper,
        session_area,
        next_room: 1,
        total_rooms,
    }
}

/// The write the auto-mapper's room creation performs: title + placement + external id.
fn automap_room(seq: i32) -> RoomUpdates {
    RoomUpdates {
        title: Some(format!("Automapped {}", seq % 8)),
        x: Some(f32::from(i16::try_from(seq % 100).expect("bounded")) * 2.0),
        y: Some(f32::from(i16::try_from(seq / 100 % 100).expect("bounded")) * 2.0),
        level: Some(0),
        external_id: Some(Some(format!("bench-room-{seq}"))),
        ..RoomUpdates::default()
    }
}

fn sanity(bench: &mut AutomapBench) {
    let atlas = bench.mapper.get_current_atlas();
    let loaded: usize = atlas.areas().map(|a| a.room_count()).sum();
    assert_eq!(loaded, bench.total_rooms, "corpus loaded in full");
    assert!(bench.mapper.is_ephemeral(&bench.session_area));
    assert!(
        atlas.find_room_by_external_id("ext-0").is_some(),
        "seeded external ids resolve"
    );

    // One full create round-trip through the ephemeral tier.
    let key = RoomKey::new(bench.session_area, RoomNumber(bench.next_room));
    bench.next_room += 1;
    bench.mapper.upsert_room(key.clone(), automap_room(0));
    let atlas = bench.mapper.get_current_atlas();
    let (found, room) = atlas
        .find_room_by_external_id("bench-room-0")
        .expect("created room resolves by external id");
    assert_eq!(found, key);
    assert_eq!(room.get_title(), "Automapped 0");
}

fn gmcp_automap(c: &mut Criterion) {
    let mut sizes = vec![MED, BIG];
    if std::env::var("SMUDGY_BENCH_AUTOMAP_BIG").is_ok() {
        sizes.push(HUGE);
    }

    let mut benches: Vec<AutomapBench> = sizes
        .iter()
        .map(|&rooms| {
            let start = Instant::now();
            let b = build(rooms);
            eprintln!(
                "  built {} automap corpus in {:.1}s",
                size_label(rooms),
                start.elapsed().as_secs_f32()
            );
            b
        })
        .collect();

    if std::env::var("SMUDGY_BENCH_SKIP_SANITY").is_err() {
        for bench in &mut benches {
            sanity(bench);
        }
        eprintln!("sanity: ephemeral create + external-id resolve verified at every size");
    }

    let mut group = c.benchmark_group("automap_step");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for bench in &mut benches {
        let label = size_label(bench.total_rooms);
        let area = bench.session_area;
        let mapper = bench.mapper.clone();
        let mut seq = bench.next_room;
        group.throughput(Throughput::Elements(1));
        group.bench_function(BenchmarkId::new("create_room", &label), |b| {
            b.iter_batched(
                || {
                    let key = RoomKey::new(area, RoomNumber(seq));
                    let updates = automap_room(seq);
                    seq += 1;
                    (key, updates)
                },
                |(key, updates)| mapper.upsert_room(key, updates),
                BatchSize::SmallInput,
            );
        });
        bench.next_room = seq;
    }
    group.finish();

    let mut group = c.benchmark_group("follow");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for bench in &benches {
        let label = size_label(bench.total_rooms);
        let atlas = bench.mapper.get_current_atlas();
        let mut probe = 0_usize;
        group.throughput(Throughput::Elements(1));
        group.bench_function(BenchmarkId::new("find_room_by_external_id", &label), |b| {
            b.iter(|| {
                probe = (probe + 1) % SEEDED_IDS;
                black_box(atlas.find_room_by_external_id(&format!("ext-{probe}")))
            });
        });
    }
    group.finish();
}

criterion_group!(benches, gmcp_automap);
criterion_main!(benches);
