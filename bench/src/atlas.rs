//! Deterministic synthetic-atlas generator for mapper-scale benches.
//!
//! [`synthetic_area`] builds an [`AreaWithDetails`] shaped like a real mapped
//! MUD zone: rooms on a square grid with reciprocal 4-way exits, a small
//! basement layer reached by up/down cross-level exits (~2% of rooms), titles
//! drawn from a small shared pool (so the identification indices see realistic
//! title collisions), mostly-unique descriptions, and sparse tags (~1% of
//! rooms — the nearest-tag speedwalk workload). [`synthetic_atlas_cache`]
//! loads a fleet of those areas into a real `AtlasCache` through the public
//! `LocalBackend` → `Mapper` path.
//!
//! Everything is seeded per-area with fixed constants (same inline splitmix64
//! as `wire.rs`), so repeated calls produce structurally identical atlases and
//! criterion numbers stay comparable across runs.

use std::collections::BTreeSet;
use std::sync::Arc;

use smudgy_cloud::{
    Area, AreaAccess, AreaId, AreaWithDetails, Connection, ConnectionDash, ConnectionEndpoint,
    ConnectionId, ConnectionKind, ConnectionRouting, CornerStyle, Exit, ExitDirection, ExitId,
    LocalBackend, Mapper, MapperBackend, PortMode, RoomNumber, RoomWithDetails, SegmentShape, Uuid,
    connection::default_anchor_for_direction, mapper::AtlasCache,
};

use crate::SplitMix64;

/// Room titles are drawn from this small pool, so distinct rooms *share*
/// titles — the shape that makes the by-title identification index carry
/// multi-room buckets, as real maps do.
const TITLE_POOL: &[&str] = &[
    "A Dusty Trail",
    "The Market Square",
    "A Narrow Alley",
    "The Temple Courtyard",
    "A Quiet Grove",
    "The City Gates",
    "A Winding Path",
    "The Docks",
    "A Torchlit Corridor",
    "The Guild Hall",
    "A Muddy Crossroads",
    "The Old Bridge",
];

/// Tags sprinkled onto ~1 room in [`TAG_ODDS`].
const TAG_POOL: &[&str] = &["INN", "PEACE", "SHOP", "GUILD"];

/// One room in this many carries a tag.
const TAG_ODDS: usize = 100;
/// One room in this many gets the shared generic description instead of a
/// unique one, so the by-description index is *mostly* discriminating.
const GENERIC_DESCRIPTION_ODDS: usize = 10;
/// One room in this many is a basement room hanging under the surface grid —
/// the source of the up/down cross-level exits.
const BASEMENT_DIVISOR: usize = 50;

/// Stable, collision-free id for a synthetic area.
fn synthetic_area_id(area_index: u32) -> AreaId {
    AreaId(Uuid::from_u128(
        (0xBE9C_u128 << 112) | u128::from(area_index),
    ))
}

/// Stable exit ids: the (1-based) area index in the high 64 bits keeps ids
/// unique across areas; a per-area counter fills the low bits.
struct ExitIds {
    area_index: u32,
    next: u64,
}

impl ExitIds {
    fn next(&mut self) -> ExitId {
        let id = (u128::from(self.area_index) + 1) << 64 | u128::from(self.next);
        self.next += 1;
        ExitId(Uuid::from_u128(id))
    }

    /// Stable Connection ids, tagged so they can never collide with exit ids.
    fn next_connection(&mut self) -> ConnectionId {
        let id =
            (0xC0DE_u128 << 96) | (u128::from(self.area_index) + 1) << 64 | u128::from(self.next);
        self.next += 1;
        ConnectionId(Uuid::from_u128(id))
    }
}

/// Smallest grid side whose square holds `rooms` rooms.
fn grid_width(rooms: usize) -> usize {
    let mut width = 1;
    while width * width < rooms {
        width += 1;
    }
    width
}

/// 1-based room number for a 0-based generation index.
fn number(index: usize) -> RoomNumber {
    RoomNumber(i32::try_from(index + 1).expect("room count fits in i32"))
}

/// Grid coordinates as map positions. The grid side even for a million-room
/// area is ~1000, comfortably inside `f32`'s exact-integer range.
#[allow(clippy::cast_precision_loss)]
fn coord(v: usize) -> f32 {
    v as f32
}

/// A plain in-area exit `from` the given direction to `to_room` as a member
/// of `connection`, with the reciprocal arrival direction filled in (the
/// generator always emits the matching reverse exit on the destination
/// room).
fn exit(
    id: ExitId,
    area_id: AreaId,
    from: ExitDirection,
    to_room: RoomNumber,
    connection: ConnectionId,
) -> Exit {
    Exit {
        id,
        from_direction: from,
        to_area_id: Some(area_id),
        to_room_number: Some(to_room),
        to_direction: Some(from.opposite()),
        path: String::new(),
        is_hidden: false,
        is_closed: false,
        is_locked: false,
        weight: 1.0,
        command: String::new(),
        connection_id: connection,
        to_unknown: false,
        to_area_token: None,
        is_secret: false,
    }
}

/// The shared Connection row for one reciprocal pair `low ↔ high` whose
/// low-room exit leaves via `low_direction`: canonical lower-room-first
/// endpoint order with the direction-default wall anchors.
fn pair_connection(
    id: ConnectionId,
    low: RoomNumber,
    high: RoomNumber,
    low_direction: ExitDirection,
) -> Connection {
    let endpoint = |room: RoomNumber, direction: ExitDirection| {
        let (side, port_offset) = default_anchor_for_direction(direction, None);
        ConnectionEndpoint {
            room_number: room,
            side,
            port_offset,
            port_mode: PortMode::AutoPinned,
        }
    };
    let kind = if matches!(low_direction, ExitDirection::Up | ExitDirection::Down) {
        ConnectionKind::CrossLevel
    } else {
        ConnectionKind::Internal
    };
    Connection {
        id,
        endpoint_a: endpoint(low, low_direction),
        endpoint_b: Some(endpoint(high, low_direction.opposite())),
        kind,
        routing: ConnectionRouting::Simple,
        segment_shape: SegmentShape::Direct,
        corner: CornerStyle::Sharp,
        route_points: Vec::new(),
        dash: ConnectionDash::Solid,
        color: smudgy_cloud::DEFAULT_CONNECTION_COLOR.to_string(),
        thickness: smudgy_cloud::DEFAULT_CONNECTION_THICKNESS,
    }
}

/// Builds every room's outgoing exits plus their shared Connection rows:
/// reciprocal 4-way links between grid neighbors on the surface, plus a
/// reciprocal down/up pair from each evenly spread surface anchor to its
/// basement room. Exits indexed by 0-based room index. One Connection per
/// reciprocal pair, exactly the stored-membership shape a real v2 area
/// projects.
fn build_exits(
    area_id: AreaId,
    ids: &mut ExitIds,
    surface: usize,
    basement: usize,
    width: usize,
) -> (Vec<Vec<Exit>>, Vec<Connection>) {
    let mut exits: Vec<Vec<Exit>> = vec![Vec::new(); surface + basement];
    let mut connections: Vec<Connection> = Vec::new();
    // Each reciprocal pair is keyed by its (low, high) room indices; the
    // low-side traversal mints the Connection, the high side reuses it.
    let mut pair_ids: std::collections::HashMap<(usize, usize), ConnectionId> =
        std::collections::HashMap::new();
    let mut link = |exits: &mut Vec<Vec<Exit>>,
                    connections: &mut Vec<Connection>,
                    ids: &mut ExitIds,
                    i: usize,
                    j: usize,
                    direction: ExitDirection| {
        let (low, high) = if i < j { (i, j) } else { (j, i) };
        let connection = *pair_ids.entry((low, high)).or_insert_with(|| {
            let id = ids.next_connection();
            let low_direction = if i == low {
                direction
            } else {
                direction.opposite()
            };
            connections.push(pair_connection(
                id,
                number(low),
                number(high),
                low_direction,
            ));
            id
        });
        exits[i].push(exit(ids.next(), area_id, direction, number(j), connection));
    };

    for i in 0..surface {
        let col = i % width;
        let row = i / width;
        let neighbors = [
            (ExitDirection::North, (row > 0).then(|| i - width)),
            (
                ExitDirection::East,
                (col + 1 < width && i + 1 < surface).then_some(i + 1),
            ),
            (
                ExitDirection::South,
                (i + width < surface).then_some(i + width),
            ),
            (ExitDirection::West, (col > 0).then(|| i - 1)),
        ];
        for (direction, neighbor) in neighbors {
            if let Some(j) = neighbor {
                link(&mut exits, &mut connections, ids, i, j, direction);
            }
        }
    }

    // `checked_div` collapses the no-basement case (divide by zero) and the
    // loop guard into one branch.
    if let Some(spread) = surface.checked_div(basement) {
        for b in 0..basement {
            let anchor = b * spread;
            let below = surface + b;
            link(
                &mut exits,
                &mut connections,
                ids,
                anchor,
                below,
                ExitDirection::Down,
            );
            link(
                &mut exits,
                &mut connections,
                ids,
                below,
                anchor,
                ExitDirection::Up,
            );
        }
    }

    (exits, connections)
}

/// Builds one synthetic area with exactly `rooms` rooms.
///
/// Layout: `rooms - rooms/50` surface rooms on a square grid at level 0 with
/// 4-way exits between neighbors; the remaining ~2% sit at level -1 under
/// evenly spread surface anchors, linked by reciprocal down/up exits. Titles
/// come from [`TITLE_POOL`], descriptions are unique for ~90% of rooms, and
/// ~1% of rooms carry a tag from [`TAG_POOL`]. Deterministic per
/// `(area_index, rooms)`.
#[must_use]
pub fn synthetic_area(area_index: u32, rooms: usize) -> AreaWithDetails {
    assert!(rooms > 0, "an area needs at least one room");
    let mut rng = SplitMix64::new(0xA71A_5000_0000_0000 ^ u64::from(area_index));
    let area_id = synthetic_area_id(area_index);

    let basement = rooms / BASEMENT_DIVISOR;
    let surface = rooms - basement;
    let width = grid_width(surface);
    let spread = surface.checked_div(basement).unwrap_or(0);

    let mut ids = ExitIds {
        area_index,
        next: 0,
    };
    let (mut exits, connections) = build_exits(area_id, &mut ids, surface, basement, width);

    let mut room_list = Vec::with_capacity(rooms);
    for (i, room_exits) in exits.iter_mut().enumerate() {
        let (x, y, level) = if i < surface {
            (coord(i % width), coord(i / width), 0)
        } else {
            // Basement rooms sit directly under their surface anchor.
            let anchor = (i - surface) * spread;
            (coord(anchor % width), coord(anchor / width), -1)
        };
        let title = TITLE_POOL[rng.below(TITLE_POOL.len())].to_string();
        let description = if rng.below(GENERIC_DESCRIPTION_ODDS) == 0 {
            "You see nothing special here.".to_string()
        } else {
            format!(
                "Synthetic room {n} of area {area_index}; the masonry is stamped {n}.",
                n = i + 1
            )
        };
        let mut tags = BTreeSet::new();
        if rng.below(TAG_ODDS) == 0 {
            tags.insert(TAG_POOL[rng.below(TAG_POOL.len())].to_string());
        }
        room_list.push(RoomWithDetails {
            room_number: number(i),
            title,
            description,
            level,
            x,
            y,
            color: String::new(),
            properties: Vec::new(),
            exits: std::mem::take(room_exits),
            tags,
            is_secret: false,
            external_id: None,
        });
    }

    // `DateTime<Utc>`'s Default is the Unix epoch — a deterministic timestamp
    // without taking a direct chrono dependency just to name the type.
    #[allow(clippy::default_trait_access)]
    let created_at = Default::default();

    AreaWithDetails {
        area: Area {
            id: area_id,
            user_id: None,
            atlas_id: None,
            name: format!("Synthetic Area {area_index}"),
            created_at,
            rev: 1,
            access: Some(AreaAccess::OWNER),
            owner_nickname: None,
            copied_from_area_id: None,
            copied_from_rev: None,
            copied_at: None,
            family_token: None,
            atlas_name: None,
        },
        format_version: smudgy_cloud::AREA_FORMAT_VERSION,
        content_hash: None,
        properties: Vec::new(),
        rooms: room_list,
        labels: Vec::new(),
        shapes: Vec::new(),
        connections,
        linked_areas: Vec::new(),
    }
}

/// Builds a real `AtlasCache` holding `total_rooms` rooms spread as evenly as
/// possible across `areas` synthetic areas.
///
/// `AtlasCache`'s constructors are crate-private, so the cache is produced
/// the way the app produces it: the areas are imported into a temp-dir
/// [`LocalBackend`] store and loaded through [`Mapper::load_all_areas`] —
/// the same public cache-build path a real session exercises. The temp dirs
/// and the tokio runtime live only for the duration of the call; the
/// returned cache is fully in-memory.
///
/// Spins up its own single-thread tokio runtime: call it from plain bench
/// setup code, never from inside an async context.
#[must_use]
pub fn synthetic_atlas_cache(total_rooms: usize, areas: usize) -> Arc<AtlasCache> {
    assert!(areas > 0, "need at least one area");
    assert!(total_rooms >= areas, "every area needs at least one room");

    let base = total_rooms / areas;
    let remainder = total_rooms % areas;
    let details: Vec<AreaWithDetails> = (0..areas)
        .map(|i| {
            let index = u32::try_from(i).expect("area count fits in u32");
            synthetic_area(index, base + usize::from(i < remainder))
        })
        .collect();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("bench tokio runtime");
    runtime.block_on(async move {
        let store_dir = tempfile::tempdir().expect("temp local map store");
        let cache_dir = tempfile::tempdir().expect("temp mapper cache dir");
        let backend = LocalBackend::new(store_dir.path());
        for area in details {
            backend
                .import_local_area(area)
                .await
                .expect("import synthetic area");
        }
        let mapper = Mapper::new(Arc::new(backend), cache_dir.path());
        let summary = mapper.load_all_areas().await.expect("load synthetic areas");
        assert_eq!(
            summary.areas.len(),
            areas,
            "every synthetic area must load into the cache"
        );
        mapper.get_current_atlas()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use smudgy_cloud::mapper::RoomKey;

    use super::*;

    #[test]
    fn room_count_is_exact_and_numbers_unique() {
        let area = synthetic_area(0, 1_000);
        assert_eq!(area.rooms.len(), 1_000);
        let numbers: HashSet<RoomNumber> = area.rooms.iter().map(|r| r.room_number).collect();
        assert_eq!(numbers.len(), 1_000);
    }

    #[test]
    fn exits_are_reciprocal_and_include_cross_level() {
        let area = synthetic_area(1, 1_000);
        let by_number: HashMap<RoomNumber, &RoomWithDetails> =
            area.rooms.iter().map(|r| (r.room_number, r)).collect();

        let mut directed = 0_usize;
        let mut vertical = 0_usize;
        let mut exit_ids = HashSet::new();
        for room in &area.rooms {
            for door in &room.exits {
                assert_eq!(door.to_area_id, Some(area.area.id), "exits stay in-area");
                assert!(exit_ids.insert(door.id), "exit ids must be unique");
                let dest = by_number[&door.to_room_number.expect("every exit has a destination")];
                let has_reciprocal = dest.exits.iter().any(|back| {
                    back.to_room_number == Some(room.room_number)
                        && back.from_direction == door.from_direction.opposite()
                });
                assert!(
                    has_reciprocal,
                    "missing reciprocal for {:?} going {:?}",
                    room.room_number, door.from_direction
                );
                directed += 1;
                if matches!(door.from_direction, ExitDirection::Up | ExitDirection::Down) {
                    vertical += 1;
                }
            }
        }
        // A ~32x31 surface grid carries thousands of directed grid links.
        assert!(directed > 3_000, "directed = {directed}");
        // rooms/50 basement rooms, one down+up pair each.
        assert_eq!(vertical, 2 * (1_000 / BASEMENT_DIVISOR));
    }

    #[test]
    fn tags_present_and_sparse() {
        let area = synthetic_area(2, 1_000);
        let tagged = area.rooms.iter().filter(|r| !r.tags.is_empty()).count();
        assert!(tagged >= 1, "at least one room must be tagged");
        assert!(tagged <= 50, "tags should stay ~1%, got {tagged}");
        assert!(
            area.rooms
                .iter()
                .flat_map(|r| r.tags.iter())
                .all(|t| TAG_POOL.contains(&t.as_str())),
            "tags come from the fixed pool"
        );
    }

    #[test]
    fn titles_shared_descriptions_mostly_unique() {
        let area = synthetic_area(3, 1_000);
        let titles: HashSet<&str> = area.rooms.iter().map(|r| r.title.as_str()).collect();
        assert!(titles.len() <= TITLE_POOL.len());
        assert!(titles.len() >= 2, "rooms must share a small pool of titles");
        let descriptions: HashSet<&str> =
            area.rooms.iter().map(|r| r.description.as_str()).collect();
        assert!(
            descriptions.len() > 800,
            "most descriptions unique, got {}",
            descriptions.len()
        );
        assert!(
            descriptions.len() < 1_000,
            "some rooms must share the generic description"
        );
    }

    #[test]
    fn generation_is_deterministic() {
        let first = synthetic_area(7, 500);
        let second = synthetic_area(7, 500);
        assert_eq!(first.area.id, second.area.id);
        assert_eq!(first.rooms.len(), second.rooms.len());
        for (a, b) in first.rooms.iter().zip(&second.rooms) {
            assert_eq!(a.room_number, b.room_number);
            assert_eq!(a.title, b.title);
            assert_eq!(a.description, b.description);
            assert_eq!(a.tags, b.tags);
            assert_eq!(a.exits.len(), b.exits.len());
            for (x, y) in a.exits.iter().zip(&b.exits) {
                assert_eq!(x.id, y.id);
                assert_eq!(x.from_direction, y.from_direction);
                assert_eq!(x.to_room_number, y.to_room_number);
            }
        }
    }

    #[test]
    fn atlas_cache_builds_at_scale() {
        let atlas = synthetic_atlas_cache(1_000, 4);
        assert_eq!(atlas.areas().len(), 4);
        let total: usize = atlas.areas().map(|area| area.room_count()).sum();
        assert_eq!(total, 1_000);

        // The identification indices really resolve rooms: every area's room 1
        // is addressable by key, and its title (drawn from the shared pool)
        // resolves through the by-title lookup.
        for area in atlas.areas() {
            let key = RoomKey::new(*area.get_id(), RoomNumber(1));
            let room = atlas.get_room(&key).expect("room 1 exists in every area");
            assert_ne!(atlas.get_rooms_by_title(room.get_title()).len(), 0);
        }
    }
}
