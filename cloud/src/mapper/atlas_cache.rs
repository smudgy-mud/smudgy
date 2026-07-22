use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::Arc,
};

use imbl::HashMap as PersistentMap;
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
/// sources**, folded into a single placement (`AreaPlacement::identified`) so
/// a scope-excluded area is invisible to identification exactly like a
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

/// How one area participates in the lookup tables, derived from the exclusion
/// axes and the area snapshot's own metadata. An area's contributions are
/// always removed under the placement computed from the same snapshot that
/// added them, so additions and removals cancel exactly.
#[derive(Clone, Copy, PartialEq, Eq)]
struct AreaPlacement {
    /// Participates in room identification (the four lookup tables and the
    /// external-id index): not manually disabled and not scope-excluded.
    identified: bool,
    /// Contributes to the cross-entry rescue index: scope-excluded. Manual
    /// disable alone does not rescue — it is the user's own toggle, not
    /// another entry's map.
    rescue: bool,
    /// Sorts into the owned prefix of every match list (own-beats-shared).
    owned: bool,
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

/// Every live room binding for one external id, rooms of owned areas first;
/// the head is the resolution winner. Keeping the non-winning bindings lets
/// resolution fall back correctly when the winner's area is later rewritten,
/// excluded, or unloaded — exactly what a from-scratch rebuild would produce.
type ExternalIdBindings = Vec<RoomKey>;

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

/// The atlas-wide room-identification state, maintained **area-scoped**: a
/// write that replaces one area's snapshot edits only that area's entries in
/// the lookup tables (skipping rooms whose `Arc` survived the rewrite), while
/// every other area's entries ride through the RCU clone untouched via the
/// persistent maps' structural sharing. Only operations that change an
/// exclusion axis — which re-place *every* area at once — rebuild the tables
/// from scratch; those are rare, user-initiated toggles.
///
/// The from-scratch build is itself the incremental insert applied to an empty
/// cache once per area, so the two paths cannot drift: any sequence of
/// area-level edits leaves the tables exactly as a full rebuild of the final
/// area set would.
#[derive(Clone)]
pub struct AtlasCache {
    areas: HashMap<AreaId, Arc<AreaCache>>,
    rooms_by_title_description_and_visible_exits:
        PersistentMap<(ExitBitfield, String), RoomMatches>,
    rooms_by_title_and_description: PersistentMap<String, RoomMatches>,
    rooms_by_title: PersistentMap<String, RoomMatches>,
    rooms_by_description: PersistentMap<String, RoomMatches>,
    /// Reverse index over rooms' server-global external ids (GMCP/MSDP room
    /// identity) — the mapper hot path for id → room resolution. Built like
    /// the other identification tables (excluded areas omitted). Uniqueness is
    /// not enforced; under duplicate bindings an owned area's room wins,
    /// otherwise the winner is unspecified but stable (documented
    /// best-effort). Resolution reads the head of the binding list.
    rooms_by_external_id: PersistentMap<String, ExternalIdBindings>,
    /// The external-id reverse index over the *scope-excluded* areas only — the
    /// mirror image of `rooms_by_external_id`, which omits them. This is the
    /// cross-entry rescue probe's index: when normal identification fails on a
    /// room, this answers "is it already mapped on another server entry?" in
    /// O(1). A second index (rather than a linear scan over excluded areas per
    /// probe) is the hot-path-honest choice: the auto-mapper consults the rescue
    /// path on *every* unmapped room while exploring, so an O(1) lookup against
    /// an incrementally-maintained table beats re-scanning a potentially large
    /// sibling map on each step. Manual-disable exclusions are excluded from
    /// this index — only per-server-scope exclusions rescue.
    rooms_by_external_id_excluded: PersistentMap<String, ExternalIdBindings>,
    /// Areas the viewer owns, for own-beats-shared precedence in lookups and
    /// routing. Maintained alongside the tables; lookups are O(1).
    owned_areas: HashSet<AreaId>,
    /// The manual-disable and per-server-scope exclusion sets. Excluded areas
    /// drop out of the room-identification lookup tables and are never routed
    /// *through* (still present in `areas` so explicit addressing keeps
    /// working). The sets are `Arc` so they ride through every write for free,
    /// and may contain ids not (yet) in `areas` — exclusion survives the area
    /// landing later.
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
        let mut cache = Self {
            areas: HashMap::with_capacity(areas.len()),
            rooms_by_title_description_and_visible_exits: PersistentMap::new(),
            rooms_by_title_and_description: PersistentMap::new(),
            rooms_by_title: PersistentMap::new(),
            rooms_by_description: PersistentMap::new(),
            rooms_by_external_id: PersistentMap::new(),
            rooms_by_external_id_excluded: PersistentMap::new(),
            owned_areas: HashSet::new(),
            exclusions,
        };
        for (area_id, area) in areas {
            cache.apply_insert(area_id, area);
        }
        cache
    }

    /// How `area` participates in the lookup tables under the current
    /// exclusion sets.
    fn placement(&self, area_id: &AreaId, area: &AreaCache) -> AreaPlacement {
        let rescue = self.exclusions.scope_excludes(area_id, area);
        AreaPlacement {
            identified: !rescue && !self.exclusions.disabled.contains(area_id),
            rescue,
            owned: area.is_owned(),
        }
    }

    /// Replaces (or adds) one area's snapshot, editing only the lookup-table
    /// entries the change touches. When the area's placement is unchanged,
    /// rooms whose `Arc` survived the rewrite are skipped entirely — the
    /// dominant case for a single-room edit. A placement flip (ownership,
    /// atlas membership) re-places the area's whole contribution, still
    /// O(that area), never O(atlas).
    fn apply_insert(&mut self, area_id: AreaId, area: Arc<AreaCache>) {
        let old = self.areas.get(&area_id).cloned();
        let new_placement = self.placement(&area_id, &area);

        match old {
            Some(ref old_area) if self.placement(&area_id, old_area) == new_placement => {
                for old_room in old_area.get_rooms() {
                    let survives = area
                        .get_room(&old_room.get_room_number())
                        .is_some_and(|new_room| Arc::ptr_eq(old_room, new_room));
                    if !survives {
                        self.remove_room(area_id, old_room, new_placement);
                    }
                }
                for new_room in area.get_rooms() {
                    let survives = old_area
                        .get_room(&new_room.get_room_number())
                        .is_some_and(|old_room| Arc::ptr_eq(old_room, new_room));
                    if !survives {
                        self.add_room(area_id, new_room, new_placement);
                    }
                }
            }
            _ => {
                if let Some(old_area) = old {
                    self.remove_area_contribution(area_id, &old_area);
                }
                if new_placement.owned {
                    self.owned_areas.insert(area_id);
                } else {
                    self.owned_areas.remove(&area_id);
                }
                for room in area.get_rooms() {
                    self.add_room(area_id, room, new_placement);
                }
            }
        }

        self.areas.insert(area_id, area);
    }

    /// Removes every table entry contributed by this snapshot of the area,
    /// under the placement that snapshot was added with.
    fn remove_area_contribution(&mut self, area_id: AreaId, area: &AreaCache) {
        let placement = self.placement(&area_id, area);
        for room in area.get_rooms() {
            self.remove_room(area_id, room, placement);
        }
    }

    fn add_room(&mut self, area_id: AreaId, room: &Arc<RoomCache>, placement: AreaPlacement) {
        if placement.identified {
            insert_match(
                &mut self.rooms_by_title_description_and_visible_exits,
                (
                    room.get_visible_exit_bitfield(),
                    room.get_title_and_description().to_string(),
                ),
                area_id,
                room,
                placement.owned,
                &self.owned_areas,
            );
            insert_match(
                &mut self.rooms_by_title_and_description,
                room.get_title_and_description().to_string(),
                area_id,
                room,
                placement.owned,
                &self.owned_areas,
            );
            insert_match(
                &mut self.rooms_by_title,
                room.get_title().to_string(),
                area_id,
                room,
                placement.owned,
                &self.owned_areas,
            );
            insert_match(
                &mut self.rooms_by_description,
                room.get_description().to_string(),
                area_id,
                room,
                placement.owned,
                &self.owned_areas,
            );
            if let Some(external_id) = room.get_external_id() {
                insert_binding(
                    &mut self.rooms_by_external_id,
                    external_id,
                    RoomKey::new(area_id, room.get_room_number()),
                    placement.owned,
                    &self.owned_areas,
                );
            }
        }
        if placement.rescue
            && let Some(external_id) = room.get_external_id()
        {
            insert_binding(
                &mut self.rooms_by_external_id_excluded,
                external_id,
                RoomKey::new(area_id, room.get_room_number()),
                placement.owned,
                &self.owned_areas,
            );
        }
    }

    fn remove_room(&mut self, area_id: AreaId, room: &Arc<RoomCache>, placement: AreaPlacement) {
        let room_number = room.get_room_number();
        if placement.identified {
            remove_match(
                &mut self.rooms_by_title_description_and_visible_exits,
                &(
                    room.get_visible_exit_bitfield(),
                    room.get_title_and_description().to_string(),
                ),
                area_id,
                room_number,
            );
            remove_match(
                &mut self.rooms_by_title_and_description,
                room.get_title_and_description(),
                area_id,
                room_number,
            );
            remove_match(
                &mut self.rooms_by_title,
                room.get_title(),
                area_id,
                room_number,
            );
            remove_match(
                &mut self.rooms_by_description,
                room.get_description(),
                area_id,
                room_number,
            );
            if let Some(external_id) = room.get_external_id() {
                remove_binding(
                    &mut self.rooms_by_external_id,
                    external_id,
                    &RoomKey::new(area_id, room_number),
                );
            }
        }
        if placement.rescue
            && let Some(external_id) = room.get_external_id()
        {
            remove_binding(
                &mut self.rooms_by_external_id_excluded,
                external_id,
                &RoomKey::new(area_id, room_number),
            );
        }
    }

    /// Resolve a server-global external id to its bound room. O(1); excluded
    /// areas are omitted (external-id resolution is room identification).
    #[must_use]
    pub fn find_room_by_external_id(&self, external_id: &str) -> Option<(RoomKey, Arc<RoomCache>)> {
        let key = self.rooms_by_external_id.get(external_id)?.first()?;
        self.get_room(key).map(|room| (key.clone(), room))
    }

    /// Cross-entry rescue probe: resolve a server-global external id against the
    /// *scope-excluded* areas only — maps the user has homed on a different
    /// server entry, deliberately absent from normal identification. Returns the
    /// matched room plus its atlas id and name (for the "shown on …" offer), or
    /// `None`. This never touches the normal lookup tables' semantics: it reads
    /// a separate index and the excluded areas stay resident (explicitly
    /// addressable) exactly as before.
    #[must_use]
    pub fn find_room_elsewhere_by_external_id(&self, external_id: &str) -> Option<ElsewhereMatch> {
        let key = self
            .rooms_by_external_id_excluded
            .get(external_id)?
            .first()?
            .clone();
        let meta = self.areas.get(&key.area_id).map(|area| area.meta());
        Some(ElsewhereMatch {
            room_key: key,
            atlas_id: meta.and_then(|m| m.atlas_id),
            atlas_name: meta.and_then(|m| m.atlas_name.clone()),
        })
    }

    #[must_use]
    pub(super) fn add_area(&self, area_id: AreaId, area: Arc<AreaCache>) -> Self {
        self.insert_area(area_id, area)
    }

    #[must_use]
    pub(super) fn insert_area(&self, area_id: AreaId, area: Arc<AreaCache>) -> Self {
        let mut next = self.clone();
        next.apply_insert(area_id, area);
        next
    }

    /// Replaces several areas in one pass. Equivalent to chained
    /// [`Self::insert_area`] calls; each area's entries are edited in place,
    /// so the cost is the touched areas' sizes, not the atlas's.
    #[must_use]
    pub(super) fn with_areas_updated(
        &self,
        updates: impl IntoIterator<Item = (AreaId, Arc<AreaCache>)>,
    ) -> Self {
        let mut next = self.clone();
        for (area_id, area) in updates {
            next.apply_insert(area_id, area);
        }
        next
    }

    #[must_use]
    pub(super) fn delete_area(&self, area_id: AreaId) -> Self {
        let mut next = self.clone();
        if let Some(area) = next.areas.remove(&area_id) {
            next.remove_area_contribution(area_id, &area);
            next.owned_areas.remove(&area_id);
        }
        next
    }

    /// Same areas, different manual-disable set — scope exclusions preserved.
    /// An exclusion change re-places every area at once, so this is the full
    /// from-scratch rebuild; toggles are rare.
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
    /// disable axis preserved. An exclusion change re-places every area at
    /// once, so this is the full from-scratch rebuild; scope changes are rare.
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
            .get(&(visible_exit_bitfield, format!("{title}\r\n{description}")))
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

    /// Resolves a room through its area's per-area table (rooms of excluded
    /// areas stay addressable). Two O(1) probes; there is no flat atlas-wide
    /// room table to maintain.
    #[must_use]
    pub fn get_room(&self, room_key: &RoomKey) -> Option<Arc<RoomCache>> {
        self.areas
            .get(&room_key.area_id)?
            .get_room(&room_key.room_number)
            .cloned()
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
                                let penalized = OrderedFloat(weight.0 * SHARED_AREA_WEIGHT_PENALTY);
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
            |room_key| {
                self.get_room(room_key)
                    .is_some_and(|r| predicate(r.as_ref()))
            },
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
            |room_key| room_key.area_id == *target_area_id && self.get_room(room_key).is_some(),
        )
        .and_then(|(path, _cost)| path.last().cloned())
    }
}

/// Inserts one room entry under `key`, keeping the owned-area prefix intact:
/// an owned area's entry goes to the end of the owned prefix, a shared area's
/// to the end of the list. Relative order within each class is insertion
/// order — arbitrary but stable, the same contract the from-scratch build
/// provides.
fn insert_match<K>(
    table: &mut PersistentMap<K, RoomMatches>,
    key: K,
    area_id: AreaId,
    room: &Arc<RoomCache>,
    owned: bool,
    owned_areas: &HashSet<AreaId>,
) where
    K: Hash + Eq + Clone,
{
    let entry = (area_id, room.clone());
    if let Some(matches) = table.get_mut(&key) {
        if owned {
            let prefix = matches
                .iter()
                .take_while(|(id, _)| owned_areas.contains(id))
                .count();
            matches.insert(prefix, entry);
        } else {
            matches.push(entry);
        }
    } else {
        table.insert(key, vec![entry]);
    }
}

/// Removes the entry `(area_id, room_number)` from the match list under
/// `key`, dropping the key when its list empties.
fn remove_match<K, Q>(
    table: &mut PersistentMap<K, RoomMatches>,
    key: &Q,
    area_id: AreaId,
    room_number: crate::RoomNumber,
) where
    K: Hash + Eq + Clone + Borrow<Q>,
    Q: Hash + Eq + ?Sized,
{
    let Some(matches) = table.get_mut(key) else {
        return;
    };
    matches.retain(|(id, room)| !(*id == area_id && room.get_room_number() == room_number));
    if matches.is_empty() {
        table.remove(key);
    }
}

/// Inserts one external-id binding, keeping the owned-area prefix intact so
/// the head of the list is the own-beats-shared resolution winner.
fn insert_binding(
    table: &mut PersistentMap<String, ExternalIdBindings>,
    external_id: &str,
    room_key: RoomKey,
    owned: bool,
    owned_areas: &HashSet<AreaId>,
) {
    if let Some(bindings) = table.get_mut(external_id) {
        if owned {
            let prefix = bindings
                .iter()
                .take_while(|key| owned_areas.contains(&key.area_id))
                .count();
            bindings.insert(prefix, room_key);
        } else {
            bindings.push(room_key);
        }
    } else {
        table.insert(external_id.to_string(), vec![room_key]);
    }
}

/// Removes one external-id binding, dropping the id when its list empties.
fn remove_binding(
    table: &mut PersistentMap<String, ExternalIdBindings>,
    external_id: &str,
    room_key: &RoomKey,
) {
    let Some(bindings) = table.get_mut(external_id) else {
        return;
    };
    bindings.retain(|key| key != room_key);
    if bindings.is_empty() {
        table.remove(external_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Area, AreaAccess, AreaWithDetails, Exit, ExitId, RoomNumber, RoomWithDetails, Uuid,
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
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
            properties: Vec::new(),
            rooms,
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
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
            connection_id: crate::ConnectionId::new(),
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
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
            properties: Vec::new(),
            rooms,
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
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
        let (id, cache) = cache_area_in_atlas(here_id, Some(here_atlas), true, vec![here_room]);
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
        let (key, _) = atlas
            .find_room_by_external_id("shared-id")
            .expect("resolves here");
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
                vec![exit(11, owned_id, 2, 1.0), exit(12, shared_id, 10, 1.0)],
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
        let (id, cache) = cache_area_in_atlas(
            kept_id,
            Some(kept_atlas),
            true,
            vec![room(1, "Midgaard", Vec::new())],
        );
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
        assert!(
            atlas
                .get_room(&RoomKey::new(dropped_id, RoomNumber(1)))
                .is_some()
        );

        // is_area_enabled reflects only the manual axis (both enabled), while
        // is_area_included honors the scope exclusion.
        assert!(
            atlas.is_area_enabled(&dropped_id),
            "manual axis untouched by scope"
        );
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
        let (id, cache) =
            cache_area_in_atlas(kept_id, None, true, vec![room(1, "Plaza", Vec::new())]);
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
        let (id, cache) = cache_area_in_atlas(
            a_id,
            Some(a_atlas),
            true,
            vec![room(1, "Plaza", Vec::new())],
        );
        areas.insert(id, cache);

        // Scope-excluded but NOT manually disabled: enabled (manual) stays true,
        // included (union) is false.
        let atlas = atlas_with_scope(areas.clone(), [a_atlas], []);
        assert!(
            atlas.is_area_enabled(&a_id),
            "scope exclusion is not the manual axis"
        );
        assert!(!atlas.is_area_included(&a_id));
        assert!(
            atlas.disabled_areas().is_empty(),
            "manual set untouched by scope"
        );

        // Manually disabled but NOT scope-excluded: enabled (manual) is false,
        // and it is likewise excluded from identification.
        let atlas = atlas_with_disabled(areas, [a_id]);
        assert!(!atlas.is_area_enabled(&a_id));
        assert!(!atlas.is_area_included(&a_id));
    }

    #[test]
    fn external_id_fallback_when_winning_binding_leaves() {
        let owned_id = area_id(1);
        let shared_id = area_id(2);

        let mut owned_room = room(1, "Gate", Vec::new());
        owned_room.external_id = Some("dup".to_string());
        let mut shared_room = room(7, "Gate", Vec::new());
        shared_room.external_id = Some("dup".to_string());

        let mut areas = HashMap::new();
        let (id, cache) = cache_area(owned_id, true, vec![owned_room]);
        areas.insert(id, cache);
        let (id, cache) = cache_area(shared_id, false, vec![shared_room]);
        areas.insert(id, cache);
        let atlas = atlas(areas);

        let (key, _) = atlas.find_room_by_external_id("dup").expect("resolves");
        assert_eq!(key.area_id, owned_id, "owned binding wins");

        // Clearing the winning binding falls back to the surviving duplicate,
        // exactly as a from-scratch rebuild of the same areas would resolve.
        let area = atlas.get_area(&owned_id).expect("area");
        let cleared = area.upsert_room(
            RoomNumber(1),
            crate::RoomUpdates {
                external_id: Some(None),
                ..Default::default()
            },
        );
        let after_clear = atlas.insert_area(owned_id, Arc::new(cleared));
        let (key, _) = after_clear
            .find_room_by_external_id("dup")
            .expect("falls back");
        assert_eq!(key.area_id, shared_id);

        // Deleting the winning area falls back the same way.
        let after_delete = atlas.delete_area(owned_id);
        let (key, _) = after_delete
            .find_room_by_external_id("dup")
            .expect("falls back");
        assert_eq!(key.area_id, shared_id);
    }

    #[test]
    fn ownership_flip_reorders_lookups_and_rebinds_winners() {
        let a_id = area_id(1);
        let b_id = area_id(2);

        let mut room_a = room(1, "Plaza", Vec::new());
        room_a.external_id = Some("x1".to_string());
        let mut room_b = room(2, "Plaza", Vec::new());
        room_b.external_id = Some("x1".to_string());

        let mut areas = HashMap::new();
        let (id, cache) = cache_area(a_id, true, vec![room_a]);
        areas.insert(id, cache);
        let (id, cache) = cache_area(b_id, false, vec![room_b.clone()]);
        areas.insert(id, cache);
        let atlas = atlas(areas);

        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Plaza")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title, vec![a_id, b_id]);
        let (key, _) = atlas.find_room_by_external_id("x1").expect("resolves");
        assert_eq!(key.area_id, a_id);

        // B re-lands as owned (an access upgrade through a sync refetch): its
        // whole contribution re-places, so it joins the owned prefix and ties
        // for the binding among owned areas; A must no longer beat it merely
        // by having been first.
        let (_, promoted) = cache_area(b_id, true, vec![room_b]);
        let atlas = atlas.insert_area(b_id, promoted);
        assert!(atlas.is_area_owned(&b_id));
        let by_title: Vec<AreaId> = atlas
            .get_rooms_by_title("Plaza")
            .map(|(area_id, _)| area_id)
            .collect();
        assert_eq!(by_title.len(), 2, "both areas still resolve");
        let (key, _) = atlas.find_room_by_external_id("x1").expect("resolves");
        assert!(
            atlas.is_area_owned(&key.area_id),
            "the winner is an owned binding after the flip"
        );
    }

    #[test]
    fn incremental_edits_match_a_full_rebuild() {
        let a_id = area_id(1);
        let b_id = area_id(2);
        let c_id = area_id(3);

        let mut a1 = room(1, "Alpha", Vec::new());
        a1.external_id = Some("e1".to_string());
        let a2 = room(2, "Beta", Vec::new());
        let mut b1 = room(1, "Alpha", Vec::new());
        b1.external_id = Some("e1".to_string());
        let mut b3 = room(3, "Gamma", Vec::new());
        b3.external_id = Some("e3".to_string());

        // Drive a sequence of area-level edits...
        let atlas_incremental = atlas(HashMap::new());
        let (_, cache_a) = cache_area(a_id, true, vec![a1, a2]);
        let atlas_incremental = atlas_incremental.insert_area(a_id, cache_a);
        let (_, cache_b) = cache_area(b_id, false, vec![b1, b3]);
        let atlas_incremental = atlas_incremental.insert_area(b_id, cache_b);
        let (_, cache_c) = cache_area(c_id, true, vec![room(9, "Delta", Vec::new())]);
        let atlas_incremental = atlas_incremental.add_area(c_id, cache_c);
        // ...including a retitle, a room deletion, and an area deletion.
        let area_a = atlas_incremental.get_area(&a_id).expect("area a");
        let retitled = area_a.upsert_room(
            RoomNumber(2),
            crate::RoomUpdates {
                title: Some("Beta Prime".to_string()),
                ..Default::default()
            },
        );
        let atlas_incremental = atlas_incremental.insert_area(a_id, Arc::new(retitled));
        let area_b = atlas_incremental.get_area(&b_id).expect("area b");
        let shrunk = area_b.delete_room(RoomNumber(3));
        let atlas_incremental = atlas_incremental.insert_area(b_id, Arc::new(shrunk));
        let atlas_incremental = atlas_incremental.delete_area(c_id);

        // ...and compare every observable lookup against a from-scratch build
        // of the same final area set.
        let final_areas: HashMap<AreaId, Arc<AreaCache>> = [a_id, b_id]
            .into_iter()
            .map(|id| (id, atlas_incremental.get_area(&id).expect("resident")))
            .collect();
        let atlas_rebuilt = atlas(final_areas);

        for title in ["Alpha", "Beta", "Beta Prime", "Gamma", "Delta"] {
            let mut incremental: Vec<(AreaId, RoomNumber)> = atlas_incremental
                .get_rooms_by_title(title)
                .map(|(area_id, room)| (area_id, room.get_room_number()))
                .collect();
            let mut rebuilt: Vec<(AreaId, RoomNumber)> = atlas_rebuilt
                .get_rooms_by_title(title)
                .map(|(area_id, room)| (area_id, room.get_room_number()))
                .collect();
            // Owned entries lead in both; order within a class is unspecified,
            // so compare as sets after checking the prefix.
            let owned_prefix_holds = |atlas: &AtlasCache, rooms: &[(AreaId, RoomNumber)]| {
                let first_shared = rooms
                    .iter()
                    .position(|(area_id, _)| !atlas.is_area_owned(area_id));
                first_shared.is_none_or(|pos| {
                    rooms[pos..]
                        .iter()
                        .all(|(area_id, _)| !atlas.is_area_owned(area_id))
                })
            };
            assert!(owned_prefix_holds(&atlas_incremental, &incremental));
            assert!(owned_prefix_holds(&atlas_rebuilt, &rebuilt));
            incremental.sort_by_key(|(area_id, number)| (area_id.0, number.0));
            rebuilt.sort_by_key(|(area_id, number)| (area_id.0, number.0));
            assert_eq!(incremental, rebuilt, "title {title:?}");
        }

        for external_id in ["e1", "e3"] {
            let incremental = atlas_incremental.find_room_by_external_id(external_id);
            let rebuilt = atlas_rebuilt.find_room_by_external_id(external_id);
            assert_eq!(
                incremental.is_some(),
                rebuilt.is_some(),
                "external id {external_id:?} resolvability"
            );
            if let (Some((incr_key, _)), Some((reb_key, _))) = (incremental, rebuilt) {
                assert_eq!(
                    atlas_incremental.is_area_owned(&incr_key.area_id),
                    atlas_rebuilt.is_area_owned(&reb_key.area_id),
                    "external id {external_id:?} winner class"
                );
            }
        }
        assert!(atlas_incremental.find_room_by_external_id("e3").is_none());
        assert!(atlas_incremental.get_area(&c_id).is_none());
        assert!(
            atlas_incremental
                .get_room(&RoomKey::new(b_id, RoomNumber(3)))
                .is_none()
        );
    }
}
