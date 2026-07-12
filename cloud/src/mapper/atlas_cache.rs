use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use ordered_float::OrderedFloat;

use crate::{
    AreaId, ExitDirection,
    mapper::{
        RoomKey,
        area_cache::AreaCache,
        room_cache::{ExitBitfield, RoomCache},
    },
};

/// Routing tie-bias: edge weights into rooms of areas the viewer does *not*
/// own are multiplied by this constant, so auto-routing prefers the viewer's
/// own zones when both contain a viable route (own-beats-shared precedence).
/// It is a soft preference — routes that only exist through shared maps
/// still resolve, they just never win a tie against an owned route.
const SHARED_AREA_WEIGHT_PENALTY: f32 = 4.0;

/// The ordered list of rooms a single lookup-table key resolves to, paired
/// with the area each room belongs to (owned areas sorted first).
type RoomMatches = Vec<(AreaId, Arc<RoomCache>)>;

static EMPTY_ROOMS_LOOKUP_VEC: RoomMatches = Vec::new();
#[derive(Clone)]
pub struct AtlasCache {
    areas: HashMap<AreaId, Arc<AreaCache>>,
    rooms_by_title_description_and_visible_exits:
        HashMap<(ExitBitfield, String), RoomMatches>,
    rooms_by_title_and_description: HashMap<String, RoomMatches>,
    rooms_by_title: HashMap<String, RoomMatches>,
    rooms_by_description: HashMap<String, RoomMatches>,
    rooms: HashMap<RoomKey, Arc<RoomCache>>,
    /// Areas the viewer owns, for own-beats-shared precedence in lookups and
    /// routing. Built once per cache rebuild; lookups are O(1).
    owned_areas: HashSet<AreaId>,
    /// Areas the user disabled: excluded from the room-identification lookup
    /// tables at build time and never routed *through* (still present in
    /// `areas` and `rooms` so explicit addressing keeps working). May contain
    /// ids not (yet) in `areas` — disabling survives the area landing later.
    /// `Arc` so the set rides through every rebuild for free.
    disabled_areas: Arc<HashSet<AreaId>>,
}

impl AtlasCache {
    pub(super) fn new_with_areas(
        areas: HashMap<AreaId, Arc<AreaCache>>,
        disabled_areas: Arc<HashSet<AreaId>>,
    ) -> Self {
        let owned_areas: HashSet<AreaId> = areas
            .iter()
            .filter(|(_, area)| area.is_owned())
            .map(|(area_id, _)| *area_id)
            .collect();

        let mut rooms_by_title_description_and_visible_exits =
            Self::build_rooms_by_title_description_and_visible_exits(&areas, &disabled_areas);
        let mut rooms_by_title_and_description =
            Self::build_rooms_by_title_and_description(&areas, &disabled_areas);
        let mut rooms_by_title = Self::build_rooms_by_title(&areas, &disabled_areas);
        let mut rooms_by_description = Self::build_rooms_by_description(&areas, &disabled_areas);
        let rooms = Self::build_rooms(&areas);

        // Own-beats-shared: rooms from owned areas sort before rooms from
        // shared areas in every lookup table, so session auto-location picks
        // the viewer's own room when both match.
        sort_owned_first(&mut rooms_by_title_description_and_visible_exits, &owned_areas);
        sort_owned_first(&mut rooms_by_title_and_description, &owned_areas);
        sort_owned_first(&mut rooms_by_title, &owned_areas);
        sort_owned_first(&mut rooms_by_description, &owned_areas);

        Self {
            areas,
            rooms_by_title_description_and_visible_exits,
            rooms_by_title_and_description,
            rooms_by_title,
            rooms_by_description,
            rooms,
            owned_areas,
            disabled_areas,
        }
    }

    fn build_rooms_by_title_description_and_visible_exits(
        areas: &HashMap<AreaId, Arc<AreaCache>>,
        disabled_areas: &HashSet<AreaId>,
    ) -> HashMap<(ExitBitfield, String), RoomMatches> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            if disabled_areas.contains(area_id) {
                continue;
            }
            for room in area.get_rooms() {
                let visible_exit_bitfield = room.get_visible_exit_bitfield();
                ret.entry((
                    visible_exit_bitfield,
                    room.get_title_and_description().to_string(),
                ))
                .or_insert(Vec::new())
                .push((*area_id, room.clone()));
            }
        }
        ret
    }

    fn build_rooms_by_title_and_description(
        areas: &HashMap<AreaId, Arc<AreaCache>>,
        disabled_areas: &HashSet<AreaId>,
    ) -> HashMap<String, RoomMatches> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            if disabled_areas.contains(area_id) {
                continue;
            }
            for room in area.get_rooms() {
                ret.entry(room.get_title_and_description().to_string())
                    .or_insert(Vec::new())
                    .push((*area_id, room.clone()));
            }
        }
        ret
    }

    fn build_rooms_by_title(
        areas: &HashMap<AreaId, Arc<AreaCache>>,
        disabled_areas: &HashSet<AreaId>,
    ) -> HashMap<String, RoomMatches> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            if disabled_areas.contains(area_id) {
                continue;
            }
            for room in area.get_rooms() {
                ret.entry(room.get_title().to_string())
                    .or_insert(Vec::new())
                    .push((*area_id, room.clone()));
            }
        }
        ret
    }

    fn build_rooms_by_description(
        areas: &HashMap<AreaId, Arc<AreaCache>>,
        disabled_areas: &HashSet<AreaId>,
    ) -> HashMap<String, RoomMatches> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            if disabled_areas.contains(area_id) {
                continue;
            }
            for room in area.get_rooms() {
                ret.entry(room.get_description().to_string())
                    .or_insert(Vec::new())
                    .push((*area_id, room.clone()));
            }
        }
        ret
    }

    fn build_rooms(areas: &HashMap<AreaId, Arc<AreaCache>>) -> HashMap<RoomKey, Arc<RoomCache>> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            for room in area.get_rooms() {
                ret.insert(
                    RoomKey::new(*area_id, room.get_room_number()),
                    room.clone(),
                );
            }
        }
        ret
    }

    #[must_use]
    pub(super) fn add_area(&self, area_id: AreaId, area: Arc<AreaCache>) -> Self {
        let mut new_areas = self.areas.clone();
        new_areas.insert(area_id, area);

        Self::new_with_areas(new_areas, self.disabled_areas.clone())
    }

    #[must_use]
    pub(super) fn insert_area(&self, area_id: AreaId, area: Arc<AreaCache>) -> Self {
        let mut new_areas = self.areas.clone();
        new_areas.remove(&area_id);
        new_areas.insert(area_id, area);

        Self::new_with_areas(new_areas, self.disabled_areas.clone())
    }

    /// Replaces several areas in one pass, rebuilding the lookup tables a
    /// single time. Equivalent to chained [`Self::insert_area`] calls but
    /// without rebuilding per area.
    #[must_use]
    pub(super) fn with_areas_updated(
        &self,
        updates: impl IntoIterator<Item = (AreaId, Arc<AreaCache>)>,
    ) -> Self {
        let mut new_areas = self.areas.clone();
        for (area_id, area) in updates {
            new_areas.insert(area_id, area);
        }

        Self::new_with_areas(new_areas, self.disabled_areas.clone())
    }

    #[must_use]
    pub(super) fn delete_area(&self, area_id: AreaId) -> Self {
        let mut new_areas = self.areas.clone();
        new_areas.remove(&area_id);

        Self::new_with_areas(new_areas, self.disabled_areas.clone())
    }

    /// Same areas, different disabled set (full lookup-table rebuild).
    #[must_use]
    pub(super) fn with_disabled_areas(&self, disabled_areas: Arc<HashSet<AreaId>>) -> Self {
        Self::new_with_areas(self.areas.clone(), disabled_areas)
    }

    #[must_use]
    pub fn areas(&self) -> impl ExactSizeIterator<Item = Arc<AreaCache>> {
        self.areas.values().cloned()
    }

    #[must_use]
    pub fn get_area(&self, area_id: &AreaId) -> Option<Arc<AreaCache>> {
        self.areas.get(area_id).cloned()
    }

    pub fn get_rooms_by_title_description_and_visible_exits<'a>(
        &self,
        title: &str,
        description: &str,
        visible_exit_directions: impl IntoIterator<Item = &'a ExitDirection>,
    ) -> impl ExactSizeIterator<Item = (AreaId, Arc<RoomCache>)> {
        let visible_exit_bitfield = ExitBitfield::from(visible_exit_directions);
        self.rooms_by_title_description_and_visible_exits
            .get(&(
                visible_exit_bitfield,
                format!("{title}\r\n{description}"),
            ))
            .unwrap_or(&EMPTY_ROOMS_LOOKUP_VEC)
            .iter()
            .cloned()
    }

    #[must_use]
    pub fn get_rooms_by_title_and_description(
        &self,
        title: &str,
        description: &str,
    ) -> impl ExactSizeIterator<Item = (AreaId, Arc<RoomCache>)> {
        self.rooms_by_title_and_description
            .get(&format!("{title}\r\n{description}"))
            .unwrap_or(&EMPTY_ROOMS_LOOKUP_VEC)
            .iter()
            .cloned()
    }

    #[must_use]
    pub fn get_rooms_by_title(
        &self,
        title: &str,
    ) -> impl ExactSizeIterator<Item = (AreaId, Arc<RoomCache>)> {
        self.rooms_by_title
            .get(title)
            .unwrap_or(&EMPTY_ROOMS_LOOKUP_VEC)
            .iter()
            .cloned()
    }

    #[must_use]
    pub fn get_rooms_by_description(
        &self,
        description: &str,
    ) -> impl ExactSizeIterator<Item = (AreaId, Arc<RoomCache>)> {
        self.rooms_by_description
            .get(description)
            .unwrap_or(&EMPTY_ROOMS_LOOKUP_VEC)
            .iter()
            .cloned()
    }

    #[must_use]
    pub fn get_room(&self, room_key: &RoomKey) -> Option<Arc<RoomCache>> {
        self.rooms.get(room_key).cloned()
    }

    /// Whether the viewer owns the given area (false for shared areas and
    /// areas not in the cache).
    #[must_use]
    pub fn is_area_owned(&self, area_id: &AreaId) -> bool {
        self.owned_areas.contains(area_id)
    }

    /// Whether the given area participates in room identification and
    /// routing (true for areas not in the cache).
    #[must_use]
    pub fn is_area_enabled(&self, area_id: &AreaId) -> bool {
        !self.disabled_areas.contains(area_id)
    }

    /// The full set of user-disabled areas (may contain ids the cache has
    /// not seen yet).
    #[must_use]
    pub fn disabled_areas(&self) -> &HashSet<AreaId> {
        &self.disabled_areas
    }

    /// Cheap handle to the disabled set for threading through rebuilds.
    pub(super) fn disabled_areas_arc(&self) -> Arc<HashSet<AreaId>> {
        self.disabled_areas.clone()
    }

    #[must_use]
    pub fn get_path_between_rooms(
        &self,
        from_room_key: &RoomKey,
        to_room_key: &RoomKey,
    ) -> Option<Vec<RoomKey>> {
        pathfinding::prelude::dijkstra(
            from_room_key,
            |room| {
                let successors = self.get_room(room).map_or_else(Vec::new, |r| {
                    r.linked_room_keys_and_weights()
                        .into_iter()
                        // Disabled areas are walls, not penalties: never route
                        // *through* one. Edges into the endpoints' own areas
                        // stay open so an explicitly named room in a disabled
                        // area is still reachable (and routable within).
                        .filter(|(key, _)| {
                            !self.disabled_areas.contains(&key.area_id)
                                || key.area_id == to_room_key.area_id
                                || key.area_id == from_room_key.area_id
                        })
                        // Own-beats-shared: edges into shared areas cost
                        // SHARED_AREA_WEIGHT_PENALTY times more, so routing
                        // only crosses into a friend's map when no
                        // comparable owned route exists.
                        .map(|(key, weight)| {
                            if self.owned_areas.contains(&key.area_id) {
                                (key, weight)
                            } else {
                                let penalized =
                                    OrderedFloat(weight.0 * SHARED_AREA_WEIGHT_PENALTY);
                                (key, penalized)
                            }
                        })
                        .collect::<Vec<_>>()
                });
                successors.into_iter()
            },
            |room_key| *room_key == *to_room_key,
        )
        .map(|(path, _)| path)
    }

    /// The nearest reachable room satisfying `predicate`, found by the same
    /// weighted traversal as [`Self::get_path_between_rooms`]: disabled areas are
    /// walls (except the start area), and edges into shared areas are penalized so
    /// an owned route wins when comparable. Dijkstra visits rooms in
    /// increasing-distance order and stops at the first match, so no tag index is
    /// needed. The start room itself is eligible (distance 0). `None` when no
    /// matching room is reachable.
    #[must_use]
    pub fn find_nearest_room_with_predicate<F>(
        &self,
        from_room_key: &RoomKey,
        predicate: F,
    ) -> Option<RoomKey>
    where
        F: Fn(&RoomCache) -> bool,
    {
        pathfinding::prelude::dijkstra(
            from_room_key,
            |room| {
                let successors = self.get_room(room).map_or_else(Vec::new, |r| {
                    r.linked_room_keys_and_weights()
                        .into_iter()
                        .filter(|(key, _)| {
                            !self.disabled_areas.contains(&key.area_id)
                                || key.area_id == from_room_key.area_id
                        })
                        .map(|(key, weight)| {
                            if self.owned_areas.contains(&key.area_id) {
                                (key, weight)
                            } else {
                                (key, OrderedFloat(weight.0 * SHARED_AREA_WEIGHT_PENALTY))
                            }
                        })
                        .collect::<Vec<_>>()
                });
                successors.into_iter()
            },
            |room_key| self.get_room(room_key).is_some_and(|r| predicate(r.as_ref())),
        )
        .and_then(|(path, _cost)| path.last().cloned())
    }

    /// The nearest reachable room whose tags satisfy a conjunctive filter: it
    /// carries every tag in `required` and none in `excluded` (both
    /// case-insensitive). Backs the multi-tag speedwalk (`\inn.peace`,
    /// `\!peace.guild`). The filters are normalized once up front, so the per-room
    /// test is just set lookups against the room's tag `BTreeSet`. The start room
    /// counts if it matches. An empty filter expresses no constraint and yields
    /// `None` rather than matching the start room unconditionally.
    #[must_use]
    pub fn find_nearest_room_matching_tags(
        &self,
        from_room_key: &RoomKey,
        required: &[String],
        excluded: &[String],
    ) -> Option<RoomKey> {
        let normalize = |tags: &[String]| -> Vec<String> {
            tags.iter()
                .map(|t| crate::mapper::normalize_tag(t))
                .filter(|t| !t.is_empty())
                .collect()
        };
        let required = normalize(required);
        let excluded = normalize(excluded);

        if required.is_empty() && excluded.is_empty() {
            return None;
        }

        self.find_nearest_room_with_predicate(from_room_key, |room| {
            let tags = room.get_tags();
            required.iter().all(|t| tags.contains(t)) && !excluded.iter().any(|t| tags.contains(t))
        })
    }

    /// The nearest reachable room carrying `tag` (case-insensitive). A convenience
    /// over [`Self::find_nearest_room_matching_tags`] for the single-tag case.
    #[must_use]
    pub fn find_nearest_room_with_tag(
        &self,
        from_room_key: &RoomKey,
        tag: &str,
    ) -> Option<RoomKey> {
        self.find_nearest_room_matching_tags(from_room_key, &[tag.to_string()], &[])
    }

    /// The nearest reachable room belonging to `target_area_id`, by the same
    /// weighted traversal as [`Self::get_path_between_rooms`]: disabled areas are
    /// walls — except the start area and, because the caller named it, the target
    /// area itself — and edges into shared areas are penalized so an owned route
    /// wins when comparable. The start room counts if it is already in the target
    /// area (distance 0). `None` when the area has no reachable room.
    #[must_use]
    pub fn find_nearest_room_in_area(
        &self,
        from_room_key: &RoomKey,
        target_area_id: &AreaId,
    ) -> Option<RoomKey> {
        pathfinding::prelude::dijkstra(
            from_room_key,
            |room| {
                let successors = self.get_room(room).map_or_else(Vec::new, |r| {
                    r.linked_room_keys_and_weights()
                        .into_iter()
                        .filter(|(key, _)| {
                            !self.disabled_areas.contains(&key.area_id)
                                || key.area_id == *target_area_id
                                || key.area_id == from_room_key.area_id
                        })
                        .map(|(key, weight)| {
                            if self.owned_areas.contains(&key.area_id) {
                                (key, weight)
                            } else {
                                (key, OrderedFloat(weight.0 * SHARED_AREA_WEIGHT_PENALTY))
                            }
                        })
                        .collect::<Vec<_>>()
                });
                successors.into_iter()
            },
            // Existence-guarded: an exit can dangle into the target area at a
            // room number the cache has never seen; such a key must not "win".
            |room_key| {
                room_key.area_id == *target_area_id && self.get_room(room_key).is_some()
            },
        )
        .and_then(|(path, _cost)| path.last().cloned())
    }
}

/// Stable-sorts every lookup vector so rooms from owned areas come before
/// rooms from shared areas (relative order within each class is preserved).
fn sort_owned_first<K>(
    table: &mut HashMap<K, RoomMatches>,
    owned_areas: &HashSet<AreaId>,
) {
    for rooms in table.values_mut() {
        rooms.sort_by_key(|(area_id, _)| !owned_areas.contains(area_id));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Area, AreaAccess, AreaWithDetails, Exit, ExitId, ExitStyle, RoomNumber, RoomWithDetails,
        Uuid,
    };
    use chrono::Utc;

    fn area_id(n: u128) -> AreaId {
        AreaId(Uuid::from_u128(n))
    }

    fn atlas(areas: HashMap<AreaId, Arc<AreaCache>>) -> AtlasCache {
        AtlasCache::new_with_areas(areas, Arc::new(HashSet::new()))
    }

    fn atlas_with_disabled(
        areas: HashMap<AreaId, Arc<AreaCache>>,
        disabled: impl IntoIterator<Item = AreaId>,
    ) -> AtlasCache {
        AtlasCache::new_with_areas(areas, Arc::new(disabled.into_iter().collect()))
    }

    fn access(owner: bool) -> AreaAccess {
        AreaAccess {
            is_owner: owner,
            can_edit: owner,
            can_reshare: false,
            can_copy: false,
            can_admin: owner,
            include_secrets: owner,
        }
    }

    fn room(number: i32, title: &str, exits: Vec<Exit>) -> RoomWithDetails {
        RoomWithDetails {
            room_number: RoomNumber(number),
            title: title.to_string(),
            description: String::new(),
            level: 0,
            x: 0.0,
            y: 0.0,
            color: String::new(),
            properties: Vec::new(),
            exits,
            tags: Default::default(),
            is_secret: false,
        }
    }

    fn exit(id: u128, to_area: AreaId, to_room: i32, weight: f32) -> Exit {
        Exit {
            id: ExitId(Uuid::from_u128(id)),
            from_direction: crate::ExitDirection::North,
            to_area_id: Some(to_area),
            to_room_number: Some(RoomNumber(to_room)),
            to_direction: None,
            path: String::new(),
            is_hidden: false,
            is_closed: false,
            is_locked: false,
            weight,
            command: String::new(),
            style: ExitStyle::Normal,
            color: String::new(),
            to_unknown: false,
            to_area_token: None,
            is_secret: false,
        }
    }

    fn cache_area(
        id: AreaId,
        owned: bool,
        rooms: Vec<RoomWithDetails>,
    ) -> (AreaId, Arc<AreaCache>) {
        let details = AreaWithDetails {
            area: Area {
                id,
                user_id: None,
                atlas_id: None,
                name: format!("area {id}"),
                created_at: Utc::now(),
                rev: 1,
                access: Some(access(owned)),
                owner_nickname: (!owned).then(|| "friend".to_string()),
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
            },
            content_hash: None,
            properties: Vec::new(),
            rooms,
            labels: Vec::new(),
            shapes: Vec::new(),
            linked_areas: Vec::new(),
        };
        (id, Arc::new(AreaCache::new_with_area(details)))
    }

    #[test]
    fn owned_rooms_sort_before_shared_in_lookups() {
        let owned_id = area_id(1);
        let shared_id = area_id(2);

        // Insert shared first so a stable no-op "sort" would leave it first.
        let mut areas = HashMap::new();
        let (id, cache) = cache_area(shared_id, false, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);
        let (id, cache) = cache_area(owned_id, true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);

        let atlas = atlas(areas);

        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Plaza")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![owned_id, shared_id]);

        let by_title_and_description: Vec<AreaId> = atlas
            .get_rooms_by_title_and_description("Plaza", "")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title_and_description, vec![owned_id, shared_id]);

        assert!(atlas.is_area_owned(&owned_id));
        assert!(!atlas.is_area_owned(&shared_id));
    }

    #[test]
    fn find_nearest_room_with_tag_returns_closest_match() {
        let a = area_id(1);
        // Linear chain 1 -> 2 -> 3 -> 4, with INN on rooms 3 (dist 2) and 4 (dist 3).
        let mut r3 = room(3, "Near Inn", vec![exit(34, a, 4, 1.0)]);
        r3.tags = ["INN".to_string()].into_iter().collect();
        let mut r4 = room(4, "Far Inn", Vec::new());
        r4.tags = ["INN".to_string()].into_iter().collect();
        let rooms = vec![
            room(1, "Start", vec![exit(12, a, 2, 1.0)]),
            room(2, "Mid", vec![exit(23, a, 3, 1.0)]),
            r3,
            r4,
        ];

        let mut areas = HashMap::new();
        let (id, cache) = cache_area(a, true, rooms);
        areas.insert(id, cache);
        let atlas = atlas(areas);

        assert_eq!(
            atlas.find_nearest_room_with_tag(&RoomKey::new(a, RoomNumber(1)), "inn"),
            Some(RoomKey::new(a, RoomNumber(3))),
            "nearest INN from room 1 is room 3, and the query folds case"
        );
        assert_eq!(
            atlas.find_nearest_room_with_tag(&RoomKey::new(a, RoomNumber(3)), "INN"),
            Some(RoomKey::new(a, RoomNumber(3))),
            "the start room counts when it carries the tag (distance 0)"
        );
        assert_eq!(
            atlas.find_nearest_room_with_tag(&RoomKey::new(a, RoomNumber(1)), "GUILD"),
            None,
            "no reachable room carries GUILD"
        );
    }

    #[test]
    fn find_nearest_room_matching_tags_handles_and_and_negation() {
        let a = area_id(1);
        // 1 -> 2 -> 3 (GUILD+PEACE, dist 2) -> 4 (GUILD only, dist 3).
        let mut r3 = room(3, "Peaceful Guild", vec![exit(34, a, 4, 1.0)]);
        r3.tags = ["GUILD".to_string(), "PEACE".to_string()]
            .into_iter()
            .collect();
        let mut r4 = room(4, "Rough Guild", Vec::new());
        r4.tags = ["GUILD".to_string()].into_iter().collect();
        let rooms = vec![
            room(1, "Start", vec![exit(12, a, 2, 1.0)]),
            room(2, "Mid", vec![exit(23, a, 3, 1.0)]),
            r3,
            r4,
        ];
        let mut areas = HashMap::new();
        let (id, cache) = cache_area(a, true, rooms);
        areas.insert(id, cache);
        let atlas = atlas(areas);
        let from = RoomKey::new(a, RoomNumber(1));

        // Nearest GUILD is the closer room 3.
        assert_eq!(
            atlas.find_nearest_room_matching_tags(&from, &["guild".to_string()], &[]),
            Some(RoomKey::new(a, RoomNumber(3))),
            "case-insensitive AND of one tag finds the nearest"
        );
        // GUILD but NOT PEACE skips the closer room 3 for the farther room 4.
        assert_eq!(
            atlas.find_nearest_room_matching_tags(
                &from,
                &["guild".to_string()],
                &["peace".to_string()]
            ),
            Some(RoomKey::new(a, RoomNumber(4))),
            "negation excludes the closer PEACE room"
        );
        // AND of two tags no room has together -> None.
        assert_eq!(
            atlas.find_nearest_room_matching_tags(
                &from,
                &["guild".to_string(), "inn".to_string()],
                &[]
            ),
            None,
        );
        // An empty filter expresses no constraint.
        assert_eq!(atlas.find_nearest_room_matching_tags(&from, &[], &[]), None);
    }

    #[test]
    fn find_nearest_room_in_area_returns_closest_room_of_that_area() {
        let a = area_id(1);
        let b = area_id(2);
        // a1 -> a2 -> b1 -> b2: the first room of area b along the route is b1.
        let rooms_a = vec![
            room(1, "Start", vec![exit(12, a, 2, 1.0)]),
            room(2, "Border", vec![exit(21, b, 1, 1.0)]),
        ];
        let rooms_b = vec![
            room(1, "Gate", vec![exit(31, b, 2, 1.0)]),
            room(2, "Square", Vec::new()),
        ];
        let mut areas = HashMap::new();
        let (id, cache) = cache_area(a, true, rooms_a);
        areas.insert(id, cache);
        let (id, cache) = cache_area(b, true, rooms_b);
        areas.insert(id, cache);
        let atlas = atlas(areas);
        let from = RoomKey::new(a, RoomNumber(1));

        assert_eq!(
            atlas.find_nearest_room_in_area(&from, &b),
            Some(RoomKey::new(b, RoomNumber(1))),
            "the nearest room of area b along the route wins"
        );
        assert_eq!(
            atlas.find_nearest_room_in_area(&from, &a),
            Some(RoomKey::new(a, RoomNumber(1))),
            "the start room counts when it is already in the target area (distance 0)"
        );
        assert_eq!(
            atlas.find_nearest_room_in_area(&from, &area_id(9)),
            None,
            "an area with no reachable room yields None"
        );
    }

    #[test]
    fn find_nearest_room_in_area_names_through_disabled_flags() {
        let a = area_id(1);
        let b = area_id(2);
        let c = area_id(3);
        // a1 -> b1 -> c1, with b disabled.
        let rooms_a = vec![room(1, "Start", vec![exit(12, b, 1, 1.0)])];
        let rooms_b = vec![room(1, "Gate", vec![exit(23, c, 1, 1.0)])];
        let rooms_c = vec![room(1, "Beyond", Vec::new())];
        let mut areas = HashMap::new();
        let (id, cache) = cache_area(a, true, rooms_a);
        areas.insert(id, cache);
        let (id, cache) = cache_area(b, true, rooms_b);
        areas.insert(id, cache);
        let (id, cache) = cache_area(c, true, rooms_c);
        areas.insert(id, cache);
        let atlas = atlas_with_disabled(areas, [b]);
        let from = RoomKey::new(a, RoomNumber(1));

        assert_eq!(
            atlas.find_nearest_room_in_area(&from, &b),
            Some(RoomKey::new(b, RoomNumber(1))),
            "naming the area overrides its disabled flag, matching get_path_between_rooms"
        );
        assert_eq!(
            atlas.find_nearest_room_in_area(&from, &c),
            None,
            "a disabled area that is NOT the target stays a wall"
        );
    }

    #[test]
    fn routing_prefers_owned_route_over_shared_shortcut() {
        let owned_id = area_id(1);
        let shared_id = area_id(2);

        // Owned: 1 -> 2 -> 3 (cost 2). Shared shortcut: 1 -> S10 -> 3
        // (raw cost 2, penalized 4 + 1). Routing must take the owned path.
        let owned_rooms = vec![
            room(
                1,
                "start",
                vec![
                    exit(11, owned_id, 2, 1.0),
                    exit(12, shared_id, 10, 1.0),
                ],
            ),
            room(2, "mid", vec![exit(13, owned_id, 3, 1.0)]),
            room(3, "goal", Vec::new()),
        ];
        let shared_rooms = vec![room(10, "shortcut", vec![exit(14, owned_id, 3, 1.0)])];

        let mut areas = HashMap::new();
        let (id, cache) = cache_area(owned_id, true, owned_rooms);
        areas.insert(id, cache);
        let (id, cache) = cache_area(shared_id, false, shared_rooms);
        areas.insert(id, cache);

        let atlas = atlas(areas);

        let path = atlas
            .get_path_between_rooms(
                &RoomKey::new(owned_id, RoomNumber(1)),
                &RoomKey::new(owned_id, RoomNumber(3)),
            )
            .expect("a route exists");
        assert_eq!(
            path,
            vec![
                RoomKey::new(owned_id, RoomNumber(1)),
                RoomKey::new(owned_id, RoomNumber(2)),
                RoomKey::new(owned_id, RoomNumber(3)),
            ]
        );

        // Shared-only destinations still resolve — the penalty is a bias,
        // not a wall.
        let into_shared = atlas.get_path_between_rooms(
            &RoomKey::new(owned_id, RoomNumber(1)),
            &RoomKey::new(shared_id, RoomNumber(10)),
        );
        assert_eq!(
            into_shared,
            Some(vec![
                RoomKey::new(owned_id, RoomNumber(1)),
                RoomKey::new(shared_id, RoomNumber(10)),
            ])
        );
    }

    #[test]
    fn disabled_area_rooms_absent_from_all_lookup_tables() {
        let enabled_id = area_id(1);
        let disabled_id = area_id(2);

        let mut areas = HashMap::new();
        let (id, cache) = cache_area(enabled_id, true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);
        let (id, cache) = cache_area(disabled_id, true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);

        let atlas = atlas_with_disabled(areas, [disabled_id]);

        let only_enabled = |rooms: Vec<AreaId>| {
            assert_eq!(rooms, vec![enabled_id]);
        };
        only_enabled(
            atlas
                .get_rooms_by_title_description_and_visible_exits("Plaza", "", std::iter::empty())
                .map(|(area_id, _)| area_id)
                .collect(),
        );
        only_enabled(
            atlas
                .get_rooms_by_title_and_description("Plaza", "")
                .map(|(area_id, _)| area_id)
                .collect(),
        );
        only_enabled(
            atlas
                .get_rooms_by_title("Plaza")
                .map(|(area_id, _)| area_id)
                .collect(),
        );
        only_enabled(
            atlas
                .get_rooms_by_description("")
                .map(|(area_id, _)| area_id)
                .collect(),
        );

        // Explicit addressing still works: the area and its rooms stay
        // resident, only the identification tables exclude them.
        assert!(atlas.get_area(&disabled_id).is_some());
        assert!(
            atlas
                .get_room(&RoomKey::new(disabled_id, RoomNumber(1)))
                .is_some()
        );
        assert!(!atlas.is_area_enabled(&disabled_id));
        assert!(atlas.is_area_enabled(&enabled_id));
    }

    #[test]
    fn routing_avoids_disabled_intermediate_but_reaches_disabled_endpoints() {
        let a_id = area_id(1);
        let b_id = area_id(2);

        // A1 -> B10 -> A3 is the only route from A1 to A3; B10 -> B11 is
        // internal to B.
        let a_rooms = vec![
            room(1, "start", vec![exit(11, b_id, 10, 1.0)]),
            room(3, "goal", Vec::new()),
        ];
        let b_rooms = vec![
            room(
                10,
                "bridge",
                vec![exit(12, a_id, 3, 1.0), exit(13, b_id, 11, 1.0)],
            ),
            room(11, "vault", Vec::new()),
        ];

        let mut areas = HashMap::new();
        let (id, cache) = cache_area(a_id, true, a_rooms);
        areas.insert(id, cache);
        let (id, cache) = cache_area(b_id, true, b_rooms);
        areas.insert(id, cache);

        let atlas = atlas_with_disabled(areas, [b_id]);

        // Through B: refused.
        assert_eq!(
            atlas.get_path_between_rooms(
                &RoomKey::new(a_id, RoomNumber(1)),
                &RoomKey::new(a_id, RoomNumber(3)),
            ),
            None,
            "must not route through a disabled intermediate area"
        );

        // To an explicitly named room in B: allowed.
        assert_eq!(
            atlas.get_path_between_rooms(
                &RoomKey::new(a_id, RoomNumber(1)),
                &RoomKey::new(b_id, RoomNumber(10)),
            ),
            Some(vec![
                RoomKey::new(a_id, RoomNumber(1)),
                RoomKey::new(b_id, RoomNumber(10)),
            ])
        );

        // Deeper into the destination's own (disabled) area: allowed.
        assert_eq!(
            atlas.get_path_between_rooms(
                &RoomKey::new(a_id, RoomNumber(1)),
                &RoomKey::new(b_id, RoomNumber(11)),
            ),
            Some(vec![
                RoomKey::new(a_id, RoomNumber(1)),
                RoomKey::new(b_id, RoomNumber(10)),
                RoomKey::new(b_id, RoomNumber(11)),
            ])
        );

        // From a disabled start area outward and within it: allowed.
        assert_eq!(
            atlas.get_path_between_rooms(
                &RoomKey::new(b_id, RoomNumber(10)),
                &RoomKey::new(a_id, RoomNumber(3)),
            ),
            Some(vec![
                RoomKey::new(b_id, RoomNumber(10)),
                RoomKey::new(a_id, RoomNumber(3)),
            ])
        );
        assert_eq!(
            atlas.get_path_between_rooms(
                &RoomKey::new(b_id, RoomNumber(10)),
                &RoomKey::new(b_id, RoomNumber(11)),
            ),
            Some(vec![
                RoomKey::new(b_id, RoomNumber(10)),
                RoomKey::new(b_id, RoomNumber(11)),
            ])
        );
    }

    #[test]
    fn disabling_unknown_area_is_preserved_and_applies_when_it_lands() {
        let resident_id = area_id(1);
        let future_id = area_id(2);

        let mut areas = HashMap::new();
        let (id, cache) = cache_area(resident_id, true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);

        // Disable an area the cache has never seen (UI can disable before a
        // background sync lands the area).
        let atlas = atlas_with_disabled(areas, [future_id]);
        assert!(atlas.disabled_areas().contains(&future_id));
        assert_eq!(
            atlas
                .get_rooms_by_title("Plaza")
                .map(|(area_id, _)| area_id)
                .collect::<Vec<_>>(),
            vec![resident_id]
        );

        // The area lands later (sync engine path: insert_area); the stored
        // disabled set must keep it out of the lookup tables.
        let (_, cache) = cache_area(future_id, false, vec![room(1, "Plaza", Vec::new())]);
        let atlas = atlas.insert_area(future_id, cache);
        assert!(atlas.disabled_areas().contains(&future_id));
        assert!(atlas.get_area(&future_id).is_some());
        assert_eq!(
            atlas
                .get_rooms_by_title("Plaza")
                .map(|(area_id, _)| area_id)
                .collect::<Vec<_>>(),
            vec![resident_id]
        );
    }

    #[test]
    fn owned_first_sort_holds_among_enabled_areas_with_a_disabled_third() {
        let owned_id = area_id(1);
        let shared_id = area_id(2);
        let disabled_id = area_id(3);

        // Insert shared first so a stable no-op "sort" would leave it first.
        let mut areas = HashMap::new();
        let (id, cache) = cache_area(shared_id, false, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);
        let (id, cache) = cache_area(owned_id, true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);
        let (id, cache) = cache_area(disabled_id, true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);

        let atlas = atlas_with_disabled(areas, [disabled_id]);

        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Plaza")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![owned_id, shared_id]);
    }
}
