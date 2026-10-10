use std::{
    borrow::Borrow,
    collections::{HashMap, HashSet},
    hash::Hash,
    sync::Arc,
};

use imbl::{HashMap as PersistentMap, OrdMap};
use ordered_float::OrderedFloat;

use crate::{
    AreaId, AtlasId, ExitDirection, RoomNumber, SourceId, Uuid,
    mapper::{
        RoomKey,
        area_cache::AreaCache,
        exit_cache::ExitCache,
        places::Sources,
        room_cache::{ExitBitfield, RoomCache},
    },
};

/// The exits a source keeps on map rooms, by map room.
type AnchoredExits = Arc<HashMap<RoomKey, Vec<ExitCache>>>;

/// One of a map's Secrets, or the caller's Private additions, as an area of
/// its own: the map it belongs to, its source, and the area and anchored
/// exits its layer holds (shared with the map's cache, so an unchanged
/// rebuild is told by pointer).
#[derive(Clone)]
struct SourceArea {
    map: AreaId,
    source: SourceId,
    area: Arc<AreaCache>,
    anchored: AnchoredExits,
}

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

/// The areas one area-lookup key resolves to, owned areas first. Areas are
/// held by id rather than by handle: the atlas already owns the only
/// authoritative snapshot of each, and an id cannot go stale against it.
type AreaMatches = Vec<AreaId>;

static EMPTY_ROOMS_LOOKUP_VEC: RoomMatches = Vec::new();
static EMPTY_AREAS_LOOKUP_VEC: AreaMatches = Vec::new();

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
/// The from-scratch build is the incremental per-room insert applied to an
/// empty cache once per area, so the two paths cannot drift: any sequence of
/// area-level edits leaves the tables exactly as a full rebuild of the final
/// area set would. Only the exit-target counts are gathered in one sorted pass
/// instead, since they are the same sums whichever order the exits arrive in.
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
    /// Reverse index over rooms' properties by name alone — "which rooms carry
    /// this property at all". Maintained exactly like the title tables, and
    /// excluded areas are omitted from it for the same reason.
    rooms_by_property_name: PersistentMap<String, RoomMatches>,
    /// Reverse index over rooms' properties by name *and* value. The pair to
    /// `rooms_by_property_name`, on the model of `rooms_by_title` /
    /// `rooms_by_title_and_description`: both questions answer in one probe
    /// rather than one probe and a scan.
    ///
    /// Values are unbounded, so this table costs a bucket per distinct
    /// (name, value). A property carrying a per-room identity therefore spends
    /// a bucket per room — which is the case that most wants the O(1) lookup,
    /// and the one packages have been hand-rolling their own indexes to get.
    rooms_by_property_name_and_value: PersistentMap<(String, String), RoomMatches>,
    /// Reverse index over rooms' tags, keyed in the normalized (uppercase)
    /// spelling so lookup is case-insensitive like [`RoomCache::has_tag`].
    /// Distinct from the nearest-room searches, which walk the graph in
    /// distance order and need no tag index; this one answers "all of them,
    /// anywhere" without a traversal.
    rooms_by_tag: PersistentMap<String, RoomMatches>,
    /// The two area-property tables, mirroring the room pair above over areas'
    /// own properties. An area contributes to these on the same placement
    /// terms as its rooms contribute to the room tables.
    areas_by_property_name: PersistentMap<String, AreaMatches>,
    areas_by_property_name_and_value: PersistentMap<(String, String), AreaMatches>,
    /// Every room an exit in another area leads to, keyed by the area it
    /// leads into and the room number, with how many such exits lead there;
    /// the number need not belong to a room. Deleting rooms reads it to find
    /// the other areas' exits to clear, and merges and pastes keep a room
    /// off a vacant number one still names (see
    /// [`Self::vacant_exit_targets`]). A room that goes clears every exit
    /// into it, here and at the server (map wire format 3 §4.2), so plain
    /// allocation never consults it. An area's exits into its own rooms are
    /// read from the area itself when a number is wanted: they are most of a
    /// map's exits, and the area's own edits keep them leading to rooms that
    /// exist. Every area's exits count, turned off or not, so an exclusion
    /// change carries the index over as it is. Maintained per room like the
    /// tables above, counted in one pass when the tables are built from
    /// scratch, and ordered so every question is a range read.
    exit_targets: OrdMap<(Uuid, RoomNumber), u32>,
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
    /// Every map's Secrets and the caller's Private additions, each an area
    /// of its own, by area id. Each is placed as its map is (turned off,
    /// out of scope and owned with it) and is replaced and dropped with its
    /// map. `areas` holds maps only.
    source_areas: HashMap<AreaId, SourceArea>,
    /// Every area that has been a map's Secret or Private additions in this
    /// atlas or any atlas it was derived from, with its map. It only grows:
    /// a Secret that leaves (revoked, deleted, or purged with its map until a
    /// refetch) keeps reading as a place to callers that hide places, so an
    /// id still held for it never shows as an ordinary area.
    places_ever: PersistentMap<AreaId, AreaId>,
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
        let exit_targets = count_exit_targets(&areas);
        Self::new_with_exit_targets(areas, exclusions, exit_targets)
    }

    /// Builds every table for `areas` from scratch, over `exit_targets`, the
    /// exit-target counts of exactly those areas.
    fn new_with_exit_targets(
        areas: HashMap<AreaId, Arc<AreaCache>>,
        exclusions: Exclusions,
        exit_targets: OrdMap<(Uuid, RoomNumber), u32>,
    ) -> Self {
        let mut cache = Self {
            areas: HashMap::with_capacity(areas.len()),
            rooms_by_title_description_and_visible_exits: PersistentMap::new(),
            rooms_by_title_and_description: PersistentMap::new(),
            rooms_by_title: PersistentMap::new(),
            rooms_by_description: PersistentMap::new(),
            rooms_by_external_id: PersistentMap::new(),
            rooms_by_external_id_excluded: PersistentMap::new(),
            rooms_by_property_name: PersistentMap::new(),
            rooms_by_property_name_and_value: PersistentMap::new(),
            rooms_by_tag: PersistentMap::new(),
            areas_by_property_name: PersistentMap::new(),
            areas_by_property_name_and_value: PersistentMap::new(),
            exit_targets,
            owned_areas: HashSet::new(),
            exclusions,
            source_areas: HashMap::new(),
            places_ever: PersistentMap::new(),
        };
        for (area_id, area) in areas {
            let placement = cache.placement(&area_id, &area);
            if placement.owned {
                cache.owned_areas.insert(area_id);
            }
            for room in area.get_rooms() {
                cache.index_room(area_id, room, placement);
            }
            cache.add_area_properties(area_id, &area, placement);
            for layer in area.source_layers() {
                let id = layer.area_id();
                if placement.owned {
                    cache.owned_areas.insert(id);
                }
                for room in layer.area().get_rooms() {
                    cache.index_room(id, room, placement);
                }
                cache.places_ever.insert(id, area_id);
                cache
                    .source_areas
                    .insert(id, SourceArea::of(area_id, layer));
            }
            cache.areas.insert(area_id, area);
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
        let old_placement = old
            .as_ref()
            .map(|old_area| self.placement(&area_id, old_area));

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
                // The area's own properties ride the same snapshot as its
                // rooms. Comparing before rewriting keeps the common edit --
                // one room, properties untouched -- from churning the area
                // tables at all.
                if old_area.property_entries() != area.property_entries() {
                    self.remove_area_properties(area_id, old_area, new_placement);
                    self.add_area_properties(area_id, &area, new_placement);
                }
            }
            _ => {
                if let Some(old_area) = &old {
                    self.remove_area_contribution(area_id, old_area);
                }
                if new_placement.owned {
                    self.owned_areas.insert(area_id);
                } else {
                    self.owned_areas.remove(&area_id);
                }
                for room in area.get_rooms() {
                    self.add_room(area_id, room, new_placement);
                }
                self.add_area_properties(area_id, &area, new_placement);
            }
        }

        self.sync_source_areas(area_id, old.as_deref(), old_placement, &area, new_placement);
        self.areas.insert(area_id, area);
    }

    /// Brings map `map_id`'s source areas in line with `new_map`, its new
    /// snapshot, placed `new_placement`, replacing `old_map`, placed
    /// `old_placement`. A source whose area and anchored exits survived the
    /// rebuild under an unchanged placement costs nothing; a rebuilt one
    /// has only its changed rooms re-indexed.
    fn sync_source_areas(
        &mut self,
        map_id: AreaId,
        old_map: Option<&AreaCache>,
        old_placement: Option<AreaPlacement>,
        new_map: &AreaCache,
        new_placement: AreaPlacement,
    ) {
        if let Some(layer) = old_map.and_then(AreaCache::map_document_layer) {
            self.count_anchored_targets(layer.anchored_shared(), false);
        }
        if let Some(layer) = new_map.map_document_layer() {
            self.count_anchored_targets(layer.anchored_shared(), true);
        }
        if let (Some(old_map), Some(old_placement)) = (old_map, old_placement) {
            for layer in old_map.source_layers() {
                let id = layer.area_id();
                if old_placement != new_placement || new_map.source_layer(&id).is_none() {
                    self.remove_source_area(id, old_placement);
                }
            }
        }
        for layer in new_map.source_layers() {
            let id = layer.area_id();
            let next = SourceArea::of(map_id, layer);
            match self.source_areas.get(&id).cloned() {
                Some(current) if current.map == map_id => {
                    self.replace_source_area(id, &current, next, new_placement);
                }
                Some(_) => {
                    // An area id held by another map: the later map wins.
                    let placement = self.source_placement(&id);
                    if let Some(placement) = placement {
                        self.remove_source_area(id, placement);
                    }
                    self.add_source_area(id, next, new_placement);
                }
                None => self.add_source_area(id, next, new_placement),
            }
        }
    }

    /// The placement `area_id`, a source area, was indexed under: its map's.
    fn source_placement(&self, area_id: &AreaId) -> Option<AreaPlacement> {
        let entry = self.source_areas.get(area_id)?;
        let map = self.areas.get(&entry.map)?;
        Some(self.placement(&entry.map, map))
    }

    fn add_source_area(&mut self, id: AreaId, entry: SourceArea, placement: AreaPlacement) {
        if placement.owned {
            self.owned_areas.insert(id);
        } else {
            self.owned_areas.remove(&id);
        }
        for room in entry.area.get_rooms() {
            self.add_room(id, room, placement);
        }
        self.count_anchored_targets(&entry.anchored, true);
        self.places_ever.insert(id, entry.map);
        self.source_areas.insert(id, entry);
    }

    fn remove_source_area(&mut self, id: AreaId, placement: AreaPlacement) {
        let Some(entry) = self.source_areas.remove(&id) else {
            return;
        };
        for room in entry.area.get_rooms() {
            self.remove_room(id, room, placement);
        }
        self.count_anchored_targets(&entry.anchored, false);
        self.owned_areas.remove(&id);
    }

    /// Replaces a source area held under an unchanged placement, editing
    /// only the entries of rooms whose `Arc` did not survive.
    fn replace_source_area(
        &mut self,
        id: AreaId,
        current: &SourceArea,
        next: SourceArea,
        placement: AreaPlacement,
    ) {
        if !Arc::ptr_eq(&current.area, &next.area) {
            for old_room in current.area.get_rooms() {
                let survives = next
                    .area
                    .get_room(&old_room.get_room_number())
                    .is_some_and(|new_room| Arc::ptr_eq(old_room, new_room));
                if !survives {
                    self.remove_room(id, old_room, placement);
                }
            }
            for new_room in next.area.get_rooms() {
                let survives = current
                    .area
                    .get_room(&new_room.get_room_number())
                    .is_some_and(|old_room| Arc::ptr_eq(old_room, new_room));
                if !survives {
                    self.add_room(id, new_room, placement);
                }
            }
        }
        if !Arc::ptr_eq(&current.anchored, &next.anchored) {
            self.count_anchored_targets(&current.anchored, false);
            self.count_anchored_targets(&next.anchored, true);
        }
        self.source_areas.insert(id, next);
    }

    /// Counts (or uncounts) where a source's anchored exits lead in the
    /// exit targets. They leave map rooms, so every destination counts,
    /// the source's own rooms included.
    fn count_anchored_targets(&mut self, anchored: &AnchoredExits, add: bool) {
        for key in anchored_target_keys(anchored) {
            if add {
                *self.exit_targets.entry(key).or_insert(0) += 1;
            } else if let Some(count) = self.exit_targets.get_mut(&key) {
                *count -= 1;
                if *count == 0 {
                    self.exit_targets.remove(&key);
                }
            }
        }
    }

    /// Removes every table entry contributed by this snapshot of the area,
    /// under the placement that snapshot was added with.
    fn remove_area_contribution(&mut self, area_id: AreaId, area: &AreaCache) {
        let placement = self.placement(&area_id, area);
        for room in area.get_rooms() {
            self.remove_room(area_id, room, placement);
        }
        self.remove_area_properties(area_id, area, placement);
    }

    /// Adds this snapshot of `area` to the area-property tables. Excluded
    /// areas contribute nothing, matching how their rooms stay out of the room
    /// tables: an atlas-wide search covers the active set, while explicit
    /// addressing (`get_area`, and every per-area lookup) keeps reaching them.
    fn add_area_properties(&mut self, area_id: AreaId, area: &AreaCache, placement: AreaPlacement) {
        if !placement.identified {
            return;
        }
        for (name, entry) in area.property_entries() {
            insert_area_match(
                &mut self.areas_by_property_name,
                name.clone(),
                area_id,
                placement.owned,
                &self.owned_areas,
            );
            insert_area_match(
                &mut self.areas_by_property_name_and_value,
                (name.clone(), entry.value.clone()),
                area_id,
                placement.owned,
                &self.owned_areas,
            );
        }
    }

    /// Removes the area-property entries this snapshot of `area` contributed
    /// under `placement`.
    fn remove_area_properties(
        &mut self,
        area_id: AreaId,
        area: &AreaCache,
        placement: AreaPlacement,
    ) {
        if !placement.identified {
            return;
        }
        for (name, entry) in area.property_entries() {
            remove_area_match(&mut self.areas_by_property_name, name, area_id);
            remove_area_match(
                &mut self.areas_by_property_name_and_value,
                &(name.clone(), entry.value.clone()),
                area_id,
            );
        }
    }

    /// The rooms in other areas that exits of `room`, a room of `area_id`,
    /// lead to, as [`Self::exit_targets`] keys.
    fn exit_target_keys(
        area_id: AreaId,
        room: &RoomCache,
    ) -> impl Iterator<Item = (Uuid, RoomNumber)> + '_ {
        room.get_exits().iter().filter_map(move |exit| {
            exit.to_area_id
                .filter(|to_area| *to_area != area_id)
                .zip(exit.to_room_number)
                .map(|(to_area, number)| (to_area.0, number))
        })
    }

    fn add_room(&mut self, area_id: AreaId, room: &Arc<RoomCache>, placement: AreaPlacement) {
        // An exit is real in an area the viewer has turned off too, so the
        // exit targets ignore placement.
        for key in Self::exit_target_keys(area_id, room) {
            *self.exit_targets.entry(key).or_insert(0) += 1;
        }
        self.index_room(area_id, room, placement);
    }

    /// Adds `room` to every lookup table its placement admits it to; the
    /// exit targets are the caller's.
    fn index_room(&mut self, area_id: AreaId, room: &Arc<RoomCache>, placement: AreaPlacement) {
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
            for (name, value) in room.properties() {
                insert_match(
                    &mut self.rooms_by_property_name,
                    name.to_owned(),
                    area_id,
                    room,
                    placement.owned,
                    &self.owned_areas,
                );
                insert_match(
                    &mut self.rooms_by_property_name_and_value,
                    (name.to_owned(), value.to_owned()),
                    area_id,
                    room,
                    placement.owned,
                    &self.owned_areas,
                );
            }
            for tag in room.tags() {
                insert_match(
                    &mut self.rooms_by_tag,
                    tag.to_owned(),
                    area_id,
                    room,
                    placement.owned,
                    &self.owned_areas,
                );
            }
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
        for key in Self::exit_target_keys(area_id, room) {
            if let Some(count) = self.exit_targets.get_mut(&key) {
                *count -= 1;
                if *count == 0 {
                    self.exit_targets.remove(&key);
                }
            }
        }
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
            for (name, value) in room.properties() {
                remove_match(&mut self.rooms_by_property_name, name, area_id, room_number);
                remove_match(
                    &mut self.rooms_by_property_name_and_value,
                    &(name.to_owned(), value.to_owned()),
                    area_id,
                    room_number,
                );
            }
            for tag in room.tags() {
                remove_match(&mut self.rooms_by_tag, tag, area_id, room_number);
            }
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
        self.find_room_by_external_id_with(external_id, Sources::Shown)
    }

    /// [`Self::find_room_by_external_id`] for a caller who sees `sources`:
    /// without Secrets and Private additions, the first binding outside them
    /// wins, as it would were they never there.
    #[must_use]
    pub fn find_room_by_external_id_with(
        &self,
        external_id: &str,
        sources: Sources,
    ) -> Option<(RoomKey, Arc<RoomCache>)> {
        let key = self
            .rooms_by_external_id
            .get(external_id)?
            .iter()
            .find(|key| self.sees(sources, &key.area_id))?;
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
        self.find_room_elsewhere_by_external_id_with(external_id, Sources::Shown)
    }

    /// [`Self::find_room_elsewhere_by_external_id`] for a caller who sees
    /// `sources`.
    #[must_use]
    pub fn find_room_elsewhere_by_external_id_with(
        &self,
        external_id: &str,
        sources: Sources,
    ) -> Option<ElsewhereMatch> {
        let key = self
            .rooms_by_external_id_excluded
            .get(external_id)?
            .iter()
            .find(|key| self.sees(sources, &key.area_id))?
            .clone();
        // A Secret's room is filed where its map is.
        let meta = self
            .areas
            .get(&self.placed_by(&key.area_id))
            .map(|area| area.meta());
        Some(ElsewhereMatch {
            room_key: key,
            atlas_id: meta.and_then(|m| m.atlas_id),
            atlas_name: meta.and_then(|m| m.atlas_name.clone()),
        })
    }

    /// Whether a caller who sees `sources` sees area `area_id` at all: a
    /// Secret's or Private additions' own area is invisible without them.
    #[must_use]
    pub fn sees(&self, sources: Sources, area_id: &AreaId) -> bool {
        sources == Sources::Shown || !self.source_areas.contains_key(area_id)
    }

    /// The rooms with this title and description whose visible exits leave
    /// by exactly `directions`, as a caller who sees `sources` reads them.
    /// Without Secrets, a room's exits into other maps' Secret rooms are no
    /// exits at all, so its visible exits are the rest.
    #[must_use]
    pub fn rooms_by_title_description_and_visible_exits_for(
        &self,
        title: &str,
        description: &str,
        directions: &[ExitDirection],
        sources: Sources,
    ) -> Vec<(AreaId, Arc<RoomCache>)> {
        if sources == Sources::Shown {
            return self
                .get_rooms_by_title_description_and_visible_exits(title, description, directions)
                .collect();
        }
        let wanted = ExitBitfield::from(directions);
        self.get_rooms_by_title_and_description(title, description)
            .filter(|(area_id, room)| {
                self.sees(sources, area_id)
                    && ExitBitfield::from(
                        room.get_exits()
                            .iter()
                            .filter(|exit| !exit.is_hidden)
                            .map(|exit| &exit.from_direction),
                    ) == wanted
            })
            .collect()
    }

    /// Whether the destination of an already readable exit may be followed.
    /// Exit content belongs to its own source; this check must not hide that
    /// content merely because its destination is unavailable.
    #[must_use]
    pub fn can_follow_exit(&self, sources: Sources, exit: &ExitCache) -> bool {
        if exit.to_unknown {
            return false;
        }
        if let Some(map) = exit.to_private_map {
            return sources == Sources::Shown
                && exit.to_area_id.and_then(|area| self.source_of(&area))
                    == Some((map, SourceId::Private));
        }
        match exit.foreign_secret() {
            Some((map, secret)) => {
                sources == Sources::Shown
                    && self
                        .source_of(&AreaId(secret))
                        .is_some_and(|(held, place)| held == map && place.is_secret())
            }
            None => exit.to_area_id.is_none_or(|to| self.sees(sources, &to)),
        }
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
        if let Some(area) = next.areas.get(&area_id).cloned() {
            let placement = next.placement(&area_id, &area);
            if let Some(layer) = area.map_document_layer() {
                next.count_anchored_targets(layer.anchored_shared(), false);
            }
            for layer in area.source_layers() {
                next.remove_source_area(layer.area_id(), placement);
            }
            next.areas.remove(&area_id);
            next.remove_area_contribution(area_id, &area);
            next.owned_areas.remove(&area_id);
        }
        next
    }

    /// Same areas, different manual-disable set — scope exclusions preserved.
    /// An exclusion change re-places every area at once, so this is the full
    /// from-scratch rebuild of the lookup tables; toggles are rare. The exit
    /// targets ignore placement and carry over as they are.
    #[must_use]
    pub(super) fn with_disabled_areas(&self, disabled_areas: Arc<HashSet<AreaId>>) -> Self {
        Self::new_with_exit_targets(
            self.areas.clone(),
            Exclusions {
                disabled: disabled_areas,
                ..self.exclusions.clone()
            },
            self.exit_targets.clone(),
        )
        .remembering_places_of(self)
    }

    /// Same areas, different per-server scope-exclusion sets — the manual
    /// disable axis preserved. An exclusion change re-places every area at
    /// once, so this is the full from-scratch rebuild of the lookup tables;
    /// scope changes are rare. The exit targets carry over as they are.
    #[must_use]
    pub(super) fn with_scope_exclusions(
        &self,
        excluded_atlases: Arc<HashSet<AtlasId>>,
        excluded_areas: Arc<HashSet<AreaId>>,
    ) -> Self {
        Self::new_with_exit_targets(
            self.areas.clone(),
            Exclusions {
                disabled: self.exclusions.disabled.clone(),
                atlases: excluded_atlases,
                areas: excluded_areas,
            },
            self.exit_targets.clone(),
        )
        .remembering_places_of(self)
    }

    /// This atlas, remembering every place `earlier` remembers as well.
    fn remembering_places_of(mut self, earlier: &Self) -> Self {
        self.places_ever = earlier.places_ever.clone().union(self.places_ever);
        self
    }

    /// Every map. A map's Secrets and Private additions are reached
    /// through [`Self::get_area`] and the map's source layers.
    #[must_use]
    pub fn areas(&self) -> impl ExactSizeIterator<Item = Arc<AreaCache>> {
        self.areas.values().cloned()
    }

    /// A map, or a Secret's or Private additions' own area, by id.
    #[must_use]
    pub fn get_area(&self, area_id: &AreaId) -> Option<Arc<AreaCache>> {
        self.area_ref(area_id).cloned()
    }

    fn area_ref(&self, area_id: &AreaId) -> Option<&Arc<AreaCache>> {
        self.areas
            .get(area_id)
            .or_else(|| self.source_areas.get(area_id).map(|entry| &entry.area))
    }

    /// The map `area_id` belongs to: itself for a map, its map for a
    /// Secret's or Private additions' area. `None` for an id the atlas does
    /// not hold.
    #[must_use]
    pub fn map_of(&self, area_id: &AreaId) -> Option<AreaId> {
        if self.areas.contains_key(area_id) {
            return Some(*area_id);
        }
        self.source_areas.get(area_id).map(|entry| entry.map)
    }

    /// The map of `area_id` when it is, or has been, a Secret's or Private
    /// additions' own area in this atlas or one it was derived from: such an
    /// area stays a place after it leaves the atlas. `None` for a map and
    /// for an id never seen as a place.
    #[must_use]
    pub fn place_map(&self, area_id: &AreaId) -> Option<AreaId> {
        self.source_areas
            .get(area_id)
            .map(|entry| entry.map)
            .or_else(|| self.places_ever.get(area_id).copied())
    }

    /// For a Secret's or Private additions' area, its map and its source.
    #[must_use]
    pub fn source_of(&self, area_id: &AreaId) -> Option<(AreaId, SourceId)> {
        self.source_areas
            .get(area_id)
            .map(|entry| (entry.map, entry.source))
    }

    /// Every map's Secrets and Private additions: each one's map, its
    /// source, and the area id it reads under.
    pub(crate) fn source_places(&self) -> impl Iterator<Item = (AreaId, SourceId, AreaId)> + '_ {
        self.source_areas
            .iter()
            .map(|(area_id, entry)| (entry.map, entry.source, *area_id))
    }

    /// `area_id`'s map when it is a source area, else `area_id` itself.
    fn placed_by(&self, area_id: &AreaId) -> AreaId {
        self.source_areas
            .get(area_id)
            .map_or(*area_id, |entry| entry.map)
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

    /// Every room in the atlas carrying a property named `name`. One probe.
    /// Excluded areas are omitted, like the other atlas-wide room lookups.
    #[must_use]
    pub fn get_rooms_with_property(
        &self,
        name: &str,
    ) -> impl ExactSizeIterator<Item = (AreaId, Arc<RoomCache>)> {
        self.rooms_by_property_name
            .get(name)
            .unwrap_or(&EMPTY_ROOMS_LOOKUP_VEC)
            .iter()
            .cloned()
    }

    /// Every room in the atlas whose `name` property holds exactly `value`.
    /// Name and value both match case-sensitively, on the same terms as
    /// [`RoomCache::get_property`]. One probe.
    #[must_use]
    pub fn get_rooms_by_property(
        &self,
        name: &str,
        value: &str,
    ) -> impl ExactSizeIterator<Item = (AreaId, Arc<RoomCache>)> {
        self.rooms_by_property_name_and_value
            .get(&(name.to_owned(), value.to_owned()))
            .unwrap_or(&EMPTY_ROOMS_LOOKUP_VEC)
            .iter()
            .cloned()
    }

    /// Every room in the atlas carrying `tag`, matched case-insensitively like
    /// [`RoomCache::has_tag`]. One probe, and no graph traversal -- unlike the
    /// nearest-room searches, which want distance rather than the whole set.
    #[must_use]
    pub fn get_rooms_with_tag(
        &self,
        tag: &str,
    ) -> impl ExactSizeIterator<Item = (AreaId, Arc<RoomCache>)> {
        self.rooms_by_tag
            .get(&crate::mapper::normalize_tag(tag))
            .unwrap_or(&EMPTY_ROOMS_LOOKUP_VEC)
            .iter()
            .cloned()
    }

    /// Every area carrying a property named `name`. One probe.
    #[must_use]
    pub fn get_areas_with_property(&self, name: &str) -> impl ExactSizeIterator<Item = AreaId> {
        self.areas_by_property_name
            .get(name)
            .unwrap_or(&EMPTY_AREAS_LOOKUP_VEC)
            .iter()
            .copied()
    }

    /// Every area whose `name` property holds exactly `value`. One probe.
    #[must_use]
    pub fn get_areas_by_property(
        &self,
        name: &str,
        value: &str,
    ) -> impl ExactSizeIterator<Item = AreaId> {
        self.areas_by_property_name_and_value
            .get(&(name.to_owned(), value.to_owned()))
            .unwrap_or(&EMPTY_AREAS_LOOKUP_VEC)
            .iter()
            .copied()
    }

    /// Resolves a room through its area's per-area table (rooms of excluded
    /// areas stay addressable). Two O(1) probes; there is no flat atlas-wide
    /// room table to maintain.
    #[must_use]
    pub fn get_room(&self, room_key: &RoomKey) -> Option<Arc<RoomCache>> {
        self.area_ref(&room_key.area_id)?
            .get_room(&room_key.room_number)
            .cloned()
    }

    /// One above the highest room of `area_id`, a map or one of a map's
    /// Secrets or Private additions: where allocation in it starts. Each
    /// place numbers its own rooms, so an empty one starts at 1. `None` when
    /// the atlas does not hold `area_id`.
    #[must_use]
    pub(crate) fn room_number_floor(&self, area_id: &AreaId) -> Option<i64> {
        self.area_ref(area_id).map(|area| area.room_number_floor())
    }

    /// The room numbers of `area_id` that an exit somewhere in the atlas
    /// leads to but no room of the area holds, ascending. A room that took
    /// one of them would silently become the destination of every such
    /// exit.
    #[must_use]
    pub fn vacant_exit_targets(&self, area_id: &AreaId) -> Vec<RoomNumber> {
        let Some(area) = self.area_ref(area_id) else {
            return Vec::new();
        };
        let mut vacant = self.numbers_named_in(area_id, RoomNumber(i32::MIN));
        vacant.retain(|number| area.get_room(number).is_none());
        vacant.sort_unstable();
        vacant.dedup();
        vacant
    }

    /// Whether an exit in another area leads to one of `numbers` in
    /// `area_id`.
    #[must_use]
    pub(crate) fn is_linked_from_elsewhere(
        &self,
        area_id: &AreaId,
        numbers: &HashSet<RoomNumber>,
    ) -> bool {
        numbers
            .iter()
            .any(|number| self.exit_targets.contains_key(&(area_id.0, *number)))
    }

    /// Every number of `area_id` at or above `from` that an exit leads to,
    /// unordered and possibly repeated: the other areas' exits from the
    /// index, the area's own exits from the area.
    fn numbers_named_in(&self, area_id: &AreaId, from: RoomNumber) -> Vec<RoomNumber> {
        let mut named: Vec<RoomNumber> = self
            .exit_targets
            .range((area_id.0, from)..=(area_id.0, RoomNumber(i32::MAX)))
            .map(|((_, number), _)| *number)
            .collect();
        if let Some(area) = self.area_ref(area_id) {
            named.extend(
                area.get_rooms()
                    .iter()
                    .flat_map(|room| room.get_exits())
                    .filter(|exit| exit.to_area_id == Some(*area_id))
                    .filter_map(|exit| exit.to_room_number)
                    .filter(|number| *number >= from),
            );
        }
        named
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
        !self.exclusions.disabled.contains(&self.placed_by(area_id))
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
        let map_id = self.placed_by(area_id);
        if self.exclusions.disabled.contains(&map_id) || self.exclusions.areas.contains(&map_id) {
            return true;
        }
        self.areas
            .get(&map_id)
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
        Self::new_with_exclusions(areas, self.exclusions.clone()).remembering_places_of(self)
    }

    /// Every room one step from `room_key`, with the step's cost: the
    /// room's own exits and, on a map room, the exits the map's Secrets and
    /// Private additions keep there. A Secret's rooms lead back into the map
    /// through their own exits, so routes pass through Secrets like any
    /// other rooms; an exit into a room of another map's Secret steps into
    /// that Secret's own area while the atlas holds it ([`Self::can_follow_exit`]).
    /// With [`Sources::Hidden`] there are no Secrets or Private
    /// additions at all: no hidden door, and no step into or out of their
    /// rooms, so routes run as on maps without them.
    ///
    /// Excluded areas (manually disabled or per-server scope-excluded, a
    /// Secret with its map) are walls, not penalties: no step enters one
    /// unless `open` admits it, as the routing calls admit the maps of the
    /// rooms a caller named. Own-beats-shared: steps into areas the viewer
    /// does not own cost [`SHARED_AREA_WEIGHT_PENALTY`] times more, so a
    /// route only crosses a friend's map when no comparable owned one
    /// exists.
    fn successors(
        &self,
        room_key: &RoomKey,
        open: impl Fn(&AreaId) -> bool,
        sources: Sources,
    ) -> Vec<(RoomKey, OrderedFloat<f32>)> {
        let shown = sources == Sources::Shown;
        if !shown && self.source_areas.contains_key(&room_key.area_id) {
            return Vec::new();
        }
        let Some(area) = self.area_ref(&room_key.area_id) else {
            return Vec::new();
        };
        let Some(room) = area.get_room(&room_key.room_number) else {
            return Vec::new();
        };
        let map = area
            .map_id()
            .and_then(|id| self.area_ref(&id))
            .unwrap_or(area);
        let layers = if shown { map.source_layers() } else { &[] };
        let anchored = layers
            .iter()
            .chain(map.map_document_layer())
            .flat_map(|layer| layer.exits_on(room_key).iter());
        room.get_exits()
            .iter()
            .chain(anchored)
            .filter(|exit| self.can_follow_exit(sources, exit))
            .filter_map(|exit| {
                let to = exit.to_area_id.zip(exit.to_room_number)?;
                Some((RoomKey::new(to.0, to.1), OrderedFloat(exit.weight)))
            })
            .filter(|(key, _)| shown || !self.source_areas.contains_key(&key.area_id))
            .filter(|(key, _)| !self.area_is_excluded(&key.area_id) || open(&key.area_id))
            .map(|(key, weight)| {
                if self.owned_areas.contains(&key.area_id) {
                    (key, weight)
                } else {
                    (key, OrderedFloat(weight.0 * SHARED_AREA_WEIGHT_PENALTY))
                }
            })
            .collect()
    }

    /// Whether `area_id` belongs to the same map as one of `named`'s areas
    /// (a map and its Secrets count as one): the routing calls keep those
    /// open through any exclusion.
    fn shares_a_map(&self, area_id: &AreaId, named: &[&AreaId]) -> bool {
        let map = self.placed_by(area_id);
        named.iter().any(|named| self.placed_by(named) == map)
    }

    #[must_use]
    pub fn get_path_between_rooms(
        &self,
        from_room_key: &RoomKey,
        to_room_key: &RoomKey,
    ) -> Option<Vec<RoomKey>> {
        self.get_path_between_rooms_with(from_room_key, to_room_key, Sources::Shown)
    }

    /// [`Self::get_path_between_rooms`] for a caller who sees `sources`.
    #[must_use]
    pub fn get_path_between_rooms_with(
        &self,
        from_room_key: &RoomKey,
        to_room_key: &RoomKey,
        sources: Sources,
    ) -> Option<Vec<RoomKey>> {
        let named = [&from_room_key.area_id, &to_room_key.area_id];
        pathfinding::prelude::dijkstra(
            from_room_key,
            |room| self.successors(room, |area| self.shares_a_map(area, &named), sources),
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
        let named = [&from_room_key.area_id];
        pathfinding::prelude::dijkstra(
            from_room_key,
            |room| self.successors(room, |area| self.shares_a_map(area, &named), Sources::Shown),
            |room_key| {
                self.get_room(room_key)
                    .is_some_and(|r| predicate(r.as_ref()))
            },
        )
        .and_then(|(path, _cost)| path.last().cloned())
    }

    /// The nearest reachable room whose key satisfies `predicate`, by the same
    /// traversal as [`Self::find_nearest_room_with_predicate`] for a caller who
    /// sees `sources`: a caller that has already looked its candidates up in
    /// an index names them by key, so the walk tests set membership rather
    /// than room contents. A key naming no room never matches.
    #[must_use]
    pub fn find_nearest_room_where<F>(
        &self,
        from_room_key: &RoomKey,
        sources: Sources,
        predicate: F,
    ) -> Option<RoomKey>
    where
        F: Fn(&RoomKey) -> bool,
    {
        let named = [&from_room_key.area_id];
        pathfinding::prelude::dijkstra(
            from_room_key,
            |room| self.successors(room, |area| self.shares_a_map(area, &named), sources),
            |room_key| predicate(room_key) && self.get_room(room_key).is_some(),
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
        self.find_nearest_room_in_area_with(from_room_key, target_area_id, Sources::Shown)
    }

    /// [`Self::find_nearest_room_in_area`] for a caller who sees `sources`.
    #[must_use]
    pub fn find_nearest_room_in_area_with(
        &self,
        from_room_key: &RoomKey,
        target_area_id: &AreaId,
        sources: Sources,
    ) -> Option<RoomKey> {
        let named = [&from_room_key.area_id, target_area_id];
        pathfinding::prelude::dijkstra(
            from_room_key,
            |room| self.successors(room, |area| self.shares_a_map(area, &named), sources),
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
            let prefix = matches.partition_point(|(id, _)| owned_areas.contains(id));
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

/// Inserts one area under `key`, keeping the owned-area prefix intact on the
/// same terms as [`insert_match`]. An area appears at most once per key, so no
/// duplicate check is needed: a snapshot's properties are a map, and a
/// replacement snapshot removes the old entries first.
fn insert_area_match<K>(
    table: &mut PersistentMap<K, AreaMatches>,
    key: K,
    area_id: AreaId,
    owned: bool,
    owned_areas: &HashSet<AreaId>,
) where
    K: Hash + Eq + Clone,
{
    if let Some(matches) = table.get_mut(&key) {
        if owned {
            let prefix = matches.partition_point(|id| owned_areas.contains(id));
            matches.insert(prefix, area_id);
        } else {
            matches.push(area_id);
        }
    } else {
        table.insert(key, vec![area_id]);
    }
}

/// Removes `area_id` from the match list under `key`, dropping the key when
/// its list empties.
fn remove_area_match<K, Q>(table: &mut PersistentMap<K, AreaMatches>, key: &Q, area_id: AreaId)
where
    K: Hash + Eq + Clone + Borrow<Q>,
    Q: Hash + Eq + ?Sized,
{
    let Some(matches) = table.get_mut(key) else {
        return;
    };
    matches.retain(|id| *id != area_id);
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

/// Where a source's anchored exits lead, as [`AtlasCache::exit_targets`]
/// keys: every destination, the source's own rooms included, since the exits
/// leave map rooms.
fn anchored_target_keys(anchored: &AnchoredExits) -> impl Iterator<Item = (Uuid, RoomNumber)> + '_ {
    anchored.values().flatten().filter_map(|exit| {
        exit.to_area_id
            .zip(exit.to_room_number)
            .map(|(to_area, number)| (to_area.0, number))
    })
}

impl SourceArea {
    fn of(map: AreaId, layer: &crate::mapper::area_cache::SourceLayer) -> Self {
        Self {
            map,
            source: layer.source(),
            area: layer.area().clone(),
            anchored: layer.anchored_shared().clone(),
        }
    }
}

/// [`AtlasCache::exit_targets`] for a whole set of areas at once: every key
/// gathered in one pass, sorted, counted and inserted in key order, which
/// costs a fraction of counting them one exit at a time into the tree.
fn count_exit_targets(areas: &HashMap<AreaId, Arc<AreaCache>>) -> OrdMap<(Uuid, RoomNumber), u32> {
    let mut keys: Vec<(Uuid, RoomNumber)> = Vec::new();
    for (area_id, area) in areas {
        if let Some(layer) = area.map_document_layer() {
            keys.extend(anchored_target_keys(layer.anchored_shared()));
        }
        for room in area.get_rooms() {
            keys.extend(AtlasCache::exit_target_keys(*area_id, room));
        }
        for layer in area.source_layers() {
            let source_area_id = layer.area_id();
            for room in layer.area().get_rooms() {
                keys.extend(AtlasCache::exit_target_keys(source_area_id, room));
            }
            keys.extend(anchored_target_keys(layer.anchored_shared()));
        }
    }
    keys.sort_unstable();
    keys.chunk_by(|left, right| left == right)
        .map(|run| (run[0], u32::try_from(run.len()).unwrap_or(u32::MAX)))
        .collect()
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
            room_data: Vec::new(),
            sources: Vec::new(),
            area: Area {
                projection_token: None,
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
                clan_id: None,
                clan_name: None,
                actions: None,
                clan_ownership: crate::clan_maps::ClanOwnership::default(),
                atlas_name: None,
            },
            format_version: crate::AREA_FORMAT_VERSION,
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
            external_id: None,
        }
    }

    /// [`room`] with properties and tags attached.
    fn tagged_room(
        number: i32,
        title: &str,
        properties: &[(&str, &str)],
        tags: &[&str],
    ) -> RoomWithDetails {
        RoomWithDetails {
            properties: properties
                .iter()
                .map(|(name, value)| crate::Property {
                    name: (*name).to_string(),
                    value: (*value).to_string(),
                })
                .collect(),
            tags: tags
                .iter()
                .map(|tag| crate::mapper::normalize_tag(tag))
                .collect(),
            ..room(number, title, Vec::new())
        }
    }

    /// [`cache_area`] carrying the area's own properties.
    fn cache_area_with_properties(
        id: AreaId,
        owned: bool,
        properties: &[(&str, &str)],
        rooms: Vec<RoomWithDetails>,
    ) -> (AreaId, Arc<AreaCache>) {
        let (id, cache) = cache_area(id, owned, rooms);
        let mut cache = (*cache).clone();
        for (name, value) in properties {
            cache = cache.set_property((*name).to_string(), (*value).to_string());
        }
        (id, Arc::new(cache))
    }

    fn room_numbers(rooms: impl Iterator<Item = (AreaId, Arc<RoomCache>)>) -> Vec<(AreaId, i32)> {
        rooms
            .map(|(area_id, room)| (area_id, room.get_room_number().0))
            .collect()
    }

    #[test]
    fn property_and_tag_lookups_answer_atlas_wide_with_owned_first() {
        let owned_id = area_id(1);
        let shared_id = area_id(2);

        // Shared inserted first, so an unordered table would put it first.
        let mut areas = HashMap::new();
        let (id, cache) = cache_area(
            shared_id,
            false,
            vec![tagged_room(7, "Plaza", &[("zone", "midgaard")], &["shop"])],
        );
        areas.insert(id, cache);
        let (id, cache) = cache_area(
            owned_id,
            true,
            vec![
                tagged_room(1, "Gate", &[("zone", "midgaard")], &["SHOP", "quiet"]),
                tagged_room(2, "Road", &[("zone", "elsewhere")], &[]),
            ],
        );
        areas.insert(id, cache);

        let atlas = atlas(areas);

        // By value: only the rooms holding exactly that value, owned first.
        assert_eq!(
            room_numbers(atlas.get_rooms_by_property("zone", "midgaard")),
            vec![(owned_id, 1), (shared_id, 7)]
        );
        // By name alone: every room carrying the property at all.
        let mut with_property = room_numbers(atlas.get_rooms_with_property("zone"));
        with_property.sort_by_key(|(id, number)| (id.0, *number));
        assert_eq!(
            with_property,
            vec![(owned_id, 1), (owned_id, 2), (shared_id, 7)]
        );
        // A name no room carries, and a value no room holds.
        assert_eq!(
            room_numbers(atlas.get_rooms_with_property("absent")),
            vec![]
        );
        assert_eq!(
            room_numbers(atlas.get_rooms_by_property("zone", "nowhere")),
            vec![]
        );
        // Tags match case-insensitively, in either direction.
        assert_eq!(
            room_numbers(atlas.get_rooms_with_tag("shop")),
            vec![(owned_id, 1), (shared_id, 7)]
        );
        assert_eq!(
            room_numbers(atlas.get_rooms_with_tag("ShOp")),
            vec![(owned_id, 1), (shared_id, 7)]
        );
    }

    #[test]
    fn area_property_lookups_answer_by_name_and_by_value() {
        let owned_id = area_id(1);
        let shared_id = area_id(2);

        let mut areas = HashMap::new();
        let (id, cache) =
            cache_area_with_properties(shared_id, false, &[("kind", "city")], Vec::new());
        areas.insert(id, cache);
        let (id, cache) = cache_area_with_properties(
            owned_id,
            true,
            &[("kind", "city"), ("era", "old")],
            Vec::new(),
        );
        areas.insert(id, cache);

        let atlas = atlas(areas);

        assert_eq!(
            atlas
                .get_areas_by_property("kind", "city")
                .collect::<Vec<_>>(),
            vec![owned_id, shared_id]
        );
        let mut with_kind = atlas.get_areas_with_property("kind").collect::<Vec<_>>();
        with_kind.sort_by_key(|id| id.0);
        let mut expected = vec![owned_id, shared_id];
        expected.sort_by_key(|id| id.0);
        assert_eq!(with_kind, expected);
        assert_eq!(
            atlas.get_areas_with_property("era").collect::<Vec<_>>(),
            vec![owned_id]
        );
        assert_eq!(atlas.get_areas_by_property("kind", "wilderness").count(), 0);
        assert_eq!(atlas.get_areas_with_property("missing").count(), 0);
    }

    #[test]
    fn incremental_writes_leave_the_tables_a_full_rebuild_would_produce() {
        let id = area_id(1);

        let initial = vec![
            tagged_room(1, "Gate", &[("zone", "midgaard")], &["shop"]),
            tagged_room(2, "Road", &[("zone", "midgaard")], &["quiet"]),
        ];
        let mut areas = HashMap::new();
        let (area, cache) = cache_area_with_properties(id, true, &[("kind", "city")], initial);
        areas.insert(area, cache);
        let mut atlas = atlas(areas);

        // Move room 1 to another zone, drop its tag, and change the area's own
        // property: one write touching every table.
        let rewritten = vec![
            tagged_room(1, "Gate", &[("zone", "elsewhere")], &[]),
            tagged_room(2, "Road", &[("zone", "midgaard")], &["quiet"]),
        ];
        let (_, next) = cache_area_with_properties(id, true, &[("kind", "wilderness")], rewritten);
        atlas.apply_insert(id, next.clone());

        // The same area set, built from scratch.
        let rebuilt = atlas.rebuild_with_areas(HashMap::from([(id, next)]));

        assert_eq!(
            room_numbers(atlas.get_rooms_by_property("zone", "midgaard")),
            room_numbers(rebuilt.get_rooms_by_property("zone", "midgaard"))
        );
        assert_eq!(
            room_numbers(atlas.get_rooms_by_property("zone", "midgaard")),
            vec![(id, 2)]
        );
        assert_eq!(
            room_numbers(atlas.get_rooms_by_property("zone", "elsewhere")),
            vec![(id, 1)]
        );
        // The stale tag and stale area property are gone, not merely shadowed.
        assert_eq!(room_numbers(atlas.get_rooms_with_tag("shop")), vec![]);
        assert_eq!(atlas.get_areas_by_property("kind", "city").count(), 0);
        assert_eq!(
            atlas
                .get_areas_by_property("kind", "wilderness")
                .collect::<Vec<_>>(),
            vec![id]
        );
        assert_eq!(
            atlas.get_areas_with_property("kind").collect::<Vec<_>>(),
            rebuilt.get_areas_with_property("kind").collect::<Vec<_>>()
        );
    }

    #[test]
    fn excluded_areas_drop_out_of_property_and_tag_lookups() {
        let kept = area_id(1);
        let disabled = area_id(2);
        let scoped_out = area_id(3);
        let other_atlas = atlas_id(9);

        let mut areas = HashMap::new();
        let (id, cache) = cache_area_with_properties(
            kept,
            true,
            &[("kind", "city")],
            vec![tagged_room(1, "Gate", &[("zone", "midgaard")], &["shop"])],
        );
        areas.insert(id, cache);
        let (id, cache) = cache_area_with_properties(
            disabled,
            true,
            &[("kind", "city")],
            vec![tagged_room(1, "Gate", &[("zone", "midgaard")], &["shop"])],
        );
        areas.insert(id, cache);

        // A manually disabled area contributes nothing.
        let atlas = atlas_with_disabled(areas, [disabled]);
        assert_eq!(
            room_numbers(atlas.get_rooms_by_property("zone", "midgaard")),
            vec![(kept, 1)]
        );
        assert_eq!(
            room_numbers(atlas.get_rooms_with_tag("shop")),
            vec![(kept, 1)]
        );
        assert_eq!(
            atlas
                .get_areas_by_property("kind", "city")
                .collect::<Vec<_>>(),
            vec![kept]
        );

        // Nor does a scope-excluded one.
        let mut areas = HashMap::new();
        let (id, cache) = cache_area_with_properties(
            kept,
            true,
            &[("kind", "city")],
            vec![tagged_room(1, "Gate", &[("zone", "midgaard")], &["shop"])],
        );
        areas.insert(id, cache);
        let (id, cache) = cache_area_in_atlas(
            scoped_out,
            Some(other_atlas),
            true,
            vec![tagged_room(1, "Gate", &[("zone", "midgaard")], &["shop"])],
        );
        areas.insert(id, cache);
        let atlas = atlas_with_scope(areas, [other_atlas], []);
        assert_eq!(
            room_numbers(atlas.get_rooms_by_property("zone", "midgaard")),
            vec![(kept, 1)]
        );
        assert_eq!(
            room_numbers(atlas.get_rooms_with_tag("shop")),
            vec![(kept, 1)]
        );

        // Explicit addressing still reaches an excluded area's rooms.
        let excluded_area = atlas.get_area(&scoped_out).expect("area stays resident");
        assert_eq!(
            excluded_area.get_rooms_with_tag("shop").len(),
            1,
            "per-area lookups are explicit addressing and ignore exclusion"
        );
    }

    #[test]
    fn per_area_lookups_see_only_their_own_area() {
        let first = area_id(1);
        let second = area_id(2);

        let (_, a) = cache_area(
            first,
            true,
            vec![
                tagged_room(1, "Gate", &[("zone", "midgaard")], &["shop"]),
                tagged_room(2, "Road", &[("zone", "midgaard")], &[]),
            ],
        );
        let (_, b) = cache_area(
            second,
            true,
            vec![tagged_room(1, "Plaza", &[("zone", "midgaard")], &["shop"])],
        );

        let numbers = |rooms: &[Arc<RoomCache>]| {
            rooms
                .iter()
                .map(|room| room.get_room_number().0)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            numbers(a.get_rooms_by_property("zone", "midgaard")),
            vec![1, 2]
        );
        assert_eq!(
            numbers(b.get_rooms_by_property("zone", "midgaard")),
            vec![1]
        );
        assert_eq!(numbers(a.get_rooms_with_property("zone")), vec![1, 2]);
        assert_eq!(numbers(a.get_rooms_with_tag("SHOP")), vec![1]);
        assert_eq!(numbers(a.get_rooms_with_tag("absent")), Vec::<i32>::new());
        assert_eq!(
            numbers(a.get_rooms_by_property("zone", "elsewhere")),
            Vec::<i32>::new()
        );
    }

    fn exit(id: u128, to_area: AreaId, to_room: i32, weight: f32) -> Exit {
        Exit {
            to_source: None,
            id: ExitId(Uuid::from_u128(id)),
            from_direction: crate::ExitDirection::North,
            to_area_id: Some(to_area),
            to_room_number: Some(RoomNumber(to_room)),
            to_direction: None,
            path: String::new(),
            is_hidden: false,
            door: None,
            weight,
            command: String::new(),
            connection_id: crate::ConnectionId::new(),
            to_unknown: false,
            to_area_token: None,
        }
    }

    fn cache_area(
        id: AreaId,
        owned: bool,
        rooms: Vec<RoomWithDetails>,
    ) -> (AreaId, Arc<AreaCache>) {
        let details = AreaWithDetails {
            room_data: Vec::new(),
            sources: Vec::new(),
            area: Area {
                projection_token: None,
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
                clan_id: None,
                clan_name: None,
                actions: None,
                clan_ownership: crate::clan_maps::ClanOwnership::default(),
                atlas_name: None,
            },
            format_version: crate::AREA_FORMAT_VERSION,
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
        let atlas_incremental = atlas_incremental.insert_area(c_id, cache_c);
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

    /// A holds rooms 1 and 2, and room 2 has an exit to A 6, which does not
    /// exist. Two of B's exits lead to A 3, one to A 4 and one to A 9, none
    /// of which exist either. The vacant targets are 3, 4, 6 and 9, whether
    /// B is turned off or not, until the last exit leading to a number is
    /// gone, while allocation starts at 3 throughout: a room that goes
    /// clears the exits into it, so a number no room holds is free. An atlas
    /// edited room by room holds the same exit targets as one built from
    /// scratch over its final areas.
    #[test]
    fn vacant_exit_targets_name_every_number_an_exit_leads_to() {
        let (a_id, b_id) = (area_id(1), area_id(2));
        let (_, a) = cache_area(
            a_id,
            true,
            vec![
                room(1, "One", Vec::new()),
                room(2, "Two", vec![exit(8, a_id, 6, 1.0)]),
            ],
        );
        let b = |exits: Vec<Exit>| cache_area(b_id, true, vec![room(1, "Lost", exits)]).1;
        let areas: HashMap<_, _> = [
            (a_id, a),
            (
                b_id,
                b(vec![
                    exit(9, a_id, 3, 1.0),
                    exit(10, a_id, 3, 1.0),
                    exit(11, a_id, 4, 1.0),
                    exit(12, a_id, 9, 1.0),
                ]),
            ),
        ]
        .into_iter()
        .collect();

        let atlas = atlas(areas.clone());
        assert_eq!(atlas.room_number_floor(&a_id), Some(3));
        assert_eq!(
            atlas.vacant_exit_targets(&a_id),
            vec![RoomNumber(3), RoomNumber(4), RoomNumber(6), RoomNumber(9)]
        );
        assert_eq!(
            atlas_with_disabled(areas, [b_id]).vacant_exit_targets(&a_id),
            vec![RoomNumber(3), RoomNumber(4), RoomNumber(6), RoomNumber(9)],
            "an exit counts in an area that is turned off"
        );

        let atlas = atlas.insert_area(b_id, b(vec![exit(10, a_id, 3, 1.0)]));
        assert_eq!(
            atlas.vacant_exit_targets(&a_id),
            vec![RoomNumber(3), RoomNumber(6)],
            "one exit still leads to 3"
        );
        let atlas = atlas.insert_area(b_id, b(vec![exit(13, a_id, 1, 1.0)]));
        assert_eq!(
            atlas.vacant_exit_targets(&a_id),
            vec![RoomNumber(6)],
            "A's own exit still leads to 6"
        );
        assert_eq!(atlas.room_number_floor(&a_id), Some(3));
        let from_scratch = atlas.rebuild_with_areas(atlas.areas.clone());
        assert_eq!(atlas.exit_targets, from_scratch.exit_targets);
        let toggled = atlas.with_disabled_areas(Arc::new([b_id].into_iter().collect()));
        assert_eq!(atlas.exit_targets, toggled.exit_targets);

        let atlas = atlas.delete_area(b_id);
        assert!(atlas.exit_targets.is_empty());
    }

    /// Counting exit targets in one pass over a whole area set, with several
    /// exits from several areas naming one room and one exit naming a
    /// number no room holds, gives exactly the counts the per-room path
    /// keeps. Exits into their own area's rooms are the area's to answer
    /// for and count in neither.
    #[test]
    fn exit_targets_counted_in_bulk_match_the_counts_kept_room_by_room() {
        let (a_id, b_id, c_id) = (area_id(1), area_id(2), area_id(3));
        let areas: HashMap<AreaId, Arc<AreaCache>> = [
            cache_area(
                a_id,
                true,
                vec![
                    room(
                        1,
                        "A1",
                        vec![
                            exit(1, a_id, 2, 1.0),
                            exit(2, b_id, 7, 1.0),
                            exit(3, c_id, 1, 1.0),
                        ],
                    ),
                    room(2, "A2", vec![exit(4, a_id, 1, 1.0), exit(5, b_id, 7, 1.0)]),
                ],
            ),
            cache_area(
                b_id,
                false,
                vec![room(
                    7,
                    "B7",
                    vec![exit(6, a_id, 2, 1.0), exit(7, a_id, 9, 1.0)],
                )],
            ),
            cache_area(
                c_id,
                true,
                vec![room(
                    1,
                    "C1",
                    vec![exit(8, b_id, 7, 1.0), exit(9, c_id, 1, 1.0)],
                )],
            ),
        ]
        .into_iter()
        .collect();

        let incremental = areas
            .iter()
            .fold(atlas(HashMap::new()), |atlas, (id, area)| {
                atlas.insert_area(*id, area.clone())
            });
        let bulk = atlas(areas);

        assert_eq!(bulk.exit_targets.get(&(b_id.0, RoomNumber(7))), Some(&3));
        assert_eq!(bulk.exit_targets.get(&(a_id.0, RoomNumber(2))), Some(&1));
        assert_eq!(bulk.exit_targets.get(&(a_id.0, RoomNumber(9))), Some(&1));
        assert_eq!(bulk.exit_targets.get(&(c_id.0, RoomNumber(1))), Some(&1));
        assert_eq!(bulk.exit_targets.get(&(a_id.0, RoomNumber(1))), None);
        assert_eq!(incremental.exit_targets, bulk.exit_targets);
    }

    /// A Secret of map `map` holding `rooms`. It keeps an exit on map room
    /// 1 into its own room 1, and its room 1 leads back to map room 1.
    fn secret_bundle(
        map: AreaId,
        secret: Uuid,
        rooms: Vec<RoomWithDetails>,
    ) -> crate::SourceBundle {
        let source = SourceId::Secret(secret);
        let rooms = rooms
            .into_iter()
            .map(|mut room| {
                if room.room_number == RoomNumber(1) {
                    room.exits.push(exit(0x51, map, 1, 1.0));
                }
                room
            })
            .collect();
        crate::SourceBundle {
            source,
            name: Some("Bookcase".to_string()),
            ownership: Some("owner".to_string()),
            clan_id: None,
            color: None,
            rev: 1,
            actions: ["read", "add", "edit", "remove"]
                .into_iter()
                .map(str::to_string)
                .collect(),
            properties: Vec::new(),
            rooms,
            room_data: vec![crate::RoomData {
                room_source: None,
                room_number: RoomNumber(1),
                properties: Vec::new(),
                tags: std::collections::BTreeSet::default(),
                exits: vec![Exit {
                    to_source: Some(source),
                    ..exit(0x50, map, 1, 1.0)
                }],
            }],
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
        }
    }

    /// Map `id`'s document: `rooms`, with `sources` beside them.
    fn map_details(
        id: AreaId,
        owned: bool,
        rooms: Vec<RoomWithDetails>,
        sources: Vec<crate::SourceBundle>,
    ) -> AreaWithDetails {
        AreaWithDetails {
            room_data: Vec::new(),
            sources,
            area: Area {
                projection_token: None,
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
                clan_id: None,
                clan_name: None,
                actions: None,
                clan_ownership: crate::clan_maps::ClanOwnership::default(),
                atlas_name: None,
            },
            format_version: crate::AREA_FORMAT_VERSION,
            properties: Vec::new(),
            rooms,
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
            linked_areas: Vec::new(),
        }
    }

    fn titled(atlas: &AtlasCache, title: &str) -> Vec<(AreaId, i32)> {
        let mut found = room_numbers(atlas.get_rooms_by_title(title));
        found.sort_by_key(|(area_id, number)| (area_id.0, *number));
        found
    }

    fn with_external_id(mut room: RoomWithDetails, external_id: &str) -> RoomWithDetails {
        room.external_id = Some(external_id.to_string());
        room
    }

    #[test]
    fn a_secret_is_an_area_of_its_own_and_its_rooms_are_identified() {
        let map_id = area_id(1);
        let secret = Uuid::from_u128(0x5ec);
        let secret_id = AreaId(secret);
        let details = map_details(
            map_id,
            true,
            vec![room(1, "Library", Vec::new())],
            vec![secret_bundle(
                map_id,
                secret,
                vec![with_external_id(room(1, "Vault", Vec::new()), "v-1")],
            )],
        );
        let atlas = atlas(HashMap::from([(
            map_id,
            Arc::new(AreaCache::new_with_area(details)),
        )]));

        let (key, room) = atlas
            .find_room_by_external_id("v-1")
            .expect("the Secret's room is identified by its external id");
        assert_eq!(key, RoomKey::new(secret_id, RoomNumber(1)));
        assert_eq!(room.get_title(), "Vault");
        assert_eq!(titled(&atlas, "Vault"), vec![(secret_id, 1)]);
        assert_eq!(atlas.map_of(&secret_id), Some(map_id));
        assert_eq!(atlas.map_of(&map_id), Some(map_id));
        assert_eq!(
            atlas.source_of(&secret_id),
            Some((map_id, SourceId::Secret(secret)))
        );
        let area = atlas.get_area(&secret_id).expect("addressable");
        assert_eq!(area.get_name(), "Bookcase");
        assert_eq!(area.map_id(), Some(map_id));
        assert_eq!(atlas.areas().count(), 1, "areas() lists maps only");
        assert!(atlas.is_area_owned(&secret_id), "owned with its map");
        let back = &room.get_exits()[0];
        assert_eq!(
            (back.to_area_id, back.to_room_number),
            (Some(map_id), Some(RoomNumber(1))),
            "an exit into a map room names the map"
        );
        let map = atlas.get_area(&map_id).expect("the map");
        let door = &map.source_layers()[0].anchored_exits(RoomNumber(1))[0];
        assert_eq!(
            (door.to_area_id, door.to_room_number),
            (Some(secret_id), Some(RoomNumber(1))),
            "an exit into the Secret's own room names the Secret"
        );
    }

    #[test]
    fn a_turned_off_map_hides_its_secrets() {
        let map_id = area_id(1);
        let secret = Uuid::from_u128(0x5ec);
        let secret_id = AreaId(secret);
        let details = map_details(
            map_id,
            true,
            vec![room(1, "Library", Vec::new())],
            vec![secret_bundle(
                map_id,
                secret,
                vec![with_external_id(room(1, "Vault", Vec::new()), "v-1")],
            )],
        );
        let areas = HashMap::from([(map_id, Arc::new(AreaCache::new_with_area(details)))]);

        let off = atlas_with_disabled(areas.clone(), [map_id]);
        assert!(off.find_room_by_external_id("v-1").is_none());
        assert!(titled(&off, "Vault").is_empty());
        assert!(!off.is_area_included(&secret_id));
        assert!(!off.is_area_enabled(&secret_id));
        assert!(off.get_area(&secret_id).is_some(), "still addressable");

        let toggled = atlas(areas).with_disabled_areas(Arc::new([map_id].into_iter().collect()));
        assert!(toggled.find_room_by_external_id("v-1").is_none());
        let back_on = toggled.with_disabled_areas(Arc::new(HashSet::new()));
        assert_eq!(titled(&back_on, "Vault"), vec![(secret_id, 1)]);
    }

    #[test]
    fn incremental_edits_with_secrets_match_a_full_rebuild() {
        let (a_id, b_id) = (area_id(1), area_id(2));
        let secret = Uuid::from_u128(0x5ec);
        let secret_id = AreaId(secret);
        let map_rooms = || vec![room(1, "Library", Vec::new()), room(2, "Hall", Vec::new())];
        let secret_rooms = |second: &str| {
            vec![
                with_external_id(room(1, "Vault", Vec::new()), "v-1"),
                room(2, second, Vec::new()),
            ]
        };
        let cellar = secret_bundle(a_id, secret, secret_rooms("Cellar"));
        let crypt = secret_bundle(a_id, secret, secret_rooms("Crypt"));
        let a = |rooms: Vec<RoomWithDetails>,
                 bundle: &crate::SourceBundle,
                 previous: Option<&AreaCache>| {
            Arc::new(AreaCache::for_viewer(
                map_details(a_id, true, rooms, vec![bundle.clone()]),
                None,
                previous,
            ))
        };
        let (_, b) = cache_area(
            b_id,
            false,
            vec![room(5, "Cellar", vec![exit(0x60, secret_id, 2, 1.0)])],
        );

        let atlas = atlas(HashMap::new())
            .insert_area(a_id, a(map_rooms(), &cellar, None))
            .insert_area(b_id, b);
        // The Secret's room 2 retitled.
        let previous = atlas.get_area(&a_id).expect("a");
        let atlas = atlas.insert_area(a_id, a(map_rooms(), &crypt, Some(&previous)));
        // A map edit leaves the Secret as it was: the same area, unindexed.
        let before = atlas.get_area(&secret_id).expect("the Secret");
        let previous = atlas.get_area(&a_id).expect("a");
        let mut retitled = map_rooms();
        retitled[1].title = "Great Hall".to_string();
        let atlas = atlas.insert_area(a_id, a(retitled, &crypt, Some(&previous)));
        assert!(Arc::ptr_eq(
            &before,
            &atlas.get_area(&secret_id).expect("the Secret")
        ));
        // A Secret that is gone takes its rooms with it.
        let previous = atlas.get_area(&a_id).expect("a");
        let without: Arc<AreaCache> = Arc::new(AreaCache::for_viewer(
            map_details(a_id, true, map_rooms(), Vec::new()),
            None,
            Some(&previous),
        ));
        let gone = atlas.insert_area(a_id, without);
        assert!(gone.get_area(&secret_id).is_none());
        assert!(gone.find_room_by_external_id("v-1").is_none());
        assert!(titled(&gone, "Vault").is_empty());
        assert!(!gone.is_area_owned(&secret_id));

        let rebuilt = atlas.rebuild_with_areas(atlas.areas.clone());
        for title in ["Library", "Hall", "Great Hall", "Vault", "Cellar", "Crypt"] {
            assert_eq!(titled(&atlas, title), titled(&rebuilt, title), "{title}");
        }
        assert_eq!(titled(&atlas, "Crypt"), vec![(secret_id, 2)]);
        assert!(titled(&atlas, "Cellar").iter().all(|(id, _)| *id == b_id));
        assert_eq!(
            atlas.find_room_by_external_id("v-1").map(|(key, _)| key),
            rebuilt.find_room_by_external_id("v-1").map(|(key, _)| key)
        );
        assert_eq!(atlas.exit_targets, rebuilt.exit_targets);
        assert_eq!(
            atlas.exit_targets.get(&(secret_id.0, RoomNumber(2))),
            Some(&1),
            "B's exit into the Secret's room 2"
        );
        assert_eq!(
            atlas.exit_targets.get(&(secret_id.0, RoomNumber(1))),
            Some(&1),
            "the door on map room 1"
        );
        assert_eq!(
            atlas.exit_targets.get(&(a_id.0, RoomNumber(1))),
            Some(&1),
            "the way back"
        );
        let deleted = atlas.delete_area(a_id);
        assert!(deleted.get_area(&secret_id).is_none());
        assert!(deleted.find_room_by_external_id("v-1").is_none());
        assert_eq!(
            deleted.exit_targets,
            deleted
                .rebuild_with_areas(deleted.areas.clone())
                .exit_targets
        );
    }

    /// Map rooms 1 and 3 have no exit between them; a Secret's hidden door
    /// on map room 1 leads into its room 1, whose exit leads on to map room
    /// 3 (and back to map room 1). The Secret's room 1 is an inn.
    fn map_with_a_way_through_a_secret(map_id: AreaId, secret: Uuid) -> Arc<AreaCache> {
        let mut inn = tagged_room(1, "Hidden inn", &[], &["inn"]);
        inn.exits.push(exit(0x52, map_id, 3, 1.0));
        Arc::new(AreaCache::new_with_area(map_details(
            map_id,
            true,
            vec![
                room(1, "Library", Vec::new()),
                room(3, "Garden", Vec::new()),
            ],
            vec![secret_bundle(map_id, secret, vec![inn])],
        )))
    }

    #[test]
    fn routes_pass_through_a_secret_and_find_rooms_in_it() {
        let map_id = area_id(1);
        let secret = Uuid::from_u128(0x5ec);
        let secret_id = AreaId(secret);
        let areas = HashMap::from([(map_id, map_with_a_way_through_a_secret(map_id, secret))]);
        let atlas = atlas(areas.clone());
        let library = RoomKey::new(map_id, RoomNumber(1));
        let garden = RoomKey::new(map_id, RoomNumber(3));
        let inn = RoomKey::new(secret_id, RoomNumber(1));

        assert_eq!(
            atlas.get_path_between_rooms(&library, &garden),
            Some(vec![library.clone(), inn.clone(), garden.clone()]),
            "map, then Secret, then map"
        );
        assert_eq!(
            atlas.get_path_between_rooms(&garden, &inn),
            None,
            "no way into the Secret from the garden"
        );
        assert_eq!(
            atlas.find_nearest_room_with_tag(&library, "INN"),
            Some(inn.clone())
        );
        assert_eq!(
            atlas.find_nearest_room_in_area(&library, &secret_id),
            Some(inn.clone())
        );

        // A turned-off map is a wall for its Secrets too, but a route a
        // caller names inside it still runs through them.
        let other = area_id(2);
        let (_, outside) = cache_area(
            other,
            true,
            vec![room(1, "Road", vec![exit(0x53, map_id, 1, 1.0)])],
        );
        let mut areas = areas;
        areas.insert(other, outside);
        let off = atlas_with_disabled(areas, [map_id]);
        let road = RoomKey::new(other, RoomNumber(1));
        assert_eq!(off.find_nearest_room_with_tag(&road, "INN"), None);
        assert_eq!(
            off.get_path_between_rooms(&road, &garden),
            Some(vec![road, library, inn, garden])
        );
    }

    /// A map snapshot without its Secret takes the Secret's ways with it:
    /// no route through its door, no room found in it, no number held for
    /// where its exits led, exactly as if it had never been read.
    #[test]
    fn losing_a_secret_takes_its_routes_and_door_targets_with_it() {
        let map_id = area_id(1);
        let secret = Uuid::from_u128(0x5ec);
        let secret_id = AreaId(secret);
        let with = map_with_a_way_through_a_secret(map_id, secret);
        let atlas = atlas(HashMap::from([(map_id, with.clone())]));
        let library = RoomKey::new(map_id, RoomNumber(1));
        let garden = RoomKey::new(map_id, RoomNumber(3));
        assert!(atlas.get_path_between_rooms(&library, &garden).is_some());
        assert!(
            atlas
                .exit_targets
                .keys()
                .any(|(area, _)| *area == secret_id.0)
        );

        let without = Arc::new(AreaCache::for_viewer(
            map_details(
                map_id,
                true,
                vec![
                    room(1, "Library", Vec::new()),
                    room(3, "Garden", Vec::new()),
                ],
                Vec::new(),
            ),
            None,
            Some(&with),
        ));
        let lost = atlas.insert_area(map_id, without.clone());
        assert_eq!(lost.get_path_between_rooms(&library, &garden), None);
        assert_eq!(lost.find_nearest_room_with_tag(&library, "INN"), None);
        assert_eq!(lost.find_nearest_room_in_area(&library, &secret_id), None);
        assert!(lost.get_area(&secret_id).is_none());
        assert_eq!(lost.map_of(&secret_id), None);
        assert!(titled(&lost, "Hidden inn").is_empty());
        let never = self::atlas(HashMap::from([(map_id, without)]));
        assert_eq!(lost.exit_targets, never.exit_targets);
        assert!(
            !lost
                .exit_targets
                .keys()
                .any(|(area, _)| *area == secret_id.0)
        );
        assert_eq!(
            lost.vacant_exit_targets(&map_id),
            never.vacant_exit_targets(&map_id)
        );
    }

    #[test]
    fn private_additions_read_under_an_id_of_the_map_and_the_viewer() {
        let map_id = area_id(1);
        let private = crate::SourceBundle {
            source: SourceId::Private,
            name: None,
            ..secret_bundle(map_id, Uuid::nil(), vec![room(1, "Nook", Vec::new())])
        };
        let read_by = |viewer: u128| {
            let cache = AreaCache::for_viewer(
                map_details(map_id, true, Vec::new(), vec![private.clone()]),
                Some(Uuid::from_u128(viewer)),
                None,
            );
            cache.source_layers()[0].area_id()
        };
        assert_eq!(read_by(7), read_by(7), "stable for one viewer");
        assert_ne!(read_by(7), read_by(8), "different for every viewer");
        assert_ne!(read_by(7), map_id);
        assert_eq!(
            Some(read_by(7)),
            crate::mapper::area_cache::source_area_id(
                map_id,
                SourceId::Private,
                Some(Uuid::from_u128(7))
            )
        );
    }

    #[test]
    fn cross_map_private_destinations_dont_route_to_ordinary_rooms_with_the_same_room_number() {
        let origin = area_id(1);
        let target = area_id(2);
        let viewer = Some(Uuid::from_u128(7));
        let private_id =
            crate::mapper::area_cache::source_area_id(target, SourceId::Private, viewer).unwrap();
        let mut private = crate::SourceBundle {
            source: SourceId::Private,
            name: None,
            ..secret_bundle(
                target,
                Uuid::nil(),
                vec![room(1, "Private room", Vec::new())],
            )
        };
        private.room_data.clear();
        for room in &mut private.rooms {
            room.exits.clear();
        }
        let mut doorway = exit(0xabc, target, 1, 1.0);
        doorway.to_source = Some(SourceId::Private);
        doorway.command = "enter".into();
        let origin_cache = Arc::new(AreaCache::for_viewer(
            map_details(
                origin,
                true,
                vec![room(1, "Origin", vec![doorway])],
                Vec::new(),
            ),
            viewer,
            None,
        ));
        let target_cache = Arc::new(AreaCache::for_viewer(
            map_details(
                target,
                true,
                vec![room(1, "Ordinary room", Vec::new())],
                vec![private],
            ),
            viewer,
            None,
        ));
        let atlas = atlas(HashMap::from([
            (origin, origin_cache),
            (target, target_cache),
        ]));
        let start = RoomKey::new(origin, RoomNumber(1));
        assert!(
            atlas
                .get_path_between_rooms(&start, &RoomKey::new(private_id, RoomNumber(1)))
                .is_some()
        );
        assert!(
            atlas
                .get_path_between_rooms(&start, &RoomKey::new(target, RoomNumber(1)))
                .is_none()
        );
        let cached_room = atlas.get_room(&start).unwrap();
        let cached = &cached_room.get_exits()[0];
        assert!(!atlas.can_follow_exit(Sources::Hidden, cached));
        let written = cached.to_exit();
        assert_eq!(written.to_area_id, Some(target));
        assert_eq!(written.to_source, Some(SourceId::Private));
        let edited = crate::ExitUpdates {
            command: Some("enter quietly".into()),
            ..Default::default()
        }
        .apply(cached);
        assert_eq!(edited.to_area_id, Some(private_id));
        assert_eq!(edited.to_exit().to_source, Some(SourceId::Private));

        let mut hidden = written;
        hidden.to_area_id = None;
        hidden.to_room_number = None;
        hidden.to_source = None;
        hidden.to_unknown = true;
        let hidden_cache = Arc::new(AreaCache::for_viewer(
            map_details(
                origin,
                true,
                vec![room(1, "Origin", vec![hidden])],
                Vec::new(),
            ),
            viewer,
            None,
        ));
        let redacted = atlas.insert_area(origin, hidden_cache);
        assert!(
            redacted
                .get_path_between_rooms(&start, &RoomKey::new(private_id, RoomNumber(1)))
                .is_none()
        );
        assert_eq!(
            redacted.get_room(&start).unwrap().get_exits()[0]
                .command
                .as_deref(),
            Some("enter")
        );
    }
}
