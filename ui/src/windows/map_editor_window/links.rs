//! The link editor's reading of a map: the links touching a room, the two
//! ends of a link in travel order, how rooms are named (by title and place,
//! never a bare number), where a link may live, how its two doors compare,
//! and the rooms a destination picker offers. Pure functions over the
//! cache; [`super::link_panel`] draws them and [`super::link_edits`]
//! changes them.

use std::collections::HashSet;

use iced::Color;
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::mapper::area_cache::AreaCache;
use smudgy_cloud::mapper::exit_cache::ExitCache;
use smudgy_cloud::{
    AreaId, ConnectionId, ExitDirection, ExitId, RoomAddress, RoomNumber, SourceId,
};
use smudgy_map_widget::map_editor::PlacedRoom;

use super::document::Document;

/// A room anywhere: its map, its place on that map, and its number there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RoomId {
    pub map: AreaId,
    pub place: SourceId,
    pub number: RoomNumber,
}

impl RoomId {
    /// `room`, placed on map `map`.
    #[must_use]
    pub fn on(map: AreaId, room: PlacedRoom) -> Self {
        Self {
            map,
            place: room.source,
            number: room.number,
        }
    }

    /// The room as the canvas places it, when it is on map `map`.
    #[must_use]
    pub fn placed_on(self, map: AreaId) -> Option<PlacedRoom> {
        (self.map == map).then_some(PlacedRoom {
            source: self.place,
            number: self.number,
        })
    }
}

/// Where an exit leads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Destination {
    Room(RoomId),
    /// A map that wasn't shared with the viewer.
    Unknown,
    /// Nowhere: the exit has no destination room.
    Nowhere,
}

impl Destination {
    #[must_use]
    pub fn room(self) -> Option<RoomId> {
        match self {
            Self::Room(room) => Some(room),
            Self::Unknown | Self::Nowhere => None,
        }
    }
}

/// The qualified room address in the document's map.
#[must_use]
pub fn doc_room(document: &Document<'_>, address: RoomAddress) -> RoomId {
    RoomId {
        map: document.area_id(),
        place: address.source,
        number: address.number,
    }
}

/// Where `exit`, kept in `document`, leads: a room of the document, another
/// map's room, or a room of another map's Secret (whose area id names it).
#[must_use]
pub fn destination(atlas: &AtlasCache, exit: &ExitCache) -> Destination {
    if exit.to_unknown {
        return Destination::Unknown;
    }
    let Some(destination) = exit.destination_address() else {
        return Destination::Nowhere;
    };
    let (map, place) = if destination.room.source.is_map() {
        atlas
            .source_of(&destination.map)
            .unwrap_or((destination.map, SourceId::Map))
    } else {
        (destination.map, destination.room.source)
    };
    Destination::Room(RoomId {
        map,
        place,
        number: destination.room.number,
    })
}

/// A room as the editor names it.
#[derive(Debug, Clone, PartialEq)]
pub struct RoomName {
    pub id: RoomId,
    /// The name of the room's map, when it is not the map being edited.
    pub map_name: Option<String>,
    /// The room's place, when it is not its map: its name and color.
    pub place: Option<(String, Option<Color>)>,
    /// The room's title; empty when it has none (or isn't loaded).
    pub title: String,
}

/// A picker's query, read once: a number (with or without `#`) names a
/// room's number; any other text is looked for in a room's title, its
/// place's name and its map's name, ignoring case. An empty query matches
/// every room.
struct Query {
    number: Option<i32>,
    text: String,
}

impl Query {
    fn new(query: &str) -> Self {
        let query = query.trim();
        let digits = query.strip_prefix('#').unwrap_or(query).trim();
        Self {
            number: digits.parse::<i32>().ok(),
            text: query.to_lowercase(),
        }
    }

    fn hits(
        &self,
        number: RoomNumber,
        title: &str,
        place: Option<&str>,
        map: Option<&str>,
    ) -> bool {
        if let Some(wanted) = self.number {
            return number.0 == wanted;
        }
        self.text.is_empty()
            || title.to_lowercase().contains(&self.text)
            || place.is_some_and(|place| place.to_lowercase().contains(&self.text))
            || map.is_some_and(|map| map.to_lowercase().contains(&self.text))
    }
}

/// `id` as the editor names it, from the map being edited (`here`).
#[must_use]
pub fn name(atlas: &AtlasCache, here: AreaId, id: RoomId) -> RoomName {
    let area = atlas.get_area(&id.map);
    let title = area
        .as_ref()
        .and_then(|area| match id.place {
            SourceId::Map => area
                .get_room(&id.number)
                .map(|room| room.get_title().to_string()),
            place => smudgy_map_widget::sources::source_room(area, place, id.number)
                .map(|room| room.get_title().to_string()),
        })
        .unwrap_or_default();
    let map_name = (id.map != here).then(|| {
        area.as_ref()
            .map(|area| area.get_name().to_string())
            .unwrap_or_default()
    });
    let place = (!id.place.is_map()).then(|| {
        area.as_ref().map_or((String::new(), None), |area| {
            (
                super::secrets::place_name(area, id.place),
                smudgy_map_widget::sources::source_color(area, id.place),
            )
        })
    });
    RoomName {
        id,
        map_name,
        place,
        title,
    }
}

/// How a link runs, seen from one of its rooms.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Way {
    /// Both ways: an exit at each end.
    Both,
    /// One way, leaving the room.
    Out,
    /// One way, arriving at the room.
    In,
}

/// An exit kept somewhere other than the link's own document: the return
/// of a link into another map, which that map (or one of its places) keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitAt {
    pub map: AreaId,
    pub place: SourceId,
    /// The room anchoring the exit, independent of the exit's owning place.
    pub room: RoomAddress,
    pub exit: ExitId,
    pub direction: ExitDirection,
}

/// One row of a room's Exits: one link.
#[derive(Debug, Clone)]
pub struct LinkRow {
    pub connection: ConnectionId,
    /// The place keeping the link.
    pub place: SourceId,
    pub way: Way,
    /// The direction the room's exit leaves in (`Both`, `Out`), or the one
    /// the far room's exit arrives from (`In`, when known).
    pub direction: Option<ExitDirection>,
    /// The direction the far room's exit leaves in (`Both`).
    pub back: Option<ExitDirection>,
    pub far: Destination,
    /// The room's own side of the passage (the far side's for `In`).
    pub door: DoorSide,
}

/// Every readable document with links on this qualified room. A retained
/// attachment stays in its original document after its room changes source.
fn documents_of(map: &AreaCache, room: PlacedRoom) -> Vec<(Document<'_>, RoomAddress)> {
    std::iter::once(Document::map(map))
        .chain(
            map.source_layers()
                .iter()
                .filter_map(|layer| Document::of(map, layer.source())),
        )
        .filter_map(|document| document.room_of(room).map(|number| (document, number)))
        .collect()
}

/// The exits of link `connection` in `content`, with their rooms, by id:
/// the exits of its end rooms (and of `at`, should its row be missing).
fn members(
    content: &AreaCache,
    connection: ConnectionId,
    at: RoomAddress,
) -> Vec<(RoomAddress, &ExitCache)> {
    let mut rooms = vec![at];
    if let Some(link) = content.get_connection(connection) {
        rooms.push(link.endpoint_a.address());
        rooms.extend(link.endpoint_b.map(|end| end.address()));
    }
    rooms.sort_unstable();
    rooms.dedup();
    let mut members: Vec<_> = rooms
        .iter()
        .filter_map(|number| content.get_room_at(*number))
        .flat_map(|room| {
            room.get_exits()
                .iter()
                .filter(|exit| exit.connection_id == connection)
                .map(move |exit| (room.address(), exit))
        })
        .collect();
    members.sort_by_key(|(_, exit)| exit.id.0);
    members
}

/// The exit another map (or one of its places) keeps from `far` back into
/// `back_to`: the return of a link into that map.
#[must_use]
pub fn return_from(atlas: &AtlasCache, far: RoomId, back_to: RoomId) -> Option<ExitAt> {
    let area = atlas.get_area(&far.map)?;
    let placed = PlacedRoom {
        source: far.place,
        number: far.number,
    };
    documents_of(&area, placed)
        .into_iter()
        .find_map(|(document, number)| {
            let room = document.content().get_room_at(number)?;
            room.get_exits().iter().find_map(|exit| {
                (destination(atlas, exit) == Destination::Room(back_to)).then_some(ExitAt {
                    map: far.map,
                    place: document.source(),
                    room: number,
                    exit: exit.id,
                    direction: exit.from_direction,
                })
            })
        })
}

/// One link through `number` of `document`, seen from that room.
fn row_for(
    atlas: &AtlasCache,
    document: &Document<'_>,
    number: RoomAddress,
    connection: ConnectionId,
) -> Option<LinkRow> {
    let members = members(document.content(), connection, number);
    let mine = members.iter().find(|(room, _)| *room == number);
    let other = members.iter().find(|(room, _)| *room != number);
    let row = |way, direction, back, far, door| LinkRow {
        connection,
        place: document.source(),
        way,
        direction,
        back,
        far,
        door,
    };
    Some(match (mine, other) {
        (Some((_, exit)), Some((far, back))) => row(
            Way::Both,
            Some(exit.from_direction),
            Some(back.from_direction),
            Destination::Room(doc_room(document, *far)),
            DoorSide::of(exit),
        ),
        (Some((_, exit)), None) => {
            let far = destination(atlas, exit);
            let returning = far
                .room()
                .filter(|far| far.map != document.area_id())
                .and_then(|far| return_from(atlas, far, doc_room(document, number)));
            match returning {
                Some(back) => row(
                    Way::Both,
                    Some(exit.from_direction),
                    Some(back.direction),
                    far,
                    DoorSide::of(exit),
                ),
                None => row(
                    Way::Out,
                    Some(exit.from_direction),
                    None,
                    far,
                    DoorSide::of(exit),
                ),
            }
        }
        (None, Some((far, exit))) => row(
            Way::In,
            exit.to_direction,
            None,
            Destination::Room(doc_room(document, *far)),
            DoorSide::of(exit),
        ),
        (None, None) => return None,
    })
}

/// Where a direction sorts among a room's exits: compass order, then up,
/// down, in, out, then the rest.
fn direction_rank(direction: Option<ExitDirection>) -> usize {
    direction.map_or(ExitDirection::ALL.len(), |direction| {
        ExitDirection::ALL
            .iter()
            .position(|candidate| *candidate == direction)
            .unwrap_or(ExitDirection::ALL.len())
    })
}

/// Every link touching `room` of `map`, one row each, wherever it is kept
/// (the map, or a place keeping it on a map room): the room's own exits by
/// direction, then the links arriving one way.
#[must_use]
pub fn room_links(atlas: &AtlasCache, map: &AreaCache, room: PlacedRoom) -> Vec<LinkRow> {
    let mut rows = Vec::new();
    for (document, number) in documents_of(map, room) {
        let content = document.content();
        let mut seen = HashSet::new();
        let exits = content
            .get_room_at(number)
            .map(|room| room.get_exits())
            .unwrap_or_default();
        for exit in exits {
            if seen.insert(exit.connection_id)
                && let Some(row) = row_for(atlas, &document, number, exit.connection_id)
            {
                rows.push(row);
            }
        }
        for link in content.get_connections() {
            let touches = link.endpoint_a.address() == number
                || link.endpoint_b.is_some_and(|end| end.address() == number);
            if touches
                && seen.insert(link.id)
                && let Some(row) = row_for(atlas, &document, number, link.id)
            {
                rows.push(row);
            }
        }
    }
    rows.sort_by_key(|row| {
        (
            row.way == Way::In,
            direction_rank(row.direction),
            row.connection.0,
        )
    });
    rows
}

/// The directions `room`'s own exits leave in, for the compass.
#[must_use]
pub fn used_directions(rows: &[LinkRow]) -> HashSet<ExitDirection> {
    rows.iter()
        .filter(|row| row.way != Way::In)
        .filter_map(|row| row.direction)
        .collect()
}

/// One end of a link in the link editor.
#[derive(Debug, Clone)]
pub struct LinkEnd {
    pub room: Destination,
    /// The room in the link's document; `None` for another map's room.
    pub doc_room: Option<RoomAddress>,
    /// The exit leaving this end, kept with the link.
    pub exit: Option<ExitCache>,
}

/// A link as the link editor shows it: its two ends in travel order.
#[derive(Debug, Clone)]
pub struct LinkView {
    pub connection: ConnectionId,
    /// The place keeping the link.
    pub place: SourceId,
    pub from: LinkEnd,
    pub to: LinkEnd,
    /// The far end's exit when another map keeps it: where, and the exit.
    pub return_elsewhere: Option<ExitAt>,
    pub return_exit: Option<ExitCache>,
}

impl LinkView {
    /// Whether the link runs both ways.
    #[must_use]
    pub fn two_way(&self) -> bool {
        self.to.exit.is_some() || self.return_elsewhere.is_some()
    }

    /// The link's rooms, where known.
    #[must_use]
    pub fn rooms(&self) -> Vec<RoomId> {
        [self.from.room.room(), self.to.room.room()]
            .into_iter()
            .flatten()
            .collect()
    }
}

/// Link `connection` of `map` with its ends in travel order: a one-way
/// link's From is the room its exit leaves; a two-way link's is `anchor`
/// (a room of the link's document) when that is one of its rooms, else its
/// first end.
#[must_use]
pub fn link_view(
    atlas: &AtlasCache,
    map: &AreaCache,
    connection: ConnectionId,
    anchor: Option<RoomAddress>,
) -> Option<LinkView> {
    let document = Document::of_connection(map, connection)?;
    let content = document.content();
    let link = content.get_connection(connection)?;
    let members = members(content, connection, link.endpoint_a.address());
    let end = |number: RoomAddress, exit: &ExitCache| LinkEnd {
        room: Destination::Room(doc_room(&document, number)),
        doc_room: Some(number),
        exit: Some(exit.clone()),
    };
    let (from, to, return_elsewhere) = match members.as_slice() {
        [(a, first), (b, second)] => {
            let flipped = anchor == Some(*b) && anchor != Some(*a);
            if flipped {
                (end(*b, second), end(*a, first), None)
            } else {
                (end(*a, first), end(*b, second), None)
            }
        }
        [(number, exit)] => {
            let far = destination(atlas, exit);
            let doc_far = exit
                .destination_address()
                .filter(|destination| destination.map == document.area_id())
                .map(|destination| destination.room);
            let returning = far
                .room()
                .filter(|far| far.map != document.area_id())
                .and_then(|far| return_from(atlas, far, doc_room(&document, *number)));
            (
                end(*number, exit),
                LinkEnd {
                    room: far,
                    doc_room: doc_far,
                    exit: None,
                },
                returning,
            )
        }
        _ => return None,
    };
    let return_exit = return_elsewhere.and_then(|back| exit_kept_at(atlas, back));
    Some(LinkView {
        connection,
        place: document.source(),
        from,
        to,
        return_elsewhere,
        return_exit,
    })
}

/// The exit `at` names, from its map's place.
#[must_use]
pub fn exit_kept_at(atlas: &AtlasCache, at: ExitAt) -> Option<ExitCache> {
    let area = atlas.get_area(&at.map)?;
    let document = Document::of(&area, at.place)?;
    document
        .content()
        .get_room_at(at.room)?
        .get_exits()
        .iter()
        .find(|exit| exit.id == at.exit)
        .cloned()
}

/// A door's state. Locked implies closed. Mudlet's door states (0 none,
/// 1 open, 2 closed, 3 locked) map onto these one to one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum DoorState {
    #[default]
    None,
    Open,
    Closed,
    Locked,
}

/// One side of a passage: its door, and whether its exit is hidden.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DoorSide {
    pub state: DoorState,
    pub name: Option<String>,
    pub opens_with: Option<String>,
    pub hidden: bool,
}

impl DoorSide {
    /// The side `exit` keeps.
    #[must_use]
    pub fn of(exit: &ExitCache) -> Self {
        let Some(door) = &exit.door else {
            return Self {
                hidden: exit.is_hidden,
                ..Self::default()
            };
        };
        Self {
            state: match door.state {
                smudgy_cloud::DoorState::Open => DoorState::Open,
                smudgy_cloud::DoorState::Closed => DoorState::Closed,
                smudgy_cloud::DoorState::Locked => DoorState::Locked,
            },
            name: door.name.clone(),
            opens_with: door.opens_with.clone(),
            hidden: exit.is_hidden,
        }
    }

    /// The exit's door this side describes; `None` without one.
    #[must_use]
    pub fn door(&self) -> Option<smudgy_cloud::Door> {
        let state = match self.state {
            DoorState::None => return None,
            DoorState::Open => smudgy_cloud::DoorState::Open,
            DoorState::Closed => smudgy_cloud::DoorState::Closed,
            DoorState::Locked => smudgy_cloud::DoorState::Locked,
        };
        Some(smudgy_cloud::Door {
            state,
            name: self.name.clone(),
            opens_with: self.opens_with.clone(),
        })
    }

    /// This side with its door set to `state`; none clears the door's name
    /// and what opens it.
    #[must_use]
    pub fn with_state(mut self, state: DoorState) -> Self {
        self.state = state;
        if state == DoorState::None {
            self.name = None;
            self.opens_with = None;
        }
        self
    }

    /// Writes this side into an exit update: its hidden flag, and its door
    /// whole (none removes the door with its name and command).
    pub fn write(&self, updates: &mut smudgy_cloud::ExitUpdates) {
        updates.is_hidden = Some(self.hidden);
        updates.door = Some(self.door());
    }
}

/// Whether a link's doors show as one block ("same on both sides"): on a
/// two-way link whose two sides agree, unless the viewer chose to see them
/// apart (`apart`). A one-way link has one side and one block.
#[must_use]
pub fn doors_shown_as_one(from: &DoorSide, to: Option<&DoorSide>, apart: bool) -> bool {
    match to {
        Some(to) => !apart && from == to,
        None => true,
    }
}

/// Where a link may live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkPlaces {
    /// Only where it is: an end in one of the map's places decides it, or
    /// the viewer can't move it.
    Fixed(SourceId),
    /// Where it is (first), and every place it may move to.
    Choice(Vec<SourceId>),
}

/// Where a link kept in `current`, with ends `ends`, may live on `map`. An
/// end in one of this map's Secrets (or Private additions) keeps the link
/// there. Otherwise (both ends map rooms, or one in another map, its map
/// or one of its Secrets) it may move to any place of this map the viewer
/// may add to, when the viewer may remove it where it is (what a move
/// takes there). A clan's Secret on a map filed by link holds no map rooms'
/// links.
#[must_use]
pub fn link_places(map: &AreaCache, current: SourceId, ends: &[RoomId]) -> LinkPlaces {
    let here = *map.get_id();
    if let Some(end) = ends
        .iter()
        .find(|end| end.map == here && !end.place.is_map())
    {
        return LinkPlaces::Fixed(end.place);
    }
    if !super::secrets::can_remove(map, current) {
        return LinkPlaces::Fixed(current);
    }
    let mut candidates = vec![SourceId::Map];
    candidates.extend(
        super::secrets::secrets(map)
            .iter()
            .map(|bundle| bundle.source),
    );
    candidates.push(SourceId::Private);
    let mut places = vec![current];
    places.extend(candidates.into_iter().filter(|place| {
        *place != current
            && super::secrets::can_add(map, *place)
            && !map.keeps_only_own_rooms(*place)
    }));
    if places.len() == 1 {
        LinkPlaces::Fixed(current)
    } else {
        LinkPlaces::Choice(places)
    }
}

/// The place a link between `ends` (rooms of `map`, or another map's) goes
/// into: a place's own room at either end decides it; otherwise `fallback`.
/// `None` when the ends are two different places' rooms, which no link may
/// join.
#[must_use]
pub fn place_for(map: AreaId, ends: &[RoomId], fallback: SourceId) -> Option<SourceId> {
    let mut own = ends
        .iter()
        .filter(|end| end.map == map && !end.place.is_map())
        .map(|end| end.place);
    match (own.next(), own.next()) {
        (Some(first), Some(second)) if first != second => None,
        (Some(place), _) => Some(place),
        (None, _) => Some(fallback),
    }
}

/// A room a picker lists, nearest first where that applies.
#[derive(Debug, Clone, Copy)]
pub struct Near {
    pub level: i32,
    pub x: f32,
    pub y: f32,
}

/// The rooms a picker found, at most a limit of them, and how many more
/// matched.
#[derive(Debug, Clone, Default)]
pub struct Found {
    pub rooms: Vec<RoomName>,
    pub more: usize,
}

/// Where a picker room is: its place's and its map's names, when it is not
/// on the map being edited's own ground.
#[derive(Clone, Copy)]
struct Ground<'a> {
    place: Option<&'a str>,
    map: Option<&'a str>,
}

impl Found {
    /// Adds room `id` (titled `title`, on `ground`) when `query` hits it:
    /// named while fewer than `limit` are listed, else counted. Only the
    /// rooms listed are named, so a large map costs little per redraw.
    #[allow(clippy::too_many_arguments)]
    fn offer(
        &mut self,
        atlas: &AtlasCache,
        here: AreaId,
        query: &Query,
        id: RoomId,
        title: &str,
        ground: Ground<'_>,
        limit: usize,
    ) {
        if !query.hits(id.number, title, ground.place, ground.map) {
            return;
        }
        if self.rooms.len() < limit {
            self.rooms.push(name(atlas, here, id));
        } else {
            self.more += 1;
        }
    }
}

/// `(rank, number)` pairs in picker order: nearest first, then by number.
fn by_nearness(rooms: &mut [((bool, f32), RoomNumber)]) {
    rooms.sort_by(|a, b| {
        a.0.0
            .cmp(&b.0.0)
            .then(a.0.1.total_cmp(&b.0.1))
            .then(a.1.cmp(&b.1))
    });
}

/// What the destination picker lists for this map: its rooms matching
/// `query`, nearest `near` first (its level first, then by distance), and
/// then the own rooms of each of `places` (in the map's place order, each
/// nearest first). `exclude` (the room the link starts from) is left out.
#[must_use]
pub fn this_map_picks(
    atlas: &AtlasCache,
    map: &AreaCache,
    near: Option<Near>,
    exclude: Option<RoomId>,
    places: &[SourceId],
    query: &str,
    limit: usize,
) -> (Found, Found) {
    let here = *map.get_id();
    let query = Query::new(query);
    let order = |level: i32, x: f32, y: f32| {
        near.map_or((false, 0.0), |near| {
            (
                level != near.level,
                (x - near.x).hypot(y - near.y) + (level - near.level).abs() as f32 * 0.001,
            )
        })
    };
    let mut map_rooms: Vec<((bool, f32), RoomNumber)> = map
        .get_rooms()
        .iter()
        .map(|room| {
            (
                order(room.get_level(), room.get_x(), room.get_y()),
                room.get_room_number(),
            )
        })
        .collect();
    by_nearness(&mut map_rooms);
    let mut this_map = Found::default();
    let ground = Ground {
        place: None,
        map: None,
    };
    for (_, number) in map_rooms {
        let id = RoomId {
            map: here,
            place: SourceId::Map,
            number,
        };
        if Some(id) == exclude {
            continue;
        }
        let title = map.get_room(&number).map_or("", |room| room.get_title());
        this_map.offer(atlas, here, &query, id, title, ground, limit);
    }

    let mut place_rooms = Found::default();
    for layer in map.source_layers() {
        if !places.contains(&layer.source()) {
            continue;
        }
        let place_name = super::secrets::place_name(map, layer.source());
        let ground = Ground {
            place: Some(&place_name),
            map: None,
        };
        let content = layer.content();
        let mut own: Vec<((bool, f32), RoomNumber)> = content
            .get_rooms()
            .iter()
            .map(|room| {
                (
                    order(room.get_level(), room.get_x(), room.get_y()),
                    room.get_room_number(),
                )
            })
            .collect();
        by_nearness(&mut own);
        for (_, number) in own {
            let id = RoomId {
                map: here,
                place: layer.source(),
                number,
            };
            if Some(id) == exclude {
                continue;
            }
            let title = content
                .get_room(&number)
                .map_or("", |room| room.get_title());
            place_rooms.offer(atlas, here, &query, id, title, ground, limit);
        }
    }
    (this_map, place_rooms)
}

/// The maps a link from map `here` can't lead into, given the session's
/// maps (`session`) and those a local tier serves (`local`): the session's
/// maps, which vanish with it, and every map of another tier. A cloud map's
/// exits reach only cloud maps (the server knows no other), a local map's
/// only local maps; a session map's reach every map that lasts.
#[must_use]
pub fn unreachable_maps(
    atlas: &AtlasCache,
    here: AreaId,
    session: &HashSet<AreaId>,
    local: &HashSet<AreaId>,
) -> HashSet<AreaId> {
    let here_lasts = !session.contains(&here);
    let here_local = local.contains(&here);
    atlas
        .areas()
        .map(|area| *area.get_id())
        .filter(|id| session.contains(id) || (here_lasts && local.contains(id) != here_local))
        .collect()
}

/// The other maps a link may lead into, by name, matching `query` by name:
/// every map but `here` and those in `excluded` (the maps it can't reach,
/// [`unreachable_maps`]).
#[must_use]
pub fn other_maps(
    atlas: &AtlasCache,
    here: AreaId,
    excluded: &HashSet<AreaId>,
    query: &str,
) -> Vec<(AreaId, String)> {
    let query = query.trim().to_lowercase();
    let mut maps: Vec<(AreaId, String)> = atlas
        .areas()
        .filter(|area| *area.get_id() != here && !excluded.contains(area.get_id()))
        .map(|area| (*area.get_id(), area.get_name().to_string()))
        .filter(|(_, name)| query.is_empty() || name.to_lowercase().contains(&query))
        .collect();
    maps.sort_by_cached_key(|(_, name)| name.to_lowercase());
    maps
}

/// Another map's rooms matching `query`, by number, then (with `secrets`)
/// the own rooms of each of its Secrets the viewer reads.
#[must_use]
pub fn other_map_picks(
    atlas: &AtlasCache,
    here: AreaId,
    other: &AreaCache,
    secrets: bool,
    query: &str,
    limit: usize,
) -> Found {
    let map = *other.get_id();
    let query = Query::new(query);
    let map_name = other.get_name().to_string();
    let mut found = Found::default();
    let mut numbers: Vec<RoomNumber> = other
        .get_rooms()
        .iter()
        .map(|room| room.get_room_number())
        .collect();
    numbers.sort_unstable();
    let ground = Ground {
        place: None,
        map: Some(&map_name),
    };
    for number in numbers {
        let title = other.get_room(&number).map_or("", |room| room.get_title());
        let id = RoomId {
            map,
            place: SourceId::Map,
            number,
        };
        found.offer(atlas, here, &query, id, title, ground, limit);
    }
    if secrets {
        for layer in other.source_layers() {
            if !layer.source().is_secret() {
                continue;
            }
            let place_name = super::secrets::place_name(other, layer.source());
            let ground = Ground {
                place: Some(&place_name),
                map: Some(&map_name),
            };
            let content = layer.content();
            let mut own: Vec<RoomNumber> = content
                .get_rooms()
                .iter()
                .map(|room| room.get_room_number())
                .collect();
            own.sort_unstable();
            for number in own {
                let title = content
                    .get_room(&number)
                    .map_or("", |room| room.get_title());
                let id = RoomId {
                    map,
                    place: layer.source(),
                    number,
                };
                found.offer(atlas, here, &query, id, title, ground, limit);
            }
        }
    }
    found
}

/// The places whose own rooms a link starting at `origin` (a room of map
/// `map`) may lead to on that map: only the origin's place for a place's
/// own room (two places are never linked), else every place the viewer may
/// add to that holds map rooms' links.
#[must_use]
pub fn reachable_places(map: &AreaCache, origin: Option<RoomId>) -> Vec<SourceId> {
    let here = *map.get_id();
    if let Some(origin) = origin.filter(|origin| origin.map == here && !origin.place.is_map()) {
        return vec![origin.place];
    }
    map.source_layers()
        .iter()
        .map(smudgy_cloud::mapper::area_cache::SourceLayer::source)
        .filter(|place| super::secrets::can_add(map, *place) && !map.keeps_only_own_rooms(*place))
        .collect()
}

/// Where a room sits, for "nearest first".
#[must_use]
pub fn near(atlas: &AtlasCache, id: RoomId) -> Option<Near> {
    let area = atlas.get_area(&id.map)?;
    let room = match id.place {
        SourceId::Map => area.get_room(&id.number)?,
        place => smudgy_map_widget::sources::source_room(&area, place, id.number)?,
    };
    Some(Near {
        level: room.get_level(),
        x: room.get_x(),
        y: room.get_y(),
    })
}

/// Two maps with links of every kind, for the link editor's tests.
#[cfg(test)]
pub(super) mod fixture {
    use std::sync::Arc;

    use async_trait::async_trait;
    use serde_json::json;
    use smudgy_cloud::mutation::{MutationEnvelope, MutationResult};
    use smudgy_cloud::{
        Area, AreaId, AreaUpdates, AreaWithDetails, CloudResult, ConnectionId, CreateAreaRequest,
        ExitId, Mapper, MapperBackend, SourceId, Uuid,
    };

    pub const KEEP: &str = "0000000a-0000-4000-8000-00000000000a";
    pub const CATACOMBS: &str = "0000000b-0000-4000-8000-00000000000b";
    pub const SECRET: &str = "0000000c-0000-4000-8000-00000000000c";
    /// The Catacombs' Secret.
    pub const CRYPT: &str = "0000000d-0000-4000-8000-00000000000d";
    /// Keep #1 north ⇄ #2 south.
    pub const C_GATE: &str = "00000000-0000-4000-8000-0000000000c1";
    /// Keep #1 east → #3.
    pub const C_GARDEN: &str = "00000000-0000-4000-8000-0000000000c2";
    /// Keep #4 south → #1 (arriving from the north).
    pub const C_FERRY: &str = "00000000-0000-4000-8000-0000000000c3";
    /// Keep #1 down → Catacombs #7; Catacombs keeps the way back.
    pub const C_OSSUARY: &str = "00000000-0000-4000-8000-0000000000c4";
    /// Bookshelf's: Keep #1 west ⇄ Bookshelf #1.
    pub const C_LIBRARY: &str = "00000000-0000-4000-8000-0000000000c5";
    /// Catacombs #7 up → Keep #1.
    pub const C_BACK: &str = "00000000-0000-4000-8000-0000000000c6";
    pub const E_GATE_N: &str = "00000000-0000-4000-8000-0000000000e1";
    pub const E_GATE_S: &str = "00000000-0000-4000-8000-0000000000e2";
    pub const E_GARDEN: &str = "00000000-0000-4000-8000-0000000000e3";
    pub const E_FERRY: &str = "00000000-0000-4000-8000-0000000000e4";
    pub const E_OSSUARY: &str = "00000000-0000-4000-8000-0000000000e5";
    pub const E_LIBRARY_W: &str = "00000000-0000-4000-8000-0000000000e6";
    pub const E_LIBRARY_E: &str = "00000000-0000-4000-8000-0000000000e7";
    pub const E_BACK: &str = "00000000-0000-4000-8000-0000000000e8";

    pub fn exit_id(id: &str) -> ExitId {
        ExitId(Uuid::parse_str(id).expect("an exit id"))
    }

    pub fn link(id: &str) -> ConnectionId {
        ConnectionId(Uuid::parse_str(id).expect("a link id"))
    }

    pub fn area(id: &str) -> AreaId {
        AreaId(Uuid::parse_str(id).expect("a map id"))
    }

    pub fn secret() -> SourceId {
        SECRET.parse().expect("a Secret id")
    }

    pub fn crypt() -> SourceId {
        CRYPT.parse().expect("a Secret id")
    }

    /// Serves its maps and accepts every write.
    struct Maps(
        Vec<AreaWithDetails>,
        Option<smudgy_cloud::access_review::AccessReview>,
    );

    #[async_trait]
    impl MapperBackend for Maps {
        async fn review_move_content(
            &self,
            _area: &AreaId,
            _request: &smudgy_cloud::mutation::MoveRequest,
            _generation: u64,
        ) -> CloudResult<smudgy_cloud::access_review::AccessReview> {
            self.1.clone().ok_or_else(|| {
                smudgy_cloud::CloudError::InvalidInput(
                    "No review configured in this UI fixture".into(),
                )
            })
        }
        async fn create_area(&self, _request: CreateAreaRequest) -> CloudResult<Area> {
            unreachable!("the tests create no maps")
        }
        async fn list_areas(&self) -> CloudResult<Vec<Area>> {
            Ok(self.0.iter().map(|map| map.area.clone()).collect())
        }
        async fn get_area(&self, area_id: &AreaId) -> CloudResult<AreaWithDetails> {
            self.0
                .iter()
                .find(|map| map.area.id == *area_id)
                .cloned()
                .ok_or(smudgy_cloud::CloudError::AreaNotFound(*area_id))
        }
        async fn update_area(&self, _area_id: &AreaId, _updates: AreaUpdates) -> CloudResult<()> {
            Ok(())
        }
        async fn delete_area(&self, _area_id: &AreaId) -> CloudResult<()> {
            Ok(())
        }
        async fn execute_mutation(
            &self,
            area_id: &AreaId,
            envelope: &MutationEnvelope,
        ) -> CloudResult<MutationResult> {
            Ok(MutationResult {
                operation_id: envelope.operation_id,
                versions: vec![smudgy_cloud::mutation::VersionInfo::map_source(
                    area_id.0, 100,
                )],
                data: Vec::new(),
            })
        }
    }

    fn room(
        number: i32,
        title: &str,
        x: f32,
        y: f32,
        level: i32,
        exits: serde_json::Value,
    ) -> serde_json::Value {
        json!({
            "room_number": number, "title": title, "description": "", "color": "",
            "level": level, "x": x, "y": y, "properties": [], "exits": exits, "tags": []
        })
    }

    /// An exit of map `map` (a room of the place keeping it when `own`).
    fn exit(
        id: &str,
        direction: &str,
        map: &str,
        to: i32,
        back: &str,
        own: bool,
        link: &str,
    ) -> serde_json::Value {
        let mut exit = json!({
            "id": id, "from_direction": direction, "to_area_id": map, "to_room_number": to,
            "to_direction": back, "to_unknown": false, "path": "", "command": "", "weight": 1.0,
            "connection_id": link, "is_hidden": false, "door": null
        });
        if own {
            exit["to_source"] = json!(SECRET);
        }
        exit
    }

    fn end(number: i32, side: &str, own: bool) -> serde_json::Value {
        let mut end = json!({
            "room_number": number, "side": side, "port_offset": 0.5, "port_mode": "AutoPinned"
        });
        if own {
            end["source"] = json!(SECRET);
        }
        end
    }

    fn connection(
        id: &str,
        kind: &str,
        a: serde_json::Value,
        b: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let mut link = json!({
            "id": id, "endpoint_a": a, "kind": kind, "routing": "Simple",
            "segment_shape": "Direct", "corner": "Sharp", "route_points": [], "dash": "Solid",
            "color": "#A4A4A4", "thickness": 1.0
        });
        if let Some(b) = b {
            link["endpoint_b"] = b;
        }
        link
    }

    /// The Keep: #1 Hall at (0, 0) with #2 West Gate north of it, #3 Garden
    /// east, #4 Ferry Dock south, and #5 Tower one level up. Its Secret
    /// Bookshelf has its own #1 Hidden Library, linked both ways with the
    /// Hall's west side; Hidden Library sits west of the Hall. The
    /// Catacombs: #7 Ossuary, which the Hall leads down into and which leads
    /// back up. Its Secret Crypt has its own #1 Bone Altar.
    pub async fn maps() -> Mapper {
        maps_where(&["read", "add", "edit", "remove"]).await
    }

    /// [`maps`], with the viewer holding `actions` on Bookshelf.
    pub async fn maps_where(actions: &[&str]) -> Mapper {
        serving(the_maps(actions)).await
    }

    /// The Keep and the Catacombs as served, with the viewer holding
    /// `actions` on Bookshelf.
    pub fn the_maps(actions: &[&str]) -> Vec<AreaWithDetails> {
        let keep: AreaWithDetails = serde_json::from_value(json!({
            "id": KEEP, "user_id": null, "atlas_id": null, "name": "Keep",
            "created_at": "2026-10-06T00:00:00Z",
            "format_version": smudgy_cloud::AREA_FORMAT_VERSION,
            "properties": [], "labels": [], "shapes": [],
            "rooms": [
                room(1, "Hall", 0.0, 0.0, 0, json!([
                    exit(E_GATE_N, "North", KEEP, 2, "South", false, C_GATE),
                    exit(E_GARDEN, "East", KEEP, 3, "West", false, C_GARDEN),
                    exit(E_OSSUARY, "Down", CATACOMBS, 7, "Up", false, C_OSSUARY),
                ])),
                room(2, "West Gate", 0.0, -1.0, 0, json!([
                    exit(E_GATE_S, "South", KEEP, 1, "North", false, C_GATE),
                ])),
                room(3, "Garden", 1.0, 0.0, 0, json!([])),
                room(4, "Ferry Dock", 0.0, 1.0, 0, json!([
                    exit(E_FERRY, "South", KEEP, 1, "North", false, C_FERRY),
                ])),
                room(5, "Tower", 0.0, 0.0, 1, json!([])),
            ],
            "connections": [
                connection(C_GATE, "Internal", end(1, "North", false), Some(end(2, "South", false))),
                connection(C_GARDEN, "Internal", end(1, "East", false), Some(end(3, "West", false))),
                connection(C_FERRY, "Internal", end(1, "North", false), Some(end(4, "South", false))),
                connection(C_OSSUARY, "External", end(1, "South", false), None),
            ],
            "sources": [{
                "source": SECRET, "name": "Bookshelf", "ownership": "owner", "rev": 2,
                "actions": actions,
                "rooms": [
                    room(1, "Hidden Library", -1.0, 0.0, 0, json!([
                        exit(E_LIBRARY_E, "East", KEEP, 1, "West", false, C_LIBRARY),
                    ])),
                ],
                "room_data": [{
                    "room_number": 1, "properties": [], "tags": [],
                    "exits": [exit(E_LIBRARY_W, "West", KEEP, 1, "East", true, C_LIBRARY)]
                }],
                "connections": [
                    connection(C_LIBRARY, "Internal", end(1, "West", false), Some(end(1, "East", true))),
                ],
                "labels": []
            }]
        }))
        .expect("the Keep");
        let catacombs: AreaWithDetails = serde_json::from_value(json!({
            "id": CATACOMBS, "user_id": null, "atlas_id": null, "name": "Catacombs",
            "created_at": "2026-10-06T00:00:00Z",
            "format_version": smudgy_cloud::AREA_FORMAT_VERSION,
            "properties": [], "labels": [], "shapes": [],
            "rooms": [
                room(7, "Ossuary", 0.0, 0.0, 0, json!([
                    exit(E_BACK, "Up", KEEP, 1, "Down", false, C_BACK),
                ])),
            ],
            "connections": [
                connection(C_BACK, "External", end(7, "North", false), None),
            ],
            "sources": [{
                "source": CRYPT, "name": "Crypt", "ownership": "owner", "rev": 1,
                "actions": ["read", "add", "edit", "remove"],
                "rooms": [room(1, "Bone Altar", 0.0, 1.0, 0, json!([]))],
                "connections": [], "labels": []
            }]
        }))
        .expect("the Catacombs");
        vec![keep, catacombs]
    }

    /// A mapper with `maps` loaded, accepting every write.
    pub async fn serving(maps: Vec<AreaWithDetails>) -> Mapper {
        serving_with_review(maps, None).await
    }

    pub async fn serving_with_review(
        maps: Vec<AreaWithDetails>,
        review: Option<smudgy_cloud::access_review::AccessReview>,
    ) -> Mapper {
        let dir = std::env::temp_dir().join(format!("smudgy-links-{}", Uuid::new_v4()));
        let mapper = Mapper::new(Arc::new(Maps(maps, review)), dir);
        mapper.load_all_areas().await.expect("loads");
        mapper
    }
}

#[cfg(test)]
mod tests {
    use super::fixture::*;
    use super::*;

    fn keep_room(number: i32) -> RoomId {
        RoomId {
            map: area(KEEP),
            place: SourceId::Map,
            number: RoomNumber(number),
        }
    }

    fn library() -> RoomId {
        RoomId {
            map: area(KEEP),
            place: secret(),
            number: RoomNumber(1),
        }
    }

    fn ossuary() -> RoomId {
        RoomId {
            map: area(CATACOMBS),
            place: SourceId::Map,
            number: RoomNumber(7),
        }
    }

    /// The Hall's links, one row each, wherever they are kept: its own
    /// exits by direction, the Secret's passage among them, the way into
    /// the Catacombs two-way through the exit the Catacombs keeps back, and
    /// the one-way link arriving from the Ferry Dock last.
    #[tokio::test]
    async fn a_rooms_links_group_into_one_row_each() {
        let mapper = maps().await;
        let atlas = mapper.get_current_atlas();
        let keep = atlas.get_area(&area(KEEP)).expect("loaded");
        let rows = room_links(&atlas, &keep, PlacedRoom::map(RoomNumber(1)));
        let summary: Vec<_> = rows
            .iter()
            .map(|row| {
                (
                    row.connection,
                    row.place,
                    row.way,
                    row.direction,
                    row.back,
                    row.far,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (
                    link(C_GATE),
                    SourceId::Map,
                    Way::Both,
                    Some(ExitDirection::North),
                    Some(ExitDirection::South),
                    Destination::Room(keep_room(2))
                ),
                (
                    link(C_GARDEN),
                    SourceId::Map,
                    Way::Out,
                    Some(ExitDirection::East),
                    None,
                    Destination::Room(keep_room(3))
                ),
                (
                    link(C_LIBRARY),
                    secret(),
                    Way::Both,
                    Some(ExitDirection::West),
                    Some(ExitDirection::East),
                    Destination::Room(library())
                ),
                (
                    link(C_OSSUARY),
                    SourceId::Map,
                    Way::Both,
                    Some(ExitDirection::Down),
                    Some(ExitDirection::Up),
                    Destination::Room(ossuary())
                ),
                (
                    link(C_FERRY),
                    SourceId::Map,
                    Way::In,
                    Some(ExitDirection::North),
                    None,
                    Destination::Room(keep_room(4))
                ),
            ]
        );
        assert_eq!(
            used_directions(&rows),
            [
                ExitDirection::North,
                ExitDirection::East,
                ExitDirection::West,
                ExitDirection::Down
            ]
            .into_iter()
            .collect()
        );

        // From the Secret's own room, only its passage.
        let rows = room_links(
            &atlas,
            &keep,
            PlacedRoom {
                source: secret(),
                number: RoomNumber(1),
            },
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].connection, link(C_LIBRARY));
        assert_eq!(rows[0].direction, Some(ExitDirection::East));
        assert_eq!(rows[0].far, Destination::Room(keep_room(1)));
    }

    /// Rooms are named by title and place: a place's room by the place's
    /// name, another map's room by its map's name.
    #[tokio::test]
    async fn rooms_are_named_by_title_and_place() {
        let mapper = maps().await;
        let atlas = mapper.get_current_atlas();
        let here = area(KEEP);
        let gate = name(&atlas, here, keep_room(2));
        assert_eq!(
            (gate.title.as_str(), gate.place, gate.map_name),
            ("West Gate", None, None)
        );
        let hidden = name(&atlas, here, library());
        assert_eq!(hidden.title, "Hidden Library");
        assert_eq!(
            hidden.place.map(|(name, _)| name).as_deref(),
            Some("Bookshelf")
        );
        assert_eq!(hidden.map_name, None);
        let far = name(&atlas, here, ossuary());
        assert_eq!(far.title, "Ossuary");
        assert_eq!(far.map_name.as_deref(), Some("Catacombs"));
    }

    /// A one-way link runs from the room its exit leaves; a two-way link
    /// from the room the selection came from.
    #[tokio::test]
    async fn a_link_reads_in_travel_order() {
        let mapper = maps().await;
        let atlas = mapper.get_current_atlas();
        let keep = atlas.get_area(&area(KEEP)).expect("loaded");

        let gate = link_view(&atlas, &keep, link(C_GATE), None).expect("the gate");
        assert!(gate.two_way());
        assert_eq!(gate.from.room, Destination::Room(keep_room(1)));
        let from_two = link_view(
            &atlas,
            &keep,
            link(C_GATE),
            Some(smudgy_cloud::RoomAddress::map(RoomNumber(2))),
        )
        .expect("the gate");
        assert_eq!(from_two.from.room, Destination::Room(keep_room(2)));
        assert_eq!(
            from_two.from.exit.as_ref().map(|exit| exit.id),
            Some(exit_id(E_GATE_S))
        );

        let ferry = link_view(
            &atlas,
            &keep,
            link(C_FERRY),
            Some(smudgy_cloud::RoomAddress::map(RoomNumber(1))),
        )
        .expect("the ferry");
        assert!(!ferry.two_way());
        assert_eq!(ferry.from.room, Destination::Room(keep_room(4)));
        assert_eq!(ferry.to.room, Destination::Room(keep_room(1)));
        assert_eq!(ferry.to.doc_room, Some(RoomAddress::map(RoomNumber(1))));

        let down = link_view(&atlas, &keep, link(C_OSSUARY), None).expect("the way down");
        assert!(down.two_way(), "the Catacombs keep the way back");
        assert_eq!(down.to.room, Destination::Room(ossuary()));
        assert_eq!(down.to.doc_room, None);
        assert_eq!(
            down.return_elsewhere.map(|back| (back.map, back.exit)),
            Some((area(CATACOMBS), exit_id(E_BACK)))
        );

        let passage = link_view(&atlas, &keep, link(C_LIBRARY), None).expect("the passage");
        assert_eq!(passage.place, secret());
        assert_eq!(passage.rooms(), vec![keep_room(1), library()]);
    }

    /// A link between map rooms, or into another map, may live in any place
    /// the viewer adds to; an end in one of the map's places keeps it there.
    #[tokio::test]
    async fn a_links_legal_places() {
        let mapper = maps().await;
        let atlas = mapper.get_current_atlas();
        let keep = atlas.get_area(&area(KEEP)).expect("loaded");
        assert_eq!(
            link_places(&keep, SourceId::Map, &[keep_room(1), keep_room(2)]),
            LinkPlaces::Choice(vec![SourceId::Map, secret(), SourceId::Private])
        );
        assert_eq!(
            link_places(&keep, secret(), &[keep_room(1), ossuary()]),
            LinkPlaces::Choice(vec![secret(), SourceId::Map, SourceId::Private])
        );
        assert_eq!(
            link_places(&keep, secret(), &[keep_room(1), library()]),
            LinkPlaces::Fixed(secret())
        );

        assert_eq!(
            place_for(area(KEEP), &[keep_room(1), library()], SourceId::Map),
            Some(secret())
        );
        assert_eq!(
            place_for(area(KEEP), &[keep_room(1), keep_room(2)], SourceId::Private),
            Some(SourceId::Private)
        );
        let other = RoomId {
            place: SourceId::Private,
            ..library()
        };
        assert_eq!(
            place_for(area(KEEP), &[other, library()], SourceId::Map),
            None
        );
        assert_eq!(
            reachable_places(&keep, Some(library())),
            vec![secret()],
            "a place's room links only within its place"
        );
        assert_eq!(reachable_places(&keep, Some(keep_room(1))), vec![secret()]);

        // A move takes `remove` where the link is: one the viewer only
        // adds to and edits there stays there.
        let mapper = maps_where(&["read", "add", "edit"]).await;
        let atlas = mapper.get_current_atlas();
        let keep = atlas.get_area(&area(KEEP)).expect("loaded");
        assert_eq!(
            link_places(&keep, secret(), &[keep_room(1), ossuary()]),
            LinkPlaces::Fixed(secret())
        );
        assert!(matches!(
            link_places(&keep, SourceId::Map, &[keep_room(1), keep_room(2)]),
            LinkPlaces::Choice(_)
        ));
    }

    /// The two sides read as one door while they agree and the viewer
    /// hasn't asked to see them apart.
    #[test]
    fn doors_on_both_sides_show_as_one_while_they_agree() {
        let closed = DoorSide::default().with_state(DoorState::Closed);
        let locked = DoorSide::default().with_state(DoorState::Locked);
        assert!(doors_shown_as_one(&closed, Some(&closed.clone()), false));
        assert!(!doors_shown_as_one(&closed, Some(&locked), false));
        assert!(!doors_shown_as_one(&closed, Some(&closed.clone()), true));
        assert!(
            doors_shown_as_one(&locked, None, true),
            "one way has one side"
        );
        let named = DoorSide {
            name: Some("gate".to_string()),
            opens_with: Some("pull lever".to_string()),
            ..locked.clone()
        };
        assert_eq!(named.with_state(DoorState::None), DoorSide::default());
    }

    /// The picker lists this map's rooms nearest the link's room first (its
    /// level first), then the places' rooms; a query narrows by title, place
    /// or number.
    #[tokio::test]
    async fn the_picker_lists_nearest_first_and_filters() {
        let mapper = maps().await;
        let atlas = mapper.get_current_atlas();
        let keep = atlas.get_area(&area(KEEP)).expect("loaded");
        let near = near(&atlas, keep_room(1));
        let numbers = |found: &Found| -> Vec<(SourceId, i32)> {
            found
                .rooms
                .iter()
                .map(|room| (room.id.place, room.id.number.0))
                .collect()
        };

        let (map_rooms, places) =
            this_map_picks(&atlas, &keep, near, Some(keep_room(1)), &[secret()], "", 10);
        assert_eq!(
            numbers(&map_rooms),
            vec![
                (SourceId::Map, 2),
                (SourceId::Map, 3),
                (SourceId::Map, 4),
                (SourceId::Map, 5)
            ],
            "the room itself is left out; the room above comes last"
        );
        assert_eq!(numbers(&places), vec![(secret(), 1)]);

        let (map_rooms, places) = this_map_picks(
            &atlas,
            &keep,
            near,
            Some(keep_room(1)),
            &[secret()],
            "gar",
            10,
        );
        assert_eq!(numbers(&map_rooms), vec![(SourceId::Map, 3)]);
        assert!(places.rooms.is_empty());
        for query in ["#3", "3", " #3 "] {
            let (map_rooms, _) = this_map_picks(&atlas, &keep, near, None, &[secret()], query, 10);
            assert_eq!(numbers(&map_rooms), vec![(SourceId::Map, 3)], "{query}");
        }
        let (_, places) = this_map_picks(&atlas, &keep, near, None, &[secret()], "bookshelf", 10);
        assert_eq!(
            numbers(&places),
            vec![(secret(), 1)],
            "a place's name finds its rooms"
        );
        let (_, places) = this_map_picks(&atlas, &keep, near, None, &[], "", 10);
        assert!(places.rooms.is_empty(), "only the places offered");

        let (map_rooms, _) = this_map_picks(&atlas, &keep, near, None, &[], "", 2);
        assert_eq!(map_rooms.rooms.len(), 2);
        assert_eq!(map_rooms.more, 3);

        let maps = other_maps(&atlas, area(KEEP), &HashSet::new(), "");
        assert_eq!(maps, vec![(area(CATACOMBS), "Catacombs".to_string())]);
        assert!(
            other_maps(
                &atlas,
                area(KEEP),
                &[area(CATACOMBS)].into_iter().collect(),
                ""
            )
            .is_empty()
        );
        assert!(other_maps(&atlas, area(KEEP), &HashSet::new(), "keep").is_empty());

        // A link reaches only maps of its own map's tier, never a session's.
        let set = |ids: &[&str]| ids.iter().map(|id| area(id)).collect::<HashSet<AreaId>>();
        let none = HashSet::new();
        for (here, session, local, unreachable) in [
            (KEEP, set(&[]), set(&[]), set(&[])),
            (KEEP, set(&[]), set(&[CATACOMBS]), set(&[CATACOMBS])),
            (KEEP, set(&[]), set(&[KEEP]), set(&[CATACOMBS])),
            (KEEP, set(&[]), set(&[KEEP, CATACOMBS]), set(&[])),
            (KEEP, set(&[CATACOMBS]), set(&[]), set(&[CATACOMBS])),
            (KEEP, set(&[KEEP]), set(&[CATACOMBS]), set(&[KEEP])),
        ] {
            assert_eq!(
                unreachable_maps(&atlas, area(here), &session, &local),
                unreachable,
                "session {session:?}, local {local:?}"
            );
        }
        let reachable = |local| {
            other_maps(
                &atlas,
                area(KEEP),
                &unreachable_maps(&atlas, area(KEEP), &none, &set(local)),
                "",
            )
        };
        assert!(
            reachable(&[CATACOMBS]).is_empty(),
            "a cloud map lists no local map"
        );
        assert!(
            reachable(&[KEEP]).is_empty(),
            "a local map lists no cloud map"
        );
        assert_eq!(reachable(&[KEEP, CATACOMBS]).len(), 1);
        let catacombs = atlas.get_area(&area(CATACOMBS)).expect("loaded");
        let found = other_map_picks(&atlas, area(KEEP), &catacombs, true, "oss", 10);
        assert_eq!(found.rooms.len(), 1);
        assert_eq!(found.rooms[0].map_name.as_deref(), Some("Catacombs"));
        let found = other_map_picks(&atlas, area(KEEP), &catacombs, true, "", 10);
        assert_eq!(
            found
                .rooms
                .iter()
                .map(|room| (room.id.place, room.title.as_str()))
                .collect::<Vec<_>>(),
            vec![(SourceId::Map, "Ossuary"), (crypt(), "Bone Altar")],
            "its rooms, then its Secrets' the viewer reads"
        );
        assert_eq!(
            found.rooms[1].place.as_ref().map(|(name, _)| name.as_str()),
            Some("Crypt")
        );
    }
}
