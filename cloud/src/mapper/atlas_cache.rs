use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use ordered_float::OrderedFloat;

use crate::{
    AreaId, AtlasId, ExitDirection,
    mapper::{
        RoomKey,
        area_cache::AreaCache,
        room_cache::{ExitBitfield, RoomCache},
    },
};

/// The two independent axes that keep an area out of the room-identification
/// lookup tables and out of routing. Exclusion is **one behavior with two
/// sources**, folded into a single predicate ([`Self::excludes`]) so a
/// scope-excluded area is invisible to identification exactly like a
/// manually-disabled one — while the two sets stay separate so the editor's
/// per-area active switch can keep reflecting only the manual axis.
///
/// - `disabled`: the user's manual per-area active/inactive toggle
///   (`Mapper::set_disabled_areas`). May carry ids the cache has not seen yet.
/// - `atlases`/`areas`: the per-server scope associations
///   (`Mapper::set_scope_exclusions`). Keying exclusion by *atlas* is
///   deliberate: an area that later syncs into an excluded atlas is excluded
///   automatically, with no recomputation.
#[derive(Clone, Default)]
struct Exclusions {
    disabled: Arc<HashSet<AreaId>>,
    atlases: Arc<HashSet<AtlasId>>,
    areas: Arc<HashSet<AreaId>>,
}

impl Exclusions {
    /// Whether `area` (with id `area_id`) is excluded by either axis.
    fn excludes(&self, area_id: &AreaId, area: &AreaCache) -> bool {
        self.disabled.contains(area_id)
            || self.scope_excludes(area_id, area)
    }

    /// Whether `area` is excluded specifically by the **per-server scope** axis
    /// (associated only with other server entries) — the manual-disable axis is
    /// deliberately not consulted. This is the cross-entry rescue predicate: a
    /// room in a scope-excluded area is one that matches a map the user has
    /// homed on a *different* entry, which is exactly what the rescue offer is
    /// about. A manually-disabled area is the user's own toggle, not another
    /// entry's map, so it is never a rescue candidate.
    fn scope_excludes(&self, area_id: &AreaId, area: &AreaCache) -> bool {
        self.areas.contains(area_id)
            || area
                .meta()
                .atlas_id
                .is_some_and(|atlas| self.atlases.contains(&atlas))
    }
}

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

/// A cross-entry rescue hit: a room found in a scope-excluded area (a map homed
/// on a different server entry), with enough context to phrase the "show here
/// too?" offer. `atlas_id`/`atlas_name` are `None` for a genuinely atlas-less
/// excluded area or when the source row carried no atlas name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ElsewhereMatch {
    pub room_key: RoomKey,
    pub atlas_id: Option<AtlasId>,
    pub atlas_name: Option<String>,
}

#[derive(Clone)]
pub struct AtlasCache {
    areas: HashMap<AreaId, Arc<AreaCache>>,
    rooms_by_title_description_and_visible_exits:
        HashMap<(ExitBitfield, String), RoomMatches>,
    rooms_by_title_and_description: HashMap<String, RoomMatches>,
    rooms_by_title: HashMap<String, RoomMatches>,
    rooms_by_description: HashMap<String, RoomMatches>,
    /// Reverse index over rooms' server-global external ids (GMCP/MSDP room
    /// identity) — the mapper hot path for id → room resolution. Built like
    /// the other identification tables (disabled areas excluded). Uniqueness
    /// is not enforced; under duplicate bindings an owned area's room wins,
    /// otherwise the winner is unspecified (documented best-effort).
    rooms_by_external_id: HashMap<String, RoomKey>,
    /// The external-id reverse index over the *scope-excluded* areas only — the
    /// mirror image of `rooms_by_external_id`, which omits them. This is the
    /// cross-entry rescue probe's index: when normal identification fails on a
    /// room, this answers "is it already mapped on another server entry?" in
    /// O(1). A second index (rather than a linear scan over excluded areas per
    /// probe) is the hot-path-honest choice: the auto-mapper consults the rescue
    /// path on *every* unmapped room while exploring, so an O(1) lookup against
    /// a table built once per (rare) cache rebuild beats re-scanning a
    /// potentially large sibling map on each step. Manual-disable exclusions are
    /// excluded from this index — only per-server-scope exclusions rescue.
    rooms_by_external_id_excluded: HashMap<String, RoomKey>,
    rooms: HashMap<RoomKey, Arc<RoomCache>>,
    /// Areas the viewer owns, for own-beats-shared precedence in lookups and
    /// routing. Built once per cache rebuild; lookups are O(1).
    owned_areas: HashSet<AreaId>,
    /// The manual-disable and per-server-scope exclusion sets. Excluded areas
    /// drop out of the room-identification lookup tables at build time and are
    /// never routed *through* (still present in `areas` and `rooms` so explicit
    /// addressing keeps working). The sets are `Arc` so they ride through every
    /// rebuild for free, and may contain ids not (yet) in `areas` — exclusion
    /// survives the area landing later.
    exclusions: Exclusions,
}

impl AtlasCache {
    pub(super) fn new_with_areas(
        areas: HashMap<AreaId, Arc<AreaCache>>,
        disabled_areas: Arc<HashSet<AreaId>>,
    ) -> Self {
        Self::new_with_exclusions(
            areas,
            Exclusions {
                disabled: disabled_areas,
                ..Exclusions::default()
            },
        )
    }

    fn new_with_exclusions(areas: HashMap<AreaId, Arc<AreaCache>>, exclusions: Exclusions) -> Self {
        let owned_areas: HashSet<AreaId> = areas
            .iter()
            .filter(|(_, area)| area.is_owned())
            .map(|(area_id, _)| *area_id)
            .collect();

        let mut rooms_by_title_description_and_visible_exits =
            Self::build_rooms_by_title_description_and_visible_exits(&areas, &exclusions);
        let mut rooms_by_title_and_description =
            Self::build_rooms_by_title_and_description(&areas, &exclusions);
        let mut rooms_by_title = Self::build_rooms_by_title(&areas, &exclusions);
        let mut rooms_by_description = Self::build_rooms_by_description(&areas, &exclusions);
        let rooms_by_external_id =
            Self::build_rooms_by_external_id(&areas, &exclusions, &owned_areas);
        let rooms_by_external_id_excluded =
            Self::build_rooms_by_external_id_excluded(&areas, &exclusions);
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
            rooms_by_external_id,
            rooms_by_external_id_excluded,
            rooms,
            owned_areas,
            exclusions,
        }
    }

    fn build_rooms_by_external_id(
        areas: &HashMap<AreaId, Arc<AreaCache>>,
        exclusions: &Exclusions,
        owned_areas: &HashSet<AreaId>,
    ) -> HashMap<String, RoomKey> {
        let mut ret: HashMap<String, RoomKey> = HashMap::new();
        for (area_id, area) in areas {
            if exclusions.excludes(area_id, area) {
                continue;
            }
            for room in area.get_rooms() {
                let Some(external_id) = room.get_external_id() else {
                    continue;
                };
                let key = RoomKey::new(*area_id, room.get_room_number());
                match ret.entry(external_id.to_string()) {
                    std::collections::hash_map::Entry::Vacant(entry) => {
                        entry.insert(key);
                    }
                    // Own-beats-shared under duplicate bindings; otherwise
                    // keep the incumbent (arbitrary but stable within a build).
                    std::collections::hash_map::Entry::Occupied(mut entry) => {
                        if owned_areas.contains(area_id)
                            && !owned_areas.contains(&entry.get().area_id)
                        {
                            entry.insert(key);
                        }
                    }
                }
            }
        }
        ret
    }

    /// The external-id reverse index over the scope-excluded areas only (the
    /// mirror of [`Self::build_rooms_by_external_id`]). Own-beats-shared has no
    /// meaning here — every area is another entry's map — so the first binding
    /// for an id wins and later ones are kept out; a rescue offer only needs to
    /// know *that* the room is homed elsewhere, not to disambiguate duplicates.
    fn build_rooms_by_external_id_excluded(
        areas: &HashMap<AreaId, Arc<AreaCache>>,
        exclusions: &Exclusions,
    ) -> HashMap<String, RoomKey> {
        let mut ret: HashMap<String, RoomKey> = HashMap::new();
        for (area_id, area) in areas {
            if !exclusions.scope_excludes(area_id, area) {
                continue;
            }
            for room in area.get_rooms() {
                let Some(external_id) = room.get_external_id() else {
                    continue;
                };
                ret.entry(external_id.to_string())
                    .or_insert_with(|| RoomKey::new(*area_id, room.get_room_number()));
            }
        }
        ret
    }

    /// Resolve a server-global external id to its bound room. O(1); disabled
    /// areas are excluded (external-id resolution is room identification).
    #[must_use]
    pub fn find_room_by_external_id(&self, external_id: &str) -> Option<(RoomKey, Arc<RoomCache>)> {
        let key = self.rooms_by_external_id.get(external_id)?;
        self.rooms.get(key).map(|room| (key.clone(), room.clone()))
    }

    /// Cross-entry rescue probe: resolve a server-global external id against the
    /// *scope-excluded* areas only — maps the user has homed on a different
    /// server entry, deliberately absent from normal identification. Returns the
    /// matched room plus its atlas id and name (for the "shown on …" offer), or
    /// `None`. This never touches the normal lookup tables' semantics: it reads
    /// a separate index and the excluded areas stay resident (explicitly
    /// addressable) exactly as before.
    #[must_use]
    pub fn find_room_elsewhere_by_external_id(
        &self,
        external_id: &str,
    ) -> Option<ElsewhereMatch> {
        let key = self.rooms_by_external_id_excluded.get(external_id)?.clone();
        let meta = self.areas.get(&key.area_id).map(|area| area.meta());
        Some(ElsewhereMatch {
            room_key: key,
            atlas_id: meta.and_then(|m| m.atlas_id),
            atlas_name: meta.and_then(|m| m.atlas_name.clone()),
        })
    }

    fn build_rooms_by_title_description_and_visible_exits(
        areas: &HashMap<AreaId, Arc<AreaCache>>,
        exclusions: &Exclusions,
    ) -> HashMap<(ExitBitfield, String), RoomMatches> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            if exclusions.excludes(area_id, area) {
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
        exclusions: &Exclusions,
    ) -> HashMap<String, RoomMatches> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            if exclusions.excludes(area_id, area) {
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
        exclusions: &Exclusions,
    ) -> HashMap<String, RoomMatches> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            if exclusions.excludes(area_id, area) {
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
        exclusions: &Exclusions,
    ) -> HashMap<String, RoomMatches> {
        let mut ret = HashMap::new();
        for (area_id, area) in areas {
            if exclusions.excludes(area_id, area) {
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

        Self::new_with_exclusions(new_areas, self.exclusions.clone())
    }

    #[must_use]
    pub(super) fn insert_area(&self, area_id: AreaId, area: Arc<AreaCache>) -> Self {
        let mut new_areas = self.areas.clone();
        new_areas.remove(&area_id);
        new_areas.insert(area_id, area);

        Self::new_with_exclusions(new_areas, self.exclusions.clone())
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

        Self::new_with_exclusions(new_areas, self.exclusions.clone())
    }

    #[must_use]
    pub(super) fn delete_area(&self, area_id: AreaId) -> Self {
        let mut new_areas = self.areas.clone();
        new_areas.remove(&area_id);

        Self::new_with_exclusions(new_areas, self.exclusions.clone())
    }

    /// Same areas, different manual-disable set — scope exclusions preserved
    /// (full lookup-table rebuild).
    #[must_use]
    pub(super) fn with_disabled_areas(&self, disabled_areas: Arc<HashSet<AreaId>>) -> Self {
        Self::new_with_exclusions(
            self.areas.clone(),
            Exclusions {
                disabled: disabled_areas,
                ..self.exclusions.clone()
            },
        )
    }

    /// Same areas, different per-server scope-exclusion sets — the manual
    /// disable axis preserved (full lookup-table rebuild).
    #[must_use]
    pub(super) fn with_scope_exclusions(
        &self,
        excluded_atlases: Arc<HashSet<AtlasId>>,
        excluded_areas: Arc<HashSet<AreaId>>,
    ) -> Self {
        Self::new_with_exclusions(
            self.areas.clone(),
            Exclusions {
                disabled: self.exclusions.disabled.clone(),
                atlases: excluded_atlases,
                areas: excluded_areas,
            },
        )
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

    /// Whether the area is enabled on the **manual** active/inactive axis only
    /// (true for areas not in the cache). This deliberately ignores per-server
    /// scope exclusion, so the map editor's per-area active switch keeps
    /// reflecting exactly the user's manual toggle. Use [`Self::is_area_included`]
    /// for "does this area participate in room identification/routing".
    #[must_use]
    pub fn is_area_enabled(&self, area_id: &AreaId) -> bool {
        !self.exclusions.disabled.contains(area_id)
    }

    /// Whether the area participates in room identification and routing: not
    /// manually disabled **and** not scope-excluded (true for areas not in the
    /// cache). This is the union both the lookup tables and routing honor.
    #[must_use]
    pub fn is_area_included(&self, area_id: &AreaId) -> bool {
        !self.area_is_excluded(area_id)
    }

    /// Whether either axis excludes `area_id` from identification/routing. The
    /// atlas axis is resolved by looking the area up to read its `atlas_id`.
    fn area_is_excluded(&self, area_id: &AreaId) -> bool {
        if self.exclusions.disabled.contains(area_id) || self.exclusions.areas.contains(area_id) {
            return true;
        }
        self.areas
            .get(area_id)
            .and_then(|area| area.meta().atlas_id)
            .is_some_and(|atlas| self.exclusions.atlases.contains(&atlas))
    }

    /// The full set of manually-disabled areas (may contain ids the cache has
    /// not seen yet). The manual axis only — scope exclusions are separate.
    #[must_use]
    pub fn disabled_areas(&self) -> &HashSet<AreaId> {
        &self.exclusions.disabled
    }

    /// Rebuild the lookup tables over a fresh area set, carrying **every**
    /// exclusion axis (manual disable + per-server scope) forward. The wholesale
    /// reload path uses this so a full refetch never silently re-includes a
    /// scope-excluded or disabled area.
    #[must_use]
    pub(super) fn rebuild_with_areas(&self, areas: HashMap<AreaId, Arc<AreaCache>>) -> Self {
        Self::new_with_exclusions(areas, self.exclusions.clone())
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
                        // Excluded areas (manually disabled or per-server
                        // scope-excluded) are walls, not penalties: never route
                        // *through* one. Edges into the endpoints' own areas
                        // stay open so an explicitly named room in an excluded
                        // area is still reachable (and routable within).
                        .filter(|(key, _)| {
                            !self.area_is_excluded(&key.area_id)
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
                            !self.area_is_excluded(&key.area_id)
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
                            !self.area_is_excluded(&key.area_id)
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

    fn atlas_with_scope(
        areas: HashMap<AreaId, Arc<AreaCache>>,
        excluded_atlases: impl IntoIterator<Item = AtlasId>,
        excluded_areas: impl IntoIterator<Item = AreaId>,
    ) -> AtlasCache {
        AtlasCache::new_with_exclusions(
            areas,
            Exclusions {
                disabled: Arc::new(HashSet::new()),
                atlases: Arc::new(excluded_atlases.into_iter().collect()),
                areas: Arc::new(excluded_areas.into_iter().collect()),
            },
        )
    }

    fn atlas_id(n: u128) -> AtlasId {
        AtlasId(Uuid::from_u128(n))
    }

    /// Like [`cache_area`] but filed into `atlas`, so scope exclusion (which
    /// keys on `atlas_id`) has something to match.
    fn cache_area_in_atlas(
        id: AreaId,
        atlas: Option<AtlasId>,
        owned: bool,
        rooms: Vec<RoomWithDetails>,
    ) -> (AreaId, Arc<AreaCache>) {
        let details = AreaWithDetails {
            area: Area {
                id,
                user_id: None,
                atlas_id: atlas,
                name: format!("area {id}"),
                created_at: Utc::now(),
                rev: 1,
                access: Some(access(owned)),
                owner_nickname: (!owned).then(|| "friend".to_string()),
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                atlas_name: None,
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
            external_id: None,
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
                atlas_name: None,
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
    fn external_id_index_resolves_prefers_owned_and_skips_disabled() {
        let owned_id = area_id(1);
        let shared_id = area_id(2);

        let mut owned_room = room(1, "Gate", Vec::new());
        owned_room.external_id = Some("12345".to_string());
        let mut shared_room = room(7, "Gate (shared)", Vec::new());
        shared_room.external_id = Some("12345".to_string());
        let mut unique_room = room(8, "Vault", Vec::new());
        unique_room.external_id = Some("v-88".to_string());

        let mut areas = HashMap::new();
        let (id, cache) = cache_area(shared_id, false, vec![shared_room, unique_room]);
        areas.insert(id, cache);
        let (id, cache) = cache_area(owned_id, true, vec![owned_room]);
        areas.insert(id, cache);

        let atlas = atlas(areas.clone());

        // Duplicate binding: the owned area's room wins.
        let (key, room_cache) = atlas.find_room_by_external_id("12345").expect("resolves");
        assert_eq!(key.area_id, owned_id);
        assert_eq!(room_cache.get_external_id(), Some("12345"));

        // Unique binding resolves; unknown ids don't.
        let (key, _) = atlas.find_room_by_external_id("v-88").expect("resolves");
        assert_eq!(key, RoomKey::new(shared_id, RoomNumber(8)));
        assert!(atlas.find_room_by_external_id("nope").is_none());

        // Disabled areas drop out of resolution (external-id lookup is room
        // identification), falling back to the remaining binding.
        let atlas = atlas_with_disabled(areas, [owned_id]);
        let (key, _) = atlas.find_room_by_external_id("12345").expect("resolves");
        assert_eq!(key.area_id, shared_id);
    }

    #[test]
    fn elsewhere_external_id_resolves_only_scope_excluded_areas() {
        let here_atlas = atlas_id(10);
        let elsewhere_atlas = atlas_id(20);
        let here_id = area_id(1);
        let elsewhere_id = area_id(2);

        // The same server-global id is bound in a participating area (here) and
        // a scope-excluded area (homed on another entry).
        let mut here_room = room(1, "Temple", Vec::new());
        here_room.external_id = Some("shared-id".to_string());
        let mut elsewhere_room = room(5, "Temple", Vec::new());
        elsewhere_room.external_id = Some("shared-id".to_string());
        // An id that exists ONLY in the excluded atlas (the rescue case).
        let mut lonely = room(6, "Crypt", Vec::new());
        lonely.external_id = Some("only-elsewhere".to_string());

        let mut areas = HashMap::new();
        let (id, cache) =
            cache_area_in_atlas(here_id, Some(here_atlas), true, vec![here_room]);
        areas.insert(id, cache);
        let (id, cache) = cache_area_in_atlas(
            elsewhere_id,
            Some(elsewhere_atlas),
            true,
            vec![elsewhere_room, lonely],
        );
        areas.insert(id, cache);

        let atlas = atlas_with_scope(areas.clone(), [elsewhere_atlas], []);

        // Normal identification resolves the participating binding and never the
        // scope-excluded one.
        let (key, _) = atlas.find_room_by_external_id("shared-id").expect("resolves here");
        assert_eq!(key.area_id, here_id);

        // The rescue probe finds the excluded binding, with atlas context.
        let hit = atlas
            .find_room_elsewhere_by_external_id("only-elsewhere")
            .expect("rescue hit");
        assert_eq!(hit.room_key, RoomKey::new(elsewhere_id, RoomNumber(6)));
        assert_eq!(hit.atlas_id, Some(elsewhere_atlas));

        // The rescue index is a pure mirror over excluded areas, so it also
        // holds the excluded binding of an id that resolves here. That is inert:
        // the caller only ever consults the rescue path *after* normal
        // identification misses, and "shared-id" resolves here, so rescue is
        // never asked about it. Unknown ids resolve nowhere.
        assert_eq!(
            atlas
                .find_room_elsewhere_by_external_id("shared-id")
                .map(|hit| hit.room_key.area_id),
            Some(elsewhere_id)
        );
        assert!(atlas.find_room_elsewhere_by_external_id("nope").is_none());

        // With nothing scope-excluded, the rescue index is empty.
        let unscoped = atlas_with_scope(areas, [], []);
        assert!(
            unscoped
                .find_room_elsewhere_by_external_id("only-elsewhere")
                .is_none()
        );
    }

    #[test]
    fn external_id_binding_survives_room_upsert_and_updates() {
        let a = area_id(1);
        let mut bound = room(1, "Gate", Vec::new());
        bound.external_id = Some("42".to_string());
        let mut areas = HashMap::new();
        let (id, cache) = cache_area(a, true, vec![bound]);
        areas.insert(id, cache);
        let atlas = atlas(areas);

        // An unrelated update keeps the binding; the rebuilt index still resolves.
        let area = atlas.get_area(&a).expect("area");
        let updated = area.upsert_room(
            RoomNumber(1),
            crate::RoomUpdates {
                title: Some("Gatehouse".to_string()),
                ..Default::default()
            },
        );
        let atlas = atlas.insert_area(a, Arc::new(updated));
        let (key, room_cache) = atlas.find_room_by_external_id("42").expect("resolves");
        assert_eq!(key, RoomKey::new(a, RoomNumber(1)));
        assert_eq!(room_cache.get_title(), "Gatehouse");

        // Clearing via present-null removes it from the index.
        let area = atlas.get_area(&a).expect("area");
        let cleared = area.upsert_room(
            RoomNumber(1),
            crate::RoomUpdates {
                external_id: Some(None),
                ..Default::default()
            },
        );
        let atlas = atlas.insert_area(a, Arc::new(cleared));
        assert!(atlas.find_room_by_external_id("42").is_none());
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

    #[test]
    fn scope_excluded_atlas_rooms_absent_from_lookups_and_routing() {
        let kept_atlas = atlas_id(10);
        let dropped_atlas = atlas_id(20);
        let kept_id = area_id(1);
        let dropped_id = area_id(2);

        // Both areas have a room titled "Midgaard" (the stock-zone collision).
        let mut areas = HashMap::new();
        let (id, cache) =
            cache_area_in_atlas(kept_id, Some(kept_atlas), true, vec![room(1, "Midgaard", Vec::new())]);
        areas.insert(id, cache);
        let (id, cache) = cache_area_in_atlas(
            dropped_id,
            Some(dropped_atlas),
            true,
            vec![room(1, "Midgaard", Vec::new())],
        );
        areas.insert(id, cache);

        let atlas = atlas_with_scope(areas, [dropped_atlas], []);

        // Only the kept atlas's room appears in identification.
        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Midgaard")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![kept_id]);

        // The excluded area stays resident and explicitly addressable.
        assert!(atlas.get_area(&dropped_id).is_some());
        assert!(atlas.get_room(&RoomKey::new(dropped_id, RoomNumber(1))).is_some());

        // is_area_enabled reflects only the manual axis (both enabled), while
        // is_area_included honors the scope exclusion.
        assert!(atlas.is_area_enabled(&dropped_id), "manual axis untouched by scope");
        assert!(atlas.is_area_included(&kept_id));
        assert!(!atlas.is_area_included(&dropped_id));
    }

    #[test]
    fn scope_excluded_atlas_is_a_routing_wall() {
        let a_id = area_id(1);
        let b_id = area_id(2);
        let b_atlas = atlas_id(20);

        // A1 -> B10 -> A3 is the only route from A1 to A3; B is scope-excluded.
        let a_rooms = vec![
            room(1, "start", vec![exit(11, b_id, 10, 1.0)]),
            room(3, "goal", Vec::new()),
        ];
        let b_rooms = vec![room(10, "bridge", vec![exit(12, a_id, 3, 1.0)])];

        let mut areas = HashMap::new();
        let (id, cache) = cache_area_in_atlas(a_id, None, true, a_rooms);
        areas.insert(id, cache);
        let (id, cache) = cache_area_in_atlas(b_id, Some(b_atlas), true, b_rooms);
        areas.insert(id, cache);

        let atlas = atlas_with_scope(areas, [b_atlas], []);

        // Routing through the excluded atlas is refused.
        assert_eq!(
            atlas.get_path_between_rooms(
                &RoomKey::new(a_id, RoomNumber(1)),
                &RoomKey::new(a_id, RoomNumber(3)),
            ),
            None,
            "must not route through a scope-excluded atlas"
        );
        // But an explicitly named room in the excluded atlas is still reachable.
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
    }

    #[test]
    fn scope_excluded_atlas_less_area_drops_out() {
        let kept_id = area_id(1);
        let dropped_id = area_id(2);

        let mut areas = HashMap::new();
        let (id, cache) = cache_area_in_atlas(kept_id, None, true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);
        let (id, cache) =
            cache_area_in_atlas(dropped_id, None, true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);

        // The atlas-less area is excluded by its area id (the areas map row).
        let atlas = atlas_with_scope(areas, [], [dropped_id]);

        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Plaza")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![kept_id]);
        assert!(!atlas.is_area_included(&dropped_id));
    }

    #[test]
    fn manual_disable_and_scope_are_independent_axes() {
        let a_id = area_id(1);
        let a_atlas = atlas_id(10);

        let mut areas = HashMap::new();
        let (id, cache) = cache_area_in_atlas(a_id, Some(a_atlas), true, vec![room(1, "Plaza", Vec::new())]);
        areas.insert(id, cache);

        // Scope-excluded but NOT manually disabled: enabled (manual) stays true,
        // included (union) is false.
        let atlas = atlas_with_scope(areas.clone(), [a_atlas], []);
        assert!(atlas.is_area_enabled(&a_id), "scope exclusion is not the manual axis");
        assert!(!atlas.is_area_included(&a_id));
        assert!(atlas.disabled_areas().is_empty(), "manual set untouched by scope");

        // Manually disabled but NOT scope-excluded: enabled (manual) is false,
        // and it is likewise excluded from identification.
        let atlas = atlas_with_disabled(areas, [a_id]);
        assert!(!atlas.is_area_enabled(&a_id));
        assert!(!atlas.is_area_included(&a_id));
    }
}
