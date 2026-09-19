//! End-to-end local saves, including persistence, acknowledgement and publication.
//! Setup and postcondition checks are outside the timer. Unlike the optimistic
//! write groups, these drive the runtime until each write completes.
//!
//! `local_merge` requires `--features pr-benchmarks`. Sizes vary both the total
//! atlas and the touched area.
//! Merges cover whole grids, partial moves cutting grid connections, and a
//! reciprocal cross-area link per room. Every merge awaits session visibility.

use criterion::{BenchmarkId, Criterion, SamplingMode};
use smudgy_bench::atlas::synthetic_area;
use smudgy_cloud::{
    AreaId, LocalBackend, Mapper, MapperBackend, RoomNumber, RoomUpdates, mapper::RoomKey,
};
use std::sync::Arc;

struct Fixture {
    runtime: tokio::runtime::Runtime,
    mapper: Mapper,
    backend: Arc<LocalBackend>,
    ids: Vec<AreaId>,
    _store: tempfile::TempDir,
    _cache: tempfile::TempDir,
}

impl Fixture {
    fn new(total: usize, area_rooms: usize) -> Self {
        Self::from_documents(
            (0..total / area_rooms)
                .map(|index| synthetic_area(u32::try_from(index).unwrap(), area_rooms))
                .collect(),
        )
    }

    fn from_documents(documents: Vec<smudgy_cloud::AreaWithDetails>) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let store = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let backend = Arc::new(LocalBackend::new(store.path()));
        let mut ids = Vec::new();
        let mapper = runtime.block_on(async {
            for area in documents {
                ids.push(area.area.id);
                backend.import_local_area(area).await.unwrap();
            }
            let mapper = Mapper::new(backend.clone(), cache.path());
            mapper.load_all_areas().await.unwrap();
            mapper
        });
        Self {
            runtime,
            mapper,
            backend,
            ids,
            _store: store,
            _cache: cache,
        }
    }
}

pub fn local_writes(c: &mut Criterion) {
    let mut group = c.benchmark_group("local_save");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for (total, area_rooms) in [(1_000, 500), (10_000, 500), (50_000, 500), (10_000, 5_000)] {
        let fixture = Fixture::new(total, area_rooms);
        let id = fixture.ids[0];
        let size = format!("{total}_rooms/{area_rooms}_touched");
        let mut sequence = 0;
        let mut save_room = || {
            sequence += 1;
            fixture.runtime.block_on(async {
                let submitted = fixture
                    .mapper
                    .upsert_room(
                        RoomKey::new(id, RoomNumber(1)),
                        RoomUpdates {
                            title: Some(format!("Saved room {}", sequence % 8)),
                            ..Default::default()
                        },
                    )
                    .unwrap();
                fixture
                    .mapper
                    .wait_for_mutation(submitted.operation_id().unwrap())
                    .await
                    .unwrap();
            });
        };
        save_room();
        assert_eq!(
            fixture
                .runtime
                .block_on(fixture.backend.get_area(&id))
                .unwrap()
                .rooms[0]
                .title,
            "Saved room 1"
        );
        group.bench_function(BenchmarkId::new("room", &size), |b| b.iter(&mut save_room));
        let save_metadata = || {
            fixture
                .runtime
                .block_on(fixture.mapper.rename_area(id, "Saved metadata"))
                .unwrap();
        };
        save_metadata();
        assert_eq!(
            fixture
                .runtime
                .block_on(fixture.backend.get_area(&id))
                .unwrap()
                .area
                .name,
            "Saved metadata"
        );
        group.bench_function(BenchmarkId::new("metadata", &size), |b| {
            b.iter(save_metadata);
        });
    }
    group.finish();
}

#[cfg(feature = "pr-benchmarks")]
pub fn local_merges(c: &mut Criterion) {
    use smudgy_cloud::backends::{AreaMergeSource, Translate};
    use std::time::{Duration, Instant};
    let mut group = c.benchmark_group("local_merge");
    group.sample_size(10);
    group.sampling_mode(SamplingMode::Flat);
    for (name, partial, boundary) in [
        ("two_equal_areas", false, false),
        ("partial_boundary", true, false),
        ("reciprocal_boundary", false, true),
    ] {
        for total in [1_000, 10_000, 50_000] {
            group.bench_function(BenchmarkId::new(name, total), |b| {
                b.iter_custom(|iterations| {
                    let mut elapsed = Duration::ZERO;
                    for _ in 0..iterations {
                        let mut documents =
                            vec![synthetic_area(0, total / 2), synthetic_area(1, total / 2)];
                        if boundary {
                            add_boundary_links(&mut documents);
                        }
                        let expected_connections: usize = documents
                            .iter()
                            .map(|area| area.connections.len())
                            .sum::<usize>()
                            - if boundary { total / 2 } else { 0 };
                        let fixture = Fixture::from_documents(documents);
                        let into = fixture.ids[0];
                        let source = fixture.ids[1];
                        let merge_source = AreaMergeSource {
                            id: source,
                            rooms: partial.then(|| {
                                (1..=total / 2)
                                    .step_by(2)
                                    .map(|number| RoomNumber(i32::try_from(number).unwrap()))
                                    .collect()
                            }),
                            translate: Translate::default(),
                        };
                        let start = Instant::now();
                        fixture
                            .runtime
                            .block_on(fixture.mapper.merge_areas(into, vec![merge_source]))
                            .unwrap();
                        elapsed += start.elapsed();
                        let atlas = fixture.mapper.get_current_atlas();
                        assert_eq!(atlas.get_area(&source).is_some(), partial);
                        let moved = if partial {
                            (total / 2).div_ceil(2)
                        } else {
                            total / 2
                        };
                        assert_eq!(
                            atlas.get_area(&into).unwrap().room_count(),
                            total / 2 + moved
                        );
                        if partial {
                            assert_eq!(
                                atlas.get_area(&source).unwrap().room_count(),
                                total / 2 - moved
                            );
                        }
                        if boundary {
                            let merged = fixture
                                .runtime
                                .block_on(fixture.backend.get_area(&into))
                                .unwrap();
                            assert_eq!(merged.connections.len(), expected_connections);
                        }
                        assert_eq!(
                            fixture
                                .runtime
                                .block_on(fixture.backend.get_area(&into))
                                .unwrap()
                                .rooms
                                .len(),
                            total / 2 + moved
                        );
                    }
                    elapsed
                });
            });
        }
    }
    group.finish();
}

/// One reciprocal cross-area link per room, in addition to each area's grid.
/// This makes pairing cost grow with the input, independently of room remapping.
#[cfg(feature = "pr-benchmarks")]
fn add_boundary_links(documents: &mut [smudgy_cloud::AreaWithDetails]) {
    use smudgy_cloud::{
        ConnectionId, ConnectionKind, ConnectionRouting, ExitDirection, ExitId, RoomSide, Uuid,
    };
    let ids: Vec<_> = documents.iter().map(|area| area.area.id).collect();
    for (index, document) in documents.iter_mut().enumerate() {
        let template = document.connections[0].clone();
        for room in &mut document.rooms {
            let id = (0xFEEE_u128 << 112)
                | (u128::try_from(index).unwrap() << 64)
                | u128::try_from(room.room_number.0).unwrap();
            let mut connection = template.clone();
            connection.id = ConnectionId(Uuid::from_u128(id));
            connection.endpoint_a.room_number = room.room_number;
            connection.endpoint_a.side = if index == 0 {
                RoomSide::East
            } else {
                RoomSide::West
            };
            connection.endpoint_b = None;
            connection.kind = ConnectionKind::External;
            connection.routing = ConnectionRouting::Stub;
            connection.route_points.clear();
            let mut exit = room.exits[0].clone();
            exit.id = ExitId(Uuid::from_u128(id));
            exit.connection_id = connection.id;
            exit.from_direction = if index == 0 {
                ExitDirection::East
            } else {
                ExitDirection::West
            };
            exit.to_direction = Some(if index == 0 {
                ExitDirection::West
            } else {
                ExitDirection::East
            });
            exit.to_area_id = Some(ids[1 - index]);
            exit.to_room_number = Some(room.room_number);
            exit.to_unknown = false;
            room.exits.push(exit);
            document.connections.push(connection);
        }
    }
}

#[cfg(not(feature = "pr-benchmarks"))]
pub fn local_merges(_: &mut Criterion) {}
