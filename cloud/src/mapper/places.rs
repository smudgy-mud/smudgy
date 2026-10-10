//! Room lookups across a map's **places**: the map's own content, each of
//! its Secrets the caller reads, and the caller's Private additions.
//!
//! Every place keeps its own indexes. The map's are its [`AreaCache`]'s (and,
//! across the atlas, the [`AtlasCache`]'s tables); a Secret's or Private
//! additions' are their layer's [`SourceLayer::content`], where attachments
//! carry source-owned content at qualified room addresses, mapped to the legacy API
//! through [`SourceLayer::room_key`]. Map-owned retained data has its own
//! document index. Lookups probe indexes without walking rooms. The
//! results name each room where it lives: a map room matched by a Secret's
//! data is the map's room, and a Secret's own room is the Secret's.

use std::collections::HashSet;
use std::sync::Arc;

use super::{
    RoomKey,
    area_cache::{AreaCache, SourceLayer},
    atlas_cache::AtlasCache,
    room_cache::RoomCache,
};
use crate::{AreaId, SourceId};

/// Which of a map's places a lookup reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Places {
    /// Every place: the map's own content, each Secret and Private additions.
    All,
    /// One place.
    Only(SourceId),
}

impl Places {
    /// Whether the lookup reads `place`.
    #[must_use]
    pub fn includes(self, place: SourceId) -> bool {
        match self {
            Self::All => true,
            Self::Only(only) => only == place,
        }
    }
}

/// Whether a caller sees a map's Secrets and Private additions at all. One
/// who does not sees maps as they would be without them: no hidden doors in
/// routes, and no Secret's or Private additions' rooms anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sources {
    Shown,
    Hidden,
}

/// What a room lookup asks of each place's indexes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoomQuery<'a> {
    /// Rooms carrying a tag, matched case-insensitively.
    Tag(&'a str),
    /// Rooms carrying a property, whatever its value.
    PropertyName(&'a str),
    /// Rooms whose property holds exactly a value.
    Property(&'a str, &'a str),
}

/// One index probe, against one area's lookups or the atlas-wide tables.
/// Lookups go through this seam so a test can count the probes a lookup
/// makes.
trait Probe {
    fn area<'c>(&self, area: &'c AreaCache) -> &'c [Arc<RoomCache>];
    fn atlas(&self, atlas: &AtlasCache, emit: &mut dyn FnMut(AreaId, &RoomCache));
}

impl Probe for RoomQuery<'_> {
    fn area<'c>(&self, area: &'c AreaCache) -> &'c [Arc<RoomCache>] {
        match *self {
            Self::Tag(tag) => area.get_rooms_with_tag(tag),
            Self::PropertyName(name) => area.get_rooms_with_property(name),
            Self::Property(name, value) => area.get_rooms_by_property(name, value),
        }
    }

    fn atlas(&self, atlas: &AtlasCache, emit: &mut dyn FnMut(AreaId, &RoomCache)) {
        match *self {
            Self::Tag(tag) => {
                for (area_id, room) in atlas.get_rooms_with_tag(tag) {
                    emit(area_id, &room);
                }
            }
            Self::PropertyName(name) => {
                for (area_id, room) in atlas.get_rooms_with_property(name) {
                    emit(area_id, &room);
                }
            }
            Self::Property(name, value) => {
                for (area_id, room) in atlas.get_rooms_by_property(name, value) {
                    emit(area_id, &room);
                }
            }
        }
    }
}

/// Collects room keys once each, in the order they are first found.
#[derive(Default)]
struct Found {
    keys: Vec<RoomKey>,
    seen: HashSet<RoomKey>,
}

impl Found {
    fn add(&mut self, key: RoomKey) {
        if self.seen.insert(key.clone()) {
            self.keys.push(key);
        }
    }
}

/// The rooms of `layer`, a source of map `map`, that `probe` matches, each
/// named where its anchor lives, independently of the source owning its content.
fn layer_rooms(_map: AreaId, layer: &SourceLayer, probe: &impl Probe, found: &mut Found) {
    for room in probe.area(layer.content()) {
        found.add(layer.room_key(room.address()));
    }
}

impl AreaCache {
    /// This area's rooms matching `query` in `places`. On a map, every place
    /// of the map takes part: its own content and each source layer. A
    /// Secret's or Private additions' own area holds one place, its own. One
    /// probe per place.
    #[must_use]
    pub fn find_rooms_in(&self, query: RoomQuery<'_>, places: Places) -> Vec<RoomKey> {
        let mut found = Found::default();
        self.collect_rooms_in(&query, places, &mut found);
        found.keys
    }

    fn collect_rooms_in(&self, probe: &impl Probe, places: Places, found: &mut Found) {
        if places.includes(self.place()) {
            if let Some(layer) = self.map_document_layer() {
                layer_rooms(*self.get_id(), layer, probe, found);
            } else {
                for room in probe.area(self) {
                    found.add(RoomKey::new(*self.get_id(), room.get_room_number()));
                }
            }
        }
        for layer in self.source_layers() {
            if places.includes(layer.source()) {
                layer_rooms(*self.get_id(), layer, probe, found);
            }
        }
    }
}

impl AtlasCache {
    /// Every room matching `query` in `places`, across the atlas: the map's
    /// own content from the atlas-wide tables, and each source from its own
    /// layer. Rooms of maps turned off or out of this server's scope are left
    /// out, as the atlas-wide tables leave them out. One probe per place.
    #[must_use]
    pub fn find_rooms_in(&self, query: RoomQuery<'_>, places: Places) -> Vec<RoomKey> {
        let mut found = Found::default();
        self.collect_rooms_in(&query, places, &mut found);
        found.keys
    }

    fn collect_rooms_in(&self, probe: &impl Probe, places: Places, found: &mut Found) {
        match places {
            // The tables hold the sources' own rooms beside the maps' rooms,
            // under the sources' own area ids.
            Places::All => probe.atlas(self, &mut |area_id, room| {
                found.add(RoomKey::new(area_id, room.get_room_number()));
            }),
            Places::Only(SourceId::Map) => probe.atlas(self, &mut |area_id, room| {
                if self.source_of(&area_id).is_none() {
                    found.add(RoomKey::new(area_id, room.get_room_number()));
                }
            }),
            Places::Only(_) => {}
        }
        for (map, source, area_id) in self.source_places() {
            if !places.includes(source) || !self.is_area_included(&map) {
                continue;
            }
            if let Some(layer) = self
                .get_area(&map)
                .as_deref()
                .and_then(|area| area.source_layer(&area_id))
            {
                layer_rooms(map, layer, probe, found);
            }
        }
        if places.includes(SourceId::Map) {
            for area in self.areas() {
                if self.is_area_included(area.get_id())
                    && let Some(layer) = area.map_document_layer()
                {
                    layer_rooms(*area.get_id(), layer, probe, found);
                }
            }
        }
    }

    /// Every room carrying `tag` in `places`, plus the matching rooms of the
    /// map `from` lies in: a walk from `from` enters that map even when it is
    /// turned off, so its rooms are candidates too.
    fn rooms_with_tag_from(&self, from: &RoomKey, tag: &str, places: Places) -> HashSet<RoomKey> {
        let probe = RoomQuery::Tag(tag);
        let mut found = Found::default();
        self.collect_rooms_in(&probe, places, &mut found);
        if let Some(map) = self.map_of(&from.area_id)
            && !self.is_area_included(&map)
            && let Some(area) = self.get_area(&map)
        {
            area.collect_rooms_in(&probe, places, &mut found);
        }
        found.seen
    }

    /// Whether `key`'s room belongs to `places`: every room for all of them;
    /// for one, the map's rooms (a source keeps data on them) and that
    /// place's own rooms.
    fn admits(&self, places: Places, key: &RoomKey) -> bool {
        match (places, self.source_of(&key.area_id)) {
            (Places::All, _) | (Places::Only(_), None) => true,
            (Places::Only(only), Some((map, source))) => {
                only == source
                    || self.get_area(&map).is_some_and(|area| {
                        area.document_layer(only).is_some_and(|layer| {
                            layer
                                .attachment(crate::RoomAddress::new(source, key.room_number))
                                .is_some()
                        })
                    })
            }
        }
    }

    /// The nearest reachable room whose tags in `places` carry every tag in
    /// `required` and none in `excluded` (case-insensitive), by the same walk
    /// as [`Self::get_path_between_rooms`], which takes every hidden door the
    /// atlas holds. A room's tags in [`Places::All`] are the union of every
    /// place's tags for it, so one tag can come from the map and another from
    /// a Secret. The candidates come from one indexed lookup per place and
    /// tag; the walk only tests membership. An empty filter matches nothing.
    /// A caller who does not see `sources` names [`Places::Only`] the map, and
    /// walks as on maps without Secrets or Private additions.
    #[must_use]
    pub fn find_nearest_room_matching_tags_in(
        &self,
        from_room_key: &RoomKey,
        required: &[String],
        excluded: &[String],
        places: Places,
        sources: Sources,
    ) -> Option<RoomKey> {
        let normalize = |tags: &[String]| -> Vec<String> {
            tags.iter()
                .map(|tag| crate::mapper::normalize_tag(tag))
                .filter(|tag| !tag.is_empty())
                .collect()
        };
        let required = normalize(required);
        let excluded = normalize(excluded);
        if required.is_empty() && excluded.is_empty() {
            return None;
        }
        let candidates = required
            .iter()
            .map(|tag| self.rooms_with_tag_from(from_room_key, tag, places))
            .reduce(|kept, next| kept.intersection(&next).cloned().collect());
        if candidates.as_ref().is_some_and(HashSet::is_empty) {
            return None;
        }
        let refused: HashSet<RoomKey> = excluded
            .iter()
            .flat_map(|tag| self.rooms_with_tag_from(from_room_key, tag, places))
            .collect();
        self.find_nearest_room_where(from_room_key, sources, |key| {
            candidates.as_ref().is_none_or(|kept| kept.contains(key))
                && !refused.contains(key)
                && self.admits(places, key)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::{HashMap, HashSet};

    use chrono::Utc;
    use uuid::Uuid;

    use super::*;
    use crate::{Area, AreaAccess, AreaWithDetails, RoomNumber, SourceBundle};

    const MAP: Uuid = Uuid::from_u128(0x9100);
    const BOOKCASE: Uuid = Uuid::from_u128(0x9101);
    const LEDGER: Uuid = Uuid::from_u128(0x9102);

    fn room(number: i32, tags: &[&str], notes: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "room_number": number, "title": format!("Room {number}"), "description": "",
            "color": "", "level": 0, "x": f64::from(number), "y": 0.0, "exits": [],
            "tags": tags,
            "properties": notes.map_or_else(Vec::new, |notes| vec![serde_json::json!({ "name": "notes", "value": notes })]),
        })
    }

    fn exit(id: u128, to_room: i32, to_source: Option<Uuid>) -> serde_json::Value {
        serde_json::json!({
            "id": Uuid::from_u128(id), "from_direction": "East",
            "to_area_id": MAP, "to_room_number": to_room, "to_source": to_source,
            "to_direction": null, "to_unknown": false, "path": "", "command": "",
            "weight": 1.0, "connection_id": Uuid::from_u128(id + 0x100),
            "is_hidden": true, "door": null
        })
    }

    /// A map of `rooms` rooms (1..=rooms, a chain of plain rooms with no
    /// exits between them) whose Bookcase keeps a door on map room 1 into its
    /// own room 2, which leads on to map room 3, tags map room 3 `LOOT` and
    /// its own room `VAULT`; whose Ledger keeps `notes` on map room 1; and
    /// whose Private additions tag map room 1 `MINE`.
    fn library(rooms: i32) -> AreaWithDetails {
        let mut vault = room(2, &["VAULT"], None);
        vault["exits"] = serde_json::json!([exit(2, 3, None)]);
        let bookcase: SourceBundle = serde_json::from_value(serde_json::json!({
            "source": BOOKCASE, "name": "Bookcase", "rev": 1, "actions": ["read"],
            "rooms": [vault],
            "room_data": [
                { "room_number": 1, "exits": [exit(1, 2, Some(BOOKCASE))] },
                { "room_number": 3, "tags": ["LOOT"] },
            ],
        }))
        .expect("the Bookcase parses");
        let ledger: SourceBundle = serde_json::from_value(serde_json::json!({
            "source": LEDGER, "name": "Ledger", "rev": 1, "actions": ["read"],
            "room_data": [{
                "room_number": 1,
                "properties": [{ "name": "notes", "value": "Owed 3 gold" }],
            }],
        }))
        .expect("the Ledger parses");
        let private: SourceBundle = serde_json::from_value(serde_json::json!({
            "source": "private", "rev": 1, "actions": ["read", "add", "edit", "remove"],
            "room_data": [{ "room_number": 1, "tags": ["MINE"] }],
        }))
        .expect("the Private additions parse");
        let map_rooms = (1..=rooms)
            .filter(|number| *number != 2)
            .map(|number| match number {
                1 => room(1, &["START"], Some("Ordinary")),
                3 => room(3, &[], None),
                n => room(n, &["FILLER"], Some("filler")),
            })
            .collect::<Vec<_>>();
        AreaWithDetails {
            room_data: Vec::new(),
            area: Area {
                id: AreaId(MAP),
                user_id: None,
                atlas_id: None,
                name: "Library".to_string(),
                created_at: Utc::now(),
                rev: 1,
                access: Some(AreaAccess::OWNER),
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                clan_id: None,
                clan_name: None,
                actions: None,
                clan_ownership: crate::clan_maps::ClanOwnership::default(),
                atlas_name: None,
                projection_token: Some("p_places".to_string()),
            },
            format_version: crate::AREA_FORMAT_VERSION,
            properties: Vec::new(),
            rooms: serde_json::from_value(serde_json::Value::Array(map_rooms))
                .expect("the map's rooms parse"),
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
            linked_areas: Vec::new(),
            sources: vec![bookcase, ledger, private],
        }
    }

    fn atlas_of(rooms: i32, disabled: &[AreaId]) -> AtlasCache {
        let map = Arc::new(AreaCache::new_with_area(library(rooms)));
        AtlasCache::new_with_areas(
            HashMap::from([(AreaId(MAP), map)]),
            Arc::new(disabled.iter().copied().collect()),
        )
    }

    fn key(area: Uuid, number: i32) -> RoomKey {
        RoomKey::new(AreaId(area), RoomNumber(number))
    }

    fn sorted(mut keys: Vec<RoomKey>) -> Vec<RoomKey> {
        keys.sort_by_key(|key| (key.area_id.0, key.room_number.0));
        keys
    }

    #[test]
    fn every_place_answers_a_lookup_and_rooms_are_named_where_they_live() {
        let atlas = atlas_of(3, &[]);
        let map = atlas.get_area(&AreaId(MAP)).expect("the map");
        for (query, places, expected) in [
            (RoomQuery::Tag("loot"), Places::All, vec![key(MAP, 3)]),
            (RoomQuery::Tag("vault"), Places::All, vec![key(BOOKCASE, 2)]),
            (RoomQuery::Tag("mine"), Places::All, vec![key(MAP, 1)]),
            (
                RoomQuery::Tag("loot"),
                Places::Only(SourceId::Map),
                Vec::new(),
            ),
            (
                RoomQuery::Tag("start"),
                Places::Only(SourceId::Map),
                vec![key(MAP, 1)],
            ),
            (
                RoomQuery::Tag("vault"),
                Places::Only(SourceId::Secret(BOOKCASE)),
                vec![key(BOOKCASE, 2)],
            ),
            (
                RoomQuery::Tag("mine"),
                Places::Only(SourceId::Secret(BOOKCASE)),
                Vec::new(),
            ),
            (
                RoomQuery::PropertyName("notes"),
                Places::All,
                vec![key(MAP, 1)],
            ),
            (
                RoomQuery::Property("notes", "Owed 3 gold"),
                Places::All,
                vec![key(MAP, 1)],
            ),
            (
                RoomQuery::Property("notes", "Owed 3 gold"),
                Places::Only(SourceId::Map),
                Vec::new(),
            ),
            (
                RoomQuery::Property("notes", "Owed 3 gold"),
                Places::Only(SourceId::Secret(LEDGER)),
                vec![key(MAP, 1)],
            ),
        ] {
            assert_eq!(
                sorted(atlas.find_rooms_in(query, places)),
                expected,
                "atlas {query:?} in {places:?}"
            );
            assert_eq!(
                sorted(map.find_rooms_in(query, places)),
                expected,
                "map {query:?} in {places:?}"
            );
        }
        let vault = atlas.get_area(&AreaId(BOOKCASE)).expect("the Bookcase");
        assert_eq!(
            vault.find_rooms_in(RoomQuery::Tag("vault"), Places::All),
            vec![key(BOOKCASE, 2)],
            "a Secret's own area answers for its one place"
        );
        assert!(
            vault
                .find_rooms_in(RoomQuery::Tag("vault"), Places::Only(SourceId::Map))
                .is_empty()
        );
    }

    #[test]
    fn a_turned_off_map_drops_out_of_atlas_lookups_but_not_its_own() {
        let atlas = atlas_of(3, &[AreaId(MAP)]);
        assert!(
            atlas
                .find_rooms_in(RoomQuery::Tag("loot"), Places::All)
                .is_empty()
        );
        let map = atlas.get_area(&AreaId(MAP)).expect("the map");
        assert_eq!(
            map.find_rooms_in(RoomQuery::Tag("loot"), Places::All),
            vec![key(MAP, 3)]
        );
    }

    #[test]
    fn the_nearest_search_finds_a_secrets_tags_through_its_hidden_door() {
        let atlas = atlas_of(3, &[]);
        let from = key(MAP, 1);
        let tags = |tags: &[&str]| tags.iter().map(ToString::to_string).collect::<Vec<_>>();
        assert_eq!(
            atlas.find_nearest_room_matching_tags_in(
                &from,
                &tags(&["vault"]),
                &[],
                Places::All,
                Sources::Shown
            ),
            Some(key(BOOKCASE, 2)),
            "a Secret's own room, behind its door"
        );
        assert_eq!(
            atlas.find_nearest_room_matching_tags_in(
                &from,
                &tags(&["loot"]),
                &[],
                Places::All,
                Sources::Shown
            ),
            Some(key(MAP, 3)),
            "a map room a Secret tags, reached only through the Secret"
        );
        assert_eq!(
            atlas.find_nearest_room_matching_tags_in(
                &from,
                &tags(&["start", "mine"]),
                &[],
                Places::All,
                Sources::Shown
            ),
            Some(key(MAP, 1)),
            "one tag from the map and one from Private additions"
        );
        assert_eq!(
            atlas.find_nearest_room_matching_tags_in(
                &from,
                &tags(&["loot"]),
                &[],
                Places::Only(SourceId::Map),
                Sources::Shown
            ),
            None,
            "the map alone keeps no LOOT"
        );
        assert_eq!(
            atlas.find_nearest_room_matching_tags_in(
                &from,
                &[],
                &tags(&["start", "mine"]),
                Places::Only(SourceId::Map),
                Sources::Shown
            ),
            Some(key(MAP, 3)),
            "an exclusion-only search narrowed to the map passes over the Secret's own room"
        );
        assert_eq!(
            atlas.find_nearest_room_matching_tags_in(&from, &[], &[], Places::All, Sources::Shown),
            None
        );
        let off = atlas_of(3, &[AreaId(MAP)]);
        assert_eq!(
            off.find_nearest_room_matching_tags_in(
                &from,
                &tags(&["loot"]),
                &[],
                Places::All,
                Sources::Shown
            ),
            Some(key(MAP, 3)),
            "the start's map is walked even when turned off"
        );
    }

    /// The library with a long way round from map room 1 to map room 3,
    /// through map rooms 4 and 5, beside the Bookcase's shortcut through its hidden
    /// door; with its Secrets and Private additions or without them. Map room
    /// 4 and the Bookcase's own room are bound to external ids.
    fn long_way(with_sources: bool) -> AtlasCache {
        let mut details = library(5);
        for room in &mut details.rooms {
            let to = match room.room_number.0 {
                1 => 4,
                4 => {
                    room.external_id = Some("m-4".to_string());
                    5
                }
                5 => 3,
                _ => continue,
            };
            room.exits.push(
                serde_json::from_value(exit(0x40 + u128::try_from(to).unwrap(), to, None))
                    .expect("the map's exit parses"),
            );
        }
        details.sources[0].rooms[0].external_id = Some("v-2".to_string());
        if !with_sources {
            details.sources.clear();
        }
        AtlasCache::new_with_areas(
            HashMap::from([(AreaId(MAP), Arc::new(AreaCache::new_with_area(details)))]),
            Arc::new(HashSet::new()),
        )
    }

    /// Without the sources, every route and search runs as on the same map
    /// without them, and a Secret's room or area answers as an unknown one.
    #[test]
    fn hidden_sources_route_and_search_as_a_map_without_them() {
        let secret = long_way(true);
        let plain = long_way(false);
        let (from, to) = (key(MAP, 1), key(MAP, 3));
        assert_eq!(
            secret.get_path_between_rooms(&from, &to),
            Some(vec![key(MAP, 1), key(BOOKCASE, 2), key(MAP, 3)]),
            "a reader takes the shortcut"
        );
        assert_eq!(
            plain.get_path_between_rooms(&from, &to),
            Some(vec![key(MAP, 1), key(MAP, 4), key(MAP, 5), key(MAP, 3)])
        );
        let vault = key(BOOKCASE, 2);
        for (a, b) in [
            (&from, &to),
            (&from, &vault),
            (&vault, &to),
            (&vault, &vault),
        ] {
            assert_eq!(
                secret.get_path_between_rooms_with(a, b, Sources::Hidden),
                plain.get_path_between_rooms(a, b),
                "route {a:?} to {b:?}"
            );
        }
        for target in [AreaId(MAP), AreaId(BOOKCASE)] {
            assert_eq!(
                secret.find_nearest_room_in_area_with(&from, &target, Sources::Hidden),
                plain.find_nearest_room_in_area(&from, &target),
            );
        }
        let tags = |tags: &[&str]| tags.iter().map(ToString::to_string).collect::<Vec<_>>();
        for (required, excluded) in [
            (tags(&["filler"]), Vec::new()),
            (tags(&["vault"]), Vec::new()),
            (tags(&["loot"]), Vec::new()),
            (Vec::new(), tags(&["start"])),
        ] {
            assert_eq!(
                secret.find_nearest_room_matching_tags_in(
                    &from,
                    &required,
                    &excluded,
                    Places::Only(SourceId::Map),
                    Sources::Hidden
                ),
                plain.find_nearest_room_matching_tags_in(
                    &from,
                    &required,
                    &excluded,
                    Places::All,
                    Sources::Shown
                ),
                "nearest with {required:?}, without {excluded:?}"
            );
        }
        for external_id in ["m-4", "v-2"] {
            let found = |atlas: &AtlasCache, sources| {
                atlas
                    .find_room_by_external_id_with(external_id, sources)
                    .map(|(key, _)| key)
            };
            assert_eq!(
                found(&secret, Sources::Hidden),
                found(&plain, Sources::Shown)
            );
        }
        assert_eq!(
            secret.find_room_by_external_id("v-2").map(|(key, _)| key),
            Some(vault)
        );
        assert!(!secret.sees(Sources::Hidden, &AreaId(BOOKCASE)));
        assert!(secret.sees(Sources::Hidden, &AreaId(MAP)));
    }

    /// Counts every probe a lookup makes, whatever it asks.
    struct Counting<'a> {
        query: RoomQuery<'a>,
        probes: Cell<usize>,
    }

    impl Probe for Counting<'_> {
        fn area<'c>(&self, area: &'c AreaCache) -> &'c [Arc<RoomCache>] {
            self.probes.set(self.probes.get() + 1);
            self.query.area(area)
        }

        fn atlas(&self, atlas: &AtlasCache, emit: &mut dyn FnMut(AreaId, &RoomCache)) {
            self.probes.set(self.probes.get() + 1);
            self.query.atlas(atlas, emit);
        }
    }

    /// A lookup costs one probe per place (the map, two Secrets and Private
    /// additions), whether the map has ten rooms or twenty thousand.
    #[test]
    fn a_lookup_costs_one_probe_per_place_however_many_rooms() {
        for rooms in [10, 20_000] {
            let atlas = atlas_of(rooms, &[]);
            let map = atlas.get_area(&AreaId(MAP)).expect("the map");
            for places in [Places::All, Places::Only(SourceId::Secret(LEDGER))] {
                let counting = Counting {
                    query: RoomQuery::Tag("filler"),
                    probes: Cell::new(0),
                };
                let mut found = Found::default();
                atlas.collect_rooms_in(&counting, places, &mut found);
                let per_place = if places == Places::All { 4 } else { 1 };
                assert_eq!(counting.probes.get(), per_place, "atlas, {rooms} rooms");
                counting.probes.set(0);
                let mut found = Found::default();
                map.collect_rooms_in(&counting, places, &mut found);
                assert_eq!(counting.probes.get(), per_place, "map, {rooms} rooms");
            }
        }
    }
}
