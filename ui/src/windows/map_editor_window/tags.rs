//! Tags in places. A room's tags read as one set, but every tag lives in
//! exactly one place (the map, one of its Secrets, or the viewer's Private
//! additions) and the editor always shows where: one chip is one (tag,
//! place), and every write touches exactly one place.
//!
//! This module holds the parts with no widgets in them: the map's tags place
//! by place (cached per revision of the map), the order the chip runs take, where the
//! tag input writes, what it suggests and when it warns, what a bulk add
//! skips, and whole-tag matching for the Rooms filter. Nothing here reaches
//! past what the viewer reads: the map's other places come from its layers,
//! and the server serves only readable ones.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;

use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::{RoomNumber, SourceId};

/// The server's limit on a tag, in characters, after normalization.
pub const TAG_LIMIT: usize = 64;

/// The most suggestions under the tag input.
pub const SUGGESTION_LIMIT: usize = 8;

/// The tag input's widget id in editor window `window`: the T key focuses
/// it and Tab completes it. Widget operations reach every window, so each
/// window's input has an id of its own.
#[must_use]
pub fn input_id(window: iced::window::Id) -> iced::widget::Id {
    iced::widget::Id::from(format!("map-editor-tag-input-{window:?}"))
}

/// A room as the Rooms section lists it: a map room under the map, or one
/// of a place's own rooms under that place.
pub type RoomRef = (SourceId, RoomNumber);

/// What the tag input holds, read the way the server reads a tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Typed {
    Empty,
    /// Longer than [`TAG_LIMIT`] once normalized.
    TooLong,
    /// The tag, normalized.
    Tag(String),
}

/// Normalizes the input as scripts and the server do (trimmed, UPPERCASE)
/// and checks it against the server's limit.
#[must_use]
pub fn typed(input: &str) -> Typed {
    let tag = smudgy_cloud::mapper::normalize_tag(input);
    if tag.is_empty() {
        Typed::Empty
    } else if tag.chars().count() > TAG_LIMIT {
        Typed::TooLong
    } else {
        Typed::Tag(tag)
    }
}

/// The place the tag input writes for a room living in `own`: a map room
/// takes "Add to", a place's own room its own place.
#[must_use]
pub fn destination(own: SourceId, add_to: SourceId) -> SourceId {
    if own.is_map() { add_to } else { own }
}

/// The order chip runs take: the room's own place, then the "Add to" place,
/// then the rest of `places` (the map, then its layers in color order).
#[must_use]
pub fn run_order(own: SourceId, add_to: SourceId, places: &[SourceId]) -> Vec<SourceId> {
    let mut order = vec![own];
    if add_to != own {
        order.push(add_to);
    }
    for place in places {
        if !order.contains(place) {
            order.push(*place);
        }
    }
    order
}

/// The places whose tags the input may suggest into `destination`: none
/// more private than it. The map takes the map's; a Secret takes the map's
/// and its own; Private takes every readable place's.
#[must_use]
pub fn suggestion_sources(destination: SourceId, places: &[SourceId]) -> Vec<SourceId> {
    match destination {
        SourceId::Map => vec![SourceId::Map],
        SourceId::Secret(_) => vec![SourceId::Map, destination],
        SourceId::Private => {
            let mut all = places.to_vec();
            if !all.contains(&SourceId::Private) {
                all.push(SourceId::Private);
            }
            all
        }
    }
}

/// A tag the input offers, with the place it is labeled as used in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    pub tag: String,
    /// "Used on the map" or "Used in ● <place>".
    pub used_in: SourceId,
}

/// What a bulk add into one place does to the selected rooms.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkPlan {
    /// The readable selected rooms that can carry the place's tags.
    pub applicable: usize,
    /// Rooms excluded by the linked-clan source restriction, by place.
    pub skipped: Vec<(SourceId, usize)>,
}

/// What adding into `destination` does with `map_rooms` selected map rooms
/// and `own_rooms` selected own rooms of each place.
#[must_use]
pub fn bulk_plan(
    map_rooms: usize,
    own_rooms: &BTreeMap<SourceId, usize>,
    destination: SourceId,
    only_own: bool,
) -> BulkPlan {
    if !only_own {
        return BulkPlan {
            applicable: map_rooms + own_rooms.values().sum::<usize>(),
            skipped: Vec::new(),
        };
    }
    BulkPlan {
        applicable: if destination.is_map() {
            map_rooms
        } else {
            own_rooms.get(&destination).copied().unwrap_or(0)
        },
        skipped: std::iter::once((&SourceId::Map, &map_rooms))
            .chain(own_rooms)
            .filter(|(place, count)| **place != destination && **count > 0)
            .map(|(place, count)| (*place, *count))
            .collect(),
    }
}

/// The whole tag a Rooms filter names (trimmed, UPPERCASE); `None` when
/// the filter is empty.
#[must_use]
pub fn filter_tag(filter: &str) -> Option<String> {
    let tag = smudgy_cloud::mapper::normalize_tag(filter);
    (!tag.is_empty()).then_some(tag)
}

/// A map's tags, place by place.
#[derive(Debug, Clone)]
pub struct TagIndex {
    /// The places tags were read from: the map, then its layers in color
    /// order.
    places: Vec<SourceId>,
    /// Each tag, sorted, with how many rooms carry it in each place that
    /// has it, in `places` order.
    tags: BTreeMap<String, Vec<(SourceId, usize)>>,
    /// Each tag's rooms in any place, as the Rooms section lists them.
    rooms: HashMap<String, Vec<RoomRef>>,
}

impl TagIndex {
    /// Reads `area`'s tags: the map's, and with `places` its layers' (each
    /// Secret the viewer reads, and their Private additions).
    #[must_use]
    pub fn build(area: &AreaCache, places: bool) -> Self {
        let mut index = Self {
            places: vec![SourceId::Map],
            tags: BTreeMap::new(),
            rooms: HashMap::new(),
        };
        for room in area.get_rooms() {
            for tag in room.tags() {
                index.count(tag, SourceId::Map, (SourceId::Map, room.get_room_number()));
            }
        }
        if places {
            for layer in area
                .document_layer(SourceId::Map)
                .into_iter()
                .chain(area.source_layers())
            {
                let source = layer.source();
                if !source.is_map() {
                    index.places.push(source);
                }
                for room in layer.content().document_rooms() {
                    let number = room.get_room_number();
                    let anchor = (room.address().source != source)
                        .then_some((room.address().source, number));
                    if source.is_map() && anchor.is_none() {
                        // The ordinary rooms were counted above.
                        continue;
                    }
                    let listed = anchor.unwrap_or((source, number));
                    for tag in room.tags() {
                        index.count(tag, source, listed);
                    }
                }
            }
        }
        for rooms in index.rooms.values_mut() {
            rooms.sort_unstable();
            rooms.dedup();
        }
        index
    }

    fn count(&mut self, tag: &str, place: SourceId, room: RoomRef) {
        let counts = self.tags.entry(tag.to_string()).or_default();
        match counts.iter_mut().find(|(counted, _)| *counted == place) {
            Some((_, count)) => *count += 1,
            None => counts.push((place, 1)),
        }
        self.rooms.entry(tag.to_string()).or_default().push(room);
    }

    /// The places tags were read from, the map first.
    #[must_use]
    pub fn places(&self) -> &[SourceId] {
        &self.places
    }

    /// Every tag, sorted, with its room count in each place that has it.
    pub fn tags(&self) -> impl Iterator<Item = (&str, &[(SourceId, usize)])> {
        self.tags
            .iter()
            .map(|(tag, counts)| (tag.as_str(), counts.as_slice()))
    }

    /// How many distinct tags the map's places carry.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tags.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tags.is_empty()
    }

    /// The rooms carrying normalized `tag` in any place.
    #[must_use]
    pub fn rooms_with(&self, tag: &str) -> &[RoomRef] {
        self.rooms.get(tag).map_or(&[], Vec::as_slice)
    }

    /// Up to [`SUGGESTION_LIMIT`] tags for the input writing into
    /// `destination` while it holds `input`: tags starting with it (the most
    /// used when it is empty), drawn only from places no more private than
    /// `destination`, leaving out those `already` says the destination has.
    /// The most used come first.
    #[must_use]
    pub fn suggestions(
        &self,
        destination: SourceId,
        input: &str,
        already: impl Fn(&str) -> bool,
    ) -> Vec<Suggestion> {
        let prefix = smudgy_cloud::mapper::normalize_tag(input);
        let sources = suggestion_sources(destination, &self.places);
        let mut found: Vec<(usize, Suggestion)> = self
            .tags
            .iter()
            .filter(|(tag, _)| tag.starts_with(&prefix) && !already(tag))
            .filter_map(|(tag, counts)| {
                let usable: Vec<&(SourceId, usize)> = counts
                    .iter()
                    .filter(|(place, _)| sources.contains(place))
                    .collect();
                let used: usize = usable.iter().map(|(_, count)| count).sum();
                // The map names a tag's use first: it is no secret there.
                let used_in = [SourceId::Map, destination]
                    .into_iter()
                    .find(|place| usable.iter().any(|(used, _)| used == place))
                    .or_else(|| usable.first().map(|(place, _)| *place))?;
                Some((
                    used,
                    Suggestion {
                        tag: tag.clone(),
                        used_in,
                    },
                ))
            })
            .collect();
        found
            .sort_by(|(a_used, a), (b_used, b)| b_used.cmp(a_used).then_with(|| a.tag.cmp(&b.tag)));
        found
            .into_iter()
            .take(SUGGESTION_LIMIT)
            .map(|(_, suggestion)| suggestion)
            .collect()
    }

    /// Where `tag` lives when only places more private than `destination`
    /// carry it on this map (the first such place, for the hint); `None`
    /// when the map has it nowhere, or somewhere at least as public.
    #[must_use]
    pub fn more_private_only(&self, destination: SourceId, tag: &str) -> Option<SourceId> {
        let counts = self.tags.get(tag)?;
        let sources = suggestion_sources(destination, &self.places);
        if counts.iter().any(|(place, _)| sources.contains(place)) {
            return None;
        }
        self.places
            .iter()
            .copied()
            .find(|place| counts.iter().any(|(counted, _)| counted == place))
    }
}

/// The open map's [`TagIndex`], rebuilt only when the map's revision moves,
/// so the views read it without scanning rooms on every render.
///
/// The revision is the map's cache snapshot: every change to any of its
/// places, the viewer's own or synced, publishes a new one. The numbered
/// revisions are not enough: an edit waiting to save moves no place's
/// number. Holding the snapshot keeps its address from being reused.
#[derive(Debug, Default)]
pub struct IndexCache(RefCell<Option<Built>>);

#[derive(Debug)]
struct Built {
    snapshot: Arc<AreaCache>,
    places: bool,
    index: Rc<TagIndex>,
}

impl IndexCache {
    /// `area`'s index, from the cache while the snapshot it was read from is
    /// still the map's.
    #[must_use]
    pub fn get(&self, area: &Arc<AreaCache>, places: bool) -> Rc<TagIndex> {
        if let Some(built) = self
            .0
            .borrow()
            .as_ref()
            .filter(|built| Arc::ptr_eq(&built.snapshot, area) && built.places == places)
        {
            return built.index.clone();
        }
        let index = Rc::new(TagIndex::build(area, places));
        *self.0.borrow_mut() = Some(Built {
            snapshot: area.clone(),
            places,
            index: index.clone(),
        });
        index
    }
}

/// The selected rooms' tags, place by place, and where the rooms live.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SelectionTags {
    /// The rooms selected.
    pub rooms: usize,
    /// How many are map rooms.
    pub map_rooms: usize,
    /// How many of each place's own rooms are selected.
    pub own_rooms: BTreeMap<SourceId, usize>,
    /// Each place's tags on the rooms, with how many of them carry each.
    pub places: BTreeMap<SourceId, BTreeMap<String, usize>>,
}

impl SelectionTags {
    /// The selected map rooms `map_rooms` and places' own rooms `own_rooms`
    /// of `area`, read from the map and its layers.
    #[must_use]
    pub fn read(
        area: &AreaCache,
        map_rooms: impl IntoIterator<Item = RoomNumber>,
        own_rooms: impl IntoIterator<Item = (SourceId, RoomNumber)>,
    ) -> Self {
        let mut selection = Self::default();
        for number in map_rooms {
            let Some(room) = area.get_room(&number) else {
                continue;
            };
            selection.rooms += 1;
            selection.map_rooms += 1;
            selection.add(SourceId::Map, room.tags());
            selection.add_attachments(area, SourceId::Map, number);
        }
        for (source, number) in own_rooms {
            let Some(room) = smudgy_map_widget::sources::source_room(area, source, number) else {
                continue;
            };
            selection.rooms += 1;
            *selection.own_rooms.entry(source).or_default() += 1;
            selection.add(source, room.tags());
            selection.add_attachments(area, source, number);
        }
        selection
    }

    fn add_attachments(&mut self, area: &AreaCache, anchor: SourceId, number: RoomNumber) {
        for layer in area
            .document_layer(SourceId::Map)
            .into_iter()
            .chain(area.source_layers())
            .filter(|layer| layer.source() != anchor)
        {
            if let Some(data) = layer.attachment(smudgy_cloud::RoomAddress::new(anchor, number)) {
                self.add(layer.source(), data.tags());
            }
        }
    }

    fn add<'a>(&mut self, place: SourceId, tags: impl Iterator<Item = &'a str>) {
        for tag in tags {
            *self
                .places
                .entry(place)
                .or_default()
                .entry(tag.to_string())
                .or_default() += 1;
        }
    }

    /// The tags `place` carries on the selected rooms, with their counts.
    #[must_use]
    pub fn in_place(&self, place: SourceId) -> Option<&BTreeMap<String, usize>> {
        self.places.get(&place)
    }

    /// Whether `place` already carries `tag` on every room it can.
    #[must_use]
    pub fn all_have(&self, place: SourceId, tag: &str, only_own: bool) -> bool {
        let plan = bulk_plan(self.map_rooms, &self.own_rooms, place, only_own);
        plan.applicable > 0
            && self
                .in_place(place)
                .and_then(|tags| tags.get(tag))
                .is_some_and(|count| *count >= plan.applicable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    fn secret(n: u128) -> SourceId {
        SourceId::Secret(Uuid::from_u128(n))
    }

    fn index(tags: &[(&str, &[(SourceId, usize)])], places: &[SourceId]) -> TagIndex {
        TagIndex {
            places: places.to_vec(),
            tags: tags
                .iter()
                .map(|(tag, counts)| ((*tag).to_string(), counts.to_vec()))
                .collect(),
            rooms: HashMap::new(),
        }
    }

    #[test]
    fn the_input_reads_a_tag_as_the_server_does() {
        assert_eq!(typed("   "), Typed::Empty);
        assert_eq!(typed("  peace "), Typed::Tag("PEACE".to_string()));
        assert_eq!(
            typed(&"x".repeat(TAG_LIMIT)),
            Typed::Tag("X".repeat(TAG_LIMIT))
        );
        assert_eq!(typed(&"x".repeat(TAG_LIMIT + 1)), Typed::TooLong);
        // Characters, not bytes.
        assert_eq!(
            typed(&"é".repeat(TAG_LIMIT)),
            Typed::Tag("É".repeat(TAG_LIMIT))
        );
    }

    #[test]
    fn a_map_room_writes_add_to_and_a_place_room_its_own_place() {
        let vaults = secret(1);
        assert_eq!(destination(SourceId::Map, vaults), vaults);
        assert_eq!(
            destination(SourceId::Map, SourceId::Private),
            SourceId::Private
        );
        assert_eq!(destination(SourceId::Map, SourceId::Map), SourceId::Map);
        // A Secret room defaults to adding tags in its own source.
        assert_eq!(destination(vaults, SourceId::Private), vaults);
        assert_eq!(destination(SourceId::Private, vaults), SourceId::Private);
    }

    #[test]
    fn runs_go_own_place_then_add_to_then_color_order() {
        let (cellar, vaults) = (secret(1), secret(2));
        let places = [SourceId::Map, cellar, vaults, SourceId::Private];
        assert_eq!(
            run_order(SourceId::Map, vaults, &places),
            [SourceId::Map, vaults, cellar, SourceId::Private]
        );
        assert_eq!(
            run_order(SourceId::Map, SourceId::Map, &places),
            [SourceId::Map, cellar, vaults, SourceId::Private]
        );
        assert_eq!(
            run_order(SourceId::Map, SourceId::Private, &places),
            [SourceId::Map, SourceId::Private, cellar, vaults]
        );
        // A place's own room leads with its place.
        assert_eq!(
            run_order(vaults, SourceId::Map, &places),
            [vaults, SourceId::Map, cellar, SourceId::Private]
        );
    }

    #[test]
    fn suggestions_never_cross_into_a_less_private_place() {
        let (cellar, vaults) = (secret(1), secret(2));
        let places = [SourceId::Map, cellar, vaults, SourceId::Private];
        assert_eq!(suggestion_sources(SourceId::Map, &places), [SourceId::Map]);
        assert_eq!(suggestion_sources(vaults, &places), [SourceId::Map, vaults]);
        assert_eq!(suggestion_sources(SourceId::Private, &places), places);
        // Private suggests from itself before it has a layer.
        assert_eq!(
            suggestion_sources(SourceId::Private, &[SourceId::Map]),
            [SourceId::Map, SourceId::Private]
        );

        let index = index(
            &[
                ("PEACE", &[(SourceId::Map, 12)]),
                ("SHOP", &[(SourceId::Map, 3), (cellar, 9)]),
                ("VAULT", &[(vaults, 2)]),
                ("WINE", &[(cellar, 4)]),
                ("ME", &[(SourceId::Private, 7)]),
            ],
            &places,
        );
        let tags = |suggestions: Vec<Suggestion>| -> Vec<String> {
            suggestions.into_iter().map(|s| s.tag).collect()
        };
        // Into the map: the map's tags, most used first; SHOP counts only
        // its map uses.
        assert_eq!(
            tags(index.suggestions(SourceId::Map, "", |_| false)),
            ["PEACE", "SHOP"]
        );
        // Into Vaults: the map's and Vaults' own, never Cellar's or Private's.
        assert_eq!(
            tags(index.suggestions(vaults, "", |_| false)),
            ["PEACE", "SHOP", "VAULT"]
        );
        // Into Private: every readable place.
        assert_eq!(
            tags(index.suggestions(SourceId::Private, "", |_| false)),
            ["PEACE", "SHOP", "ME", "WINE", "VAULT"]
        );
        // A prefix narrows; what the destination has is left out.
        assert_eq!(
            tags(index.suggestions(SourceId::Private, " s", |_| false)),
            ["SHOP"]
        );
        assert_eq!(
            tags(index.suggestions(vaults, "", |tag| tag == "PEACE")),
            ["SHOP", "VAULT"]
        );
    }

    #[test]
    fn a_suggestion_names_the_map_first_then_the_destination() {
        let (cellar, vaults) = (secret(1), secret(2));
        let places = [SourceId::Map, cellar, vaults, SourceId::Private];
        let index = index(
            &[
                ("SHOP", &[(SourceId::Map, 1), (cellar, 9)]),
                ("WINE", &[(cellar, 4), (SourceId::Private, 1)]),
                ("VAULT", &[(vaults, 2)]),
            ],
            &places,
        );
        let used_in = |destination, tag: &str| {
            index
                .suggestions(destination, tag, |_| false)
                .into_iter()
                .find(|s| s.tag == tag)
                .map(|s| s.used_in)
        };
        assert_eq!(used_in(SourceId::Private, "SHOP"), Some(SourceId::Map));
        assert_eq!(used_in(SourceId::Private, "WINE"), Some(SourceId::Private));
        assert_eq!(used_in(vaults, "VAULT"), Some(vaults));
        assert_eq!(used_in(SourceId::Private, "VAULT"), Some(vaults));
    }

    #[test]
    fn at_most_eight_suggestions_show() {
        let names: Vec<String> = (0..12).map(|n| format!("T{n:02}")).collect();
        let counts = [(SourceId::Map, 1)];
        let tags: Vec<(&str, &[(SourceId, usize)])> = names
            .iter()
            .map(|name| (name.as_str(), &counts[..]))
            .collect();
        let index = index(&tags, &[SourceId::Map]);
        assert_eq!(
            index.suggestions(SourceId::Map, "t", |_| false).len(),
            SUGGESTION_LIMIT
        );
    }

    #[test]
    fn the_hint_names_a_more_private_place_holding_the_only_copies() {
        let (cellar, vaults) = (secret(1), secret(2));
        let places = [SourceId::Map, cellar, vaults, SourceId::Private];
        let index = index(
            &[
                ("PEACE", &[(SourceId::Map, 12), (vaults, 1)]),
                ("VAULT", &[(vaults, 2), (SourceId::Private, 1)]),
                ("ME", &[(SourceId::Private, 7)]),
            ],
            &places,
        );
        // Typing VAULT into the map: only Vaults and Private have it.
        assert_eq!(
            index.more_private_only(SourceId::Map, "VAULT"),
            Some(vaults)
        );
        assert_eq!(
            index.more_private_only(SourceId::Map, "ME"),
            Some(SourceId::Private)
        );
        // Into Cellar, another Secret's tag is no less private to say so of.
        assert_eq!(index.more_private_only(cellar, "VAULT"), Some(vaults));
        // Already somewhere at least as public: no hint.
        assert_eq!(index.more_private_only(SourceId::Map, "PEACE"), None);
        assert_eq!(index.more_private_only(vaults, "VAULT"), None);
        // Private is the most private place there is.
        assert_eq!(index.more_private_only(SourceId::Private, "VAULT"), None);
        // A new tag: nothing to warn about.
        assert_eq!(index.more_private_only(SourceId::Map, "NEW"), None);
    }

    #[test]
    fn a_bulk_add_can_annotate_every_readable_room_unless_linked_clan_rules_limit_it() {
        let (cellar, vaults) = (secret(1), secret(2));
        let own: BTreeMap<SourceId, usize> = [(cellar, 3), (vaults, 2)].into_iter().collect();
        assert_eq!(
            bulk_plan(35, &own, vaults, true),
            BulkPlan {
                applicable: 2,
                skipped: vec![(SourceId::Map, 35), (cellar, 3)],
            }
        );
        // Map-owned tags can also annotate rooms in other sources.
        assert_eq!(
            bulk_plan(35, &own, SourceId::Map, false),
            BulkPlan {
                applicable: 40,
                skipped: Vec::new(),
            }
        );
        assert_eq!(
            bulk_plan(0, &BTreeMap::new(), SourceId::Private, false),
            BulkPlan {
                applicable: 0,
                skipped: Vec::new(),
            }
        );
    }

    #[test]
    fn a_filter_names_a_whole_tag_ignoring_case() {
        assert_eq!(filter_tag("  vault "), Some("VAULT".to_string()));
        assert_eq!(filter_tag("   "), None);
        let mut index = index(&[], &[SourceId::Map]);
        index
            .rooms
            .insert("VAULT".to_string(), vec![(SourceId::Map, RoomNumber(4))]);
        let rooms = |filter: &str| filter_tag(filter).map_or(0, |tag| index.rooms_with(&tag).len());
        assert_eq!(rooms("Vault"), 1);
        // Whole tags only.
        assert_eq!(rooms("vaul"), 0);
        assert_eq!(rooms("vaults"), 0);
    }

    #[test]
    fn every_room_already_tagged_in_the_place_means_nothing_to_add() {
        let vaults = secret(2);
        let mut selection = SelectionTags {
            rooms: 4,
            map_rooms: 3,
            own_rooms: [(vaults, 1)].into_iter().collect(),
            places: BTreeMap::new(),
        };
        selection
            .places
            .insert(vaults, [("VAULT".to_string(), 3)].into_iter().collect());
        assert!(!selection.all_have(vaults, "VAULT", false));
        selection
            .places
            .insert(vaults, [("VAULT".to_string(), 4)].into_iter().collect());
        assert!(selection.all_have(vaults, "VAULT", false));
        assert!(!selection.all_have(SourceId::Map, "VAULT", false));
    }

    #[tokio::test]
    async fn the_index_reads_every_readable_place_and_follows_their_revisions() {
        use super::super::commands::CommandStack;
        use super::super::source_rooms::change_tags;

        let (mapper, area_id, secret) = super::super::source_rooms::tests::loaded().await;
        let area = |mapper: &smudgy_cloud::Mapper| {
            mapper
                .get_current_atlas()
                .get_area(&area_id)
                .expect("loaded")
        };
        let cache = IndexCache::default();
        let index = cache.get(&area(&mapper), true);
        // The Secret's DOOR on map room 2 lists under that map room.
        assert_eq!(index.rooms_with("DOOR"), [(SourceId::Map, RoomNumber(2))]);
        let tags: Vec<(&str, Vec<(SourceId, usize)>)> = index
            .tags()
            .map(|(tag, counts)| (tag, counts.to_vec()))
            .collect();
        assert_eq!(tags, [("DOOR", vec![(secret, 1)])]);
        // Without places only the map's own tags count: here, none.
        assert!(TagIndex::build(&area(&mapper), false).is_empty());
        // The same snapshot reads the same index.
        assert!(Rc::ptr_eq(&index, &cache.get(&area(&mapper), true)));

        // A write to the Secret, still waiting to save, replaces the map's
        // snapshot: the index follows.
        let change = change_tags(
            &area(&mapper),
            secret,
            &[(SourceId::Map, RoomNumber(1)), (secret, RoomNumber(3))],
            "wine",
            true,
            "Hidden",
        )
        .expect("neither room has it");
        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(&mapper, change.command);
        let rebuilt = cache.get(&area(&mapper), true);
        assert!(!Rc::ptr_eq(&index, &rebuilt));
        let mut wine = vec![(SourceId::Map, RoomNumber(1)), (secret, RoomNumber(3))];
        wine.sort_unstable();
        assert_eq!(rebuilt.rooms_with("WINE"), wine.as_slice());
        assert_eq!(
            rebuilt.more_private_only(SourceId::Map, "WINE"),
            Some(secret)
        );

        // So does a write to the map.
        let change = change_tags(
            &area(&mapper),
            SourceId::Map,
            &[(SourceId::Map, RoomNumber(1))],
            "peace",
            true,
            "Map",
        )
        .expect("room 1 lacks it");
        let _ = stack.push_and_apply(&mapper, change.command);
        let after_map = cache.get(&area(&mapper), true);
        assert!(!Rc::ptr_eq(&rebuilt, &after_map));
        assert_eq!(
            after_map.rooms_with("PEACE"),
            [(SourceId::Map, RoomNumber(1))]
        );
    }

    #[tokio::test]
    async fn the_selection_reads_each_places_tags_on_its_rooms() {
        let (mapper, area_id, secret) = super::super::source_rooms::tests::loaded().await;
        let area = mapper
            .get_current_atlas()
            .get_area(&area_id)
            .expect("loaded");
        let selection = SelectionTags::read(
            &area,
            [RoomNumber(1), RoomNumber(2)],
            [(secret, RoomNumber(3))],
        );
        assert_eq!(selection.rooms, 3);
        assert_eq!(selection.map_rooms, 2);
        assert_eq!(selection.own_rooms.get(&secret), Some(&1));
        assert_eq!(
            selection.in_place(secret).and_then(|tags| tags.get("DOOR")),
            Some(&1)
        );
        assert!(selection.in_place(SourceId::Map).is_none());
    }
}
